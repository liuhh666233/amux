//! Stop idle computer-use sandboxes and sweep orphans (AMUX-5300).
//!
//! A sandbox is a 4 GB Linux desktop. This box has had a forgotten VM hold
//! 22 GB for five days, so nothing here waits for a human to remember: every
//! tick removes (by the `amux-computer` label only, never by image or name)
//! sandboxes idle past AMUX_COMPUTER_IDLE_S, exited ones, duplicates for one
//! lane, and any whose label names no lane. The decision is the pure
//! `integrations::computer::plan_sweep`; this file only applies it and logs a
//! verdict line per removal.
//!
//! WHAT IT WILL NOT DO: start Docker. When the daemon is down there is nothing
//! running to reap, and booting colima to check would be the very resource
//! spend this job exists to prevent.

use crate::integrations::computer as cu;
use std::time::Duration;

fn tick_secs() -> u64 {
    std::env::var("AMUX_COMPUTER_REAP_TICK_S")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60)
}

/// One sweep. Returns how many containers were removed, or None when Docker
/// could not be read (unmeasured, not zero).
pub async fn tick() -> Option<usize> {
    cu::docker_bin()?;
    let boxes = match cu::list().await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("[computer] reaper: docker unreadable, skipping: {e}");
            return None;
        }
    };
    if boxes.is_empty() {
        return Some(0);
    }
    let lim = cu::limits();
    let plan = cu::plan_sweep(&boxes, cu::now(), lim.idle_s, &cu::last_action);
    let mut removed = 0;
    for a in plan {
        match cu::stop_name(&a.name).await {
            Ok(_) => {
                removed += 1;
                tracing::info!(
                    "[computer] verdict=reaped lane={} name={} reason={}",
                    a.lane,
                    a.name,
                    a.reason
                );
            }
            Err(e) => tracing::warn!(
                "[computer] verdict=reap_failed lane={} name={} reason={}: {e}",
                a.lane,
                a.name,
                a.reason
            ),
        }
    }
    Some(removed)
}

pub fn spawn() {
    let interval = Duration::from_secs(tick_secs());
    let h = tokio::spawn(async move {
        let mut t = tokio::time::interval(interval);
        loop {
            t.tick().await;
            crate::runtime_jobs::registry::tick(
                crate::runtime_jobs::registry::ids::COMPUTER_REAPER,
            );
            let _ = tick().await;
        }
    });
    crate::runtime_jobs::registry::adopt(
        crate::runtime_jobs::registry::ids::COMPUTER_REAPER,
        Some(interval),
        &h,
    );
}
