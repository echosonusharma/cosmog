//! Folder upload persistence tests (pure SQLite): migration 21, job and item lifecycle,
//! key length recompute, and job-owned transfer rows staying out of the normal list.

mod common;

use cosmog_lib::db::accounts::NewAccount;
use cosmog_lib::db::folder_upload::{
    Drift, ExcludeReason, ItemFilter, ItemKind, ItemState, JobState, NewItem, NewJob, OverwritePolicy,
    RemoteState,
};
use cosmog_lib::db::transfers::{Direction, NewTransfer, TransferOrigin, TransferStatus};
use cosmog_lib::db::Db;
use rusqlite::params;

async fn seed_account(db: &Db) -> String {
    db.insert_account(NewAccount {
        name: "fu".into(),
        protocol: "s3".into(),
        endpoint: None,
        region: "us-east-1".into(),
        access_key_id: "AK".into(),
        addressing_style: None,
    })
    .await
    .unwrap()
    .id
}

async fn seed_job(db: &Db, acct: &str, id: &str, dest: &str) {
    db.fu_insert_job(NewJob {
        id: id.into(),
        account_id: acct.into(),
        bucket: "b".into(),
        source_kind: "path".into(),
        source_root: format!("/src/{id}"),
        dest_prefix: dest.into(),
    })
    .await
    .unwrap();
}

fn file(seq: i64, rel: &str, size: i64) -> NewItem {
    NewItem {
        seq,
        kind: ItemKind::File,
        rel_path: rel.into(),
        source_ref: None,
        size,
        mtime_ms: 1_700_000_000_000 + seq,
        ext: Some("txt".into()),
        hidden: rel.starts_with('.'),
        link_target: None,
        state: ItemState::Pending,
        exclude_reason: None,
    }
}

fn excluded(seq: i64, rel: &str, reason: ExcludeReason) -> NewItem {
    NewItem { state: ItemState::Excluded, exclude_reason: Some(reason), ..file(seq, rel, 0) }
}

fn transfer(id: &str, acct: &str, job: Option<&str>) -> NewTransfer {
    NewTransfer {
        id: id.into(),
        account_id: acct.into(),
        bucket: "b".into(),
        key: format!("k/{id}"),
        direction: Direction::Upload,
        local_path: format!("/tmp/{id}"),
        options_json: None,
        origin: TransferOrigin::User,
        job_id: job.map(str::to_string),
    }
}

async fn sql(db: &Db, q: &'static str) {
    db.conn.call(move |c| Ok::<_, tokio_rusqlite::Error>(c.execute_batch(q)?)).await.unwrap();
}

/// Runs a (successful, empty) remote check of `dest` on an analyzed job.
async fn checked(db: &Db, job: &str, dest: &str) {
    let g = db.fu_begin_remote(job, dest).await.unwrap().expect("check starts");
    assert!(db.fu_finish_remote(job, g, dest, RemoteState::Done, 0, 0, None).await.unwrap());
}

async fn item(db: &Db, job: &str, seq: i64) -> cosmog_lib::db::folder_upload::ItemRow {
    let (rows, _) = db.fu_list_items(job, ItemFilter::All, 0, 100_000).await.unwrap();
    rows.into_iter().find(|r| r.seq == seq).expect("item")
}

