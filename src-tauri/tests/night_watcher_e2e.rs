//! Night Watcher end-to-end integration test (MinIO-backed).
//!
//! WHAT THIS COVERS
//! ----------------
//! This drives the *real* Night Watcher reconcile core end to end:
//!   reconcile_watch / reconcile_file  ->  change detection (decide + fast path)
//!   ->  ignore-file matching  ->  encrypt-if-needed  ->  TransferManager upload
//!   ->  object lands in the S3 (MinIO) bucket  ->  nw_file_state persisted.
//!
//! It asserts on ACTUAL remote + DB state (head_object, list_objects,
//! get_object body bytes, nw_file_state rows), not just that a function
//! returned Ok.
//!
//! WHY A `NwCtx` SEAM INSTEAD OF A FULL `AppState`
//! ----------------------------------------------
//! The production entrypoint `reconcile_file(&AppState, ..)` needs an `AppState`,
//! which in turn requires two things that are pure app plumbing, not sync logic:
//! a live `tauri::AppHandle` (`AppState::new` takes one, and the crate does NOT
//! enable tauri's `test` feature, so `mock_app()`/`mock_builder()` are
//! unavailable); and `AppState::store_for`, which pulls the account secret from
//! the OS keyring and wraps the store in a `LoggingStore` that calls
//! `AppHandle::emit`. So
//! the reconcile core was refactored (behavior-preserving) to be generic over a
//! small `NwCtx` trait exposing exactly what it uses (db, db_path, transfers,
//! store_for, claim/unclaim). `AppState` is the sole production implementor and
//! calls its own methods verbatim. Here we supply a `TestCtx` that injects a
//! real MinIO store + real `Db` + real `TransferManager`, bypassing only the
//! keyring/AppHandle.
//!
//! WHAT IS *NOT* COVERED HERE (and why)
//! ------------------------------------
//! - The desktop `notify` filesystem watcher and the periodic `spawn`/`run_once`
//!   scheduler tick: both require a live `AppState` (spawn(state)) and real
//!   inotify + wall-clock timing. They are pure accelerators; the reconcile core
//!   they feed is exactly what is exercised here.
//! - `AppState::store_for` keyring lookup + `LoggingStore` emit path: replaced by
//!   direct store injection (documented above). The store *behavior* (real S3
//!   PUT/HEAD/GET against MinIO) is fully exercised.

// The real reconcile entrypoints are exposed only behind `nw-test-hooks` so
// they never ship in a release binary. Run this file with:
//   cargo test --features nw-test-hooks --test night_watcher_e2e
// Without the feature the file compiles to nothing (plain `cargo test` skips it).
#![cfg(feature = "nw-test-hooks")]

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cosmog_lib::db::accounts::NewAccount;
use cosmog_lib::db::night_watcher::NewWatch;
use cosmog_lib::db::Db;
use cosmog_lib::error::AppResult;
use cosmog_lib::night_watcher::{
    reconcile_dir_for_test, reconcile_file_for_test, reconcile_watch_for_test, rescan_bucket,
    run_once_for_test, NwCtx,
};
use cosmog_lib::store::{GetOptions, ListOptions, ObjectStore};
use cosmog_lib::transfer::{TransferCtx, TransferManager};

/// A minimal `NwCtx` for tests: real Db + TransferManager + an injected MinIO
/// store, plus an in-memory claim set. Everything is `Arc`-shared so `Clone`
/// (required by `NwCtx`) hands out the same underlying state.
#[derive(Clone)]
struct TestCtx {
    db: Db,
    db_path: PathBuf,
    transfers: TransferManager,
    store: Arc<dyn ObjectStore>,
    claims: Arc<Mutex<HashSet<(String, String)>>>,
    scan_claims: Arc<Mutex<HashSet<String>>>,
}

