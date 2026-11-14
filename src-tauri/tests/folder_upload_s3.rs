//! Folder upload safety against real S3 semantics (MinIO): conditional writes on every upload
//! path, resume under the same upload after a network loss, oversized saved layouts.
//! Set FU_REQUIRE_MINIO=1 to fail instead of skip when MinIO is down.

mod common;
#[path = "common/flaky_store.rs"]
mod flaky_store;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use cosmog_lib::db::folder_upload::{JobState, OverwritePolicy};
use cosmog_lib::db::transfers::TransferStatus;
use cosmog_lib::folder_upload as fu;
use cosmog_lib::store::{ObjectStore, PutOptions};
use cosmog_lib::transfer::{ResumeState, SourceStat, TransferCtx, TransferEvent};
use flaky_store::FlakyStore;
use fu_ctx::{analyze_checked, body, make_ctx, small_parts, wait_settled, wait_until, write};

const MIB: usize = 1024 * 1024;

macro_rules! need_minio {
    () => {
        if !common::minio_available().await {
            assert!(std::env::var("FU_REQUIRE_MINIO").is_err(), "MinIO required but not reachable");
            eprintln!("skipping: MinIO not reachable at {}", common::minio_endpoint());
            return;
        }
        eprintln!("minio reachable: running");
    };
}

fn ctx_for(part_mib: u64) -> TransferCtx {
    let mut c = TransferCtx::new("t");
    c.part_size = part_mib * MIB as u64;
    c.multipart_threshold = 5 * MIB as u64;
    c.parallelism = 1;
    c
}

async fn abort_all(store: &Arc<dyn ObjectStore>, bucket: &str) -> usize {
    let (ups, _) = store.list_multipart_uploads(bucket, None, None).await.unwrap();
    for u in &ups {
        store.abort_multipart_upload(bucket, &u.key, &u.upload_id).await.unwrap();
    }
    ups.len()
}

/// H4: If-None-Match is honored on single PUT, on CompleteMultipartUpload and for folder markers.
#[tokio::test]
#[serial_test::serial]
async fn conditional_writes_never_replace_existing_objects() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-cond").await;
    let src = tempfile::NamedTempFile::new().unwrap();
    let inm = PutOptions { if_none_match: Some("*".into()), ..Default::default() };
    for (key, len) in [("small.txt", 1024usize), ("big.bin", 12 * MIB)] {
        store.put_object_bytes(&bucket, key, "text/plain", b"theirs".to_vec(), Default::default()).await.unwrap();
        std::fs::write(src.path(), body(len, 1)).unwrap();
        let e = store.put_object(&bucket, key, src.path().into(), inm.clone(), ctx_for(5)).await.unwrap_err();
        assert_eq!(e.code(), "conflict", "{key}");
        assert_eq!(store.read_object_full(&bucket, key).await.unwrap(), b"theirs", "{key} replaced");
    }
    // The refused multipart upload stays open for its owner (the worker) to abort.
    assert_eq!(abort_all(&store, &bucket).await, 1);
    // Absent key: the condition passes on both paths.
    for (key, len) in [("new-small", 1024usize), ("new-big", 11 * MIB)] {
        std::fs::write(src.path(), body(len, 2)).unwrap();
        store.put_object(&bucket, key, src.path().into(), inm.clone(), ctx_for(5)).await.unwrap();
        assert_eq!(store.read_object_full(&bucket, key).await.unwrap(), body(len, 2));
    }
    // L4: a marker never replaces an existing object at its key.
    store.put_object_bytes(&bucket, "dir/", "text/plain", b"data".to_vec(), Default::default()).await.unwrap();
    assert!(!store.create_folder_if_absent(&bucket, "dir").await.unwrap());
    assert_eq!(store.read_object_full(&bucket, "dir/").await.unwrap(), b"data");
    assert!(store.create_folder_if_absent(&bucket, "fresh").await.unwrap());
    assert_eq!(store.read_object_full(&bucket, "fresh/").await.unwrap(), b"");
    common::cleanup_bucket(&store, &bucket).await;
}

