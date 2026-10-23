#[cfg(target_os = "android")]
use super::{jni_err, jstring_owned};

/// `rel_path` tree-relative, forward slashes, no leading slash; `mtime` epoch ms (0 unknown); `size` -1 if unknown.
#[derive(serde::Serialize)]
pub struct SafEntry {
    pub rel_path: String,
    pub doc_uri: String,
    pub size: i64,
    pub mtime: i64,
    pub is_dir: bool,
}

/// Explicit stack in one spawn_blocking (no async recursion) keeps a single JNIEnv attach.
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
        let mut stack: Vec<(String, String)> = vec![(root_doc_id, String::new())];

        // Frame closures must return jni::errors::Error, so fatal String errors stash here.
        let mut fatal: Option<String> = None;

        // Non-zero means a PARTIAL walk: caller must skip mark-and-sweep or it prunes live files.
        let mut read_errors = 0u64;

        let mut is_root = true;
        while let Some((parent_doc_id, prefix)) = stack.pop() {
            let errors_before = read_errors;
            // Per-row local refs overflow the ~512-entry table and abort the process; a frame per dir frees them on pop.
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

                // Per-row frames keep huge dirs from filling the outer one; `break_out` exits the row loop so the cursor closes and the frame pops before a fatal propagates.
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
                        let mtime = cursor_get_long(env, &cursor, 4).unwrap_or(0);

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
            // Unreadable root (grant revoked, provider gone) must fail the scan, not look empty.
            if is_root && read_errors > errors_before {
                return Err("cannot read watched folder; folder access may have been revoked, pick it again".into());
            }
            is_root = false;
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

/// None on null column or error (exception cleared).
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

/// None on null column or error (exception cleared).
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

/// Call only once no watch uses this tree URI: the persisted read grant is shared per URI.
#[cfg(target_os = "android")]
pub async fn release_tree_permission(tree_uri: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        use jni::objects::{JObject, JString, JValue};
        use jni::JavaVM;

        const FLAG_GRANT_READ_URI_PERMISSION: i32 = 1;

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
            .new_string(&tree_uri)
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
        env.call_method(
            &resolver,
            "releasePersistableUriPermission",
            "(Landroid/net/Uri;I)V",
            &[
                JValue::Object(&uri_obj),
                JValue::Int(FLAG_GRANT_READ_URI_PERMISSION),
            ],
        )
        .map_err(|e| jni_err(&mut env, "releasePersistableUriPermission", e))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(not(target_os = "android"))]
pub async fn release_tree_permission(_tree_uri: String) -> Result<(), String> {
    Ok(())
}
