use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use super::facets::{compute_facets, FacetDim, Facets, TextMatch};
use super::{build_fts_query, like_contains, prefix_upper_bound, row_to_cached, CachedObjectMeta, SELECT_COLS};
use crate::db::Db;
use crate::error::AppResult;

#[derive(Debug, Clone, Deserialize)]
pub struct SearchQuery {
    pub account_id: String,
    pub bucket: String,
    pub scope: SearchScope,
    pub query: Option<String>,
    #[serde(default)]
    pub filters: SearchFilters,
    #[serde(default)]
    pub sort: SortBy,
    #[serde(default)]
    pub sort_dir: SortDir,
    pub page_size: Option<u32>,
    pub cursor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchScope {
    /// `recursive=false` matches direct children only, `true` everything below.
    Prefix { prefix: String, recursive: bool },
    /// Runs against whatever is cached.
    Bucket,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchFilters {
    #[serde(default)]
    pub extensions: Vec<String>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    #[serde(default)]
    pub storage_classes: Vec<String>,
    #[serde(default)]
    pub content_types: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    #[default]
    Name,
    Size,
    Modified,
    Extension,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub objects: Vec<CachedObjectMeta>,
    pub total: i64,
    pub facets: Facets,
    pub next_cursor: Option<i64>,
}

pub(super) fn build_filter(
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    exclude_facet: Option<FacetDim>,
) -> (String, Vec<Value>) {
    let mut clauses = vec![
        String::from("co.account_id = ?"),
        String::from("co.bucket = ?"),
    ];
    let mut p: Vec<Value> = vec![
        Value::Text(account_id.to_string()),
        Value::Text(bucket.to_string()),
    ];

    match scope {
        SearchScope::Prefix { prefix, recursive } => {
            // Range, not LIKE: case-sensitive like S3 keys, and uses the PK index.
            clauses.push("co.key >= ? AND co.key < ?".into());
            p.push(Value::Text(prefix.clone()));
            p.push(prefix_upper_bound(prefix));
            if !recursive {
                // Exclude keys with a slash after the prefix (descendants).
                let mut pat = String::with_capacity(prefix.len() + 4);
                for c in prefix.chars() {
                    match c {
                        '%' | '_' | '\\' => { pat.push('\\'); pat.push(c); }
                        _ => pat.push(c),
                    }
                }
                pat.push_str("%/%");
                clauses.push("co.key NOT LIKE ? ESCAPE '\\'".into());
                p.push(Value::Text(pat));
            }
        }
        SearchScope::Bucket => {}
    }

    if !filters.extensions.is_empty() && exclude_facet != Some(FacetDim::Extension) {
        let placeholders = vec!["?"; filters.extensions.len()].join(",");
        clauses.push(format!("co.extension IN ({placeholders})"));
        for e in &filters.extensions {
            p.push(Value::Text(e.clone()));
        }
    }
    if !filters.storage_classes.is_empty() && exclude_facet != Some(FacetDim::StorageClass) {
        let placeholders = vec!["?"; filters.storage_classes.len()].join(",");
        clauses.push(format!("co.storage_class IN ({placeholders})"));
        for e in &filters.storage_classes {
            p.push(Value::Text(e.clone()));
        }
    }
    if !filters.content_types.is_empty() && exclude_facet != Some(FacetDim::ContentType) {
        let placeholders = vec!["?"; filters.content_types.len()].join(",");
        clauses.push(format!("co.content_type IN ({placeholders})"));
        for e in &filters.content_types {
            p.push(Value::Text(e.clone()));
        }
    }
    if exclude_facet != Some(FacetDim::Size) {
        if let Some(min) = filters.size_min {
            clauses.push("co.size >= ?".into());
            p.push(Value::Integer(min));
        }
        if let Some(max) = filters.size_max {
            clauses.push("co.size <= ?".into());
            p.push(Value::Integer(max));
        }
    }
    if exclude_facet != Some(FacetDim::Date) {
        if let Some(after) = filters.modified_after {
            clauses.push("co.last_modified >= ?".into());
            p.push(Value::Integer(after));
        }
        if let Some(before) = filters.modified_before {
            clauses.push("co.last_modified <= ?".into());
            p.push(Value::Integer(before));
        }
    }

    let where_sql = format!("WHERE {}", clauses.join(" AND "));
    (where_sql, p)
}

impl Db {
    pub async fn search_objects(&self, q: SearchQuery) -> AppResult<SearchResult> {
        let q_clone = q.clone();
        let result = self
            .conn
            .call(move |conn| {
                let SearchQuery {
                    account_id,
                    bucket,
                    scope,
                    query,
                    filters,
                    sort,
                    sort_dir,
                    page_size,
                    cursor,
                } = q_clone;

                let limit = page_size.unwrap_or(100).min(1000) as i64;

                let (filter_sql, filter_params) =
                    build_filter(&account_id, &bucket, &scope, &filters, None);

                let raw_query = query.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let fts = raw_query.and_then(build_fts_query);
                // Terms <3 chars can't use trigram FTS: LIKE on basename, AND'd with the FTS terms.
                let short_like_terms: Vec<String> = raw_query
                    .iter()
                    .flat_map(|q| q.split_whitespace())
                    .filter(|t| t.chars().count() < 3 && !t.is_empty())
                    .map(like_contains)
                    .collect();

                // FTS table must lead FROM for MATCH/BM25 rank, so fts_param goes first.
                let like_placeholders: String = short_like_terms
                    .iter()
                    .map(|_| "co.basename LIKE ? ESCAPE '\\'")
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let like_clause: String = if like_placeholders.is_empty() {
                    String::new()
                } else {
                    format!(" AND ({like_placeholders})")
                };

                let (from_sql, where_sql, base_params) = if let Some(text) = &fts {
                    let filter_inner = filter_sql
                        .strip_prefix("WHERE ")
                        .expect("build_filter always produces a WHERE-prefixed clause");
                    let mut params = vec![Value::Text(text.clone())];
                    params.extend(filter_params.iter().cloned());
                    (
                        "cached_objects_fts fts JOIN cached_objects co ON co.rowid = fts.rowid".to_string(),
                        format!("WHERE cached_objects_fts MATCH ? AND {filter_inner}"),
                        params,
                    )
                } else {
                    (
                        "cached_objects co".to_string(),
                        filter_sql.clone(),
                        filter_params.clone(),
                    )
                };

                let count_sql = format!(
                    "SELECT COUNT(*) FROM {from_sql} {where_sql} {like_clause}"
                );
                let total: i64 = {
                    let mut stmt = conn.prepare_cached(&count_sql)?;
                    let mut all = base_params.clone();
                    for p in &short_like_terms {
                        all.push(Value::Text(p.clone()));
                    }
                    let refs: Vec<&dyn rusqlite::ToSql> =
                        all.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                    stmt.query_row(refs.as_slice(), |row| row.get(0))?
                };

                // Offset, not keyset: sort columns aren't rowid-correlated, so a rowid cursor drops/dupes rows.
                let offset = cursor.unwrap_or(0).max(0);

                // FTS + default Name sort ranks by BM25 relevance, always ASC.
                let fts_name_sort = fts.is_some() && matches!(sort, SortBy::Name);
                let dir = match sort_dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" };
                let (order_col, order_dir_str) = match (sort, fts.is_some()) {
                    (SortBy::Name, true)  => ("fts.rank", "ASC"),
                    (SortBy::Name, false) => ("co.key", dir),
                    (SortBy::Size, _)     => ("co.size", dir),
                    (SortBy::Modified, _) => ("co.last_modified", dir),
                    (SortBy::Extension, _)=> ("co.extension", dir),
                };
                // rowid tiebreak keeps OFFSET page boundaries stable.
                let tiebreak = if fts_name_sort || matches!(sort_dir, SortDir::Asc) { "ASC" } else { "DESC" };

                let select_sql = format!(
                    "SELECT {SELECT_COLS} FROM {from_sql} {where_sql} {like_clause}
                     ORDER BY {order_col} {order_dir_str}, co.rowid {tiebreak}
                     LIMIT ? OFFSET ?"
                );
                let mut stmt = conn.prepare_cached(&select_sql)?;
                let mut all = base_params.clone();
                for p in &short_like_terms {
                    all.push(Value::Text(p.clone()));
                }
                all.push(Value::Integer(limit));
                all.push(Value::Integer(offset));
                let refs: Vec<&dyn rusqlite::ToSql> =
                    all.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                let mut rows = stmt.query(refs.as_slice())?;
                let mut objects = Vec::new();
                while let Some(row) = rows.next()? {
                    objects.push(row_to_cached(row)?);
                }
                let next_cursor = if objects.len() as i64 == limit { Some(offset + limit) } else { None };

                // Reuse the FTS query and short-term LIKEs so facet counts match the results.
                let tm = TextMatch { fts: fts.as_deref(), likes: &short_like_terms };
                let facets = compute_facets(conn, &account_id, &bucket, &scope, &filters, tm)?;

                Ok::<SearchResult, tokio_rusqlite::Error>(SearchResult {
                    objects,
                    total,
                    facets,
                    next_cursor,
                })
            })
            .await?;
        Ok(result)
    }
}
