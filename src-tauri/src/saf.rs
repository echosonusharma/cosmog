//! Android SAF (Storage Access Framework) helpers.
//!
//! `saveDialog` on Android returns a `content://` URI where the OS has already
//! pre-created a 0-byte placeholder file. The S3 downloader writes to an
//! absolute filesystem path in the app cache; this module copies those bytes
//! into the SAF URI via ContentResolver.openOutputStream, streaming in chunks
//! so multi-GB downloads never load the whole file into memory.
//!
//! JNI safety: when a Java method throws, jni-rs returns `Err(JavaException)`
//! but the exception STAYS PENDING on the thread. Calling almost any other
//! JNI function (including detaching the thread on drop of the AttachGuard)
//! with a pending exception aborts the process with a JNI error. Every
//! fallible JNI call below therefore routes its error through `jni_err`,
//! which clears the pending exception before the error propagates.

/// Clear any pending Java exception and format the JNI error. Must be applied
/// to every fallible JNI call before the error can propagate or another JNI
/// call is made.
#[cfg(target_os = "android")]
fn jni_err(env: &mut jni::JNIEnv, what: &str, e: jni::errors::Error) -> String {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    format!("{what}: {e}")
}

/// Convert a Java `String` to an owned Rust `String`. Taking `s` by value keeps
/// it alive for the whole body so the borrowed `JavaStr` never outlives it (the
/// inline `get_string(&s)?.into()` form trips NLL as a block-tail expression).
#[cfg(target_os = "android")]
fn jstring_owned(env: &mut jni::JNIEnv, s: jni::objects::JString) -> Result<String, String> {
    match env.get_string(&s) {
        Ok(js) => Ok(js.into()),
        Err(e) => Err(jni_err(env, "get_string", e)),
    }
}

/// SAF display names come from arbitrary DocumentsProviders; a malicious one
/// can return `../../databases/x.db` and walk out of the staging directory.
/// Strip path separators and reject dot-only names.
#[cfg(target_os = "android")]
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "upload".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(target_os = "android")]
pub async fn finalize_saf_download(cache_path: String, uri: String) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || -> Result<u64, String> {
        use jni::objects::{JObject, JString, JValue};
        use jni::JavaVM;
        use std::io::Read;

        const CHUNK: usize = 1024 * 1024;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        let uri_jstr: JString = env
            .new_string(&uri)
            .map_err(|e| jni_err(&mut env, "new_string(uri)", e))?;
        let uri_class = env
            .find_class("android/net/Uri")
            .map_err(|e| jni_err(&mut env, "find_class(Uri)", e))?;
        let uri_obj = env
            .call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri_jstr))],
            )
            .map_err(|e| jni_err(&mut env, "Uri.parse", e))?
            .l()
            .map_err(|e| format!("Uri.parse.l: {e}"))?;

        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| jni_err(&mut env, "getContentResolver", e))?
            .l()
            .map_err(|e| format!("getContentResolver.l: {e}"))?;

        // "wt" (write + truncate) is the reliable way to replace content;
        // plain "w" truncation is provider-dependent (Google Drive keeps tail
        // bytes when the new content is shorter). Some providers reject "wt"
        // with IllegalArgumentException, so fall back to "w".
        let mut out_stream = JObject::null();
        for mode in ["wt", "w"] {
            let mode_jstr: JString = env
                .new_string(mode)
                .map_err(|e| jni_err(&mut env, "new_string(mode)", e))?;
            match env.call_method(
                &resolver,
                "openOutputStream",
                "(Landroid/net/Uri;Ljava/lang/String;)Ljava/io/OutputStream;",
                &[
                    JValue::Object(&uri_obj),
                    JValue::Object(&JObject::from(mode_jstr)),
                ],
            ) {
                Ok(v) => {
                    out_stream = v.l().map_err(|e| format!("openOutputStream.l: {e}"))?;
                    break;
                }
                Err(e) => {
                    let msg = jni_err(&mut env, "openOutputStream", e);
                    if mode == "w" {
                        return Err(msg);
                    }
                }
            }
        }

        if out_stream.is_null() {
            return Err("openOutputStream returned null".into());
        }

        // Pre-allocate one reusable byte[] on the Java side to avoid a per-chunk
        // allocation across the JNI boundary.
        let jbuf = env
            .new_byte_array(CHUNK as i32)
            .map_err(|e| jni_err(&mut env, "new_byte_array", e))?;

        let mut file = std::fs::File::open(&cache_path)
            .map_err(|e| format!("open cache_path: {e}"))?;
        let mut buf = vec![0u8; CHUNK];
        let mut total: u64 = 0;
        let mut copy_result: Result<(), String> = Ok(());

        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    copy_result = Err(format!("read: {e}"));
                    break;
                }
            };
            let signed: &[i8] =
                unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const i8, n) };
            if let Err(e) = env.set_byte_array_region(&jbuf, 0, signed) {
                copy_result = Err(jni_err(&mut env, "set_byte_array_region", e));
                break;
            }
            let write_res = env.call_method(
                &out_stream,
                "write",
                "([BII)V",
                &[
                    JValue::Object(&jbuf),
                    JValue::Int(0),
                    JValue::Int(n as i32),
                ],
            );
            if let Err(e) = write_res {
                copy_result = Err(jni_err(&mut env, "OutputStream.write", e));
                break;
            }
            total += n as u64;
        }

        // Always attempt to flush + close before returning. These may throw
        // too (e.g. deferred disk-full errors surface on close); clear so the
        // thread detaches cleanly.
        if let Err(e) = env.call_method(&out_stream, "flush", "()V", &[]) {
            let msg = jni_err(&mut env, "OutputStream.flush", e);
            if copy_result.is_ok() {
                copy_result = Err(msg);
            }
        }
        if let Err(e) = env.call_method(&out_stream, "close", "()V", &[]) {
            let msg = jni_err(&mut env, "OutputStream.close", e);
            if copy_result.is_ok() {
                copy_result = Err(msg);
            }
        }

        copy_result?;
        let _ = std::fs::remove_file(&cache_path);
        Ok(total)
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn finalize_saf_download(_cache_path: String, _uri: String) -> Result<u64, String> {
    Err("SAF finalize is Android-only".into())
}

