//! Folder upload on the in-memory store: nothing outside the folder is read, runner writes
//! never beat a pause or cancel, multipart uploads are never orphaned, startup and markers.

mod common;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;
#[path = "common/mem_store.rs"]
mod mem_store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmog_lib::db::folder_upload::{ExcludeReason, ItemState, JobState, OverwritePolicy};
use cosmog_lib::folder_upload as fu;
use cosmog_lib::transfer::TransferEvent;
use fu_ctx::{analyze_checked, body, item_at, make_ctx, small_parts, wait_settled, wait_until, write};
use mem_store::MemStore;

const MIB: usize = 1024 * 1024;
const SKIP: Option<OverwritePolicy> = Some(OverwritePolicy::Skip);
const SECRET: &[u8] = b"TOP-SECRET-91c2";

fn leaked(mem: &MemStore) -> Vec<String> {
    let objs = mem.objects.lock().unwrap();
    objs.iter().filter(|(_, v)| v.windows(SECRET.len()).any(|w| w == SECRET)).map(|(k, _)| k.clone()).collect()
}

/// M2: a file swapped for a link to a secret while waiting for a slot is never read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_swapped_for_a_link_after_approval_is_not_read() {
    let mem = MemStore::new();
    *mem.delay.lock().unwrap() = Duration::from_millis(300);
    // One transfer slot: b waits (already checked) while a uploads.
    let (ctx, _db, acct) = make_ctx(mem.clone(), 1).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("id_key"), SECRET).unwrap();
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a.txt", b"first");
    write(src.path(), "b.txt", b"second");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    let (b, secret) = (src.path().join("b.txt"), outside.path().join("id_key"));
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::Started { .. }) && !d.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(&b).unwrap();
            std::os::unix::fs::symlink(&secret, &b).unwrap();
        }
    });
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 20).await;
    assert!(leaked(&mem).is_empty(), "secret uploaded as {:?}", leaked(&mem));
    assert!(!mem.keys().contains(&"b.txt".to_string()));
    let it = item_at(&ctx, &id, "b.txt").await;
    assert_eq!((it.state, it.exclude_reason), (ItemState::Excluded, Some(ExcludeReason::Symlink)));
    assert!(it.drift.is_some(), "a change after approval is drift");
    assert_eq!(job.state, JobState::CompletedWithErrors, "excluded after approval is an error");
}

/// M2: a parent folder swapped for a link to elsewhere resolves outside the root: refused.
#[tokio::test]
async fn parent_dir_swapped_for_a_link_is_refused() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("f.txt"), SECRET).unwrap();
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "sub/f.txt", b"inside");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    std::fs::remove_dir_all(src.path().join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), src.path().join("sub")).unwrap();
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    assert!(leaked(&mem).is_empty(), "secret uploaded as {:?}", leaked(&mem));
    assert_eq!(item_at(&ctx, &id, "sub/f.txt").await.state, ItemState::Excluded);
    assert_eq!(job.state, JobState::CompletedWithErrors);
}

/// M2: no longer a regular file inside the folder right after the upload: failure, never done.
#[tokio::test]
async fn swap_right_after_upload_fails_the_item() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x"), SECRET).unwrap();
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a.txt", b"real content");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    let (a, x) = (src.path().join("a.txt"), outside.path().join("x"));
    ctx.set_hook(move |ev| {
        // Last progress comes after the write is committed.
        if let TransferEvent::Progress { bytes_done, bytes_total: Some(t), .. } = ev {
            if bytes_done == t {
                let _ = std::fs::remove_file(&a);
                let _ = std::os::unix::fs::symlink(&x, &a);
            }
        }
    });
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    let it = item_at(&ctx, &id, "a.txt").await;
    assert_eq!(it.state, ItemState::Failed);
    assert!(it.error.as_deref().unwrap().contains("source_changed"));
    assert_eq!(job.state, JobState::CompletedWithErrors);
    assert_eq!(mem.objects.lock().unwrap()["a.txt"], b"real content", "what was read was the real file");
}

