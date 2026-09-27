use super::*;
use std::sync::{Arc, Mutex};

fn item(kind: &str, category: &str, ask_type: &str, q: &str) -> Value {
    json!({"key": format!("card:{}", hash(q)), "kind": kind, "card": "T-1", "worker": "lane-a",
           "category": category, "ask_type": ask_type, "question": q, "title": "t", "unblocks": ""})
}

// ---- the money cap parser: live specimens (2026-09-27) --------------------

#[test]
fn money_cap_parser_reads_live_specimens() {
    let over = "Do you approve a TS indexer node (about $90-130/mo spot or $390-425/mo on-demand), or hold?";
    assert_eq!(max_dollar_figure(over), Some(425.0));
    assert_eq!(max_dollar_figure("Approve about $16 of Gemini embedding to re-index?"), Some(16.0));
    assert_eq!(max_dollar_figure("OK to spend $40 of GPU on the backfill?"), Some(40.0));
    assert_eq!(max_dollar_figure("Do you approve GPU spend to re-extract the two broken indexes?"), None);
    assert_eq!(max_dollar_figure("Approve about $1,000 of API spend?"), Some(1000.0));
    assert_eq!(max_dollar_figure("roughly $1.5k a month"), Some(1500.0));
    assert_eq!(max_dollar_figure("about 75 USD"), Some(75.0));
    assert_eq!(max_dollar_figure("$90 to $130"), Some(130.0));
    // A bare $ with no digit, and numbers that are not money.
    assert_eq!(max_dollar_figure("set $AMUX_URL first; 3 lanes, 400 cards"), None);

    let p = Policy::default();
    assert_eq!(decide(&p, &item("card", "money", "budget", over)), Decision::SkipCap(Some(425.0)));
    assert_eq!(decide(&p, &item("card", "money", "budget", "Approve about $16 of Gemini embedding?")), Decision::Approve);
    assert_eq!(decide(&p, &item("card", "money", "budget", "$40 of GPU for the backfill?")), Decision::Approve);
    assert_eq!(
        decide(&p, &item("card", "money", "budget", "Do you approve GPU spend on-demand?")),
        Decision::SkipCap(None),
        "no figure = not auto-approved"
    );
    // Exactly at the cap is inside it.
    assert_eq!(decide(&p, &item("card", "money", "budget", "Spend $50?")), Decision::Approve);
}

// ---- policy matching per category -----------------------------------------

#[test]
fn defaults_approve_judgment_and_small_spend_only() {
    let p = Policy::default();
    assert!(p.enabled && p.other && p.money && !p.prod_data && !p.outbound);
    assert_eq!(p.money_cap_usd, 50.0);
    assert!(p.sources.values().all(|s| s == "default"));
    assert_eq!(decide(&p, &item("card", "other", "decision", "Which option?")), Decision::Approve);
    assert_eq!(
        decide(&p, &item("card", "prod_data", "decision", "Migrate prod data?")),
        Decision::SkipCategory("prod_data".into())
    );
    assert_eq!(
        decide(&p, &item("card", "outbound", "customer_outbound", "Send the welcome email?")),
        Decision::SkipCategory("outbound".into())
    );
    // An email approval is outbound whatever its category field says.
    assert_eq!(
        decide(&p, &item("email", "other", "", "Send this email to x@y.com?")),
        Decision::SkipCategory("outbound".into())
    );
    assert_eq!(
        p.summary("all workers"),
        "Auto-approve is ON for all workers: judgment asks and spend up to $50."
    );

    let all = Policy { prod_data: true, outbound: true, ..Policy::default() };
    assert_eq!(decide(&all, &item("card", "prod_data", "decision", "Migrate prod data?")), Decision::Approve);
    assert_eq!(decide(&all, &item("email", "outbound", "", "Send this email?")), Decision::Approve);
    let off = Policy { enabled: false, ..all.clone() };
    assert_eq!(decide(&off, &item("card", "other", "decision", "Which option?")), Decision::Off);
    let no_other = Policy { other: false, ..Policy::default() };
    assert_eq!(
        decide(&no_other, &item("card", "other", "decision", "Which option?")),
        Decision::SkipCategory("other".into())
    );
}