/// Delete the SAF document at `uri`. The save dialog pre-creates a 0-byte
/// placeholder file the moment the user picks a location; when the download
/// is canceled or fails before finalize, that placeholder must be removed or
/// the user finds an empty file at their chosen destination.
#[cfg(target_os = "android")]
pub async fn delete_saf_document(uri: String) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || -> Result<bool, String> {
        use jni::objects::{JObject, JString, JValue};
        use jni::JavaVM;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        let uri_jstr: JString = env
            .new_string(&uri)
            .map_err(|e| jni_err(&mut env, "new_string(uri)", e))?;
        let uri_class = env
            .find_class("android/net/Uri")
            .map_err(|e| jni_err(&mut env, "find_class(Uri)", e))?;
        let uri_obj = env
            .call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri_jstr))],
            )
            .map_err(|e| jni_err(&mut env, "Uri.parse", e))?
            .l()
            .map_err(|e| format!("Uri.parse.l: {e}"))?;

        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| jni_err(&mut env, "getContentResolver", e))?
            .l()
            .map_err(|e| format!("getContentResolver.l: {e}"))?;

        let dc_class = env
            .find_class("android/provider/DocumentsContract")
            .map_err(|e| jni_err(&mut env, "find_class(DocumentsContract)", e))?;
        let res = env.call_static_method(
            dc_class,
            "deleteDocument",
            "(Landroid/content/ContentResolver;Landroid/net/Uri;)Z",
            &[JValue::Object(&resolver), JValue::Object(&uri_obj)],
        );

        // deleteDocument throws FileNotFoundException when the document is
        // already gone; treat that as "nothing to delete", not an error.
        let deleted = match res {
            Ok(v) => v.z().unwrap_or(false),
            Err(e) => {
                let _ = jni_err(&mut env, "deleteDocument", e);
                false
            }
        };

        Ok(deleted)
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn delete_saf_document(_uri: String) -> Result<bool, String> {
    Err("SAF delete is Android-only".into())
}

/// Result of staging a SAF upload: `path` is the absolute filesystem path the
/// caller can hand to Rust's upload path, `display_name` is the human filename
/// resolved from ContentResolver's OpenableColumns.DISPLAY_NAME.
#[derive(serde::Serialize)]
pub struct SafStagedUpload {
    pub path: String,
    pub display_name: String,
    pub bytes: u64,
}

