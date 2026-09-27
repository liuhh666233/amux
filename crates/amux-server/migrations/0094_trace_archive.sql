-- Agent trace archive (2026-09-26, Ethan: "make sure we're capturing traces").
-- One row per archived transcript: Claude Code sessions and their subagents,
-- and Codex rollouts. The archive copy lives under ~/.amux/traces, so a trace
-- outlives Claude Code's own session cleanup (30 days by default) and is in
-- the backup. `source_deleted` marks traces whose original is gone and that
-- now exist only here.
CREATE TABLE IF NOT EXISTS trace_archive (
    source_path    TEXT PRIMARY KEY,
    provider       TEXT NOT NULL,             -- claude | codex
    kind           TEXT NOT NULL,             -- session | subagent
    conv_id        TEXT NOT NULL DEFAULT '',
    parent_conv    TEXT NOT NULL DEFAULT '',
    worker         TEXT NOT NULL DEFAULT '',
    archive_path   TEXT NOT NULL,
    source_bytes   INTEGER NOT NULL,
    archive_bytes  INTEGER NOT NULL,
    source_mtime   REAL NOT NULL,
    archived_at    REAL NOT NULL,
    first_ts       TEXT NOT NULL DEFAULT '',
    last_ts        TEXT NOT NULL DEFAULT '',
    records        INTEGER NOT NULL DEFAULT 0,
    user_msgs      INTEGER NOT NULL DEFAULT 0,
    tool_calls     INTEGER NOT NULL DEFAULT 0,
    tool_errors    INTEGER NOT NULL DEFAULT 0,
    output_tokens  INTEGER NOT NULL DEFAULT 0,
    source_deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_trace_archive_worker ON trace_archive(worker, last_ts);
