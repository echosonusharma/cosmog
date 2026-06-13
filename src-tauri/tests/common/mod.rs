//! Shared helpers for integration tests. All tests in this crate assume a
//! running MinIO instance (started by `docker compose up minio`).

use std::path::PathBuf;
use std::sync::Arc;

use cosmog_lib::db::Db;
use cosmog_lib::store::s3::{S3Config, S3Store};
use cosmog_lib::store::ObjectStore;

pub const MINIO_ACCESS_KEY: &str = "cosmogtest";
pub const MINIO_SECRET_KEY: &str = "cosmogtest123";
pub const MINIO_REGION: &str = "us-east-1";

pub fn minio_endpoint() -> String {
    std::env::var("MINIO_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".to_string())
}

/// True when MinIO is reachable. Tests opt out gracefully when it isn't,
/// so `cargo test` works even without docker up.
pub async fn minio_available() -> bool {
    let url = format!("{}/minio/health/live", minio_endpoint());
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(
            url.replace("http://", "")
                .replace("https://", "")
                .split('/')
                .next()
                .unwrap_or("localhost:9000"),
        ),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

/// Macro: skip the rest of a test function if MinIO is down.
#[macro_export]
macro_rules! require_minio {
    () => {
        if !$crate::common::minio_available().await {
            eprintln!("skipping: MinIO not reachable at {}", $crate::common::minio_endpoint());
            return;
        }
    };
}

pub async fn make_store() -> Arc<dyn ObjectStore> {
    let store = S3Store::new(S3Config {
        region: MINIO_REGION.into(),
        endpoint: Some(minio_endpoint()),
        access_key_id: MINIO_ACCESS_KEY.into(),
        secret_access_key: MINIO_SECRET_KEY.into(),
        addressing_style: "path".into(),
    })
    .await
    .expect("build S3Store");
    Arc::new(store)
}

pub fn unique_bucket(prefix: &str) -> String {
    use rand::Rng;
    let suffix: u32 = rand::thread_rng().gen();
    format!("{prefix}-{suffix:x}")
}

pub fn unique_key(prefix: &str) -> String {
    use rand::Rng;
    let suffix: u32 = rand::thread_rng().gen();
    format!("{prefix}/{suffix:x}.dat")
}

/// Create a fresh on-disk SQLite Db in a temp dir. Returned tempdir lives as
/// long as the caller holds it; dropping it cleans up.
pub async fn tmp_db() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("test.sqlite");
    let db = Db::open(&path).await.expect("open db");
    (db, dir)
}

/// Variant that also inserts a placeholder account row matching the running
/// MinIO. Useful for tests that exercise cache / transfer tables which FK
/// onto accounts. Returns the account id alongside the db handle.
pub async fn tmp_db_with_account() -> (Db, tempfile::TempDir, String) {
    let (db, dir) = tmp_db().await;
    let acct = db
        .insert_account(cosmog_lib::db::accounts::NewAccount {
            name: "test".into(),
            protocol: "s3".into(),
            endpoint: Some(minio_endpoint()),
            region: MINIO_REGION.into(),
            access_key_id: MINIO_ACCESS_KEY.into(),
            addressing_style: Some("path".into()),
        })
        .await
        .expect("insert account");
    (db, dir, acct.id)
}

/// Create + register a fresh test bucket. Returns the name; caller is
/// expected to clean up via `cleanup_bucket` (best-effort).
pub async fn create_test_bucket(store: &Arc<dyn ObjectStore>, prefix: &str) -> String {
    let name = unique_bucket(prefix);
    store
        .create_bucket(&name, None)
        .await
        .expect("create test bucket");
    name
}

/// Best-effort: empty + delete a bucket. Ignores errors (so a test failing
/// mid-flight doesn't break cleanup of a different bucket).
pub async fn cleanup_bucket(store: &Arc<dyn ObjectStore>, bucket: &str) {
    // Page through and batch-delete everything.
    let mut continuation: Option<String> = None;
    loop {
        let page = match store
            .list_objects(
                bucket,
                cosmog_lib::store::ListOptions {
                    prefix: None,
                    delimiter: None,
                    continuation: continuation.clone(),
                    max_keys: Some(1000),
                },
            )
            .await
        {
            Ok(p) => p,
            Err(_) => break,
        };
        if !page.objects.is_empty() {
            let keys: Vec<String> = page.objects.iter().map(|o| o.key.clone()).collect();
            let _ = store.delete_objects(bucket, &keys).await;
        }
        if page.is_truncated {
            continuation = page.continuation;
        } else {
            break;
        }
    }
    let _ = store.delete_bucket(bucket).await;
}

/// Helper to seed a bucket with N small objects whose keys follow a deterministic
/// prefix pattern. Used by sync/search tests.
pub async fn seed_objects(
    store: &Arc<dyn ObjectStore>,
    bucket: &str,
    prefix: &str,
    count: usize,
) {
    use cosmog_lib::store::PutOptions;
    use cosmog_lib::transfer::TransferCtx;

    for i in 0..count {
        let key = format!("{prefix}/item-{i:04}.txt");
        let payload = format!("body-{i}").into_bytes();
        let tmp = tempfile::NamedTempFile::new().expect("tmpfile");
        tokio::fs::write(tmp.path(), &payload).await.expect("write");
        let ctx = TransferCtx::new(format!("seed-{i}"));
        store
            .put_object(
                bucket,
                &key,
                tmp.path().to_path_buf(),
                PutOptions::default(),
                ctx,
            )
            .await
            .expect("put");
    }
}