#[cfg(target_os = "android")]
pub async fn stage_saf_upload(uri: String, dest_dir: String) -> Result<SafStagedUpload, String> {
    tokio::task::spawn_blocking(move || -> Result<SafStagedUpload, String> {
        use jni::objects::{JObject, JString, JValue};
        use jni::JavaVM;
        use std::io::Write;

        const CHUNK: usize = 1024 * 1024;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        let uri_jstr: JString = env
            .new_string(&uri)
            .map_err(|e| jni_err(&mut env, "new_string(uri)", e))?;
        let uri_class = env
            .find_class("android/net/Uri")
            .map_err(|e| jni_err(&mut env, "find_class(Uri)", e))?;
        let uri_obj = env
            .call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri_jstr))],
            )
            .map_err(|e| jni_err(&mut env, "Uri.parse", e))?
            .l()
            .map_err(|e| format!("Uri.parse.l: {e}"))?;

        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| jni_err(&mut env, "getContentResolver", e))?
            .l()
            .map_err(|e| format!("getContentResolver.l: {e}"))?;

        // Query OpenableColumns.DISPLAY_NAME for the human filename.
        let display_name = sanitize_file_name(
            &query_display_name(&mut env, &resolver, &uri_obj).unwrap_or_else(|| "upload".into()),
        );

        // openInputStream(uri)
        let in_stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(&uri_obj)],
            )
            .map_err(|e| jni_err(&mut env, "openInputStream", e))?
            .l()
            .map_err(|e| format!("openInputStream.l: {e}"))?;
        if in_stream.is_null() {
            return Err("openInputStream returned null".into());
        }

        // Stage each upload under a per-call subdir so files never collide on
        // display_name alone and no timestamp leaks into the cached filename.
        let subdir = uuid::Uuid::new_v4().simple().to_string();
        let dest_subdir = std::path::Path::new(&dest_dir).join(subdir);
        std::fs::create_dir_all(&dest_subdir).map_err(|e| format!("mkdir dest_subdir: {e}"))?;
        let dest_path = dest_subdir.join(&display_name);
        let mut file = std::fs::File::create(&dest_path)
            .map_err(|e| format!("create dest_path: {e}"))?;

        let jbuf = env
            .new_byte_array(CHUNK as i32)
            .map_err(|e| jni_err(&mut env, "new_byte_array", e))?;
        let mut buf = vec![0u8; CHUNK];
        let mut total: u64 = 0;
        let mut copy_err: Option<String> = None;

        loop {
            let read_res = env.call_method(
                &in_stream,
                "read",
                "([B)I",
                &[JValue::Object(&jbuf)],
            );
            let n = match read_res {
                Ok(v) => v.i().unwrap_or(-1),
                Err(e) => {
                    copy_err = Some(jni_err(&mut env, "InputStream.read", e));
                    break;
                }
            };
            if n <= 0 { break; }
            let signed: &mut [i8] = unsafe {
                std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, n as usize)
            };
            if let Err(e) = env.get_byte_array_region(&jbuf, 0, signed) {
                copy_err = Some(jni_err(&mut env, "get_byte_array_region", e));
                break;
            }
            if let Err(e) = file.write_all(&buf[..n as usize]) {
                copy_err = Some(format!("file write: {e}"));
                break;
            }
            total += n as u64;
        }

        if let Err(e) = env.call_method(&in_stream, "close", "()V", &[]) {
            let _ = jni_err(&mut env, "InputStream.close", e);
        }

        if let Some(e) = copy_err {
            let _ = std::fs::remove_file(&dest_path);
            let _ = std::fs::remove_dir(&dest_subdir);
            return Err(e);
        }

        Ok(SafStagedUpload {
            path: dest_path.to_string_lossy().to_string(),
            display_name,
            bytes: total,
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn stage_saf_upload(_uri: String, _dest_dir: String) -> Result<SafStagedUpload, String> {
    Err("SAF stage is Android-only".into())
}

/// Start (or stop) the Android foreground TransferService so the OS keeps our
/// process alive while uploads/downloads are in flight. Without this, Doze
/// mode / cached-process reap kills long transfers and they restart from 0.
#[cfg(target_os = "android")]
pub fn set_transfer_service(active: bool) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err("android context not initialized".into());
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };

    let cls = env
        .find_class("com/sonus/cosmog/TransferService")
        .map_err(|e| jni_err(&mut env, "find_class(TransferService)", e))?;
    let method = if active { "start" } else { "stop" };
    env.call_static_method(
        cls,
        method,
        "(Landroid/content/Context;)V",
        &[JValue::Object(&context)],
    )
    .map_err(|e| jni_err(&mut env, method, e))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn set_transfer_service(_active: bool) -> Result<(), String> {
    Ok(())
}

/// Query `OpenableColumns.DISPLAY_NAME`. `ContentResolver.query` can throw
/// (SecurityException on a revoked grant), so every JNI error path clears the
/// pending exception via `jni_err` before returning None.
#[cfg(target_os = "android")]
fn query_display_name(
    env: &mut jni::JNIEnv,
    resolver: &jni::objects::JObject,
    uri_obj: &jni::objects::JObject,
) -> Option<String> {
    use jni::objects::{JObject, JObjectArray, JString, JValue};

    fn ok_or_clear<T>(env: &mut jni::JNIEnv, r: Result<T, jni::errors::Error>, what: &str) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                let _ = jni_err(env, what, e);
                None
            }
        }
    }

    let col_name: JString = {
        let r = env.new_string("_display_name");
        ok_or_clear(env, r, "new_string(col)")?
    };
    let projection: JObjectArray = {
        let r = env.new_object_array(1, "java/lang/String", JObject::null());
        ok_or_clear(env, r, "new_object_array")?
    };
    {
        let r = env.set_object_array_element(&projection, 0, &JObject::from(col_name));
        ok_or_clear(env, r, "set_object_array_element")?;
    }

    let cursor_obj = {
        let r = env.call_method(
            resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[
                JValue::Object(uri_obj),
                JValue::Object(&projection),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        );
        ok_or_clear(env, r, "ContentResolver.query")?.l().ok()?
    };
    if cursor_obj.is_null() {
        return None;
    }

    let mut result: Option<String> = None;
    let has_row = {
        let r = env.call_method(&cursor_obj, "moveToFirst", "()Z", &[]);
        ok_or_clear(env, r, "Cursor.moveToFirst")
            .and_then(|v| v.z().ok())
            .unwrap_or(false)
    };

    if has_row {
        let col_idx_arg: Option<JString> = {
            let r = env.new_string("_display_name");
            ok_or_clear(env, r, "new_string(col_idx)")
        };
        if let Some(col_idx_arg) = col_idx_arg {
            let idx = {
                let r = env.call_method(
                    &cursor_obj,
                    "getColumnIndex",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&JObject::from(col_idx_arg))],
                );
                ok_or_clear(env, r, "Cursor.getColumnIndex")
                    .and_then(|v| v.i().ok())
                    .unwrap_or(-1)
            };
            if idx >= 0 {
                let v = {
                    let r = env.call_method(
                        &cursor_obj,
                        "getString",
                        "(I)Ljava/lang/String;",
                        &[JValue::Int(idx)],
                    );
                    ok_or_clear(env, r, "Cursor.getString")
                };
                if let Some(v) = v {
                    if let Ok(s_obj) = v.l() {
                        if !s_obj.is_null() {
                            let s: JString = s_obj.into();
                            result = env.get_string(&s).ok().map(|js| js.into());
                        }
                    }
                }
            }
        }
    }

    if let Err(e) = env.call_method(&cursor_obj, "close", "()V", &[]) {
        let _ = jni_err(env, "Cursor.close", e);
    }
    result
}

