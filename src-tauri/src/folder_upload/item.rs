//! One item before upload: drift and source checks, the overwrite guard, enqueue or resume.

use super::engine::Run;
use super::own::{owner, tag, Owner, TAG_KEY};
use super::policy::{classify, wire_code, Class};
use super::runtime::Inflight;
use super::walk::mtime_ms;
use super::{keys, FolderCtx, MAX_FILE_BYTES};
use crate::db::folder_upload::{Drift, ExcludeReason, ItemKind, ItemRow, ItemState, OverwritePolicy};
use crate::db::transfers::{Transfer, TransferStatus};
use crate::error::AppResult;
use crate::store::PutOptions;
use crate::transfer::source_guard::{file_id, inspect, FileId, Inspect};
use crate::transfer::SourceGuard;

/// Current local version of an item, as checked just before enqueue.
pub(super) struct Local {
    pub size: i64,
    pub mtime_ms: i64,
    pub drift: Option<Drift>,
    pub id: FileId,
    /// Differs from the recorded stat (the recorded stat is now updated).
    pub changed: bool,
}

impl<C: FolderCtx> Run<C> {
    pub(super) async fn start(&mut self, item: ItemRow) -> AppResult<()> {
        match item.kind {
            ItemKind::Dir => self.start_dir(item).await,
            ItemKind::File => self.start_file(item).await,
            _ => self.exclude(item.seq, ExcludeReason::Special).await,
        }
    }

    /// Became un-uploadable after approval; counted as an error.
    pub(super) async fn exclude(&mut self, seq: i64, reason: ExcludeReason) -> AppResult<()> {
        self.dirty = true;
        self.ctx.db().fu_item_exclude(&self.job.id, seq, reason).await
    }

    async fn start_dir(&mut self, item: ItemRow) -> AppResult<()> {
        // No marker wanted: nothing to upload, the item is trivially done.
        if !self.job.create_dir_markers {
            return self.finish(item.seq, ItemState::Done, None).await;
        }
        let key = self.key(&item.rel_path, true);
        // An existing object at `dir/` is never replaced; either way the folder is there.
        match self.store.create_folder_if_absent(&self.job.bucket, &key).await {
            Ok(_) => self.finish(item.seq, ItemState::Done, None).await,
            Err(e) => self.on_failure(item.seq, item.attempts, e.code(), e.to_wire_string(), None, None).await,
        }
    }

    pub(super) fn abs(&self, rel: &str) -> std::path::PathBuf {
        keys::abs_path(std::path::Path::new(&self.job.source_root), rel)
    }

    /// Source check before upload: gone, now a link/special/outside the folder, or changed.
    pub(super) async fn pre_drift(&mut self, item: &ItemRow) -> AppResult<Option<Local>> {
        let m = match inspect(&self.abs(&item.rel_path), &self.canon_root).await {
            Inspect::Missing => {
                self.finish(item.seq, ItemState::SkippedMissing, None).await?;
                return Ok(None);
            }
            Inspect::Link | Inspect::Outside => {
                self.exclude(item.seq, ExcludeReason::Symlink).await?;
                return Ok(None);
            }
            Inspect::Special => {
                self.exclude(item.seq, ExcludeReason::Special).await?;
                return Ok(None);
            }
            Inspect::Error(e) => {
                self.on_failure(item.seq, item.attempts, e.code(), e.to_wire_string(), None, None).await?;
                return Ok(None);
            }
            Inspect::File(m) if m.len() > MAX_FILE_BYTES => {
                self.exclude(item.seq, ExcludeReason::FileTooLarge).await?;
                return Ok(None);
            }
            Inspect::File(m) => m,
        };
        let (size, mtime) = (m.len() as i64, mtime_ms(&m));
        let mut drift = item.drift;
        let changed = size != item.size || mtime != item.mtime_ms;
        if changed {
            let d = if item.drift == Some(Drift::ChangedDuringUpload) { Drift::ChangedDuringUpload } else { Drift::Changed };
            self.ctx.db().fu_item_drift(&self.job.id, item.seq, size, mtime, d).await?;
            drift = Some(d);
        }
        Ok(Some(Local { size, mtime_ms: mtime, drift, id: file_id(&m), changed }))
    }

