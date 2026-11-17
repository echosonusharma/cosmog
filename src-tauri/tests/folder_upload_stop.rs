//! A PUT that lands while the job is stopped (cancel, pause, app quit) is counted as uploaded
//! and never sent again; "canceled" only ever means "not in the bucket".

mod common;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;
#[path = "common/mem_store.rs"]
mod mem_store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosmog_lib::db::folder_upload::{ItemState, JobState, OverwritePolicy};
use cosmog_lib::folder_upload as fu;
use cosmog_lib::transfer::TransferEvent;
use fu_ctx::{analyze_checked, item_at, make_ctx, wait_settled, write, Ctx};
use mem_store::MemStore;

const FILES: [&str; 4] = ["a.txt", "b.txt", "c.txt", "d.txt"];

/// Four small files, one transfer slot; every PUT lands, then waits `post` before answering.
async fn setup(post: Duration) -> (Arc<MemStore>, Ctx, tempfile::TempDir, tempfile::TempDir, String) {
    let mem = MemStore::new();
    *mem.post_commit_delay.lock().unwrap() = post;
    let (ctx, db, acct) = make_ctx(mem.clone(), 1).await;
    let src = tempfile::tempdir().unwrap();
    for f in FILES {
        write(src.path(), f, f.as_bytes());
    }
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    (mem, ctx, db, src, id)
}

/// Runs `stop` once the first PUT has landed (its response still pending).
fn on_first_landing(ctx: &Ctx, stop: impl Fn() + Send + Sync + 'static) {
    let fired = AtomicBool::new(false);
    ctx.set_hook(move |ev| {
        if let TransferEvent::Progress { bytes_done, bytes_total: Some(t), .. } = ev {
            if bytes_done == t && !fired.swap(true, Ordering::SeqCst) {
                stop();
            }
        }
    });
}

async fn bucket_matches_done(mem: &MemStore, ctx: &Ctx, id: &str) {
    let job = fu::get_job(ctx, id).await.unwrap();
    assert_eq!(mem.keys().len() as i64, job.counts.done, "every object in the bucket is counted done");
    for f in FILES {
        let it = item_at(ctx, id, f).await;
        let present = mem.keys().contains(&f.to_string());
        assert_eq!(it.state == ItemState::Done, present, "{f}: {:?} but present={present}", it.state);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn put_landing_during_cancel_counts_as_uploaded() {
    let (mem, ctx, _db, _src, id) = setup(Duration::from_secs(5)).await;
    let (c, i) = (ctx.clone(), id.clone());
    on_first_landing(&ctx, move || {
        let (c, i) = (c.clone(), i.clone());
        tokio::spawn(async move { fu::cancel(&c, &i).await.unwrap() });
    });
    fu::approve(&ctx, &id, Some(OverwritePolicy::Skip), true).await.unwrap();
    let job = wait_settled(&ctx, &id, 20).await;
    assert_eq!(job.state, JobState::Canceled);
    let keys = mem.keys();
    assert_eq!(keys.len(), 1, "the landed PUT stays: {keys:?}");
    assert_eq!(item_at(&ctx, &id, &keys[0]).await.state, ItemState::Done);
    assert_eq!(job.counts.canceled, 3);
    bucket_matches_done(&mem, &ctx, &id).await;
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn put_landing_during_pause_is_done_and_not_resent() {
    let (mem, ctx, _db, _src, id) = setup(Duration::from_secs(5)).await;
    let (c, i) = (ctx.clone(), id.clone());
    on_first_landing(&ctx, move || {
        let (c, i) = (c.clone(), i.clone());
        tokio::spawn(async move { fu::pause(&c, &i).await.unwrap() });
    });
    // Overwrite: a re-send would not be refused, so it would show as a second write.
    fu::approve(&ctx, &id, Some(OverwritePolicy::Overwrite), true).await.unwrap();
    let job = wait_settled(&ctx, &id, 20).await;
    assert_eq!(job.state, JobState::Paused);
    let keys = mem.keys();
    assert_eq!(keys.len(), 1, "{keys:?}");
    let first = keys[0].clone();
    assert_eq!(item_at(&ctx, &id, &first).await.state, ItemState::Done, "landed during pause: done");
    bucket_matches_done(&mem, &ctx, &id).await;
    let etag = mem.etag_of(&first);

    *mem.post_commit_delay.lock().unwrap() = Duration::ZERO;
    fu::resume(&ctx, &id).await.unwrap();
    let job = wait_settled(&ctx, &id, 20).await;
    assert_eq!(job.state, JobState::Completed);
    assert_eq!((job.counts.done, job.counts.skipped_exists), (4, 0));
    assert_eq!(*mem.puts.lock().unwrap(), 4, "each file written exactly once");
    assert_eq!(mem.etag_of(&first), etag, "{first} not sent again");
}

/// App quit mid-request (runner torn down, rows reaped), then resume: verified, not re-sent.
#[test]
fn put_landing_before_quit_is_not_resent_after_restart() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let landed = Arc::new(AtomicBool::new(false));
    let l = landed.clone();
    let (mem, ctx, db, src, id) = rt.block_on(async move {
        let (mem, ctx, db, src, id) = setup(Duration::from_secs(60)).await;
        on_first_landing(&ctx, move || l.store(true, Ordering::SeqCst));
        fu::approve(&ctx, &id, Some(OverwritePolicy::Overwrite), true).await.unwrap();
        (mem, ctx, db, src, id)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !landed.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Quit: nothing in flight gets to finish.
    rt.shutdown_background();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        assert_eq!(*mem.puts.lock().unwrap(), 1);
        ctx.db.reap_orphan_transfers().await.unwrap();
        *mem.post_commit_delay.lock().unwrap() = Duration::ZERO;
        let ctx2 = ctx.restarted(mem.clone(), 1);
        fu::resume_all(&ctx2).await;
        let job = wait_settled(&ctx2, &id, 20).await;
        assert_eq!(job.state, JobState::Completed, "{job:?}");
        assert_eq!(job.counts.done, 4);
        assert_eq!(*mem.puts.lock().unwrap(), 4, "the landed PUT was verified, not sent again");
        drop((db, src));
    });
}
