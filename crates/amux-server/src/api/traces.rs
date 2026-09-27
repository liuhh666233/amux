//! GET /api/traces — the agent trace archive (runtime_jobs/trace_archive.rs).
//!
//! `/` lists archived traces, newest first, filterable by `worker` and
//! `provider`, with fleet totals and the retention setting that decides how
//! long Claude Code keeps its own copies. `/file?source=<source_path>` returns
//! one trace as JSONL, read from the archive, so it works after the original
//! is gone. Only paths present in the index can be read.

use super::AppState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(list))
        .route("/file", axum::routing::get(file))
}

#[derive(Deserialize)]
struct ListQuery {
    worker: Option<String>,
    provider: Option<String>,
    limit: Option<i64>,
}

async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let limit = q.limit.unwrap_or(200).clamp(1, 5000);
    let worker = q.worker.unwrap_or_default();
    let provider = q.provider.unwrap_or_default();
    let retention = std::fs::read_to_string(
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/settings.json"),
    )
    .ok()
    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    .map(|v| v["cleanupPeriodDays"].clone())
    .unwrap_or(Value::Null);
    let store = state.store.clone();
    let out = tokio::task::spawn_blocking(move || -> rusqlite::Result<Value> {
        let conn = store.read().map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
        let totals = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(source_bytes),0), COALESCE(SUM(archive_bytes),0), COALESCE(SUM(source_deleted),0), \
             COALESCE(SUM(tool_calls),0), COALESCE(SUM(output_tokens),0) FROM trace_archive \
             WHERE (?1 = '' OR worker = ?1) AND (?2 = '' OR provider = ?2)",
            rusqlite::params![worker, provider],
            |r| Ok(json!({"traces": r.get::<_, i64>(0)?, "source_bytes": r.get::<_, i64>(1)?, "archive_bytes": r.get::<_, i64>(2)?,
                          "only_in_archive": r.get::<_, i64>(3)?, "tool_calls": r.get::<_, i64>(4)?, "output_tokens": r.get::<_, i64>(5)?})),
        )?;
        let mut st = conn.prepare(
            "SELECT source_path, provider, kind, conv_id, parent_conv, worker, archive_bytes, source_bytes, first_ts, last_ts, \
             records, user_msgs, tool_calls, tool_errors, output_tokens, source_deleted, archived_at FROM trace_archive \
             WHERE (?1 = '' OR worker = ?1) AND (?2 = '' OR provider = ?2) ORDER BY last_ts DESC LIMIT ?3",
        )?;
        let rows: Vec<Value> = st
            .query_map(rusqlite::params![worker, provider, limit], |r| {
                Ok(json!({
                    "source": r.get::<_, String>(0)?, "provider": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                    "conv_id": r.get::<_, String>(3)?, "parent_conv": r.get::<_, String>(4)?, "worker": r.get::<_, String>(5)?,
                    "archive_bytes": r.get::<_, i64>(6)?, "source_bytes": r.get::<_, i64>(7)?,
                    "first_ts": r.get::<_, String>(8)?, "last_ts": r.get::<_, String>(9)?,
                    "records": r.get::<_, i64>(10)?, "user_msgs": r.get::<_, i64>(11)?, "tool_calls": r.get::<_, i64>(12)?,
                    "tool_errors": r.get::<_, i64>(13)?, "output_tokens": r.get::<_, i64>(14)?,
                    "only_in_archive": r.get::<_, i64>(15)? == 1, "archived_at": r.get::<_, f64>(16)?,
                }))
            })?
            .flatten()
            .collect();
        Ok(json!({"totals": totals, "traces": rows}))
    })
    .await;
    match out {
        Ok(Ok(mut v)) => {
            let n = v["traces"].as_array().map(Vec::len).unwrap_or(0);
            v["measured"] = json!(true);
            v["n_considered"] = json!(n);
            v["claude_cleanup_period_days"] = retention;
            Json(v).into_response()
        }
        Ok(Err(e)) => Json(json!({"measured": false, "n_considered": 0, "why_unmeasured": e.to_string()})).into_response(),
        Err(e) => Json(json!({"measured": false, "n_considered": 0, "why_unmeasured": e.to_string()})).into_response(),
    }
}

#[derive(Deserialize)]
struct FileQuery {
    source: String,
}

async fn file(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let store = state.store.clone();
    let source = q.source.clone();
    let archive: Option<String> = tokio::task::spawn_blocking(move || {
        store.read().ok().and_then(|c| {
            c.query_row("SELECT archive_path FROM trace_archive WHERE source_path=?1", [&source], |r| r.get(0)).ok()
        })
    })
    .await
    .ok()
    .flatten();
    let Some(path) = archive else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no archived trace for that source path"}))).into_response();
    };
    let body = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(std::fs::File::open(path)?), &mut out)?;
        Ok(out)
    })
    .await;
    match body {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/x-ndjson")], bytes).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("archive unreadable: {e}")}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
