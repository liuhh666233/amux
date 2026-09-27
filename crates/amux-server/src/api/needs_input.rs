//! /api/needs-input: the owner's triage queue (AMUX-5286).
//!
//! Ethan, 2026-09-27, pointing at the orange "17" header pill on his iPhone:
//! "this should open a modal where i can go thru one by one quickly. the
//! entire point of amux is to set this shit up to be set and forget and then
//! very quickly or automatically plow thru blockers".
//!
//! One ordered list of everything waiting on the owner, read from the stores
//! that already hold it (no new primitive):
//!
//! - every open `needsyou` card that is not archived and is not addressed to a
//!   peer lane (`ask_actor` naming someone other than the owner);
//! - every open `decision` card that carries a `decision_question` or is
//!   `waiting_on` the owner;
//! - every pending outbound-email approval (`~/.amux/email-approvals`).
//!
//! Workers blocked on the owner (`owner_block` in the session list) are merged
//! by the dashboard, which already holds that list; this endpoint emits the
//! same `rank`/`since` keys so the merge is a sort, not a second policy.
//!
//! Order: money / customer-outbound / prod-data asks first (`rank` 0), then
//! everything else (`rank` 1), oldest first within each. Snoozes live in the
//! `prefs` table under [`SNOOZE_KEY`] so they follow the owner across devices.
//!
//! The actions themselves (approve, decline, reply, standing approval) go
//! through the existing endpoints from the dashboard: board PATCH, the owner
//! send path, /api/email/approve|reject, /api/approvals/standing. Each one is
//! reported here via POST /api/needs-input/log so a sweep sees
//! `verdict="triage_action"` with the action, the card and the outcome,
//! including refusals.

use super::AppState;
use crate::config::now_f64;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// `prefs` key holding `{item_key: until_epoch_s}`.
pub const SNOOZE_KEY: &str = "needs_input_snooze";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/needs-input", axum::routing::get(get_queue))
        .route("/api/needs-input/snooze", axum::routing::post(snooze))
        .route("/api/needs-input/log", axum::routing::post(log_action))
}

/// What kind of ask this is, and whether it jumps the queue.
///
/// The three categories the owner's standing-authority boundary names (money,
/// anyone outside the company, production data) rank 0. `ask_type` is the
/// declared signal; the text fallback catches the asks filed before AF-318 or
/// typed `decision` while plainly being about spend.
pub fn classify(ask_type: &str, text: &str) -> (&'static str, u8) {
    let t = text.to_ascii_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
    // A REAL SPEND, not a money-adjacent word (Ethan, 2026-09-27 14:42: "the
    // bar for needs input just needs to be much higher"). "Should billing
    // /estimate be reachable without an API key", "what are the per-million
    // rates" and "six dead lanes" were all counted as money on the word
    // "billing", "rates" or a lane's loose ask_type. Money now needs a dollar
    // figure, a monthly price, or a verb that commits spend, even when the
    // lane declared ask_type=budget.
    let dollar = t
        .char_indices()
        .any(|(i, c)| c == '$' && t[i + 1..].starts_with(|d: char| d.is_ascii_digit()));
    let spends = dollar
        || has(&[
            "/mo", "per month", "a month", "spend ", "spending", " pay ", "purchase", " buy ",
            "top up", "top-up", "fund ", "provision", "raise the spend", "spend limit", "gpu spend",
            "on-demand", "credits",
        ]);
    if has(&[
        "prod data",
        "production data",
        "customer data",
        "prod db",
        "production db",
        "migrate prod",
        "delete prod",
    ]) {
        return ("prod_data", 0);
    }
    if ask_type == "customer_outbound" {
        return ("outbound", 0);
    }
    if spends {
        return ("money", 0);
    }
    if has(&[
        "send email",
        "send the draft",
        "welcome email",
        "outbound",
        "publish",
        "post to",
        "linkedin",
        "reply to",
        "app store",
        "tweet",
        "post the",
        "post on",
        "posts for",
        "natively",
        "press release",
    ]) || t.contains('@') && has(&["email", "send", "reply"])
    {
        return ("outbound", 0);
    }
    ("other", 1)
}

/// Does this ask clear the owner bar: a real spend, outbound to outside
/// people, production data, or a credential/access only the owner holds?
/// One definition for the pill, the triage queue's default view and the
/// per-worker "Needs input" status, so the three cannot disagree.
pub fn clears_owner_bar(ask_type: &str, text: &str) -> bool {
    matches!(ask_type, "credential" | "access")
        || matches!(classify(ask_type, text).0, "money" | "outbound" | "prod_data")
}