#[tokio::test]
async fn job_state_transitions_persist_across_reopen() {
    let (db, td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j1", "dst/").await;

    let j = db.fu_get_job("j1").await.unwrap();
    assert_eq!(j.state, JobState::Analyzing);
    assert_eq!(j.remote_state, RemoteState::Pending);
    assert!(j.create_dir_markers);
    assert!(j.approved_at.is_none() && j.started_at.is_none() && j.finished_at.is_none());

    // Approval is refused until analysis is done.
    let err = db.fu_approve("j1", None, true, None, JobState::Queued).await.unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    let err = db.fu_approve("nope", None, true, None, JobState::Queued).await.unwrap_err();
    assert_eq!(err.code(), "not_found");

    db.fu_set_analysis("j1", r#"{"files":3}"#.into(), 3, 300).await.unwrap();
    db.fu_set_state("j1", JobState::Analyzed, None, None).await.unwrap();
    checked(&db, "j1", "dst/").await;
    db.fu_approve("j1", Some(OverwritePolicy::Skip), false, Some("age1pub".into()), JobState::Queued)
        .await
        .unwrap();
    let j = db.fu_get_job("j1").await.unwrap();
    assert_eq!(j.state, JobState::Queued);
    assert_eq!(j.overwrite_policy, Some(OverwritePolicy::Skip));
    assert!(!j.create_dir_markers);
    assert_eq!(j.enc_recipient.as_deref(), Some("age1pub"));
    assert!(j.approved_at.is_some());
    assert!(j.started_at.is_none());
    // A second approval is refused.
    assert!(db.fu_approve("j1", None, true, None, JobState::Queued).await.is_err());

    db.fu_set_state("j1", JobState::Running, None, None).await.unwrap();
    let started = db.fu_get_job("j1").await.unwrap().started_at.expect("started_at");
    db.fu_set_state("j1", JobState::Paused, Some("folder_source_unavailable".into()), None).await.unwrap();
    let j = db.fu_get_job("j1").await.unwrap();
    assert_eq!(j.state, JobState::Paused);
    assert_eq!(j.pause_reason.as_deref(), Some("folder_source_unavailable"));
    db.fu_set_state("j1", JobState::Running, None, None).await.unwrap();
    let j = db.fu_get_job("j1").await.unwrap();
    assert_eq!(j.pause_reason, None);
    assert_eq!(j.started_at, Some(started), "started_at is stamped once");
    assert!(j.finished_at.is_none());
    db.fu_set_state("j1", JobState::CompletedWithErrors, None, Some("e".into())).await.unwrap();
    let before = db.fu_get_job("j1").await.unwrap();
    assert!(before.finished_at.is_some());

    // Unknown job.
    assert_eq!(db.fu_set_state("x", JobState::Running, None, None).await.unwrap_err().code(), "not_found");

    let path = td.path().join("test.sqlite");
    drop(db);
    let db = Db::open(&path).await.unwrap();
    let after = db.fu_get_job("j1").await.unwrap();
    assert_eq!(after.state, JobState::CompletedWithErrors);
    assert_eq!(after.error.as_deref(), Some("e"));
    assert_eq!(after.analysis_json.as_deref(), Some(r#"{"files":3}"#));
    assert_eq!((after.total_files, after.total_bytes), (3, 300));
    assert_eq!(after.finished_at, before.finished_at);
    assert_eq!(after.started_at, Some(started));
    assert_eq!(db.fu_list_jobs().await.unwrap().len(), 1);
}

#[tokio::test]
async fn find_unfinished_matches_only_approved_open_jobs() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j1", "d/").await;
    assert_eq!(db.fu_find_unfinished(&acct, "b", "/src/j1", "d/").await.unwrap(), None);
    db.fu_set_state("j1", JobState::Paused, None, None).await.unwrap();
    assert_eq!(db.fu_find_unfinished(&acct, "b", "/src/j1", "d/").await.unwrap().as_deref(), Some("j1"));
    assert_eq!(db.fu_find_unfinished(&acct, "b", "/src/j1", "other/").await.unwrap(), None);
    db.fu_set_state("j1", JobState::Completed, None, None).await.unwrap();
    assert_eq!(db.fu_find_unfinished(&acct, "b", "/src/j1", "d/").await.unwrap(), None);
}

#[tokio::test]
async fn items_insert_in_batches_and_list_in_seq_order() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;

    // Second half first, each batch reversed.
    let batch2: Vec<_> = (1000..2000).rev().map(|s| file(s, &format!("b/{s}"), s)).collect();
    let batch1: Vec<_> = (0..1000).rev().map(|s| file(s, &format!("a/{s}"), s)).collect();
    db.fu_insert_items("j", batch2).await.unwrap();
    db.fu_insert_items("j", batch1).await.unwrap();

    let (rows, total) = db.fu_list_items("j", ItemFilter::All, 0, 5000).await.unwrap();
    assert_eq!(total, 2000);
    let seqs: Vec<i64> = rows.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (0..2000).collect::<Vec<_>>());
    assert_eq!(rows[1500].rel_path, "b/1500");
    assert_eq!(rows[1500].size, 1500);
    assert_eq!(rows[1500].state, ItemState::Pending);

    let (page, total) = db.fu_list_items("j", ItemFilter::All, 998, 4).await.unwrap();
    assert_eq!(total, 2000);
    assert_eq!(page.iter().map(|r| r.rel_path.as_str()).collect::<Vec<_>>(), ["a/998", "a/999", "b/1000", "b/1001"]);
}

#[tokio::test]
async fn partial_unique_index_on_rel_path() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items("j", vec![file(0, "x.txt", 1)]).await.unwrap();

    // A live duplicate fails and rolls back its whole batch.
    let err = db.fu_insert_items("j", vec![file(1, "y.txt", 1), file(2, "x.txt", 1)]).await;
    assert!(err.is_err());
    let (_, total) = db.fu_list_items("j", ItemFilter::All, 0, 10).await.unwrap();
    assert_eq!(total, 1, "batch rolled back");

    // An excluded duplicate is allowed.
    db.fu_insert_items("j", vec![excluded(3, "x.txt", ExcludeReason::NonUtf8)]).await.unwrap();
    let (rows, total) = db.fu_list_items("j", ItemFilter::Excluded, 0, 10).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].exclude_reason, Some(ExcludeReason::NonUtf8));

    // Same rel_path in another job is fine.
    seed_job(&db, &acct, "k", "").await;
    db.fu_insert_items("k", vec![file(0, "x.txt", 1)]).await.unwrap();
}

