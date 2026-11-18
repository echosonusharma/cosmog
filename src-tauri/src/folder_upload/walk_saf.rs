//! SAF tree walk for folder upload: same items, stats and exclusion rules as `walk`, fed by
//! the streaming `saf::walk` lister. Files carry their document URI as `source_ref`.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::analysis::Stats;
use super::keys::{check_name, join_rel, root_name, windows_unsafe};
use super::walk::{WalkMsg, Walker, PROGRESS_EVERY};
use super::{MAX_ENTRIES, MAX_FILE_BYTES};
use crate::db::cache::KeyParts;
use crate::db::folder_upload::{ExcludeReason, ItemKind};
use crate::error::{AppError, AppResult};
use crate::saf::uri::document_uri;
use crate::saf::walk::Listing;

#[derive(Debug, Clone, Default)]
pub(super) struct Dir {
    rel: String,
    /// Topmost hidden ancestor dir, if any.
    hidden_dir: Option<String>,
}

/// (size, mtime ms) of a document whose listing gave no size; None when it cannot be opened.
pub(super) type SizeFallback<'a> = dyn FnMut(&str) -> Option<(i64, i64)> + 'a;

pub(super) struct SafWalker<'a> {
    pub w: Walker<'a>,
    tree: &'a str,
    cancel: &'a CancellationToken,
    entries: u64,
    last_progress: Instant,
    fallback: &'a mut SizeFallback<'a>,
}

/// Names that cannot become one key segment.
fn invalid_name(name: &str) -> bool {
    name.is_empty() || name == "." || name == ".." || name.contains('/')
}

impl<'a> SafWalker<'a> {
    pub(super) fn new(
        dest: &'a str,
        tree: &'a str,
        cancel: &'a CancellationToken,
        tx: &'a mpsc::Sender<WalkMsg>,
        fallback: &'a mut SizeFallback<'a>,
    ) -> Self {
        Self { w: Walker::new(dest, tx), tree, cancel, entries: 0, last_progress: Instant::now(), fallback }
    }

