mod common;

use cosmog_lib::store::{PutOptions, ListOptions};
use cosmog_lib::transfer::TransferCtx;

#[tokio::test]
#[serial_test::serial]
async fn put_head_delete_object() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-obj").await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(tmp.path(), b"hello-world").await.unwrap();
    let ctx = TransferCtx::new("t1");
    store
        .put_object(
            &bucket,
            "greet.txt",
            tmp.path().into(),
            PutOptions {
                content_type: Some("text/plain".into()),
                ..Default::default()
            },
            ctx,
        )
        .await
        .unwrap();

    let meta = store.head_object(&bucket, "greet.txt").await.unwrap();
    assert_eq!(meta.size, 11);
    assert_eq!(meta.content_type.as_deref(), Some("text/plain"));

    store.delete_object(&bucket, "greet.txt").await.unwrap();
    let err = store.head_object(&bucket, "greet.txt").await.unwrap_err();
    assert_eq!(err.code(), "not_found");

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn list_with_prefix_and_delimiter() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-list").await;

    common::seed_objects(&store, &bucket, "a", 3).await;
    common::seed_objects(&store, &bucket, "b/nested", 2).await;

    let direct = store
        .list_objects(
            &bucket,
            ListOptions {
                prefix: Some("a/".into()),
                delimiter: Some("/".into()),
                continuation: None,
                max_keys: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(direct.objects.len(), 3, "direct objects under a/");

    let recursive_b = store
        .list_objects(
            &bucket,
            ListOptions {
                prefix: Some("b/".into()),
                delimiter: None,
                continuation: None,
                max_keys: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(recursive_b.objects.len(), 2);

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn batch_delete_returns_per_key_results() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-batch").await;
    common::seed_objects(&store, &bucket, "batch", 5).await;

    let keys: Vec<String> = (0..5).map(|i| format!("batch/item-{i:04}.txt")).collect();
    let result = store.delete_objects(&bucket, &keys).await.unwrap();
    assert_eq!(result.deleted.len(), 5);
    assert!(result.errors.is_empty());

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn copy_object_then_head_destination() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-copy").await;
    common::seed_objects(&store, &bucket, "src", 1).await;

    store
        .copy_object(&bucket, "src/item-0000.txt", &bucket, "dst/item-0000.txt")
        .await
        .unwrap();
    let meta = store
        .head_object(&bucket, "dst/item-0000.txt")
        .await
        .unwrap();
    assert!(meta.size > 0);

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn presign_get_returns_url() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-sign").await;
    common::seed_objects(&store, &bucket, "p", 1).await;
    let url = store
        .presign_get(&bucket, "p/item-0000.txt", 60)
        .await
        .unwrap();
    assert!(url.starts_with("http"));
    assert!(url.contains("p/item-0000.txt"));

    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn preview_object_caps_bytes() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-preview").await;

    let big = vec![b'A'; 200_000];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(tmp.path(), &big).await.unwrap();
    store
        .put_object(
            &bucket,
            "big.txt",
            tmp.path().into(),
            PutOptions::default(),
            TransferCtx::new("p1"),
        )
        .await
        .unwrap();

    let prev = store.read_object_range(&bucket, "big.txt", 1024).await.unwrap();
    assert_eq!(prev.bytes.len(), 1024);
    assert!(prev.truncated);
    assert_eq!(prev.total_size, Some(200_000));

    common::cleanup_bucket(&store, &bucket).await;
}