impl NwCtx for TestCtx {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    async fn store_for(&self, _account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        Ok(self.store.clone())
    }
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool {
        self.claims
            .lock()
            .unwrap()
            .insert((watch_id.to_string(), rel_path.to_string()))
    }
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str) {
        self.claims
            .lock()
            .unwrap()
            .remove(&(watch_id.to_string(), rel_path.to_string()));
    }
    fn nw_scan_claim(&self, watch_id: &str) -> bool {
        self.scan_claims.lock().unwrap().insert(watch_id.to_string())
    }
    fn nw_scan_unclaim(&self, watch_id: &str) {
        self.scan_claims.lock().unwrap().remove(watch_id);
    }
}

/// Poll until `f` returns Ok(Some(_)) or the deadline passes. Uploads run in a
/// spawned TransferManager worker and the state-persist runs in a spawned sink
/// task, so reconcile returning Ok(true) only means "enqueued".
async fn wait_for<T, F, Fut>(mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..100 {
        if let Some(v) = f().await {
            return Some(v);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

async fn make_ctx(store: Arc<dyn ObjectStore>) -> (TestCtx, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.sqlite");
    let db = Db::open(&db_path).await.expect("open db");
    let acct = db
        .insert_account(NewAccount {
            name: "nw-e2e".into(),
            protocol: "s3".into(),
            endpoint: Some(common::minio_endpoint()),
            region: common::MINIO_REGION.into(),
            access_key_id: common::MINIO_ACCESS_KEY.into(),
            addressing_style: Some("path".into()),
        })
        .await
        .expect("insert account");
    let transfers = TransferManager::new(db.clone(), 4);
    let ctx = TestCtx {
        db,
        db_path,
        transfers,
        store,
        claims: Arc::new(Mutex::new(HashSet::new())),
        scan_claims: Arc::new(Mutex::new(HashSet::new())),
    };
    (ctx, dir, acct.id)
}

/// Full lifecycle over a real MinIO bucket:
///   1. initial scan uploads all matched files (objects land under key_prefix)
///   2. an unchanged file is NOT re-uploaded (etag stable, decide == Skip)
///   3. a modified file IS re-uploaded (new content lands remotely, etag changes)
///   4. a file matched by the ignore file is NEVER uploaded
///   5. a locally-deleted file: remote object survives (delete_policy=keep) and
///      its nw_file_state row is dropped.
#[tokio::test]
#[serial_test::serial]
async fn e2e_full_sync_lifecycle() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-nw-e2e").await;

    let (ctx, tmp, acct) = make_ctx(store.clone()).await;
    let watch_dir = tempfile::tempdir().expect("watch dir");
    let root = watch_dir.path();

    // Seed local files: two normal, one that will be ignored, one nested.
    tokio::fs::write(root.join("keep.txt"), b"alpha").await.unwrap();
    tokio::fs::create_dir(root.join("sub")).await.unwrap();
    tokio::fs::write(root.join("sub").join("nested.bin"), b"nested-body").await.unwrap();
    tokio::fs::write(root.join("debug.log"), b"should be ignored").await.unwrap();
    // Ignore file: exclude *.log.
    let ignore_path = root.join(".cosmogignore");
    tokio::fs::write(&ignore_path, "*.log\n").await.unwrap();

    // Register the watch in the DB (reconcile reads file_state by watch id).
    ctx.db
        .insert_watch(
            "w-e2e",
            NewWatch {
                account_id: acct.clone(),
                bucket: bucket.clone(),
                local_dir: root.to_string_lossy().to_string(),
                key_prefix: "photos".into(),
                ignore_file: Some(ignore_path.to_string_lossy().to_string()),
                full_scan_secs: 300,
                tree_uri: None,
            },
        )
        .await
        .unwrap();
    let watch = ctx.db.get_watch("w-e2e").await.unwrap().unwrap();
    // insert_watch always uses account/bucket/local_dir we supplied; confirm.
    assert_eq!(watch.bucket, bucket);
    assert_eq!(watch.key_prefix, "photos");

    // 1. initial full scan
    reconcile_watch_for_test(&ctx, &watch).await.unwrap();

    // keep.txt -> photos/keep.txt
    let keep_meta = wait_for(|| {
        let s = store.clone();
        let b = bucket.clone();
        async move { s.head_object(&b, "photos/keep.txt").await.ok() }
    })
    .await
    .expect("keep.txt should have been uploaded to photos/keep.txt");
    assert_eq!(keep_meta.size, 5, "keep.txt body is 5 bytes");

    // nested file -> photos/sub/nested.bin
    let nested_meta = wait_for(|| {
        let s = store.clone();
        let b = bucket.clone();
        async move { s.head_object(&b, "photos/sub/nested.bin").await.ok() }
    })
    .await
    .expect("nested file should have been uploaded with joined prefix + rel path");
    assert_eq!(nested_meta.size, 11);

    // The ignored .log must NOT be present.
    let log_head = store.head_object(&bucket, "photos/debug.log").await;
    assert!(
        log_head.is_err(),
        "*.log was matched by the ignore file and must NOT be uploaded, got {log_head:?}"
    );
    // The ignore file itself is not ignored, but assert the bucket only holds
    // what we expect (no debug.log leaked in under any key).
    let listed = store
        .list_objects(&bucket, ListOptions { prefix: Some("photos/".into()), ..Default::default() })
        .await
        .unwrap();
    let keys: HashSet<String> = listed.objects.iter().map(|o| o.key.clone()).collect();
    assert!(keys.contains("photos/keep.txt"));
    assert!(keys.contains("photos/sub/nested.bin"));
    assert!(!keys.iter().any(|k| k.ends_with("debug.log")), "no .log object, keys={keys:?}");

    // nw_file_state rows persisted for the two uploaded files, with etag.
    let keep_state = wait_for(|| {
        let db = ctx.db.clone();
        async move { db.file_state_get("w-e2e", "keep.txt").await.ok().flatten() }
    })
    .await
    .expect("keep.txt file_state persisted");
    assert!(keep_state.synced_etag.is_some(), "synced_etag recorded on upload done");
    assert_eq!(keep_state.size, 5);
    let first_etag = keep_state.synced_etag.clone().unwrap();
    // ignored file must have NO state row.
    assert!(
        ctx.db.file_state_get("w-e2e", "debug.log").await.unwrap().is_none(),
        "ignored file must not get a state row"
    );

    // 2. re-scan with no changes: nothing re-uploaded
    let did_upload = reconcile_file_for_test(&ctx, &watch, "keep.txt", &root.join("keep.txt"))
        .await
        .unwrap();
    assert!(!did_upload, "unchanged file must be Skip (no re-upload)");

    // 3. modify keep.txt: must re-upload with new content + new etag
    // Sleep so mtime (1s resolution) actually advances past the recorded value.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    tokio::fs::write(root.join("keep.txt"), b"alpha-CHANGED-longer").await.unwrap();
    let did_upload = reconcile_file_for_test(&ctx, &watch, "keep.txt", &root.join("keep.txt"))
        .await
        .unwrap();
    assert!(did_upload, "modified file must enqueue an upload");

    // Wait until the remote object reflects the new size AND the state etag changed.
    let changed = wait_for(|| {
        let s = store.clone();
        let b = bucket.clone();
        let db = ctx.db.clone();
        let prev = first_etag.clone();
        async move {
            let meta = s.head_object(&b, "photos/keep.txt").await.ok()?;
            let st = db.file_state_get("w-e2e", "keep.txt").await.ok().flatten()?;
            let et = st.synced_etag.clone()?;
            if meta.size == 20 && et != prev {
                Some((meta, st))
            } else {
                None
            }
        }
    })
    .await
    .expect("modified file should re-upload: remote size + state etag must change");
    assert_eq!(changed.0.size, 20, "remote object now holds new 20-byte body");

    // Download and verify the actual bytes are the new content.
    let dl_dir = tempfile::tempdir().unwrap();
    let dl = dl_dir.path().join("out");
    store
        .get_object(&bucket, "photos/keep.txt", dl.clone(), GetOptions::default(), TransferCtx::new("verify"))
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&dl).await.unwrap(), b"alpha-CHANGED-longer");

    // 4. add a new file matched by ignore: never uploaded
    tokio::fs::write(root.join("late.log"), b"nope").await.unwrap();
    reconcile_watch_for_test(&ctx, &watch).await.unwrap();
    // Give any (incorrect) upload a chance to land, then assert it did NOT.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        store.head_object(&bucket, "photos/late.log").await.is_err(),
        "newly-added ignored file must not upload"
    );

    // 5. delete a local file: remote survives (keep), state row dropped
    // Deletion is detected by a COMPLETE full scan (mark-and-sweep), not by a
    // single-file reconcile: a lone stat failure is treated as transient and
    // must NOT drop state. So confirm the row survives a single-file reconcile,
    // then a full scan prunes it.
    tokio::fs::remove_file(root.join("sub").join("nested.bin")).await.unwrap();
    let did_upload =
        reconcile_file_for_test(&ctx, &watch, "sub/nested.bin", &root.join("sub").join("nested.bin"))
            .await
            .unwrap();
    assert!(!did_upload, "deleting a local file enqueues no upload");
    // A single missing-file reconcile is transient: state row must be kept.
    assert!(
        ctx.db.file_state_get("w-e2e", "sub/nested.bin").await.unwrap().is_some(),
        "single-file reconcile must not prune (transient stat failures are common)"
    );
    // A full scan sees the whole tree and prunes the vanished file's row.
    reconcile_watch_for_test(&ctx, &watch).await.unwrap();
    // remote object still there (delete_policy = keep).
    assert!(
        store.head_object(&bucket, "photos/sub/nested.bin").await.is_ok(),
        "delete_policy=keep: remote object must survive local deletion"
    );
    // state row forgotten after the full-scan sweep.
    assert!(
        ctx.db.file_state_get("w-e2e", "sub/nested.bin").await.unwrap().is_none(),
        "full-scan mark-and-sweep must drop the nw_file_state row for a deleted file"
    );

    drop(tmp);
    common::cleanup_bucket(&store, &bucket).await;
}