// ---------------------------------------------------------------------------
// Night Watcher SAF/JNI helpers.
//
// These back the "Night Watcher" background folder-sync feature. They talk to
// the Kotlin `com/sonus/cosmog/NightWatchService` foreground service and the
// `com/sonus/cosmog/NwTreePicker` (a Kotlin `object` whose `launch`/`poll`/`reset`
// are `@JvmStatic`, invoked as static methods). Same jni-exception-clearing discipline as above:
// every fallible JNI call routes through `jni_err`.
// ---------------------------------------------------------------------------

// Cached app-class GlobalRefs. find_class for app classes FAILS from a native
// (spawn_blocking) thread because it uses the system ClassLoader, not the app
// one. Same pattern as secrets.rs SECRET_STORE_CLASS: cache on the JVM thread
// during nw init, then reuse everywhere. Framework classes (android/net/Uri,
// DocumentsContract) still resolve fine via find_class from any thread.
#[cfg(target_os = "android")]
pub(crate) static NIGHTWATCH_SERVICE_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();
#[cfg(target_os = "android")]
pub(crate) static NW_TREE_PICKER_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();

/// Cache the Night Watcher app-class GlobalRefs. Called from
/// `Java_com_sonus_cosmog_CosmogApp_initNwClasses` on the main/JVM thread (see
/// CosmogApp.onCreate + MainActivity.onCreate), where find_class uses the app
/// ClassLoader. Idempotent: OnceLock.set is a no-op once populated, so a second
/// call from MainActivity after the Application init does not re-cache or leak.
#[cfg(target_os = "android")]
fn cache_nw_class(env: &mut jni::JNIEnv, name: &str, slot: &std::sync::OnceLock<jni::objects::GlobalRef>) {
    if slot.get().is_some() {
        return;
    }
    match env.find_class(name) {
        Ok(cls) => match env.new_global_ref(cls) {
            Ok(g) => {
                let _ = slot.set(g);
            }
            Err(e) => {
                let _ = jni_err(env, "new_global_ref(nw class)", e);
            }
        },
        Err(e) => {
            let _ = jni_err(env, "find_class(nw class)", e);
        }
    }
}

/// JNI entrypoint invoked from CosmogApp/MainActivity on the JVM thread to cache
/// the NW app classes as GlobalRefs (bug #3). Idempotent (bug: init not
/// idempotent) via the OnceLock guards in `cache_nw_class`.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_CosmogApp_initNwClasses(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
) {
    cache_nw_class(&mut env, "com/sonus/cosmog/NightWatchService", &NIGHTWATCH_SERVICE_CLASS);
    cache_nw_class(&mut env, "com/sonus/cosmog/NwTreePicker", &NW_TREE_PICKER_CLASS);
}

/// A picked SAF tree: `uri` is the persisted tree `content://` URI,
/// `display_name` is the human folder name for UI.
#[derive(serde::Serialize)]
pub struct SafTree {
    pub uri: String,
    pub display_name: String,
}

/// One entry found while walking a SAF tree. `rel_path` is the path relative to
/// the tree root (forward-slash separated, no leading slash). `doc_uri` is a
/// document `content://` URI usable with ContentResolver.openInputStream.
/// `mtime` is in SECONDS (DocumentsContract reports COLUMN_LAST_MODIFIED in ms).
#[derive(serde::Serialize)]
pub struct SafEntry {
    pub rel_path: String,
    pub doc_uri: String,
    pub size: i64,
    pub mtime: i64,
    pub is_dir: bool,
}

/// Start (or stop) the NightWatchService foreground service. Verbatim shape of
/// `set_transfer_service` for a different class.
#[cfg(target_os = "android")]
pub fn set_nightwatch_service(active: bool) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err("android context not initialized".into());
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };

    // Use the cached class ref: find_class for app classes fails from this
    // native thread (bug #3).
    let cls_ref = NIGHTWATCH_SERVICE_CLASS
        .get()
        .ok_or("NightWatchService class not cached (initNwClasses not called)")?;
    let cls: &jni::objects::JClass = cls_ref.as_obj().into();
    let method = if active { "start" } else { "stop" };
    env.call_static_method(
        cls,
        method,
        "(Landroid/content/Context;)V",
        &[JValue::Object(&context)],
    )
    .map_err(|e| jni_err(&mut env, method, e))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn set_nightwatch_service(_active: bool) -> Result<(), String> {
    Ok(())
}