/// Standing-approval category for an ask (the closed vocabulary in
/// `standing_approvals::CATEGORIES`).
pub fn standing_category(ask_type: &str, category: &str) -> &'static str {
    match (ask_type, category) {
        ("budget", _) | (_, "money") => "budget",
        ("customer_outbound", _) | (_, "outbound") => "customer_outbound",
        (_, "prod_data") => "prod_data",
        ("credential", _) => "credential",
        ("access", _) => "access",
        _ => "decision",
    }
}

/// One-tap reply chips offered when the ask text names them.
pub fn chips(text: &str) -> Vec<Value> {
    let t = text.to_ascii_lowercase();
    let mut out = Vec::new();
    if t.contains("recommend") {
        out.push(json!({"label": "Your recommendation", "text": "Go with your recommendation. Proceed."}));
    }
    if t.contains("default") {
        out.push(json!({"label": "Defaults", "text": "Use the defaults. Proceed."}));
    }
    out
}

/// Is `actor` the owner (or unnamed)? A needsyou card addressed to a peer lane
/// is waiting on that lane, not on the owner.
pub fn actor_is_owner(actor: &str) -> bool {
    let a = actor.trim().to_ascii_lowercase();
    a.is_empty()
        || ["ethan", "owner", "human", "me", "founder"]
            .iter()
            .any(|w| a.contains(w))
}

/// Older than this, an ask sorts behind every fresh one (it stays listed).
pub const STALE_AFTER_S: f64 = 14.0 * 86_400.0;

fn secs(t: f64) -> f64 {
    if t > 1e12 { t / 1000.0 } else { t }
}

/// Sort in place: fresh before stale (older than 14 days), then rank, then
/// NEWEST first, then key. Oldest-first put a 27-day-old ask from a stopped
/// lane at "1 of 99" on 2026-09-27; what can move now comes first. The
/// dashboard adds the live-worker tier on top, from its session list.
pub fn order(items: &mut [Value]) {
    order_at(items, now_f64())
}

/// `order` against a caller-supplied clock, so a queue built for `now` is
/// staled against the same `now`.
pub fn order_at(items: &mut [Value], now: f64) {
    items.sort_by(|a, b| {
        let r = |v: &Value| v["rank"].as_u64().unwrap_or(1);
        let s = |v: &Value| secs(v["since"].as_f64().unwrap_or(0.0));
        let stale = |v: &Value| (s(v) > 0.0 && now - s(v) > STALE_AFTER_S) as u8;
        stale(a).cmp(&stale(b))
            .then_with(|| r(a).cmp(&r(b)))
            .then_with(|| s(b).partial_cmp(&s(a)).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                a["key"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["key"].as_str().unwrap_or(""))
            })
    });
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// Snoozes from the prefs table, expired ones dropped.
pub fn load_snoozes(conn: &Connection, now: f64) -> BTreeMap<String, f64> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM prefs WHERE key = ?1",
            [SNOOZE_KEY],
            |r| r.get(0),
        )
        .ok();
    let mut out = BTreeMap::new();
    if let Some(Value::Object(m)) = raw.and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
        for (k, v) in m {
            if let Some(until) = v.as_f64().filter(|u| *u > now) {
                out.insert(k, until);
            }
        }
    }
    out
}

pub struct Queue {
    pub items: Vec<Value>,
    pub snoozed: Vec<Value>,
    pub n_considered: usize,
    pub excluded_peer_actor: usize,
}

