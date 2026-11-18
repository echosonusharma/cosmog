//! An item's transfer ended: record the result durably, then let go of the row.

use super::engine::Run;
use super::own::{owner, Owner};
use super::policy::{classify, throttle_delay, wire_code, Class, IO_MAX_ATTEMPTS, NET_MAX_ATTEMPTS, THROTTLE_MAX_ATTEMPTS};
use super::runtime::{Inflight, Stop};
use super::walk::mtime_ms;
use super::source::{same_version, StatRule};
use super::FolderCtx;
use crate::db::folder_upload::{Drift, ItemState};
use crate::db::transfers::{Transfer, TransferStatus};
use crate::error::{AppError, AppResult};
use crate::transfer::source_guard::{file_id, Inspect};
use crate::transfer::{ProgressSink, TransferEvent};

impl<C: FolderCtx> Run<C> {
    /// Progress goes straight to the runtime; terminals go to the runner.
    pub(super) fn sink(&self, seq: i64) -> ProgressSink {
        let (rt, tx, ctx, job_id) = (self.rt.clone(), self.tx.clone(), self.ctx.clone(), self.job.id.clone());
        ProgressSink::from_fn(move |ev| {
            ctx.on_transfer_event(&job_id, &ev);
            match &ev {
                TransferEvent::Started { bytes_total, .. } => {
                    if let Some(i) = rt.inflight.lock().unwrap().get_mut(&seq) {
                        i.bytes_done = 0;
                        i.bytes_total = bytes_total.or(i.bytes_total);
                    }
                }
                TransferEvent::Progress { bytes_done, bytes_total, .. } => {
                    if let Some(i) = rt.inflight.lock().unwrap().get_mut(&seq) {
                        i.bytes_done = *bytes_done;
                        i.bytes_total = bytes_total.or(i.bytes_total);
                    }
                }
                TransferEvent::PartCompleted { .. } => {
                    if let Some(i) = rt.inflight.lock().unwrap().get_mut(&seq) {
                        i.progressed = true;
                    }
                }
                TransferEvent::Done { .. } | TransferEvent::Failed { .. } | TransferEvent::Canceled { .. } => {
                    let _ = tx.send((seq, ev));
                }
                _ => {}
            }
        })
    }

    pub(super) async fn on_event(&mut self, seq: i64, ev: TransferEvent) -> AppResult<()> {
        let tid = match &ev {
            TransferEvent::Done { transfer_id, .. } | TransferEvent::Failed { transfer_id, .. } | TransferEvent::Canceled { transfer_id } => transfer_id.clone(),
            _ => return Ok(()),
        };
        let inf = {
            let mut map = self.rt.inflight.lock().unwrap();
            match map.get(&seq) {
                Some(i) if i.transfer_id == tid => map.remove(&seq),
                _ => None,
            }
        };
        let Some(inf) = inf else { return Ok(()) };
        self.dirty = true;
        match ev {
            TransferEvent::Done { etag, .. } => {
                if *self.rt.waiting.lock().unwrap() == Some("rate_limited") {
                    self.set_waiting(None);
                }
                // The item is durable before its row goes, so a crash in between never re-uploads it.
                self.post_drift(seq, &inf, etag).await?;
                self.ctx.db().delete_transfer_rows(vec![tid]).await?;
                Ok(())
            }
            TransferEvent::Failed { error, .. } => {
                let row = self.ctx.db().get_transfer(&tid).await.ok();
                // A network loss leaves the row paused with its upload: the item keeps pointing at it.
                let parked = row.as_ref().filter(|r| r.status == TransferStatus::Paused && r.upload_id.is_some()).map(|r| r.id.clone());
                if parked.is_none() {
                    self.drop_rows(row.into_iter().collect()).await?;
                }
                let code = wire_code(&error);
                self.on_failure(seq, inf.attempts, &code, error, Some(&inf), parked).await
            }
            _ => {
                // A request already sent may have landed before the stop reached it: the bucket decides.
                let item = self.ctx.db().fu_get_item(&self.job.id, seq).await?;
                if let Some(etag) = super::resume::landed(&self.store, &self.job, &item).await {
                    match self.stopping {
                        // Pause: done (or one re-upload if it changed), never left to be sent again.
                        Stop::Pause => self.post_drift(seq, &inf, etag).await?,
                        _ => {
                            self.dirty = true;
                            self.ctx.db().fu_item_done(&self.job.id, seq, etag).await?;
                        }
                    }
                    let row = self.ctx.db().get_transfer(&tid).await.ok();
                    return self.drop_rows(row.into_iter().collect()).await;
                }
                // Pause keeps the paused row and its parts; anything else ends the item.
                if self.stopping == Stop::Pause {
                    return Ok(());
                }
                self.finish(seq, ItemState::Canceled, None).await?;
                let row = self.ctx.db().get_transfer(&tid).await.ok();
                self.drop_rows(row.into_iter().collect()).await
            }
        }
    }

