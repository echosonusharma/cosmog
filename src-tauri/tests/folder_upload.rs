//! Folder upload engine against MinIO (Part J 1-9) plus the shared ListParts resume path.
//! Set FU_REQUIRE_MINIO=1 to fail instead of skip when MinIO is down.

mod common;
#[path = "common/fu_ctx.rs"]
mod fu_ctx;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cosmog_lib::db::folder_upload::{ExcludeReason, ItemFilter, ItemState, JobState, OverwritePolicy};
use cosmog_lib::db::transfers::TransferStatus;
use cosmog_lib::folder_upload as fu;
use cosmog_lib::store::{ListOptions, ObjectStore, PutOptions};
use cosmog_lib::transfer::{ResumeState, SourceStat, TransferCtx, TransferEvent};
use fu_ctx::{analyze_checked, make_ctx, wait_settled, wait_until, write, Ctx};

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

async fn keys(store: &Arc<dyn ObjectStore>, bucket: &str, prefix: &str) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    let mut cont = None;
    loop {
        let opts = ListOptions { prefix: Some(prefix.into()), delimiter: None, continuation: cont.take(), max_keys: Some(1000) };
        let page = store.list_objects(bucket, opts).await.unwrap();
        out.extend(page.objects.into_iter().map(|o| (o.key, o.size)));
        match page.continuation {
            Some(c) if page.is_truncated => cont = Some(c),
            _ => return out,
        }
    }
}

/// Lists the whole (per-test) bucket: MinIO only honors an exact-key prefix here.
async fn no_multipart_left(store: &Arc<dyn ObjectStore>, bucket: &str, prefix: &str) {
    let (ups, _) = store.list_multipart_uploads(bucket, None, None).await.unwrap();
    let ups: Vec<_> = ups.into_iter().filter(|u| u.key.starts_with(prefix)).collect();
    assert!(ups.is_empty(), "orphan multipart uploads: {ups:?}");
}

async fn tune(ctx: &Ctx, part_mib: u64, parallelism: u32) {
    let mut s = ctx.db.settings_load().await.unwrap();
    s.part_size_bytes = part_mib * MIB as u64;
    s.multipart_parallelism = parallelism;
    ctx.db.settings_save(s).await.unwrap();
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// Every local regular file, '/'-joined, with its bytes.
fn local_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push((e.path(), r));
            } else if ft.is_file() {
                // Unreadable files are skipped; a FIFO is never opened.
                if let Ok(body) = std::fs::read(e.path()) {
                    out.insert(r, body);
                }
            }
        }
    }
    out
}

