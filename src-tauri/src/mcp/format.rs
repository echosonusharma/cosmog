//! Response formatting: listings as compact CSV under a rough token budget so
//! huge listings can't blow the client context; errors as actionable `isError`.

use serde_json::{json, Value};

/// Rough response budget (~4 chars/token); CSV rows cut past this many chars.
const MAX_RESPONSE_CHARS: usize = 100_000;

pub fn text(body: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": body.into() }],
        "isError": false,
    })
}

/// An error result. `isError` lets the model see and recover rather than the
/// call failing at the protocol layer.
pub fn error(body: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": body.into() }],
        "isError": true,
    })
}

/// Escape one CSV field per RFC 4180: quote when it holds a comma, quote, or
/// newline, and double any embedded quotes.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Builds CSV from a header and rows, cutting rows at the token budget;
/// returns `(csv, rendered_row_count, truncated_by_budget)`.
pub fn csv_table(header: &[&str], rows: &[Vec<String>]) -> (String, usize, bool) {
    let mut out = String::new();
    out.push_str(&header.join(","));
    out.push('\n');
    let mut rendered = 0;
    let mut truncated = false;
    for row in rows {
        let line: String = row.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(",");
        if out.len() + line.len() + 1 > MAX_RESPONSE_CHARS {
            truncated = true;
            break;
        }
        out.push_str(&line);
        out.push('\n');
        rendered += 1;
    }
    (out, rendered, truncated)
}

/// Composes listing text with a footer so the model knows to page or narrow.
pub fn listing(
    csv: String,
    rendered: usize,
    has_more: bool,
    next_cursor: Option<String>,
    budget_truncated: bool,
) -> Value {
    // A budget cut drops rows mid-page; the store cursor resumes after the
    // full page and would silently skip rows, so suppress it and flag more.
    let has_more = has_more || budget_truncated;
    let next_cursor = if budget_truncated { None } else { next_cursor };
    let mut footer = format!("\nrows: {rendered}\nhas_more: {has_more}");
    if let Some(c) = next_cursor {
        footer.push_str(&format!("\nnext_cursor: {c}"));
    }
    if budget_truncated {
        footer.push_str("\ntruncation_reason: response token budget reached, narrow the prefix or query");
    } else if has_more {
        footer.push_str("\ntruncation_reason: page limit reached, pass next_cursor to continue");
    }
    text(format!("{csv}{footer}"))
}
