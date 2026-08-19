mod common;

use chrono::Utc;
use cosmog_lib::db::request_logs::NewRequestLog;

fn sample_log(
    account_id: &str,
    account_name: &str,
    operation: &str,
    status: &str,
    bucket: Option<&str>,
    duration_ms: i64,
) -> NewRequestLog {
    NewRequestLog {
        account_id: Some(account_id.into()),
        account_name: Some(account_name.into()),
        operation: operation.into(),
        http_method: Some("GET".into()),
        request_url: None,
        request_params: None,
        response_meta: None,
        bucket: bucket.map(str::to_string),
        key: bucket.map(|_| "obj.txt".into()),
        status: status.into(),
        response_status: Some(if status == "ok" { 200 } else { 403 }),
        error_code: if status == "error" {
            Some("AccessDenied".into())
        } else {
            None
        },
        error_msg: None,
        duration_ms,
    }
}

#[tokio::test]
async fn request_log_stats_empty_table() {
    let (db, _td) = common::tmp_db().await;
    let since = Utc::now().timestamp() - 86_400;

    let stats = db.request_log_stats(since).await.unwrap();
    assert_eq!(stats.total, 0);
    assert_eq!(stats.ok_count, 0);
    assert_eq!(stats.error_count, 0);
    assert_eq!(stats.avg_duration_ms, 0);
    assert!(stats.by_account.is_empty());
    assert!(stats.by_operation.is_empty());
    assert!(stats.by_day.is_empty());
    assert!(stats.by_day_by_account.is_empty());
    assert!(stats.top_buckets.is_empty());
}

#[tokio::test]
async fn request_log_stats_aggregates_rows() {
    let (db, _td) = common::tmp_db().await;
    let since = Utc::now().timestamp() - 86_400 * 7;

    db.insert_request_log(sample_log(
        "acct-a",
        "Alpha",
        "list_objects",
        "ok",
        Some("photos"),
        100,
    ))
    .await
    .unwrap();
    db.insert_request_log(sample_log(
        "acct-a",
        "Alpha",
        "get_object",
        "ok",
        Some("photos"),
        300,
    ))
    .await
    .unwrap();
    db.insert_request_log(sample_log(
        "acct-b",
        "Beta",
        "put_object",
        "error",
        Some("archive"),
        500,
    ))
    .await
    .unwrap();
    db.insert_request_log(sample_log(
        "acct-b",
        "Beta",
        "list_objects",
        "ok",
        None,
        50,
    ))
    .await
    .unwrap();

    let stats = db.request_log_stats(since).await.unwrap();
    assert_eq!(stats.total, 4);
    assert_eq!(stats.ok_count, 3);
    assert_eq!(stats.error_count, 1);
    assert_eq!(stats.avg_duration_ms, 237);

    assert_eq!(stats.by_account.len(), 2);
    assert_eq!(stats.by_account[0].count, 2);
    assert_eq!(stats.by_account[0].account_name.as_deref(), Some("Alpha"));

    assert_eq!(stats.by_operation.len(), 3);
    let list_op = stats
        .by_operation
        .iter()
        .find(|o| o.operation == "list_objects")
        .unwrap();
    assert_eq!(list_op.count, 2);

    assert_eq!(stats.by_day.len(), 1);
    assert_eq!(stats.by_day[0].count, 4);
    assert_eq!(stats.by_day[0].error_count, 1);

    assert_eq!(stats.by_day_by_account.len(), 2);
    assert_eq!(stats.top_buckets.len(), 2);
    assert_eq!(stats.top_buckets[0].bucket, "photos");
    assert_eq!(stats.top_buckets[0].count, 2);
}

#[tokio::test]
async fn request_log_stats_respects_since_cutoff() {
    let (db, _td) = common::tmp_db().await;

    db.insert_request_log(sample_log(
        "acct-a",
        "Alpha",
        "list_objects",
        "ok",
        Some("old"),
        100,
    ))
    .await
    .unwrap();

    let since = Utc::now().timestamp() + 86_400;
    let stats = db.request_log_stats(since).await.unwrap();
    assert_eq!(stats.total, 0);
}