#[test]
fn credential_and_access_are_never_approved() {
    let all = Policy { prod_data: true, outbound: true, money_cap_usd: 1e9, ..Policy::default() };
    assert!(matches!(decide(&all, &item("card", "other", "credential", "Mint a key")), Decision::Never(_)));
    assert!(matches!(decide(&all, &item("card", "other", "access", "Add me to the GCP project")), Decision::Never(_)));
    // Filed without the ask type, recognised by the text.
    assert!(matches!(decide(&all, &item("card", "other", "decision", "Sign in once at the vercel app?")), Decision::Never(_)));
    assert!(matches!(decide(&all, &item("card", "other", "", "Paste the Stripe API key into server.env")), Decision::Never(_)));
}

// ---- scoped resolution -----------------------------------------------------

#[test]
fn policy_resolves_worker_over_group_over_global_with_sources() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(home.join("env")).unwrap();
    std::fs::write(home.join("amux.env"), "AMUX_NEEDS_INPUT_AUTO_OUTBOUND=1\nAMUX_NEEDS_INPUT_AUTO_MONEY_CAP=200\n").unwrap();
    std::fs::write(home.join("env/gtm.env"), "AMUX_NEEDS_INPUT_AUTO_MONEY_CAP=10\n").unwrap();
    std::fs::write(home.join("sessions/lane-a.env"), "CC_TAGS=gtm\nAMUX_NEEDS_INPUT_AUTO=0\n").unwrap();
    std::fs::write(home.join("sessions/lane-b.env"), "CC_TAGS=ops\n").unwrap();

    let a = resolve(home, "lane-a");
    assert!(!a.enabled);
    assert_eq!(a.sources["enabled"], "worker");
    assert_eq!(a.money_cap_usd, 10.0);
    assert_eq!(a.sources["money_cap_usd"], "group:gtm");
    assert!(a.outbound);
    assert_eq!(a.sources["outbound"], "global");
    assert_eq!(a.sources["other"], "default");
    assert!(a.summary("lane-a").starts_with("Auto-approve is OFF for lane-a"));

    let b = resolve(home, "lane-b");
    assert!(b.enabled);
    assert_eq!(b.money_cap_usd, 200.0);
    assert_eq!(b.sources["money_cap_usd"], "global");

    // The server.env kill switch wins over every layer.
    std::fs::write(home.join("server.env"), "AMUX_NEEDS_INPUT_AUTO=0\n").unwrap();
    let b = resolve(home, "lane-b");
    assert!(!b.enabled);
    assert_eq!(b.sources["enabled"], "server.env");
}

// ---- through the job, against a hermetic store ----------------------------

#[derive(Default)]
struct Mock {
    sends: Mutex<Vec<(String, String)>>,
    emails: Mutex<Vec<String>>,
    fyis: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Actions for Arc<Mock> {
    async fn send(&self, _: &AppState, worker: &str, text: &str, _: &str) -> Result<String, String> {
        if worker == "lane-refuse" {
            return Err("target is paused".into());
        }
        self.sends.lock().unwrap().push((worker.into(), text.into()));
        Ok(format!("message queued for {worker}"))
    }
    async fn card(&self, state: &AppState, card: &str, note: &str, marker: &str) -> Result<String, String> {
        // The REAL board half, against the hermetic store.
        card_via_board(state, card, note, marker).await
    }
    async fn email(&self, _: &AppState, id: &str) -> Result<String, String> {
        self.emails.lock().unwrap().push(id.into());
        Ok("sent".into())
    }
    async fn fyi(&self, _: &AppState, text: &str) {
        self.fyis.lock().unwrap().push(text.into());
    }
}

fn state() -> AppState {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::db::Store::open(&dir.path().join("ni-auto.db")).unwrap();
    std::mem::forget(dir);
    AppState {
        store: Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    }
}

fn seed(state: &AppState, id: &str, worker: &str, ask_type: &str, q: &str, now: f64) {
    let (id, worker, ask_type, q) = (id.to_string(), worker.to_string(), ask_type.to_string(), q.to_string());
    let t = now as i64;
    state
        .store
        .write(move |conn| {
            conn.execute(
                "INSERT INTO issues (id,title,desc,status,session,created,updated,type,archived,ask_type,ask_question,ask_unblocks,ask_actor,entered_state_at)
                 VALUES (?1,?2,'context',  'needsyou',?3,?4,?4,'code',0,?5,?6,'unblocks it','Ethan',?4)",
                rusqlite::params![id, format!("title {id}"), worker, t, ask_type, q],
            )?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        })
        .unwrap();
}

fn card(state: &AppState, id: &str) -> crate::db::board_store::IssueRow {
    crate::db::board_store::get_issue(&state.store.read().unwrap(), id).unwrap().unwrap()
}

fn outcomes(state: &AppState) -> BTreeMap<String, String> {
    load_ledger(&state.store.read().unwrap())
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.card, e.outcome))
        .collect()
}

