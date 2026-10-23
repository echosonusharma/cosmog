//! Local mirror of remote listings (`cached_objects` + FTS5), fed by sync sweeps and write-through hooks.

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
mod browse;
mod facets;
mod index;
mod objects;
mod search;
mod stats;
#[cfg(test)]
mod tests;

pub use index::BucketIndexStatus;
pub use objects::SyncScope;
pub use facets::{FacetBucket, Facets};
pub use search::{SearchFilters, SearchQuery, SearchResult, SearchScope, SortBy, SortDir};
pub use stats::{BucketStats, FileTypeStat, LargeObjectStat, MonthStat, StorageClassStat};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedObjectMeta {
    pub account_id: String,
    pub bucket: String,
    pub key: String,
    pub size: i64,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub storage_class: Option<String>,
    pub content_type: Option<String>,
    pub extension: Option<String>,
    pub basename: String,
    pub version_id: Option<String>,
    pub synced_at: i64,
}

/// Computed once at upsert so search/facets hit indexed columns instead of LIKE/instr().
#[derive(Debug, Clone)]
pub struct KeyParts {
    pub basename: String,
    pub extension: Option<String>,
}

impl KeyParts {
    pub fn from_key(key: &str) -> Self {
        let base = match key.rfind('/') {
            Some(idx) => key[idx + 1..].to_string(),
            None => key.to_string(),
        };
        let extension = base
            .rfind('.')
            // Leading dot (".bashrc") = no extension.
            .filter(|&i| i > 0 && i + 1 < base.len())
            .map(|i| base[i + 1..].to_lowercase())
            // Alphanumerics/_/- only; reject junk that's never a real extension.
            .filter(|s| {
                s.chars().count() <= 16
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            });
        KeyParts {
            basename: base,
            extension,
        }
    }
}

/// Trigram tokenizer: terms under 3 chars are skipped; None means the caller falls back to LIKE.
pub fn build_fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if terms.is_empty() { None } else { Some(terms.join(" ")) }
}

/// Exclusive bound for a case-sensitive BINARY `key >= prefix AND key < bound` scan: UTF-8 byte order = code point order,
/// so bumping the last bumpable char bounds every extension. Nothing bumpable: an empty blob, since TEXT < BLOB.
fn prefix_upper_bound(prefix: &str) -> Value {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(c) = chars.pop() {
        let next = if c == '\u{D7FF}' { Some('\u{E000}') } else { char::from_u32(c as u32 + 1) };
        if let Some(n) = next {
            chars.push(n);
            return Value::Text(chars.into_iter().collect());
        }
    }
    Value::Blob(Vec::new())
}

/// 1-indexed CHARACTER offset just past `prefix` for substr(); byte length on multibyte prefixes ("文档/") misclassifies nested keys.
fn substr_offset_past_prefix(prefix: &str) -> i64 {
    prefix.chars().count() as i64 + 1
}

fn like_contains(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => { out.push('\\'); out.push(c); }
            _ => out.push(c),
        }
    }
    out.push('%');
    out
}

fn row_to_cached(row: &rusqlite::Row) -> rusqlite::Result<CachedObjectMeta> {
    Ok(CachedObjectMeta {
        account_id: row.get(0)?,
        bucket: row.get(1)?,
        key: row.get(2)?,
        size: row.get(3)?,
        etag: row.get(4)?,
        last_modified: row.get(5)?,
        storage_class: row.get(6)?,
        content_type: row.get(7)?,
        extension: row.get(8)?,
        basename: row.get(9)?,
        version_id: row.get(10)?,
        synced_at: row.get(11)?,
    })
}

const SELECT_COLS: &str = "co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at";
