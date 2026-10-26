#[cfg(target_os = "android")]
use super::{jni_err, NIGHTWATCH_SERVICE_CLASS, TRANSFER_SERVICE_CLASS};

/// Foreground service so Doze/cached-process reaps don't kill in-flight transfers (they would restart from 0).
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

    // Uncached only if startup init failed; find_class then works only on the JVM thread.
    let local;
    let cls: &jni::objects::JClass = match TRANSFER_SERVICE_CLASS.get() {
        Some(g) => g.as_obj().into(),
        None => {
            local = env
                .find_class("com/sonus/cosmog/TransferService")
                .map_err(|e| jni_err(&mut env, "find_class(TransferService)", e))?;
            &local
        }
    };
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
    // startOrRetry, not start: a refusal must re-arm rather than leave sync off.
    let method = if active { "startOrRetry" } else { "stop" };
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

/// Keeps long syncs from losing CPU between bounded wakelock acquires.
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
pub fn nw_wakelock_idle() -> Result<(), String> {
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
    env.call_static_method(cls, "idleWakelock", "()V", &[])
        .map_err(|e| jni_err(&mut env, "idleWakelock", e))?;
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

/// Whether the OS would let us schedule an exact alarm. Not pre-granted to
/// fresh installs targeting A13+, and without it Night Watcher cannot restart
/// itself after an FGS timeout, or after a reboot on A15+.
#[cfg(target_os = "android")]
pub fn nw_can_schedule_exact_alarms() -> Result<bool, String> {
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
    let v = env
        .call_static_method(
            cls,
            "canScheduleExactAlarms",
            "(Landroid/content/Context;)Z",
            &[JValue::Object(&context)],
        )
        .map_err(|e| jni_err(&mut env, "canScheduleExactAlarms", e))?;
    v.z().map_err(|e| format!("canScheduleExactAlarms: {e}"))
}

#[cfg(not(target_os = "android"))]
pub fn nw_can_schedule_exact_alarms() -> Result<bool, String> {
    Ok(true)
}

/// Opens the system "Alarms & reminders" screen; the grant exists only there.
#[cfg(target_os = "android")]
pub fn nw_request_exact_alarms() -> Result<(), String> {
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
    let opened = env
        .call_static_method(
            cls,
            "requestExactAlarms",
            "(Landroid/content/Context;)Z",
            &[JValue::Object(&context)],
        )
        .map_err(|e| jni_err(&mut env, "requestExactAlarms", e))?
        .z()
        .map_err(|e| format!("requestExactAlarms: {e}"))?;
    if !opened {
        return Err("no system screen handled the alarms permission request".into());
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn nw_request_exact_alarms() -> Result<(), String> {
    Ok(())
}
