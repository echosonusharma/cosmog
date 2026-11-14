//! Folder upload on the in-memory store: network loss mid-file, overwrite safety (conditional
//! writes, lost responses, own re-uploads) and restart verification.

mod common;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;
#[path = "common/mem_store.rs"]
mod mem_store;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use cosmog_lib::db::folder_upload::{ItemState, JobState, OverwritePolicy};
use cosmog_lib::db::transfers::TransferStatus;
use cosmog_lib::folder_upload as fu;
use cosmog_lib::transfer::TransferEvent;
use fu_ctx::{analyze_checked, body, item_at, make_ctx, small_parts, wait_settled, wait_until, write};
use mem_store::MemStore;

const MIB: usize = 1024 * 1024;
const SKIP: Option<OverwritePolicy> = Some(OverwritePolicy::Skip);

/// H1: a network loss mid-file parks the row with its upload; back online, the same upload
/// continues and no part is sent twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_loss_mid_file_keeps_the_multipart_upload() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    let big = body(16 * MIB, 3);
    write(src.path(), "big.bin", &big);
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "n/").await;
    let parts = Arc::new(AtomicUsize::new(0));
    let (p, m) = (parts.clone(), mem.clone());
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::PartCompleted { .. }) && p.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
            *m.fail.lock().unwrap() = Some("network_unreachable");
        }
    });
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let (c, i) = (ctx.clone(), id.clone());
    wait_until(20, move || {
        let (c, i) = (c.clone(), i.clone());
        async move {
            let rows = c.db.list_job_transfers(&i).await.unwrap();
            fu::get_job(&c, &i).await.unwrap().waiting == Some("network") && rows.iter().any(|r| r.status == TransferStatus::Paused)
        }
    })
    .await;
    let rows = ctx.db.list_job_transfers(&id).await.unwrap();
    assert_eq!(rows.len(), 1);
    let upload_id = rows[0].upload_id.clone().expect("parked row keeps its upload_id");
    assert_eq!(mem.open_uploads(), vec![upload_id.clone()], "upload still open on the provider");
    assert!(mem.aborted.lock().unwrap().is_empty(), "nothing aborted");
    let it = item_at(&ctx, &id, "big.bin").await;
    assert_eq!((it.state, it.attempts), (ItemState::Pending, 0), "first loss is not counted");

    *mem.fail.lock().unwrap() = None;
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");
    assert_eq!(mem.objects.lock().unwrap()["n/big.bin"], big);
    let sent = mem.parts_sent.lock().unwrap().clone();
    assert!(sent.iter().all(|(u, _)| *u == upload_id), "one upload end to end: {sent:?}");
    let distinct: BTreeSet<i32> = sent.iter().map(|(_, n)| *n).collect();
    assert_eq!(distinct.len(), sent.len(), "a part was sent twice: {sent:?}");
    assert_eq!(distinct, (1..=4).collect());
    assert!(mem.open_uploads().is_empty());
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
}

/// H1: online but the same file keeps timing out without progress: counted, then failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endless_network_failures_end_as_a_visible_failure() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "stuck.bin", &body(6 * MIB, 1));
    write(src.path(), "ok.txt", b"fine");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    *mem.part_fail.lock().unwrap() = Some("request_timeout");
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 120).await;
    assert_eq!(job.state, JobState::CompletedWithErrors);
    let it = item_at(&ctx, &id, "stuck.bin").await;
    assert_eq!(it.state, ItemState::Failed);
    assert!(it.error.as_deref().unwrap().contains("request_timeout"));
    assert_eq!(it.attempts, 4, "counted only after the network came back");
    assert_eq!(mem.objects.lock().unwrap()["ok.txt"], b"fine");
    assert!(mem.open_uploads().is_empty(), "the failed file's upload is aborted");
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
}

/// tls_error is a certificate problem: pause with the code, never "waiting for network".
#[tokio::test]
async fn tls_error_pauses_instead_of_waiting() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a", b"a");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    *mem.fail.lock().unwrap() = Some("tls_error");
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    assert_eq!(job.state, JobState::Paused);
    assert_eq!(job.pause_reason.as_deref(), Some("tls_error"));
    assert_eq!(job.waiting, None);
}

/// H4: no policy (nothing existed at the check) still never overwrites a key written later.
#[tokio::test]
async fn no_policy_never_overwrites_a_late_key() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "small.txt", b"mine");
    write(src.path(), "big.bin", &body(12 * MIB, 2));
    write(src.path(), "free.txt", b"free");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    for k in ["small.txt", "big.bin"] {
        mem.put_object_bytes_sync(k, b"theirs");
    }
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.counts.skipped_exists, 2);
    assert_eq!(job.appeared_after_check, 2);
    assert_eq!(job.counts.done, 1);
    for k in ["small.txt", "big.bin"] {
        assert_eq!(mem.objects.lock().unwrap()[k], b"theirs", "{k} overwritten");
    }
    assert!(mem.open_uploads().is_empty(), "refused multipart upload aborted");
}