/// M5: the runner's final write never overwrites a state set while it ran.
#[tokio::test]
async fn runner_final_write_never_overwrites_a_cancel() {
    let mem = MemStore::new();
    *mem.delay.lock().unwrap() = Duration::from_millis(100);
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..4 {
        write(src.path(), &format!("f{i}"), b"x");
    }
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    // A cancel that won the race against the runner (as the old unlocked path allowed).
    ctx.db.fu_set_state(&id, JobState::Canceled, None, None).await.unwrap();
    let c = ctx.clone();
    wait_until(10, move || {
        let c = c.clone();
        async move { !c.jobs.any_runner_live() }
    })
    .await;
    assert_eq!(ctx.db.fu_get_job(&id).await.unwrap().state, JobState::Canceled);
}

/// M5: pausing a queued job while the scheduler is about to start it leaves it paused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_racing_the_scheduler_stays_paused() {
    for _ in 0..10 {
        let mem = MemStore::new();
        *mem.delay.lock().unwrap() = Duration::from_millis(30);
        let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
        let (s1, s2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(s1.path(), "a", b"a");
        for i in 0..3 {
            write(s2.path(), &format!("b{i}"), b"b");
        }
        let j1 = analyze_checked(&ctx, &acct, "b", s1.path(), "1/").await;
        let j2 = analyze_checked(&ctx, &acct, "b", s2.path(), "2/").await;
        fu::approve(&ctx, &j1, SKIP, true).await.unwrap();
        fu::approve(&ctx, &j2, SKIP, true).await.unwrap();
        // j1 ends about now and kicks j2; the pause races that kick.
        tokio::time::sleep(Duration::from_millis(25)).await;
        fu::pause(&ctx, &j2).await.unwrap();
        wait_settled(&ctx, &j1, 10).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let j = fu::get_job(&ctx, &j2).await.unwrap();
        assert_eq!(j.state, JobState::Paused, "{j:?}");
        assert!(!ctx.jobs.any_runner_live());
    }
}

/// M6: an abort that fails (offline) keeps the row and upload_id; startup retries it.
#[tokio::test]
async fn failed_abort_keeps_the_row_until_startup_succeeds() {
    let mem = MemStore::new();
    *mem.delay.lock().unwrap() = Duration::from_millis(100);
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    small_parts(&ctx).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "big.bin", &body(16 * MIB, 5));
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    let m = mem.clone();
    wait_until(20, move || {
        let n = m.parts_sent.lock().unwrap().len();
        async move { n >= 1 }
    })
    .await;
    *mem.abort_fail.lock().unwrap() = Some("network_unreachable");
    fu::cancel(&ctx, &id).await.unwrap();
    assert_eq!(fu::get_job(&ctx, &id).await.unwrap().state, JobState::Canceled);
    let open = mem.open_uploads();
    assert_eq!(open.len(), 1);
    let rows = ctx.db.list_job_transfers(&id).await.unwrap();
    assert_eq!(rows.len(), 1, "row kept while its upload could not be aborted");
    assert_eq!(rows[0].upload_id.as_deref(), Some(open[0].as_str()));

    *mem.abort_fail.lock().unwrap() = None;
    let ctx2 = ctx.restarted(mem.clone(), 2);
    fu::startup(&ctx2).await;
    assert!(mem.open_uploads().is_empty(), "startup aborted it");
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
}

