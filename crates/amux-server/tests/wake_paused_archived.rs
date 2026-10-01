//! Waking an archived worker that was also paused (Ethan, 2026-10-01: Wake on
//! an archived worker answered "409: worker is paused; resume it first").
//! Archiving usually happens to a paused worker, so CC_PAUSED rides into the
//! archive; Wake must clear both, through the same resume path as Resume.
//!
//! Its own process, so AMUX_HOME can point at a fixture fleet. The throwaway
//! home refuses a real tmux spawn, so the test asserts what Wake decided, not
//! that a provider launched.

use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

#[tokio::test]
async fn wake_clears_archived_and_paused_instead_of_refusing() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let env = sessions.join("sleeper.env");
    std::fs::write(&env, "CC_DIR=/tmp\nCC_ARCHIVED=1\nCC_PAUSED=1\n").unwrap();
    std::env::set_var("AMUX_HOME", home.path());

    let db = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(Store::open(&db.path().join("amux-test.db")).unwrap());
    let app = router(AppState {
        store,
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });
    let res = app
        .oneshot(Request::builder().method("POST").uri("/api/sessions/sleeper/wake").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.contains("worker is paused; resume it first"),
        "Wake must resume a paused archived worker, not refuse it: {body}"
    );
    let after = std::fs::read_to_string(&env).unwrap();
    assert!(!after.contains("CC_ARCHIVED=1"), "still archived after Wake: {after}");
    assert!(!after.contains("CC_PAUSED=1"), "still paused after Wake: {after}");
}
