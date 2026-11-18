//! Blocking tree walk: never follows links, never opens special files, streams item batches.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::analysis::Stats;
use super::keys::{check_name, join_rel, key_too_long, windows_unsafe};
use super::{MAX_ENTRIES, MAX_FILE_BYTES};
use crate::db::cache::KeyParts;
use crate::db::folder_upload::{ExcludeReason, ItemKind, ItemState, NewItem};
use crate::error::{AppError, AppResult};

pub(super) enum WalkMsg {
    Items(Vec<NewItem>),
    Progress { files: i64, dirs: i64, bytes: i64, current_dir: String },
}

const BATCH: usize = 1000;
pub(super) const PROGRESS_EVERY: Duration = Duration::from_millis(250);

struct Frame {
    abs: PathBuf,
    rel: String,
    /// Topmost hidden ancestor dir, if any.
    hidden_dir: Option<String>,
}

/// Millisecond mtime, 0 when unknown.
pub(super) fn mtime_ms(m: &Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn os_hidden(m: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn os_hidden(_m: &Metadata) -> bool {
    false
}

pub(super) struct Walker<'a> {
    dest: &'a str,
    tx: &'a mpsc::Sender<WalkMsg>,
    pub stats: Stats,
    seq: i64,
    batch: Vec<NewItem>,
    /// Taken by the next `push` (SAF document URI of that item).
    pub source_ref: Option<String>,
}

impl<'a> Walker<'a> {
    pub(super) fn new(dest: &'a str, tx: &'a mpsc::Sender<WalkMsg>) -> Self {
        Self { dest, tx, stats: Stats::default(), seq: 0, batch: Vec::with_capacity(BATCH), source_ref: None }
    }

    pub(super) fn send(&self, msg: WalkMsg) -> AppResult<()> {
        self.tx.blocking_send(msg).map_err(|_| AppError::Canceled("analysis".into()))
    }

    pub(super) fn push(&mut self, kind: ItemKind, rel: String, size: i64, mtime: i64, hidden: bool, link: Option<String>, excl: Option<ExcludeReason>) -> AppResult<()> {
        // key_too_long is only decided for uploadable items; fu_set_dest re-evaluates it later.
        let excl = excl.or_else(|| key_too_long(self.dest, &rel, kind == ItemKind::Dir).then_some(ExcludeReason::KeyTooLong));
        if let Some(r) = excl {
            self.stats.add_excluded(r);
        }
        let ext = (kind == ItemKind::File).then(|| KeyParts::from_key(&rel).extension).flatten();
        self.seq += 1;
        self.batch.push(NewItem {
            seq: self.seq,
            kind,
            rel_path: rel,
            source_ref: self.source_ref.take(),
            size,
            mtime_ms: mtime,
            ext,
            hidden,
            link_target: link,
            state: if excl.is_some() { ItemState::Excluded } else { ItemState::Pending },
            exclude_reason: excl,
        });
        if self.batch.len() >= BATCH {
            self.flush()?;
        }
        Ok(())
    }

    pub(super) fn flush(&mut self) -> AppResult<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let items = std::mem::take(&mut self.batch);
        self.tx.blocking_send(WalkMsg::Items(items)).map_err(|_| AppError::Canceled("analysis".into()))
    }
}