#[tokio::test]
async fn job_approves_new_items_once_and_leaves_the_rest() {
    let st = state();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(home.join("email-approvals")).unwrap();
    let now = now_f64();
    let mock = Arc::new(Mock::default());

    // Already waiting when the policy first runs: the baseline.
    seed(&st, "OLD-1", "lane-a", "decision", "Which schema option?", now - 500.0);
    let rep = tick_with(&mock, &st, &home, now).await;
    assert_eq!(rep.baseline, 1);
    assert!(rep.approved.is_empty());
    assert!(mock.sends.lock().unwrap().is_empty(), "the first run approves nothing");
    assert_eq!(card(&st, "OLD-1").status, "needsyou");

    // New arrivals.
    seed(&st, "NEW-OTHER", "lane-a", "decision", "Option A or B? I recommend A.", now);
    seed(&st, "NEW-16", "lane-a", "budget", "Approve about $16 of Gemini embedding?", now);
    seed(&st, "NEW-425", "lane-a", "budget", "Approve a node at about $90-130/mo spot or $390-425/mo on-demand?", now);
    seed(&st, "NEW-NOFIG", "lane-a", "budget", "Approve GPU spend for the re-extract?", now);
    seed(&st, "NEW-CRED", "lane-a", "credential", "Mint the Stripe key", now);
    seed(&st, "NEW-PROD", "lane-a", "decision", "OK to migrate prod data to the new shard?", now);
    seed(&st, "NEW-SNOOZE", "lane-a", "decision", "Rename the flag?", now);
    seed(&st, "NEW-OFFLANE", "lane-off", "decision", "Proceed with the refactor?", now);
    seed(&st, "NEW-REFUSE", "lane-refuse", "decision", "Proceed with the rollout?", now);
    std::fs::write(home.join("sessions/lane-off.env"), "AMUX_NEEDS_INPUT_AUTO=0\n").unwrap();
    std::fs::write(
        home.join("email-approvals/apr_00000000000000bb.json"),
        json!({"id":"apr_00000000000000bb","created":now,"session":"gtm","endpoint":"send",
               "preview":{"to":"x@example.com","subject":"hi","body":"hello"}})
        .to_string(),
    )
    .unwrap();
    st.store
        .write(move |conn| {
            conn.execute(
                "INSERT INTO prefs (key,value) VALUES (?1,?2)",
                rusqlite::params![needs_input::SNOOZE_KEY, json!({"card:NEW-SNOOZE": now + 3600.0}).to_string()],
            )?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        })
        .unwrap();

    let rep = tick_with(&mock, &st, &home, now + 60.0).await;
    let mut approved = rep.approved.clone();
    approved.sort();
    assert_eq!(approved, ["NEW-16", "NEW-OTHER"]);
    assert_eq!(rep.refused, ["NEW-REFUSE"]);
    let sends = mock.sends.lock().unwrap().clone();
    assert_eq!(sends.len(), 2, "{sends:?}");
    assert!(sends.iter().all(|(w, t)| w == "lane-a" && t.starts_with("Approved (NEW-") && t.contains(". Proceed.")));
    assert!(mock.emails.lock().unwrap().is_empty(), "outbound is off by default");
    assert_eq!(mock.fyis.lock().unwrap().len(), 1, "one FYI per batch");

    // The approve moved the card out of needsyou and noted it, through the
    // real board PATCH.
    for id in ["NEW-OTHER", "NEW-16"] {
        let c = card(&st, id);
        assert_eq!(c.status, "todo", "{id}");
        assert!(c.desc.contains("Approved automatically by owner policy"), "{id}: {}", c.desc);
    }
    // Everything else is untouched and still waiting on the owner.
    for id in ["OLD-1", "NEW-425", "NEW-NOFIG", "NEW-CRED", "NEW-PROD", "NEW-SNOOZE", "NEW-OFFLANE", "NEW-REFUSE"] {
        assert_eq!(card(&st, id).status, "needsyou", "{id}");
    }
    let o = outcomes(&st);
    assert_eq!(o["OLD-1"], "baseline");
    assert_eq!(o["NEW-425"], "skipped_cap");
    assert_eq!(o["NEW-NOFIG"], "skipped_cap");
    assert_eq!(o["NEW-CRED"], "never");
    assert_eq!(o["NEW-PROD"], "skipped_category");
    assert_eq!(o["NEW-SNOOZE"], "snoozed");
    assert_eq!(o["NEW-OFFLANE"], "off");
    assert_eq!(o["NEW-REFUSE"], "refused");
    assert_eq!(o["apr_00000000000000bb"], "skipped_category");

    // Dedupe: the next tick does nothing, including no retry of the refusal.
    let rep = tick_with(&mock, &st, &home, now + 120.0).await;
    assert!(rep.approved.is_empty() && rep.refused.is_empty(), "{rep:?}");
    assert_eq!(mock.sends.lock().unwrap().len(), 2);
    assert_eq!(mock.fyis.lock().unwrap().len(), 1);

    // The GET view lists approved and refused only, newest first.
    let led = load_ledger(&st.store.read().unwrap()).unwrap();
    let v = view(&home, "", &led, &[], now + 120.0);
    assert_eq!(v["recent_count"], 3);
    assert_eq!(v["summary"], "Auto-approve is ON for all workers: judgment asks and spend up to $50.");

    // The owner's explicit sweep, with outbound switched on globally: the
    // baseline item and the held email are re-evaluated; the credential ask,
    // the snoozed item and the refusal are still left alone.
    std::fs::write(home.join("amux.env"), "AMUX_NEEDS_INPUT_AUTO_OUTBOUND=1\n").unwrap();
    let (n, rep) = sweep_with(&mock, &st, &home, now + 180.0).await;
    assert!(n >= 6, "{n}");
    let mut approved = rep.approved.clone();
    approved.sort();
    assert_eq!(approved, ["OLD-1", "apr_00000000000000bb"]);
    assert_eq!(mock.emails.lock().unwrap().as_slice(), ["apr_00000000000000bb"]);
    assert_eq!(card(&st, "NEW-CRED").status, "needsyou");
    assert_eq!(card(&st, "NEW-SNOOZE").status, "needsyou");
    assert_eq!(card(&st, "NEW-REFUSE").status, "needsyou");
    assert_eq!(card(&st, "NEW-425").status, "needsyou");

    // The server.env kill switch stops the job outright.
    seed(&st, "NEW-AFTER-KILL", "lane-a", "decision", "Proceed?", now + 200.0);
    std::fs::write(home.join("server.env"), "AMUX_NEEDS_INPUT_AUTO=0\n").unwrap();
    let rep = tick_with(&mock, &st, &home, now + 240.0).await;
    assert!(!rep.ran);
    assert_eq!(card(&st, "NEW-AFTER-KILL").status, "needsyou");
}

