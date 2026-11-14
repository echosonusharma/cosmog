//! Job runner: keeps at most W job transfers in flight, reacts to their outcomes, stops on request.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use super::policy::{classify, Class};
use super::runtime::{JobRt, Stop};
use super::{keys, resume, set_state_if, view, FolderCtx};
use crate::db::folder_upload::{ItemState, JobRow, JobState};
use crate::error::{AppError, AppResult};
use crate::store::ObjectStore;
use crate::transfer::{EncryptSpec, TransferEvent};

pub(super) enum Outcome {
    Finished,
    Paused(String, Option<String>),
    Canceled,
}

pub(super) struct Run<C: FolderCtx> {
    pub ctx: C,
    pub job: JobRow,
    pub rt: Arc<JobRt>,
    pub store: Arc<dyn ObjectStore>,
    pub tx: mpsc::UnboundedSender<(i64, TransferEvent)>,
    pub enc: Option<EncryptSpec>,
    /// Canonical source root; every source path must resolve inside it.
    pub canon_root: PathBuf,
    /// Items whose key holds a stale object this job wrote: replace it (after a HEAD) next time.
    pub replace_own: HashSet<i64>,
    pub stopping: Stop,
    pub net_wait: bool,
    /// Pause requested by an item failure: (code, wire error).
    pub pause: Option<(String, Option<String>)>,
    pub dirty: bool,
    counts_at: Instant,
}

const TICK: Duration = Duration::from_secs(1);

/// Window of job transfers: twice the transfer concurrency, at most 32.
pub(super) fn window(concurrency: u32) -> usize {
    (2 * concurrency as usize).clamp(1, 32)
}

pub(super) async fn run_job<C: FolderCtx>(ctx: C, job_id: String, mut stop: watch::Receiver<Stop>) {
    let rt = ctx.jobs().rt(&job_id);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let res = async {
        let job = ctx.db().fu_get_job(&job_id).await?;
        let store = ctx.store_for(&job.account_id).await?;
        let mut run = Run {
            ctx: ctx.clone(),
            job,
            rt: rt.clone(),
            store,
            tx,
            enc: None,
            canon_root: PathBuf::new(),
            replace_own: HashSet::new(),
            stopping: Stop::None,
            net_wait: false,
            pause: None,
            dirty: true,
            counts_at: Instant::now(),
        };
        let out = run.drive(&mut stop, &mut rx).await;
        Ok::<_, AppError>((run, out))
    }
    .await;
    rt.inflight.lock().unwrap().clear();
    *rt.waiting.lock().unwrap() = None;
    let outcome = match res {
        Ok((run, Ok(o))) => settle(&run, o).await,
        Ok((_, Err(e))) | Err(e) => Err(e),
    };
    if let Err(e) = outcome {
        // A deleted job (account removed) just ends; a running one pauses with the code (resumable).
        if ctx.db().fu_get_job(&job_id).await.is_ok() {
            tracing::warn!(job = %job_id, "folder upload runner failed: {e}");
            let _ = set_state_if(&ctx, &job_id, JobState::Paused, Some(e.code().into()), Some(e.to_wire_string())).await;
        }
    }
}

async fn settle<C: FolderCtx>(run: &Run<C>, outcome: Outcome) -> AppResult<()> {
    let (ctx, id) = (&run.ctx, run.job.id.as_str());
    match outcome {
        Outcome::Finished => {
            resume::purge_rows(ctx, &run.store, id).await?;
            let c = ctx.db().fu_counts(id).await?;
            let errors = c.failed + c.skipped_missing + c.canceled + c.excluded_late > 0;
            let state = if errors { JobState::CompletedWithErrors } else { JobState::Completed };
            set_state_if(ctx, id, state, None, None).await
        }
        Outcome::Paused(reason, err) => set_state_if(ctx, id, JobState::Paused, Some(reason), err).await,
        Outcome::Canceled => resume::cancel_cleanup(ctx, &run.store, id).await,
    }
}

impl<C: FolderCtx> Run<C> {
    async fn drive(
        &mut self,
        stop: &mut watch::Receiver<Stop>,
        rx: &mut mpsc::UnboundedReceiver<(i64, TransferEvent)>,
    ) -> AppResult<Outcome> {
        if let Some(o) = self.preflight().await? {
            return Ok(o);
        }
        resume::reconcile(self).await?;
        let timing = self.ctx.timing();
        let mut net_backoff = timing.net_backoff_min;
        loop {
            let req = stop.borrow().clone();
            if req != Stop::None && (self.stopping == Stop::None || req == Stop::Cancel) && self.stopping != req {
                self.begin_stop(req);
            } else if self.pause.is_some() && self.stopping == Stop::None {
                self.begin_stop(Stop::Pause);
            }
            let inflight = self.rt.inflight.lock().unwrap().len();
            if self.stopping != Stop::None {
                if inflight == 0 {
                    return Ok(match self.stopping {
                        Stop::Cancel => Outcome::Canceled,
                        _ => match self.pause.take() {
                            Some((code, wire)) => Outcome::Paused(code, wire),
                            None => Outcome::Paused("user".into(), None),
                        },
                    });
                }
            } else if self.net_wait {
                if inflight == 0 {
                    tokio::select! {
                        _ = tokio::time::sleep(net_backoff) => {}
                        _ = stop.changed() => continue,
                    }
                    match self.store.head_bucket(&self.job.bucket).await {
                        Ok(()) => {
                            self.net_wait = false;
                            net_backoff = timing.net_backoff_min;
                            self.set_waiting(None);
                        }
                        Err(e) => match classify(e.code()) {
                            Class::Network => net_backoff = (net_backoff * 2).min(timing.net_backoff_max),
                            Class::Pause => self.pause = Some((e.code().into(), Some(e.to_wire_string()))),
                            _ => {
                                self.net_wait = false;
                                self.set_waiting(None);
                            }
                        },
                    }
                    continue;
                }
            } else {
                self.refill().await?;
                if self.rt.inflight.lock().unwrap().is_empty() && !self.net_wait && self.pause.is_none() {
                    let c = self.ctx.db().fu_counts(&self.job.id).await?;
                    if c.pending == 0 && c.uploading == 0 {
                        return Ok(Outcome::Finished);
                    }
                }
            }
            tokio::select! {
                ev = rx.recv() => {
                    if let Some((seq, ev)) = ev {
                        self.on_event(seq, ev).await?;
                        while let Ok((seq, ev)) = rx.try_recv() {
                            self.on_event(seq, ev).await?;
                        }
                    }
                }
                r = stop.changed() => if r.is_err() { tokio::time::sleep(TICK).await; },
                _ = tokio::time::sleep(TICK) => {}
            }
            self.refresh_counts().await;
        }
    }

