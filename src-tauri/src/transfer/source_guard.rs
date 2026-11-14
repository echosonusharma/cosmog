//! Opt-in source checks (folder uploads): the path must still be the regular file seen at
//! enqueue, located inside the chosen root. std has no O_NOFOLLOW, so every open is verified
//! by comparing the opened handle's identity with a no-follow stat.

use std::fs::Metadata;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// File identity: device + inode on unix; size + mtime elsewhere (std has no stable file index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    pub a: u64,
    pub b: u64,
}

pub fn file_id(m: &Metadata) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileId { a: m.dev(), b: m.ino() }
    }
    #[cfg(not(unix))]
    {
        let ns = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        FileId { a: m.len(), b: ns }
    }
}

/// Root is canonical; `id` is the identity of the file at enqueue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGuard {
    pub root: PathBuf,
    pub id: FileId,
}

/// What a source path is right now, without following a final link.
pub enum Inspect {
    Missing,
    Link,
    Special,
    /// Resolves (through a swapped parent) to a place outside the root.
    Outside,
    File(Metadata),
    Error(AppError),
}

/// No-follow stat of `path`, plus containment of its real location in canonical `root`.
pub async fn inspect(path: &Path, root: &Path) -> Inspect {
    let m = match tokio::fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Inspect::Missing,
        Err(e) => return Inspect::Error(AppError::Io(e.to_string())),
        Ok(m) => m,
    };
    if m.file_type().is_symlink() {
        return Inspect::Link;
    }
    if !m.is_file() {
        return Inspect::Special;
    }
    let canon = match tokio::fs::canonicalize(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Inspect::Missing,
        Err(e) => return Inspect::Error(AppError::Io(e.to_string())),
        Ok(c) => c,
    };
    if !canon.starts_with(root) {
        return Inspect::Outside;
    }
    // The canonical location must be the same file the path stat saw (no swap in between).
    match tokio::fs::symlink_metadata(&canon).await {
        Ok(c) if c.is_file() && file_id(&c) == file_id(&m) => Inspect::File(m),
        Ok(_) => Inspect::Outside,
        Err(e) => Inspect::Error(AppError::Io(e.to_string())),
    }
}

fn changed(path: &Path) -> AppError {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    AppError::SourceChanged(format!("{name}: no longer the file checked inside the folder"))
}

/// Opens `path` only if it is still the guarded regular file inside the root.
pub async fn open_checked(path: &Path, g: &SourceGuard) -> AppResult<tokio::fs::File> {
    match inspect(path, &g.root).await {
        Inspect::File(m) if file_id(&m) == g.id => {}
        Inspect::Error(e) => return Err(e),
        _ => return Err(changed(path)),
    }
    let f = tokio::fs::File::open(path).await?;
    // The open follows links: the handle's own identity decides.
    let fm = f.metadata().await?;
    if !fm.is_file() || file_id(&fm) != g.id {
        return Err(changed(path));
    }
    Ok(f)
}

/// Canonical form for display and storage (no `\\?\` verbatim prefix on Windows).
pub fn display_root(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn swapped_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = tokio::fs::canonicalize(dir.path()).await.unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"secret").unwrap();
        let p = root.join("a.txt");
        std::fs::write(&p, b"ok").unwrap();
        let id = file_id(&std::fs::symlink_metadata(&p).unwrap());
        let g = SourceGuard { root: root.clone(), id };
        assert!(open_checked(&p, &g).await.is_ok());
        std::fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(outside.path(), &p).unwrap();
        assert_eq!(open_checked(&p, &g).await.unwrap_err().code(), "source_changed");
        assert!(matches!(inspect(&p, &root).await, Inspect::Link));
    }

    #[tokio::test]
    async fn swapped_parent_dir_is_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = tokio::fs::canonicalize(dir.path()).await.unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("f"), b"secret").unwrap();
        std::os::unix::fs::symlink(other.path(), root.join("sub")).unwrap();
        assert!(matches!(inspect(&root.join("sub/f"), &root).await, Inspect::Outside));
    }
}
