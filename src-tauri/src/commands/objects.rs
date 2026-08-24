//! Object-level Tauri commands. Mutating commands update the local search
//! cache best-effort: a cache write failure never rolls back the remote op.

use chrono::Utc;
use tauri::State;
use tracing::warn;

use crate::db::capabilities::{CapState, WriteOp};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::validate;
use crate::store::{
    CannedAcl, DeleteObjectsResult, ListOptions, ListPage, ObjectMeta, ObjectPreview, ObjectTag,
    ObjectVersion,
};

/// On cache write failure, expires the prefix TTL so the next browse_prefix
/// triggers a background re-sync and auto-corrects the stale entry.
fn expire_prefix_on_cache_err(state: &AppState, account_id: &str, bucket: &str, key: &str, err: &crate::error::AppError) {
    warn!("cache write failed for {key}: {err} — expiring prefix TTL to trigger re-sync");
    let db = state.db.clone();
    let account_id = account_id.to_string();
    let bucket = bucket.to_string();
    // Derive the *parent* listing prefix; strip a trailing slash first so
    // folder markers like "foo/bar/" resolve to "foo/" instead of themselves.
    let prefix = {
        let stripped = key.trim_end_matches('/');
        stripped.rfind('/').map(|i| &stripped[..=i]).unwrap_or("").to_string()
    };
    tokio::spawn(async move {
        let _ = db.prefix_sync_expire(&account_id, &bucket, &prefix).await;
    });
}

