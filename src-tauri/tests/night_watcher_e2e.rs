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
use cosmog_lib::night_watcher::{reconcile_file_for_test, reconcile_watch_for_test, NwCtx};
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

    // ── 1. initial full scan ─────────────────────────────────────────────
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

    // ── 2. re-scan with no changes: nothing re-uploaded ──────────────────
    let did_upload = reconcile_file_for_test(&ctx, &watch, "keep.txt", &root.join("keep.txt"))
        .await
        .unwrap();
    assert!(!did_upload, "unchanged file must be Skip (no re-upload)");

    // ── 3. modify keep.txt: must re-upload with new content + new etag ───
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

    // ── 4. add a new file matched by ignore: never uploaded ──────────────
    tokio::fs::write(root.join("late.log"), b"nope").await.unwrap();
    reconcile_watch_for_test(&ctx, &watch).await.unwrap();
    // Give any (incorrect) upload a chance to land, then assert it did NOT.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        store.head_object(&bucket, "photos/late.log").await.is_err(),
        "newly-added ignored file must not upload"
    );

    // ── 5. delete a local file: remote survives (keep), state row dropped ─
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
