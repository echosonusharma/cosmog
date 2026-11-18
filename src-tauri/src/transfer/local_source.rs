//! Upload source access: a `content://` document opens through SAF (fresh fd each time), anything else through std.

use std::io;
use std::path::Path;

use crate::error::AppError;

pub fn is_saf(path: &Path) -> bool {
    path.to_str().is_some_and(crate::saf::is_content_uri)
}

pub async fn open(path: &Path) -> io::Result<tokio::fs::File> {
    if !is_saf(path) {
        return tokio::fs::File::open(path).await;
    }
    let uri = path.to_string_lossy().into_owned();
    let f = tokio::task::spawn_blocking(move || crate::saf::open_saf_fd(&uri)).await.map_err(io::Error::other)??;
    Ok(tokio::fs::File::from_std(f))
}

/// Follows links like `tokio::fs::metadata`; for SAF the fstat of a fresh fd.
pub async fn metadata(path: &Path) -> io::Result<std::fs::Metadata> {
    if is_saf(path) {
        open(path).await?.metadata().await
    } else {
        tokio::fs::metadata(path).await
    }
}

/// A lost SAF grant becomes `folder_source_unavailable` (folder jobs pause on it); the rest stays io.
pub fn app_err(path: &Path, e: io::Error) -> AppError {
    if is_saf(path) && e.kind() == io::ErrorKind::PermissionDenied {
        return AppError::FolderSourceUnavailable(crate::saf::uri::tree_display_name(&path.to_string_lossy()));
    }
    AppError::from(e)
}

pub async fn open_app(path: &Path) -> Result<tokio::fs::File, AppError> {
    open(path).await.map_err(|e| app_err(path, e))
}

pub async fn metadata_app(path: &Path) -> Result<std::fs::Metadata, AppError> {
    metadata(path).await.map_err(|e| app_err(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_paths_use_std_and_content_uris_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("content:x");
        std::fs::write(&p, b"abc").unwrap();
        assert!(!is_saf(&p));
        assert_eq!(metadata(&p).await.unwrap().len(), 3);
        let uri = Path::new("content://auth/tree/a/document/a%2Fb");
        assert!(is_saf(uri));
        // Off Android the SAF branch is taken and refuses, never falling back to std.
        #[cfg(not(target_os = "android"))]
        assert_eq!(open(uri).await.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn lost_saf_grant_maps_to_source_unavailable() {
        let doc = Path::new("content://com.android.externalstorage.documents/tree/primary%3ADCIM/document/primary%3ADCIM%2Fa.jpg");
        let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "SecurityException");
        let e = app_err(doc, denied());
        assert_eq!(e.code(), "folder_source_unavailable");
        assert!(e.to_wire_string().contains("DCIM"), "names the tree folder: {}", e.to_wire_string());
        assert_eq!(app_err(Path::new("/tmp/a"), denied()).code(), "io", "plain paths keep io");
        assert_eq!(app_err(doc, io::Error::new(io::ErrorKind::NotFound, "gone")).code(), "io");
    }
}