#[tokio::test]
async fn set_dest_recomputes_key_too_long_on_utf8_bytes() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;

    // 'é' is 2 bytes: char counts stay far under 1024 while byte counts reach it.
    let fits = "é".repeat(511); // 1022 bytes, + "d/" = 1024
    let over = format!("{}a", "é".repeat(511)); // 1023 bytes, + "d/" = 1025
    let dir_fits = format!("{}a", "é".repeat(510)); // 1021, + "d/" + "/" = 1024
    let dir_over = format!("{}ab", "é".repeat(510)); // 1022, + "d/" + "/" = 1025
    assert!(over.chars().count() < 1024);
    let dir = |seq: i64, rel: &str| NewItem { kind: ItemKind::Dir, ..file(seq, rel, 0) };
    db.fu_insert_items(
        "j",
        vec![
            file(0, &fits, 1),
            file(1, &over, 1),
            dir(2, &dir_fits),
            dir(3, &dir_over),
            excluded(4, &"z".repeat(2000), ExcludeReason::Symlink),
            file(5, "short", 1),
        ],
    )
    .await
    .unwrap();

    assert_eq!(db.fu_set_dest("j", "d/").await.unwrap(), 2);
    assert_eq!(db.fu_get_job("j").await.unwrap().dest_prefix, "d/");
    let st = |r: cosmog_lib::db::folder_upload::ItemRow| (r.state, r.exclude_reason);
    assert_eq!(st(item(&db, "j", 0).await), (ItemState::Pending, None));
    assert_eq!(st(item(&db, "j", 1).await), (ItemState::Excluded, Some(ExcludeReason::KeyTooLong)));
    assert_eq!(st(item(&db, "j", 2).await), (ItemState::Pending, None));
    assert_eq!(st(item(&db, "j", 3).await), (ItemState::Excluded, Some(ExcludeReason::KeyTooLong)));
    assert_eq!(db.fu_counts("j").await.unwrap().key_too_long, 2);

    // Prefix grows by one byte: the exact-fit items cross the boundary.
    assert_eq!(db.fu_set_dest("j", "dd/").await.unwrap(), 4);
    assert_eq!(st(item(&db, "j", 0).await).0, ItemState::Excluded);

    // Prefix shrinks: key_too_long items return to pending; other exclusions stay.
    assert_eq!(db.fu_set_dest("j", "").await.unwrap(), 0);
    for seq in [0, 1, 2, 3, 5] {
        assert_eq!(st(item(&db, "j", seq).await), (ItemState::Pending, None), "seq {seq}");
    }
    assert_eq!(st(item(&db, "j", 4).await), (ItemState::Excluded, Some(ExcludeReason::Symlink)));

    assert_eq!(db.fu_set_dest("missing", "").await.unwrap_err().code(), "not_found");
}

#[tokio::test]
async fn mark_remote_then_set_dest_resets_remote_state() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "d/").await;
    db.fu_insert_items("j", vec![file(0, "a", 10), file(1, "b", 20), file(2, "c", 30)]).await.unwrap();
    db.fu_set_state("j", JobState::Analyzed, None, None).await.unwrap();

    assert_eq!(db.fu_begin_remote("j", "old/").await.unwrap(), None, "only the current dest can be checked");
    let g = db.fu_begin_remote("j", "d/").await.unwrap().unwrap();
    // A stale check (older generation) writes nothing.
    assert_eq!(db.fu_mark_remote("j", g - 1, vec![("a".into(), 1, None)]).await.unwrap_err().code(), "canceled");
    let n = db
        .fu_mark_remote("j", g, vec![("a".into(), 11, Some(5)), ("b".into(), 22, None), ("nope".into(), 1, None)])
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert!(db.fu_finish_remote("j", g, "d/", RemoteState::Done, 2, 33, Some("enabled".into())).await.unwrap());
    assert!(!db.fu_finish_remote("j", g, "old/", RemoteState::Done, 9, 9, None).await.unwrap());
    assert!(!db.fu_finish_remote("j", g - 1, "d/", RemoteState::Done, 9, 9, None).await.unwrap());

    let j = db.fu_get_job("j").await.unwrap();
    assert_eq!(j.remote_state, RemoteState::Done);
    assert_eq!(j.remote_prefix.as_deref(), Some("d/"));
    assert_eq!((j.remote_existing, j.remote_existing_bytes), (2, 33));
    assert_eq!(j.remote_versioning.as_deref(), Some("enabled"));
    let a = item(&db, "j", 0).await;
    assert!(a.remote_exists);
    assert_eq!((a.remote_size, a.remote_mtime), (Some(11), Some(5)));
    let (existing, total) = db.fu_list_items("j", ItemFilter::Existing, 0, 10).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(existing.iter().map(|r| r.seq).collect::<Vec<_>>(), [0, 1]);

    db.fu_set_dest("j", "e/").await.unwrap();
    let j = db.fu_get_job("j").await.unwrap();
    assert_eq!(j.remote_state, RemoteState::Pending);
    assert_eq!(j.remote_prefix, None);
    assert_eq!((j.remote_existing, j.remote_existing_bytes), (0, 0));
    assert_eq!(j.remote_versioning, None);
    let (rows, _) = db.fu_list_items("j", ItemFilter::All, 0, 10).await.unwrap();
    for r in rows {
        assert!(!r.remote_exists && r.remote_size.is_none() && r.remote_mtime.is_none());
    }
    assert_eq!(db.fu_list_items("j", ItemFilter::Existing, 0, 10).await.unwrap().1, 0);
}

#[tokio::test]
async fn next_batch_honors_limit_order_due_time_and_kind() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items(
        "j",
        vec![
            file(0, "f0", 1),
            NewItem { kind: ItemKind::Dir, ..file(1, "d1", 0) },
            file(2, "f2", 1),
            file(3, "f3", 1),
            excluded(4, "l4", ExcludeReason::Symlink),
            file(5, "f5", 1),
        ],
    )
    .await
    .unwrap();
    let seqs = |v: Vec<cosmog_lib::db::folder_upload::ItemRow>| v.iter().map(|r| r.seq).collect::<Vec<_>>();

    assert_eq!(seqs(db.fu_next_batch("j", 3, 100).await.unwrap()), [0, 1, 2]);
    db.fu_item_started("j", 0, "t0").await.unwrap();
    db.fu_item_retry_later("j", 2, 200, None, true).await.unwrap();
    assert_eq!(seqs(db.fu_next_batch("j", 10, 100).await.unwrap()), [1, 3, 5]);
    assert_eq!(seqs(db.fu_next_batch("j", 10, 200).await.unwrap()), [1, 2, 3, 5]);
    assert!(db.fu_next_batch("j", 0, 200).await.unwrap().is_empty());

    let inflight = db.fu_inflight("j").await.unwrap();
    assert_eq!(seqs(inflight.clone()), [0]);
    assert_eq!(inflight[0].transfer_id.as_deref(), Some("t0"));
    assert_eq!(inflight[0].state, ItemState::Uploading);
    assert_eq!(db.fu_counts("j").await.unwrap().next_due_at, Some(0));
}

