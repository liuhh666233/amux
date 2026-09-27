//! AMUX-5286: /api/needs-input over the real router and a migrated store.
//!
//! Pins the queue shape against the REAL issues schema (the unit test uses a
//! hand-written fixture schema), the snooze round trip through prefs, and the
//! owner-only refusal for a worker origin.

use amux_server::api::{router, AppState};
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    worker: Option<&str>,
) -> (u16, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(w) = worker {
        b = b.header("x-amux-session", w);
    }
    let req = b
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn queue_snooze_and_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    // Point the approvals reader at an empty home, never the live one.
    std::env::set_var("AMUX_HOME", dir.path().join("home"));
    let store = amux_server::db::Store::open(&dir.path().join("t.db")).unwrap();
    store
        .write(|c| {
            for (id, ask_type, q, t) in [("NI-1", "decision", "Pick one?", 100), ("NI-2", "budget", "Spend $5?", 500)] {
                c.execute(
                    "INSERT INTO issues (id,title,status,session,created,updated,type,ask_type,ask_question,ask_unblocks,ask_actor,entered_state_at)
                     VALUES (?1,?1,'needsyou','lane-a',?2,?2,'code',?3,?4,'it moves','Ethan',?2)",
                    rusqlite::params![id, t, ask_type, q],
                )?;
            }
            Ok(amux_server::db::WriteOutcome { applied: true, events: vec![] })
        })
        .unwrap();
    let app = router(AppState {
        store: std::sync::Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });

    let (s, q) = call(&app, "GET", "/api/needs-input", None, None).await;
    assert_eq!(s, 200, "{q}");
    assert_eq!(q["measured"], true);
    let keys: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["card:NI-2", "card:NI-1"], "money first, then oldest");

    let (s, r) = call(
        &app,
        "POST",
        "/api/needs-input/snooze",
        Some(json!({"key":"card:NI-2","minutes":60})),
        Some("lane-a"),
    )
    .await;
    assert_eq!(s, 403, "a worker cannot hide the owner's asks: {r}");

    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/snooze",
        Some(json!({"key":"card:NI-2","minutes":60})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    // Idempotent: the same snooze again is still one snoozed item.
    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/snooze",
        Some(json!({"key":"card:NI-2","minutes":60})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (_, q) = call(&app, "GET", "/api/needs-input", None, None).await;
    assert_eq!(q["count"], 1);
    assert_eq!(q["snoozed_count"], 1);

    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/snooze",
        Some(json!({"key":"card:NI-2","until":0})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (_, q) = call(&app, "GET", "/api/needs-input", None, None).await;
    assert_eq!(q["count"], 2, "until:0 clears the snooze");

    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/snooze",
        Some(json!({"key":"bogus","minutes":5})),
        None,
    )
    .await;
    assert_eq!(s, 400);
    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/log",
        Some(json!({"action":"approve","card":"NI-1","outcome":"ok"})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (s, _) = call(
        &app,
        "POST",
        "/api/needs-input/log",
        Some(json!({"action":"nuke"})),
        None,
    )
    .await;
    assert_eq!(s, 400);
}
