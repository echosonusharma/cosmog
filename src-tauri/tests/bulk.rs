mod common;

use cosmog_lib::bulk::{delete_folder, download_directory, upload_directory};
use cosmog_lib::db::transfers::TransferStatus;
use cosmog_lib::transfer::{ProgressSink, TransferManager};
use tokio_util::sync::CancellationToken;

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
async fn upload_and_download_directory_roundtrip() {
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

    // Build a small tree.
    let src_dir = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(src_dir.path().join("nested")).await.unwrap();
    tokio::fs::write(src_dir.path().join("top.txt"), b"top").await.unwrap();
    tokio::fs::write(src_dir.path().join("nested/inner.txt"), b"inner").await.unwrap();

    let uploaded = upload_directory(
        &transfers,
        store.clone(),
        &acct_id,
        &bucket,
        "remote",
        src_dir.path(),
        |_| ProgressSink::noop(),
    )
    .await
    .unwrap();
    assert_eq!(uploaded.enqueued.len(), 2);

    // Wait for the queue to drain. Poll the transfer rows.
    wait_done(&transfers, &uploaded.enqueued).await;

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
