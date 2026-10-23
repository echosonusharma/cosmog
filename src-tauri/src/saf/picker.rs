#[cfg(target_os = "android")]
use super::{jni_err, jstring_owned, NW_TREE_PICKER_CLASS};

#[derive(serde::Serialize)]
pub struct SafTree {
    pub uri: String,
    pub display_name: String,
}

/// Kotlin `launch`/`poll`/`reset` must stay @JvmStatic: they are called as static methods.
#[cfg(target_os = "android")]
fn nw_picker_class() -> Result<&'static jni::objects::GlobalRef, String> {
    NW_TREE_PICKER_CLASS
        .get()
        .ok_or_else(|| "NwTreePicker class not cached (initNwClasses not called)".to_string())
}

/// Kotlin poll() contract: null while pending, `"<treeUri>\n<name>"` (split on first '\n'),
/// `"__NW_CANCELED__"`, or `"__NW_ERROR__<msg>"`.
#[cfg(target_os = "android")]
pub async fn nw_pick_tree() -> Result<SafTree, String> {
    tokio::task::spawn_blocking(move || -> Result<SafTree, String> {
        use jni::JavaVM;

        const POLL_INTERVAL_MS: u64 = 250;
        const MAX_POLLS: u32 = 480; // ~120s at 250ms.
        const CANCEL_SENTINEL: &str = "__NW_CANCELED__";
        const ERROR_PREFIX: &str = "__NW_ERROR__";

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
                if let Some(msg) = s.strip_prefix(ERROR_PREFIX) {
                    return Err(msg.to_string());
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