/// A directory moved in can surface as a single event for the dir. Walking it must
/// upload every nested file under the right key and still honor the ignore file.
#[tokio::test]
#[serial_test::serial]
async fn e2e_dir_event_uploads_nested_files() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-nw-dir").await;

    let (ctx, tmp, acct) = make_ctx(store.clone()).await;
    let watch_dir = tempfile::tempdir().expect("watch dir");
    let root = watch_dir.path();
    let ignore_path = root.join(".cosmogignore");
    tokio::fs::write(&ignore_path, "*.log\n").await.unwrap();

    let moved = root.join("moved");
    tokio::fs::create_dir_all(moved.join("deep")).await.unwrap();
    tokio::fs::write(moved.join("a.txt"), b"aaa").await.unwrap();
    tokio::fs::write(moved.join("deep").join("b.txt"), b"bbbb").await.unwrap();
    tokio::fs::write(moved.join("skip.log"), b"ignored").await.unwrap();

    ctx.db
        .insert_watch(
            "w-dir",
            NewWatch {
                account_id: acct.clone(),
                bucket: bucket.clone(),
                local_dir: root.to_string_lossy().to_string(),
                key_prefix: "inbox".into(),
                ignore_file: Some(ignore_path.to_string_lossy().to_string()),
                full_scan_secs: 300,
                tree_uri: None,
            },
        )
        .await
        .unwrap();
    let watch = ctx.db.get_watch("w-dir").await.unwrap().unwrap();

    let enqueued = reconcile_dir_for_test(&ctx, &watch, "moved", &moved).await;
    assert_eq!(enqueued, 2, "two non-ignored files under the dir");

    for (key, size) in [("inbox/moved/a.txt", 3), ("inbox/moved/deep/b.txt", 4)] {
        let meta = wait_for(|| {
            let s = store.clone();
            let b = bucket.clone();
            async move { s.head_object(&b, key).await.ok() }
        })
        .await
        .unwrap_or_else(|| panic!("{key} should be uploaded"));
        assert_eq!(meta.size, size, "{key} size");
    }
    let st = wait_for(|| {
        let db = ctx.db.clone();
        async move { db.file_state_get("w-dir", "moved/deep/b.txt").await.ok().flatten() }
    })
    .await
    .expect("nested file state keyed by its full rel path");
    assert!(st.synced_etag.is_some());

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        store.head_object(&bucket, "inbox/moved/skip.log").await.is_err(),
        "ignored file inside the walked dir must not upload"
    );

    // Second dir event with nothing changed enqueues nothing.
    let again = reconcile_dir_for_test(&ctx, &watch, "moved", &moved).await;
    assert_eq!(again, 0, "unchanged files must be skipped");

    drop(tmp);
    common::cleanup_bucket(&store, &bucket).await;
}