    /// One listed directory; returns the subdirs to descend.
    pub(super) fn visit(&mut self, f: &Dir, l: Listing) -> AppResult<Vec<(String, Dir)>> {
        if self.cancel.is_cancelled() {
            return Err(AppError::Canceled("analysis".into()));
        }
        let is_root = f.rel.is_empty();
        let hidden_parent = f.hidden_dir.is_some();
        if !l.complete && l.rows.is_empty() {
            if is_root {
                return Err(AppError::FolderSourceUnavailable(root_name(self.tree)));
            }
            self.w.push(ItemKind::Dir, f.rel.clone(), 0, 0, hidden_parent, None, Some(ExcludeReason::UnreadableDir))?;
            return Ok(Vec::new());
        }
        // Listing broke off partway: the folder is reported as not fully readable.
        let entry_error = !l.complete;
        if entry_error {
            self.w.push(ItemKind::Dir, f.rel.clone(), 0, 0, hidden_parent, None, Some(ExcludeReason::UnreadableDir))?;
        }
        let children = l.rows.len();
        let mut uploadable = 0u64;
        let mut names = HashSet::new();
        let mut subdirs = Vec::new();
        for r in l.rows {
            self.progress(&f.rel)?;
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(AppError::FolderTooManyFiles(format!("more than {MAX_ENTRIES} entries")));
            }
            // Providers allow duplicate names (cloud drives); only the first keeps the key.
            let dup = !names.insert(r.name.clone());
            let bad = match check_name(OsStr::new(&r.name)) {
                Err((reason, _)) => Some(reason),
                Ok(n) if dup || invalid_name(n) => Some(ExcludeReason::InvalidName),
                Ok(_) => None,
            };
            let rel = join_rel(&f.rel, &r.name);
            let hidden_self = r.name.starts_with('.');
            let hidden = hidden_parent || hidden_self;
            if windows_unsafe(&r.name) {
                self.w.stats.windows_unsafe_names += 1;
            }
            if r.is_dir() {
                self.w.stats.add_dir(&rel);
                if let Some(reason) = bad {
                    self.w.push(ItemKind::Dir, rel, 0, 0, hidden, None, Some(reason))?;
                } else {
                    let hidden_dir = f.hidden_dir.clone().or_else(|| hidden_self.then(|| rel.clone()));
                    uploadable += 1;
                    subdirs.push((r.doc_id, Dir { rel, hidden_dir }));
                }
                continue;
            }
            // No byte stream behind it (e.g. a Google Doc): never opened, like a special file.
            if r.is_virtual() {
                self.w.push(ItemKind::Special, rel, 0, 0, hidden, None, Some(bad.unwrap_or(ExcludeReason::Special)))?;
                continue;
            }
            let uri = document_uri(self.tree, &r.doc_id);
            let stat = match r.size.filter(|s| *s >= 0) {
                Some(s) => Some((s, r.mtime)),
                None => (self.fallback)(&uri).map(|(s, m)| (s, if r.mtime > 0 { r.mtime } else { m })),
            };
            let Some((size, mtime)) = stat else {
                self.w.push(ItemKind::File, rel, 0, r.mtime, hidden, None, Some(bad.unwrap_or(ExcludeReason::Unreadable)))?;
                continue;
            };
            let excl = bad.or_else(|| (size as u64 > MAX_FILE_BYTES).then_some(ExcludeReason::FileTooLarge));
            if excl.is_none() {
                uploadable += 1;
                let ext = KeyParts::from_key(&rel).extension;
                self.w.stats.add_file(&rel, size, ext.as_deref(), f.hidden_dir.as_deref(), hidden);
            }
            self.w.source_ref = Some(uri);
            self.w.push(ItemKind::File, rel, size, mtime, hidden, None, excl)?;
        }
        if children == 0 && !is_root {
            self.w.stats.empty_dirs += 1;
        }
        // Empty, or only excluded children: a marker keeps the folder in the bucket.
        if uploadable == 0 && !is_root && !entry_error {
            self.w.push(ItemKind::Dir, f.rel.clone(), 0, 0, hidden_parent, None, None)?;
        }
        self.progress(&f.rel)?;
        Ok(subdirs)
    }

    /// Throttled; also called per row so one huge folder still shows movement.
    fn progress(&mut self, dir: &str) -> AppResult<()> {
        if self.last_progress.elapsed() < PROGRESS_EVERY {
            return Ok(());
        }
        self.last_progress = Instant::now();
        let s = &self.w.stats;
        let p = WalkMsg::Progress { files: s.files, dirs: s.dirs, bytes: s.total_bytes, current_dir: dir.to_string() };
        self.w.send(p)
    }

    pub(super) fn finish(mut self) -> AppResult<Stats> {
        self.w.flush()?;
        Ok(self.w.stats)
    }
}

/// JNI setup failures arrive as strings.
struct WalkErr(AppError);

impl From<String> for WalkErr {
    fn from(s: String) -> Self {
        WalkErr(AppError::Internal(s))
    }
}

