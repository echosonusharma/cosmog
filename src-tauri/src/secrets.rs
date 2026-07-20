//! Thin wrapper over the platform-native secret store.
//!
//! Desktop: OS keyring (Secret Service on Linux, Keychain on macOS, Credential
//! Manager on Windows) via the `keyring` crate.
//!
//! Android: EncryptedSharedPreferences backed by the Android Keystore. See
//! `SecretStore.kt`.
//!
//! [`SERVICE`] must match the application's identifier so OS UIs render
//! sensible attribution. If you change it, existing users will lose access to
//! their stored secrets.

use crate::error::{AppError, AppResult};

// ── platform backends ────────────────────────────────────────────────────────

#[cfg(not(target_os = "android"))]
mod backend {
    use crate::error::{AppError, AppResult};

    const SERVICE: &str = "com.sonus.cosmog";

    fn entry(key: &str) -> AppResult<keyring::Entry> {
        keyring::Entry::new(SERVICE, key).map_err(AppError::from)
    }

    pub fn set(key: &str, value: &str) -> AppResult<()> {
        entry(key)?.set_password(value).map_err(AppError::from)
    }

    pub fn get(key: &str) -> AppResult<Option<String>> {
        match entry(key)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::from(e)),
        }
    }

    pub fn delete(key: &str) -> AppResult<()> {
        match entry(key)?.delete_credential() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::from(e)),
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) static SECRET_STORE_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> =
    std::sync::OnceLock::new();

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_NativeBridge_initNdkContext(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    context: jni::objects::JObject,
) {
    eprintln!("initNdkContext: entered");
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("initNdkContext get_java_vm failed: {e}");
            return;
        }
    };
    let ctx_global = match env.new_global_ref(&context) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("initNdkContext new_global_ref failed: {e}");
            return;
        }
    };
    let ctx_raw = ctx_global.as_obj().as_raw();
    // Leak the global ref so the JNI reference stays valid for the process lifetime.
    std::mem::forget(ctx_global);
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            ctx_raw.cast(),
        );
    }
    // Cache the SecretStore class as a global ref. FindClass from an attached
    // native thread would otherwise use the system ClassLoader and fail to
    // find app classes.
    match env.find_class("com/sonus/cosmog/SecretStore") {
        Ok(cls) => match env.new_global_ref(cls) {
            Ok(g) => {
                let _ = SECRET_STORE_CLASS.set(g);
                eprintln!("initNdkContext: SecretStore class cached");
            }
            Err(e) => eprintln!("initNdkContext: new_global_ref(SecretStore) failed: {e}"),
        },
        Err(e) => eprintln!("initNdkContext: find_class(SecretStore) failed: {e}"),
    }
    eprintln!("initNdkContext: done");
}

#[cfg(target_os = "android")]
mod backend {
    use crate::error::{AppError, AppResult};
    use jni::objects::{JObject, JString, JValue};
    use jni::JavaVM;

    fn with_env<T>(
        f: impl FnOnce(&mut jni::JNIEnv, &JObject, &jni::objects::JClass) -> Result<T, jni::errors::Error>,
    ) -> AppResult<T> {
        let ctx = ndk_context::android_context();
        if ctx.vm().is_null() || ctx.context().is_null() {
            return Err(AppError::Keyring(
                "android context not initialized (initNdkContext not called)".into(),
            ));
        }
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| AppError::Keyring(format!("JavaVM::from_raw: {e}")))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| AppError::Keyring(format!("attach_current_thread: {e}")))?;
        let app = unsafe { JObject::from_raw(ctx.context().cast()) };
        let cls_ref = super::SECRET_STORE_CLASS.get().ok_or_else(|| {
            AppError::Keyring("SecretStore class not cached (init not complete)".into())
        })?;
        let cls_obj: &JObject = cls_ref.as_obj();
        let cls: &jni::objects::JClass = cls_obj.into();
        let result = f(&mut env, &app, cls).map_err(|e| AppError::Keyring(format!("JNI: {e}")));
        // Do not drop `app` — it is a global ref owned by ndk_context.
        std::mem::forget(app);
        result
    }

    pub fn set(key: &str, value: &str) -> AppResult<()> {
        with_env(|env, app, cls| {
            let k: JString = env.new_string(key)?;
            let v: JString = env.new_string(value)?;
            env.call_static_method(
                cls,
                "set",
                "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(app),
                    JValue::Object(&JObject::from(k)),
                    JValue::Object(&JObject::from(v)),
                ],
            )?;
            Ok(())
        })
    }

    pub fn get(key: &str) -> AppResult<Option<String>> {
        with_env(|env, app, cls| {
            let k: JString = env.new_string(key)?;
            let ret = env.call_static_method(
                cls,
                "get",
                "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(app), JValue::Object(&JObject::from(k))],
            )?;
            let obj = ret.l()?;
            if obj.is_null() {
                Ok(None)
            } else {
                let s: JString = obj.into();
                let rust: String = env.get_string(&s)?.into();
                Ok(Some(rust))
            }
        })
    }

    pub fn delete(key: &str) -> AppResult<()> {
        with_env(|env, app, cls| {
            let k: JString = env.new_string(key)?;
            env.call_static_method(
                cls,
                "remove",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(app), JValue::Object(&JObject::from(k))],
            )?;
            Ok(())
        })
    }
}

// ── public API (stable across platforms) ─────────────────────────────────────

pub fn set_secret(account_id: &str, secret: &str) -> AppResult<()> {
    backend::set(account_id, secret)
}

pub fn get_secret(account_id: &str) -> AppResult<String> {
    backend::get(account_id)?.ok_or_else(|| {
        AppError::NotFound(
            "credentials not found in system keychain. Please re-add this account in Settings."
                .into(),
        )
    })
}

pub fn delete_secret(account_id: &str) -> AppResult<()> {
    backend::delete(account_id)
}

// ── per-bucket encryption identities (age X25519 secret keys) ─────────────────

fn enc_key(account_id: &str, bucket: &str) -> String {
    format!("enc:{account_id}:{bucket}")
}

/// Store the bech32 `AGE-SECRET-KEY-...` string for a bucket.
pub fn set_enc_identity(account_id: &str, bucket: &str, secret: &str) -> AppResult<()> {
    backend::set(&enc_key(account_id, bucket), secret)
}

/// Retrieve the bech32 `AGE-SECRET-KEY-...` string, or `None` if the entry is
/// missing. Callers should scrub the returned buffer once done.
pub fn get_enc_identity(account_id: &str, bucket: &str) -> AppResult<Option<String>> {
    backend::get(&enc_key(account_id, bucket))
}

pub fn delete_enc_identity(account_id: &str, bucket: &str) -> AppResult<()> {
    backend::delete(&enc_key(account_id, bucket))
}