/// Build the ordered, deduped queue from the card store and the approvals dir.
pub fn build(conn: &Connection, home: &Path, now: f64) -> rusqlite::Result<Queue> {
    let snoozes = load_snoozes(conn, now);
    let mut by_key: BTreeMap<String, Value> = BTreeMap::new();
    let mut n_considered = 0usize;
    let mut excluded_peer_actor = 0usize;
    let mut stmt = conn.prepare(
        "SELECT id, title, COALESCE(desc,''), status, COALESCE(session,''), COALESCE(type,''),
                COALESCE(ask_type,''), COALESCE(ask_question,''), COALESCE(ask_unblocks,''),
                COALESCE(ask_actor,''), COALESCE(decision_question,''), COALESCE(decision_rationale,''),
                COALESCE(waiting_on,''), COALESCE(entered_state_at,0), COALESCE(updated,0), COALESCE(created,0)
           FROM issues
          WHERE deleted IS NULL AND COALESCE(archived,0) = 0
            AND (status = 'needsyou'
                 OR (type = 'decision' AND status NOT IN ('done','verified','discarded')
                     AND (COALESCE(decision_question,'') <> ''
                          OR lower(COALESCE(waiting_on,'')) IN ('ethan','owner','human'))))",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, String>(10)?,
            r.get::<_, String>(11)?,
            r.get::<_, String>(12)?,
            (
                r.get::<_, i64>(13)?,
                r.get::<_, i64>(14)?,
                r.get::<_, i64>(15)?,
            ),
        ))
    })?;
    for row in rows {
        let (
            id,
            title,
            desc,
            status,
            session,
            ctype,
            ask_type,
            ask_q,
            ask_unb,
            actor,
            dq,
            drat,
            waiting,
            (entered, updated, created),
        ) = row?;
        n_considered += 1;
        if status == "needsyou" && !actor_is_owner(&actor) {
            excluded_peer_actor += 1;
            continue;
        }
        let question = [ask_q.as_str(), dq.as_str(), title.as_str()]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("")
            .to_string();
        let unblocks = if ask_unb.trim().is_empty() {
            drat.clone()
        } else {
            ask_unb.clone()
        };
        let (category, rank) = classify(&ask_type, &format!("{question} {title}"));
        let since = [entered, updated, created]
            .into_iter()
            .find(|t| *t > 0)
            .unwrap_or(0);
        let key = format!("card:{id}");
        by_key.insert(
            key.clone(),
            json!({
                "key": key, "kind": "card", "card": id, "title": title,
                "worker": session, "status": status, "card_type": ctype,
                "ask_type": ask_type, "ask_actor": actor, "waiting_on": waiting,
                "question": question, "unblocks": unblocks,
                "context": clip(&desc, 600),
                "category": category, "rank": rank, "since": since,
                "chips": chips(&format!("{question} {unblocks} {desc}")),
                "standing_category": standing_category(&ask_type, category),
            }),
        );
    }
    for doc in crate::api::email_approval::list_pending(home) {
        n_considered += 1;
        let id = doc["id"].as_str().unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let p = &doc["preview"];
        let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
        let key = format!("email:{id}");
        by_key.insert(
            key.clone(),
            json!({
                "key": key, "kind": "email", "approval_id": id, "card": "",
                "worker": doc["session"].as_str().unwrap_or(""),
                "status": "pending approval",
                "question": format!("Send this email to {}? \"{}\"", s("to"), s("subject")),
                "unblocks": "The email is sent exactly as drafted. Decline discards it; nothing is sent.",
                "context": clip(&s("body"), 600),
                "email": {"to": s("to"), "cc": s("cc"), "from": s("from"), "subject": s("subject"), "endpoint": doc["endpoint"].clone()},
                "category": "outbound", "rank": 0,
                "since": doc["created"].as_f64().unwrap_or(now),
                "chips": [],
                "standing_category": "customer_outbound",
            }),
        );
    }
    let mut items = Vec::new();
    let mut snoozed = Vec::new();
    for (k, mut v) in by_key {
        if let Some(until) = snoozes.get(&k) {
            v["snoozed_until"] = json!(until);
            snoozed.push(v);
        } else {
            items.push(v);
        }
    }
    order_at(&mut items, now);
    order_at(&mut snoozed, now);
    Ok(Queue {
        items,
        snoozed,
        n_considered,
        excluded_peer_actor,
    })
}

async fn get_queue(State(state): State<AppState>) -> Response {
    let home = crate::integrations::email::default_amux_home();
    let now = now_f64();
    let res = state
        .store
        .read_async(move |conn| Ok(build(conn, &home, now)?))
        .await;
    match res {
        Ok(q) => {
            let body = json!({
                "items": q.items, "count": q.items.len(),
                "snoozed": q.snoozed.iter().map(|v| json!({"key": v["key"], "until": v["snoozed_until"], "card": v["card"]})).collect::<Vec<_>>(),
                "snoozed_count": q.snoozed.len(),
                "excluded_peer_actor": q.excluded_peer_actor,
                "order": "rank (0 = money, outbound, prod data) then oldest first",
            });
            Json(crate::api::measured::measured(body, q.n_considered)).into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(crate::api::measured::unmeasured(
                json!({"items": [], "count": 0, "error": e.to_string()}),
                "the card store could not be read, so an empty queue here means nothing",
            )),
        )
            .into_response(),
    }
}

