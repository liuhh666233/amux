//! Trace archive: every agent trace amux's workers produce is kept, indexed
//! and backed up.
//!
//! # Why this exists
//!
//! A trace is the full record of a run: the prompts, the agent's reasoning,
//! its tool calls and their results, errors, and the owner's feedback. It is
//! how cost and latency are understood, how a failure is diagnosed, and the
//! raw material for improving agents over time ("Agent traces are the new
//! oil", Sam Z Liu, 2026-09-25, raised by Ethan the next day).
//!
//! amux was keeping none of it. Measured 2026-09-26: Claude Code deletes
//! sessions after 30 days by default and `cleanupPeriodDays` was unset (the
//! oldest amux transcript on disk was exactly 30 days old), and the restic
//! backup excluded every transcript as "regenerable churn". 14 GB of Claude
//! transcripts and 6.8 GB of Codex rollouts were one cleanup away from gone.
//!
//! # What it does
//!
//! 1. Retention guard: sets `cleanupPeriodDays` in `~/.claude/settings.json`
//!    when it is missing (`trace_retention_set`) and warns when an explicit
//!    value is under a year (`trace_retention_short`). An explicit value is
//!    the owner's and is not overwritten.
//! 2. Archive: a transcript quiet for 2 hours is gzipped into
//!    `~/.amux/traces/<provider>/...` and indexed (`trace_archive` table) with
//!    its worker, conversation, time span, message, tool-call, error and token
//!    counts. A transcript that grows again is re-archived. Oldest first, a
//!    bounded number of bytes per tick, skipped while the host is critical.
//!    `~/.amux` is a backup source, so the archive is backed up.
//! 3. When an original disappears, its row is marked `source_deleted`: the
//!    trace now exists only in the archive.
//!
//! `GET /api/traces` lists them; `GET /api/traces/file?source=` returns one.

use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use super::registry::ids;

const JOB: &str = ids::TRACE_ARCHIVE;

fn env_f64(k: &str, d: f64) -> f64 {
    std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d)
}

fn user_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn archive_root() -> PathBuf {
    crate::config::amux_home().join("traces")
}

// ---- retention guard --------------------------------------------------------

/// What to do about Claude Code's cleanup setting. Pure.
pub fn retention_action(settings: &Value) -> Option<&'static str> {
    match settings.get("cleanupPeriodDays").and_then(Value::as_i64) {
        None => Some("set"),
        Some(d) if d < 365 => Some("short"),
        _ => None,
    }
}

fn retention_guard() {
    let path = user_home().join(".claude/settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else { return };
    match retention_action(&v) {
        Some("set") => {
            v["cleanupPeriodDays"] = Value::from(3650);
            let tmp = path.with_extension("json.amux-tmp");
            let ok = serde_json::to_string_pretty(&v)
                .ok()
                .and_then(|s| std::fs::write(&tmp, s + "\n").ok())
                .and_then(|()| std::fs::rename(&tmp, &path).ok())
                .is_some();
            tracing::warn!(target: "amux::traces", verdict = "trace_retention_set", ok, measured = true, n_considered = 1,
                "Claude Code cleanupPeriodDays was unset (30-day deletion); set to 3650 so traces are not deleted");
        }
        Some("short") => {
            let key = "trace-retention-short".to_string();
            if crate::log_dedupe::first_this_bucket(&key, crate::log_dedupe::hour_bucket(crate::config::now_f64())) {
                tracing::warn!(target: "amux::traces", verdict = "trace_retention_short", measured = true, n_considered = 1,
                    days = v["cleanupPeriodDays"].as_i64().unwrap_or(0),
                    "Claude Code deletes sessions after fewer than 365 days; traces older than that survive only in the amux archive");
            }
        }
        _ => {}
    }
}

// ---- sources ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub provider: &'static str,
    pub kind: &'static str,
    pub conv_id: String,
    pub parent_conv: String,
    pub rel: PathBuf,
    pub bytes: u64,
    pub mtime: f64,
}