/// Encrypted bucket: reconcile must upload age-encrypted ciphertext with the
/// cosmog metadata markers, and the remote body must NOT be the plaintext.
#[tokio::test]
#[serial_test::serial]
async fn e2e_encrypted_bucket_uploads_ciphertext() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-nw-enc").await;

    let (ctx, tmp, acct) = make_ctx(store.clone()).await;
    let watch_dir = tempfile::tempdir().expect("watch dir");
    let root = watch_dir.path();

    let plaintext = b"top secret night watcher payload";
    tokio::fs::write(root.join("secret.txt"), plaintext).await.unwrap();

    // Enable encryption for (account, bucket): generate an age identity, store
    // the recipient. Uses the real crypto path the encrypt helper reads.
    let recipient = enable_bucket_encryption(&ctx.db, &acct, &bucket).await;

    ctx.db
        .insert_watch(
            "w-enc",
            NewWatch {
                account_id: acct.clone(),
                bucket: bucket.clone(),
                local_dir: root.to_string_lossy().to_string(),
                key_prefix: String::new(),
                ignore_file: None,
                full_scan_secs: 300,
                tree_uri: None,
            },
        )
        .await
        .unwrap();
    let watch = ctx.db.get_watch("w-enc").await.unwrap().unwrap();

    reconcile_file_for_test(&ctx, &watch, "secret.txt", &root.join("secret.txt"))
        .await
        .unwrap();

    let meta = wait_for(|| {
        let s = store.clone();
        let b = bucket.clone();
        async move { s.head_object(&b, "secret.txt").await.ok() }
    })
    .await
    .expect("encrypted object should upload");

    // Metadata markers set by encrypt_for_bucket_if_needed.
    assert_eq!(meta.user_metadata.get("cosmog-encrypted").map(String::as_str), Some("1"));
    assert_eq!(
        meta.user_metadata.get("cosmog-recipient").map(String::as_str),
        Some(recipient.as_str())
    );
    // Ciphertext is larger than plaintext and does NOT equal it.
    let dl_dir = tempfile::tempdir().unwrap();
    let dl = dl_dir.path().join("cipher");
    store
        .get_object(&bucket, "secret.txt", dl.clone(), GetOptions::default(), TransferCtx::new("v"))
        .await
        .unwrap();
    let body = tokio::fs::read(&dl).await.unwrap();
    assert_ne!(body.as_slice(), plaintext, "remote body must be ciphertext, not plaintext");
    assert!(
        cosmog_lib::crypto::is_age_ciphertext(&body),
        "remote body must be an age v1 ciphertext, got prefix {:?}",
        &body[..body.len().min(22)]
    );

    drop(tmp);
    common::cleanup_bucket(&store, &bucket).await;
}