#[tokio::test]
#[serial_test::serial]
async fn tree_is_mirrored_exactly() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-tree").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write(root, "a/b/c.txt", b"nested");
    write(root, ".git/objects/ab/cd", b"git object");
    write(root, ".env", b"SECRET=1");
    write(root, "zero.bin", b"");
    write(root, "résumé ü.txt", "unicode ü".as_bytes());
    write(root, "trail ", b"trailing space name");
    write(root, "big/multipart.bin", &pattern(20 * MIB, 7));
    std::fs::create_dir_all(root.join("emptydir")).unwrap();

    let a = fu::analyze(&ctx, &acct, &bucket, root.to_str().unwrap(), "dst", |_| {}).await.unwrap();
    assert_eq!(a.dest_prefix, "dst/");
    assert_eq!(a.files, 7);
    assert_eq!(a.empty_dirs, 1);
    assert_eq!(a.hidden.files, 2, "hidden: .env and .git/objects/ab/cd");
    assert_eq!(a.zero_byte_files, 1);
    let check = fu::check_remote(&ctx, &a.job_id, "dst/", |_| {}).await.unwrap();
    assert_eq!(check.existing, 0);
    let job = fu::approve(&ctx, &a.job_id, None, true).await.unwrap();
    assert_eq!(job.total_files, 7);
    let job = wait_settled(&ctx, &a.job_id, 60).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");
    assert_eq!(job.counts.done, 7);

    let local = local_files(root);
    let mut expected: BTreeSet<String> = local.keys().map(|r| format!("dst/{r}")).collect();
    expected.insert("dst/emptydir/".into());
    let remote = keys(&store, &bucket, "dst/").await;
    assert_eq!(remote.keys().cloned().collect::<BTreeSet<_>>(), expected);
    for (rel, body) in &local {
        let got = store.read_object_full(&bucket, &format!("dst/{rel}")).await.unwrap();
        assert!(got == *body, "bytes differ for {rel}");
    }
    assert!(ctx.db.list_job_transfers(&a.job_id).await.unwrap().is_empty(), "no job rows left");
    no_multipart_left(&store, &bucket, "dst/").await;
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn analysis_counts_match_a_reference_walk() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-ana").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    for i in 0..25 {
        let ext = ["jpg", "txt", "rs"][i % 3];
        write(root, &format!("d{}/f{i}.{ext}", i % 4), &vec![b'x'; i * 10]);
    }
    write(root, "noext", b"12345");
    #[cfg(unix)]
    {
        assert!(std::process::Command::new("mkfifo").arg(root.join("fifo")).status().unwrap().success());
        use std::os::unix::fs::PermissionsExt;
        write(root, "locked.txt", b"no");
        std::fs::set_permissions(root.join("locked.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let a = fu::analyze(&ctx, &acct, &bucket, root.to_str().unwrap(), "", |_| {}).await.unwrap();
    // Reference walk (FIFO and the unreadable file are not uploadable).
    let mut files = 0;
    let mut bytes = 0;
    let mut by_ext: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for (rel, body) in local_files(root) {
        if rel == "locked.txt" {
            continue;
        }
        files += 1;
        bytes += body.len() as i64;
        let ext = rel.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        let e = by_ext.entry(ext).or_default();
        e.0 += 1;
        e.1 += body.len() as i64;
    }
    #[cfg(unix)]
    let root_user = std::fs::File::open(root.join("locked.txt")).is_ok();
    #[cfg(not(unix))]
    let root_user = false;
    assert_eq!(a.files, files + root_user as i64);
    assert_eq!(a.total_bytes, bytes + if root_user { 2 } else { 0 });
    if !root_user {
        let got: BTreeMap<String, (i64, i64)> = a.by_ext.iter().map(|e| (e.ext.clone(), (e.files, e.bytes))).collect();
        assert_eq!(got, by_ext);
    }
    #[cfg(unix)]
    {
        assert_eq!(a.excluded["special"], 1, "FIFO excluded, walk did not hang");
        if !root_user {
            assert_eq!(a.excluded["unreadable"], 1);
        }
    }
    // A long destination: every key over 1024 bytes becomes key_too_long.
    let dest = format!("{}/", "p".repeat(1015));
    let check = fu::check_remote(&ctx, &a.job_id, &dest, |_| {}).await.unwrap();
    let page = fu::list_items(&ctx, &a.job_id, ItemFilter::All, 0, 500).await.unwrap();
    let expect = page
        .items
        .iter()
        .filter(|i| i.exclude_reason.is_none() || i.exclude_reason == Some(ExcludeReason::KeyTooLong))
        .filter(|i| dest.len() + i.rel_path.len() + (i.kind == cosmog_lib::db::folder_upload::ItemKind::Dir) as usize > 1024)
        .count() as i64;
    assert!(expect > 0);
    assert_eq!(check.key_too_long, expect);
    let back = fu::check_remote(&ctx, &a.job_id, "short/", |_| {}).await.unwrap();
    assert_eq!(back.key_too_long, 0, "a shorter destination re-includes them");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("locked.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    fu::cancel(&ctx, &a.job_id).await.unwrap();
    assert!(ctx.db.fu_get_job(&a.job_id).await.is_err(), "cancel before approval deletes the job");
    common::cleanup_bucket(&store, &bucket).await;
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn symlinks_are_never_followed() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-link").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let outside = tempfile::tempdir().unwrap();
    let secret = b"TOP-SECRET-7f3a9c";
    std::fs::write(outside.path().join("id_key"), secret).unwrap();
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write(root, "a/real.txt", b"real");
    std::os::unix::fs::symlink(outside.path().join("id_key"), root.join("leak")).unwrap();
    std::os::unix::fs::symlink("..", root.join("a/loop")).unwrap();

    let job_id = analyze_checked(&ctx, &acct, &bucket, root, "s/").await;
    let ex = fu::list_items(&ctx, &job_id, ItemFilter::Excluded, 0, 100).await.unwrap();
    let links: BTreeMap<String, Option<String>> = ex
        .items
        .iter()
        .filter(|i| i.exclude_reason == Some(ExcludeReason::Symlink))
        .map(|i| (i.rel_path.clone(), i.link_target.clone()))
        .collect();
    assert_eq!(links.len(), 2);
    assert_eq!(links["a/loop"].as_deref(), Some(".."));
    assert!(links["leak"].as_deref().unwrap().ends_with("id_key"));
    fu::approve(&ctx, &job_id, None, true).await.unwrap();
    assert_eq!(wait_settled(&ctx, &job_id, 30).await.state, JobState::Completed);
    let remote = keys(&store, &bucket, "").await;
    assert_eq!(remote.keys().cloned().collect::<Vec<_>>(), vec!["s/a/real.txt".to_string()]);
    for k in remote.keys() {
        let body = store.read_object_full(&bucket, k).await.unwrap();
        assert!(!body.windows(secret.len()).any(|w| w == secret), "secret leaked into {k}");
    }
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn overwrite_choices_are_honored() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-ow").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    for i in 0..10 {
        write(root, &format!("f{i}.txt"), format!("local {i}").as_bytes());
    }
    for i in 0..3 {
        store.put_object_bytes(&bucket, &format!("o/f{i}.txt"), "text/plain", b"seeded".to_vec(), Default::default()).await.unwrap();
    }
    // A remote dir marker over a local dir is not an overwrite.
    store.put_object_bytes(&bucket, "o/f9.txt/", "application/x-directory", vec![], Default::default()).await.unwrap();

    let a = fu::analyze(&ctx, &acct, &bucket, root.to_str().unwrap(), "o/", |_| {}).await.unwrap();
    let check = fu::check_remote(&ctx, &a.job_id, "o/", |_| {}).await.unwrap();
    assert_eq!(check.existing, 3);
    assert_eq!(check.existing_bytes, 18);
    let err = fu::approve(&ctx, &a.job_id, None, true).await.unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    fu::approve(&ctx, &a.job_id, Some(OverwritePolicy::Skip), true).await.unwrap();
    let job = wait_settled(&ctx, &a.job_id, 30).await;
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(job.counts.skipped_exists, 3);
    assert_eq!(job.counts.done, 7);
    for i in 0..3 {
        assert_eq!(store.read_object_full(&bucket, &format!("o/f{i}.txt")).await.unwrap(), b"seeded");
    }

    // Same tree again with overwrite.
    let id = analyze_checked(&ctx, &acct, &bucket, root, "o/").await;
    fu::approve(&ctx, &id, Some(OverwritePolicy::Overwrite), true).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 30).await.counts.done, 10);
    assert_eq!(store.read_object_full(&bucket, "o/f0.txt").await.unwrap(), b"local 0");

    // Late appearance: a key written after the check is skipped via If-None-Match (412).
    let id = analyze_checked(&ctx, &acct, &bucket, root, "late/").await;
    store.put_object_bytes(&bucket, "late/f5.txt", "text/plain", b"late".to_vec(), Default::default()).await.unwrap();
    fu::approve(&ctx, &id, Some(OverwritePolicy::Skip), true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.counts.skipped_exists, 1);
    assert_eq!(job.appeared_after_check, 1);
    assert_eq!(store.read_object_full(&bucket, "late/f5.txt").await.unwrap(), b"late");
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn drift_outcomes() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-drift").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write(root, "grow.txt", b"v1");
    write(root, "gone.txt", b"bye");
    write(root, "same.txt", b"same");
    let id = analyze_checked(&ctx, &acct, &bucket, root, "d/").await;
    write(root, "grow.txt", b"version two, longer");
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.state, JobState::CompletedWithErrors);
    assert_eq!(job.counts.skipped_missing, 1);
    assert_eq!(job.drift, 1);
    assert_eq!(store.read_object_full(&bucket, "d/grow.txt").await.unwrap(), b"version two, longer");
    let drift = fu::list_items(&ctx, &id, ItemFilter::Drift, 0, 10).await.unwrap();
    assert_eq!(drift.items[0].rel_path, "grow.txt");
    assert_eq!(drift.items[0].state, ItemState::Done);

    // A file that keeps changing while it uploads: one re-upload, then source_changed.
    let src2 = tempfile::tempdir().unwrap();
    write(src2.path(), "hot.log", b"start");
    let id = analyze_checked(&ctx, &acct, &bucket, src2.path(), "h/").await;
    let hot = src2.path().join("hot.log");
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::Started { .. }) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&hot).unwrap();
            f.write_all(b"+more").unwrap();
        }
    });
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.counts.failed, 1);
    let failed = fu::list_items(&ctx, &id, ItemFilter::Failed, 0, 10).await.unwrap();
    assert!(failed.items[0].error.as_deref().unwrap().contains("source_changed"));
    assert_eq!(failed.items[0].drift, Some(cosmog_lib::db::folder_upload::Drift::ChangedDuringUpload));

    // Skip policy: the re-upload after one change replaces our own object, not "appeared after check".
    let src3 = tempfile::tempdir().unwrap();
    write(src3.path(), "once.log", b"start");
    let id = analyze_checked(&ctx, &acct, &bucket, src3.path(), "s/").await;
    let once = src3.path().join("once.log");
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::Started { .. }) && !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&once).unwrap();
            f.write_all(b"+more").unwrap();
        }
    });
    fu::approve(&ctx, &id, Some(OverwritePolicy::Skip), true).await.unwrap();
    let job = wait_settled(&ctx, &id, 30).await;
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(job.counts.done, 1);
    assert_eq!(job.appeared_after_check, 0);
    assert_eq!(store.read_object_full(&bucket, "s/once.log").await.unwrap(), b"start+more");
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn encrypted_round_trip() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-enc").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    let (secret, recipient) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, &bucket, &recipient).await.unwrap();
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write(root, "small.txt", b"plain small");
    write(root, "sub/zero", b"");
    write(root, "sub/big.bin", &pattern(12 * MIB, 3));
    let id = analyze_checked(&ctx, &acct, &bucket, root, "e/").await;
    assert!(fu::get_analysis(&ctx, &id).await.unwrap().warnings.contains(&"encrypted_temp_space".to_string()));
    fu::approve(&ctx, &id, None, true).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 60).await.state, JobState::Completed);
    let identity = cosmog_lib::crypto::parse_identity(&secret).unwrap();
    for (rel, body) in local_files(root) {
        let key = format!("e/{rel}");
        let cipher = store.read_object_full(&bucket, &key).await.unwrap();
        assert_ne!(cipher, body);
        assert_eq!(cosmog_lib::crypto::decrypt_bytes(&identity, &cipher).unwrap(), body, "{rel}");
        let meta = store.head_object(&bucket, &key).await.unwrap();
        assert_eq!(meta.user_metadata.get("cosmog-encrypted").map(String::as_str), Some("1"));
    }
    no_multipart_left(&store, &bucket, "e/").await;
    common::cleanup_bucket(&store, &bucket).await;
}

