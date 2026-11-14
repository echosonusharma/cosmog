//! Which objects this job wrote: every upload carries a tag naming the job, the item and the
//! local version it was cut from, so a lost response or a crash is never mistaken for a foreign key.

use crate::db::folder_upload::ItemRow;
use crate::store::ObjectMeta;

/// User metadata key (sent as `x-amz-meta-cosmog-fu`).
pub(super) const TAG_KEY: &str = "cosmog-fu";

pub(super) fn tag(job_id: &str, seq: i64, size: i64, mtime_ms: i64) -> String {
    format!("{job_id}/{seq}/{size}/{mtime_ms}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Owner {
    /// Not written by this job for this item.
    Foreign,
    /// Written by this job; `current` when cut from the local version recorded now.
    Ours { current: bool },
}

pub(super) fn owner(meta: &ObjectMeta, job_id: &str, item: &ItemRow) -> Owner {
    let want = tag(job_id, item.seq, item.size, item.mtime_ms);
    match meta.user_metadata.get(TAG_KEY) {
        Some(t) if *t == want => Owner::Ours { current: true },
        Some(t) if t.starts_with(&format!("{job_id}/{}/", item.seq)) => Owner::Ours { current: false },
        // Providers that drop user metadata: fall back to the ETag this job stored.
        _ if item.done_etag.is_some() && meta.etag == item.done_etag => Owner::Ours { current: false },
        _ => Owner::Foreign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::folder_upload::{ItemKind, ItemState};

    fn item(seq: i64, size: i64, etag: Option<&str>) -> ItemRow {
        ItemRow {
            job_id: "j".into(),
            seq,
            kind: ItemKind::File,
            rel_path: "a".into(),
            source_ref: None,
            size,
            mtime_ms: 7,
            ext: None,
            hidden: false,
            link_target: None,
            state: ItemState::Pending,
            exclude_reason: None,
            remote_exists: false,
            remote_size: None,
            remote_mtime: None,
            attempts: 0,
            next_attempt_at: 0,
            transfer_id: None,
            drift: None,
            error: None,
            updated_at: 0,
            done_etag: etag.map(str::to_string),
        }
    }

    fn meta(t: Option<String>, etag: Option<&str>) -> ObjectMeta {
        ObjectMeta {
            key: "k".into(),
            size: 1,
            etag: etag.map(str::to_string),
            last_modified: None,
            storage_class: None,
            content_type: None,
            version_id: None,
            user_metadata: t.map(|t| [(TAG_KEY.to_string(), t)].into_iter().collect()).unwrap_or_default(),
        }
    }

    #[test]
    fn tag_tells_versions_and_items_apart() {
        assert_eq!(owner(&meta(Some(tag("j", 3, 10, 7)), None), "j", &item(3, 10, None)), Owner::Ours { current: true });
        assert_eq!(owner(&meta(Some(tag("j", 3, 9, 7)), None), "j", &item(3, 10, None)), Owner::Ours { current: false });
        // seq 3 must not match seq 30, nor another job.
        assert_eq!(owner(&meta(Some(tag("j", 30, 10, 7)), None), "j", &item(3, 10, None)), Owner::Foreign);
        assert_eq!(owner(&meta(Some(tag("k", 3, 10, 7)), None), "j", &item(3, 10, None)), Owner::Foreign);
        assert_eq!(owner(&meta(None, Some("e1")), "j", &item(3, 10, Some("e1"))), Owner::Ours { current: false });
        assert_eq!(owner(&meta(None, Some("e2")), "j", &item(3, 10, Some("e1"))), Owner::Foreign);
        assert_eq!(owner(&meta(None, None), "j", &item(3, 10, None)), Owner::Foreign);
    }
}
