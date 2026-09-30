//! Worker worktree hygiene: phantom deletions in sparse worktrees (2026-09-30).
//!
//! # The bug
//!
//! A worker's sparse worktree marks every tracked file outside its cone
//! skip-worktree (`S` in `git ls-files -v`). Git clears that flag when such a
//! file appears on disk, and when the file then goes away again the path reads
//! as an ordinary tracked deletion. Found on the goal-spec-12 workers:
//! `research/ray-s3-vector-backend/cutover-baseline-e2e.log`, outside every
//! gs12 cone, read ` D` in four of seven worktrees. It refused their rebases,
//! and gs12-model re-flagged it by hand ("shows as deleted again"). Nothing in
//! the repo writes that path by name, so the trigger is not pinned; the repair
//! is.
//!
//! # The repair
//!
//! `git sparse-checkout reapply` re-flags out-of-cone entries and leaves the
//! worktree otherwise alone. Checked before this shipped: with one real
//! in-cone deletion and one phantom, reapply cleared the phantom and kept the
//! real deletion. So the sweep runs reapply only when the worktree shows a
//! deletion, never mid-rebase, mid-merge or under a live index lock, and
//! reports exactly which paths it cleared. A deletion that survives reapply is
//! the worker's own and is left alone.
//!
//! Isolated workers are skipped: they receive no amux automation.
//!
//! Every 2 minutes (was 10): the phantom recurred within one 10-minute tick on
//! three workers, which all wrap their pushes in a manual skip-worktree.
//!
//! Logs verdict=worktree_sparse_repaired per lane it fixed, and
//! verdict=worktree_hygiene_sweep per tick with the population considered.

use std::path::Path;
use std::time::Duration;

const TICK_SECS: u64 = 120;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

pub fn spawn() -> super::PeriodicTask {
    super::spawn_periodic(super::registry::ids::WORKTREE_HYGIENE, TICK_SECS, || async {
        sweep().await;
    })
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::process::Command::new("git").arg("-C").arg(dir).args(args).output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Tracked deletions in `git status --porcelain` output.
pub(crate) fn deleted_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix(" D "))
        .map(|p| p.trim().to_string())
        .collect()
}

/// The deletions `reapply` cleared: present before, absent after.
pub(crate) fn repaired(before: &[String], after: &[String]) -> Vec<String> {
    before.iter().filter(|p| !after.contains(p)).cloned().collect()
}

async fn repair(lane: &str, wt: &Path) -> Option<Vec<String>> {
    if git(wt, &["config", "--get", "core.sparseCheckout"]).await?.trim() != "true" {
        return None;
    }
    let git_dir = git(wt, &["rev-parse", "--absolute-git-dir"]).await?;
    let gd = Path::new(git_dir.trim());
    if ["rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD", "index.lock"]
        .iter()
        .any(|m| gd.join(m).exists())
    {
        return None;
    }
    let before = deleted_paths(&git(wt, &["status", "--porcelain", "--untracked-files=no"]).await?);
    if before.is_empty() {
        return Some(Vec::new());
    }
    git(wt, &["sparse-checkout", "reapply"]).await?;
    let after = deleted_paths(&git(wt, &["status", "--porcelain", "--untracked-files=no"]).await?);
    let fixed = repaired(&before, &after);
    if !fixed.is_empty() {
        tracing::warn!(session = lane, repaired = ?fixed, kept_deletions = after.len(), measured = true,
            n_considered = before.len(), verdict = "worktree_sparse_repaired",
            "sparse worktree had out-of-cone files reading as deleted; reapply re-flagged them skip-worktree");
    }
    Some(fixed)
}

async fn sweep() {
    let (mut worktrees, mut lanes_fixed) = (0usize, 0usize);
    for lane in crate::api::session_verbs::all_lane_names() {
        if crate::api::session_verbs::session_is_isolated(&lane) {
            continue;
        }
        let Some(wt) = crate::api::session_verbs::worker_worktree(&lane) else { continue };
        worktrees += 1;
        if repair(&lane, &wt).await.is_some_and(|f| !f.is_empty()) {
            lanes_fixed += 1;
        }
    }
    tracing::debug!(worktrees, lanes_fixed, measured = true, n_considered = worktrees,
        verdict = "worktree_hygiene_sweep", "worker worktree hygiene sweep");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_deletions_reapply_cleared_count_as_repaired() {
        let before = deleted_paths(" D research/x.log\n M docs/a.md\n D docs/gone.md\n");
        assert_eq!(before, vec!["research/x.log", "docs/gone.md"]);
        let after = deleted_paths(" D docs/gone.md\n");
        assert_eq!(repaired(&before, &after), vec!["research/x.log"], "a real deletion is not reported as repaired");
        assert!(repaired(&before, &before).is_empty());
    }
}