/// Pauses after `parts` parts of the multipart file; returns its upload_id and part numbers sent.
async fn pause_mid_multipart(ctx: &Ctx, id: &str, parts: usize) -> (String, BTreeSet<i32>) {
    let seen = Arc::new(Mutex::new((String::new(), BTreeSet::new())));
    let s = seen.clone();
    ctx.set_hook(move |ev| {
        let mut g = s.lock().unwrap();
        match ev {
            TransferEvent::MultipartInitiated { upload_id, .. } => g.0 = upload_id.clone(),
            TransferEvent::PartCompleted { part_number, .. } => {
                g.1.insert(*part_number);
            }
            _ => {}
        }
    });
    fu::approve(ctx, id, None, true).await.unwrap();
    let s = seen.clone();
    wait_until(60, || {
        let n = s.lock().unwrap().1.len();
        async move { n >= parts }
    })
    .await;
    fu::pause(ctx, id).await.unwrap();
    let g = seen.lock().unwrap().clone();
    g
}

#[tokio::test]
#[serial_test::serial]
async fn pause_and_resume_keep_parts() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-pause").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    tune(&ctx, 5, 1).await;
    let src = tempfile::tempdir().unwrap();
    let body = pattern(40 * MIB, 9);
    write(src.path(), "big.bin", &body);
    let id = analyze_checked(&ctx, &acct, &bucket, src.path(), "p/").await;
    let (upload_id, before) = pause_mid_multipart(&ctx, &id, 2).await;

    let job = fu::get_job(&ctx, &id).await.unwrap();
    assert_eq!(job.state, JobState::Paused);
    assert_eq!(job.pause_reason.as_deref(), Some("user"));
    let rows = ctx.db.list_job_transfers(&id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, TransferStatus::Paused);
    assert_eq!(rows[0].upload_id.as_deref(), Some(upload_id.as_str()), "pause keeps the multipart upload");

    let after = Arc::new(Mutex::new((String::new(), BTreeSet::new())));
    let a = after.clone();
    ctx.set_hook(move |ev| match ev {
        TransferEvent::MultipartInitiated { upload_id, .. } => a.lock().unwrap().0 = upload_id.clone(),
        TransferEvent::PartCompleted { part_number, .. } => {
            a.lock().unwrap().1.insert(*part_number);
        }
        _ => {}
    });
    fu::resume(&ctx, &id).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 60).await.state, JobState::Completed);
    let (uid2, resent) = after.lock().unwrap().clone();
    assert_eq!(uid2, upload_id, "same multipart upload resumed");
    assert!(resent.is_disjoint(&before), "parts {before:?} re-sent: {resent:?}");
    assert_eq!(store.read_object_full(&bucket, "p/big.bin").await.unwrap(), body);
    no_multipart_left(&store, &bucket, "p/").await;
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn pause_and_resume_encrypted_restarts_cleanly() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-pause-enc").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 3).await;
    tune(&ctx, 5, 1).await;
    let (secret, recipient) = cosmog_lib::crypto::new_identity();
    ctx.db.set_encryption_config(&acct, &bucket, &recipient).await.unwrap();
    let src = tempfile::tempdir().unwrap();
    let body = pattern(30 * MIB, 4);
    write(src.path(), "big.bin", &body);
    let id = analyze_checked(&ctx, &acct, &bucket, src.path(), "pe/").await;
    let (old_upload, _) = pause_mid_multipart(&ctx, &id, 2).await;
    fu::resume(&ctx, &id).await.unwrap();
    assert_eq!(wait_settled(&ctx, &id, 60).await.state, JobState::Completed);
    no_multipart_left(&store, &bucket, "pe/").await;
    let ids: Vec<String> = ctx
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            TransferEvent::MultipartInitiated { upload_id, .. } => Some(upload_id.clone()),
            _ => None,
        })
        .collect();
    assert_ne!(ids.last().unwrap(), &old_upload, "ciphertext restarts on a new upload");
    let identity = cosmog_lib::crypto::parse_identity(&secret).unwrap();
    let cipher = store.read_object_full(&bucket, "pe/big.bin").await.unwrap();
    assert_eq!(cosmog_lib::crypto::decrypt_bytes(&identity, &cipher).unwrap(), body);
    common::cleanup_bucket(&store, &bucket).await;
}