/// M6: the upload_id is in the row before the first part is sent (a crash then cannot orphan it).
#[test]
fn upload_id_is_durable_before_the_first_part() {
    let mem = MemStore::new();
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (s, m) = (seen.clone(), mem.clone());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (ctx, _db, id, _src) = rt.block_on(async move {
        let (ctx, db, acct) = make_ctx(m.clone(), 2).await;
        small_parts(&ctx).await;
        *m.delay.lock().unwrap() = Duration::from_secs(30);
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "big.bin", &body(11 * MIB, 1));
        let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
        ctx.set_hook(move |ev| {
            if let TransferEvent::MultipartInitiated { upload_id, .. } = ev {
                *s.lock().unwrap() = Some(upload_id.clone());
            }
        });
        fu::approve(&ctx, &id, SKIP, true).await.unwrap();
        (ctx, db, id, src)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while seen.lock().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Crash: nothing spawned after this point runs.
    rt.shutdown_background();
    let uid = seen.lock().unwrap().clone().unwrap();
    let check = tokio::runtime::Runtime::new().unwrap();
    let rows = check.block_on(async { ctx.db.list_job_transfers(&id).await.unwrap() });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].upload_id.as_deref(), Some(uid.as_str()), "upload_id persisted before any part");
    assert!(mem.parts_sent.lock().unwrap().is_empty());
}

/// L3 and L4: a folder holding only excluded entries still gets a marker; an existing object
/// at a marker key is never replaced.
#[tokio::test]
async fn markers_cover_excluded_only_folders_and_never_replace() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("links")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", src.path().join("links/l")).unwrap();
    std::fs::create_dir_all(src.path().join("empty")).unwrap();
    write(src.path(), "f.txt", b"f");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    mem.put_object_bytes_sync("empty/", b"not a marker");
    fu::approve(&ctx, &id, SKIP, true).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 10).await.state, JobState::Completed);
    let objs = mem.objects.lock().unwrap().clone();
    assert_eq!(objs.keys().cloned().collect::<Vec<_>>(), ["empty/", "f.txt", "links/"]);
    assert_eq!(objs["empty/"], b"not a marker");
}

/// L7: startup keeps an analysis started in this run; L8: errors name only the folder.
#[tokio::test]
async fn startup_keeps_this_runs_analysis_and_errors_hide_paths() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a", b"a");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    fu::startup(&ctx).await;
    assert!(ctx.db.fu_get_job(&id).await.is_ok(), "same-run analysis kept");
    let missing = src.path().join("secret-parent").join("gone-folder");
    let e = fu::analyze(&ctx, &acct, "b", missing.to_str().unwrap(), "", |_| {}).await.unwrap_err();
    assert_eq!(e.code(), "folder_source_unavailable");
    assert_eq!(e.detail(), "gone-folder");
    assert!(!e.to_wire_string().contains("secret-parent"));
}

/// Cancel holds the scheduler lock while it waits for the walk to stop; the walk's error path
/// must signal idle before it takes that lock (it once deadlocked every later control call).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_analysis_never_deadlocks() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..50 {
        write(src.path(), &format!("d{i}/f"), b"x");
    }
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    let (ctx2, acct2, root) = (ctx.clone(), acct.clone(), src.path().to_str().unwrap().to_string());
    let analysis = tokio::spawn(async move {
        fu::analyze(&ctx2, &acct2, "b", &root, "", move |ev| {
            if let fu::FolderAnalyzeEvent::Started { job_id } = ev {
                let _ = tx.lock().unwrap().send(job_id);
                // Holds the walk back until cancel owns the scheduler lock.
                std::thread::sleep(Duration::from_millis(300));
            }
        })
        .await
    });
    let id = tokio::task::spawn_blocking(move || rx.recv().unwrap()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), fu::cancel(&ctx, &id)).await.expect("cancel deadlocked").unwrap();
    let res = tokio::time::timeout(Duration::from_secs(10), analysis).await.expect("analyze hung").unwrap();
    assert_eq!(res.unwrap_err().code(), "canceled");
    assert!(ctx.db.fu_get_job(&id).await.is_err(), "the discarded job is gone");
    tokio::time::timeout(Duration::from_secs(5), fu::kick(&ctx)).await.expect("scheduler lock leaked");
}
