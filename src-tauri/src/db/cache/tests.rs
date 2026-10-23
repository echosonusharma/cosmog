use super::*;
use crate::db::Db;

#[test]
fn substr_offset_counts_chars_not_bytes() {
    assert_eq!(substr_offset_past_prefix(""), 1);
    assert_eq!(substr_offset_past_prefix("docs/"), 6);
    // "文档/" is 4 characters but 7 UTF-8 bytes; byte math would produce 8.
    assert_eq!(substr_offset_past_prefix("文档/"), 4);
    assert_ne!(
        substr_offset_past_prefix("文档/") - 1,
        "文档/".len() as i64,
        "offset must be character-based, not prefix.len()"
    );
}

/// Regression: multibyte prefix byte math misclassified nested keys as children and deleted them.
#[tokio::test]
async fn multibyte_prefix_direct_sweep_keeps_nested_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("test.sqlite")).await.expect("open db");

    // cached_objects has an FK onto accounts(id); seed a parent row.
    db.conn
        .call(|conn| {
            conn.execute(
                "INSERT INTO accounts (id, name, protocol, region, access_key_id, addressing_style, created_at, updated_at)
                 VALUES ('acct', 't', 's3', 'us-east-1', 'k', 'path', 0, 0)",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();

    let meta = |key: &str| crate::store::ObjectMeta {
        key: key.to_string(),
        size: 1,
        etag: None,
        last_modified: None,
        storage_class: None,
        content_type: None,
        version_id: None,
        user_metadata: Default::default(),
    };
    db.cache_upsert_objects_batch(
        "acct",
        "b",
        &["文档/top.txt", "文档/sub/inner.txt", "root.txt"]
            .iter()
            .map(|k| meta(k))
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();

    db.cache_mark_unseen("acct", "b", SyncScope::PrefixDirect { prefix: "文档/".into() })
        .await
        .unwrap();
    let removed = db
        .cache_sweep_unseen("acct", "b", SyncScope::PrefixDirect { prefix: "文档/".into() })
        .await
        .unwrap();

    assert_eq!(removed, 1, "only the direct child should be swept");
    assert!(
        db.cache_get_object("acct", "b", "文档/sub/inner.txt")
            .await
            .unwrap()
            .is_some(),
        "nested row under a multibyte prefix must survive a direct sweep"
    );
    assert!(
        db.cache_get_object("acct", "b", "文档/top.txt").await.unwrap().is_none(),
        "direct child should have been swept"
    );
    assert!(
        db.cache_get_object("acct", "b", "root.txt").await.unwrap().is_some(),
        "rows outside the prefix must be untouched"
    );
}

#[test]
fn prefix_upper_bound_bumps_last_bumpable_char() {
    assert_eq!(prefix_upper_bound("a/"), Value::Text("a0".into()));
    assert_eq!(prefix_upper_bound("\u{D7FF}"), Value::Text("\u{E000}".into()));
    assert_eq!(prefix_upper_bound("a\u{10FFFF}"), Value::Text("b".into()));
    assert_eq!(prefix_upper_bound(""), Value::Blob(Vec::new()));
    assert_eq!(prefix_upper_bound("\u{10FFFF}"), Value::Blob(Vec::new()));
}

#[tokio::test]
async fn prefix_range_handles_unicode_and_like_metachars() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("test.sqlite")).await.expect("open db");
    db.conn
        .call(|conn| {
            conn.execute(
                "INSERT INTO accounts (id, name, protocol, region, access_key_id, addressing_style, created_at, updated_at)
                 VALUES ('acct', 't', 's3', 'us-east-1', 'k', 'path', 0, 0)",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    let meta = |key: &str| crate::store::ObjectMeta {
        key: key.to_string(),
        size: 1,
        etag: None,
        last_modified: None,
        storage_class: None,
        content_type: None,
        version_id: None,
        user_metadata: Default::default(),
    };
    let keys = ["caf\u{e9} 100%_x/a.txt", "caf\u{e9} 100%_x/sub/b.txt", "cafe 100ab_x/c.txt", "caf\u{e9} 100%_y/d.txt"];
    db.cache_upsert_objects_batch("acct", "b", &keys.iter().map(|k| meta(k)).collect::<Vec<_>>())
        .await
        .unwrap();

    let (files, subs, _) = db.browse_children("acct", "b", "caf\u{e9} 100%_x/", 0).await.unwrap();
    assert_eq!(files.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), [keys[0]]);
    assert_eq!(subs, ["caf\u{e9} 100%_x/sub/"]);

    let scope = || SyncScope::PrefixRecursive { prefix: "caf\u{e9} 100%_x/".into() };
    db.cache_mark_unseen("acct", "b", scope()).await.unwrap();
    assert_eq!(db.cache_sweep_unseen("acct", "b", scope()).await.unwrap(), 2);
    assert!(db.cache_get_object("acct", "b", keys[2]).await.unwrap().is_some());
    assert!(db.cache_get_object("acct", "b", keys[3]).await.unwrap().is_some());
}

#[tokio::test]
async fn prefix_sweep_is_case_sensitive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("test.sqlite")).await.expect("open db");
    db.conn
        .call(|conn| {
            conn.execute(
                "INSERT INTO accounts (id, name, protocol, region, access_key_id, addressing_style, created_at, updated_at)
                 VALUES ('acct', 't', 's3', 'us-east-1', 'k', 'path', 0, 0)",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    let meta = |key: &str| crate::store::ObjectMeta {
        key: key.to_string(),
        size: 1,
        etag: None,
        last_modified: None,
        storage_class: None,
        content_type: None,
        version_id: None,
        user_metadata: Default::default(),
    };
    db.cache_upsert_objects_batch(
        "acct",
        "b",
        &[meta("Photos/a.jpg"), meta("photos/b.jpg")],
    )
    .await
    .unwrap();

    let scope = || SyncScope::PrefixRecursive { prefix: "photos/".into() };
    db.cache_mark_unseen("acct", "b", scope()).await.unwrap();
    let removed = db.cache_sweep_unseen("acct", "b", scope()).await.unwrap();

    assert_eq!(removed, 1);
    assert!(db.cache_get_object("acct", "b", "photos/b.jpg").await.unwrap().is_none());
    assert!(
        db.cache_get_object("acct", "b", "Photos/a.jpg").await.unwrap().is_some(),
        "a differently-cased sibling prefix must not be swept"
    );

    let (files, _, _) = db.browse_children("acct", "b", "Photos/", 0).await.unwrap();
    assert_eq!(files.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["Photos/a.jpg"]);
}

/// Regression (migration 19): FTS UPDATE trigger fires only on key/basename change, not `seen` sweeps.
#[tokio::test]
async fn fts_update_trigger_guards_non_key_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("test.sqlite")).await.expect("open db");

    db.conn
        .call(|conn| {
            conn.execute(
                "INSERT INTO accounts (id, name, protocol, region, access_key_id, addressing_style, created_at, updated_at)
                 VALUES ('acct', 't', 's3', 'us-east-1', 'k', 'path', 0, 0)",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();

    db.cache_upsert_objects_batch(
        "acct",
        "b",
        &[crate::store::ObjectMeta {
            key: "docs/unicorn.pdf".to_string(),
            size: 1,
            etag: None,
            last_modified: None,
            storage_class: None,
            content_type: None,
            version_id: None,
            user_metadata: Default::default(),
        }],
    )
    .await
    .unwrap();

    async fn fts_hits(db: &Db, term: &str) -> i64 {
        let sql = format!(
            "SELECT COUNT(*) FROM cached_objects_fts WHERE cached_objects_fts MATCH '\"{term}\"'"
        );
        db.conn
            .call(move |conn| {
                let n: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await
            .unwrap()
    }
    assert_eq!(fts_hits(&db, "unicorn").await, 1);

    // Updating non-indexed columns must not disturb the FTS rows.
    db.conn
        .call(|conn| {
            conn.execute("UPDATE cached_objects SET seen = 0 WHERE account_id = 'acct'", [])?;
            conn.execute("UPDATE cached_objects SET seen = 1 WHERE account_id = 'acct'", [])?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    assert_eq!(fts_hits(&db, "unicorn").await, 1, "seen-only updates must not rewrite the FTS index");

    // Renaming key + basename must reindex via the trigger; the app always writes both together (KeyParts).
    db.conn
        .call(|conn| {
            conn.execute(
                "UPDATE cached_objects SET key = 'docs/phoenix.pdf', basename = 'phoenix.pdf'
                  WHERE account_id = 'acct'",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    assert_eq!(fts_hits(&db, "unicorn").await, 0);
    assert_eq!(
        fts_hits(&db, "phoenix").await,
        1,
        "key/basename rename must be reflected in the FTS index"
    );
}
