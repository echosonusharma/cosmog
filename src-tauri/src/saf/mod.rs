//! JNI: a Java exception stays PENDING after Err and further JNI calls abort, so every fallible call clears it via `jni_err`.

mod fd;
mod picker;
mod read;
mod service;
mod tree;
pub mod uri;
pub mod walk;
mod write;

pub use fd::open_saf_fd;
pub use picker::{fu_pick_tree, nw_pick_tree, pick_in_progress, PickActive, SafTree};
pub use read::{hash_saf_document, read_saf_bytes, stage_saf_upload, SafStagedUpload};
#[cfg(target_os = "android")]
pub use service::nw_wakelock_idle;
pub use service::{
    nw_can_schedule_exact_alarms, nw_request_exact_alarms, nw_wakelock_heartbeat,
    apply_transfer_service_holds, set_folder_service_hold, set_nightwatch_boot_flag, set_nightwatch_service, set_transfer_service,
};
pub use tree::{collect_tree_files, persisted_tree_uris, release_tree_permission, SafEntry};
pub use write::{delete_saf_document, finalize_saf_download, write_saf_bytes};

/// True for SAF `content://` URIs that std::fs cannot open.
pub fn is_content_uri(s: &str) -> bool {
    s.starts_with("content://")
}

/// `ContentResolver` plus parsed `android.net.Uri` for `uri`.
#[cfg(target_os = "android")]
fn resolver_and_uri<'a>(
    env: &mut jni::JNIEnv<'a>,
    context: &jni::objects::JObject,
    uri: &str,
) -> Result<(jni::objects::JObject<'a>, jni::objects::JObject<'a>), String> {
    use jni::objects::{JObject, JValue};
    let uri_jstr = env.new_string(uri).map_err(|e| jni_err(env, "new_string(uri)", e))?;
    let uri_obj = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&JObject::from(uri_jstr))],
        )
        .map_err(|e| jni_err(env, "Uri.parse", e))?
        .l()
        .map_err(|e| format!("Uri.parse.l: {e}"))?;
    let resolver = env
        .call_method(context, "getContentResolver", "()Landroid/content/ContentResolver;", &[])
        .map_err(|e| jni_err(env, "getContentResolver", e))?
        .l()
        .map_err(|e| format!("getContentResolver.l: {e}"))?;
    Ok((resolver, uri_obj))
}

#[cfg(target_os = "android")]
fn jni_err(env: &mut jni::JNIEnv, what: &str, e: jni::errors::Error) -> String {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    format!("{what}: {e}")
}

/// Takes `s` by value so the borrowed `JavaStr` never outlives it (inline form trips NLL).
#[cfg(target_os = "android")]
fn jstring_owned(env: &mut jni::JNIEnv, s: jni::objects::JString) -> Result<String, String> {
    match env.get_string(&s) {
        Ok(js) => Ok(js.into()),
        Err(e) => Err(jni_err(env, "get_string", e)),
    }
}

// App classes must be cached as GlobalRefs: native threads use the system ClassLoader, so find_class fails for them.
// Framework classes resolve anywhere.
#[cfg(target_os = "android")]
pub(crate) static TRANSFER_SERVICE_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();
#[cfg(target_os = "android")]
pub(crate) static NIGHTWATCH_SERVICE_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();
#[cfg(target_os = "android")]
pub(crate) static NW_TREE_PICKER_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();

/// Must run on the JVM thread, where find_class works.
#[cfg(target_os = "android")]
fn cache_app_class(env: &mut jni::JNIEnv, name: &str, slot: &std::sync::OnceLock<jni::objects::GlobalRef>) {
    if slot.get().is_some() {
        return;
    }
    match env.find_class(name) {
        Ok(cls) => match env.new_global_ref(cls) {
            Ok(g) => {
                let _ = slot.set(g);
            }
            Err(e) => {
                let _ = jni_err(env, "new_global_ref(app class)", e);
            }
        },
        Err(e) => {
            let _ = jni_err(env, "find_class(app class)", e);
        }
    }
}

/// Called from CosmogApp and MainActivity on the JVM thread; idempotent via OnceLock.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_CosmogApp_initNwClasses(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
) {
    cache_app_class(&mut env, "com/sonus/cosmog/TransferService", &TRANSFER_SERVICE_CLASS);
    cache_app_class(&mut env, "com/sonus/cosmog/NightWatchService", &NIGHTWATCH_SERVICE_CLASS);
    cache_app_class(&mut env, "com/sonus/cosmog/NwTreePicker", &NW_TREE_PICKER_CLASS);
}
