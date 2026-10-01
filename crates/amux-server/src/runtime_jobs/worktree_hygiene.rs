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
//! # Orphaned push pipelines
//!
//! The same sweep reaps a `pre-push` hook or `git remote-https` whose parent is
//! gone (PPID 1) and whose cwd is a worker worktree, once it has run 2 minutes.
//! Its `git push` is dead, so nothing will ever read its result or send the
//! pack (a worktree with any live `git push` is spared: a hook stage can be
//! reparented to PID 1 while its push lives, verdict=orphaned_push_spared_live_push); it only burns CPU on the gates every other push is waiting behind.
//! Found 2026-09-30: three such pipelines, 24 to 28 minutes old, in exactly the
//! three worktrees whose skip-worktree flag kept flipping. A stale hook writing
//! the index while the worker's next push runs is the likely source of the
//! phantom above. Logs verdict=orphaned_push_reaped.
//!
//! Every 10 minutes, and never in a worktree with a push in flight
//! (2026-10-01, GC-21). The tick was 2 minutes while the trigger was unknown.
//! Mixpeek's pre-push hook now guards the skip-worktree bits itself: it
//! snapshots them at entry, re-checks between stages, restores lost ones and
//! fails the push naming the window. A sweep repairing the bit mid-hook hid
//! the loss from that guard, so a worktree with a live `git push` or
//! pre-push hook is left to the guard (verdict=worktree_repair_deferred_push).
//!
//! Logs verdict=worktree_sparse_repaired per lane it fixed, and
//! verdict=worktree_hygiene_sweep per tick with the population considered.

use std::path::Path;
use std::time::Duration;

const TICK_SECS: u64 = 600;
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

/// `ps -o etime` ("[[dd-]hh:]mm:ss") in seconds.
pub(crate) fn etime_secs(e: &str) -> u64 {
    let (days, rest) = match e.trim().split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, e.trim()),
    };
    let parts: Vec<u64> = rest.split(':').map(|x| x.parse().unwrap_or(0)).collect();
    let hms = match parts.as_slice() {
        [h, m, s] => h * 3600 + m * 60 + s,
        [m, s] => m * 60 + s,
        [s] => *s,
        _ => 0,
    };
    days * 86400 + hms
}

/// Orphaned push-pipeline candidates from `ps -Ao pid=,ppid=,etime=,command=`.
pub(crate) fn orphan_push_pids(ps: &str, min_age: u64) -> Vec<u32> {
    ps.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            let ppid = it.next()?;
            let etime = it.next()?;
            let cmd: Vec<&str> = it.collect();
            let cmd = cmd.join(" ");
            let push = cmd.contains("githooks/pre-push") || cmd.contains("git-core/git remote-https");
            (ppid == "1" && push && etime_secs(etime) >= min_age).then_some(pid)
        })
        .collect()
}

