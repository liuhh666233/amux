//! Drain state: what stands between a lane and an empty board (RR-0052,
//! Invariant 5: "a durable orchestrator, not an LLM conversation, owns board
//! drainage").
//!
//! The board driver already dispatches, reclaims and promotes. What it could not
//! do was ANSWER the drain question: is this lane done, and if not, what exactly
//! is in the way? `LaneTrace` records what the last tick did, which is a
//! different fact. This computes the answer from the board alone, with the same
//! dependency predicate dispatch uses (`deps_blocking` ->
//! `bs::dependency_resolved`), so the view cannot disagree with the mechanism it
//! describes.

use crate::db::board_store as bs;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Blocker {
    pub id: String,
    /// Status of the blocking card, or "missing" when it no longer exists.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BlockedCard {
    pub id: String,
    pub status: String,
    pub blocked_by: Vec<Blocker>,
    /// Free-text external watch (`blocked_on`), when the block is not a card.
    pub blocked_on: Option<String>,
    /// Some blocker is waiting on a human (a `needsyou` card). No worker can
    /// clear this one.
    pub needs_human: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunningCard {
    pub id: String,
    pub holder: Option<String>,
    pub attempt: Option<i64>,
    pub heartbeat_age_s: Option<i64>,
    pub lease_expires_in_s: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DrainState {
    pub lane: String,
    /// False when the board could not be read. Every count below is then 0 and
    /// means nothing (ethos rule 4).
    pub measured: bool,
    /// Open agent-owned cards considered: todo, doing, backlog, blocked,
    /// review, needsyou. done/verified/discarded/quarantined are finished work
    /// for this question; verification has its own loop.
    pub n_considered: usize,
    pub ready: usize,
    pub running: Vec<RunningCard>,
    pub blocked: Vec<BlockedCard>,
    pub review: usize,
    pub needs_human: usize,
    /// Backlog with nothing blocking it. Not dispatched unless the lane opts in
    /// (AMUX_DISPATCH_BACKLOG_WHEN_IDLE), so it is reported apart from `ready`.
    pub parked: usize,
    /// `blocked` cards with no unresolved dependency and no external watch: the
    /// block is gone and nothing has moved them. The driver unblocks these.
    pub unblockable: Vec<String>,
    pub verdict: &'static str,
    /// Is the lane getting closer to empty, and how fast? The counts above
    /// are a snapshot; this is the rate.
    pub trend: Trend,
}

/// Intake against closes for one lane. mixpeek-general, 2026-09-29: a lane
/// closed 1,544 cards in 7 days and still grew, because 1,696 arrived, and its
/// closes fell from ~220 a day to 44 when it hit its weekly model limit.
/// Nothing reported either; both were found by board arithmetic. Closed means
/// done, verified or discarded (all take the card off the lane).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Trend {
    pub measured: bool,
    pub opened_24h: i64,
    pub closed_24h: i64,
    pub opened_7d: i64,
    pub closed_7d: i64,
    /// opened_7d - closed_7d: positive means the lane is falling behind.
    pub net_7d: i64,
    /// Open cards / net daily close rate, when the lane is gaining; null when
    /// it is not, because "never" is not a number.
    pub days_to_empty: Option<f64>,
    /// The last 24h closed under half the 7-day daily average, on a lane that
    /// averages at least 10 closes a day: a rate limit, a stall, a stuck tool.
    pub slowdown: bool,
    /// `falling_behind`, `gaining`, `idle` (nothing opened or closed in 7d),
    /// `empty`, or `unmeasured`.
    pub verdict: &'static str,
}

impl Trend {
    pub fn unmeasured() -> Self {
        Trend { measured: false, opened_24h: 0, closed_24h: 0, opened_7d: 0, closed_7d: 0, net_7d: 0,
            days_to_empty: None, slowdown: false, verdict: "unmeasured" }
    }
}

/// Pure: the trend from its four counts and the lane's open count.
pub fn trend_from(open: i64, opened_24h: i64, closed_24h: i64, opened_7d: i64, closed_7d: i64) -> Trend {
    let net_7d = opened_7d - closed_7d;
    let net_daily_close = (closed_7d - opened_7d) as f64 / 7.0;
    let days_to_empty = (open > 0 && net_daily_close > 0.0).then(|| ((open as f64 / net_daily_close) * 10.0).round() / 10.0);
    let avg = closed_7d as f64 / 7.0;
    let slowdown = avg >= 10.0 && (closed_24h as f64) < avg * 0.5;
    let verdict = if open == 0 {
        "empty"
    } else if opened_7d == 0 && closed_7d == 0 {
        "idle"
    } else if net_7d > 0 {
        "falling_behind"
    } else {
        "gaining"
    };
    Trend { measured: true, opened_24h, closed_24h, opened_7d, closed_7d, net_7d, days_to_empty, slowdown, verdict }
}

const CLOSED: &str = "status IN ('done','verified','discarded')";

/// Say which lanes are losing ground, at most once per lane per 6 hours, so
/// a lane growing faster than it closes, or suddenly closing far less (a
/// weekly model limit), shows in the log a sweep reads instead of only in
/// board arithmetic. Every WARN carries the counts it judged on.
pub fn warn_losing_lanes(conn: &Connection, now: i64) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let Ok(trends) = lane_trends(conn, now) else {
        tracing::warn!(verdict = "lane_trend_unmeasured", measured = false, "board drain: lane trends could not be read");
        return;
    };
    let mut last = LAST.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    for (lane, t) in trends {
        let behind = t.verdict == "falling_behind" && t.net_7d >= 20;
        if !(behind || t.slowdown) || now - last.get(&lane).copied().unwrap_or(0) < 6 * 3600 {
            continue;
        }
        last.insert(lane.clone(), now);
        tracing::warn!(
            target: "amux::board_drive", lane = %lane,
            verdict = if t.slowdown { "lane_close_rate_dropped" } else { "lane_falling_behind" },
            opened_7d = t.opened_7d, closed_7d = t.closed_7d, net_7d = t.net_7d,
            closed_24h = t.closed_24h, avg_closed_per_day = t.closed_7d / 7,
            "board drain: {lane} is losing ground ({} opened vs {} closed in 7d; {} closed in the last 24h)",
            t.opened_7d, t.closed_7d, t.closed_24h
        );
    }
}