    /// Root reachable and encryption unchanged, else the job pauses before any work.
    async fn preflight(&mut self) -> AppResult<Option<Outcome>> {
        let root = std::path::Path::new(&self.job.source_root);
        let canon = tokio::fs::canonicalize(root).await.ok();
        // The stored root is canonical: resolving elsewhere now means a link in it was re-pointed.
        let same = canon.as_deref().is_some_and(|c| crate::transfer::source_guard::display_root(c) == root);
        let ok = same && matches!(tokio::fs::metadata(root).await, Ok(m) if m.is_dir()) && tokio::fs::read_dir(root).await.is_ok();
        if !ok {
            let e = AppError::FolderSourceUnavailable(keys::root_name(&self.job.source_root));
            return Ok(Some(Outcome::Paused(e.code().into(), Some(e.to_wire_string()))));
        }
        self.canon_root = canon.unwrap_or_default();
        if let Some(e) = self.check_encryption().await? {
            return Ok(Some(Outcome::Paused(e.code().into(), Some(e.to_wire_string()))));
        }
        Ok(None)
    }

    /// On/off change is an error; a rotated key just switches recipient.
    pub(super) async fn check_encryption(&mut self) -> AppResult<Option<AppError>> {
        let db = self.ctx.db().clone();
        let (account, bucket) = (self.job.account_id.clone(), self.job.bucket.clone());
        let cfg = db.get_encryption_config(&account, &bucket).await?;
        match (cfg.map(|c| c.recipient), self.job.enc_recipient.clone()) {
            (Some(_), None) | (None, Some(_)) => {
                return Ok(Some(AppError::FolderEncryptionChanged(bucket)));
            }
            (Some(cur), Some(old)) if cur != old => {
                db.fu_set_enc_recipient(&self.job.id, Some(cur.clone())).await?;
                self.job.enc_recipient = Some(cur);
            }
            _ => {}
        }
        self.enc = crate::transfer::encrypt::encrypt_spec_for_bucket(&db, self.ctx.db_path(), &account, &bucket).await?;
        Ok(None)
    }

    async fn refill(&mut self) -> AppResult<()> {
        let settings = self.ctx.db().settings_load().await?;
        let w = window(settings.transfer_concurrency);
        if self.rt.inflight.lock().unwrap().len() >= w {
            return Ok(());
        }
        if let Some(e) = self.check_encryption().await? {
            self.pause = Some((e.code().into(), Some(e.to_wire_string())));
            return Ok(());
        }
        loop {
            let free = w.saturating_sub(self.rt.inflight.lock().unwrap().len());
            if free == 0 || self.net_wait || self.pause.is_some() {
                return Ok(());
            }
            let now = chrono::Utc::now().timestamp();
            let batch = self.ctx.db().fu_next_batch(&self.job.id, free as i64, now).await?;
            if batch.is_empty() {
                return Ok(());
            }
            for item in batch {
                self.start(item).await?;
                if self.net_wait || self.pause.is_some() {
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn begin_stop(&mut self, req: Stop) {
        self.stopping = req.clone();
        let ids: Vec<String> = self.rt.inflight.lock().unwrap().values().map(|i| i.transfer_id.clone()).collect();
        for id in ids {
            match req {
                Stop::Pause => self.ctx.transfers().suspend(&id),
                _ => {
                    let _ = self.ctx.transfers().cancel(&id);
                }
            }
        }
    }

    pub(super) fn set_waiting(&self, w: Option<&'static str>) {
        let changed = {
            let mut cur = self.rt.waiting.lock().unwrap();
            let changed = *cur != w;
            *cur = w;
            changed
        };
        if changed {
            self.ctx.notify(&self.job.id, JobState::Running);
        }
    }

    async fn refresh_counts(&mut self) {
        if self.dirty && self.counts_at.elapsed() >= TICK {
            self.dirty = false;
            self.counts_at = Instant::now();
            let _ = view::counts(&self.ctx, &self.rt, &self.job.id, Some(Duration::ZERO)).await;
        }
    }

    pub(super) fn key(&self, rel: &str, is_dir: bool) -> String {
        keys::object_key(&self.job.dest_prefix, rel, is_dir)
    }

    /// Terminal item state other than an upload by this job (see `fu_item_done`).
    pub(super) async fn finish(&mut self, seq: i64, state: ItemState, error: Option<String>) -> AppResult<()> {
        self.dirty = true;
        self.ctx.db().fu_item_finished(&self.job.id, seq, state, error, None).await
    }
}