/// Push the NightWatchService CPU wakelock cap forward from the headless sync
/// loop (same `:nightwatch` process). Called periodically so a sync spanning
/// many cycles, or a single long transfer, never loses the CPU when the bounded
/// acquire from the previous heartbeat lapses. Calls the static
/// `NightWatchService.heartbeatWakelock()`.
#[cfg(target_os = "android")]
pub fn nw_wakelock_heartbeat() -> Result<(), String> {
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() {
        return Err("android context not initialized".into());
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;

    // Cached class ref (bug #3): find_class for app classes fails on this thread.
    let cls_ref = NIGHTWATCH_SERVICE_CLASS
        .get()
        .ok_or("NightWatchService class not cached (initNwClasses not called)")?;
    let cls: &jni::objects::JClass = cls_ref.as_obj().into();
    env.call_static_method(cls, "heartbeatWakelock", "()V", &[])
        .map_err(|e| jni_err(&mut env, "heartbeatWakelock", e))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn nw_wakelock_heartbeat() -> Result<(), String> {
    Ok(())
}

/// Persist the "start Night Watcher on boot" flag on the Kotlin side. Calls
/// NightWatchService.setBootFlag(context, enabled).
#[cfg(target_os = "android")]
pub fn set_nightwatch_boot_flag(enabled: bool) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err("android context not initialized".into());
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };

    // Cached class ref (bug #3): find_class for app classes fails on this thread.
    let cls_ref = NIGHTWATCH_SERVICE_CLASS
        .get()
        .ok_or("NightWatchService class not cached (initNwClasses not called)")?;
    let cls: &jni::objects::JClass = cls_ref.as_obj().into();
    env.call_static_method(
        cls,
        "setBootFlag",
        "(Landroid/content/Context;Z)V",
        &[JValue::Object(&context), JValue::Bool(enabled as u8)],
    )
    .map_err(|e| jni_err(&mut env, "setBootFlag", e))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn set_nightwatch_boot_flag(_enabled: bool) -> Result<(), String> {
    Ok(())
}

/// The cached `NwTreePicker` class ref. `launch`/`poll`/`reset` are `@JvmStatic`,
/// so they are invoked as static methods on this class (not instance methods on
/// INSTANCE). find_class for app classes fails on the native spawn_blocking
/// thread these calls run on, hence the cached GlobalRef (bug #3).
#[cfg(target_os = "android")]
fn nw_picker_class() -> Result<&'static jni::objects::GlobalRef, String> {
    NW_TREE_PICKER_CLASS
        .get()
        .ok_or_else(|| "NwTreePicker class not cached (initNwClasses not called)".to_string())
}

