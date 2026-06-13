mod common;

use cosmog_lib::db::capabilities::{probe_account, probe_bucket, CapState};

#[tokio::test]
#[serial_test::serial]
async fn probe_account_then_bucket() {
    require_minio!();
    let store = common::make_store().await;
    let (db, _td, acct_id) = common::tmp_db_with_account().await;
    let bucket = common::create_test_bucket(&store, "cosmog-cap").await;

    let acct = probe_account(store.clone(), &acct_id).await.unwrap();
    assert_eq!(acct.list_buckets, CapState::Allowed);
    assert_eq!(acct.create_bucket, CapState::Unknown);
    db.account_capabilities_upsert(&acct).await.unwrap();

    let bkt = probe_bucket(store.clone(), &acct_id, &bucket).await.unwrap();
    assert_eq!(bkt.head_bucket, CapState::Allowed);
    assert_eq!(bkt.list_objects, CapState::Allowed);
    db.bucket_capabilities_upsert(&bkt).await.unwrap();

    let stored = db.bucket_capabilities_get(&acct_id, &bucket).await.unwrap();
    assert!(stored.is_some());
    let s = stored.unwrap();
    assert_eq!(s.head_bucket, CapState::Allowed);
    assert_eq!(s.list_objects, CapState::Allowed);

    common::cleanup_bucket(&store, &bucket).await;
}