/// Blocking: walks the tree at `tree_uri` (normalized) into item batches on `tx`.
pub(super) fn walk_saf(tree_uri: &str, dest: &str, cancel: &CancellationToken, tx: &mpsc::Sender<WalkMsg>) -> AppResult<Stats> {
    let mut fallback = |uri: &str| -> Option<(i64, i64)> {
        let m = crate::saf::open_saf_fd(uri).ok()?.metadata().ok()?;
        Some((m.len() as i64, super::walk::mtime_ms(&m)))
    };
    let mut sw = SafWalker::new(dest, tree_uri, cancel, tx, &mut fallback);
    let mut visit = |d: &Dir, l: Listing| sw.visit(d, l).map_err(WalkErr);
    crate::saf::walk::walk_tree(tree_uri, Dir::default(), &|| cancel.is_cancelled(), &mut visit).map_err(|e| e.0)?;
    if cancel.is_cancelled() {
        return Err(AppError::Canceled("analysis".into()));
    }
    sw.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::folder_upload::{ItemState, NewItem};
    use crate::saf::walk::{drive, Row, FLAG_VIRTUAL_DOCUMENT, MIME_DIR};
    use std::collections::HashMap;

    const TREE: &str = "content://com.android.externalstorage.documents/tree/primary%3AT";

    fn dir(id: &str, name: &str) -> Row {
        Row { doc_id: id.into(), name: name.into(), mime: MIME_DIR.into(), ..Default::default() }
    }

    fn file(id: &str, name: &str, size: Option<i64>) -> Row {
        Row { doc_id: id.into(), name: name.into(), mime: "image/jpeg".into(), size, mtime: 1000, flags: 0 }
    }

    /// Runs the visitor over a fake tree; `None` listings are unreadable, `(rows, false)` partial.
    fn run(tree: HashMap<&str, (Vec<Row>, bool)>, dest: &str) -> (AppResult<Stats>, Vec<NewItem>, Vec<String>) {
        let (tx, mut rx) = mpsc::channel(1024);
        let cancel = CancellationToken::new();
        let mut opened = Vec::new();
        let res = {
            let mut fallback = |uri: &str| -> Option<(i64, i64)> {
                opened.push(uri.to_string());
                uri.ends_with("sized").then_some((42, 7))
            };
            let mut sw = SafWalker::new(dest, TREE, &cancel, &tx, &mut fallback);
            let mut list = |id: &str| -> AppResult<Listing> {
                Ok(tree.get(id).map(|(rows, complete)| Listing { rows: rows.clone(), complete: *complete }).unwrap_or_default())
            };
            let mut visit = |d: &Dir, l: Listing| sw.visit(d, l);
            drive("primary:T".into(), Dir::default(), &mut list, &mut visit).and_then(|_| sw.finish())
        };
        drop(tx);
        let mut items = Vec::new();
        while let Ok(m) = rx.try_recv() {
            if let WalkMsg::Items(v) = m {
                items.extend(v);
            }
        }
        (res, items, opened)
    }

    fn find<'a>(items: &'a [NewItem], rel: &str) -> Vec<&'a NewItem> {
        items.iter().filter(|i| i.rel_path == rel).collect()
    }

    #[test]
    fn walks_files_dirs_and_empty_dirs_like_desktop() {
        let tree = HashMap::from([
            ("primary:T", (vec![dir("primary:T/a", "a"), file("primary:T/x.jpg", "x.jpg", Some(5)), dir("primary:T/e", "e")], true)),
            ("primary:T/a", (vec![file("primary:T/a/y z.jpg", "y z.jpg", Some(3))], true)),
            ("primary:T/e", (vec![], true)),
        ]);
        let (res, items, _) = run(tree, "up/");
        let stats = res.unwrap();
        assert_eq!((stats.files, stats.total_bytes, stats.empty_dirs), (2, 8, 1));
        let x = find(&items, "x.jpg")[0];
        assert_eq!((x.kind, x.state, x.size, x.mtime_ms), (ItemKind::File, ItemState::Pending, 5, 1000));
        assert_eq!(x.source_ref.as_deref(), Some(format!("{TREE}/document/primary%3AT%2Fx.jpg").as_str()));
        let y = find(&items, "a/y z.jpg")[0];
        assert_eq!(y.source_ref.as_deref(), Some(format!("{TREE}/document/primary%3AT%2Fa%2Fy%20z.jpg").as_str()));
        // Only the empty folder gets a marker; "a" has a file, so none for it.
        let e = find(&items, "e");
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].kind, e[0].state, e[0].source_ref.as_deref()), (ItemKind::Dir, ItemState::Pending, None));
        assert!(find(&items, "a").is_empty());
        let seqs: Vec<i64> = items.iter().map(|i| i.seq).collect();
        assert_eq!(seqs, (1..=items.len() as i64).collect::<Vec<_>>(), "seq is dense and ordered");
    }

    #[test]
    fn virtual_duplicate_and_bad_names_are_excluded() {
        let mut v = file("primary:T/doc", "doc.gdoc", Some(0));
        v.flags = FLAG_VIRTUAL_DOCUMENT;
        let tree = HashMap::from([(
            "primary:T",
            (
                vec![
                    v,
                    file("primary:T/1", "same", Some(1)),
                    dir("primary:T/2", "same"),
                    file("primary:T/3", "a/b", Some(1)),
                    file("primary:T/4", "..", Some(1)),
                    file("primary:T/5", "tab\there", Some(1)),
                ],
                true,
            ),
        )]);
        let (res, items, _) = run(tree, "");
        assert_eq!(res.unwrap().files, 1, "only the first 'same' uploads");
        let doc = find(&items, "doc.gdoc")[0];
        assert_eq!((doc.kind, doc.exclude_reason), (ItemKind::Special, Some(ExcludeReason::Special)));
        let same = find(&items, "same");
        assert_eq!(same.len(), 2);
        assert_eq!((same[0].state, same[0].kind), (ItemState::Pending, ItemKind::File));
        assert_eq!((same[1].kind, same[1].exclude_reason), (ItemKind::Dir, Some(ExcludeReason::InvalidName)));
        assert_eq!(find(&items, "a/b")[0].exclude_reason, Some(ExcludeReason::InvalidName));
        assert_eq!(find(&items, "..")[0].exclude_reason, Some(ExcludeReason::InvalidName));
        assert_eq!(find(&items, "tab\there")[0].exclude_reason, Some(ExcludeReason::ControlChars));
    }

    #[test]
    fn unknown_size_opens_the_document_or_marks_it_unreadable() {
        let tree = HashMap::from([(
            "primary:T",
            (vec![file("primary:T/sized", "sized", None), file("primary:T/gone", "gone", Some(-1)), file("primary:T/k", "k", Some(9))], true),
        )]);
        let (res, items, opened) = run(tree, "");
        assert_eq!(res.unwrap().total_bytes, 42 + 9);
        let s = find(&items, "sized")[0];
        assert_eq!((s.size, s.mtime_ms, s.state), (42, 1000, ItemState::Pending), "listing mtime wins when known");
        assert_eq!(find(&items, "gone")[0].exclude_reason, Some(ExcludeReason::Unreadable));
        assert_eq!(opened.len(), 2, "known sizes are never opened");
    }

    #[test]
    fn unreadable_root_fails_and_unreadable_subdirs_are_reported() {
        let (res, _, _) = run(HashMap::new(), "");
        assert_eq!(res.err().map(|e| e.code()), Some("folder_source_unavailable"));

        let tree = HashMap::from([
            ("primary:T", (vec![dir("primary:T/locked", "locked"), dir("primary:T/half", "half")], true)),
            ("primary:T/half", (vec![file("primary:T/half/a", "a", Some(1))], false)),
        ]);
        let (res, items, _) = run(tree, "");
        res.unwrap();
        let locked = find(&items, "locked");
        assert_eq!((locked.len(), locked[0].exclude_reason), (1, Some(ExcludeReason::UnreadableDir)));
        let half = find(&items, "half");
        assert_eq!(half.len(), 1, "a partial folder gets no marker, only the unreadable_dir item");
        assert_eq!(half[0].exclude_reason, Some(ExcludeReason::UnreadableDir));
        assert_eq!(find(&items, "half/a")[0].state, ItemState::Pending, "rows read before the break still upload");
    }

    #[test]
    fn key_length_and_hidden_follow_desktop_rules() {
        let long = "x".repeat(1020);
        let tree = HashMap::from([
            ("primary:T", (vec![file("primary:T/l", &long, Some(1)), dir("primary:T/.h", ".h")], true)),
            ("primary:T/.h", (vec![file("primary:T/.h/f", "f", Some(2))], true)),
        ]);
        let (res, items, _) = run(tree, "dest/");
        let stats = res.unwrap();
        assert_eq!(find(&items, &long)[0].exclude_reason, Some(ExcludeReason::KeyTooLong));
        let f = find(&items, ".h/f")[0];
        assert!(f.hidden && f.state == ItemState::Pending);
        // Like desktop: key length is destination-dependent, so the analysis still counts the file.
        assert_eq!(stats.files, 2);
    }
}
