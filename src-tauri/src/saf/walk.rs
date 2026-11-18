//! Streaming SAF tree walk: one ContentResolver query per directory, rows handed to a visitor
//! that decides which subdirectories to descend into. Only one directory listing is in memory.

pub const MIME_DIR: &str = "vnd.android.document/directory";
/// `DocumentsContract.Document.FLAG_VIRTUAL_DOCUMENT`: no byte stream, cannot be uploaded.
pub const FLAG_VIRTUAL_DOCUMENT: i64 = 1 << 9;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub doc_id: String,
    pub name: String,
    pub mime: String,
    /// None when the provider does not know it.
    pub size: Option<i64>,
    /// Epoch ms, 0 unknown.
    pub mtime: i64,
    pub flags: i64,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.mime == MIME_DIR
    }

    pub fn is_virtual(&self) -> bool {
        self.flags & FLAG_VIRTUAL_DOCUMENT != 0
    }
}

/// One directory's children; `complete` false when the query failed or broke off.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub rows: Vec<Row>,
    pub complete: bool,
}

/// Visitor callback: (dir context, listing) -> subdirs to descend as (doc_id, context).
pub type Visit<'a, D, E> = dyn FnMut(&D, Listing) -> Result<Vec<(String, D)>, E> + 'a;

/// Depth-first traversal over any lister; the JNI walk and tests share it.
pub fn drive<D, E>(
    root_id: String,
    root: D,
    list: &mut dyn FnMut(&str) -> Result<Listing, E>,
    visit: &mut Visit<'_, D, E>,
) -> Result<(), E> {
    let mut stack = vec![(root_id, root)];
    while let Some((id, d)) = stack.pop() {
        let listing = list(&id)?;
        let subs = visit(&d, listing)?;
        stack.extend(subs.into_iter().rev());
    }
    Ok(())
}

/// Blocking: call from spawn_blocking. `cancel` is polled between rows (the listing then ends incomplete).
#[cfg(target_os = "android")]
pub fn walk_tree<D, E: From<String>>(
    tree_uri: &str,
    root: D,
    cancel: &dyn Fn() -> bool,
    visit: &mut Visit<'_, D, E>,
) -> Result<(), E> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err(E::from("android context not initialized".into()));
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| E::from(format!("JavaVM::from_raw: {e}")))?;
    let mut env = vm.attach_current_thread().map_err(|e| E::from(format!("attach_current_thread: {e}")))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let (resolver, tree_obj) = super::resolver_and_uri(&mut env, &context, tree_uri).map_err(E::from)?;
    let dc = env
        .find_class("android/provider/DocumentsContract")
        .map_err(|e| E::from(super::jni_err(&mut env, "find_class(DocumentsContract)", e)))?;
    let root_id = {
        let s = env
            .call_static_method(&dc, "getTreeDocumentId", "(Landroid/net/Uri;)Ljava/lang/String;", &[JValue::Object(&tree_obj)])
            .map_err(|e| E::from(super::jni_err(&mut env, "getTreeDocumentId", e)))?
            .l()
            .map_err(|e| E::from(format!("getTreeDocumentId.l: {e}")))?;
        if s.is_null() {
            return Err(E::from("getTreeDocumentId returned null".into()));
        }
        super::jstring_owned(&mut env, s.into()).map_err(E::from)?
    };
    let proj = jni_impl::projection(&mut env).map_err(E::from)?;
    let mut list = |id: &str| jni_impl::list_children(&mut env, &resolver, &dc, &tree_obj, &proj, id, cancel).map_err(E::from);
    drive(root_id, root, &mut list, visit)
}

#[cfg(not(target_os = "android"))]
pub fn walk_tree<D, E: From<String>>(
    _tree_uri: &str,
    _root: D,
    _cancel: &dyn Fn() -> bool,
    _visit: &mut Visit<'_, D, E>,
) -> Result<(), E> {
    Err(E::from("SAF tree walk is Android-only".into()))
}