#[tokio::test]
async fn retry_later_counts_attempts_only_when_asked() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items("j", vec![file(0, "a", 1)]).await.unwrap();

    db.fu_item_started("j", 0, "t1").await.unwrap();
    db.fu_item_retry_later("j", 0, 50, Some("net".into()), false).await.unwrap();
    let r = item(&db, "j", 0).await;
    assert_eq!((r.state, r.attempts, r.next_attempt_at), (ItemState::Pending, 0, 50));
    assert_eq!(r.transfer_id, None);
    assert_eq!(r.error.as_deref(), Some("net"));

    db.fu_item_started("j", 0, "t2").await.unwrap();
    db.fu_item_retry_later("j", 0, 90, Some("slow".into()), true).await.unwrap();
    db.fu_item_retry_later("j", 0, 120, None, true).await.unwrap();
    let r = item(&db, "j", 0).await;
    assert_eq!((r.attempts, r.next_attempt_at, r.error), (2, 120, None));

    assert_eq!(db.fu_item_retry_later("j", 99, 0, None, true).await.unwrap_err().code(), "not_found");
}

#[tokio::test]
async fn counts_done_bytes_reset_and_cancel() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items(
        "j",
        vec![
            file(0, "a", 100),
            file(1, "b", 200),
            NewItem { kind: ItemKind::Dir, ..file(2, "d", 7) },
            file(3, "c", 300),
            file(4, "e", 400),
            file(5, "f", 500),
            file(6, ".h", 600),
            excluded(7, "l", ExcludeReason::Symlink),
        ],
    )
    .await
    .unwrap();

    db.fu_item_finished("j", 0, ItemState::Done, None, None).await.unwrap();
    db.fu_item_drift("j", 1, 250, 9, Drift::Changed).await.unwrap();
    db.fu_item_finished("j", 1, ItemState::Done, None, None).await.unwrap();
    db.fu_item_finished("j", 2, ItemState::Done, None, None).await.unwrap();
    db.fu_item_finished("j", 3, ItemState::Failed, Some("boom".into()), Some(Drift::ChangedDuringUpload)).await.unwrap();
    db.fu_item_finished("j", 4, ItemState::SkippedExists, None, None).await.unwrap();
    db.fu_item_started("j", 5, "t5").await.unwrap();

    let r1 = item(&db, "j", 1).await;
    assert_eq!((r1.size, r1.mtime_ms, r1.drift), (250, 9, Some(Drift::Changed)), "drift kept on finish");
    let c = db.fu_counts("j").await.unwrap();
    assert_eq!((c.pending, c.uploading, c.done, c.failed, c.skipped_exists, c.excluded), (1, 1, 3, 1, 1, 1));
    assert_eq!((c.done_files, c.drift, c.appeared_after_check, c.skipped_missing, c.canceled), (2, 2, 1, 0, 0));
    assert_eq!(db.fu_done_bytes("j").await.unwrap(), 100 + 250);

    assert_eq!(db.fu_list_items("j", ItemFilter::Failed, 0, 10).await.unwrap().1, 1);
    assert_eq!(db.fu_list_items("j", ItemFilter::Skipped, 0, 10).await.unwrap().1, 1);
    assert_eq!(db.fu_list_items("j", ItemFilter::Drift, 0, 10).await.unwrap().1, 2);
    assert_eq!(db.fu_list_items("j", ItemFilter::Hidden, 0, 10).await.unwrap().0[0].rel_path, ".h");

    db.fu_item_retry_later("j", 3, 0, None, true).await.unwrap();
    db.fu_item_finished("j", 3, ItemState::Failed, Some("again".into()), None).await.unwrap();
    assert_eq!(db.fu_reset_failed("j").await.unwrap(), 1);
    let r3 = item(&db, "j", 3).await;
    assert_eq!((r3.state, r3.attempts, r3.next_attempt_at, r3.error), (ItemState::Pending, 0, 0, None));

    assert_eq!(db.fu_cancel_pending("j").await.unwrap(), 2, "items 3 and 6");
    let c = db.fu_counts("j").await.unwrap();
    assert_eq!((c.pending, c.canceled, c.uploading), (0, 2, 1));
    assert_eq!(c.next_due_at, None);

    db.fu_item_exclude("j", 5, ExcludeReason::Symlink).await.unwrap();
    let r5 = item(&db, "j", 5).await;
    assert_eq!((r5.state, r5.exclude_reason, r5.transfer_id), (ItemState::Excluded, Some(ExcludeReason::Symlink), None));
    // Excluded after approval is drift and an error; the walk-time exclusion (seq 7) is not.
    assert_eq!(r5.drift, Some(Drift::Changed));
    let c = db.fu_counts("j").await.unwrap();
    assert_eq!((c.excluded, c.excluded_late), (2, 1));
}

