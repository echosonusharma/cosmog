use std::path::{Path, PathBuf};

use tracing::warn;

use crate::db::night_watcher::NightWatch;
use crate::error::{AppError, AppResult};

pub(super) async fn hash_file(path: &Path) -> AppResult<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<String> {
        let mut hasher = blake3::Hasher::new();
        let mut f = std::fs::File::open(&path)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", path.display())))?;
        std::io::copy(&mut f, &mut hasher)
            .map_err(|e| AppError::Internal(format!("hash {}: {e}", path.display())))?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Returns rel paths + subdir read-error count. A root read failure is a hard error so state isn't pruned.
pub(super) async fn collect_files(root: PathBuf) -> AppResult<(Vec<String>, u64)> {
    tokio::task::spawn_blocking(move || -> AppResult<(Vec<String>, u64)> {
        let mut out = Vec::new();
        let mut read_errors = 0u64;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    if dir == root {
                        return Err(AppError::InvalidInput(format!(
                            "watched dir vanished during scan: {}: {e}",
                            dir.display()
                        )));
                    }
                    warn!(dir = %dir.display(), "read_dir failed: {e}");
                    read_errors += 1;
                    continue;
                }
            };
            for entry in rd.flatten() {
                let p = entry.path();
                match entry.file_type() {
                    Ok(t) if t.is_dir() => stack.push(p),
                    Ok(t) if t.is_file() => {
                        if let Ok(rel) = p.strip_prefix(&root) {
                            out.push(normalize_rel(rel));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok((out, read_errors))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

pub(super) fn normalize_rel(rel: &Path) -> String {
    let lossy = rel.to_string_lossy();
    // Windows only: on Unix '\' is a legal filename char and converting would collide keys.
    #[cfg(windows)]
    return lossy.replace('\\', "/");
    #[cfg(not(windows))]
    return lossy.into_owned();
}

// Android has no `ignore` dep, so matching is a no-op there.

#[cfg(not(target_os = "android"))]
pub(super) struct Matcher(Option<ignore::gitignore::Gitignore>);

#[cfg(not(target_os = "android"))]
impl Matcher {
    pub(super) fn build(watch: &NightWatch) -> Self {
        let Some(path) = watch.ignore_file.as_ref() else {
            return Matcher(None);
        };
        let mut b = ignore::gitignore::GitignoreBuilder::new(&watch.local_dir);
        if let Some(e) = b.add(path) {
            warn!(watch = %watch.id, "ignore file load failed: {e}");
        }
        Matcher(b.build().ok())
    }

    pub(super) fn is_ignored(&self, rel: &str) -> bool {
        match &self.0 {
            // Ancestors too, so a `build/` rule excludes `build/x.bin`.
            Some(ig) => ig.matched_path_or_any_parents(rel, false).is_ignore(),
            None => false,
        }
    }
}

#[cfg(target_os = "android")]
pub(super) struct Matcher;

#[cfg(target_os = "android")]
impl Matcher {
    pub(super) fn build(_watch: &NightWatch) -> Self {
        Matcher
    }
    pub(super) fn is_ignored(&self, _rel: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "android"))]
    #[test]
    fn ignore_file_excludes_matched_paths() {
        use crate::db::night_watcher::NightWatch;

        let dir = tempfile::tempdir().unwrap();
        let ignore_path = dir.path().join(".cosmogignore");
        std::fs::write(&ignore_path, "*.log\nbuild/\n").unwrap();

        let watch = NightWatch {
            id: "w1".into(),
            account_id: "a1".into(),
            bucket: "b1".into(),
            local_dir: dir.path().to_string_lossy().to_string(),
            key_prefix: String::new(),
            ignore_file: Some(ignore_path.to_string_lossy().to_string()),
            delete_policy: "keep".into(),
            full_scan_secs: 300,
            enabled: true,
            last_scan_at: None,
            last_error: None,
            created_at: 0,
            tree_uri: None,
        };
        let m = Matcher::build(&watch);
        assert!(m.is_ignored("debug.log"));
        assert!(m.is_ignored("build/out.bin"));
        assert!(!m.is_ignored("keep.txt"));
    }
}