    /// After upload: changed while uploading means one re-upload, then `source_changed`;
    /// no longer a regular file inside the folder is a failure.
    pub(super) async fn post_drift(&mut self, seq: i64, inf: &Inflight, etag: Option<String>) -> AppResult<()> {
        let rule = super::source::stat_rule(&self.job);
        let changed = match self.inspect_src(&inf.source).await {
            Inspect::File(m) => {
                let (size, mtime) = (m.len() as i64, mtime_ms(&m));
                // Another inode (replaced file) counts as a change even with an equal stat; SAF fds have no stable one.
                let replaced = rule == StatRule::Full && file_id(&m) != inf.file_id;
                let changed = replaced || !same_version(rule, (inf.size, inf.mtime_ms), (size, mtime));
                changed.then_some((size, mtime))
            }
            // Deleted after a complete upload: what was uploaded was the file.
            Inspect::Missing | Inspect::Error(_) => None,
            Inspect::Link | Inspect::Special | Inspect::Outside => {
                let e = AppError::SourceChanged(format!("{}: no longer a regular file inside the folder", inf.rel_path));
                return self.finish(seq, ItemState::Failed, Some(e.to_wire_string())).await;
            }
        };
        match changed {
            None => {
                self.dirty = true;
                self.ctx.db().fu_item_done(&self.job.id, seq, etag).await
            }
            Some((size, mtime)) => self.changed_during_upload(seq, inf, size, mtime).await,
        }
    }

    async fn changed_during_upload(&mut self, seq: i64, inf: &Inflight, size: i64, mtime: i64) -> AppResult<()> {
        if inf.drift == Some(Drift::ChangedDuringUpload) {
            let e = AppError::SourceChanged(inf.rel_path.clone());
            return self.finish(seq, ItemState::Failed, Some(e.to_wire_string())).await;
        }
        let db = self.ctx.db();
        db.fu_item_drift(&self.job.id, seq, size, mtime, Drift::ChangedDuringUpload).await?;
        db.fu_item_retry_later(&self.job.id, seq, 0, None, false).await
    }

    /// 412 on a conditional write: a foreign key appeared, or it is our own earlier write.
    async fn on_conflict(&mut self, seq: i64, inf: &Inflight, wire: String) -> AppResult<()> {
        let item = self.ctx.db().fu_get_item(&self.job.id, seq).await?;
        let key = self.key(&item.rel_path, false);
        match self.store.head_object(&self.job.bucket, &key).await {
            // A write of this exact version whose response was lost: it is uploaded.
            Ok(meta) if owner(&meta, &self.job.id, &item) == (Owner::Ours { current: true }) => {
                self.post_drift(seq, inf, meta.etag).await
            }
            Ok(meta) if owner(&meta, &self.job.id, &item) == (Owner::Ours { current: false }) => {
                self.replace_own.insert(seq);
                self.ctx.db().fu_item_retry_later(&self.job.id, seq, 0, None, false).await
            }
            Ok(_) => self.finish(seq, ItemState::SkippedExists, None).await,
            // Gone again: try once more, counted so it cannot loop.
            Err(e) if e.is_not_found() => self.retry_counted(seq, inf.attempts, wire).await,
            // Boxed: on_failure can lead back here.
            Err(e) => Box::pin(self.on_failure(seq, inf.attempts, e.code(), e.to_wire_string(), None, None)).await,
        }
    }