    async fn start_file(&mut self, item: ItemRow) -> AppResult<()> {
        let Some(local) = self.pre_drift(&item).await? else {
            return Ok(());
        };
        // Parked by a network loss: continue its multipart upload unless the file changed.
        if let Some(row) = self.parked_row(&item).await? {
            if !local.changed && row.upload_id.is_some() {
                return self.adopt(&item, row, local).await;
            }
            self.drop_rows(vec![row]).await?;
        }
        let key = self.key(&item.rel_path, false);
        // Only Overwrite may replace an object this job did not write; no policy behaves as Skip.
        let guarded = self.job.overwrite_policy != Some(OverwritePolicy::Overwrite);
        let mut inm = guarded;
        if guarded {
            if item.remote_exists {
                return self.finish(item.seq, ItemState::SkippedExists, None).await;
            }
            // Our own earlier write (re-upload after a change, or a stale version): replace it only if still ours.
            let own = item.drift == Some(Drift::ChangedDuringUpload) || self.replace_own.contains(&item.seq);
            if own {
                match self.store.head_object(&self.job.bucket, &key).await {
                    Ok(meta) if owner(&meta, &self.job.id, &item) == Owner::Foreign => {
                        return self.finish(item.seq, ItemState::SkippedExists, None).await;
                    }
                    Ok(_) => inm = false,
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return self.on_failure(item.seq, item.attempts, e.code(), e.to_wire_string(), None, None).await,
                }
            }
        }
        self.replace_own.remove(&item.seq);
        let mut opts = PutOptions::default();
        if inm {
            opts.if_none_match = Some("*".into());
        }
        opts.user_metadata.insert(TAG_KEY.into(), tag(&self.job.id, item.seq, local.size, local.mtime_ms));
        opts.source_guard = Some(SourceGuard { root: self.canon_root.clone(), id: local.id });
        if let Some(spec) = &self.enc {
            crate::transfer::encrypt::stamp(&mut opts, spec);
        }
        let enq = self
            .ctx
            .transfers()
            .enqueue_upload_for_job(
                self.store.clone(),
                self.job.account_id.clone(),
                self.job.bucket.clone(),
                key,
                self.abs(&item.rel_path),
                opts,
                self.sink(item.seq),
                self.job.id.clone(),
            )
            .await;
        match enq {
            Ok(tid) => self.track(&item, tid, &local, inm).await,
            Err(e) => self.on_failure(item.seq, item.attempts, e.code(), e.to_wire_string(), None, None).await,
        }
    }

    /// The paused row a network loss left this item pointing at, if it still exists.
    async fn parked_row(&self, item: &ItemRow) -> AppResult<Option<Transfer>> {
        let Some(tid) = &item.transfer_id else { return Ok(None) };
        match self.ctx.db().get_transfer(tid).await {
            Ok(r) => Ok(Some(r)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Continues a kept row (same upload_id, same options) through the manager's retry.
    pub(super) async fn adopt(&mut self, item: &ItemRow, row: Transfer, local: Local) -> AppResult<()> {
        let inm = row
            .options_json
            .as_deref()
            .and_then(|o| serde_json::from_str::<PutOptions>(o).ok())
            .is_some_and(|o| o.if_none_match.is_some());
        let retriable = matches!(row.status, TransferStatus::Paused | TransferStatus::Failed | TransferStatus::Canceled);
        let res = if retriable {
            self.ctx.transfers().retry(self.store.clone(), &row.id, self.sink(item.seq)).await
        } else {
            Err(crate::error::AppError::InvalidInput("row not retriable".into()))
        };
        match res {
            Ok(tid) => {
                // The new row owns the upload now; the old one holds nothing.
                self.ctx.db().delete_transfer_rows(vec![row.id.clone()]).await?;
                let net = row.error.as_deref().is_some_and(|e| classify(&wire_code(e)) == Class::Network);
                self.track(item, tid, &local, inm).await?;
                if let Some(i) = self.rt.inflight.lock().unwrap().get_mut(&item.seq) {
                    i.net_retry = net;
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(job = %self.job.id, "resume of a kept upload failed, restarting the item: {e}");
                self.drop_rows(vec![row]).await?;
                self.ctx.db().fu_item_retry_later(&self.job.id, item.seq, 0, None, false).await
            }
        }
    }

    pub(super) async fn track(&mut self, item: &ItemRow, tid: String, local: &Local, inm: bool) -> AppResult<()> {
        self.dirty = true;
        self.ctx.db().fu_item_started(&self.job.id, item.seq, &tid).await?;
        // Restarted after a network wait (its last error was network-class).
        let net_retry = item.error.as_deref().is_some_and(|e| classify(&wire_code(e)) == Class::Network);
        let inf = Inflight {
            transfer_id: tid,
            rel_path: item.rel_path.clone(),
            bytes_done: 0,
            bytes_total: Some(local.size as u64),
            size: local.size,
            mtime_ms: local.mtime_ms,
            drift: local.drift,
            if_none_match: inm,
            attempts: item.attempts,
            file_id: local.id,
            net_retry,
            progressed: false,
        };
        self.rt.inflight.lock().unwrap().insert(item.seq, inf);
        Ok(())
    }
}