/// Generate a real age identity and persist the recipient in `bucket_encryption`
/// via the same `Db::set_encryption_config` the enable command uses. Upload only
/// needs the public recipient to encrypt, so we don't touch the keyring.
async fn enable_bucket_encryption(db: &Db, account_id: &str, bucket: &str) -> String {
    let (_secret, recipient) = cosmog_lib::crypto::new_identity();
    db.set_encryption_config(account_id, bucket, &recipient)
        .await
        .expect("set encryption config");
    recipient
}

/// Full scan into an encrypted bucket: every object must decrypt to the exact local bytes.
#[tokio::test]
#[serial_test::serial]
async fn e2e_encrypted_scan_roundtrips_exact_bytes() {
    use rand::RngCore;

    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-nw-encrt").await;

    let (ctx, tmp, acct) = make_ctx(store.clone()).await;
    let watch_dir = tempfile::tempdir().expect("watch dir");
    let root = watch_dir.path();

    // Empty, small, nested, and one above the 8 MiB multipart threshold.
    let mut big = vec![0u8; 20 * 1024 * 1024 + 4321];
    rand::thread_rng().fill_bytes(&mut big);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("empty.bin", Vec::new()),
        ("small.txt", b"night watcher secret".to_vec()),
        ("sub/dir/nested.txt", b"nested secret body".to_vec()),
        ("big.bin", big),
    ];
    for (rel, body) in &files {
        let p = root.join(rel);
        tokio::fs::create_dir_all(p.parent().unwrap()).await.unwrap();
        tokio::fs::write(&p, body).await.unwrap();
    }

    let (secret, recipient) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, &bucket, &recipient).await.unwrap();

    ctx.db
        .insert_watch(
            "w-encrt",
            NewWatch {
                account_id: acct.clone(),
                bucket: bucket.clone(),
                local_dir: root.to_string_lossy().to_string(),
                key_prefix: "vault".into(),
                ignore_file: None,
                full_scan_secs: 300,
                tree_uri: None,
            },
        )
        .await
        .unwrap();
    let watch = ctx.db.get_watch("w-encrt").await.unwrap().unwrap();

    reconcile_watch_for_test(&ctx, &watch).await.unwrap();

    let dl_dir = tempfile::tempdir().unwrap();
    for (i, (rel, body)) in files.iter().enumerate() {
        let st = wait_for(|| {
            let db = ctx.db.clone();
            async move { db.file_state_get("w-encrt", rel).await.ok().flatten() }
        })
        .await
        .unwrap_or_else(|| panic!("{rel}: no synced state row"));
        // The fingerprint describes the plaintext, never the ciphertext.
        assert_eq!(st.size, body.len() as i64, "{rel}: state size");
        assert_eq!(st.hash, blake3::hash(body).to_hex().to_string(), "{rel}: state hash");

        let key = format!("vault/{rel}");
        let meta = store.head_object(&bucket, &key).await.unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(meta.user_metadata.get("cosmog-encrypted").map(String::as_str), Some("1"), "{key}");
        assert!(meta.size > body.len() as i64, "{key}: ciphertext must be larger than plaintext");

        let enc = dl_dir.path().join(format!("{i}.age"));
        let dec = dl_dir.path().join(format!("{i}.plain"));
        store
            .get_object(&bucket, &key, enc.clone(), GetOptions::default(), TransferCtx::new("v"))
            .await
            .unwrap();
        let cipher = tokio::fs::read(&enc).await.unwrap();
        assert_eq!(cipher.len() as i64, meta.size, "{key}: downloaded size");
        assert!(cosmog_lib::crypto::is_age_ciphertext(&cipher), "{key}: not age ciphertext");
        let identity = cosmog_lib::crypto::parse_identity(&secret).unwrap();
        cosmog_lib::crypto::decrypt_file(&enc, &dec, identity)
            .await
            .unwrap_or_else(|e| panic!("{key}: decrypt failed: {e}"));
        let plain = tokio::fs::read(&dec).await.unwrap();
        assert!(plain == *body, "{key}: decrypted bytes differ from the local file");
    }

    // Temp ciphertext is gone once the workers settled.
    let enc_tmp = tmp.path().join("enc_tmp").join("nightwatch");
    let leftovers = wait_for(|| {
        let d = enc_tmp.clone();
        async move {
            let n = std::fs::read_dir(&d).map(|rd| rd.count()).unwrap_or(0);
            (n == 0).then_some(())
        }
    })
    .await;
    assert!(leftovers.is_some(), "enc_tmp must be empty after uploads settle");

    // A second scan re-uploads nothing.
    let before = store.head_object(&bucket, "vault/big.bin").await.unwrap().etag;
    reconcile_watch_for_test(&ctx, &watch).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let active = ctx.transfers.list(None).await.unwrap();
    assert_eq!(active.len(), files.len(), "second scan must enqueue nothing: {active:?}");
    assert_eq!(store.head_object(&bucket, "vault/big.bin").await.unwrap().etag, before);

    drop(tmp);
    common::cleanup_bucket(&store, &bucket).await;
}

