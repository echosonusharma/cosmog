mod common;

use cosmog_lib::store::CannedAcl;

#[tokio::test]
#[serial_test::serial]
async fn bucket_lifecycle() {
    require_minio!();
    let store = common::make_store().await;
    let name = common::unique_bucket("cosmog-bkt");

    // create
    store.create_bucket(&name, None).await.expect("create");
    // list contains it
    let buckets = store.list_buckets().await.expect("list");
    assert!(buckets.iter().any(|b| b.name == name), "bucket missing from list");
    // head succeeds
    store.head_bucket(&name).await.expect("head");
    // delete
    store.delete_bucket(&name).await.expect("delete");
    // head should now fail with NotFound
    let err = store.head_bucket(&name).await.expect_err("head after delete");
    assert!(
        matches!(err.code(), "not_found" | "s3"),
        "unexpected code: {} ({})", err.code(), err
    );
}

#[tokio::test]
#[serial_test::serial]
async fn bucket_acl_and_versioning() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-acl").await;

    // versioning round-trip
    store
        .put_bucket_versioning(&bucket, true)
        .await
        .expect("enable versioning");
    let on = store.get_bucket_versioning(&bucket).await.expect("get versioning");
    assert!(on, "versioning not reflected");

    // ACL set; MinIO supports private + public-read
    store
        .put_bucket_acl(&bucket, CannedAcl::Private)
        .await
        .expect("set acl");

    common::cleanup_bucket(&store, &bucket).await;
}
