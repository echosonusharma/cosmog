//! Raw file descriptors for SAF documents. `/proc/self/fd/N` cannot be reopened (FUSE answers
//! EACCES), so every open of a document goes through ContentResolver again.

/// Seekable regular file for `doc_uri`. NotFound for a gone document, PermissionDenied for a
/// lost grant, InvalidInput for a non-regular fd (a provider pipe).
#[cfg(target_os = "android")]
pub fn open_saf_fd(doc_uri: &str) -> std::io::Result<std::fs::File> {
    use jni::objects::{JObject, JValue};
    use jni::JavaVM;
    use std::io::{Error, ErrorKind};
    use std::os::fd::FromRawFd;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err(Error::other("android context not initialized"));
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| Error::other(format!("JavaVM::from_raw: {e}")))?;
    let mut env = vm.attach_current_thread().map_err(|e| Error::other(format!("attach_current_thread: {e}")))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };

    let fd = env
        .with_local_frame(16, |env| -> Result<i32, IoFatal> {
            let (resolver, uri) = super::resolver_and_uri(env, &context, doc_uri).map_err(|e| IoFatal(Error::other(e)))?;
            let mode = env.new_string("r")?;
            let pfd = env
                .call_method(
                    &resolver,
                    "openFileDescriptor",
                    "(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;",
                    &[JValue::Object(&uri), JValue::Object(&JObject::from(mode))],
                )
                .map_err(|e| IoFatal(io_err(env, "openFileDescriptor", e)))?
                .l()?;
            if pfd.is_null() {
                return Err(IoFatal(Error::new(ErrorKind::NotFound, "openFileDescriptor returned null")));
            }
            let fd = match env.call_method(&pfd, "detachFd", "()I", &[]) {
                Ok(v) => v.i()?,
                Err(e) => {
                    let err = io_err(env, "detachFd", e);
                    let _ = env.call_method(&pfd, "close", "()V", &[]);
                    let _ = env.exception_clear();
                    return Err(IoFatal(err));
                }
            };
            // The fd is ours now; closing the wrapper does not touch it.
            if let Err(e) = env.call_method(&pfd, "close", "()V", &[]) {
                let _ = super::jni_err(env, "ParcelFileDescriptor.close", e);
            }
            Ok(fd)
        })
        .map_err(|f| f.0)?;
    if fd < 0 {
        return Err(Error::other("detachFd returned an invalid fd"));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(file)
}

#[cfg(not(target_os = "android"))]
pub fn open_saf_fd(_doc_uri: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "SAF open is Android-only"))
}

#[cfg(target_os = "android")]
struct IoFatal(std::io::Error);

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for IoFatal {
    fn from(e: jni::errors::Error) -> Self {
        IoFatal(std::io::Error::other(format!("jni: {e}")))
    }
}

/// Maps the pending Java exception to an io kind, then clears it.
#[cfg(target_os = "android")]
fn io_err(env: &mut jni::JNIEnv, what: &str, e: jni::errors::Error) -> std::io::Error {
    use std::io::ErrorKind;
    let mut kind = ErrorKind::Other;
    if env.exception_check().unwrap_or(false) {
        let ex = env.exception_occurred();
        let _ = env.exception_clear();
        if let Ok(ex) = ex {
            let is = |env: &mut jni::JNIEnv, cls: &str| env.is_instance_of(&ex, cls).unwrap_or(false);
            // DocumentsProvider wraps a missing child as IllegalArgumentException.
            if is(env, "java/io/FileNotFoundException") || is(env, "java/lang/IllegalArgumentException") {
                kind = ErrorKind::NotFound;
            } else if is(env, "java/lang/SecurityException") {
                kind = ErrorKind::PermissionDenied;
            }
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
        }
    }
    std::io::Error::new(kind, format!("{what}: {e}"))
}