/// H1: network lost mid-file on a real provider: the job waits, then continues the same
/// multipart upload (ListParts) without re-sending parts, and completes it conditionally.
#[tokio::test]
#[serial_test::serial]
async fn network_loss_resumes_the_same_upload() {
    need_minio!();
    let real = common::make_store().await;
    let bucket = common::create_test_bucket(&real, "cosmog-fu-net").await;
    let flaky = FlakyStore::new(real.clone());
    let (ctx, _db, acct) = make_ctx(flaky.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    let big = body(26 * MIB, 9);
    write(src.path(), "big.bin", &big);
    let id = analyze_checked(&ctx, &acct, &bucket, src.path(), "n/").await;
    let seen = Arc::new(Mutex::new((BTreeSet::new(), Vec::new(), 0usize)));
    let s = seen.clone();
    ctx.set_hook(move |ev| {
        let mut g = s.lock().unwrap();
        match ev {
            TransferEvent::MultipartInitiated { upload_id, .. } => g.0.insert(upload_id.clone()),
            TransferEvent::PartCompleted { part_number, .. } => {
                g.1.push(*part_number);
                true
            }
            _ => true,
        };
    });
    *flaky.cut_after_parts.lock().unwrap() = Some(2);
    fu::approve(&ctx, &id, Some(OverwritePolicy::Skip), true).await.unwrap();
    let (c, i) = (ctx.clone(), id.clone());
    wait_until(30, move || {
        let (c, i) = (c.clone(), i.clone());
        async move { c.db.list_job_transfers(&i).await.unwrap().iter().any(|r| r.status == TransferStatus::Paused) }
    })
    .await;
    let row = ctx.db.list_job_transfers(&id).await.unwrap().remove(0);
    let upload_id = row.upload_id.clone().expect("upload kept across the network loss");
    let (open, _) = real.list_multipart_uploads(&bucket, Some("n/big.bin"), None).await.unwrap();
    assert_eq!(open.iter().map(|u| u.upload_id.clone()).collect::<Vec<_>>(), vec![upload_id.clone()]);
    assert_eq!(fu::get_job(&ctx, &id).await.unwrap().waiting, Some("network"));

    flaky.offline.store(false, std::sync::atomic::Ordering::SeqCst);
    let job = wait_settled(&ctx, &id, 60).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");
    let (ids, parts, _) = seen.lock().unwrap().clone();
    assert_eq!(ids, BTreeSet::from([upload_id]), "one multipart upload end to end");
    let distinct: BTreeSet<i32> = parts.iter().copied().collect();
    assert_eq!(distinct.len(), parts.len(), "a part was sent twice: {parts:?}");
    assert_eq!(distinct, (1..=6).collect());
    assert_eq!(real.read_object_full(&bucket, "n/big.bin").await.unwrap(), big);
    let meta = real.head_object(&bucket, "n/big.bin").await.unwrap();
    assert!(meta.user_metadata.get("cosmog-fu").is_some_and(|t| t.starts_with(&id)), "tagged as this job's write");
    assert_eq!(abort_all(&real, &bucket).await, 0, "no multipart upload left");
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
    common::cleanup_bucket(&real, &bucket).await;
}

/// L5: a saved part size that would need more than 10,000 parts restarts with the adaptive size.
#[tokio::test]
#[serial_test::serial]
async fn oversized_saved_layout_restarts_fresh() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-l5").await;
    let src = tempfile::NamedTempFile::new().unwrap();
    let data = body(11 * MIB, 4);
    std::fs::write(src.path(), &data).unwrap();
    let m = std::fs::metadata(src.path()).unwrap();
    let stat = SourceStat {
        len: m.len(),
        mtime_secs: m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
    };
    let old = Arc::new(Mutex::new(String::new()));
    let o = old.clone();
    let mut ctx = ctx_for(5);
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    ctx = ctx.with_cancel(cancel).with_progress(cosmog_lib::transfer::ProgressSink::from_fn(move |ev| {
        if let TransferEvent::PartCompleted { upload_id, .. } = ev {
            *o.lock().unwrap() = upload_id;
            c.cancel();
        }
    }));
    assert!(store.put_object(&bucket, "l5.bin", src.path().into(), PutOptions::default(), ctx).await.is_err());
    let old_id = old.lock().unwrap().clone();
    assert!(!old_id.is_empty());
    let mut ctx = ctx_for(5);
    ctx.source_stat = Some(stat);
    ctx.resume = Some(ResumeState {
        upload_id: old_id.clone(),
        completed_parts: vec![],
        source_len: Some(stat.len),
        source_mtime_secs: Some(stat.mtime_secs),
        // 1 KiB parts: 11,264 parts, past the S3 limit.
        part_size: Some(1024),
    });
    let r = store.put_object(&bucket, "l5.bin", src.path().into(), PutOptions::default(), ctx).await.unwrap();
    assert_ne!(r.upload_id.as_deref(), Some(old_id.as_str()), "restarted on a new upload");
    assert_eq!(store.read_object_full(&bucket, "l5.bin").await.unwrap(), data);
    assert_eq!(abort_all(&store, &bucket).await, 0, "the old upload was aborted");
    common::cleanup_bucket(&store, &bucket).await;
}
