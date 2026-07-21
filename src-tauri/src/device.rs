//! Device / OS info for the bug-report dialog.
//!
//! The web layer's `navigator` only describes the WebView runtime (e.g.
//! "Linux aarch64" on Android), which is useless for triage. This resolves the
//! real platform: OS name + version + CPU arch on desktop, and the actual
//! Android release / API level / device model via JNI on Android.

/// Serializes to the FE as `{ os, os_version, arch, model }`. `model` is only
/// populated on Android (desktop OSes have no meaningful single model string).
#[derive(serde::Serialize)]
pub struct DeviceInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub model: Option<String>,
}

#[cfg(not(target_os = "android"))]
pub fn get_device_info() -> Result<DeviceInfo, String> {
    let info = os_info::get();
    Ok(DeviceInfo {
        os: info.os_type().to_string(),
        os_version: info.version().to_string(),
        arch: std::env::consts::ARCH.to_string(),
        model: None,
    })
}

#[cfg(target_os = "android")]
pub fn get_device_info() -> Result<DeviceInfo, String> {
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() {
        return Err("android context not initialized".into());
    }
    let vm =
        unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("JavaVM::from_raw: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;

    // android.os.Build.* static String fields.
    let manufacturer =
        build_string(&mut env, "android/os/Build", "MANUFACTURER").unwrap_or_default();
    let model = build_string(&mut env, "android/os/Build", "MODEL").unwrap_or_default();
    let release = build_string(&mut env, "android/os/Build$VERSION", "RELEASE")
        .unwrap_or_else(|| "unknown".into());

    // Build.VERSION.SDK_INT is an int field (API level).
    let sdk = env
        .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }

    let model_str = format!("{manufacturer} {model}").trim().to_string();

    Ok(DeviceInfo {
        os: "Android".into(),
        os_version: if sdk > 0 {
            format!("{release} (API {sdk})")
        } else {
            release
        },
        arch: std::env::consts::ARCH.to_string(),
        model: if model_str.is_empty() {
            None
        } else {
            Some(model_str)
        },
    })
}

/// Read a static `String` field off a Java class. Reading `Build` constants
/// does not throw, but any pending exception is cleared before returning so a
/// stray one can never abort the process on the next JNI call.
#[cfg(target_os = "android")]
fn build_string(env: &mut jni::JNIEnv, class: &str, field: &str) -> Option<String> {
    use jni::objects::JString;

    let val = match env.get_static_field(class, field, "Ljava/lang/String;") {
        Ok(v) => v,
        Err(_) => {
            clear_exception(env);
            return None;
        }
    };
    let obj = val.l().ok()?;
    if obj.is_null() {
        return None;
    }
    let jstr: JString = obj.into();
    // Bind to a local: `env.get_string` borrows `jstr`, and returning the
    // match directly would drop `jstr` before the borrowed temporary.
    let out = match env.get_string(&jstr) {
        Ok(js) => Some(String::from(js)),
        Err(_) => {
            clear_exception(env);
            None
        }
    };
    out
}

#[cfg(target_os = "android")]
fn clear_exception(env: &mut jni::JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}
