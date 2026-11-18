#[cfg(target_os = "android")]
use super::jni_err;
use super::walk::{walk_tree, Listing};

/// `rel_path` tree-relative, forward slashes, no leading slash; `mtime` epoch ms (0 unknown); `size` -1 if unknown.
#[derive(serde::Serialize)]
pub struct SafEntry {
    pub rel_path: String,
    pub doc_uri: String,
    pub size: i64,
    pub mtime: i64,
    pub is_dir: bool,
}

/// Files only (dirs are descended, not emitted). `read_errors` non-zero means a PARTIAL walk:
/// the caller must skip mark-and-sweep or it prunes live files.
pub async fn collect_tree_files(tree_uri: String) -> Result<(Vec<SafEntry>, u64), String> {
    tokio::task::spawn_blocking(move || collect_blocking(&tree_uri))
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// (rel prefix, is root).
type Dir = (String, bool);

fn collect_blocking(tree_uri: &str) -> Result<(Vec<SafEntry>, u64), String> {
    let mut out = Vec::new();
    let mut read_errors = 0u64;
    let mut visit = |dir: &Dir, l: Listing| -> Result<Vec<(String, Dir)>, String> {
        let (prefix, is_root) = dir;
        if !l.complete {
            read_errors += 1;
            // Unreadable root (grant revoked, provider gone) must fail the scan, not look empty.
            if *is_root {
                return Err("cannot read watched folder; folder access may have been revoked, pick it again".into());
            }
        }
        let mut subs = Vec::new();
        for r in l.rows {
            let rel_path = if prefix.is_empty() { r.name.clone() } else { format!("{prefix}/{}", r.name) };
            if r.is_dir() {
                subs.push((r.doc_id, (rel_path, false)));
                continue;
            }
            out.push(SafEntry {
                rel_path,
                doc_uri: super::uri::document_uri(tree_uri, &r.doc_id),
                size: r.size.unwrap_or(-1),
                mtime: r.mtime,
                is_dir: false,
            });
        }
        Ok(subs)
    };
    walk_tree(tree_uri, (String::new(), true), &|| false, &mut visit)?;
    Ok((out, read_errors))
}

/// Call only once nothing uses this tree. Releases every persisted grant whose URI is the same
/// tree after normalization (the grant was taken on the picker's raw spelling).
#[cfg(target_os = "android")]
pub async fn release_tree_permission(tree_uri: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        use jni::objects::{JObject, JValue};
        use jni::JavaVM;

        const FLAG_GRANT_READ_URI_PERMISSION: i32 = 1;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm.attach_current_thread().map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        env.with_local_frame(32, |env| -> Result<(), Fatal> {
            let (resolver, given) = super::resolver_and_uri(env, &context, &tree_uri).map_err(Fatal)?;
            let list = env
                .call_method(&resolver, "getPersistedUriPermissions", "()Ljava/util/List;", &[])
                .map_err(|e| Fatal(jni_err(env, "getPersistedUriPermissions", e)))?
                .l()?;
            let n = env.call_method(&list, "size", "()I", &[]).map_err(|e| Fatal(jni_err(env, "List.size", e)))?.i()?;
            let mut released = false;
            for i in 0..n {
                // Frame per grant: an app can hold many.
                let r = env.with_local_frame(8, |env| -> Result<bool, Fatal> {
                    let perm = env
                        .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])
                        .map_err(|e| Fatal(jni_err(env, "List.get", e)))?
                        .l()?;
                    let uri = env
                        .call_method(&perm, "getUri", "()Landroid/net/Uri;", &[])
                        .map_err(|e| Fatal(jni_err(env, "UriPermission.getUri", e)))?
                        .l()?;
                    let s = env
                        .call_method(&uri, "toString", "()Ljava/lang/String;", &[])
                        .map_err(|e| Fatal(jni_err(env, "Uri.toString", e)))?
                        .l()?;
                    let s = super::jstring_owned(env, s.into()).map_err(Fatal)?;
                    if !super::uri::same_tree(&s, &tree_uri) {
                        return Ok(false);
                    }
                    release(env, &resolver, &uri)?;
                    Ok(true)
                })?;
                released |= r;
            }
            if !released {
                release(env, &resolver, &given)?;
            }
            Ok(())
        })
        .map_err(|f| f.0)?;

        fn release(env: &mut jni::JNIEnv, resolver: &JObject, uri: &JObject) -> Result<(), Fatal> {
            env.call_method(
                resolver,
                "releasePersistableUriPermission",
                "(Landroid/net/Uri;I)V",
                &[JValue::Object(uri), JValue::Int(FLAG_GRANT_READ_URI_PERMISSION)],
            )
            .map_err(|e| Fatal(jni_err(env, "releasePersistableUriPermission", e)))?;
            Ok(())
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(target_os = "android")]
struct Fatal(String);

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for Fatal {
    fn from(e: jni::errors::Error) -> Self {
        Fatal(format!("jni: {e}"))
    }
}

#[cfg(not(target_os = "android"))]
pub async fn release_tree_permission(_tree_uri: String) -> Result<(), String> {
    Ok(())
}

/// Tree URIs this app holds a persisted grant for (raw spelling as granted).
#[cfg(target_os = "android")]
pub async fn persisted_tree_uris() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        use jni::objects::{JObject, JValue};
        use jni::JavaVM;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm.attach_current_thread().map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };
        env.with_local_frame(16, |env| -> Result<Vec<String>, Fatal> {
            let resolver = env
                .call_method(&context, "getContentResolver", "()Landroid/content/ContentResolver;", &[])
                .map_err(|e| Fatal(jni_err(env, "getContentResolver", e)))?
                .l()?;
            let list = env
                .call_method(&resolver, "getPersistedUriPermissions", "()Ljava/util/List;", &[])
                .map_err(|e| Fatal(jni_err(env, "getPersistedUriPermissions", e)))?
                .l()?;
            let n = env.call_method(&list, "size", "()I", &[]).map_err(|e| Fatal(jni_err(env, "List.size", e)))?.i()?;
            let mut out = Vec::new();
            for i in 0..n {
                let s = env.with_local_frame(8, |env| -> Result<String, Fatal> {
                    let perm = env
                        .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])
                        .map_err(|e| Fatal(jni_err(env, "List.get", e)))?
                        .l()?;
                    let uri = env
                        .call_method(&perm, "getUri", "()Landroid/net/Uri;", &[])
                        .map_err(|e| Fatal(jni_err(env, "UriPermission.getUri", e)))?
                        .l()?;
                    let s = env
                        .call_method(&uri, "toString", "()Ljava/lang/String;", &[])
                        .map_err(|e| Fatal(jni_err(env, "Uri.toString", e)))?
                        .l()?;
                    super::jstring_owned(env, s.into()).map_err(Fatal)
                })?;
                if super::uri::normalize_tree_uri(&s).is_some() {
                    out.push(s);
                }
            }
            Ok(out)
        })
        .map_err(|f| f.0)
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn persisted_tree_uris() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}
