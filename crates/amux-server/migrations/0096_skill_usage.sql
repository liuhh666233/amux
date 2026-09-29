-- One row per skill invocation seen in a Claude Code transcript (AMUX-5340):
-- the model's Skill tool calls (source 'model') and user-typed slash commands
-- (source 'user'). Keyed by the transcript record's uuid, so the incremental
-- indexer and the one-time backfill can both insert without double counting.
-- Feeds "most used" on the Skills page.
CREATE TABLE IF NOT EXISTS skill_usage (
    record_uuid TEXT NOT NULL,
    skill       TEXT NOT NULL,
    source      TEXT NOT NULL,
    session     TEXT NOT NULL DEFAULT '',
    ts          INTEGER NOT NULL,
    PRIMARY KEY (record_uuid, skill)
);
CREATE INDEX IF NOT EXISTS idx_skill_usage_skill ON skill_usage(skill, ts);