/// Records Allowed/Denied write outcomes to the capability cache; ignores
/// other error classes (a network blip is not proof of denial).
async fn record_write(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    op: WriteOp,
    result: &AppResult<()>,
) {
    let cap = match result {
        Ok(()) => CapState::Allowed,
        Err(AppError::AccessDenied(_)) => CapState::Denied,
        _ => return,
    };
    let _ = state
        .db
        .capability_record_write(account_id, bucket, op, cap)
        .await;
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    delimiter: Option<String>,
    continuation: Option<String>,
    max_keys: Option<i32>,
) -> AppResult<ListPage> {
    let store = state.store_for(&account_id).await?;
    store
        .list_objects(
            &bucket,
            ListOptions {
                prefix,
                delimiter,
                continuation,
                max_keys,
            },
        )
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn head_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<ObjectMeta> {
    let meta = state
        .store_for(&account_id)
        .await?
        .head_object(&bucket, &key)
        .await?;
    if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
    }
    Ok(meta)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn create_folder(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
) -> AppResult<()> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let prefix = validate::require_non_empty("prefix", &prefix)?;
    let store = state.store_for(&account_id).await?;
    let key = format!("{}/", prefix.trim_end_matches('/'));
    let res = store.create_folder(&bucket, &prefix).await;
    record_write(&state, &account_id, &bucket, WriteOp::Put, &res).await;
    res?;
    let meta = ObjectMeta {
        key: key.clone(),
        size: 0,
        etag: None,
        last_modified: Some(Utc::now().timestamp()),
        storage_class: None,
        content_type: Some("application/x-directory".into()),
        version_id: None,
        user_metadata: Default::default(),
    };
    if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let res = store.delete_object(&bucket, &key).await;
    record_write(&state, &account_id, &bucket, WriteOp::Delete, &res).await;
    res?;
    if let Err(e) = state.db.cache_remove_object(&account_id, &bucket, &key).await {
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &key, &e);
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
) -> AppResult<DeleteObjectsResult> {
    let res = state
        .store_for(&account_id)
        .await?
        .delete_objects(&bucket, &keys)
        .await;
    // Outer Result is request-level success (per-key errors inside); mirror
    // that into the capability tracker.
    let cap = match &res {
        Ok(_) => CapState::Allowed,
        Err(AppError::AccessDenied(_)) => CapState::Denied,
        _ => CapState::Unknown,
    };
    if !matches!(cap, CapState::Unknown) {
        let _ = state
            .db
            .capability_record_write(&account_id, &bucket, WriteOp::Delete, cap)
            .await;
    }
    let result = res?;
    // Only remove cache rows for keys the server confirmed deleted.
    for key in &result.deleted {
        if let Err(e) = state.db.cache_remove_object(&account_id, &bucket, key).await {
            expire_prefix_on_cache_err(&state, &account_id, &bucket, key, &e);
        }
    }
    Ok(result)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object_version(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .delete_object_version(&bucket, &key, &version_id)
        .await
    // We don't touch the cache here: only the live/latest version is mirrored.
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn restore_object_version(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .restore_object_version(&bucket, &key, &version_id)
        .await
    // No cache write: the restore only affects remote version state, and a
    // subsequent list/head refreshes the mirrored latest version.
}

#[derive(serde::Serialize)]
pub struct VersionsPage {
    pub versions: Vec<ObjectVersion>,
    pub continuation: Option<String>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_object_versions(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    continuation: Option<String>,
) -> AppResult<VersionsPage> {
    let (versions, continuation) = state
        .store_for(&account_id)
        .await?
        .list_object_versions(&bucket, prefix.as_deref(), continuation)
        .await?;
    Ok(VersionsPage {
        versions,
        continuation,
    })
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn copy_object(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    src_key: String,
    dst_bucket: String,
    dst_key: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let res = store
        .copy_object(&src_bucket, &src_key, &dst_bucket, &dst_key)
        .await;
    record_write(&state, &account_id, &dst_bucket, WriteOp::Put, &res).await;
    res?;

    match store.head_object(&dst_bucket, &dst_key).await {
        Ok(meta) => {
            if let Err(e) = state
                .db
                .cache_upsert_object(&account_id, &dst_bucket, &meta)
                .await
            {
                expire_prefix_on_cache_err(&state, &account_id, &dst_bucket, &meta.key, &e);
            }
        }
        Err(e) => warn!("head after copy_object failed: {e}"),
    }
    Ok(())
}

/// Move/rename: S3 has no atomic move — copy then delete the source. If the
/// source delete fails, both keys exist and the error says which to clean up.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn move_object(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    src_key: String,
    dst_bucket: String,
    dst_key: String,
) -> AppResult<()> {
    if src_bucket == dst_bucket && src_key == dst_key {
        return Err(AppError::InvalidInput("source equals destination".into()));
    }
    let store = state.store_for(&account_id).await?;
    let copy_res = store
        .copy_object(&src_bucket, &src_key, &dst_bucket, &dst_key)
        .await;
    record_write(&state, &account_id, &dst_bucket, WriteOp::Put, &copy_res).await;
    copy_res?;
    // Mirror dst into the cache before deleting src so the FE never sees a
    // moment with neither key indexed.
    if let Ok(meta) = store.head_object(&dst_bucket, &dst_key).await {
        if let Err(e) = state
            .db
            .cache_upsert_object(&account_id, &dst_bucket, &meta)
            .await
        {
            expire_prefix_on_cache_err(&state, &account_id, &dst_bucket, &dst_key, &e);
        }
    }
    let del_res = store.delete_object(&src_bucket, &src_key).await;
    record_write(&state, &account_id, &src_bucket, WriteOp::Delete, &del_res).await;
    if let Err(e) = del_res {
        return Err(AppError::Internal(format!(
            "copied to \"{dst_key}\" but could not delete source \"{src_key}\": {e}. \
             Both keys exist — delete the unwanted one manually."
        )));
    }
    if let Err(e) = state
        .db
        .cache_remove_object(&account_id, &src_bucket, &src_key)
        .await
    {
        expire_prefix_on_cache_err(&state, &account_id, &src_bucket, &src_key, &e);
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_acl(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    acl: CannedAcl,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_object_acl(&bucket, &key, acl)
        .await
}

/// Hard cap for in-memory writes, enforced BEFORE any encryption decision;
/// tighter than crypto's 512 MiB limit — IPC payloads buffer whole objects in RAM.
const MAX_INMEMORY_PUT_BYTES: usize = 64 * 1024 * 1024;

/// Encrypts `data` via the bucket's age recipient (passthrough when unconfigured);
/// returns `(bytes, user_metadata)` that callers must forward for HEAD-based detection.
async fn encrypt_for_bucket(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    data: Vec<u8>,
) -> AppResult<(Vec<u8>, std::collections::HashMap<String, String>)> {
    if data.len() > MAX_INMEMORY_PUT_BYTES {
        return Err(AppError::InvalidInput(format!(
            "in-memory write refused: payload is {} bytes, limit is {} bytes — \
             use the streaming file upload instead",
            data.len(),
            MAX_INMEMORY_PUT_BYTES
        )));
    }
    let cfg = match state.db.get_encryption_config(account_id, bucket).await? {
        Some(c) => c,
        None => return Ok((data, Default::default())),
    };
    if (data.len() as u64) > crate::crypto::MAX_INMEMORY_CRYPT_BYTES {
        return Err(AppError::InvalidInput(format!(
            "encrypted in-memory write refused: payload is {} bytes, limit is {} bytes",
            data.len(),
            crate::crypto::MAX_INMEMORY_CRYPT_BYTES
        )));
    }
    let recipient = crate::crypto::parse_recipient(&cfg.recipient)?;
    let ciphertext = tokio::task::spawn_blocking(move || {
        crate::crypto::encrypt_bytes(&recipient, &data)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    let mut md = std::collections::HashMap::new();
    md.insert("cosmog-encrypted".into(), "1".into());
    md.insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
    md.insert("cosmog-recipient".into(), cfg.recipient);
    Ok((ciphertext, md))
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn preview_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    max_bytes: Option<u64>,
) -> AppResult<ObjectPreview> {
    let max = max_bytes.unwrap_or(1024 * 1024);
    let store = state.store_for(&account_id).await?;

    // Encrypted buckets: decide per object. Pre-encryption objects lack the
    // marker and are served as-is; marked ones take whole-object GCM decrypt.
    if state.db.get_encryption_config(&account_id, &bucket).await?.is_some() {
        let head = store.head_object(&bucket, &key).await?;
        let marked = head.user_metadata.get("cosmog-encrypted").map(|s| s.as_str()) == Some("1");

        // Size guard: refuse to buffer huge ciphertext into RAM for previews
        // (see `MAX_PREVIEW_DECRYPT_BYTES`).
        if marked && head.size as u64 > MAX_PREVIEW_DECRYPT_BYTES {
            // Fall back to a bounded range read; oversized ciphertext can't be
            // decrypted here anyway, so the FE sees ciphertext bytes.
            return store.read_object_range(&bucket, &key, max).await;
        }

        // Unmarked: do the cheap BOUNDED range read first and only pay for a
        // whole-object fetch when those bytes turn out to be an age payload.
        if !marked {
            let preview = store.read_object_range(&bucket, &key, max).await?;
            // Range reads start at offset 0 — enough for the magic probe.
            if !crate::crypto::is_age_ciphertext(&preview.bytes) {
                return Ok(preview);
            }
            // Age bytes detected without the marker: fall through to the
            // full-fetch decrypt path below, which re-checks and warns.
        }

        // Whole-ciphertext fetch: read_object_full bypasses read_object_range's
        // 8 MiB preview cap; age decrypt needs the full stream to authenticate.
        let ciphertext = store.read_object_full(&bucket, &key).await?;
        // Trust the payload bytes, not S3 user metadata: an attacker with PUT
        // rights could strip/forge `cosmog-encrypted`.
        let looks_encrypted = crate::crypto::is_age_ciphertext(&ciphertext);
        if marked && !looks_encrypted {
            // Metadata says encrypted, bytes disagree (legacy pre-age upload or
            // stripped header): never serve raw ciphertext — surface an error.
            return Err(AppError::InvalidInput(
                "cannot decrypt: payload is not in the expected age format".into(),
            ));
        }
        if !marked && looks_encrypted {
            tracing::warn!(
                bucket = %bucket, key = %key,
                "age-encrypted payload without cosmog-encrypted marker: attempting decrypt anyway"
            );
        }
        if !looks_encrypted {
            let content_type = head.content_type.clone();
            let total_size = Some(ciphertext.len() as i64);
            let truncated = ciphertext.len() as u64 > max;
            let bytes = ciphertext.into_iter().take(max as usize).collect();
            return Ok(ObjectPreview { bytes, content_type, total_size, truncated });
        }
        let content_type = head.content_type.clone();

        let aid = account_id.clone();
        let bkt = bucket.clone();
        let secret = tokio::task::spawn_blocking(move || crate::secrets::get_enc_identity(&aid, &bkt))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??
            .ok_or_else(|| AppError::EncryptionIdentityMissing(format!(
                "identity for bucket '{bucket}' not present in the OS keychain. \
                 Import a previously exported identity file to decrypt this object."
            )))?;
        let identity = crate::crypto::parse_identity(&secret)?;
        let plaintext = tokio::task::spawn_blocking(move || {
            crate::crypto::decrypt_bytes(&identity, &ciphertext)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

        // total_size reports plaintext length, keeping FE units coherent with
        // the returned `bytes` (what `truncated` is compared against).
        let total_size = Some(plaintext.len() as i64);
        let truncated = plaintext.len() as u64 > max;
        let bytes = plaintext.into_iter().take(max as usize).collect();
        return Ok(ObjectPreview { bytes, content_type, total_size, truncated });
    }

    store.read_object_range(&bucket, &key, max).await
}

/// Max ciphertext buffered in RAM for an in-app preview decrypt; anything
/// larger errors so the user knows to download instead of preview.
const MAX_PREVIEW_DECRYPT_BYTES: u64 = 128 * 1024 * 1024;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<Vec<ObjectTag>> {
    state
        .store_for(&account_id)
        .await?
        .get_object_tagging(&bucket, &key)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    tags: Vec<ObjectTag>,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_object_tagging(&bucket, &key, &tags)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .delete_object_tagging(&bucket, &key)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn presign_get(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    expires_secs: Option<u64>,
    allow_ciphertext: Option<bool>,
) -> AppResult<String> {
    let expires = match expires_secs {
        Some(s) => s,
        None => state.db.settings_load().await?.presign_default_expires_secs,
    };
    // SigV4 presigned URLs cap at 7 days (604800s); sub-minute links are
    // useless and usually a caller bug.
    let expires = expires.clamp(60, 604_800);
    let store = state.store_for(&account_id).await?;

    // Encrypted bucket: a presigned URL may deliver ciphertext and the S3
    // marker is attacker-controllable — refuse unless allow_ciphertext=true.
    if !allow_ciphertext.unwrap_or(false)
        && state.db.get_encryption_config(&account_id, &bucket).await?.is_some()
    {
        return Err(AppError::InvalidInput(
            "bucket has client-side encryption enabled: a presigned link may deliver \
             ciphertext. Pass allow_ciphertext=true to opt in and share the key \
             out-of-band."
                .into(),
        ));
    }

    store.presign_get(&bucket, &key, expires).await
}

async fn put_object_inner(
    store: &std::sync::Arc<dyn crate::store::ObjectStore>,
    state: &AppState,
    account_id: &str,
    bucket: &str,
    key: &str,
    data: Vec<u8>,
    content_type: &str,
    md: std::collections::HashMap<String, String>,
) -> AppResult<()> {
    let res = store.put_object_bytes(bucket, key, content_type, data, md).await;
    record_write(state, account_id, bucket, WriteOp::Put, &res).await;
    res?;
    match store.head_object(bucket, key).await {
        Ok(meta) => {
            if let Err(e) = state.db.cache_upsert_object(account_id, bucket, &meta).await {
                expire_prefix_on_cache_err(state, account_id, bucket, &meta.key, &e);
            }
        }
        Err(e) => warn!("head after put_object failed: {e}"),
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_text(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    content: String,
    content_type: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let (data, md) = encrypt_for_bucket(&state, &account_id, &bucket, content.into_bytes()).await?;
    put_object_inner(&store, &state, &account_id, &bucket, &key, data, &content_type, md).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_bytes_cmd(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    bytes: Vec<u8>,
    content_type: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let (data, md) = encrypt_for_bucket(&state, &account_id, &bucket, bytes).await?;
    put_object_inner(&store, &state, &account_id, &bucket, &key, data, &content_type, md).await
}

/// Hard stop for `list_keys_under_prefix` paging: bucket-root walks with
/// millions of keys would otherwise page S3 forever, ballooning RAM + IPC payload.
const MAX_LISTED_KEYS: usize = 100_000;

/// Lists every key under `prefix` by paging S3 directly (no cache), used by
/// delete-folder/empty-bucket so stale cache can't cause silent misses.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_keys_under_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
) -> AppResult<Vec<String>> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let store = state.store_for(&account_id).await?;
    let mut keys = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let page = store
            .list_objects(
                &bucket,
                ListOptions {
                    prefix: if prefix.is_empty() { None } else { Some(prefix.clone()) },
                    delimiter: None,
                    continuation: continuation.clone(),
                    max_keys: Some(1000),
                },
            )
            .await?;
        for obj in &page.objects {
            keys.push(obj.key.clone());
            if keys.len() >= MAX_LISTED_KEYS {
                return Err(AppError::InvalidInput(format!(
                    "prefix listing exceeded the hard cap of {MAX_LISTED_KEYS} keys; \
                     narrow the prefix and retry"
                )));
            }
        }
        if page.is_truncated {
            continuation = page.continuation;
            if continuation.is_none() {
                // Provider returned truncated=true but no token; avoid infinite loop.
                break;
            }
        } else {
            break;
        }
    }
    Ok(keys)
}