#[tokio::test]
async fn startup_cleanup_drops_only_unapproved_jobs() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    let states = [
        ("a", JobState::Analyzing),
        ("b", JobState::Analyzed),
        ("c", JobState::Queued),
        ("d", JobState::Running),
        ("e", JobState::Paused),
        ("f", JobState::Completed),
    ];
    for (id, st) in states {
        seed_job(&db, &acct, id, "").await;
        db.fu_insert_items(id, vec![file(0, "x", 1)]).await.unwrap();
        db.fu_set_state(id, st, None, None).await.unwrap();
    }
    // An analysis of this run (in `keep`, or created after `before`) survives.
    seed_job(&db, &acct, "g", "").await;
    let now = chrono::Utc::now().timestamp();
    assert_eq!(db.fu_startup_cleanup(vec!["b".into()], now + 10).await.unwrap(), 2, "a and g");
    seed_job(&db, &acct, "h", "").await;
    assert_eq!(db.fu_startup_cleanup(vec![], now - 10).await.unwrap(), 0, "created after start");
    sql(&db, "UPDATE folder_upload_jobs SET created_at = 0 WHERE id IN ('b','h')").await;
    assert_eq!(db.fu_startup_cleanup(vec![], now - 10).await.unwrap(), 2, "b and h");
    let ids: Vec<String> = db.fu_list_jobs().await.unwrap().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, ["c", "d", "e", "f"]);
    assert_eq!(db.fu_list_items("a", ItemFilter::All, 0, 10).await.unwrap().1, 0, "items cascade");
    assert_eq!(db.fu_list_items("c", ItemFilter::All, 0, 10).await.unwrap().1, 1);
}

#[tokio::test]
async fn prune_finished_keeps_newest_and_recent() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    for (i, id) in ["f1", "f2", "f3", "f4"].iter().enumerate() {
        seed_job(&db, &acct, id, "").await;
        db.fu_set_state(id, JobState::Completed, None, None).await.unwrap();
        let at = 1_000 + i as i64 * 100;
        db.conn
            .call(move |c| Ok::<_, tokio_rusqlite::Error>(c.execute("UPDATE folder_upload_jobs SET finished_at = ?1 WHERE id = ?2", params![at, format!("f{}", i + 1)])?))
            .await
            .unwrap();
    }
    seed_job(&db, &acct, "run", "").await;
    db.fu_set_state("run", JobState::Running, None, None).await.unwrap();
    db.insert_transfer(transfer("t_old", &acct, Some("f1"))).await.unwrap();

    // keep 3 newest; f1 goes, along with its transfer row (no upload_id).
    assert_eq!(db.fu_prune_finished(3, 0).await.unwrap(), 1);
    assert!(db.get_transfer("t_old").await.is_err());
    // Older than 1150 removes f2 (1100) only.
    assert_eq!(db.fu_prune_finished(10, 1150).await.unwrap(), 1);
    let ids: Vec<String> = db.fu_list_jobs().await.unwrap().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, ["f3", "f4", "run"]);
    // keep 0 clears every finished job, never an unfinished one.
    assert_eq!(db.fu_prune_finished(0, 0).await.unwrap(), 2);
    let ids: Vec<String> = db.fu_list_jobs().await.unwrap().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, ["run"]);
}