/// The tree root still answers a query as a directory (grant held, provider present).
#[cfg(target_os = "android")]
pub async fn tree_readable(tree_uri: String) -> bool {
    tokio::task::spawn_blocking(move || -> Result<bool, String> {
        use jni::objects::JObject;
        use jni::JavaVM;

        let root = super::uri::tree_document_id(&tree_uri).ok_or("not a tree uri")?;
        let doc = super::uri::document_uri(&tree_uri, &root);
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm.attach_current_thread().map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };
        env.with_local_frame(16, |env| -> Result<bool, jni_impl::Fatal> {
            let (resolver, uri) = super::resolver_and_uri(env, &context, &doc).map_err(jni_impl::Fatal)?;
            let proj = jni_impl::projection(env).map_err(jni_impl::Fatal)?;
            let l = jni_impl::query_rows(env, &resolver, &uri, &proj, &|| false)?;
            Ok(l.complete && l.rows.first().is_some_and(Row::is_dir))
        })
        .map_err(|f| f.0)
    })
    .await
    .ok()
    .and_then(|r| r.map_err(|e| tracing::debug!("tree_readable: {e}")).ok())
    .unwrap_or(false)
}

#[cfg(not(target_os = "android"))]
pub async fn tree_readable(_tree_uri: String) -> bool {
    false
}

#[cfg(target_os = "android")]
mod jni_impl {
    use super::{Listing, Row};
    use crate::saf::{jni_err, jstring_owned};
    use jni::objects::{JObject, JObjectArray, JValue};
    use jni::JNIEnv;

    const COLS: [&str; 6] = ["document_id", "_display_name", "mime_type", "_size", "last_modified", "flags"];

    /// Frame closures need an error that is `From<jni::errors::Error>`.
    pub struct Fatal(pub String);

    impl From<jni::errors::Error> for Fatal {
        fn from(e: jni::errors::Error) -> Self {
            Fatal(format!("jni: {e}"))
        }
    }