#[tokio::test]
#[serial_test::serial]
async fn cancel_leaves_uploaded_objects_and_nothing_dangling() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-cancel").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 2).await;
    tune(&ctx, 5, 1).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..30 {
        write(src.path(), &format!("s/f{i:02}.txt"), format!("{i}").as_bytes());
    }
    write(src.path(), "big.bin", &pattern(40 * MIB, 1));
    let id = analyze_checked(&ctx, &acct, &bucket, src.path(), "c/").await;
    let parts = Arc::new(AtomicUsize::new(0));
    let p = parts.clone();
    ctx.set_hook(move |ev| {
        if matches!(ev, TransferEvent::PartCompleted { .. }) {
            p.fetch_add(1, Ordering::SeqCst);
        }
    });
    fu::approve(&ctx, &id, None, true).await.unwrap();
    let p = parts.clone();
    wait_until(60, || {
        let n = p.load(Ordering::SeqCst);
        async move { n >= 1 }
    })
    .await;
    fu::cancel(&ctx, &id).await.unwrap();
    let job = fu::get_job(&ctx, &id).await.unwrap();
    assert_eq!(job.state, JobState::Canceled);
    assert_eq!(job.counts.pending, 0);
    assert_eq!(job.counts.uploading, 0);
    assert!(job.counts.canceled >= 1);
    let remote = keys(&store, &bucket, "c/").await;
    assert_eq!(remote.len() as i64, job.counts.done, "uploaded objects stay");
    no_multipart_left(&store, &bucket, "c/").await;
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
    // Removing a finished job leaves no rows behind.
    fu::remove(&ctx, &id).await.unwrap();
    assert!(ctx.db.fu_get_job(&id).await.is_err());
    common::cleanup_bucket(&store, &bucket).await;
}

