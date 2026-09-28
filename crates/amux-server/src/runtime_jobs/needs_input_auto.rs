//! Runs [`crate::api::needs_input_auto::tick_with`] every minute (AMUX-5301).
//!
//! The policy, the ledger and the per-item actions live in the api module; this
//! file is only the loop. Kill switches: `AMUX_NEEDS_INPUT_AUTO=0` in
//! server.env (fleet-wide, re-read every tick), the same key scoped per worker
//! or group, and the per-job `AMUX_NEEDS_INPUT_AUTO_SECS=0`.

use crate::api::AppState;

const JOB: &str = super::registry::ids::NEEDS_INPUT_AUTO;

fn tick_secs() -> u64 {
    crate::config::env_i64("AMUX_NEEDS_INPUT_AUTO_TICK_S", 60).max(30) as u64
}

pub fn spawn(state: AppState) -> super::PeriodicTask {
    super::spawn_periodic(JOB, tick_secs(), move || {
        let st = state.clone();
        async move {
            let home = crate::config::amux_home();
            let _ = crate::api::needs_input_auto::tick_with(
                &crate::api::needs_input_auto::RealActions,
                &st,
                &home,
                crate::config::now_f64(),
            )
            .await;
        }
    })
}
