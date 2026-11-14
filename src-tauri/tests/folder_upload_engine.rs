//! Folder upload engine on an in-memory store: window bound, waiting and pause reasons, startup.

mod common;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;
#[path = "common/mem_store.rs"]
mod mem_store;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosmog_lib::db::folder_upload::JobState;
use cosmog_lib::db::transfers::{Direction, NewTransfer, TransferOrigin};
use cosmog_lib::folder_upload as fu;
use fu_ctx::{analyze_checked, make_ctx, wait_settled, wait_until, write};
use mem_store::MemStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn window_bounds_job_transfers() {
    let mem = MemStore::new();
    *mem.delay.lock().unwrap() = Duration::from_millis(2);
    let (ctx, _db, acct) = make_ctx(mem.clone(), 3).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..5000 {
        write(src.path(), &format!("d{}/f{i}.txt", i % 50), b"x");
    }
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "w/").await;
    let max_rows = Arc::new(AtomicUsize::new(0));
    let leaked = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let poll = {
        let (db, id, max_rows, leaked, stop) = (ctx.db.clone(), id.clone(), max_rows.clone(), leaked.clone(), stop.clone());
        tokio::spawn(async move {
            while !stop.load(Ordering::SeqCst) {
                let n = db.list_job_transfers(&id).await.unwrap().len();
                max_rows.fetch_max(n, Ordering::SeqCst);
                if db.list_transfers(None).await.unwrap().iter().any(|t| t.job_id.is_some()) {
                    leaked.store(true, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        })
    };
    let t0 = std::time::Instant::now();
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 300).await;
    stop.store(true, Ordering::SeqCst);
    poll.await.unwrap();
    eprintln!("5000 files in {:?}, max job rows {}", t0.elapsed(), max_rows.load(Ordering::SeqCst));
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(job.counts.done, 5000);
    assert_eq!(mem.keys().len(), 5000);
    let w = 6; // min(2 * 3, 32)
    let seen = max_rows.load(Ordering::SeqCst);
    assert!(seen > 0 && seen <= w, "max concurrent job rows {seen} > W {w}");
    assert!(!leaked.load(Ordering::SeqCst), "list_transfers returned job rows");
}

#[tokio::test]
async fn network_loss_waits_and_recovers() {
    let mem = MemStore::new();
    *mem.delay.lock().unwrap() = Duration::from_millis(20);
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..20 {
        write(src.path(), &format!("f{i}"), b"data");
    }
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    *mem.fail.lock().unwrap() = Some("network_unreachable");
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let c = ctx.clone();
    let i = id.clone();
    wait_until(10, move || {
        let (c, i) = (c.clone(), i.clone());
        async move { fu::get_job(&c, &i).await.unwrap().waiting == Some("network") }
    })
    .await;
    // Stays waiting, nothing is failed or counted while offline.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let job = fu::get_job(&ctx, &id).await.unwrap();
    assert_eq!(job.state, JobState::Running);
    assert_eq!(job.counts.failed, 0);
    let items = fu::list_items(&ctx, &id, cosmog_lib::db::folder_upload::ItemFilter::All, 0, 100).await.unwrap();
    assert!(items.items.iter().all(|i| i.attempts == 0), "network failures don't count attempts");
    *mem.fail.lock().unwrap() = None;
    let job = wait_settled(&ctx, &id, 20).await;
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(job.waiting, None);
    assert_eq!(mem.keys().len(), 20);
}

#[tokio::test]
async fn auth_failure_pauses_with_code() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..5 {
        write(src.path(), &format!("f{i}"), b"data");
    }
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    *mem.fail.lock().unwrap() = Some("access_denied");
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    assert_eq!(job.state, JobState::Paused);
    assert_eq!(job.pause_reason.as_deref(), Some("access_denied"));
    assert_eq!(job.counts.failed, 0, "items wait, they are not failed");
    *mem.fail.lock().unwrap() = None;
    fu::resume(&ctx, &id).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 10).await.state, JobState::Completed);
    assert_eq!(mem.keys().len(), 5);
}

#[tokio::test]
async fn missing_source_root_pauses_instead_of_failing() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("drive");
    write(&root, "a.txt", b"a");
    let id = analyze_checked(&ctx, &acct, "b", &root, "").await;
    *mem.fail.lock().unwrap() = Some("access_denied");
    fu::approve(&ctx, &id, None, true).await.unwrap();
    wait_settled(&ctx, &id, 10).await;
    *mem.fail.lock().unwrap() = None;
    let unplugged = parent.path().join("unplugged");
    std::fs::rename(&root, &unplugged).unwrap();
    fu::resume(&ctx, &id).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    assert_eq!(job.state, JobState::Paused);
    assert_eq!(job.pause_reason.as_deref(), Some("folder_source_unavailable"));
    std::fs::rename(&unplugged, &root).unwrap();
    fu::resume(&ctx, &id).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 10).await.state, JobState::Completed);
}

#[tokio::test]
async fn encryption_toggle_pauses_until_confirmed() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a.txt", b"plain");
    let id = analyze_checked(&ctx, &acct, "b", src.path(), "").await;
    *mem.fail.lock().unwrap() = Some("access_denied");
    fu::approve(&ctx, &id, None, true).await.unwrap();
    wait_settled(&ctx, &id, 10).await;
    *mem.fail.lock().unwrap() = None;
    let (_secret, recipient) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, "b", &recipient).await.unwrap();
    fu::resume(&ctx, &id).await.unwrap();
    let job = wait_settled(&ctx, &id, 10).await;
    assert_eq!(job.pause_reason.as_deref(), Some("folder_encryption_changed"));
    assert!(mem.keys().is_empty());
    // Resuming again is the user's confirmation: continue under the new setting.
    fu::resume(&ctx, &id).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 10).await.state, JobState::Completed);
    let body = mem.objects.lock().unwrap()["a.txt"].clone();
    assert!(cosmog_lib::crypto::is_age_ciphertext(&body), "uploaded encrypted after confirm");
}

#[tokio::test]
async fn startup_drops_unapproved_jobs_and_orphan_rows() {
    let mem = MemStore::new();
    let (ctx, _db, acct) = make_ctx(mem.clone(), 2).await;
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "a.txt", b"a");
    let analyzed = analyze_checked(&ctx, &acct, "b", src.path(), "x/").await;
    ctx.db
        .insert_transfer(NewTransfer {
            id: "orphan".into(),
            account_id: acct.clone(),
            bucket: "b".into(),
            key: "k".into(),
            direction: Direction::Upload,
            local_path: "/nope".into(),
            options_json: None,
            origin: TransferOrigin::User,
            job_id: Some("ghost-job".into()),
        })
        .await
        .unwrap();
    let ctx2 = ctx.restarted(mem.clone(), 2);
    fu::startup(&ctx2).await;
    assert!(ctx.db.fu_get_job(&analyzed).await.is_err(), "analyzed job dropped at startup");
    assert!(ctx.db.get_transfer("orphan").await.is_err(), "orphan job row swept");
    // Duplicate guard: an unfinished job for the same source and destination blocks a second one.
    let id = analyze_checked(&ctx2, &acct, "b", src.path(), "y/").await;
    *mem.fail.lock().unwrap() = Some("access_denied");
    fu::approve(&ctx2, &id, None, true).await.unwrap();
    wait_settled(&ctx2, &id, 10).await;
    let err = fu::analyze(&ctx2, &acct, "b", src.path().to_str().unwrap(), "y/", |_| {}).await.unwrap_err();
    assert_eq!(err.code(), "folder_job_exists");
}
