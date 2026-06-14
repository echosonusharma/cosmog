mod common;

use cosmog_lib::db::settings::AppSettings;

#[tokio::test]
async fn settings_roundtrip_defaults_and_overrides() {
    let (db, _td) = common::tmp_db().await;
    let s = db.settings_load().await.unwrap();
    assert_eq!(s.transfer_concurrency, 3);

    let mut updated = s.clone();
    updated.transfer_concurrency = 8;
    updated.theme = "dark".into();
    let saved = db.settings_save(updated).await.unwrap();
    assert_eq!(saved.transfer_concurrency, 8);
    assert_eq!(saved.theme, "dark");

    let read = db.settings_load().await.unwrap();
    assert_eq!(read.transfer_concurrency, 8);
    assert_eq!(read.theme, "dark");

    // Reset returns defaults.
    let reset = db.settings_reset().await.unwrap();
    assert_eq!(reset.transfer_concurrency, AppSettings::default().transfer_concurrency);
}

#[tokio::test]
async fn settings_normalize_clamps_bad_values() {
    let (db, _td) = common::tmp_db().await;
    let mut s = AppSettings::default();
    s.transfer_concurrency = 9999; // out of range
    s.part_size_bytes = 1; // below S3 floor
    s.theme = "rainbow".into();
    let saved = db.settings_save(s).await.unwrap();
    assert!(saved.transfer_concurrency <= 16);
    assert!(saved.part_size_bytes >= 5 * 1024 * 1024);
    assert_eq!(saved.theme, "system");
}

#[tokio::test]
async fn accounts_crud_without_keyring() {
    let (db, _td) = common::tmp_db().await;
    let acct = db
        .insert_account(cosmog_lib::db::accounts::NewAccount {
            name: "test".into(),
            protocol: "s3".into(),
            endpoint: Some("http://localhost:9000".into()),
            region: "us-east-1".into(),
            access_key_id: "AK".into(),
            addressing_style: Some("path".into()),
        })
        .await
        .unwrap();
    let by_id = db.get_account(&acct.id).await.unwrap();
    assert_eq!(by_id.name, "test");

    let updated = db
        .update_account(
            &acct.id,
            cosmog_lib::db::accounts::UpdateAccount {
                name: Some("renamed".into()),
                endpoint: None,
                region: None,
                access_key_id: None,
                addressing_style: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "renamed");

    db.delete_account(&acct.id).await.unwrap();
    assert!(db.get_account(&acct.id).await.is_err());
}

#[tokio::test]
async fn backup_to_produces_valid_sqlite_file() {
    let (db, _td) = common::tmp_db().await;
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().join("backup.sqlite");
    db.backup_to(dest.clone()).await.unwrap();
    assert!(dest.exists());
    let header = tokio::fs::read(&dest).await.unwrap();
    assert_eq!(&header[..16], b"SQLite format 3\0");
}

#[tokio::test]
async fn migrations_idempotent() {
    let (db, _td) = common::tmp_db().await;
    // Calling settings_save twice should not blow up — schema is in place.
    db.settings_save(AppSettings::default()).await.unwrap();
    db.settings_save(AppSettings::default()).await.unwrap();
}

#[tokio::test]
async fn reap_orphan_transfers_flips_active_to_failed() {
    let (db, _td) = common::tmp_db().await;
    let acct = db
        .insert_account(cosmog_lib::db::accounts::NewAccount {
            name: "a".into(),
            protocol: "s3".into(),
            endpoint: None,
            region: "us-east-1".into(),
            access_key_id: "AK".into(),
            addressing_style: None,
        })
        .await
        .unwrap();
    db.insert_transfer(cosmog_lib::db::transfers::NewTransfer {
        id: "tid".into(),
        account_id: acct.id,
        bucket: "b".into(),
        key: "k".into(),
        direction: cosmog_lib::db::transfers::Direction::Upload,
        local_path: "/tmp/x".into(),
        options_json: None,
    })
    .await
    .unwrap();
    db.update_transfer_status("tid", cosmog_lib::db::transfers::TransferStatus::Active, None)
        .await
        .unwrap();
    let n = db.reap_orphan_transfers().await.unwrap();
    assert_eq!(n, 1);
    let row = db.get_transfer("tid").await.unwrap();
    assert!(matches!(row.status, cosmog_lib::db::transfers::TransferStatus::Failed));
}