/// Remote body of `key` plus whether it carries the encrypted marker; `None` if absent.
async fn fetch_remote(store: &Arc<dyn ObjectStore>, bucket: &str, key: &str) -> Option<(Vec<u8>, bool)> {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("body");
    let res = store
        .get_object(bucket, key, dst.clone(), GetOptions::default(), TransferCtx::new("v"))
        .await
        .ok()?;
    let marked = res.user_metadata.get("cosmog-encrypted").map(String::as_str) == Some("1");
    Some((tokio::fs::read(&dst).await.unwrap(), marked))
}

/// Rescan, then poll until the remote object satisfies `ok`; returns its body.
async fn rescan_until<F>(ctx: &TestCtx, watch_id: &str, bucket: &str, key: &str, ok: F) -> Option<Vec<u8>>
where
    F: Fn(&[u8], bool) -> bool,
{
    let watch = ctx.db.get_watch(watch_id).await.unwrap().unwrap();
    reconcile_watch_for_test(ctx, &watch).await.unwrap();
    for _ in 0..50 {
        if let Some((body, marked)) = fetch_remote(&ctx.store, bucket, key).await {
            if ok(&body, marked) {
                return Some(body);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// Files synced before the bucket's encryption changed must be re-uploaded under the new
/// state: enable encrypts them, rotate re-encrypts to the new key, disable restores plaintext.
#[tokio::test]
#[serial_test::serial]
async fn e2e_encryption_change_reuploads_synced_files() {
    require_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-nw-encchg").await;

    let (ctx, tmp, acct) = make_ctx(store.clone()).await;
    let watch_dir = tempfile::tempdir().expect("watch dir");
    let root = watch_dir.path();
    let plaintext = b"synced before encryption was enabled".to_vec();
    tokio::fs::write(root.join("doc.txt"), &plaintext).await.unwrap();

    ctx.db
        .insert_watch(
            "w-chg",
            NewWatch {
                account_id: acct.clone(),
                bucket: bucket.clone(),
                local_dir: root.to_string_lossy().to_string(),
                key_prefix: String::new(),
                ignore_file: None,
                full_scan_secs: 300,
                tree_uri: None,
            },
        )
        .await
        .unwrap();

    // 1. Plaintext bucket: the object is the raw file.
    let body = rescan_until(&ctx, "w-chg", &bucket, "doc.txt", |_, marked| !marked).await;
    assert_eq!(body.as_deref(), Some(plaintext.as_slice()));
    wait_for(|| {
        let db = ctx.db.clone();
        async move { db.file_state_get("w-chg", "doc.txt").await.ok().flatten() }
    })
    .await
    .expect("plaintext sync recorded");

    let decrypt = |secret: String, cipher: Vec<u8>| {
        let id = cosmog_lib::crypto::parse_identity(&secret).unwrap();
        cosmog_lib::crypto::decrypt_bytes(&id, &cipher)
    };

    // 2. Enable encryption: the unchanged local file must be re-uploaded as ciphertext.
    let (secret_a, recipient_a) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, &bucket, &recipient_a).await.unwrap();
    let cipher_a = rescan_until(&ctx, "w-chg", &bucket, "doc.txt", |b, marked| {
        marked && cosmog_lib::crypto::is_age_ciphertext(b)
    })
    .await
    .expect("object must be re-uploaded encrypted after enabling encryption");
    assert_eq!(decrypt(secret_a.clone(), cipher_a.clone()).unwrap(), plaintext);

    // Settled: another scan must not upload again.
    wait_for(|| {
        let db = ctx.db.clone();
        async move { db.file_state_get("w-chg", "doc.txt").await.ok().flatten() }
    })
    .await
    .expect("encrypted sync recorded");
    let watch = ctx.db.get_watch("w-chg").await.unwrap().unwrap();
    let again = reconcile_file_for_test(&ctx, &watch, "doc.txt", &root.join("doc.txt")).await.unwrap();
    assert!(!again, "file already synced under the current key must be skipped");

    // 3. Rotate: the object must become decryptable with the new key only.
    let (secret_b, recipient_b) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, &bucket, &recipient_b).await.unwrap();
    let cipher_b = rescan_until(&ctx, "w-chg", &bucket, "doc.txt", |b, marked| {
        marked && decrypt(secret_b.clone(), b.to_vec()).is_ok()
    })
    .await
    .expect("object must be re-encrypted to the rotated key");
    assert_eq!(decrypt(secret_b.clone(), cipher_b.clone()).unwrap(), plaintext);
    assert!(decrypt(secret_a, cipher_b).is_err(), "old key must not decrypt the rotated object");

    // 4. Disable: the object goes back to plaintext.
    ctx.db.delete_encryption_config(&acct, &bucket).await.unwrap();
    let body = rescan_until(&ctx, "w-chg", &bucket, "doc.txt", |_, marked| !marked)
        .await
        .expect("object must be re-uploaded as plaintext after disabling encryption");
    assert_eq!(body, plaintext);

    drop(tmp);
    common::cleanup_bucket(&store, &bucket).await;
}

/// An encryption change must make watches on that bucket due on the next tick despite their cadence.
/// Empty watch dirs, so no S3 call is made and MinIO is not needed.
#[tokio::test]
#[serial_test::serial]
async fn encryption_change_forces_scan_of_that_bucket_only() {
    let (ctx, tmp, acct) = make_ctx(common::make_store().await).await;
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    for (id, bucket, dir) in [("w-enc", "enc-bucket", &dir_a), ("w-other", "other-bucket", &dir_b)] {
        ctx.db
            .insert_watch(
                id,
                NewWatch {
                    account_id: acct.clone(),
                    bucket: bucket.into(),
                    local_dir: dir.path().to_string_lossy().to_string(),
                    key_prefix: String::new(),
                    ignore_file: None,
                    full_scan_secs: 300,
                    tree_uri: None,
                },
            )
            .await
            .unwrap();
    }
    // Both scanned a minute ago: not due by cadence.
    let past = chrono::Utc::now().timestamp() - 60;
    for id in ["w-enc", "w-other"] {
        ctx.db.set_watch_scan_result(id, past, None).await.unwrap();
    }
    let last_scan = |id: &'static str| {
        let db = ctx.db.clone();
        async move { db.get_watch(id).await.unwrap().unwrap().last_scan_at }
    };

    run_once_for_test(&ctx).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(last_scan("w-enc").await, Some(past), "no change yet: cadence holds");
    assert_eq!(last_scan("w-other").await, Some(past));

    enable_bucket_encryption(&ctx.db, &acct, "enc-bucket").await;
    rescan_bucket(&ctx.db, &acct, "enc-bucket").await;
    run_once_for_test(&ctx).await.unwrap();

    let scanned = wait_for(|| async {
        last_scan("w-enc").await.filter(|t| *t > past)
    })
    .await;
    assert!(scanned.is_some(), "watch on the changed bucket must scan on the next tick");
    let w = ctx.db.get_watch("w-enc").await.unwrap().unwrap();
    assert_eq!(w.last_error, None, "forced scan must complete cleanly");
    assert_eq!(last_scan("w-other").await, Some(past), "watch on another bucket must not scan");

    // The force is one-shot: the following tick leaves both alone.
    let after = last_scan("w-enc").await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    run_once_for_test(&ctx).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(last_scan("w-enc").await, after, "force flag must be consumed");
    assert_eq!(last_scan("w-other").await, Some(past));

    drop(tmp);
}
