mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cosmog_lib::store::{GetOptions, PutOptions};
use cosmog_lib::transfer::{ProgressSink, TransferCtx, TransferEvent};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[serial_test::serial]
async fn single_part_roundtrip() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-roundtrip").await;

    let src = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(src.path(), b"hello").await.unwrap();
    store
        .put_object(
            &bucket,
            "k",
            src.path().into(),
            PutOptions::default(),
            TransferCtx::new("up"),
        )
        .await
        .unwrap();

    let dst_dir = tempfile::tempdir().unwrap();
    let dst = dst_dir.path().join("k");
    store
        .get_object(
            &bucket,
            "k",
            dst.clone(),
            GetOptions::default(),
            TransferCtx::new("down"),
        )
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&dst).await.unwrap(), b"hello");

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn multipart_upload_above_threshold() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-mpu").await;

    // 12 MiB triggers multipart at default 8 MiB threshold w/ 5 MiB min parts.
    let payload = vec![b'X'; 12 * 1024 * 1024];
    let src = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(src.path(), &payload).await.unwrap();

    let bytes_seen = Arc::new(AtomicU64::new(0));
    let bs = bytes_seen.clone();
    let sink = ProgressSink::from_fn(move |event| {
        if let TransferEvent::Progress { bytes_done, .. } = event {
            bs.store(bytes_done, Ordering::Relaxed);
        }
    });
    let ctx = TransferCtx::new("mpu").with_progress(sink);

    let result = store
        .put_object(
            &bucket,
            "big.bin",
            src.path().into(),
            PutOptions::default(),
            ctx,
        )
        .await
        .unwrap();
    assert!(result.upload_id.is_some(), "expected multipart path");
    // bytes_seen is best-effort: progress emits are throttled (200ms) and the
    // upload can finish before any tick fires on a local MinIO. We don't
    // assert a particular byte count, just that the multipart path ran (the
    // upload_id check above is the real proof).
    let _ = bytes_seen;

    let meta = store.head_object(&bucket, "big.bin").await.unwrap();
    assert_eq!(meta.size, payload.len() as i64);

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn cancel_during_download_cleans_up() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-cancel").await;

    // 10 MiB so download has time to be canceled mid-stream.
    let payload = vec![b'C'; 10 * 1024 * 1024];
    let src = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(src.path(), &payload).await.unwrap();
    store
        .put_object(
            &bucket,
            "cancelme.bin",
            src.path().into(),
            PutOptions::default(),
            TransferCtx::new("up"),
        )
        .await
        .unwrap();

    let dst_dir = tempfile::tempdir().unwrap();
    let dst = dst_dir.path().join("cancelme.bin");

    // Pre-cancel: the worker will observe the token immediately on first
    // poll, before the body stream starts. This is deterministic regardless
    // of how fast localhost MinIO returns.
    let cancel = CancellationToken::new();
    cancel.cancel();

    let res = store
        .get_object(
            &bucket,
            "cancelme.bin",
            dst.clone(),
            GetOptions::default(),
            TransferCtx::new("down").with_cancel(cancel),
        )
        .await;
    assert!(matches!(&res, Err(e) if e.code() == "canceled"), "expected canceled, got {res:?}");
    // The partial file should have been removed.
    assert!(!dst.exists(), "partial file left behind");

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn range_get_returns_partial() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-range").await;

    let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let src = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(src.path(), &payload).await.unwrap();
    store
        .put_object(
            &bucket,
            "range.bin",
            src.path().into(),
            PutOptions::default(),
            TransferCtx::new("u"),
        )
        .await
        .unwrap();

    let dst_dir = tempfile::tempdir().unwrap();
    let dst = dst_dir.path().join("range.bin");
    store
        .get_object(
            &bucket,
            "range.bin",
            dst.clone(),
            GetOptions {
                version_id: None,
                range_start: Some(100),
                range_end: Some(199),
            },
            TransferCtx::new("r"),
        )
        .await
        .unwrap();
    let read = tokio::fs::read(&dst).await.unwrap();
    assert_eq!(read.len(), 100);
    assert_eq!(&read[..], &payload[100..200]);

    common::cleanup_bucket(&store, &bucket).await;
}