pub(super) fn walk(root: &Path, dest: &str, cancel: &CancellationToken, tx: &mpsc::Sender<WalkMsg>) -> AppResult<Stats> {
    let mut w = Walker::new(dest, tx);
    let mut probe = Probe::new();
    let mut stack = vec![Frame { abs: root.to_path_buf(), rel: String::new(), hidden_dir: None }];
    let mut entries: u64 = 0;
    let mut last_progress = Instant::now();
    while let Some(f) = stack.pop() {
        let rd = match std::fs::read_dir(&f.abs) {
            Ok(rd) => rd,
            Err(e) if f.rel.is_empty() => {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                return Err(AppError::FolderSourceUnavailable(format!("{name}: {e}")));
            }
            Err(_) => {
                w.push(ItemKind::Dir, f.rel, 0, 0, f.hidden_dir.is_some(), None, Some(ExcludeReason::UnreadableDir))?;
                continue;
            }
        };
        let mut children = 0u64;
        // Children that will produce an object (files to upload, folders walked).
        let mut uploadable = 0u64;
        let mut entry_error = false;
        let mut subdirs = Vec::new();
        for ent in rd {
            if cancel.is_cancelled() {
                return Err(AppError::Canceled("analysis".into()));
            }
            let Ok(ent) = ent else {
                // Nameless entry error: the folder is reported as not fully readable (once).
                if !entry_error {
                    entry_error = true;
                    w.push(ItemKind::Dir, f.rel.clone(), 0, 0, f.hidden_dir.is_some(), None, Some(ExcludeReason::UnreadableDir))?;
                }
                continue;
            };
            children += 1;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(AppError::FolderTooManyFiles(format!("more than {MAX_ENTRIES} entries")));
            }
            let (name, bad) = match check_name(&ent.file_name()) {
                Ok(n) => (n.to_string(), None),
                Err((r, shown)) => (shown, Some(r)),
            };
            let rel = join_rel(&f.rel, &name);
            let path = ent.path();
            let lmeta = std::fs::symlink_metadata(&path);
            let hidden_self = name.starts_with('.') || lmeta.as_ref().is_ok_and(os_hidden);
            let hidden = f.hidden_dir.is_some() || hidden_self;
            if windows_unsafe(&name) {
                w.stats.windows_unsafe_names += 1;
            }
            let Ok(ft) = ent.file_type() else {
                w.push(ItemKind::Special, rel, 0, 0, hidden, None, Some(ExcludeReason::Unreadable))?;
                continue;
            };
            if ft.is_symlink() {
                let target = std::fs::read_link(&path).ok().map(|t| t.to_string_lossy().into_owned());
                w.push(ItemKind::Symlink, rel, 0, 0, hidden, target, Some(ExcludeReason::Symlink))?;
            } else if ft.is_dir() {
                w.stats.add_dir(&rel);
                if let Some(r) = bad {
                    w.push(ItemKind::Dir, rel, 0, 0, hidden, None, Some(r))?;
                } else {
                    let hidden_dir = f.hidden_dir.clone().or_else(|| hidden_self.then(|| rel.clone()));
                    uploadable += 1;
                    subdirs.push(Frame { abs: path, rel, hidden_dir });
                }
            } else if ft.is_file() {
                let meta = match lmeta {
                    Ok(m) => m,
                    Err(_) => {
                        w.push(ItemKind::File, rel, 0, 0, hidden, None, Some(ExcludeReason::Unreadable))?;
                        continue;
                    }
                };
                let (size, mtime) = (meta.len() as i64, mtime_ms(&meta));
                let excl = bad
                    .or_else(|| (meta.len() > MAX_FILE_BYTES).then_some(ExcludeReason::FileTooLarge))
                    // Read-only open without reading: unreadable or locked files show up now.
                    .or_else(|| (!probe.readable(&path)).then_some(ExcludeReason::Unreadable));
                if excl.is_none() {
                    uploadable += 1;
                    let ext = KeyParts::from_key(&rel).extension;
                    w.stats.add_file(&rel, size, ext.as_deref(), f.hidden_dir.as_deref(), hidden);
                }
                w.push(ItemKind::File, rel, size, mtime, hidden, None, excl)?;
            } else {
                w.push(ItemKind::Special, rel, 0, 0, hidden, None, Some(ExcludeReason::Special))?;
            }
        }
        if children == 0 && !f.rel.is_empty() {
            w.stats.empty_dirs += 1;
        }
        // Empty, or only excluded children: a marker keeps the folder in the bucket.
        if uploadable == 0 && !f.rel.is_empty() && !entry_error {
            w.push(ItemKind::Dir, f.rel.clone(), 0, 0, f.hidden_dir.is_some(), None, None)?;
        }
        stack.extend(subdirs.into_iter().rev());
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            let p = WalkMsg::Progress { files: w.stats.files, dirs: w.stats.dirs, bytes: w.stats.total_bytes, current_dir: f.rel };
            tx.blocking_send(p).map_err(|_| AppError::Canceled("analysis".into()))?;
        }
    }
    w.flush()?;
    Ok(w.stats)
}

/// Read-open probe on a helper thread: a FIFO swapped in after the type check would block
/// `open` forever, so a probe that does not answer in time counts as unreadable.
struct Probe {
    tx: std::sync::mpsc::Sender<PathBuf>,
    rx: std::sync::mpsc::Receiver<bool>,
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

impl Probe {
    fn new() -> Self {
        let (tx, jobs) = std::sync::mpsc::channel::<PathBuf>();
        let (done, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for p in jobs {
                if done.send(std::fs::File::open(&p).is_ok()).is_err() {
                    return;
                }
            }
        });
        Self { tx, rx }
    }

    fn readable(&mut self, path: &Path) -> bool {
        if self.tx.send(path.to_path_buf()).is_err() {
            *self = Self::new();
            return false;
        }
        match self.rx.recv_timeout(PROBE_TIMEOUT) {
            Ok(ok) => ok,
            // Stuck on a blocking open: abandon that thread, start a fresh one.
            Err(_) => {
                *self = Self::new();
                false
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn probe_never_blocks_on_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let ok = std::process::Command::new("mkfifo").arg(&fifo).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            eprintln!("skipping: mkfifo unavailable");
            return;
        }
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        let mut p = Probe::new();
        let t = Instant::now();
        assert!(!p.readable(&fifo), "a FIFO open never answers: unreadable");
        assert!(t.elapsed() < PROBE_TIMEOUT + Duration::from_secs(1));
        // A fresh helper takes over after the stuck one.
        assert!(p.readable(&file));
    }
}