    async fn retry_counted(&mut self, seq: i64, attempts: i64, wire: String) -> AppResult<()> {
        if attempts + 1 >= THROTTLE_MAX_ATTEMPTS {
            return self.finish(seq, ItemState::Failed, Some(wire)).await;
        }
        self.ctx.db().fu_item_retry_later(&self.job.id, seq, 0, Some(wire), true).await
    }

    /// `inf` is the item's ended transfer (None for failures before enqueue); `parked` its kept row.
    pub(super) async fn on_failure(
        &mut self,
        seq: i64,
        attempts: i64,
        code: &str,
        wire: String,
        inf: Option<&Inflight>,
        parked: Option<String>,
    ) -> AppResult<()> {
        self.dirty = true;
        let t = self.ctx.timing();
        let now = chrono::Utc::now().timestamp();
        let db = self.ctx.db().clone();
        let id = self.job.id.clone();
        let later = |at: i64, count: bool, wire: String| {
            let (db, id) = (db.clone(), id.clone());
            async move { db.fu_item_retry_later(&id, seq, at, Some(wire), count).await }
        };
        let inm = inf.is_some_and(|i| i.if_none_match);
        match classify(code) {
            Class::Network => {
                self.net_wait = true;
                self.set_waiting(Some("network"));
                // Online again (the probe passed) yet no progress: count it, so it cannot loop silently.
                let count = inf.is_some_and(|i| i.net_retry && !i.progressed);
                if count && attempts + 1 >= NET_MAX_ATTEMPTS {
                    if let Some(tid) = parked {
                        let rows = self.ctx.db().get_transfer(&tid).await.ok();
                        self.drop_rows(rows.into_iter().collect()).await?;
                    }
                    return self.finish(seq, ItemState::Failed, Some(wire)).await;
                }
                match parked {
                    Some(tid) => db.fu_item_park(&id, seq, &tid, wire, count).await,
                    None => later(0, count, wire).await,
                }
            }
            Class::Requeue => later(0, false, wire).await,
            Class::Pause => {
                self.pause = Some((code.to_string(), Some(wire.clone())));
                later(0, false, wire).await
            }
            Class::Conflict if inm => self.on_conflict(seq, inf.expect("inm implies a transfer"), wire).await,
            // The guarded open refused a replaced file: treat like a change during upload.
            Class::Fatal if code == "source_changed" && inf.is_some_and(|i| i.drift != Some(Drift::ChangedDuringUpload)) => {
                let i = inf.expect("checked above");
                self.changed_during_upload(seq, i, i.size, i.mtime_ms).await
            }
            Class::Throttle | Class::Conflict => {
                if code == "rate_limited" {
                    self.set_waiting(Some("rate_limited"));
                }
                if attempts + 1 >= THROTTLE_MAX_ATTEMPTS {
                    self.finish(seq, ItemState::Failed, Some(wire)).await
                } else {
                    later(now + throttle_delay(t.retry_base, t.retry_cap, attempts), true, wire).await
                }
            }
            Class::Io if attempts + 1 < IO_MAX_ATTEMPTS => later(now + t.io_backoff, true, wire).await,
            _ => self.finish(seq, ItemState::Failed, Some(wire)).await,
        }
    }

    /// Deletes rows, aborting their multipart upload first; a row whose abort failed is kept.
    pub(super) async fn drop_rows(&self, rows: Vec<Transfer>) -> AppResult<()> {
        super::resume::abort_and_delete(&self.ctx, &self.store, rows).await
    }
}
