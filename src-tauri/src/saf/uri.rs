//! Pure SAF URI helpers: tree normalization, document URIs, display names. Encoding matches
//! `android.net.Uri.encode`, so built URIs equal what DocumentsContract would build.

const SCHEME: &str = "content://";

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'!' | b'.' | b'~' | b'\'' | b'(' | b')' | b'*')
}

/// `Uri.encode`: UTF-8 bytes, uppercase hex, the unreserved set kept.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-decodes to UTF-8; None on a bad escape or invalid UTF-8.
pub fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// (authority, decoded tree document id) of a `content://<auth>/tree/<id>[/...]` URI.
fn parse_tree(uri: &str) -> Option<(&str, String)> {
    let rest = uri.strip_prefix(SCHEME)?;
    let rest = rest.split(['?', '#']).next()?;
    let (auth, path) = rest.split_once('/')?;
    let mut segs = path.split('/');
    if auth.is_empty() || segs.next()? != "tree" {
        return None;
    }
    let id = decode(segs.next()?)?;
    (!id.is_empty()).then_some((auth, id))
}

/// Canonical `content://<auth>/tree/<Uri.encode(id)>`, so equal trees compare equal as strings.
pub fn normalize_tree_uri(uri: &str) -> Option<String> {
    let (auth, id) = parse_tree(uri)?;
    Some(format!("{SCHEME}{auth}/tree/{}", encode(&id)))
}

pub fn tree_document_id(uri: &str) -> Option<String> {
    parse_tree(uri).map(|(_, id)| id)
}

/// `buildDocumentUriUsingTree(tree, doc_id)`.
pub fn document_uri(tree_uri: &str, doc_id: &str) -> String {
    let tree = normalize_tree_uri(tree_uri).unwrap_or_else(|| tree_uri.to_string());
    format!("{tree}/document/{}", encode(doc_id))
}

/// Providers whose listed mtime and fstat mtime are the same file's (local storage). Others may
/// serve cache copies with a fresh mtime, so only size can tell a change there.
pub fn mtime_reliable(uri: &str) -> bool {
    uri.strip_prefix(SCHEME).and_then(|r| r.split('/').next()) == Some("com.android.externalstorage.documents")
}

/// Same tree after normalization.
pub fn same_tree(a: &str, b: &str) -> bool {
    match (normalize_tree_uri(a), normalize_tree_uri(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Folder name from the tree id: `primary:DCIM/Camera` -> "Camera". The internal storage root
/// gives "" so the UI shows its own localized label.
pub fn tree_display_name(uri: &str) -> String {
    let Some(id) = tree_document_id(uri) else { return "folder".into() };
    let (vol, path) = id.split_once(':').unwrap_or(("", id.as_str()));
    match path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()) {
        Some(last) => last.to_string(),
        None if vol == "primary" => String::new(),
        None if !vol.is_empty() => vol.to_string(),
        None => id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT: &str = "content://com.android.externalstorage.documents";

    #[test]
    fn encode_matches_android_uri_encode() {
        assert_eq!(encode("primary:DCIM/Camera"), "primary%3ADCIM%2FCamera");
        assert_eq!(encode("a b+c_-!.~'()*"), "a%20b%2Bc_-!.~'()*");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(decode("primary%3ADCIM%2FCamera").unwrap(), "primary:DCIM/Camera");
        assert_eq!(decode("%c3%a9").unwrap(), "é", "lowercase hex decodes");
        assert!(decode("%zz").is_none());
        assert!(decode("%4").is_none());
        assert!(decode("%FF").is_none(), "invalid UTF-8");
    }

    #[test]
    fn tree_uris_normalize_to_one_form() {
        let canon = format!("{EXT}/tree/primary%3ADCIM%2FCamera");
        assert_eq!(normalize_tree_uri(&canon).unwrap(), canon);
        // Lowercase escapes, unescaped ':', '/'-encoded spellings and a document suffix all collapse.
        for v in [
            format!("{EXT}/tree/primary%3aDCIM%2fCamera"),
            format!("{EXT}/tree/primary:DCIM%2FCamera"),
            format!("{EXT}/tree/primary%3ADCIM%2FCamera/document/primary%3ADCIM%2FCamera%2Fa.jpg"),
            format!("{EXT}/tree/primary%3ADCIM%2FCamera?x=1"),
        ] {
            assert_eq!(normalize_tree_uri(&v).unwrap(), canon, "{v}");
            assert!(same_tree(&v, &canon));
        }
        assert!(!same_tree(&canon, &format!("{EXT}/tree/primary%3ADCIM")));
        for bad in ["/sdcard/DCIM", "content://", "content://auth/document/x", "content://auth/tree/", "content:///tree/x"] {
            assert!(normalize_tree_uri(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn document_uri_matches_build_document_uri_using_tree() {
        let tree = format!("{EXT}/tree/primary%3ADCIM");
        assert_eq!(
            document_uri(&tree, "primary:DCIM/My Pics/a (1).jpg"),
            format!("{EXT}/tree/primary%3ADCIM/document/primary%3ADCIM%2FMy%20Pics%2Fa%20(1).jpg")
        );
        assert_eq!(tree_document_id(&tree).unwrap(), "primary:DCIM");
    }

    #[test]
    fn only_local_storage_mtime_is_trusted() {
        assert!(mtime_reliable(&format!("{EXT}/tree/primary%3AA/document/primary%3AA%2Fb")));
        assert!(!mtime_reliable("content://com.google.android.apps.docs.storage/tree/x/document/y"));
        assert!(!mtime_reliable("content://com.android.externalstorage.documents.evil/tree/x"));
        assert!(!mtime_reliable("/sdcard/a"));
    }

    #[test]
    fn display_name_is_the_folder_name() {
        let t = |id: &str| format!("{EXT}/tree/{}", encode(id));
        assert_eq!(tree_display_name(&t("primary:DCIM/Camera")), "Camera");
        assert_eq!(tree_display_name(&t("primary:DCIM/Camera/")), "Camera");
        assert_eq!(tree_display_name(&t("primary:Download")), "Download");
        assert_eq!(tree_display_name(&t("primary:")), "", "the UI localizes the storage root");
        assert_eq!(tree_display_name(&t("1234-ABCD:")), "1234-ABCD");
        assert_eq!(tree_display_name(&t("1234-ABCD:Music")), "Music");
        assert_eq!(tree_display_name(&t("raw:/storage/emulated/0/Download/x")), "x");
        assert_eq!(tree_display_name("not a uri"), "folder");
    }
}
