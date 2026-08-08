mod common;

use cosmog_lib::db::cache::{SearchQuery, SearchScope, SortBy, SortDir, SearchFilters};
use cosmog_lib::store::{ObjectMeta, PutOptions};
use cosmog_lib::sync::{full_bucket_scan, sync_prefix_direct, sync_prefix_recursive};
use cosmog_lib::transfer::{ProgressSink, TransferCtx};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[serial_test::serial]
async fn prefix_sync_populates_cache_and_detects_deletes() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    let bucket = common::create_test_bucket(&store, "cosmog-sync").await;

    common::seed_objects(&store, &bucket, "p", 4).await;

    // First sync: 4 rows go into the cache.
    let stats = sync_prefix_recursive(&db, store.clone(), &acct_id, &bucket, "p/")
        .await
        .unwrap();
    assert_eq!(stats.upserted, 4);
    assert_eq!(stats.removed, 0);

    // Delete one object remotely; next sync should remove it.
    store
        .delete_object(&bucket, "p/item-0001.txt")
        .await
        .unwrap();
    let stats2 = sync_prefix_recursive(&db, store.clone(), &acct_id, &bucket, "p/")
        .await
        .unwrap();
    assert_eq!(stats2.removed, 1, "deletion not swept");

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn full_bucket_scan_then_search_fts() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fts").await;

    // Seed with deliberate words for FTS hit.
    for name in &["cats/orange.jpg", "cats/black.jpg", "dogs/poodle.jpg", "notes/readme.md"] {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        tokio::fs::write(tmp.path(), b"x").await.unwrap();
        store
            .put_object(
                &bucket,
                name,
                tmp.path().into(),
                PutOptions::default(),
                TransferCtx::new(format!("seed-{name}")),
            )
            .await
            .unwrap();
    }

    full_bucket_scan(
        &db,
        store.clone(),
        &acct_id,
        &bucket,
        ProgressSink::noop(),
        "scan".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    // FTS query for "cats" should match the two jpgs.
    let result = db
        .search_objects(SearchQuery {
            account_id: acct_id.clone(),
            bucket: bucket.clone(),
            scope: SearchScope::Bucket,
            query: Some("cats".into()),
            filters: SearchFilters::default(),
            sort: SortBy::Name,
            sort_dir: SortDir::Asc,
            page_size: Some(50),
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(result.objects.len(), 2);
    assert!(result.objects.iter().all(|o| o.key.starts_with("cats/")));

    // Extension facet should report jpg and md counts.
    let jpg_count = result
        .facets
        .extensions
        .iter()
        .find(|b| b.value == "jpg")
        .map(|b| b.count)
        .unwrap_or(0);
    assert!(jpg_count >= 2);

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn prefix_direct_lists_only_direct_children() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    let bucket = common::create_test_bucket(&store, "cosmog-direct").await;

    common::seed_objects(&store, &bucket, "lvl", 2).await; // lvl/item-0000.txt etc
    common::seed_objects(&store, &bucket, "lvl/sub", 3).await; // lvl/sub/item-0000.txt etc

    sync_prefix_direct(&db, store.clone(), &acct_id, &bucket, "lvl/")
        .await
        .unwrap();

    let result = db
        .search_objects(SearchQuery {
            account_id: acct_id.clone(),
            bucket: bucket.clone(),
            scope: SearchScope::Prefix {
                prefix: "lvl/".into(),
                recursive: false,
            },
            query: None,
            filters: SearchFilters::default(),
            sort: SortBy::Name,
            sort_dir: SortDir::Asc,
            page_size: Some(100),
            cursor: None,
        })
        .await
        .unwrap();
    // Only direct children; nested ones live under lvl/sub/.
    assert_eq!(result.objects.len(), 2);
    assert!(result
        .objects
        .iter()
        .all(|o| o.key.starts_with("lvl/") && !o.key.contains("/sub/")));

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
async fn cache_upsert_batch_inserts_all_rows() {
    let (db, _td, acct_id) = common::tmp_db_with_account().await;

    let objects: Vec<ObjectMeta> = (0..50)
        .map(|i| ObjectMeta {
            key: format!("prefix/item-{i:04}.txt"),
            size: 100 + i as i64,
            etag: Some(format!("etag-{i}")),
            last_modified: Some(1_000_000 + i as i64),
            storage_class: Some("STANDARD".into()),
            content_type: Some("text/plain".into()),
            version_id: None,
            user_metadata: Default::default(),
        })
        .collect();

    let count = db
        .cache_upsert_objects_batch(&acct_id, "b", &objects)
        .await
        .unwrap();
    assert_eq!(count, 50);

    let row = db
        .cache_get_object(&acct_id, "b", "prefix/item-0025.txt")
        .await
        .unwrap()
        .expect("row must exist");
    assert_eq!(row.size, 125);
    assert_eq!(row.etag.as_deref(), Some("etag-25"));
}

#[tokio::test]
async fn cache_upsert_batch_overwrites_on_conflict() {
    let (db, _td, acct_id) = common::tmp_db_with_account().await;

    let key = "prefix/dup.txt";
    db.cache_upsert_objects_batch(
        &acct_id,
        "b",
        &[ObjectMeta {
            key: key.into(),
            size: 10,
            etag: Some("v1".into()),
            last_modified: None,
            storage_class: None,
            content_type: None,
            version_id: None,
            user_metadata: Default::default(),
        }],
    )
    .await
    .unwrap();

    db.cache_upsert_objects_batch(
        &acct_id,
        "b",
        &[ObjectMeta {
            key: key.into(),
            size: 20,
            etag: Some("v2".into()),
            last_modified: None,
            storage_class: None,
            content_type: None,
            version_id: None,
            user_metadata: Default::default(),
        }],
    )
    .await
    .unwrap();

    let row = db.cache_get_object(&acct_id, "b", key).await.unwrap().unwrap();
    assert_eq!(row.size, 20);
    assert_eq!(row.etag.as_deref(), Some("v2"));
}

fn meta(key: &str, size: i64) -> ObjectMeta {
    ObjectMeta {
        key: key.into(),
        size,
        etag: None,
        last_modified: None,
        storage_class: None,
        content_type: None,
        version_id: None,
        user_metadata: Default::default(),
    }
}

// Offset pagination must return every row exactly once in sort order, even when
// the sort column (size) has no correlation with rowid (insertion order). The
// old rowid-cursor keyset dropped/duplicated rows here.
#[tokio::test]
async fn search_pagination_covers_all_rows_for_size_sort() {
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    // Insertion order a..e (rowid 1..5); sizes deliberately unsorted.
    let objs = vec![
        meta("a", 50), meta("b", 10), meta("c", 40), meta("d", 20), meta("e", 30),
    ];
    db.cache_upsert_objects_batch(&acct_id, "b", &objs).await.unwrap();

    let mut keys = Vec::new();
    let mut cursor: Option<i64> = None;
    loop {
        let res = db
            .search_objects(SearchQuery {
                account_id: acct_id.clone(),
                bucket: "b".into(),
                scope: SearchScope::Bucket,
                query: None,
                filters: SearchFilters::default(),
                sort: SortBy::Size,
                sort_dir: SortDir::Desc,
                page_size: Some(2),
                cursor,
            })
            .await
            .unwrap();
        for o in &res.objects {
            keys.push(o.key.clone());
        }
        assert_eq!(res.total, 5);
        match res.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    // Size desc: a(50) c(40) e(30) d(20) b(10). No gaps, no dupes.
    assert_eq!(keys, vec!["a", "c", "e", "d", "b"]);
}

// A mixed query whose short (<3 char) term can't go through the trigram index
// must still constrain results, not be silently dropped.
#[tokio::test]
async fn search_mixed_short_and_long_terms_both_apply() {
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    db.cache_upsert_objects_batch(
        &acct_id,
        "b",
        &[meta("report99.txt", 1), meta("report.txt", 1)],
    )
    .await
    .unwrap();

    let res = db
        .search_objects(SearchQuery {
            account_id: acct_id.clone(),
            bucket: "b".into(),
            scope: SearchScope::Bucket,
            // "report" -> FTS, "99" -> LIKE on basename. AND of both.
            query: Some("report 99".into()),
            filters: SearchFilters::default(),
            sort: SortBy::Name,
            sort_dir: SortDir::Asc,
            page_size: Some(50),
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(res.total, 1, "short term must further constrain the match");
    assert_eq!(res.objects.len(), 1);
    assert_eq!(res.objects[0].key, "report99.txt");
}

// browse_children pages files via offset and only yields folders on page 1.
#[tokio::test]
async fn browse_children_paginates_by_offset() {
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    db.cache_upsert_objects_batch(
        &acct_id,
        "b",
        &[meta("p/a", 1), meta("p/b", 1), meta("p/c", 1), meta("p/sub/x", 1)],
    )
    .await
    .unwrap();

    let (files, subs, has_more) = db.browse_children(&acct_id, "b", "p/", 0).await.unwrap();
    let keys: Vec<_> = files.iter().map(|f| f.key.clone()).collect();
    assert_eq!(keys, vec!["p/a", "p/b", "p/c"]);
    assert_eq!(subs, vec!["p/sub/"]);
    assert!(!has_more);

    // Offset past the first two direct files; folders suppressed off page 1.
    let (files, subs, _) = db.browse_children(&acct_id, "b", "p/", 2).await.unwrap();
    let keys: Vec<_> = files.iter().map(|f| f.key.clone()).collect();
    assert_eq!(keys, vec!["p/c"]);
    assert!(subs.is_empty(), "folders should only come back on the first page");
}