/// Every lane's trend from one grouped read: (lane, trend).
pub fn lane_trends(conn: &Connection, now: i64) -> rusqlite::Result<Vec<(String, Trend)>> {
    trends_where(conn, now, None)
}

/// One lane's trend (unmeasured when the board cannot be read).
pub fn lane_trend(conn: &Connection, lane: &str, now: i64) -> Trend {
    match trends_where(conn, now, Some(lane)) {
        Ok(mut v) => v.pop().map(|(_, t)| t).unwrap_or_else(|| trend_from(0, 0, 0, 0, 0)),
        Err(_) => Trend::unmeasured(),
    }
}

fn trends_where(conn: &Connection, now: i64, lane: Option<&str>) -> rusqlite::Result<Vec<(String, Trend)>> {
    let (d1, d7) = (now - 86_400, now - 7 * 86_400);
    let sql = format!(
        "SELECT session, \
           SUM(status IN ('todo','doing','backlog','blocked','review','needsyou') AND COALESCE(archived,0)=0), \
           SUM(created >= ?1), SUM({CLOSED} AND closed_at >= ?1), \
           SUM(created >= ?2), SUM({CLOSED} AND closed_at >= ?2) \
         FROM issues WHERE deleted IS NULL AND session IS NOT NULL AND session <> '' \
           AND COALESCE(owner_type,'agent') = 'agent' AND (?3 IS NULL OR session = ?3) GROUP BY session"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![d1, d7, lane], |r| {
        let g = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0));
        Ok((r.get::<_, String>(0)?, trend_from(g(1)?, g(2)?, g(3)?, g(4)?, g(5)?)))
    })?;
    rows.collect()
}