#[tokio::test]
async fn delete_job_and_account_cascade() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    seed_job(&db, &acct, "k", "").await;
    db.fu_insert_items("j", vec![file(0, "a", 1), file(1, "b", 1)]).await.unwrap();
    db.fu_insert_items("k", vec![file(0, "a", 1)]).await.unwrap();

    db.fu_delete_job("k").await.unwrap();
    assert_eq!(db.fu_get_job("k").await.unwrap_err().code(), "not_found");
    assert_eq!(db.fu_list_items("k", ItemFilter::All, 0, 10).await.unwrap().1, 0);

    db.delete_account(&acct).await.unwrap();
    assert!(db.fu_list_jobs().await.unwrap().is_empty());
    let left: i64 = db
        .conn
        .call(|c| Ok::<_, tokio_rusqlite::Error>(c.query_row("SELECT COUNT(*) FROM folder_upload_items", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn job_transfer_rows_stay_out_of_the_normal_list() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_transfer(transfer("u1", &acct, None)).await.unwrap();
    db.insert_transfer(transfer("j1", &acct, Some("job"))).await.unwrap();
    db.insert_transfer(transfer("j2", &acct, Some("job"))).await.unwrap();
    db.insert_transfer(transfer("o1", &acct, Some("other"))).await.unwrap();
    db.conn
        .call(|c| {
            c.execute("UPDATE transfers SET upload_id = 'up', parts_json = '{\"p\":1}' WHERE id IN ('j1','u1')", [])?;
            Ok::<_, tokio_rusqlite::Error>(c.execute("UPDATE transfers SET status = 'done'", [])?)
        })
        .await
        .unwrap();

    let ids = |v: Vec<cosmog_lib::db::transfers::Transfer>| v.into_iter().map(|t| t.id).collect::<Vec<_>>();
    assert_eq!(ids(db.list_transfers(None).await.unwrap()), ["u1"]);
    assert_eq!(ids(db.list_transfers(Some(TransferStatus::Done)).await.unwrap()), ["u1"]);
    assert_eq!(ids(db.list_finished_with_upload().await.unwrap()), ["u1"]);

    let rows = db.list_job_transfers("job").await.unwrap();
    assert_eq!(rows.len(), 2);
    let j1 = rows.iter().find(|t| t.id == "j1").unwrap();
    assert_eq!(j1.job_id.as_deref(), Some("job"));
    assert_eq!(j1.upload_id.as_deref(), Some("up"));
    assert_eq!(j1.parts_json.as_deref(), Some("{\"p\":1}"));
    assert_eq!(db.get_transfer("j2").await.unwrap().job_id.as_deref(), Some("job"));
    assert_eq!(db.get_transfer("u1").await.unwrap().job_id, None);

    // Prune and clear-completed never touch job rows.
    assert_eq!(db.prune_finished_transfers(0).await.unwrap(), 0, "u1 holds an upload_id");
    assert_eq!(db.clear_completed_transfers().await.unwrap(), 1);
    assert_eq!(db.list_job_transfers("job").await.unwrap().len(), 2);
    assert_eq!(db.list_job_transfers("other").await.unwrap().len(), 1);

    assert_eq!(db.delete_transfer_rows(vec!["j1".into(), "j2".into(), "missing".into()]).await.unwrap(), 2);
    assert!(db.list_job_transfers("job").await.unwrap().is_empty());
    assert_eq!(db.delete_transfer_rows(vec![]).await.unwrap(), 0);
}

#[tokio::test]
async fn reaping_marks_job_rows_failed() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_transfer(transfer("j1", &acct, Some("job"))).await.unwrap();
    db.insert_transfer(transfer("j2", &acct, Some("job"))).await.unwrap();
    db.update_transfer_status("j2", TransferStatus::Active, None).await.unwrap();
    db.insert_transfer(transfer("j3", &acct, Some("job"))).await.unwrap();
    db.update_transfer_status("j3", TransferStatus::Paused, None).await.unwrap();

    assert_eq!(db.reap_orphan_transfers().await.unwrap(), 2);
    for (id, want) in [("j1", TransferStatus::Failed), ("j2", TransferStatus::Failed), ("j3", TransferStatus::Paused)] {
        let t = db.get_transfer(id).await.unwrap();
        assert_eq!(t.status, want, "{id}");
        assert_eq!(t.job_id.as_deref(), Some("job"));
    }
    let err = db.get_transfer("j1").await.unwrap().error.unwrap();
    assert!(err.contains("\"code\":\"interrupted\""), "{err}");
}

#[tokio::test]
async fn version_20_database_migrates_to_latest() {
    let (db, td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    db.insert_transfer(transfer("old", &acct, None)).await.unwrap();
    // Roll the file back to the v20 schema.
    db.conn
        .call(|c| {
            c.execute_batch(
                "DROP TABLE folder_upload_counts; DROP TABLE folder_upload_items; DROP TABLE folder_upload_jobs;
                 DROP INDEX idx_transfers_job; ALTER TABLE transfers DROP COLUMN job_id;
                 DELETE FROM schema_migrations WHERE version >= 21;",
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    let path = td.path().join("test.sqlite");
    drop(db);

    let db = Db::open(&path).await.unwrap();
    let max: i64 = db
        .conn
        .call(|c| Ok::<_, tokio_rusqlite::Error>(c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(max, 22);
    let old = db.get_transfer("old").await.unwrap();
    assert_eq!(old.job_id, None);
    assert_eq!(db.list_transfers(None).await.unwrap().len(), 1);
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items("j", vec![file(0, "a", 1)]).await.unwrap();
    assert_eq!(db.fu_counts("j").await.unwrap().pending, 1);
}

// A database that applied the first cut of migration 21 gains the later columns and seeded counts.
#[tokio::test]
async fn version_21_database_migrates_to_22() {
    let (db, td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_insert_items("j", vec![file(0, "a", 3), file(1, "b", 4)]).await.unwrap();
    // Roll the file back to the v21 schema, keeping its rows.
    db.conn
        .call(|c| {
            c.execute_batch(
                "DROP TRIGGER fuc_ins; DROP TRIGGER fuc_del; DROP TRIGGER fuc_upd;
                 DROP TABLE folder_upload_counts; DROP INDEX idx_fui_order;
                 ALTER TABLE folder_upload_jobs DROP COLUMN remote_gen;
                 ALTER TABLE folder_upload_items DROP COLUMN done_etag;
                 DELETE FROM schema_migrations WHERE version = 22;",
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
    let path = td.path().join("test.sqlite");
    drop(db);

    let db = Db::open(&path).await.unwrap();
    assert_eq!(db.fu_list_jobs().await.unwrap().len(), 1);
    assert_eq!(db.fu_get_job("j").await.unwrap().id, "j");
    assert_eq!(db.fu_counts("j").await.unwrap().pending, 2, "counts seeded from existing items");
    db.fu_insert_items("j", vec![file(2, "c", 5)]).await.unwrap();
    assert_eq!(db.fu_counts("j").await.unwrap().pending, 3, "triggers live after the migration");
    assert_eq!(db.fu_list_items("j", ItemFilter::All, 0, 10).await.unwrap().1, 3);
}

/// M3: the destination is frozen once approved, and approval needs a finished check of the current destination.
#[tokio::test]
async fn approval_needs_a_check_of_the_current_dest() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "a/").await;
    db.fu_insert_items("j", vec![file(0, "x", 1)]).await.unwrap();
    db.fu_set_state("j", JobState::Analyzed, None, None).await.unwrap();
    let skip = Some(OverwritePolicy::Skip);
    assert_eq!(db.fu_approve("j", skip, true, None, JobState::Queued).await.unwrap_err().code(), "invalid_input", "no check yet");
    let g = db.fu_begin_remote("j", "a/").await.unwrap().unwrap();
    assert_eq!(db.fu_approve("j", skip, true, None, JobState::Queued).await.unwrap_err().code(), "invalid_input", "still checking");
    assert!(db.fu_finish_remote("j", g, "a/", RemoteState::Done, 0, 0, None).await.unwrap());
    // A dest change after the check: the old result no longer counts.
    db.fu_set_dest("j", "b/").await.unwrap();
    assert_eq!(db.fu_approve("j", skip, true, None, JobState::Queued).await.unwrap_err().code(), "invalid_input", "check was for a/");
    checked(&db, "j", "b/").await;
    db.fu_approve("j", skip, true, None, JobState::Queued).await.unwrap();
    // A late dest change (a stale check finishing) cannot move an approved job.
    assert_eq!(db.fu_set_dest("j", "c/").await.unwrap_err().code(), "invalid_input");
    let j = db.fu_get_job("j").await.unwrap();
    assert_eq!((j.dest_prefix.as_str(), j.state), ("b/", JobState::Queued));
    assert!(db.fu_begin_remote("j", "b/").await.unwrap().is_none(), "no check once approved");
}

/// M4: a newer check (same dest) supersedes the older one, whose writes become no-ops.
#[tokio::test]
async fn superseded_check_cannot_write() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "d/").await;
    db.fu_insert_items("j", vec![file(0, "a", 1), file(1, "b", 1)]).await.unwrap();
    db.fu_set_state("j", JobState::Analyzed, None, None).await.unwrap();
    let old = db.fu_begin_remote("j", "d/").await.unwrap().unwrap();
    db.fu_mark_remote("j", old, vec![("a".into(), 1, None)]).await.unwrap();
    let new = db.fu_begin_remote("j", "d/").await.unwrap().unwrap();
    assert!(!item(&db, "j", 0).await.remote_exists, "a new check starts from clean flags");
    assert!(db.fu_mark_remote("j", old, vec![("b".into(), 1, None)]).await.is_err());
    assert!(!db.fu_finish_remote("j", old, "d/", RemoteState::Done, 0, 0, None).await.unwrap(), "old check cannot report done");
    assert_eq!(db.fu_get_job("j").await.unwrap().remote_state, RemoteState::Checking);
    db.fu_mark_remote("j", new, vec![("a".into(), 1, None), ("b".into(), 1, None)]).await.unwrap();
    assert!(db.fu_finish_remote("j", new, "d/", RemoteState::Done, 2, 2, None).await.unwrap());
    assert_eq!(db.fu_get_job("j").await.unwrap().remote_existing, 2);
}

/// M5 support: the runner's final write only lands while the job is still running.
#[tokio::test]
async fn conditional_state_write() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "j", "").await;
    db.fu_set_state("j", JobState::Canceled, None, None).await.unwrap();
    assert!(!db.fu_set_state_if("j", JobState::Running, JobState::Completed, None, None).await.unwrap());
    assert_eq!(db.fu_get_job("j").await.unwrap().state, JobState::Canceled);
    db.fu_set_state("j", JobState::Running, None, None).await.unwrap();
    assert!(db.fu_set_state_if("j", JobState::Running, JobState::Completed, None, None).await.unwrap());
    let j = db.fu_get_job("j").await.unwrap();
    assert_eq!(j.state, JobState::Completed);
    assert!(j.finished_at.is_some());
}

/// L1: trigger-kept counts always equal a full scan, through every kind of item update.
#[tokio::test]
async fn trigger_counts_match_a_full_scan() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    for job in ["j", "k"] {
        seed_job(&db, &acct, job, "").await;
        let mut items: Vec<NewItem> = (0..300).map(|i| file(i, &format!("f{i}"), i * 3)).collect();
        items.push(NewItem { kind: ItemKind::Dir, ..file(300, "d", 0) });
        items.push(excluded(301, "l", ExcludeReason::KeyTooLong));
        db.fu_insert_items(job, items).await.unwrap();
    }
    let mut rng = 7u64;
    let mut next = |n: u64| {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 33) % n
    };
    for _ in 0..1500 {
        let seq = next(302) as i64;
        match next(9) {
            0 => db.fu_item_started("j", seq, "t").await.unwrap(),
            1 => db.fu_item_done("j", seq, Some("e".into())).await.unwrap(),
            2 => db.fu_item_finished("j", seq, ItemState::SkippedExists, None, None).await.unwrap(),
            3 => db.fu_item_finished("j", seq, ItemState::Failed, Some("x".into()), Some(Drift::ChangedDuringUpload)).await.unwrap(),
            4 => db.fu_item_drift("j", seq, next(1000) as i64, 5, Drift::Changed).await.unwrap(),
            5 => db.fu_item_exclude("j", seq, ExcludeReason::Special).await.unwrap(),
            6 => db.fu_item_retry_later("j", seq, next(50) as i64, None, true).await.unwrap(),
            7 => db.fu_item_park("j", seq, "t", "e".into(), false).await.unwrap(),
            _ => {
                db.fu_reset_failed("j").await.unwrap();
            }
        }
    }
    // Some remote hits (they matter for skipped_exists), a dest change and a cancel.
    db.fu_set_state("j", JobState::Analyzed, None, None).await.unwrap();
    let g = db.fu_begin_remote("j", "").await.unwrap().unwrap();
    db.fu_mark_remote("j", g, (0..100).map(|i| (format!("f{i}"), 1, None)).collect()).await.unwrap();
    db.fu_cancel_pending("j").await.unwrap();
    let got = db.fu_counts("j").await.unwrap();
    let bytes = db.fu_done_bytes("j").await.unwrap();
    let want = db
        .conn
        .call(|c| {
            let q = "SELECT SUM(state='pending'), SUM(state='uploading'), SUM(state='done'), SUM(state='skipped_exists'),
                SUM(state='skipped_missing'), SUM(state='failed'), SUM(state='canceled'), SUM(state='excluded'),
                SUM(state='done' AND kind='file'), SUM(drift IS NOT NULL), SUM(state='skipped_exists' AND remote_exists=0),
                SUM(state='excluded' AND exclude_reason='key_too_long'), SUM(state='excluded' AND drift IS NOT NULL),
                MIN(CASE WHEN state='pending' THEN next_attempt_at END),
                COALESCE(SUM(CASE WHEN state='done' AND kind='file' THEN size END), 0)
                FROM folder_upload_items WHERE job_id = 'j'";
            Ok::<_, tokio_rusqlite::Error>(c.query_row(q, [], |r| {
                Ok(((0..13).map(|i| r.get::<_, i64>(i)).collect::<Result<Vec<_>, _>>()?, r.get::<_, Option<i64>>(13)?, r.get::<_, i64>(14)?))
            })?)
        })
        .await
        .unwrap();
    let mine = vec![
        got.pending, got.uploading, got.done, got.skipped_exists, got.skipped_missing, got.failed, got.canceled,
        got.excluded, got.done_files, got.drift, got.appeared_after_check, got.key_too_long, got.excluded_late,
    ];
    assert_eq!(mine, want.0);
    assert_eq!(got.next_due_at, want.1);
    assert_eq!(bytes, want.2);
    assert!(got.done > 0 && got.excluded_late > 0 && got.appeared_after_check >= 0);
    // The other job's counts are untouched, and a deleted job leaves no counts behind.
    assert_eq!(db.fu_counts("k").await.unwrap().pending, 301);
    db.fu_delete_job("k").await.unwrap();
    let left: i64 = db
        .conn
        .call(|c| Ok::<_, tokio_rusqlite::Error>(c.query_row("SELECT COUNT(*) FROM folder_upload_counts WHERE job_id = 'k'", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// H2: marking remote hits on a 200k-item job uses the rel_path index (no scan per key).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mark_remote_is_indexed_on_a_large_job() {
    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    seed_job(&db, &acct, "big", "").await;
    for chunk in 0..200 {
        let items = (0..1000).map(|i| {
            let n = chunk * 1000 + i;
            file(n, &format!("d{}/f{n}", n % 300), 1)
        });
        db.fu_insert_items("big", items.collect()).await.unwrap();
    }
    db.fu_set_state("big", JobState::Analyzed, None, None).await.unwrap();
    let g = db.fu_begin_remote("big", "").await.unwrap().unwrap();
    let t = std::time::Instant::now();
    for page in 0..2 {
        let hits = (0..1000).map(|i| {
            let n = page * 50_000 + i * 7;
            (format!("d{}/f{n}", n % 300), 1, None)
        });
        assert_eq!(db.fu_mark_remote("big", g, hits.collect()).await.unwrap(), 1000);
    }
    let took = t.elapsed();
    eprintln!("2000 remote hits on 200k items: {took:?}");
    // A scan per key costs seconds here; the index lookup a few milliseconds.
    assert!(took < std::time::Duration::from_millis(1500), "took {took:?}");
    let t = std::time::Instant::now();
    let c = db.fu_counts("big").await.unwrap();
    eprintln!("counts on 200k items: {:?}", t.elapsed());
    assert_eq!(c.pending, 200_000);
    assert!(t.elapsed() < std::time::Duration::from_millis(50));
}

#[tokio::test]
async fn saf_tree_is_in_use_until_jobs_finish_and_watches_go() {
    use cosmog_lib::db::night_watcher::NewWatch;
    use cosmog_lib::folder_upload::tree_in_use;

    let (db, _td) = common::tmp_db().await;
    let acct = seed_account(&db).await;
    let tree = "content://com.android.externalstorage.documents/tree/primary%3ADCIM";
    // Same tree, other spelling: lowercase escape.
    let alt = "content://com.android.externalstorage.documents/tree/primary%3aDCIM";
    db.fu_insert_job(NewJob {
        id: "s1".into(),
        account_id: acct.clone(),
        bucket: "b".into(),
        source_kind: "saf".into(),
        source_root: tree.into(),
        dest_prefix: "".into(),
    })
    .await
    .unwrap();
    assert!(tree_in_use(&db, alt).await.unwrap(), "an unfinished job holds the tree");
    let other = "content://com.android.externalstorage.documents/tree/primary%3ADCIM%2FCamera";
    assert!(!tree_in_use(&db, other).await.unwrap(), "a subfolder is another tree");

    db.fu_set_state("s1", JobState::CompletedWithErrors, None, None).await.unwrap();
    assert!(tree_in_use(&db, tree).await.unwrap(), "retry_failed can reopen it: still held");
    db.fu_set_state("s1", JobState::Completed, None, None).await.unwrap();
    assert!(!tree_in_use(&db, tree).await.unwrap(), "completed jobs release it");

    db.insert_watch(
        "w1",
        NewWatch {
            account_id: acct.clone(),
            bucket: "b".into(),
            local_dir: "DCIM".into(),
            key_prefix: "".into(),
            ignore_file: None,
            full_scan_secs: 300,
            tree_uri: Some(alt.into()),
        },
    )
    .await
    .unwrap();
    assert!(tree_in_use(&db, tree).await.unwrap(), "a Night Watcher watch holds it");
    db.delete_watch("w1").await.unwrap();
    assert!(!tree_in_use(&db, tree).await.unwrap());
}
