#[cfg(target_os = "android")]
use super::jni_err;

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

        // Plain "w" may not truncate (Drive keeps tail bytes); some providers reject "wt", hence the fallback.
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

/// Removes the 0-byte placeholder the save dialog pre-creates when a download is canceled or fails.
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