fn mtime_of(m: &std::fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn push_source(out: &mut Vec<Source>, path: PathBuf, provider: &'static str, kind: &'static str, conv: String, parent: String, rel: PathBuf) {
    if let Ok(m) = std::fs::metadata(&path) {
        out.push(Source { bytes: m.len(), mtime: mtime_of(&m), path, provider, kind, conv_id: conv, parent_conv: parent, rel });
    }
}

fn stem(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

fn claude_sources(out: &mut Vec<Source>) {
    let root = user_home().join(".claude/projects");
    let Ok(projects) = std::fs::read_dir(&root) else { return };
    for proj in projects.flatten() {
        let pdir = proj.path();
        let pname = proj.file_name().to_string_lossy().to_string();
        let Ok(entries) = std::fs::read_dir(&pdir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                let rel = PathBuf::from(&pname).join(p.file_name().unwrap_or_default());
                push_source(out, p.clone(), "claude", "session", stem(&p), String::new(), rel);
            } else if p.is_dir() {
                let conv = e.file_name().to_string_lossy().to_string();
                let Ok(subs) = std::fs::read_dir(p.join("subagents")) else { continue };
                for s in subs.flatten() {
                    let sp = s.path();
                    if sp.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                        let rel = PathBuf::from(&pname).join(&conv).join("subagents").join(sp.file_name().unwrap_or_default());
                        push_source(out, sp.clone(), "claude", "subagent", stem(&sp), conv.clone(), rel);
                    }
                }
            }
        }
    }
}

fn codex_sources(out: &mut Vec<Source>) {
    let root = user_home().join(".codex/sessions");
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                let rel = p.strip_prefix(&root).map(Path::to_path_buf).unwrap_or_else(|_| PathBuf::from(p.file_name().unwrap_or_default()));
                push_source(out, p.clone(), "codex", "session", stem(&p), String::new(), rel);
            }
        }
    }
}

// ---- stats --------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stats {
    pub records: i64,
    pub user_msgs: i64,
    pub tool_calls: i64,
    pub tool_errors: i64,
    pub output_tokens: i64,
    pub first_ts: String,
    pub last_ts: String,
}

/// Fold one transcript record into the stats. Covers Claude Code records and
/// Codex rollout items; anything unrecognised still counts as a record.
pub fn fold(st: &mut Stats, r: &Value) {
    st.records += 1;
    let ts = r["timestamp"].as_str().unwrap_or("");
    if !ts.is_empty() {
        if st.first_ts.is_empty() {
            st.first_ts = ts.to_string();
        }
        st.last_ts = ts.to_string();
    }
    let content = &r["message"]["content"];
    match r["type"].as_str() {
        Some("user") => match content {
            Value::String(s) if !s.trim().is_empty() && !s.trim_start().starts_with('<') => st.user_msgs += 1,
            Value::Array(parts) => {
                if parts.iter().any(|p| p["type"] == "text" && !p["text"].as_str().unwrap_or("").trim_start().starts_with('<')) {
                    st.user_msgs += 1;
                }
                st.tool_errors += parts.iter().filter(|p| p["type"] == "tool_result" && p["is_error"] == true).count() as i64;
            }
            _ => {}
        },
        Some("assistant") => {
            if let Value::Array(parts) = content {
                st.tool_calls += parts.iter().filter(|p| p["type"] == "tool_use").count() as i64;
            }
            st.output_tokens += r["message"]["usage"]["output_tokens"].as_i64().unwrap_or(0);
        }
        Some("response_item") => {
            let p = &r["payload"];
            match p["type"].as_str() {
                Some("function_call") | Some("custom_tool_call") | Some("local_shell_call") => st.tool_calls += 1,
                Some("message") if p["role"] == "user" => st.user_msgs += 1,
                _ => {}
            }
        }
        Some("event_msg") => {
            let p = &r["payload"];
            if p["type"] == "token_count" {
                if let Some(o) = p["info"]["last_token_usage"]["output_tokens"].as_i64() {
                    st.output_tokens += o;
                }
            }
        }
        _ => {}
    }
}

/// Stream `src` into a gzip at `dst` (via a temp file and rename), folding
/// stats on the way. Returns the stats and the archive size.
fn archive_one(src: &Path, dst: &Path) -> std::io::Result<(Stats, u64)> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dst.with_extension("gz.tmp");
    let reader = std::io::BufReader::new(std::fs::File::open(src)?);
    let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(&tmp)?, flate2::Compression::new(6));
    let mut st = Stats::default();
    for line in reader.split(b'\n') {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&line) {
            fold(&mut st, &v);
        } else {
            st.records += 1;
        }
        gz.write_all(&line)?;
        gz.write_all(b"\n")?;
    }
    gz.finish()?.sync_all()?;
    std::fs::rename(&tmp, dst)?;
    let size = std::fs::metadata(dst)?.len();
    Ok((st, size))
}

// ---- tick ---------------------------------------------------------------------

fn codex_owners() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(crate::api::session_verbs::home().join("sessions")) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("env") {
            continue;
        }
        let lane = stem(&p);
        let id = crate::api::session_verbs::meta_str(&crate::api::session_verbs::load_meta(&lane), "codex_session_id");
        if !id.is_empty() {
            out.insert(id, lane);
        }
    }
    out
}

