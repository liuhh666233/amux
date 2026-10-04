-- 0102: the steering queue id on a queued schedule run.
--
-- ADDITIVE ONLY (shared live DB).
--
-- WHY: a tmux schedule that fires while its lane is mid-turn is parked on the
-- steering queue and its run row says `queued`. When the queue later delivers
-- it, nothing went back to the run row, because the row never recorded which
-- queue entry it was. On 2026-10-04 SCHED-544's 03:32Z and 04:20Z ticks reached
-- mixpeek-override at 03:36Z and 04:24Z and both rows still read `queued` an
-- hour later, which reads exactly like a tick that never arrived.
--
-- NULL for every row written before this migration and for every non-queued
-- outcome.

-- ADDCOL: schedule_runs queue_id TEXT
CREATE INDEX IF NOT EXISTS idx_schedule_runs_queue_id ON schedule_runs(queue_id);