    pub fn projection<'a>(env: &mut JNIEnv<'a>) -> Result<JObjectArray<'a>, String> {
        let arr = env
            .new_object_array(COLS.len() as i32, "java/lang/String", JObject::null())
            .map_err(|e| jni_err(env, "new_object_array(projection)", e))?;
        for (i, c) in COLS.iter().enumerate() {
            let s = env.new_string(c).map_err(|e| jni_err(env, "new_string(col)", e))?;
            env.set_object_array_element(&arr, i as i32, JObject::from(s))
                .map_err(|e| jni_err(env, "set_object_array_element", e))?;
        }
        Ok(arr)
    }

    /// One directory; a frame per call frees its local refs on return.
    pub fn list_children(
        env: &mut JNIEnv,
        resolver: &JObject,
        dc: &jni::objects::JClass,
        tree: &JObject,
        proj: &JObjectArray,
        parent_id: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Listing, String> {
        env.with_local_frame(64, |env| -> Result<Listing, Fatal> {
            let pid = env.new_string(parent_id).map_err(|e| Fatal(jni_err(env, "new_string(parentDocId)", e)))?;
            let children = env
                .call_static_method(
                    dc,
                    "buildChildDocumentsUriUsingTree",
                    "(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValue::Object(tree), JValue::Object(&JObject::from(pid))],
                )
                .map_err(|e| Fatal(jni_err(env, "buildChildDocumentsUriUsingTree", e)))?
                .l()
                .map_err(|e| Fatal(format!("buildChildDocumentsUriUsingTree.l: {e}")))?;
            query_rows(env, resolver, &children, proj, cancel)
        })
        .map_err(|f| f.0)
    }

    /// A failed query or cursor yields an incomplete listing, never an error.
    pub fn query_rows(
        env: &mut JNIEnv,
        resolver: &JObject,
        uri: &JObject,
        proj: &JObjectArray,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Listing, Fatal> {
        let r = env.call_method(
            resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[
                JValue::Object(uri),
                JValue::Object(proj),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        );
        let cursor = match r.map(|v| v.l()) {
            Ok(Ok(c)) if !c.is_null() => c,
            Ok(_) => return Ok(Listing::default()),
            Err(e) => {
                let _ = jni_err(env, "ContentResolver.query", e);
                return Ok(Listing::default());
            }
        };
        let mut out = Listing { rows: Vec::new(), complete: true };
        loop {
            if cancel() {
                out.complete = false;
                break;
            }
            match env.call_method(&cursor, "moveToNext", "()Z", &[]).map(|v| v.z().unwrap_or(false)) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    let _ = jni_err(env, "Cursor.moveToNext", e);
                    out.complete = false;
                    break;
                }
            }
            // Per-row frame: huge directories would otherwise overflow the local ref table.
            let row = env.with_local_frame(16, |env| -> Result<Row, Fatal> {
                Ok(Row {
                    doc_id: get_string(env, &cursor, 0).unwrap_or_default(),
                    name: get_string(env, &cursor, 1).unwrap_or_default(),
                    mime: get_string(env, &cursor, 2).unwrap_or_default(),
                    size: get_long(env, &cursor, 3),
                    mtime: get_long(env, &cursor, 4).unwrap_or(0),
                    flags: get_long(env, &cursor, 5).unwrap_or(0),
                })
            });
            match row {
                Ok(r) if !r.doc_id.is_empty() => out.rows.push(r),
                Ok(_) => {}
                Err(f) => {
                    let _ = env.call_method(&cursor, "close", "()V", &[]);
                    return Err(f);
                }
            }
        }
        if let Err(e) = env.call_method(&cursor, "close", "()V", &[]) {
            let _ = jni_err(env, "Cursor.close", e);
        }
        Ok(out)
    }

    fn is_null(env: &mut JNIEnv, cursor: &JObject, col: i32) -> bool {
        match env.call_method(cursor, "isNull", "(I)Z", &[JValue::Int(col)]) {
            Ok(v) => v.z().unwrap_or(true),
            Err(e) => {
                let _ = jni_err(env, "Cursor.isNull", e);
                true
            }
        }
    }

    /// None on null column or error (exception cleared).
    fn get_string(env: &mut JNIEnv, cursor: &JObject, col: i32) -> Option<String> {
        if is_null(env, cursor, col) {
            return None;
        }
        let v = match env.call_method(cursor, "getString", "(I)Ljava/lang/String;", &[JValue::Int(col)]) {
            Ok(v) => v,
            Err(e) => {
                let _ = jni_err(env, "Cursor.getString", e);
                return None;
            }
        };
        let s = v.l().ok()?;
        if s.is_null() {
            return None;
        }
        jstring_owned(env, s.into()).ok()
    }

    fn get_long(env: &mut JNIEnv, cursor: &JObject, col: i32) -> Option<i64> {
        if is_null(env, cursor, col) {
            return None;
        }
        match env.call_method(cursor, "getLong", "(I)J", &[JValue::Int(col)]) {
            Ok(v) => v.j().ok(),
            Err(e) => {
                let _ = jni_err(env, "Cursor.getLong", e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn row(id: &str, name: &str, dir: bool) -> Row {
        Row { doc_id: id.into(), name: name.into(), mime: if dir { MIME_DIR.into() } else { "text/plain".into() }, ..Default::default() }
    }

    #[test]
    fn flags_and_dir_mime() {
        assert!(row("a", "a", true).is_dir());
        assert!(!row("a", "a", false).is_dir());
        let mut r = row("a", "a", false);
        r.flags = FLAG_VIRTUAL_DOCUMENT | 1;
        assert!(r.is_virtual());
        r.flags = 1;
        assert!(!r.is_virtual());
    }

    #[test]
    fn drive_visits_depth_first_in_listing_order_and_only_descends_where_told() {
        let tree: HashMap<&str, Vec<Row>> = HashMap::from([
            ("root", vec![row("d1", "d1", true), row("f", "f", false), row("d2", "d2", true), row("skip", "skip", true)]),
            ("d1", vec![row("d1/x", "x", false)]),
            ("d2", vec![]),
            ("skip", vec![row("never", "never", false)]),
        ]);
        let mut listed = Vec::new();
        let mut list = |id: &str| -> Result<Listing, String> {
            listed.push(id.to_string());
            Ok(Listing { rows: tree[id].clone(), complete: true })
        };
        let mut seen = Vec::new();
        let mut visit = |prefix: &String, l: Listing| -> Result<Vec<(String, String)>, String> {
            let mut subs = Vec::new();
            for r in l.rows {
                let rel = if prefix.is_empty() { r.name.clone() } else { format!("{prefix}/{}", r.name) };
                if r.is_dir() && r.name != "skip" {
                    subs.push((r.doc_id, rel.clone()));
                }
                seen.push(rel);
            }
            Ok(subs)
        };
        drive("root".into(), String::new(), &mut list, &mut visit).unwrap();
        assert_eq!(listed, ["root", "d1", "d2"]);
        assert_eq!(seen, ["d1", "f", "d2", "skip", "d1/x"]);
    }

    #[test]
    fn visitor_error_stops_the_walk() {
        let mut calls = 0;
        let mut list = |_: &str| -> Result<Listing, String> {
            calls += 1;
            Ok(Listing { rows: vec![row("d", "d", true)], complete: true })
        };
        let mut visit = |_: &(), _: Listing| -> Result<Vec<(String, ())>, String> { Err("stop".into()) };
        assert_eq!(drive("r".into(), (), &mut list, &mut visit).unwrap_err(), "stop");
        assert_eq!(calls, 1);
    }
}
