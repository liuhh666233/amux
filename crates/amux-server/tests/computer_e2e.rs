//! Live end-to-end harness for `/api/computer/*` (AMUX-5300). IGNORED by
//! default: it needs Docker, pulls a ~2 GB image, and starts a real desktop.
//!
//! It serves exactly the computer routes (plus the sandbox reaper loop) on a
//! loopback port so the REAL bash CLI can be pointed at it, without starting
//! a second full amux server beside the live one:
//!
//! ```bash
//! AMUX_HOME=/tmp/cua-home AMUX_COMPUTER_E2E_UNTIL=/tmp/cua-e2e.run \
//!   cargo test -p amux-server --test computer_e2e -- --ignored --nocapture
//! # prints: SERVING http://127.0.0.1:<port>
//! AMUX_API=http://127.0.0.1:<port> AMUX_SESSION=cua-e2e ./amux computer start
//! rm /tmp/cua-e2e.run   # ends the harness
//! ```
//!
//! Point AMUX_HOME at a scratch home holding a COPY of a profile under
//! `playwright-auth/profiles/`, so `open --profile` never launches Chrome on a
//! profile the live server may be driving.

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs Docker and starts a real CUA desktop"]
async fn serve_computer_routes_for_the_cli() {
    let until = std::env::var("AMUX_COMPUTER_E2E_UNTIL").unwrap_or_default();
    let _ = tracing_subscriber::fmt().with_ansi(false).try_init();
    amux_server::runtime_jobs::computer_reaper::spawn();
    let dir = tempfile::tempdir().unwrap();
    let store =
        std::sync::Arc::new(amux_server::db::Store::open(&dir.path().join("e2e.db")).unwrap());
    let app: axum::Router = axum::Router::new()
        .nest("/api/computer", amux_server::api::computer::routes())
        .with_state(amux_server::api::AppState {
            store,
            started: std::time::Instant::now(),
            build_hash: "computer-e2e".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    println!("SERVING http://{addr}");
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(3600);
    while std::time::Instant::now() < deadline
        && !until.is_empty()
        && std::path::Path::new(&until).exists()
    {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    server.abort();
}
