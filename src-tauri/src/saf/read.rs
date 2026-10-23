#[cfg(target_os = "android")]
use super::jni_err;

/// Provider-controlled names can path-traverse (`../../db/x.db`): strip separators, reject dot-only names.
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

/// Query can throw (revoked grant): every error path clears via `jni_err` and returns None.
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