/// Launch the system tree picker (ACTION_OPEN_DOCUMENT_TREE) and poll for the
/// result.
///
/// Sentinel scheme (KOTLIN MUST ALIGN):
/// - `NwTreePicker.poll()` returns `null` while the pick is still pending.
/// - On success it returns `"<treeUri>\n<displayName>"` (result and name joined
///   by a single '\n'; only the FIRST '\n' is treated as the separator, so a
///   name may itself contain newlines).
/// - On user cancel it returns the literal string `"__NW_CANCELED__"`, which
///   cannot collide with a real tree URI (those start with a scheme like
///   `content://`). This maps to `Err("canceled")`.
/// - If nothing arrives within ~120s we give up with `Err("tree pick timed out")`.
#[cfg(target_os = "android")]
pub async fn nw_pick_tree() -> Result<SafTree, String> {
    tokio::task::spawn_blocking(move || -> Result<SafTree, String> {
        use jni::JavaVM;

        const POLL_INTERVAL_MS: u64 = 250;
        const MAX_POLLS: u32 = 480; // ~120s at 250ms.
        const CANCEL_SENTINEL: &str = "__NW_CANCELED__";

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;

        // reset() clears any stale result from a previous pick.
        {
            let cls_ref = nw_picker_class()?;
            let cls: &jni::objects::JClass = cls_ref.as_obj().into();
            env.call_static_method(cls, "reset", "()V", &[])
                .map_err(|e| jni_err(&mut env, "NwTreePicker.reset", e))?;
        }
        // launch() fires the ACTION_OPEN_DOCUMENT_TREE intent.
        {
            let cls_ref = nw_picker_class()?;
            let cls: &jni::objects::JClass = cls_ref.as_obj().into();
            env.call_static_method(cls, "launch", "()V", &[])
                .map_err(|e| jni_err(&mut env, "NwTreePicker.launch", e))?;
        }

        // Poll on this single attached thread. std::thread::sleep is fine here
        // because we are inside spawn_blocking.
        let mut polls = 0u32;
        loop {
            let result: Option<String> = {
                let cls_ref = nw_picker_class()?;
                let cls: &jni::objects::JClass = cls_ref.as_obj().into();
                let v = env
                    .call_static_method(cls, "poll", "()Ljava/lang/String;", &[])
                    .map_err(|e| jni_err(&mut env, "NwTreePicker.poll", e))?
                    .l()
                    .map_err(|e| format!("poll.l: {e}"))?;
                if v.is_null() {
                    None
                } else {
                    Some(jstring_owned(&mut env, v.into())?)
                }
            };

            if let Some(s) = result {
                if s == CANCEL_SENTINEL {
                    return Err("canceled".into());
                }
                let (uri, display_name) = match s.split_once('\n') {
                    Some((u, n)) => (u.to_string(), n.to_string()),
                    None => (s.clone(), String::new()),
                };
                return Ok(SafTree { uri, display_name });
            }

            polls += 1;
            if polls >= MAX_POLLS {
                return Err("tree pick timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
        }
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn nw_pick_tree() -> Result<SafTree, String> {
    Err("tree pick is Android-only".into())
}

/// Recursively walk a SAF tree via DocumentsContract, returning every file
/// (is_dir = false). The walk runs inside a single spawn_blocking on one
/// attached thread, using an explicit stack (not async recursion) so the whole
/// traversal shares one JNIEnv attach.
#[cfg(target_os = "android")]
pub async fn collect_tree_files(tree_uri: String) -> Result<(Vec<SafEntry>, u64), String> {
    tokio::task::spawn_blocking(move || -> Result<(Vec<SafEntry>, u64), String> {
        use jni::objects::{JObject, JObjectArray, JString, JValue};
        use jni::JavaVM;

        const MIME_DIR: &str = "vnd.android.document/directory";

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| jni_err(&mut env, "getContentResolver", e))?
            .l()
            .map_err(|e| format!("getContentResolver.l: {e}"))?;

        // Parse the tree URI string into an android.net.Uri.
        let tree_uri_obj = {
            let uri_jstr: JString = env
                .new_string(&tree_uri)
                .map_err(|e| jni_err(&mut env, "new_string(tree_uri)", e))?;
            let uri_class = env
                .find_class("android/net/Uri")
                .map_err(|e| jni_err(&mut env, "find_class(Uri)", e))?;
            env.call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri_jstr))],
            )
            .map_err(|e| jni_err(&mut env, "Uri.parse", e))?
            .l()
            .map_err(|e| format!("Uri.parse.l: {e}"))?
        };

        // Hoist DocumentsContract find_class out of the walk loop (bug #2): one
        // lookup reused for every row instead of leaking a local ref per row.
        let dc_class = env
            .find_class("android/provider/DocumentsContract")
            .map_err(|e| jni_err(&mut env, "find_class(DocumentsContract)", e))?;

        // getTreeDocumentId(treeUri) -> root document id.
        let root_doc_id: String = {
            let s_obj = env
                .call_static_method(
                    &dc_class,
                    "getTreeDocumentId",
                    "(Landroid/net/Uri;)Ljava/lang/String;",
                    &[JValue::Object(&tree_uri_obj)],
                )
                .map_err(|e| jni_err(&mut env, "getTreeDocumentId", e))?
                .l()
                .map_err(|e| format!("getTreeDocumentId.l: {e}"))?;
            if s_obj.is_null() {
                return Err("getTreeDocumentId returned null".into());
            }
            jstring_owned(&mut env, s_obj.into())?
        };

        // Projection reused for every children query.
        let projection: JObjectArray = {
            let arr = env
                .new_object_array(5, "java/lang/String", JObject::null())
                .map_err(|e| jni_err(&mut env, "new_object_array(projection)", e))?;
            let cols = [
                "document_id",   // COLUMN_DOCUMENT_ID
                "_display_name", // COLUMN_DISPLAY_NAME
                "mime_type",     // COLUMN_MIME_TYPE
                "_size",         // COLUMN_SIZE
                "last_modified", // COLUMN_LAST_MODIFIED
            ];
            for (i, c) in cols.iter().enumerate() {
                let s: JString = env
                    .new_string(c)
                    .map_err(|e| jni_err(&mut env, "new_string(col)", e))?;
                env.set_object_array_element(&arr, i as i32, &JObject::from(s))
                    .map_err(|e| jni_err(&mut env, "set_object_array_element", e))?;
            }
            arr
        };

        let mut out: Vec<SafEntry> = Vec::new();
        // Explicit DFS stack: (parent doc id, rel_path prefix for this dir).
        let mut stack: Vec<(String, String)> = vec![(root_doc_id, String::new())];

        // Fatal error surfaced from inside a local frame. The frame closure
        // returns `Result<_, jni::errors::Error>` (its E: From<Error> bound
        // rules out String), so fatal String errors are stashed here and the
        // outer loop breaks after the frame pops cleanly.
        let mut fatal: Option<String> = None;

        // Count of subdirs we could not fully read (unreadable/null cursor,
        // truncated iteration). A non-zero count means enumeration is partial,
        // so the caller must skip its mark-and-sweep to avoid pruning state for
        // files that still exist under an unreadable subtree.
        let mut read_errors = 0u64;

        while let Some((parent_doc_id, prefix)) = stack.pop() {
            // Bug #2: every row would leak JNI local refs (strings, cursors,
            // uris) into a table capped at ~512, aborting the process on any
            // real folder. Process each directory inside its own local frame so
            // all those refs are freed when the frame pops. Owned Rust data
            // (SafEntry, stack entries) escapes the frame just fine. Capacity is
            // generous: refs churn within the frame, they do not accumulate.
            let frame_res: Result<(), jni::errors::Error> = env.with_local_frame(64, |env| {
                // buildChildDocumentsUriUsingTree(treeUri, parentDocId).
                let parent_jstr: JString = match env.new_string(&parent_doc_id) {
                    Ok(s) => s,
                    Err(e) => {
                        fatal = Some(jni_err(env, "new_string(parentDocId)", e));
                        return Ok(());
                    }
                };
                let children_uri = match env.call_static_method(
                    &dc_class,
                    "buildChildDocumentsUriUsingTree",
                    "(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;",
                    &[
                        JValue::Object(&tree_uri_obj),
                        JValue::Object(&JObject::from(parent_jstr)),
                    ],
                ) {
                    Ok(v) => match v.l() {
                        Ok(o) => o,
                        Err(e) => {
                            fatal = Some(format!("buildChildDocumentsUriUsingTree.l: {e}"));
                            return Ok(());
                        }
                    },
                    Err(e) => {
                        fatal = Some(jni_err(env, "buildChildDocumentsUriUsingTree", e));
                        return Ok(());
                    }
                };

                // resolver.query(childrenUri, projection, null, null, null).
                let cursor = {
                    let r = env.call_method(
                        &resolver,
                        "query",
                        "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                        &[
                            JValue::Object(&children_uri),
                            JValue::Object(&projection),
                            JValue::Object(&JObject::null()),
                            JValue::Object(&JObject::null()),
                            JValue::Object(&JObject::null()),
                        ],
                    );
                    match r {
                        Ok(v) => match v.l() {
                            Ok(o) => o,
                            Err(_) => {
                                read_errors += 1;
                                return Ok(());
                            }
                        },
                        Err(e) => {
                            // A single unreadable dir should not abort the walk,
                            // but it makes enumeration partial: count it so the
                            // caller skips the sweep.
                            let _ = jni_err(env, "ContentResolver.query(children)", e);
                            read_errors += 1;
                            return Ok(());
                        }
                    }
                };
                if cursor.is_null() {
                    read_errors += 1;
                    return Ok(());
                }

                // Iterate rows. Column order matches `projection` (0..=4). Each
                // row runs in its own nested frame so a directory with thousands
                // of files never fills the outer frame (bug #2): all per-row
                // refs (id/name/mime strings, the built doc uri) are freed per
                // iteration. `break_out` propagates a fatal error out of the
                // frame closure (which returns Ok to pop the frame cleanly).
                loop {
                    let has_next = match env.call_method(&cursor, "moveToNext", "()Z", &[]) {
                        Ok(v) => v.z().unwrap_or(false),
                        Err(e) => {
                            // Iteration broke early: this dir's listing is
                            // truncated, so treat it as a partial read.
                            let _ = jni_err(env, "Cursor.moveToNext", e);
                            read_errors += 1;
                            false
                        }
                    };
                    if !has_next {
                        break;
                    }

                    let mut break_out = false;
                    let row_res: Result<(), jni::errors::Error> = env.with_local_frame(16, |env| {
                        let doc_id = cursor_get_string(env, &cursor, 0).unwrap_or_default();
                        let name = cursor_get_string(env, &cursor, 1).unwrap_or_default();
                        let mime = cursor_get_string(env, &cursor, 2).unwrap_or_default();
                        // Null size/mtime fall back to -1 / 0.
                        let size = cursor_get_long(env, &cursor, 3).unwrap_or(-1);
                        let mtime_ms = cursor_get_long(env, &cursor, 4).unwrap_or(0);
                        let mtime = mtime_ms / 1000; // ms -> seconds.

                        if doc_id.is_empty() {
                            return Ok(());
                        }
                        let rel_path = if prefix.is_empty() {
                            name.clone()
                        } else {
                            format!("{prefix}/{name}")
                        };

                        if mime == MIME_DIR {
                            // Recurse via the explicit stack (owned data escapes).
                            stack.push((doc_id, rel_path));
                            return Ok(());
                        }

                        // buildDocumentUriUsingTree(treeUri, childDocId).
                        let id_jstr: JString = match env.new_string(&doc_id) {
                            Ok(s) => s,
                            Err(e) => {
                                fatal = Some(jni_err(env, "new_string(childDocId)", e));
                                break_out = true;
                                return Ok(());
                            }
                        };
                        let uri_obj = match env.call_static_method(
                            &dc_class,
                            "buildDocumentUriUsingTree",
                            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;",
                            &[
                                JValue::Object(&tree_uri_obj),
                                JValue::Object(&JObject::from(id_jstr)),
                            ],
                        ) {
                            Ok(v) => match v.l() {
                                Ok(o) => o,
                                Err(e) => {
                                    fatal = Some(format!("buildDocumentUriUsingTree.l: {e}"));
                                    break_out = true;
                                    return Ok(());
                                }
                            },
                            Err(e) => {
                                fatal = Some(jni_err(env, "buildDocumentUriUsingTree", e));
                                break_out = true;
                                return Ok(());
                            }
                        };
                        let s_obj = match env
                            .call_method(&uri_obj, "toString", "()Ljava/lang/String;", &[])
                        {
                            Ok(v) => match v.l() {
                                Ok(o) => o,
                                Err(e) => {
                                    fatal = Some(format!("Uri.toString.l: {e}"));
                                    break_out = true;
                                    return Ok(());
                                }
                            },
                            Err(e) => {
                                fatal = Some(jni_err(env, "Uri.toString", e));
                                break_out = true;
                                return Ok(());
                            }
                        };
                        let doc_uri = match jstring_owned(env, s_obj.into()) {
                            Ok(s) => s,
                            Err(e) => {
                                fatal = Some(e);
                                break_out = true;
                                return Ok(());
                            }
                        };

                        out.push(SafEntry {
                            rel_path,
                            doc_uri,
                            size,
                            mtime,
                            is_dir: false,
                        });
                        Ok(())
                    });
                    if let Err(e) = row_res {
                        fatal = Some(jni_err(env, "with_local_frame(row)", e));
                        break;
                    }
                    if break_out {
                        break;
                    }
                }

                if let Err(e) = env.call_method(&cursor, "close", "()V", &[]) {
                    let _ = jni_err(env, "Cursor.close", e);
                }
                Ok(())
            });

            // Frame push/pop itself can fail (OOM allocating the frame).
            if let Err(e) = frame_res {
                return Err(jni_err(&mut env, "with_local_frame(dir)", e));
            }
            if let Some(e) = fatal {
                return Err(e);
            }
        }

        Ok((out, read_errors))
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn collect_tree_files(_tree_uri: String) -> Result<(Vec<SafEntry>, u64), String> {
    Err("SAF tree walk is Android-only".into())
}

/// Read a Cursor string column by index, clearing any pending exception.
/// Returns None on null column or error.
#[cfg(target_os = "android")]
fn cursor_get_string(
    env: &mut jni::JNIEnv,
    cursor: &jni::objects::JObject,
    col: i32,
) -> Option<String> {
    use jni::objects::JValue;

    // Guard against null columns; getString on some providers returns null.
    let is_null = match env.call_method(cursor, "isNull", "(I)Z", &[JValue::Int(col)]) {
        Ok(v) => v.z().unwrap_or(true),
        Err(e) => {
            let _ = jni_err(env, "Cursor.isNull", e);
            return None;
        }
    };
    if is_null {
        return None;
    }
    let v = match env.call_method(
        cursor,
        "getString",
        "(I)Ljava/lang/String;",
        &[JValue::Int(col)],
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = jni_err(env, "Cursor.getString", e);
            return None;
        }
    };
    let s_obj = v.l().ok()?;
    if s_obj.is_null() {
        return None;
    }
    jstring_owned(env, s_obj.into()).ok()
}

/// Read a Cursor long column by index, clearing any pending exception.
/// Returns None on null column or error.
#[cfg(target_os = "android")]
fn cursor_get_long(
    env: &mut jni::JNIEnv,
    cursor: &jni::objects::JObject,
    col: i32,
) -> Option<i64> {
    use jni::objects::JValue;

    let is_null = match env.call_method(cursor, "isNull", "(I)Z", &[JValue::Int(col)]) {
        Ok(v) => v.z().unwrap_or(true),
        Err(e) => {
            let _ = jni_err(env, "Cursor.isNull", e);
            return None;
        }
    };
    if is_null {
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

/// Stream a SAF document through blake3 and return the lowercase hex digest.
/// Reuses the openInputStream + read-loop shape from `stage_saf_upload`.
#[cfg(target_os = "android")]
pub async fn hash_saf_document(doc_uri: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        use jni::objects::{JObject, JString, JValue};
        use jni::JavaVM;

        const CHUNK: usize = 1024 * 1024;

        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err("android context not initialized".into());
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM::from_raw: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach_current_thread: {e}"))?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        let uri_jstr: JString = env
            .new_string(&doc_uri)
            .map_err(|e| jni_err(&mut env, "new_string(uri)", e))?;
        let uri_class = env
            .find_class("android/net/Uri")
            .map_err(|e| jni_err(&mut env, "find_class(Uri)", e))?;
        let uri_obj = env
            .call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri_jstr))],
            )
            .map_err(|e| jni_err(&mut env, "Uri.parse", e))?
            .l()
            .map_err(|e| format!("Uri.parse.l: {e}"))?;

        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| jni_err(&mut env, "getContentResolver", e))?
            .l()
            .map_err(|e| format!("getContentResolver.l: {e}"))?;

        let in_stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(&uri_obj)],
            )
            .map_err(|e| jni_err(&mut env, "openInputStream", e))?
            .l()
            .map_err(|e| format!("openInputStream.l: {e}"))?;
        if in_stream.is_null() {
            return Err("openInputStream returned null".into());
        }

        let jbuf = env
            .new_byte_array(CHUNK as i32)
            .map_err(|e| jni_err(&mut env, "new_byte_array", e))?;
        let mut buf = vec![0u8; CHUNK];
        let mut hasher = blake3::Hasher::new();
        let mut hash_err: Option<String> = None;

        loop {
            let read_res = env.call_method(&in_stream, "read", "([B)I", &[JValue::Object(&jbuf)]);
            let n = match read_res {
                Ok(v) => v.i().unwrap_or(-1),
                Err(e) => {
                    hash_err = Some(jni_err(&mut env, "InputStream.read", e));
                    break;
                }
            };
            if n <= 0 {
                break;
            }
            let signed: &mut [i8] =
                unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, n as usize) };
            if let Err(e) = env.get_byte_array_region(&jbuf, 0, signed) {
                hash_err = Some(jni_err(&mut env, "get_byte_array_region", e));
                break;
            }
            hasher.update(&buf[..n as usize]);
        }

        if let Err(e) = env.call_method(&in_stream, "close", "()V", &[]) {
            let _ = jni_err(&mut env, "InputStream.close", e);
        }

        if let Some(e) = hash_err {
            return Err(e);
        }

        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn hash_saf_document(_doc_uri: String) -> Result<String, String> {
    Err("SAF document hashing is Android-only".into())
}
