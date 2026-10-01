//! A chat turn outlives a server restart and its reply still arrives.
//!
//! Ethan, 2026-10-01, on "The amux server restarted during this turn, so the
//! reply was lost. Send the message again to retry.": "this needs to be better
//! like terminal auto". A terminal worker keeps running through a restart
//! because tmux owns it. A chat turn now runs detached (its own process group,
//! output to files under chat-state/turns/), and a restarted server re-attaches.
//!
//! The previous server process is simulated by doing exactly what it leaves
//! behind: a provider started detached, writing to the turn's files, and the
//! in-flight markers in meta. Recovery must FOLLOW that turn to the end and
//! record its reply once, with no error row and no second provider run. Own
//! process, because AMUX_HOME is global.
use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

// Slow on purpose: the reply's second half is written AFTER recovery starts
// following the turn, so a passing test proves the follower kept reading.
const SLOW_CLAUDE: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_ARGS_LOG"
sid=""
while [ $# -gt 0 ]; do
  case "$1" in --session-id|--resume) sid="$2"; shift;; esac
  shift
done
input=$(cat)
python3 -c 'import json,sys; print(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first half, "}}}))'
sleep 1.5
python3 -c 'import json,sys; print(json.dumps({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"second half: "+sys.argv[1]}}}))' "$input"
printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$sid\"}"
"#;

async fn history(app: &axum::Router) -> Value {
    let req = Request::builder().uri("/api/sessions/chatty/chat").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_turn_running_across_a_restart_is_reattached_and_its_reply_recorded_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let turns = home.join("chat-state/turns");
    std::fs::create_dir_all(&turns).unwrap();
    let fake = tmp.path().join("fake-claude");
    std::fs::write(&fake, SLOW_CLAUDE).unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
    let args_log = tmp.path().join("args.log");
    unsafe {
        std::env::set_var("AMUX_HOME", &home);
        std::env::set_var("AMUX_CHAT_CLAUDE_BIN", &fake);
        std::env::set_var("FAKE_ARGS_LOG", &args_log);
    }
    let dir = tmp.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        home.join("sessions/chatty.env"),
        format!("CC_DIR=\"{}\"\nCC_WORKER_TYPE=\"chat\"\n", dir.display()),
    )
    .unwrap();

    // What the previous server process did before it was replaced: started the
    // provider DETACHED (new process group, files for stdin/stdout/exit) ...
    let turn = "TURN-ACROSS-RESTART";
    let stem = turns.join(format!("{turn}.0"));
    let p = |ext: &str| format!("{}.{ext}", stem.display());
    std::fs::write(p("in"), "what is the status?").unwrap();
    let inner = format!(
        "{} --session-id conv-1 \"$@\"; ec=$?; printf '%s' \"$ec\" > '{tmp}' && mv '{tmp}' '{exit}'",
        fake.display(),
        tmp = p("exit.tmp"),
        exit = p("exit"),
    );
    let launcher = format!(
        "bash -c \"$0\" amux-chat < '{}' > '{}' 2> '{}' & echo $!",
        p("in"),
        p("out"),
        p("err")
    );
    let child = std::process::Command::new("bash")
        .arg("-c")
        .arg(&launcher)
        .arg(&inner)
        .process_group(0)
        .output()
        .unwrap();
    assert!(child.status.success());
    // ... and left its in-flight markers. The user message row exists already.
    let pgid = {
        // The launcher was the group leader; read the group from the turn's pid.
        let pid: i32 = String::from_utf8_lossy(&child.stdout).trim().parse().unwrap();
        unsafe { libc::getpgid(pid) }
    };
    std::fs::write(
        home.join("sessions/chatty.meta.json"),
        json!({
            "chat_running": true,
            "chat_turns": 1,
            "chat_conversation_id": "conv-1",
            "cc_conversation_id": "conv-1",
            "chat_inflight_turn": turn,
            "chat_inflight_text": "what is the status?",
            "chat_inflight_origin": "owner",
            "chat_inflight_files": stem.to_string_lossy(),
            "chat_inflight_pgid": pgid,
            "chat_inflight_provider": "claude",
            "chat_inflight_started": 1.0,
        })
        .to_string(),
    )
    .unwrap();

    // The restarted server.
    let store = Arc::new(Store::open(&tmp.path().join("t.db")).unwrap());
    let state = AppState {
        store,
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let app = router(state.clone());
    let (workers, interrupted, _resumed) = amux_server::api::chat_worker::recover_all(&state).await;
    assert_eq!((workers, interrupted), (1, 0), "a re-attached turn is not reported as interrupted");

    let mut h = history(&app).await;
    for _ in 0..200 {
        let done = h["messages"].as_array().unwrap().iter().any(|m| m["role"] == "assistant" && m["turn_id"] == turn);
        if done && h["busy"] == false {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        h = history(&app).await;
    }
    let msgs = h["messages"].as_array().unwrap();
    let replies: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "assistant" && m["turn_id"] == turn).collect();
    assert_eq!(replies.len(), 1, "the reply is recorded exactly once: {msgs:?}");
    let r = replies[0];
    assert!(r["error"].is_null(), "no error row for a turn that survived the restart: {r}");
    assert_eq!(r["text"], "first half, second half: what is the status?", "the whole reply, both halves: {r}");
    let log = std::fs::read_to_string(&args_log).unwrap();
    assert_eq!(log.lines().count(), 1, "the provider ran once; the turn was followed, not re-run");
    assert!(!std::path::Path::new(&p("out")).exists(), "the turn's files are removed once its reply is recorded");
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(home.join("sessions/chatty.meta.json")).unwrap()).unwrap();
    assert_eq!(meta["chat_inflight_turn"], "", "the in-flight marker is cleared");

    // Idempotent: another restart with nothing in flight does nothing.
    assert_eq!(amux_server::api::chat_worker::recover_all(&state).await, (1, 0, 0));
}