/// A worker hiding the owner's asks from the owner is the one thing this
/// store must not allow: owner-only, the rule standing approvals use.
pub(crate) fn refuse_worker(headers: &HeaderMap, action: &str) -> Option<Response> {
    let lane = super::alerts::hdr_worker(headers);
    if lane.is_empty() {
        return None;
    }
    tracing::warn!(verdict = "triage_action", action, outcome = "refused_worker", caller = %lane,
        "a worker tried to write the owner's needs-input triage state");
    Some(
        (
            StatusCode::FORBIDDEN,
            Json(
                json!({"ok": false, "error": "only the owner may snooze or log triage actions",
                "code": "needs_input_owner_only"}),
            ),
        )
            .into_response(),
    )
}

fn str_of(b: &Value, k: &str) -> String {
    b.get(k)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// POST /api/needs-input/snooze `{key, until}` or `{key, minutes}`.
/// `until: 0` clears it. Idempotent: the stored value is the answer.
async fn snooze(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    if let Some(r) = refuse_worker(&headers, "snooze") {
        return r;
    }
    let b = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let key = str_of(&b, "key");
    if !(key.starts_with("card:") || key.starts_with("email:") || key.starts_with("worker:"))
        || key.len() > 200
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "key must look like card:<ID>, email:<apr_id> or worker:<name>"})),
        )
            .into_response();
    }
    let now = now_f64();
    let until = match (b.get("until").and_then(Value::as_f64), b.get("minutes").and_then(Value::as_f64)) {
        (Some(u), _) => u,
        (None, Some(m)) if m > 0.0 => now + m * 60.0,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "send until (epoch seconds, 0 clears) or minutes > 0"})),
            )
                .into_response()
        }
    };
    let k2 = key.clone();
    let res = state
        .store
        .write_async(move |conn| {
            let mut map: Map<String, Value> = load_snoozes(conn, now).into_iter().map(|(k, v)| (k, json!(v))).collect();
            if until > now {
                map.insert(k2.clone(), json!(until));
            } else {
                map.remove(&k2);
            }
            conn.execute(
                "INSERT INTO prefs (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![SNOOZE_KEY, Value::Object(map).to_string()],
            )?;
            Ok(crate::db::WriteOutcome {
                applied: true,
                events: vec![crate::db::PendingEvent {
                    entity_type: amux_core::revision::EntityType::Other("pref".into()),
                    entity_id: SNOOZE_KEY.into(),
                    mutation: amux_core::revision::MutationKind::Updated,
                    payload: None,
                }],
            })
        })
        .await;
    match res {
        Ok(_) => {
            let cleared = until <= now;
            tracing::info!(verdict = "triage_action", action = if cleared { "unsnooze" } else { "snooze" },
                card = %key, until, outcome = "ok", "needs-input snooze stored");
            Json(json!({"ok": true, "key": key, "until": if cleared { Value::Null } else { json!(until) }})).into_response()
        }
        Err(e) => {
            tracing::warn!(verdict = "triage_action", action = "snooze", card = %key, outcome = "store_error", error = %e,
                "needs-input snooze could not be stored");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// POST /api/needs-input/log `{action, key, card, worker, outcome, detail}`.
///
/// The actions run through existing endpoints; this is the one line a sweep
/// greps for, including the refusals the dashboard showed the owner.
async fn log_action(headers: HeaderMap, body: Option<Json<Value>>) -> Response {
    if let Some(r) = refuse_worker(&headers, "log") {
        return r;
    }
    let b = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let action = str_of(&b, "action");
    const ACTIONS: [&str; 7] = [
        "approve",
        "decline",
        "reply",
        "approve_always",
        "snooze",
        "open",
        "skip",
    ];
    if !ACTIONS.contains(&action.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "unknown action", "actions": ACTIONS})),
        )
            .into_response();
    }
    let outcome = clip(&str_of(&b, "outcome"), 40);
    let (key, card, worker) = (
        clip(&str_of(&b, "key"), 200),
        clip(&str_of(&b, "card"), 60),
        clip(&str_of(&b, "worker"), 80),
    );
    let detail = clip(&str_of(&b, "detail"), 300);
    if outcome == "ok" || outcome == "already" {
        tracing::info!(verdict = "triage_action", action = %action, card = %card, key = %key, worker = %worker,
            outcome = %outcome, detail = %detail, "needs-input triage action");
    } else {
        tracing::warn!(verdict = "triage_action", action = %action, card = %card, key = %key, worker = %worker,
            outcome = %outcome, detail = %detail, "needs-input triage action did not complete");
    }
    Json(json!({"ok": true, "logged": true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_bar_is_one_rule_for_pill_queue_and_status() {
        // Live 2026-09-27 18:28: these read "Needs input" and should not.
        assert!(!clears_owner_bar("decision", "Want me to go after the Gemini source now, or wait for gs-4 to say whether it's theirs?"));
        assert!(!clears_owner_bar("budget", "May mvs-infra keep 24 hours of MVS restore points (retained snapshot generations)?"));
        // These should.
        assert!(clears_owner_bar("credential", "Can you mint a new Ethan Personal org API key in Studio?"));
        assert!(clears_owner_bar("access", "May amux-ops get roles/artifactregistry.admin on the us-east1 repo?"));
        assert!(clears_owner_bar("budget", "Do you approve a TS indexer node (about $90-130/mo spot)?"));
        assert!(clears_owner_bar("external", "Post the 526-character launch tweet natively from X yourself (file gtm/launch-videos/amux-launch.mp4)?"));
    }

    #[test]
    fn classify_puts_money_outbound_and_prod_first() {
        // A declared budget ask with no spend in it is not money.
        assert_eq!(classify("budget", "anything"), ("other", 1));
        // Live 2026-09-27 specimens: real spend stays money...
        assert_eq!(classify("budget", "Do you approve a TS indexer node (about $90-130/mo spot or $390-425/mo on-demand), or hold?"), ("money", 0));
        assert_eq!(classify("budget", "Do you approve GPU spend to re-extract the two broken TubeScience indexes?"), ("money", 0));
        // ...money-adjacent words do not.
        assert_eq!(classify("decision", "Should POST /v1/organizations/billing/estimate be reachable without an API key?"), ("other", 1));
        assert_eq!(classify("budget", "What are the per-million rates for the codex and gemini model families?"), ("other", 1));
        assert_eq!(classify("budget", "Six registered goal-spec lanes (gs-2, gs-3, gs-5) are dead; archive them?"), ("other", 1));
        // Outbound to a person outside stays outbound.
        assert_eq!(classify("external", "Approve the welcome email to partners@thefasttrackgirl.com?"), ("outbound", 0));
        assert_eq!(classify("customer_outbound", "x"), ("outbound", 0));
        assert_eq!(
            classify("decision", "Approve about $1,000 of API spend?"),
            ("money", 0)
        );
        assert_eq!(
            classify("decision", "OK to migrate prod data to the new shard?"),
            ("prod_data", 0)
        );
        assert_eq!(
            classify("decision", "Which name reads better?"),
            ("other", 1)
        );
        assert_eq!(
            classify("credential", "Sign in once at the vercel app"),
            ("other", 1)
        );
        // A bare $ with no digit is not money.
        assert_eq!(classify("", "set $AMUX_URL first?"), ("other", 1));
    }

    #[test]
    fn order_is_fresh_then_rank_then_newest() {
        let now = now_f64();
        let mut v = vec![
            json!({"key":"card:B","rank":1,"since":now - 100.0}),
            json!({"key":"card:A","rank":1,"since":now - 100.0}),
            json!({"key":"card:OLDER","rank":1,"since":now - 5000.0}),
            json!({"key":"email:apr_1","rank":0,"since":now - 50.0}),
            json!({"key":"card:MONEY","rank":0,"since":now - 400.0}),
            // 27 days old, money: the live 2026-09-27 head of the queue.
            json!({"key":"card:STALE_MONEY","rank":0,"since":now - 27.0 * 86_400.0}),
        ];
        order(&mut v);
        let keys: Vec<_> = v.iter().map(|x| x["key"].as_str().unwrap()).collect();
        assert_eq!(
            keys,
            ["email:apr_1", "card:MONEY", "card:A", "card:B", "card:OLDER", "card:STALE_MONEY"]
        );
    }

    #[test]
    fn chips_only_when_the_ask_names_them() {
        assert!(chips("Which of these?").is_empty());
        let c = chips("reply defaults, or override; I recommend option 2");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["label"], "Your recommendation");
        assert_eq!(c[1]["label"], "Defaults");
    }

    #[test]
    fn peer_actor_is_not_the_owner() {
        assert!(actor_is_owner(""));
        assert!(actor_is_owner("Ethan"));
        assert!(actor_is_owner("ethan (sign-in)"));
        assert!(!actor_is_owner("mvs-infra"));
    }

    #[test]
    fn standing_category_maps_to_the_closed_vocabulary() {
        for (t, c) in [
            ("budget", "money"),
            ("", "outbound"),
            ("", "prod_data"),
            ("credential", "other"),
            ("access", "other"),
            ("judgment", "other"),
        ] {
            assert!(
                crate::api::standing_approvals::CATEGORIES.contains(&standing_category(t, c)),
                "{t}/{c}"
            );
        }
        assert_eq!(standing_category("decision", "money"), "budget");
    }

    type Row<'a> = (
        &'a str,
        &'a str,
        &'a str,
        &'a str,
        &'a str,
        &'a str,
        i64,
        i64,
    );
    fn seed(conn: &Connection, row: Row<'_>) {
        let (id, status, ty, actor, ask_type, q, entered, archived) = row;
        conn.execute(
            "INSERT INTO issues (id,title,desc,status,session,created,updated,type,archived,ask_type,ask_question,ask_unblocks,ask_actor,entered_state_at)
             VALUES (?1,?2,'context for '||?1,?3,'lane-a',?4,?4,?5,?6,?7,?8,'unblocks it',?9,?4)",
            rusqlite::params![id, format!("title {id}"), status, entered, ty, archived, ask_type, q, actor],
        )
        .unwrap();
    }

    #[test]
    fn build_dedupes_filters_orders_and_snoozes() {
        let dir = tempfile::tempdir().unwrap();
        // The real migration chain, so a column build() starts reading exists
        // here the moment its migration does (tests/schema_fixtures.rs).
        let conn = crate::db::migrate::test_memdb();
        seed(
            &conn,
            (
                "T-1",
                "needsyou",
                "code",
                "Ethan",
                "decision",
                "Which option?",
                300,
                0,
            ),
        );
        seed(
            &conn,
            (
                "T-2",
                "needsyou",
                "code",
                "",
                "budget",
                "Spend $40?",
                900,
                0,
            ),
        );
        seed(
            &conn,
            (
                "T-3", "needsyou", "code", "Ethan", "decision", "Old one?", 100, 0,
            ),
        );
        seed(
            &conn,
            (
                "T-4",
                "needsyou",
                "code",
                "Ethan",
                "decision",
                "Archived?",
                50,
                1,
            ),
        );
        seed(
            &conn,
            (
                "T-5",
                "needsyou",
                "code",
                "mvs-infra",
                "access",
                "Peer ask?",
                60,
                0,
            ),
        );
        seed(
            &conn,
            (
                "T-6",
                "todo",
                "code",
                "Ethan",
                "decision",
                "Not waiting?",
                70,
                0,
            ),
        );
        // A decision card that is needsyou AND type decision appears once.
        seed(
            &conn,
            (
                "T-7",
                "needsyou",
                "decision",
                "Ethan",
                "decision",
                "Both matchers?",
                200,
                0,
            ),
        );
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("email-approvals")).unwrap();
        std::fs::write(
            home.join("email-approvals/apr_00000000000000aa.json"),
            json!({"id":"apr_00000000000000aa","created":5000.0,"session":"gtm","endpoint":"send",
                   "preview":{"to":"x@example.com","subject":"hi","body":"hello"}})
            .to_string(),
        )
        .unwrap();
        let q = build(&conn, &home, 10_000.0).unwrap();
        let keys: Vec<_> = q
            .items
            .iter()
            .map(|x| x["key"].as_str().unwrap().to_string())
            .collect();
        // Rank 0 (money/outbound) first, newest first within a rank; all
        // seeds are fresh against this build's clock.
        assert_eq!(
            keys,
            [
                "email:apr_00000000000000aa",
                "card:T-2",
                "card:T-1",
                "card:T-7",
                "card:T-3",
            ]
        );
        assert_eq!(q.excluded_peer_actor, 1);
        assert_eq!(q.items[0]["kind"], "email");
        assert_eq!(q.items[1]["category"], "money");
        conn.execute(
            "INSERT INTO prefs (key,value) VALUES (?1,?2)",
            rusqlite::params![
                SNOOZE_KEY,
                json!({"card:T-3": 20_000.0, "card:T-1": 1.0}).to_string()
            ],
        )
        .unwrap();
        let q = build(&conn, &home, 10_000.0).unwrap();
        let keys: Vec<_> = q
            .items
            .iter()
            .map(|x| x["key"].as_str().unwrap().to_string())
            .collect();
        // T-3 is snoozed into the future; T-1's snooze expired and it is back.
        assert_eq!(
            keys,
            ["email:apr_00000000000000aa", "card:T-2", "card:T-1", "card:T-7"]
        );
        assert_eq!(q.snoozed.len(), 1);
    }
}
