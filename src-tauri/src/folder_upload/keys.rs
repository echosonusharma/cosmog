//! Object keys for folder items: `dest_prefix + rel_path`, '/'-joined, never trimmed or lossy.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::db::folder_upload::{ExcludeReason, MAX_KEY_BYTES};
use crate::error::{AppError, AppResult};

/// "" (bucket root) or a prefix ending in '/'; rejects empty, '.', '..' and control-char segments.
pub fn normalize_dest(raw: &str) -> AppResult<String> {
    let s = raw.trim_start_matches('/');
    if s.is_empty() {
        return Ok(String::new());
    }
    let s = if s.ends_with('/') { s.to_string() } else { format!("{s}/") };
    if s.len() as i64 > MAX_KEY_BYTES {
        return Err(AppError::InvalidInput("destination prefix over 1024 bytes".into()));
    }
    for seg in s[..s.len() - 1].split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || has_control(seg) {
            return Err(AppError::InvalidInput(format!("bad destination segment {seg:?}")));
        }
    }
    Ok(s)
}

/// U+0000-001F and U+007F break XML listings for every client.
pub fn has_control(s: &str) -> bool {
    s.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
}

/// The name as UTF-8, or the exclusion reason plus a lossy display form.
pub fn check_name(name: &OsStr) -> Result<&str, (ExcludeReason, String)> {
    match name.to_str() {
        None => Err((ExcludeReason::NonUtf8, name.to_string_lossy().into_owned())),
        Some(s) if has_control(s) => Err((ExcludeReason::ControlChars, s.to_string())),
        Some(s) => Ok(s),
    }
}

pub fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Dirs map to their `rel/` marker object.
pub fn object_key(dest: &str, rel: &str, is_dir: bool) -> String {
    if is_dir && !rel.ends_with('/') {
        format!("{dest}{rel}/")
    } else {
        format!("{dest}{rel}")
    }
}

pub fn key_too_long(dest: &str, rel: &str, is_dir: bool) -> bool {
    object_key(dest, rel, is_dir).len() as i64 > MAX_KEY_BYTES
}

/// Names Windows cannot create as-is (still uploaded, only warned about).
pub fn windows_unsafe(name: &str) -> bool {
    name.chars().any(|c| matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || name.ends_with('.')
        || name.ends_with(' ')
}

/// Local path of a '/'-joined rel path under `root`, using the OS separator.
/// Last component of a local root: errors (and their logs) never carry the full path.
pub fn root_name(root: &str) -> String {
    Path::new(root).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string())
}

pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_is_normalized_and_validated() {
        assert_eq!(normalize_dest("").unwrap(), "");
        assert_eq!(normalize_dest("/").unwrap(), "");
        assert_eq!(normalize_dest("/photos").unwrap(), "photos/");
        assert_eq!(normalize_dest("a/b/").unwrap(), "a/b/");
        assert_eq!(normalize_dest(" spaced /x ").unwrap(), " spaced /x /", "never trimmed");
        for bad in ["a//b", "a/./b", "../x", "a/..", "a\u{1}b", "a\u{7f}"] {
            assert_eq!(normalize_dest(bad).unwrap_err().code(), "invalid_input", "{bad:?}");
        }
        assert!(normalize_dest(&"x".repeat(1023)).is_ok(), "1023 + '/' = 1024 bytes");
        assert!(normalize_dest(&"x".repeat(1024)).is_err());
    }

    #[test]
    fn components_join_with_forward_slash() {
        let rel = ["a", "b c", "d.txt"].iter().fold(String::new(), |acc, n| join_rel(&acc, n));
        assert_eq!(rel, "a/b c/d.txt");
        assert_eq!(object_key("dest/", &rel, false), "dest/a/b c/d.txt");
        assert_eq!(object_key("dest/", "empty", true), "dest/empty/");
        let p = abs_path(Path::new("root"), &rel);
        let comps: Vec<_> = p.components().map(|c| c.as_os_str().to_str().unwrap().to_string()).collect();
        assert_eq!(comps, ["root", "a", "b c", "d.txt"], "OS separators locally");
    }

    #[test]
    fn names_are_never_trimmed() {
        let name = OsStr::new("a ");
        assert_eq!(check_name(name).unwrap(), "a ");
        assert_eq!(object_key("", "dir/a ", false), "dir/a ");
        assert!(windows_unsafe("a ") && windows_unsafe("x.") && windows_unsafe("a:b"));
        assert!(!windows_unsafe("résumé ü.txt"));
    }

    #[test]
    fn key_limit_counts_utf8_bytes() {
        // 'é' is 2 bytes: 512 of them = 1024 bytes exactly.
        let rel = "é".repeat(512);
        assert!(!key_too_long("", &rel, false));
        assert!(key_too_long("", &rel, true), "dir marker adds '/'");
        assert!(key_too_long("d/", &rel, false));
        let rel = "é".repeat(511) + "x";
        assert!(!key_too_long("", &rel, false), "1023 bytes");
    }

    #[test]
    fn control_chars_are_rejected() {
        assert_eq!(check_name(OsStr::new("a\tb")).unwrap_err().0, ExcludeReason::ControlChars);
        assert_eq!(check_name(OsStr::new("a\u{7f}")).unwrap_err().0, ExcludeReason::ControlChars);
        assert!(check_name(OsStr::new("ok ü")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_is_excluded_not_lossy() {
        use std::os::unix::ffi::OsStrExt;
        let name = OsStr::from_bytes(b"bad\xffname");
        let (reason, shown) = check_name(name).unwrap_err();
        assert_eq!(reason, ExcludeReason::NonUtf8);
        assert!(shown.contains('\u{fffd}'), "lossy form is display-only");
    }
}