#[test]
fn owner_action_and_public_surface_are_never_auto_approved_but_mentions_are() {
    let all = Policy { enabled: true, other: true, money: true, money_cap_usd: 50.0, prod_data: true, outbound: true, ..Default::default() };
    let never = |at: &str, q: &str| matches!(decide(&all, &item("card", "other", at, q)), Decision::Never(_));
    // Live 2026-09-27: asks the OWNER must act on.
    assert!(never("decision", "Will you run `! ~/.amux/seed-standing-approvals.sh` once to record the two standing approvals?"));
    assert!(never("credential", "Can you mint a new Ethan Personal org API key in Studio?"));
    // Repo rule: new endpoints and public surface stay with the owner.
    assert!(never("decision", "Should POST /v1/organizations/billing/estimate be reachable without an API key?"));
    assert!(never("decision", "Sign off (or amend) the MP-106 PITR API design so restore-to-an-older-checkpoint can be built?"));
    // Mentions are not asks: these are the worker's call.
    assert!(!never("credential", "Want me to do that pass, or would you rather look at the categories yourself first?"));
    assert!(!never("decision", "Want me to go after the Gemini source now, or wait for gs-4 to say whether it's theirs?"));
    assert!(!never("decision", "Should Mixpeek offer a sandbox or demo API key so a prospect can test one call?") );
}

#[test]
fn the_recorders_unblocks_boilerplate_does_not_make_an_ask_a_credential() {
    let all = Policy { enabled: true, other: true, money: true, money_cap_usd: 50.0, prod_data: true, outbound: true, ..Default::default() };
    let mut it = item("card", "other", "credential", "Want me to do that pass, or would you rather look at the categories yourself first?");
    it["unblocks"] = serde_json::json!("ethan completes the sign-in, grant or credential step named in the question and notes it on this card.");
    assert_eq!(decide(&all, &it), Decision::Approve);
}