fn owner_of(src: &Source, claims: &std::collections::BTreeMap<String, String>, codex: &HashMap<String, String>) -> String {
    match (src.provider, src.kind) {
        ("claude", "session") => crate::api::session_verbs::conversation_owner(&src.path, claims),
        ("claude", _) => {
            let parent = src.path.parent().and_then(Path::parent).map(|d| d.with_extension("jsonl"));
            parent.map(|p| crate::api::session_verbs::conversation_owner(&p, claims)).unwrap_or_default()
        }
        _ => codex.iter().find(|(id, _)| src.conv_id.contains(id.as_str())).map(|(_, w)| w.clone()).unwrap_or_default(),
    }
}

/// Which sources need archiving: quiet long enough and new or changed since
/// the last archive. Oldest first, because the oldest are nearest deletion.
pub fn pick<'a>(sources: &'a [Source], known: &HashMap<String, (i64, f64)>, now: f64, quiet_s: f64) -> Vec<&'a Source> {
    let mut v: Vec<&Source> = sources
        .iter()
        .filter(|s| now - s.mtime >= quiet_s)
        .filter(|s| match known.get(&s.path.to_string_lossy().to_string()) {
            Some((b, m)) => *b != s.bytes as i64 || (*m - s.mtime).abs() > 1.0,
            None => true,
        })
        .collect();
    v.sort_by(|a, b| a.mtime.partial_cmp(&b.mtime).unwrap_or(std::cmp::Ordering::Equal));
    v
}

async fn tick(app: crate::api::AppState) {
    tokio::task::spawn_blocking(retention_guard).await.ok();
    if crate::runtime_jobs::host_guard::critical_now().is_some() {
        tracing::info!(target: "amux::traces", verdict = "trace_archive_deferred", measured = true, n_considered = 0,
            "host is critical; trace archiving waits");
        return;
    }
    let known: HashMap<String, (i64, f64)> = app
        .store
        .read()
        .ok()
        .and_then(|c| {
            let mut st = c.prepare("SELECT source_path, source_bytes, source_mtime FROM trace_archive").ok()?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, f64>(2)?))))
                .ok()?
                .flatten()
                .collect();
            Some(rows)
        })
        .unwrap_or_default();
    let budget = env_f64("AMUX_TRACE_ARCHIVE_TICK_MB", 400.0) * 1_048_576.0;
    let quiet = env_f64("AMUX_TRACE_ARCHIVE_QUIET_S", 7200.0);
    let root = archive_root();
    let result = tokio::task::spawn_blocking(move || {
        let mut sources = Vec::new();
        claude_sources(&mut sources);
        codex_sources(&mut sources);
        let now = crate::config::now_f64();
        let live: std::collections::HashSet<String> = sources.iter().map(|s| s.path.to_string_lossy().to_string()).collect();
        let deleted: Vec<String> = known.keys().filter(|k| !live.contains(*k)).cloned().collect();
        let todo = pick(&sources, &known, now, quiet);
        let backlog = todo.len();
        let claims = crate::api::session_verbs::conversation_claims();
        let codex = codex_owners();
        let mut rows = Vec::new();
        let mut spent = 0.0;
        for s in todo {
            if spent >= budget {
                break;
            }
            let dst = root.join(s.provider).join(format!("{}.gz", s.rel.to_string_lossy()));
            match archive_one(&s.path, &dst) {
                Ok((st, gz)) => {
                    spent += s.bytes as f64;
                    rows.push((s.clone(), owner_of(s, &claims, &codex), dst, gz, st));
                }
                Err(e) => tracing::warn!(target: "amux::traces", verdict = "trace_archive_failed", path = %s.path.display(),
                    error = %e, measured = true, n_considered = 1, "could not archive a trace"),
            }
        }
        (rows, deleted, backlog, sources.len())
    })
    .await;
    let Ok((rows, deleted, backlog, total)) = result else { return };
    let archived = rows.len();
    let bytes: u64 = rows.iter().map(|r| r.0.bytes).sum();
    let now = crate::config::now_f64();
    let n_deleted = deleted.len();
    let _ = app
        .store
        .write_async(move |conn| {
            for (s, worker, dst, gz, st) in &rows {
                conn.execute(
                    "INSERT INTO trace_archive(source_path, provider, kind, conv_id, parent_conv, worker, archive_path, source_bytes, \
                     archive_bytes, source_mtime, archived_at, first_ts, last_ts, records, user_msgs, tool_calls, tool_errors, output_tokens, source_deleted) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,0) \
                     ON CONFLICT(source_path) DO UPDATE SET worker=excluded.worker, archive_path=excluded.archive_path, \
                     source_bytes=excluded.source_bytes, archive_bytes=excluded.archive_bytes, source_mtime=excluded.source_mtime, \
                     archived_at=excluded.archived_at, first_ts=excluded.first_ts, last_ts=excluded.last_ts, records=excluded.records, \
                     user_msgs=excluded.user_msgs, tool_calls=excluded.tool_calls, tool_errors=excluded.tool_errors, \
                     output_tokens=excluded.output_tokens, source_deleted=0",
                    rusqlite::params![
                        s.path.to_string_lossy(), s.provider, s.kind, s.conv_id, s.parent_conv, worker,
                        dst.to_string_lossy(), s.bytes as i64, *gz as i64, s.mtime, now, st.first_ts, st.last_ts,
                        st.records, st.user_msgs, st.tool_calls, st.tool_errors, st.output_tokens
                    ],
                )?;
            }
            for d in &deleted {
                conn.execute("UPDATE trace_archive SET source_deleted=1 WHERE source_path=?1", [d])?;
            }
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        })
        .await;
    tracing::info!(target: "amux::traces", verdict = "trace_archive_tick", archived, bytes, backlog = backlog.saturating_sub(archived),
        sources_deleted = n_deleted, measured = true, n_considered = total,
        "archived {archived} trace(s); {} still waiting", backlog.saturating_sub(archived));
}