/// The verdict, from counts alone. Pure so every arm has a test.
///
/// - `draining`: something is running or ready; the driver has work to hand out.
/// - `waiting_on_dependency`: nothing runnable, but a blocker is another card a
///   worker can finish. The swarm clears this with no human.
/// - `unblockable`: a `blocked` card whose block is gone. A bug signal if it
///   persists past one driver tick.
/// - `waiting_on_human`: only human-owned asks stand in the way.
/// - `waiting_on_external`: blocked on a free-text watch, not a card.
/// - `waiting_on_review`: only review remains.
/// - `backlog_only`: only undispatched backlog remains.
/// - `drained`: nothing open.
pub fn verdict(
    ready: usize,
    running: usize,
    blocked: &[BlockedCard],
    unblockable: usize,
    review: usize,
    needs_human: usize,
    parked: usize,
) -> &'static str {
    if running > 0 || ready > 0 {
        return "draining";
    }
    if blocked
        .iter()
        .any(|b| !b.needs_human && !b.blocked_by.is_empty())
    {
        return "waiting_on_dependency";
    }
    if unblockable > 0 {
        return "unblockable";
    }
    if needs_human > 0 || blocked.iter().any(|b| b.needs_human) {
        return "waiting_on_human";
    }
    if !blocked.is_empty() {
        return "waiting_on_external";
    }
    if review > 0 {
        return "waiting_on_review";
    }
    if parked > 0 {
        return "backlog_only";
    }
    "drained"
}

const OPEN: [&str; 6] = ["todo", "doing", "backlog", "blocked", "review", "needsyou"];

/// The answer when the board could not be read at all.
pub fn drain_state_unmeasured(lane: &str) -> DrainState {
    DrainState {
        lane: lane.to_string(),
        measured: false,
        n_considered: 0,
        ready: 0,
        running: vec![],
        blocked: vec![],
        review: 0,
        needs_human: 0,
        parked: 0,
        unblockable: vec![],
        verdict: "unmeasured",
        trend: Trend::unmeasured(),
    }
}

