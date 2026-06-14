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
        }],
    )
    .await
    .unwrap();

    let row = db.cache_get_object(&acct_id, "b", key).await.unwrap().unwrap();
    assert_eq!(row.size, 20);
    assert_eq!(row.etag.as_deref(), Some("v2"));
}