/// Crash = drop the runtime the engine runs on: no terminal statuses, no aborts, no DB updates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn resume_after_crash_uploads_only_the_remainder() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-crash").await;
    let (ctx, _db, acct) = make_ctx(store.clone(), 2).await;
    tune(&ctx, 5, 1).await;
    let src = tempfile::tempdir().unwrap();
    for i in 0..60 {
        write(src.path(), &format!("small/f{i:02}.txt"), format!("small {i}").as_bytes());
    }
    let big = pattern(40 * MIB, 5);
    write(src.path(), "big.bin", &big);
    let id = analyze_checked(&ctx, &acct, &bucket, src.path(), "r/").await;

    let run1 = Arc::new(Mutex::new((String::new(), BTreeSet::new())));
    let (ctx1, id1, r1) = (ctx.restarted(common::make_store().await, 2), id.clone(), run1.clone());
    let crashed = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            let r = r1.clone();
            ctx1.set_hook(move |ev| match ev {
                TransferEvent::MultipartInitiated { upload_id, .. } => r.lock().unwrap().0 = upload_id.clone(),
                TransferEvent::PartCompleted { part_number, .. } => {
                    r.lock().unwrap().1.insert(*part_number);
                }
                _ => {}
            });
            fu::approve(&ctx1, &id1, None, true).await.unwrap();
            let r = r1.clone();
            wait_until(60, || {
                let n = r.lock().unwrap().1.len();
                async move { n >= 2 }
            })
            .await;
        });
        rt.shutdown_background();
    });
    crashed.join().unwrap();
    let (upload_id, parts_before) = run1.lock().unwrap().clone();
    assert!(!upload_id.is_empty());

    // Restart: reap, then a fresh engine on the same DB.
    ctx.db.reap_orphan_transfers().await.unwrap();
    let done_before: Vec<String> = fu::list_items(&ctx, &id, ItemFilter::All, 0, 500)
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|i| i.state == ItemState::Done)
        .map(|i| i.key)
        .collect();
    let mut heads = BTreeMap::new();
    for k in &done_before {
        let m = store.head_object(&bucket, k).await.unwrap();
        heads.insert(k.clone(), (m.etag, m.last_modified));
    }
    eprintln!("crash after {} done items, parts {parts_before:?}", done_before.len());
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let ctx2 = ctx.restarted(common::make_store().await, 2);
    let run2 = Arc::new(Mutex::new((String::new(), BTreeSet::new())));
    let r = run2.clone();
    ctx2.set_hook(move |ev| match ev {
        TransferEvent::MultipartInitiated { upload_id, .. } => r.lock().unwrap().0 = upload_id.clone(),
        TransferEvent::PartCompleted { part_number, .. } => {
            r.lock().unwrap().1.insert(*part_number);
        }
        _ => {}
    });
    fu::resume_all(&ctx2).await;
    let job = wait_settled(&ctx2, &id, 90).await;
    assert_eq!(job.state, JobState::Completed, "{job:?}");

    for (k, before) in &heads {
        let m = store.head_object(&bucket, k).await.unwrap();
        assert_eq!(&(m.etag, m.last_modified), before, "{k} was re-uploaded");
    }
    let (uid2, parts_after) = run2.lock().unwrap().clone();
    assert_eq!(uid2, upload_id, "multipart resumed under the same upload_id");
    assert!(!parts_after.is_empty(), "the remaining parts were sent after the restart");
    assert!(parts_after.is_disjoint(&parts_before), "re-sent {parts_before:?} ∩ {parts_after:?}");
    let remote = keys(&store, &bucket, "r/").await;
    assert_eq!(remote.len(), 61);
    assert_eq!(store.read_object_full(&bucket, "r/big.bin").await.unwrap(), big);
    no_multipart_left(&store, &bucket, "r/").await;
    assert!(ctx.db.list_job_transfers(&id).await.unwrap().is_empty());
    common::cleanup_bucket(&store, &bucket).await;
}