pub fn drain_state(conn: &Connection, lane: &str, now: i64) -> DrainState {
    let mut st = drain_state_unmeasured(lane);
    let statuses: Vec<String> = OPEN.iter().map(|s| s.to_string()).collect();
    let rows = match bs::list_issues(
        conn,
        &statuses,
        &[lane.to_string()],
        bs::ArchivedFilter::ActiveOnly,
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                target: "amux::board_drive", lane, %error, measured = false, n_considered = 0,
                verdict = "drain_state_unmeasured", "board drain: could not read the lane's open cards"
            );
            return st;
        }
    };
    let attempts = crate::db::attempts::running_attempt_numbers(conn).unwrap_or_default();
    let status_of = |id: &str| -> String {
        conn.query_row(
            "SELECT status FROM issues WHERE id=?1 AND deleted IS NULL",
            [id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "missing".to_string())
    };
    for row in rows.iter().filter(|r| r.owner_type == "agent") {
        st.n_considered += 1;
        let unresolved = crate::runtime_jobs::board_drive::deps_blocking(conn, row);
        let blockers: Vec<Blocker> = unresolved
            .iter()
            .map(|id| Blocker {
                id: id.clone(),
                status: status_of(id),
            })
            .collect();
        let blocked_on = row.blocked_on.clone().filter(|b| !b.trim().is_empty());
        let as_blocked = |blockers: Vec<Blocker>| BlockedCard {
            id: row.id.clone(),
            status: row.status.clone(),
            needs_human: blockers.iter().any(|b| b.status == "needsyou"),
            blocked_by: blockers,
            blocked_on: blocked_on.clone(),
        };
        match row.status.as_str() {
            "doing" => st.running.push(RunningCard {
                id: row.id.clone(),
                holder: row.lease_owner.clone().or_else(|| row.session.clone()),
                attempt: attempts.get(&row.id).copied(),
                heartbeat_age_s: row.lease_heartbeat_at.map(|h| now - h),
                lease_expires_in_s: row.lease_expires_at.map(|e| e - now),
            }),
            "todo" if blockers.is_empty() && blocked_on.is_none() => st.ready += 1,
            "backlog" if blockers.is_empty() && blocked_on.is_none() => st.parked += 1,
            "todo" | "backlog" => st.blocked.push(as_blocked(blockers)),
            "blocked" => {
                if blockers.is_empty() && blocked_on.is_none() {
                    st.unblockable.push(row.id.clone());
                } else {
                    st.blocked.push(as_blocked(blockers));
                }
            }
            "review" => st.review += 1,
            "needsyou" => st.needs_human += 1,
            _ => {}
        }
    }
    st.measured = true;
    st.trend = lane_trend(conn, lane, now);
    st.verdict = verdict(
        st.ready,
        st.running.len(),
        &st.blocked,
        st.unblockable.len(),
        st.review,
        st.needs_human,
        st.parked,
    );
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(needs_human: bool, by_card: bool) -> BlockedCard {
        BlockedCard {
            id: "B".into(),
            status: "blocked".into(),
            blocked_by: if by_card {
                vec![Blocker {
                    id: "D".into(),
                    status: "doing".into(),
                }]
            } else {
                vec![]
            },
            blocked_on: (!by_card).then(|| "vendor ships the fix".to_string()),
            needs_human,
        }
    }

    #[test]
    fn every_verdict_arm_is_reachable_and_ordered() {
        assert_eq!(verdict(1, 0, &[], 0, 0, 0, 0), "draining");
        assert_eq!(
            verdict(0, 1, &[blocked(true, true)], 0, 3, 3, 3),
            "draining",
            "anything running outranks what is waiting"
        );
        assert_eq!(
            verdict(0, 0, &[blocked(false, true)], 1, 0, 1, 0),
            "waiting_on_dependency",
            "a card a worker can finish outranks a human ask"
        );
        assert_eq!(verdict(0, 0, &[], 1, 0, 0, 0), "unblockable");
        assert_eq!(
            verdict(0, 0, &[blocked(true, true)], 0, 0, 0, 0),
            "waiting_on_human"
        );
        assert_eq!(verdict(0, 0, &[], 0, 0, 2, 0), "waiting_on_human");
        assert_eq!(
            verdict(0, 0, &[blocked(false, false)], 0, 0, 0, 0),
            "waiting_on_external"
        );
        assert_eq!(verdict(0, 0, &[], 0, 2, 0, 0), "waiting_on_review");
        assert_eq!(verdict(0, 0, &[], 0, 0, 0, 4), "backlog_only");
        assert_eq!(verdict(0, 0, &[], 0, 0, 0, 0), "drained");
    }

    #[test]
    fn drain_state_reads_the_same_dependency_rule_dispatch_uses() {
        let conn = crate::db::migrate::test_memdb();
        let ins = |id: &str, status: &str, kind: &str, deps: &str| {
            conn.execute(
                "INSERT INTO issues (id,title,desc,status,session,created,updated,owner_type,type,depends_on) \
                 VALUES (?1,?1,'',?2,'lane',1,1,'agent',?3,?4)",
                rusqlite::params![id, status, kind, deps],
            )
            .unwrap();
        };
        // A code dependency that is only `done` still blocks: code completes at verified.
        ins("DEP", "done", "code", "[]");
        ins("WAITS", "todo", "code", "[\"DEP\"]");
        ins("FREE", "todo", "chore", "[]");
        ins("STUCK", "blocked", "code", "[]");
        ins("ASK", "needsyou", "decision", "[]");
        let st = drain_state(&conn, "lane", 100);
        assert!(st.measured);
        assert_eq!(
            st.n_considered, 4,
            "done is not open work for this question"
        );
        assert_eq!(st.ready, 1);
        assert_eq!(st.blocked.len(), 1);
        assert_eq!(st.blocked[0].id, "WAITS");
        assert_eq!(
            st.blocked[0].blocked_by,
            vec![Blocker {
                id: "DEP".into(),
                status: "done".into()
            }]
        );
        assert_eq!(st.unblockable, vec!["STUCK".to_string()]);
        assert_eq!(st.needs_human, 1);
        assert_eq!(st.verdict, "draining");

        conn.execute("UPDATE issues SET status='discarded' WHERE id='FREE'", [])
            .unwrap();
        assert_eq!(
            drain_state(&conn, "lane", 100).verdict,
            "waiting_on_dependency"
        );
        conn.execute("UPDATE issues SET status='verified' WHERE id='DEP'", [])
            .unwrap();
        let st = drain_state(&conn, "lane", 100);
        assert_eq!(st.ready, 1, "a verified dependency frees its successor");
        assert_eq!(st.verdict, "draining");
    }
}

