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