async fn cmd_out(prog: &str, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(GIT_TIMEOUT, tokio::process::Command::new(prog).args(args).kill_on_drop(true).output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn process_cwd(pid: u32) -> Option<String> {
    let proc_link = format!("/proc/{pid}/cwd");
    if let Ok(p) = std::fs::read_link(&proc_link) {
        return Some(p.to_string_lossy().into_owned());
    }
    let out = cmd_out("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).await?;
    out.lines().find_map(|l| l.strip_prefix('n')).map(str::to_string)
}

async fn kill_tree(pid: u32) {
    if let Some(kids) = cmd_out("pgrep", &["-P", &pid.to_string()]).await {
        for k in kids.split_whitespace().filter_map(|k| k.parse::<u32>().ok()) {
            Box::pin(kill_tree(k)).await;
        }
    }
    let _ = cmd_out("kill", &["-9", &pid.to_string()]).await;
}

/// PIDs of live `git push` processes in `ps -Ao pid=,ppid=,etime=,command=`.
pub(crate) fn live_git_push_pids(ps: &str) -> Vec<u32> {
    ps.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            let (_ppid, _etime) = (it.next()?, it.next()?);
            let cmd: Vec<&str> = it.collect();
            let is_push = cmd.first().is_some_and(|c| *c == "git" || c.ends_with("/git")) && cmd.get(1) == Some(&"push");
            is_push.then_some(pid)
        })
        .collect()
}

/// PIDs of a push in flight: a live `git push`, or any process running the
/// pre-push hook (whatever its parent).
pub(crate) fn push_in_flight_pids(ps: &str) -> Vec<u32> {
    let mut pids = live_git_push_pids(ps);
    pids.extend(ps.lines().filter_map(|l| {
        let pid: u32 = l.split_whitespace().next()?.parse().ok()?;
        l.contains("githooks/pre-push").then_some(pid)
    }));
    pids
}

/// Working directories of every push in flight.
async fn push_in_flight_cwds() -> Vec<std::path::PathBuf> {
    let Some(ps) = cmd_out("ps", &["-Ao", "pid=,ppid=,etime=,command="]).await else { return Vec::new() };
    let mut out = Vec::new();
    for pid in push_in_flight_pids(&ps) {
        if let Some(cwd) = process_cwd(pid).await {
            out.push(std::path::PathBuf::from(cwd));
        }
    }
    out
}

async fn reap_orphaned_pushes(worktrees: &[(String, std::path::PathBuf)]) -> usize {
    let Some(ps) = cmd_out("ps", &["-Ao", "pid=,ppid=,etime=,command="]).await else { return 0 };
    // PPID 1 alone does not prove the push is dead (2026-10-01, gs12-compute):
    // a hook stage whose intermediate subshell exited is reparented to PID 1
    // while the hook's own `git push` is still alive. A worktree with ANY live
    // `git push` is never reaped.
    let mut pushing: Vec<std::path::PathBuf> = Vec::new();
    for pid in live_git_push_pids(&ps) {
        if let Some(cwd) = process_cwd(pid).await {
            pushing.push(std::path::PathBuf::from(cwd));
        }
    }
    let mut reaped = 0;
    for pid in orphan_push_pids(&ps, 120) {
        let Some(cwd) = process_cwd(pid).await else { continue };
        let Some((lane, wt)) = worktrees.iter().find(|(_, wt)| Path::new(&cwd).starts_with(wt)) else { continue };
        if pushing.iter().any(|p| p.starts_with(wt)) {
            tracing::info!(session = %lane, pid, measured = true, n_considered = 1,
                verdict = "orphaned_push_spared_live_push",
                "a PPID-1 push process shares its worktree with a live git push; not reaped");
            continue;
        }
        kill_tree(pid).await;
        reaped += 1;
        tracing::warn!(session = %lane, pid, cwd = %cwd, measured = true, n_considered = 1,
            verdict = "orphaned_push_reaped",
            "reaped a push pipeline whose git push had died; its result could never be read");
    }
    reaped
}

async fn sweep() {
    let (mut worktrees, mut lanes_fixed) = (0usize, 0usize);
    let mut paths: Vec<(String, std::path::PathBuf)> = Vec::new();
    let pushing = push_in_flight_cwds().await;
    for lane in crate::api::session_verbs::all_lane_names() {
        if crate::api::session_verbs::session_is_isolated(&lane) {
            continue;
        }
        let Some(wt) = crate::api::session_verbs::worker_worktree(&lane) else { continue };
        worktrees += 1;
        paths.push((lane.clone(), wt.clone()));
        if pushing.iter().any(|p| p.starts_with(&wt)) {
            tracing::info!(session = %lane, measured = true, n_considered = 1,
                verdict = "worktree_repair_deferred_push",
                "push in flight in this worktree; its pre-push guard owns the skip-worktree bits, not this sweep");
            continue;
        }
        if repair(&lane, &wt).await.is_some_and(|f| !f.is_empty()) {
            lanes_fixed += 1;
        }
    }
    let reaped = reap_orphaned_pushes(&paths).await;
    tracing::debug!(worktrees, lanes_fixed, reaped, measured = true, n_considered = worktrees,
        verdict = "worktree_hygiene_sweep", "worker worktree hygiene sweep");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parses_every_ps_shape() {
        assert_eq!(etime_secs("00:05"), 5);
        assert_eq!(etime_secs("24:16"), 24 * 60 + 16);
        assert_eq!(etime_secs("01:02:03"), 3723);
        assert_eq!(etime_secs("2-01:00:00"), 2 * 86400 + 3600);
    }

    #[test]
    fn only_old_orphaned_push_processes_are_candidates() {
        let ps = "92498 1 28:05 bash /Users/ethan/Dev/mixpeek/.githooks/pre-push origin https://github.com/mixpeek/mixpeek.git\n\
                  91989 1 26:55 /usr/local/Cellar/git/2.39.0/libexec/git-core/git remote-https origin https://github.com/x.git\n\
                  12681 11945 06:44 bash /Users/ethan/Dev/mixpeek/.githooks/pre-push origin https://github.com/x.git\n\
                  555 1 00:30 bash /Users/ethan/Dev/mixpeek/.githooks/pre-push origin x\n\
                  777 1 50:00 /usr/sbin/cron\n";
        assert_eq!(orphan_push_pids(ps, 120), vec![92498, 91989],
            "live-parent hooks, young orphans and unrelated PPID-1 processes are left alone");
    }

    #[test]
    fn live_git_push_processes_are_found_by_argv() {
        let ps = "95959 1200 25:00 git push origin HEAD:main\n\
                  47045 39576 17:55 /usr/local/bin/git push origin c80236:main\n\
                  47082 47045 17:55 /usr/local/Cellar/git/2.39.0/libexec/git-core/git remote-https origin x\n\
                  555 1 01:00 bash -c echo git push later\n";
        assert_eq!(live_git_push_pids(ps), vec![95959, 47045],
            "only a real `git push` argv counts, not remote-https or a shell mentioning it");
    }

    #[test]
    fn a_hook_or_a_push_marks_the_worktree_busy() {
        let ps = "100 99 01:00 git push origin HEAD:main\n\
                  101 100 01:00 bash /Users/ethan/Dev/mixpeek/.githooks/pre-push origin x\n\
                  102 1 01:00 bash /Users/ethan/Dev/mixpeek/.githooks/pre-push origin x\n\
                  103 1 01:00 /usr/sbin/cron\n";
        assert_eq!(push_in_flight_pids(ps), vec![100, 101, 102]);
    }

    #[test]
    fn only_deletions_reapply_cleared_count_as_repaired() {
        let before = deleted_paths(" D research/x.log\n M docs/a.md\n D docs/gone.md\n");
        assert_eq!(before, vec!["research/x.log", "docs/gone.md"]);
        let after = deleted_paths(" D docs/gone.md\n");
        assert_eq!(repaired(&before, &after), vec!["research/x.log"], "a real deletion is not reported as repaired");
        assert!(repaired(&before, &before).is_empty());
    }
}