#[cfg(test)]
mod trend_tests {
    use super::*;

    #[test]
    fn the_lane_that_closed_1544_and_still_grew_is_falling_behind_and_slowing() {
        // Live, mixpeek-frustrations 2026-09-29: 1,010 open, 1,696 opened and
        // 1,544 closed in 7d, 44 closed in the last 24h.
        let t = trend_from(1010, 50, 44, 1696, 1544);
        assert_eq!((t.verdict, t.net_7d, t.slowdown), ("falling_behind", 152, true));
        assert_eq!(t.days_to_empty, None, "a lane losing ground has no ETA");
        // Gaining: 70 opened, 210 closed in 7d, 60 open -> 3 days to empty.
        let t = trend_from(60, 10, 30, 70, 210);
        assert_eq!((t.verdict, t.days_to_empty, t.slowdown), ("gaining", Some(3.0), false));
        // Small lanes do not flap: under 10 closes a day is never a "slowdown".
        assert!(!trend_from(5, 0, 0, 3, 20).slowdown);
        assert_eq!(trend_from(4, 0, 0, 0, 0).verdict, "idle");
        assert_eq!(trend_from(0, 0, 0, 5, 5).verdict, "empty");
    }

    #[test]
    fn the_grouped_read_counts_opens_and_closes_by_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::db::Store::open(&dir.path().join("trend.db")).unwrap();
        let now = 1_790_000_000i64;
        store
            .write(move |conn| {
                let ins = |id: &str, status: &str, created: i64, closed: Option<i64>| {
                    conn.execute(
                        "INSERT INTO issues (id,title,desc,status,session,created,updated,type,archived,owner_type,closed_at) \
                         VALUES (?1,'t','',?2,'lane-a',?3,?3,'code',0,'agent',?4)",
                        rusqlite::params![id, status, created, closed],
                    )
                };
                ins("A-1", "todo", now - 3600, None)?;          // opened 24h
                ins("A-2", "done", now - 3 * 86_400, Some(now - 1800))?; // closed 24h, opened 7d
                ins("A-3", "discarded", now - 10 * 86_400, Some(now - 2 * 86_400))?; // closed 7d
                ins("A-4", "backlog", now - 20 * 86_400, None)?; // old open
                Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
            })
            .unwrap();
        let conn = store.read().unwrap();
        let t = lane_trend(&conn, "lane-a", now);
        assert_eq!((t.opened_24h, t.closed_24h, t.opened_7d, t.closed_7d), (1, 1, 2, 2), "{t:?}");
        assert_eq!(t.verdict, "gaining");
        assert_eq!(lane_trends(&conn, now).unwrap().len(), 1);
    }
}