/// Every 10 minutes: at most `AMUX_TRACE_ARCHIVE_TICK_MB` (400) per tick, so
/// the first backfill spreads over hours instead of one burst.
pub fn spawn(app: crate::api::AppState) -> super::PeriodicTask {
    super::spawn_periodic(JOB, 600, move || {
        let app = app.clone();
        async move { tick(app).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retention_is_set_when_missing_and_only_warned_when_short() {
        assert_eq!(retention_action(&json!({})), Some("set"));
        assert_eq!(retention_action(&json!({"cleanupPeriodDays": 30})), Some("short"));
        assert_eq!(retention_action(&json!({"cleanupPeriodDays": 3650})), None);
    }

    #[test]
    fn folds_claude_and_codex_records() {
        let mut st = Stats::default();
        for r in [
            json!({"type":"user","timestamp":"2026-09-26T10:00:00Z","message":{"content":"fix the build"}}),
            json!({"type":"user","timestamp":"2026-09-26T10:00:01Z","message":{"content":"<task-notification>x"}}),
            json!({"type":"assistant","timestamp":"2026-09-26T10:00:05Z","message":{"content":[{"type":"tool_use"},{"type":"text","text":"ok"}],"usage":{"output_tokens":42}}}),
            json!({"type":"user","timestamp":"2026-09-26T10:00:09Z","message":{"content":[{"type":"tool_result","is_error":true}]}}),
            json!({"type":"response_item","timestamp":"2026-09-26T10:01:00Z","payload":{"type":"function_call"}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"output_tokens":8}}}}),
        ] {
            fold(&mut st, &r);
        }
        assert_eq!((st.records, st.user_msgs, st.tool_calls, st.tool_errors, st.output_tokens), (6, 1, 2, 1, 50));
        assert_eq!((st.first_ts.as_str(), st.last_ts.as_str()), ("2026-09-26T10:00:00Z", "2026-09-26T10:01:00Z"));
    }

    #[test]
    fn archives_round_trip_and_picks_quiet_changed_sources_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.jsonl");
        std::fs::write(&src, "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n\n{bad json}\n").unwrap();
        let dst = dir.path().join("out/a.jsonl.gz");
        let (st, gz) = archive_one(&src, &dst).unwrap();
        assert!(gz > 0 && st.records == 2 && st.user_msgs == 1);
        let mut back = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(std::fs::File::open(&dst).unwrap()), &mut back).unwrap();
        assert_eq!(back, "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n{bad json}\n");
        let mk = |p: &str, bytes: u64, mtime: f64| Source { path: PathBuf::from(p), provider: "claude", kind: "session",
            conv_id: String::new(), parent_conv: String::new(), rel: PathBuf::from(p), bytes, mtime };
        let sources = vec![mk("/new", 10, 9_000.0), mk("/old", 10, 1_000.0), mk("/hot", 10, 9_990.0), mk("/same", 5, 2_000.0)];
        let known = HashMap::from([("/same".to_string(), (5i64, 2_000.0f64))]);
        let picked: Vec<String> = pick(&sources, &known, 10_000.0, 60.0).iter().map(|s| s.path.to_string_lossy().to_string()).collect();
        assert_eq!(picked, vec!["/old", "/new"], "quiet and changed only, oldest first");
    }
}
