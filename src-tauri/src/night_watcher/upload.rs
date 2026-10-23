use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::{NwCtx, MAX_UPLOAD_RETRIES, RETRY_PAUSE_SECS, UPLOADS};
use crate::db::night_watcher::{FileState, NightWatch};
use crate::error::AppResult;
use crate::store::PutOptions;
use crate::transfer::{ProgressSink, TransferEvent};

#[allow(clippy::too_many_arguments)]
pub(super) async fn enqueue_upload_for<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
    mtime: i64,
    size: i64,
    hash: String,
    stage_dir: Option<PathBuf>,
) -> AppResult<()> {
    let mut opts = PutOptions::default();
    let (upload_path, cleanup_on_err) =
        crate::transfer::encrypt::encrypt_for_bucket_if_needed_with(
            state.db(),
            state.db_path(),
            &watch.account_id,
            &watch.bucket,
            abs,
            &mut opts,
        )
        .await?;

    let key = build_key(&watch.key_prefix, rel_path);
    let sink = persist_sink(
        state.clone(),
        watch.account_id.clone(),
        watch.key_prefix.clone(),
        watch.id.clone(),
        rel_path.to_string(),
        hash,
        mtime,
        size,
        stage_dir.clone(),
    );

    let enqueue = async {
        let store = state.store_for(&watch.account_id).await?;
        state
            .transfers()
            .enqueue_upload(
                store,
                watch.account_id.clone(),
                watch.bucket.clone(),
                key,
                upload_path,
                opts,
                sink,
                crate::db::transfers::TransferOrigin::NightWatch,
            )
            .await
    }
    .await;

    match enqueue {
        Ok(id) => {
            UPLOADS.entry(id).or_insert_with(|| watch.id.clone());
            Ok(())
        }
        Err(e) => {
            if let Some(p) = cleanup_on_err {
                let _ = tokio::fs::remove_file(&p).await;
            }
            Err(e)
        }
    }
}

/// `stage_dir` (SAF) is removed only on terminal events since the worker holds it open.
#[allow(clippy::too_many_arguments)]
fn persist_sink<S: NwCtx>(
    state: S,
    account_id: String,
    key_prefix: String,
    watch_id: String,
    rel_path: String,
    hash: String,
    mtime: i64,
    size: i64,
    stage_dir: Option<PathBuf>,
) -> ProgressSink {
    let cleanup_stage = move || {
        if let Some(dir) = stage_dir.clone() {
            tokio::spawn(async move {
                let _ = tokio::fs::remove_dir_all(&dir).await;
            });
        }
    };
    ProgressSink::from_fn(move |event| match event {
        TransferEvent::Started { transfer_id, .. } => {
            UPLOADS.insert(transfer_id, watch_id.clone());
        }
        TransferEvent::Done { etag, transfer_id } => {
            UPLOADS.remove(&transfer_id);
            let state = state.clone();
            let watch_id = watch_id.clone();
            let rel_path = rel_path.clone();
            let hash = hash.clone();
            let key_prefix = key_prefix.clone();
            cleanup_stage();
            tokio::spawn(async move {
                // Prefix changed or watch deleted mid-upload: recording it would skip the re-upload.
                let moved = match state.db().get_watch(&watch_id).await {
                    Ok(Some(w)) => w.key_prefix.trim_matches('/') != key_prefix.trim_matches('/'),
                    Ok(None) => true,
                    Err(_) => false,
                };
                if moved {
                    info!(watch = %watch_id, rel = %rel_path, "night watcher: destination changed mid-upload; not recording");
                    state.nw_unclaim(&watch_id, &rel_path);
                    return;
                }
                let st = FileState {
                    rel_path: rel_path.clone(),
                    hash,
                    mtime,
                    size,
                    synced_etag: etag,
                };
                if let Err(e) = state.db().file_state_upsert(&watch_id, st).await {
                    warn!(watch = %watch_id, rel = %rel_path, "night watcher: persist synced state failed: {e}");
                } else {
                    info!(watch = %watch_id, rel = %rel_path, "night watcher: synced");
                }
                let _ = state.db().file_retry_clear(&watch_id, &rel_path).await;
                state.nw_unclaim(&watch_id, &rel_path);
            });
        }
        TransferEvent::Failed { error, transfer_id } => {
            UPLOADS.remove(&transfer_id);
            warn!(watch = %watch_id, rel = %rel_path, "night watcher: upload failed: {}", crate::error::wire_for_log(&error));
            // Possibly stale credentials: the next attempt builds a fresh client.
            state.invalidate_store(&account_id);
            cleanup_stage();
            let state = state.clone();
            let watch_id = watch_id.clone();
            let rel_path = rel_path.clone();
            tokio::spawn(async move {
                record_failure(&state, &watch_id, &rel_path).await;
                state.nw_unclaim(&watch_id, &rel_path);
            });
        }
        TransferEvent::Canceled { transfer_id } => {
            UPLOADS.remove(&transfer_id);
            debug!(watch = %watch_id, rel = %rel_path, "night watcher: upload canceled");
            cleanup_stage();
            state.nw_unclaim(&watch_id, &rel_path);
        }
        _ => {}
    })
}

pub(super) async fn record_failure<S: NwCtx>(state: &S, watch_id: &str, rel_path: &str) {
    match state
        .db()
        .file_retry_record_failure(watch_id, rel_path, MAX_UPLOAD_RETRIES, RETRY_PAUSE_SECS)
        .await
    {
        Ok(n) if n >= MAX_UPLOAD_RETRIES => {
            warn!(watch = %watch_id, rel = %rel_path, fails = n, pause_secs = RETRY_PAUSE_SECS, "night watcher: pausing file after repeated upload failures");
        }
        Ok(_) => {}
        Err(e) => warn!(watch = %watch_id, rel = %rel_path, "night watcher: record retry failed: {e}"),
    }
}

fn build_key(prefix: &str, rel: &str) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        rel.to_string()
    } else {
        format!("{p}/{rel}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::build_key;
    use crate::night_watcher::fs::normalize_rel;

    #[test]
    fn build_key_joins_prefix_and_normalizes_slashes() {
        assert_eq!(build_key("", "a/b.txt"), "a/b.txt");
        assert_eq!(build_key("photos", "a/b.txt"), "photos/a/b.txt");
        assert_eq!(build_key("/photos/", "a/b.txt"), "photos/a/b.txt");
        assert_eq!(normalize_rel(Path::new("a").join("b.txt").as_path()), "a/b.txt");
    }
}
