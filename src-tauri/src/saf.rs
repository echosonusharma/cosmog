//! Android SAF helpers: stream between app-cache files and `content://` URIs via ContentResolver.
//! JNI: a Java exception stays PENDING after Err and further JNI calls abort, so every fallible call clears it via `jni_err`.

/// Clear any pending Java exception and format the error; every fallible JNI
/// call must apply this before propagating or calling JNI again.
#[cfg(target_os = "android")]
fn jni_err(env: &mut jni::JNIEnv, what: &str, e: jni::errors::Error) -> String {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    format!("{what}: {e}")
}

/// Convert a Java `String` to owned Rust. Taking `s` by value keeps it alive so
/// the borrowed `JavaStr` never outlives it (the inline form trips NLL).
#[cfg(target_os = "android")]
fn jstring_owned(env: &mut jni::JNIEnv, s: jni::objects::JString) -> Result<String, String> {
    match env.get_string(&s) {
        Ok(js) => Ok(js.into()),
        Err(e) => Err(jni_err(env, "get_string", e)),
    }
}

/// Provider-controlled names can path-traverse (`../../db/x.db`): strip path
/// separators and reject dot-only names.
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

        // "wt" reliably replaces content; plain "w" truncation is provider-dependent
        // (Drive keeps tail bytes). Some providers reject "wt", hence the fallback.
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

        // flush/close can throw too (deferred disk-full); clear so the thread detaches cleanly.
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

/// Delete the SAF document at `uri`: the save dialog pre-creates a 0-byte
/// placeholder that must be removed when a download is canceled/fails.
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

        // Already-gone documents throw FileNotFoundException: treat as success.
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

        let display_name = sanitize_file_name(
            &query_display_name(&mut env, &resolver, &uri_obj).unwrap_or_else(|| "upload".into()),
        );

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

        // Per-call subdir: no collisions on display_name, no timestamp in cached filenames.
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

/// Start/stop the foreground TransferService so Doze/cached-process reaps don't
/// kill in-flight transfers (which would restart from 0).
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

/// Query `OpenableColumns.DISPLAY_NAME`; query can throw (revoked grant), so
/// every error path clears the pending exception via `jni_err` and returns None.
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

// Night Watcher SAF/JNI helpers: Kotlin NightWatchService + NwTreePicker
// (`launch`/`poll`/`reset` are @JvmStatic). Same jni_err discipline as above.

// App classes must be cached as GlobalRefs: find_class for them FAILS on native
// (spawn_blocking) threads (system vs app ClassLoader). Framework classes resolve anywhere.
#[cfg(target_os = "android")]
pub(crate) static NIGHTWATCH_SERVICE_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();
#[cfg(target_os = "android")]
pub(crate) static NW_TREE_PICKER_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();

/// Cache NW app-class GlobalRefs from the JVM thread (find_class works there);
/// idempotent via OnceLock.
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

/// JNI entrypoint (CosmogApp/MainActivity, JVM thread) caching NW classes as
/// GlobalRefs; idempotent via OnceLock.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_CosmogApp_initNwClasses(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
) {
    cache_nw_class(&mut env, "com/sonus/cosmog/NightWatchService", &NIGHTWATCH_SERVICE_CLASS);
    cache_nw_class(&mut env, "com/sonus/cosmog/NwTreePicker", &NW_TREE_PICKER_CLASS);
}

#[derive(serde::Serialize)]
pub struct SafTree {
    pub uri: String,
    pub display_name: String,
}

/// One SAF tree-walk entry: `rel_path` is tree-relative (forward slashes, no
/// leading slash); `mtime` is SECONDS (DocumentsContract reports milliseconds).
#[derive(serde::Serialize)]
pub struct SafEntry {
    pub rel_path: String,
    pub doc_uri: String,
    pub size: i64,
    pub mtime: i64,
    pub is_dir: bool,
}

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

/// Extend the NightWatchService CPU wakelock from the sync loop so long syncs/
/// transfers never lose CPU between bounded acquires.
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

/// Cached NwTreePicker ref; `launch`/`poll`/`reset` are `@JvmStatic` (static calls).
#[cfg(target_os = "android")]
fn nw_picker_class() -> Result<&'static jni::objects::GlobalRef, String> {
    NW_TREE_PICKER_CLASS
        .get()
        .ok_or_else(|| "NwTreePicker class not cached (initNwClasses not called)".to_string())
}

/// Launch the tree picker and poll. KOTLIN MUST ALIGN: poll() gives null while pending,
/// `"<treeUri>\n<name>"` on success (split on FIRST '\n'), `"__NW_CANCELED__"` on cancel, ~120s timeout.
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

        {
            let cls_ref = nw_picker_class()?;
            let cls: &jni::objects::JClass = cls_ref.as_obj().into();
            env.call_static_method(cls, "reset", "()V", &[])
                .map_err(|e| jni_err(&mut env, "NwTreePicker.reset", e))?;
        }
        {
            let cls_ref = nw_picker_class()?;
            let cls: &jni::objects::JClass = cls_ref.as_obj().into();
            env.call_static_method(cls, "launch", "()V", &[])
                .map_err(|e| jni_err(&mut env, "NwTreePicker.launch", e))?;
        }

        // Blocking sleep is OK inside spawn_blocking.
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

/// Walk a SAF tree via DocumentsContract, returning all files. Single
/// spawn_blocking + explicit stack (no async recursion) = one JNIEnv attach.
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

        // Hoisted out of the walk loop: reusing one lookup avoids a leaked local ref per row.
        let dc_class = env
            .find_class("android/provider/DocumentsContract")
            .map_err(|e| jni_err(&mut env, "find_class(DocumentsContract)", e))?;

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

        let projection: JObjectArray = {
            let arr = env
                .new_object_array(5, "java/lang/String", JObject::null())
                .map_err(|e| jni_err(&mut env, "new_object_array(projection)", e))?;
            let cols = [
                "document_id",
                "_display_name",
                "mime_type",
                "_size",
                "last_modified",
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
        // DFS stack: (parent doc id, rel_path prefix).
        let mut stack: Vec<(String, String)> = vec![(root_doc_id, String::new())];

        // Fatal String errors stash here: the frame closure must return
        // Result<_, jni::errors::Error>, whose bounds rule out String.
        let mut fatal: Option<String> = None;

        // Subdirs we couldn't fully read; non-zero means enumeration is PARTIAL
        // and the caller must skip mark-and-sweep to avoid pruning live files.
        let mut read_errors = 0u64;

        while let Some((parent_doc_id, prefix)) = stack.pop() {
            // Bug #2: per-row local refs leak into a ~512-entry table and abort on
            // real folders; each dir gets its own frame so refs free on pop.
            let frame_res: Result<(), jni::errors::Error> = env.with_local_frame(64, |env| {
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

                // Per-row nested frames keep huge dirs from filling the outer frame (bug #2);
                // `break_out` propagates fatals after the frame pops cleanly.
                loop {
                    let has_next = match env.call_method(&cursor, "moveToNext", "()Z", &[]) {
                        Ok(v) => v.z().unwrap_or(false),
                        Err(e) => {
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
                            stack.push((doc_id, rel_path));
                            return Ok(());
                        }

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

/// Read a Cursor string column; None on null column or error (exception cleared).
#[cfg(target_os = "android")]
fn cursor_get_string(
    env: &mut jni::JNIEnv,
    cursor: &jni::objects::JObject,
    col: i32,
) -> Option<String> {
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

/// Read a Cursor long column; None on null column or error (exception cleared).
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
