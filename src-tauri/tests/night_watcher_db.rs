//! Night Watcher persistence tests. Pure SQLite (no MinIO): exercises the
//! migration-15 schema, watch CRUD, the file-state records the change-detection
//! fast path relies on, and FK cascade behaviour.

mod common;

use cosmog_lib::db::accounts::NewAccount;
use cosmog_lib::db::night_watcher::{FileState, NewWatch, WatchPatch};

async fn seed_account(db: &cosmog_lib::db::Db) -> String {
    db.insert_account(NewAccount {
        name: "nw".into(),
        protocol: "s3".into(),
        endpoint: Some("http://localhost:9000".into()),
        region: "us-east-1".into(),
        access_key_id: "AK".into(),
        addressing_style: Some("path".into()),
    })
    .await
    .unwrap()
    .id
}

fn new_watch(account_id: &str, dir: &str) -> NewWatch {
    NewWatch {
        account_id: account_id.into(),
        bucket: "b1".into(),
        local_dir: dir.into(),
        key_prefix: "prefix".into(),
        ignore_file: None,
        full_scan_secs: 300,
        tree_uri: None,
    }
}

#[tokio::test]
async fn watch_crud_roundtrip_and_enabled_filter() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;

    db.insert_watch("w1", new_watch(&acct, "/tmp/one")).await.unwrap();
    db.insert_watch("w2", new_watch(&acct, "/tmp/two")).await.unwrap();

    let all = db.list_watches().await.unwrap();
    assert_eq!(all.len(), 2);
    // Defaults applied by the migration + insert.
    let w1 = db.get_watch("w1").await.unwrap().unwrap();
    assert_eq!(w1.delete_policy, "keep");
    assert_eq!(w1.key_prefix, "prefix");
    assert!(w1.enabled);
    assert_eq!(w1.last_scan_at, None);

    // Disable w2, confirm the enabled filter drops it.
    db.set_watch_enabled("w2", false).await.unwrap();
    let enabled = db.list_enabled_watches().await.unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, "w1");

    // Patch a field, leave others untouched.
    db.update_watch(
        "w1",
        WatchPatch {
            full_scan_secs: Some(900),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let w1 = db.get_watch("w1").await.unwrap().unwrap();
    assert_eq!(w1.full_scan_secs, 900);
    assert_eq!(w1.key_prefix, "prefix"); // unchanged

    db.delete_watch("w1").await.unwrap();
    assert!(db.get_watch("w1").await.unwrap().is_none());
}

#[tokio::test]
async fn scan_result_records_error_then_clears() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_watch("w1", new_watch(&acct, "/tmp/one")).await.unwrap();

    db.set_watch_scan_result("w1", 111, Some("boom".into())).await.unwrap();
    let w = db.get_watch("w1").await.unwrap().unwrap();
    assert_eq!(w.last_scan_at, Some(111));
    assert_eq!(w.last_error.as_deref(), Some("boom"));

    db.set_watch_scan_result("w1", 222, None).await.unwrap();
    let w = db.get_watch("w1").await.unwrap().unwrap();
    assert_eq!(w.last_scan_at, Some(222));
    assert_eq!(w.last_error, None);
}

#[tokio::test]
async fn file_state_upsert_overwrites_and_counts() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_watch("w1", new_watch(&acct, "/tmp/one")).await.unwrap();

    let st = FileState {
        rel_path: "a/b.txt".into(),
        hash: "hash1".into(),
        mtime: 100,
        size: 10,
        synced_etag: Some("etag1".into()),
    };
    db.file_state_upsert("w1", st).await.unwrap();

    let got = db.file_state_get("w1", "a/b.txt").await.unwrap().unwrap();
    assert_eq!(got.hash, "hash1");
    assert_eq!(got.mtime, 100);
    assert_eq!(got.size, 10);
    assert_eq!(got.synced_etag.as_deref(), Some("etag1"));

    // Upsert same key with changed content: overwrites, does not duplicate.
    db.file_state_upsert(
        "w1",
        FileState {
            rel_path: "a/b.txt".into(),
            hash: "hash2".into(),
            mtime: 200,
            size: 20,
            synced_etag: Some("etag2".into()),
        },
    )
    .await
    .unwrap();
    let got = db.file_state_get("w1", "a/b.txt").await.unwrap().unwrap();
    assert_eq!(got.hash, "hash2");
    assert_eq!(db.file_state_count("w1").await.unwrap(), 1);

    // delete_policy=keep path: dropping the record is a local-only forget.
    db.file_state_delete("w1", "a/b.txt").await.unwrap();
    assert!(db.file_state_get("w1", "a/b.txt").await.unwrap().is_none());
    assert_eq!(db.file_state_count("w1").await.unwrap(), 0);
}

#[tokio::test]
async fn deleting_account_cascades_to_watch_and_file_state() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_watch("w1", new_watch(&acct, "/tmp/one")).await.unwrap();
    db.file_state_upsert(
        "w1",
        FileState {
            rel_path: "x".into(),
            hash: "h".into(),
            mtime: 1,
            size: 1,
            synced_etag: None,
        },
    )
    .await
    .unwrap();

    // FK ON DELETE CASCADE: account -> nw_watch -> nw_file_state.
    db.delete_account(&acct).await.unwrap();
    assert!(db.get_watch("w1").await.unwrap().is_none());
    assert_eq!(db.file_state_count("w1").await.unwrap(), 0);
}
