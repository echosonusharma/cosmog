use chrono::Utc;
use rusqlite::types::Value;
use serde::Serialize;

use super::search::{build_filter, SearchFilters, SearchScope};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Facets {
    pub extensions: Vec<FacetBucket>,
    pub size_buckets: Vec<FacetBucket>,
    pub date_buckets: Vec<FacetBucket>,
    pub storage_classes: Vec<FacetBucket>,
    pub content_types: Vec<FacetBucket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FacetBucket {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FacetDim {
    Extension,
    Size,
    Date,
    StorageClass,
    ContentType,
}

pub(super) fn compute_facets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    tm: TextMatch,
) -> Result<Facets, tokio_rusqlite::Error> {
    let mut facets = Facets::default();

    facets.extensions =
        facet_group(conn, account_id, bucket, scope, filters, tm, FacetDim::Extension, "co.extension")?;
    facets.storage_classes = facet_group(
        conn,
        account_id,
        bucket,
        scope,
        filters,
        tm,
        FacetDim::StorageClass,
        "co.storage_class",
    )?;
    facets.content_types = facet_group(
        conn,
        account_id,
        bucket,
        scope,
        filters,
        tm,
        FacetDim::ContentType,
        "co.content_type",
    )?;

    facets.size_buckets = facet_size_buckets(conn, account_id, bucket, scope, filters, tm)?;
    facets.date_buckets = facet_date_buckets(conn, account_id, bucket, scope, filters, tm)?;

    Ok(facets)
}

#[derive(Clone, Copy)]
pub(super) struct TextMatch<'a> {
    pub(super) fts: Option<&'a str>,
    pub(super) likes: &'a [String],
}

fn text_match(tm: TextMatch, params: &mut Vec<Value>) -> (&'static str, String) {
    let mut clause = String::new();
    let join = match tm.fts {
        Some(text) => {
            clause.push_str(" AND cached_objects_fts MATCH ?");
            params.push(Value::Text(text.to_string()));
            "JOIN cached_objects_fts fts ON fts.rowid = co.rowid"
        }
        None => "",
    };
    for t in tm.likes {
        clause.push_str(" AND co.basename LIKE ? ESCAPE '\\'");
        params.push(Value::Text(t.clone()));
    }
    (join, clause)
}

fn facet_group(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    tm: TextMatch,
    dim: FacetDim,
    col: &str,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let (filter_sql, mut params) = build_filter(account_id, bucket, scope, filters, Some(dim));
    let (join_sql, fts_clause) = text_match(tm, &mut params);

    let sql = format!(
        "SELECT {col} AS v, COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
         AND {col} IS NOT NULL AND {col} != ''
         GROUP BY {col}
         ORDER BY COUNT(*) DESC
         LIMIT 50"
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let v: Option<String> = row.get(0)?;
        let count: i64 = row.get(1)?;
        if let Some(v) = v {
            out.push(FacetBucket { value: v, count });
        }
    }
    Ok(out)
}

const SIZE_BUCKETS: &[(&str, i64, i64)] = &[
    ("<1KB", 0, 1024 - 1),
    ("1KB-1MB", 1024, 1024 * 1024 - 1),
    ("1MB-100MB", 1024 * 1024, 100 * 1024 * 1024 - 1),
    ("100MB-1GB", 100 * 1024 * 1024, 1024 * 1024 * 1024 - 1),
    (">1GB", 1024 * 1024 * 1024, i64::MAX),
];

fn facet_size_buckets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    tm: TextMatch,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let mut out = Vec::with_capacity(SIZE_BUCKETS.len());
    for (label, lo, hi) in SIZE_BUCKETS {
        let (filter_sql, mut params) =
            build_filter(account_id, bucket, scope, filters, Some(FacetDim::Size));
        let (join_sql, fts_clause) = text_match(tm, &mut params);
        params.push(Value::Integer(*lo));
        params.push(Value::Integer(*hi));

        let sql = format!(
            "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
             AND co.size BETWEEN ? AND ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
        out.push(FacetBucket {
            value: (*label).to_string(),
            count,
        });
    }
    Ok(out)
}

fn facet_date_buckets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    tm: TextMatch,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let now = Utc::now().timestamp();
    let day = 86_400;
    let ranges: &[(&str, i64)] = &[
        ("Last 24h", now - day),
        ("Last 7 days", now - 7 * day),
        ("Last 30 days", now - 30 * day),
        ("Last year", now - 365 * day),
    ];

    let mut out = Vec::with_capacity(ranges.len() + 1);
    for (label, since) in ranges {
        let (filter_sql, mut params) =
            build_filter(account_id, bucket, scope, filters, Some(FacetDim::Date));
        let (join_sql, fts_clause) = text_match(tm, &mut params);
        params.push(Value::Integer(*since));

        let sql = format!(
            "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
             AND co.last_modified >= ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
        out.push(FacetBucket {
            value: (*label).to_string(),
            count,
        });
    }

    // "Older": before the 1-year window, plus rows with no last_modified.
    let (filter_sql, mut params) =
        build_filter(account_id, bucket, scope, filters, Some(FacetDim::Date));
    let (join_sql, fts_clause) = text_match(tm, &mut params);
    let year_ago = now - 365 * day;
    params.push(Value::Integer(year_ago));
    let sql = format!(
        "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
         AND (co.last_modified IS NULL OR co.last_modified < ?)"
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
    out.push(FacetBucket {
        value: "Older".into(),
        count,
    });

    Ok(out)
}
