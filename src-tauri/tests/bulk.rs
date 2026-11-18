mod common;
#[path = "common/mem_store.rs"]
mod mem_store;

use cosmog_lib::bulk::{delete_folder, download_directory};
use cosmog_lib::db::transfers::TransferStatus;
use cosmog_lib::transfer::{ProgressSink, TransferManager};
use cosmog_lib::transfer::TransferEvent;
use mem_store::MemStore;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn seed_mem(store: &MemStore, prefix: &str, n: usize) {
    let mut objs = store.objects.lock().unwrap();
    for i in 0..n {
        objs.insert(format!("{prefix}{i:05}"), b"x".to_vec());
    }
}

fn collecting_sink() -> (ProgressSink, Arc<Mutex<Vec<TransferEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    (ProgressSink::from_fn(move |e| ev.lock().unwrap().push(e)), events)
}

async fn wait_done(transfers: &TransferManager, ids: &[String]) {
    for _ in 0..60 {
        let mut pending = 0usize;
        for id in ids {
            if let Ok(t) = transfers.get(id).await {
                if !matches!(
                    t.status,
                    TransferStatus::Done | TransferStatus::Failed | TransferStatus::Canceled
                ) {
                    pending += 1;
                }
            }
        }
        if pending == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("transfers did not finish within timeout");
}

#[tokio::test]
#[serial_test::serial]
async fn delete_folder_removes_all_under_prefix() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td) = common::tmp_db().await;
    let bucket = common::create_test_bucket(&store, "cosmog-folder").await;

    common::seed_objects(&store, &bucket, "trash", 7).await;
    common::seed_objects(&store, &bucket, "keep", 2).await;

    let result = delete_folder(
        &db,
        store.clone(),
        "acct",
        &bucket,
        "trash/",
        ProgressSink::noop(),
        "tid".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.deleted, 7);
    assert_eq!(result.failed, 0);

    // "keep/*" should still be there.
    let remaining = store
        .list_objects(
            &bucket,
            cosmog_lib::store::ListOptions {
                prefix: Some("keep/".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(remaining.objects.len(), 2);

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn download_directory_roundtrip() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td) = common::tmp_db().await;
    // TransferManager inserts transfer rows that FK-reference accounts.id;
    // create a dummy account row before enqueuing.
    let acct = db
        .insert_account(cosmog_lib::db::accounts::NewAccount {
            name: "test".into(),
            protocol: "s3".into(),
            endpoint: Some(common::minio_endpoint()),
            region: common::MINIO_REGION.into(),
            access_key_id: common::MINIO_ACCESS_KEY.into(),
            addressing_style: Some("path".into()),
        })
        .await
        .unwrap();
    let acct_id = acct.id.clone();
    let bucket = common::create_test_bucket(&store, "cosmog-dir").await;
    let transfers = TransferManager::new(db.clone(), 4);

    // Seed the remote tree directly (the old upload_directory walker is gone).
    for (key, body) in [("remote/top.txt", &b"top"[..]), ("remote/nested/inner.txt", &b"inner"[..])] {
        store
            .put_object_bytes(&bucket, key, "text/plain", body.to_vec(), Default::default())
            .await
            .unwrap();
    }

    // Confirm the remote side.
    let listed = store
        .list_objects(
            &bucket,
            cosmog_lib::store::ListOptions {
                prefix: Some("remote".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let keys: Vec<&str> = listed.objects.iter().map(|o| o.key.as_str()).collect();
    assert!(keys.contains(&"remote/top.txt"));
    assert!(keys.contains(&"remote/nested/inner.txt"));

    // Download back to a fresh dir.
    let dst = tempfile::tempdir().unwrap();
    let downloaded = download_directory(
        &transfers,
        store.clone(),
        &acct_id,
        &bucket,
        "remote/",
        dst.path(),
        |_| ProgressSink::noop(),
        "op".into(),
        ProgressSink::noop(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(downloaded.enqueued.len(), 2);

    wait_done(&transfers, &downloaded.enqueued).await;

    assert_eq!(
        tokio::fs::read(dst.path().join("top.txt")).await.unwrap(),
        b"top"
    );
    assert_eq!(
        tokio::fs::read(dst.path().join("nested/inner.txt"))
            .await
            .unwrap(),
        b"inner"
    );

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
async fn delete_folder_reports_structured_capped_errors_and_listing_progress() {
    let store = MemStore::new();
    seed_mem(&store, "x/deny/", 150);
    seed_mem(&store, "x/ok/", 5);
    seed_mem(&store, "y/", 3);
    store.delete_deny.lock().unwrap().push("x/deny/".into());
    let (db, _td) = common::tmp_db().await;
    let (sink, events) = collecting_sink();

    let r = delete_folder(&db, store.clone(), "acct", "b", "x/", sink, "tid".into(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((r.deleted, r.failed, r.found), (5, 150, 155));
    assert_eq!(r.errors.len(), cosmog_lib::bulk::MAX_DELETE_ERRORS);
    assert_eq!(r.errors[0].key, "x/deny/00000");
    assert_eq!(r.errors[0].message, "denied");
    assert!(!r.canceled && r.error.is_none());
    // Denied and out-of-prefix keys survive.
    assert_eq!(store.objects.lock().unwrap().len(), 153);

    let ev = events.lock().unwrap();
    // Listing progress (found, nothing deleted yet) precedes the delete progress.
    let progress: Vec<(u64, Option<u64>)> = ev
        .iter()
        .filter_map(|e| match e {
            TransferEvent::Progress { bytes_done, bytes_total, .. } => Some((*bytes_done, *bytes_total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress, vec![(0, Some(155)), (5, Some(155))]);
    assert!(matches!(ev.last(), Some(TransferEvent::Done { .. })));
}

#[tokio::test]
async fn delete_folder_cancel_between_batches_returns_partial_counts() {
    let store = MemStore::new();
    seed_mem(&store, "big/", 1500);
    let (db, _td) = common::tmp_db().await;
    let cancel = CancellationToken::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let (ev, c) = (events.clone(), cancel.clone());
    // Cancel as soon as the first batch lands.
    let sink = ProgressSink::from_fn(move |e: TransferEvent| {
        if matches!(e, TransferEvent::Progress { bytes_done, .. } if bytes_done > 0) {
            c.cancel();
        }
        ev.lock().unwrap().push(e);
    });

    let r = delete_folder(&db, store.clone(), "acct", "b", "big/", sink, "tid".into(), cancel)
        .await
        .unwrap();
    assert!(r.canceled);
    assert_eq!((r.deleted, r.failed, r.found), (1000, 0, 1500));
    assert!(r.error.is_none());
    assert_eq!(store.objects.lock().unwrap().len(), 500);
    assert!(matches!(events.lock().unwrap().last(), Some(TransferEvent::Canceled { .. })));
}

#[tokio::test]
async fn delete_folder_hard_failure_keeps_counts() {
    let store = MemStore::new();
    seed_mem(&store, "f/", 4);
    *store.fail.lock().unwrap() = Some("access_denied");
    let (db, _td) = common::tmp_db().await;
    let (sink, events) = collecting_sink();

    let r = delete_folder(&db, store.clone(), "acct", "b", "f/", sink, "tid".into(), CancellationToken::new())
        .await
        .unwrap();
    assert!(!r.canceled);
    assert_eq!((r.deleted, r.found), (0, 4));
    assert!(r.error.as_deref().unwrap().contains("access_denied"));
    assert!(matches!(events.lock().unwrap().last(), Some(TransferEvent::Failed { .. })));
}