/// H4: a key created while a multipart upload runs is caught at completion, natively or by the
/// HEAD fallback of a provider that rejects If-None-Match.
#[tokio::test]
async fn key_created_during_multipart_is_never_replaced() {
    for unsupported in [false, true] {
        let mem = MemStore::new();
        *mem.inm_unsupported.lock().unwrap() = unsupported;
        let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
        small_parts(&ctx).await;
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "big.bin", &body(12 * MIB, 4));
        let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
        let m = mem.clone();
        ctx.set_hook(move |ev| {
            if let TransferEvent::PartCompleted { part_number: 1, .. } = ev {
                m.put_object_bytes_sync("big.bin", b"raced in");
            }
        });
        fu::approve(&ctx, &id, SKIP, true).await.unwrap();
        let job = wait_settled(&ctx, &id, 30).await;
        assert_eq!((job.counts.skipped_exists, job.appeared_after_check), (1, 1), "unsupported={unsupported}");
        assert_eq!(mem.objects.lock().unwrap()["big.bin"], b"raced in", "unsupported={unsupported}");
        assert!(mem.open_uploads().is_empty());
    }
}

/// H3: a PUT that landed but whose response was lost is this job's own write, not "appeared".
#[tokio::test]
async fn lost_response_is_not_reported_as_existing() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a.txt", b"payload");
    write(src.path(), "big.bin", &body(11 * MIB, 6));
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    mem.lose_responses.store(2, Ordering::SeqCst);
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 60).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");
    assert_eq!((job.counts.done, job.counts.skipped_exists, job.appeared_after_check), (2, 0, 0));
    assert_eq!(mem.objects.lock().unwrap()["a.txt"], b"payload");
    assert_eq!(mem.objects.lock().unwrap()["big.bin"], body(11 * MIB, 6));
    assert!(mem.open_uploads().is_empty());
}

/// H3: under Skip, a file changed while uploading is re-uploaded over this job's own object.
#[tokio::test]
async fn own_reupload_after_change_replaces_only_our_object() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    let p = src.path().join("hot.bin");
    write(src.path(), "hot.bin", &body(7 * MIB, 8));
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    let fired = Arc::new(AtomicBool::new(false));
    let (f, pc) = (fired.clone(), p.clone());
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::PartCompleted { .. }) && !f.swap(true, Ordering::SeqCst) {
            use std::io::Write;
            std::fs::OpenOptions::new().append(true).open(&pc).unwrap().write_all(b"tail").unwrap();
        }
    });
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");
    assert_eq!((job.counts.done, job.counts.skipped_exists, job.appeared_after_check), (1, 0, 0));
    assert_eq!(mem.objects.lock().unwrap()["hot.bin"], std::fs::read(&p).unwrap(), "current bytes, not torn");
}

/// M1: crash after the upload but before the item update: verified against the bucket, not re-sent.
#[tokio::test]
async fn restart_verifies_an_uploaded_item_instead_of_resending() {
    for policy in [SKIP, Some(OverwritePolicy::Overwrite)] {
        let mem = MemStore::new();
        let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "a.txt", b"one");
        write(src.path(), "b.txt", b"two");
        let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
        fu::approve(&ctx, &id, policy, true).await.unwrap();
        assert_eq!(wait_settled(&ctx, &id, 10).await.state, JobState::Completed);
        // The state the old order (row deleted, item not yet done) left behind.
        for r in ["a.txt", "b.txt"] {
            let seq = item_at(&ctx, &id, r).await.seq;
            ctx.db.fu_item_started(&id, seq, "gone-row").await.unwrap();
        }
        ctx.db.fu_set_state(&id, JobState::Running, None, None).await.unwrap();
        write(src.path(), "b.txt", b"two, edited");
        let puts = *mem.puts.lock().unwrap();
        let ctx2 = ctx.restarted(mem.clone(), 2);
        fu::resume_all(&ctx2).await;
        let job = wait_settled(&ctx2, &id, 10).await;
        assert_eq!(job.state, JobState::Completed, "{job:?}");
        assert_eq!(*mem.puts.lock().unwrap(), puts + 1, "only the edited file is sent again ({policy:?})");
        assert_eq!(mem.objects.lock().unwrap()["b.txt"], b"two, edited");
        assert_eq!((job.counts.done, job.counts.skipped_exists, job.appeared_after_check), (2, 0, 0));
    }
}

/// H4: a listing that claims more but gives no continuation token fails the check.
#[tokio::test]
async fn truncated_listing_without_token_fails_the_check() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a", b"a");
    let a = fu::analyze(&ctx, &acct, "b", src.path().to_str().unwrap(), "", |_| {}).await.unwrap();
    *mem.truncate_listing.lock().unwrap() = true;
    let e = fu::check_remote(&ctx, &a.job_id, "", |_| {}).await.unwrap_err();
    assert_eq!(e.code(), "s3");
    let job = ctx.db.fu_get_job(&a.job_id).await.unwrap();
    assert_eq!(job.remote_state, cosmog_lib::db::folder_upload::RemoteState::Failed);
    assert_eq!(fu::approve(&ctx, &a.job_id, SKIP, true).await.unwrap_err().code(), "invalid_input");
}