/// Shared engine change: a resume rebuilds parts from ListParts even when the journal is empty.
#[tokio::test]
#[serial_test::serial]
async fn multipart_resume_uses_list_parts_not_the_journal() {
    need_minio!();
    let store = common::make_store().await;
    let bucket = common::create_test_bucket(&store, "cosmog-fu-lp").await;
    let src = tempfile::NamedTempFile::new().unwrap();
    let body = pattern(26 * MIB, 2);
    std::fs::write(src.path(), &body).unwrap();
    let meta = std::fs::metadata(src.path()).unwrap();
    let stat = SourceStat {
        len: meta.len(),
        mtime_secs: meta.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
    };

    let first = Arc::new(Mutex::new((String::new(), BTreeSet::new())));
    let cancel = tokio_util::sync::CancellationToken::new();
    let (f, c) = (first.clone(), cancel.clone());
    let sink = cosmog_lib::transfer::ProgressSink::from_fn(move |ev| match ev {
        TransferEvent::MultipartInitiated { upload_id, .. } => f.lock().unwrap().0 = upload_id,
        TransferEvent::PartCompleted { part_number, .. } => {
            let mut g = f.lock().unwrap();
            g.1.insert(part_number);
            if g.1.len() >= 3 {
                c.cancel();
            }
        }
        _ => {}
    });
    let mut ctx = TransferCtx::new("lp1").with_progress(sink).with_cancel(cancel);
    ctx.part_size = 5 * MIB as u64;
    ctx.parallelism = 1;
    assert!(store.put_object(&bucket, "lp.bin", src.path().into(), PutOptions::default(), ctx).await.is_err());
    let (upload_id, sent_first) = first.lock().unwrap().clone();
    assert_eq!(sent_first.len(), 3);

    let second = Arc::new(Mutex::new(Vec::new()));
    let s = second.clone();
    let sink = cosmog_lib::transfer::ProgressSink::from_fn(move |ev| {
        if let TransferEvent::PartCompleted { part_number, upload_id, .. } = ev {
            s.lock().unwrap().push((part_number, upload_id));
        }
    });
    let mut ctx = TransferCtx::new("lp2").with_progress(sink);
    ctx.part_size = 5 * MIB as u64;
    ctx.parallelism = 1;
    ctx.source_stat = Some(stat);
    // An empty journal: only ListParts can know parts 1-3 are there.
    ctx.resume = Some(ResumeState {
        upload_id: upload_id.clone(),
        completed_parts: vec![],
        source_len: Some(stat.len),
        source_mtime_secs: Some(stat.mtime_secs),
        part_size: Some(5 * MIB as u64),
    });
    let res = store.put_object(&bucket, "lp.bin", src.path().into(), PutOptions::default(), ctx).await.unwrap();
    assert_eq!(res.upload_id.as_deref(), Some(upload_id.as_str()));
    let resent: HashSet<i32> = second.lock().unwrap().iter().map(|(n, _)| *n).collect();
    assert_eq!(resent, (4..=6).collect::<HashSet<i32>>(), "only the missing parts are sent");
    assert_eq!(store.read_object_full(&bucket, "lp.bin").await.unwrap(), body);
    no_multipart_left(&store, &bucket, "").await;
    common::cleanup_bucket(&store, &bucket).await;
}
