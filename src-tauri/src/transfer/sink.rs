use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::db::Db;

use super::{CompletedPart, ProgressSink, ResumeState, TransferEvent};

pub(super) fn composite_sink(
    db: Db,
    transfer_id: String,
    external: ProgressSink,
    seed: Option<ResumeState>,
) -> (
    ProgressSink,
    Arc<Mutex<ResumeState>>,
    Arc<PartsJournal>,
) {
    // Seeded with carried-over parts so journal flushes never drop them.
    let resume: Arc<Mutex<ResumeState>> = Arc::new(Mutex::new(seed.unwrap_or_default()));
    let resume_ret = resume.clone();
    let journal = Arc::new(PartsJournal {
        db: db.clone(),
        resume: resume.clone(),
        db_lock: Arc::new(AsyncMutex::new(())),
        closed: AtomicBool::new(false),
    });
    let journal_for_sink = journal.clone();

    let sink = ProgressSink::from_fn(move |event: TransferEvent| {
        // Store terminals are premature (retry pending, decrypt not done); the worker emits the real one.
        if matches!(
            event,
            TransferEvent::Done { .. }
                | TransferEvent::Failed { .. }
                | TransferEvent::Canceled { .. }
        ) {
            return;
        }
        external.emit(event.clone());

        let db = db.clone();
        let tid = transfer_id.clone();

        match event {
            TransferEvent::Started { bytes_total, .. } => {
                tokio::spawn(async move {
                    let _ = db
                        .update_transfer_bytes(&tid, 0, bytes_total.map(|n| n as i64))
                        .await;
                });
            }
            TransferEvent::Progress {
                bytes_done,
                bytes_total,
                ..
            } => {
                tokio::spawn(async move {
                    let _ = db
                        .update_transfer_bytes(
                            &tid,
                            bytes_done as i64,
                            bytes_total.map(|n| n as i64),
                        )
                        .await;
                });
            }
            TransferEvent::MultipartInitiated { upload_id, part_size, .. } => {
                {
                    let mut guard = resume.lock().unwrap();
                    // A different id means the saved state was discarded; its parts are void.
                    if guard.upload_id != upload_id {
                        guard.completed_parts.clear();
                    }
                    guard.upload_id = upload_id;
                    guard.part_size = Some(part_size);
                }
                // Persist now so a crash before the first part still leaves an upload_id to abort.
                let journal = journal_for_sink.clone();
                tokio::spawn(async move {
                    journal.flush(&tid).await;
                });
            }
            TransferEvent::PartCompleted {
                upload_id, part_number, etag, ..
            } => {
                let persist_now = {
                    let mut guard = resume.lock().unwrap();
                    if guard.upload_id.is_empty() {
                        guard.upload_id = upload_id;
                    }
                    guard.completed_parts.push(CompletedPart { part_number, etag });
                    // O(log n) writes; the worker flushes the tail before each retry.
                    guard.completed_parts.len().is_power_of_two()
                };
                if persist_now {
                    let journal = journal_for_sink.clone();
                    tokio::spawn(async move {
                        journal.flush(&tid).await;
                    });
                }
            }
            _ => {}
        }
    });
    (sink, resume_ret, journal)
}

/// Resume snapshot plus the lock serializing its writes to transfers.parts_json.
pub(super) struct PartsJournal {
    db: Db,
    resume: Arc<Mutex<ResumeState>>,
    db_lock: Arc<AsyncMutex<()>>,
    /// Set at terminal; late spawned flushes must not resurrect a cleared upload_id.
    closed: AtomicBool,
}

impl PartsJournal {
    /// Re-snapshots under `db_lock` so an older queued write can never clobber a newer one.
    pub(super) async fn flush(&self, transfer_id: &str) {
        let _guard = self.db_lock.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let snapshot = self.resume.lock().unwrap().clone();
        if snapshot.upload_id.is_empty() {
            return;
        }
        let uid = Some(snapshot.upload_id.clone());
        let _ = self
            .db
            .update_transfer_multipart(transfer_id, uid, &snapshot)
            .await;
    }

    pub(super) async fn close_and_clear(&self, transfer_id: &str) {
        let _guard = self.db_lock.lock().await;
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.db.clear_transfer_multipart(transfer_id).await;
    }
}
