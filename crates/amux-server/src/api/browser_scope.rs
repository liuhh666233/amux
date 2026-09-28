//! Scoped access to saved browser profiles (AMUX-5307).
//!
//! Ethan, 2026-09-27: "there's a lot of browser profiles, we should have scopes
//! for accessing certain profiles per group/worker/global but by default all
//! workers should be able to discover all profiles."
//!
//! DISCOVERY stays open: `GET /api/browser/profiles` lists every profile to
//! every caller. USE is scoped: opening a profile for a worker goes through
//! [`profile_allowed`], which reads two keys from the same worker > group >
//! global env layers every other scoped setting uses (`scope_env_layers`):
//!
//! - `AMUX_BROWSER_PROFILES_ALLOW`: comma list of names or globs (`persona-*`,
//!   `*` for all).
//! - `AMUX_BROWSER_PROFILES_DENY`: same shape.
//!
//! Resolution walks the tiers most specific first (worker, then the worker's
//! groups as one tier, then global). At each tier a matching DENY refuses; then,
//! if that tier defines an ALLOW list, the profile is allowed when it matches
//! and refused when it does not (the most specific allow list REPLACES the
//! broader ones, which is what lets a worker be narrowed below a global `*`).
//! A tier with neither key, or only a deny that does not match, defers to the
//! next. Nothing configured anywhere means allow: today's behaviour.
//!
//! Owner requests (no worker identity: the dashboard, a bare curl) are always
//! allowed. The worker identity is the same self-declared `X-Amux-Session`
//! every other browser verb uses, so this is a policy for well-behaved lanes,
//! not a sandbox against a hostile one.

use super::session_verbs::{scope_env_layers, EnvFile};
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) const ALLOW_KEY: &str = "AMUX_BROWSER_PROFILES_ALLOW";
pub(crate) const DENY_KEY: &str = "AMUX_BROWSER_PROFILES_DENY";

/// Glob match: `*` is any run, `?` one character. Case-insensitive, because
/// profile names are typed by hand and `Persona-*` meaning nothing would be a
/// silent grant failure.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    // Iterative wildcard match with single-star backtracking.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Parse a list value: commas or whitespace separate, quotes are stripped.
/// Empty means "not set", the same way `scoped_setting_in` treats a blank.
pub(crate) fn parse_list(raw: &str) -> Option<Vec<String>> {
    let v: Vec<String> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    (!v.is_empty()).then_some(v)
}

/// One scope layer's own settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Layer {
    /// `worker:<name>`, `group:<name>` or `global`.
    pub scope: String,
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

/// The rule that decided a verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Rule {
    /// `AMUX_BROWSER_PROFILES_ALLOW`, `AMUX_BROWSER_PROFILES_DENY`, or
    /// `default` / `owner` when no key decided it.
    pub key: String,
    /// The matching pattern, or the whole list when an allow list did NOT
    /// match (there is no single pattern to name then).
    pub value: String,
    /// Where it came from: `worker:<n>`, `group:<g>`, `global`, `default`, `owner`.
    pub scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub allowed: bool,
    pub rule: Rule,
    pub reason: String,
}

/// A worker's resolved policy: tiers, most specific first. Groups share a tier.
#[derive(Clone, Debug, Default)]
pub(crate) struct Policy {
    pub tiers: Vec<Vec<Layer>>,
}

fn layer_from(path: &Path, scope: String) -> Layer {
    let f = EnvFile::load(path);
    Layer {
        scope,
        allow: f.get(ALLOW_KEY).and_then(parse_list),
        deny: f.get(DENY_KEY).and_then(parse_list),
    }
}

/// Label an env layer path the way the Scope tab names levels.
fn scope_label(home: &Path, path: &Path) -> String {
    if path == home.join("amux.env") {
        return "global".into();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if path.parent() == Some(&home.join("env")) {
        format!("group:{stem}")
    } else {
        format!("worker:{stem}")
    }
}

impl Policy {
    /// The policy for a worker, read from the SAME layer list the launched
    /// process sources (`scope_env_layers`), so a gate and the lane's own shell
    /// cannot disagree about which files are in scope.
    pub(crate) fn for_worker(home: &Path, worker: &str) -> Policy {
        let (mut w, mut g, mut gl) = (Vec::new(), Vec::new(), Vec::new());
        for path in scope_env_layers(home, worker) {
            let scope = scope_label(home, &path);
            let layer = layer_from(&path, scope.clone());
            if scope == "global" {
                gl.push(layer);
            } else if scope.starts_with("group:") {
                g.push(layer);
            } else {
                w.push(layer);
            }
        }
        Policy {
            tiers: vec![w, g, gl],
        }
    }

    /// The policy a member of `group` with no worker-level override gets.
    pub(crate) fn for_group(home: &Path, group: &str) -> Policy {
        let gp = home.join("env").join(format!("{}.env", group.to_lowercase()));
        Policy {
            tiers: vec![
                vec![layer_from(&gp, format!("group:{}", group.to_lowercase()))],
                vec![layer_from(&home.join("amux.env"), "global".into())],
            ],
        }
    }

    pub(crate) fn for_global(home: &Path) -> Policy {
        Policy {
            tiers: vec![vec![layer_from(&home.join("amux.env"), "global".into())]],
        }
    }

    pub(crate) fn decide(&self, profile: &str) -> Verdict {
        let profile = canonical(profile);
        for tier in &self.tiers {
            // DENY beats ALLOW at the same scope.
            for l in tier {
                if let Some(p) = l
                    .deny
                    .iter()
                    .flatten()
                    .find(|p| glob_match(p, &profile))
                {
                    return Verdict {
                        allowed: false,
                        reason: format!(
                            "profile '{profile}' matches {DENY_KEY} pattern '{p}' at {}",
                            l.scope
                        ),
                        rule: Rule {
                            key: DENY_KEY.into(),
                            value: p.clone(),
                            scope: l.scope.clone(),
                        },
                    };
                }
            }
            let with_allow: Vec<&Layer> = tier.iter().filter(|l| l.allow.is_some()).collect();
            if with_allow.is_empty() {
                continue;
            }
            for l in &with_allow {
                if let Some(p) = l
                    .allow
                    .iter()
                    .flatten()
                    .find(|p| glob_match(p, &profile))
                {
                    return Verdict {
                        allowed: true,
                        reason: format!(
                            "profile '{profile}' matches {ALLOW_KEY} pattern '{p}' at {}",
                            l.scope
                        ),
                        rule: Rule {
                            key: ALLOW_KEY.into(),
                            value: p.clone(),
                            scope: l.scope.clone(),
                        },
                    };
                }
            }
            let scope = with_allow
                .iter()
                .map(|l| l.scope.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let value = with_allow
                .iter()
                .flat_map(|l| l.allow.iter().flatten().cloned())
                .collect::<Vec<_>>()
                .join(",");
            return Verdict {
                allowed: false,
                reason: format!(
                    "profile '{profile}' is not in the {ALLOW_KEY} list at {scope} ({value}); \
                     the most specific allow list replaces broader ones"
                ),
                rule: Rule {
                    key: ALLOW_KEY.into(),
                    value,
                    scope,
                },
            };
        }
        Verdict {
            allowed: true,
            reason: "no scope sets a profile rule; every profile is allowed by default".into(),
            rule: Rule {
                key: "default".into(),
                value: "*".into(),
                scope: "default".into(),
            },
        }
    }
}

/// A blank profile is the default profile everywhere else in this API.
fn canonical(profile: &str) -> String {
    let p = profile.trim();
    if p.is_empty() {
        "default".into()
    } else {
        p.to_string()
    }
}

/// A refused use, carrying everything the 403 body needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDenied {
    pub worker: String,
    pub profile: String,
    pub rule: Rule,
    pub reason: String,
    /// The profiles this worker IS allowed, so a refusal names a way forward.
    pub allowed_profiles: Vec<String>,
}

impl ProfileDenied {
    pub fn body(&self) -> Value {
        json!({
            "error": format!(
                "worker '{}' may not use browser profile '{}': {}",
                self.worker, self.profile, self.reason
            ),
            "code": "profile_not_in_scope",
            "worker": self.worker,
            "profile": self.profile,
            "rule": self.rule,
            "reason": self.reason,
            "allowed_profiles": self.allowed_profiles,
            "hint": format!(
                "use one of allowed_profiles, or ask the owner to change {ALLOW_KEY} / \
                 {DENY_KEY} in the Scope tab (worker > group > global; deny beats allow \
                 at the same scope). GET /api/browser/profile-access?level=worker&name={} \
                 shows the whole resolution.",
                self.worker
            ),
        })
    }

    pub fn response(&self) -> Response {
        (StatusCode::FORBIDDEN, Json(self.body())).into_response()
    }
}

/// THE ENFORCEMENT FUNCTION. Every place that opens a profile for a worker
/// calls this: `/api/browser/start`, `/profile/create`, `/profile/combine`,
/// the driver verbs' browser resolution, and the CUA sandbox loader
/// (AMUX-5300). An empty `worker` is the owner and is always allowed.
///
/// Logs one verdict line per call (`browser_profile_scope: allow|deny`), so a
/// sweep of the server log shows who was refused what and by which rule.
pub fn profile_allowed(worker: &str, profile: &str) -> Result<(), Box<ProfileDenied>> {
    profile_allowed_in(&super::session_verbs::home(), worker, profile).map(|_| ())
}

pub(crate) fn profile_allowed_in(
    home: &Path,
    worker: &str,
    profile: &str,
) -> Result<Verdict, Box<ProfileDenied>> {
    let worker = worker.trim();
    let profile = canonical(profile);
    if worker.is_empty() {
        return Ok(Verdict {
            allowed: true,
            reason: "owner request (no worker identity): always allowed".into(),
            rule: Rule {
                key: "owner".into(),
                value: "*".into(),
                scope: "owner".into(),
            },
        });
    }
    let policy = Policy::for_worker(home, worker);
    let v = policy.decide(&profile);
    if v.allowed {
        tracing::info!(
            worker, profile = %profile, rule_key = %v.rule.key, rule = %v.rule.value,
            scope = %v.rule.scope, "browser_profile_scope: allow"
        );
        return Ok(v);
    }
    let allowed_profiles: Vec<String> =
        crate::integrations::browser::list_profiles(home, false)
            .into_iter()
            .map(|p| p.name)
            .filter(|n| policy.decide(n).allowed)
            .collect();
    tracing::warn!(
        worker, profile = %profile, rule_key = %v.rule.key, rule = %v.rule.value,
        scope = %v.rule.scope, n_allowed = allowed_profiles.len(),
        "browser_profile_scope: deny ({})", v.reason
    );
    Err(Box::new(ProfileDenied {
        worker: worker.to_string(),
        profile,
        rule: v.rule,
        reason: v.reason,
        allowed_profiles,
    }))
}

/// Every worker with a scope file, and every group known from CC_TAGS or an
/// env/<group>.env file. The universe the profiles listing reports access over.
pub(crate) fn known_workers_and_groups(home: &Path) -> (Vec<String>, Vec<String>) {
    let mut workers = std::collections::BTreeSet::new();
    let mut groups = std::collections::BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(home.join("sessions")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("env") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            workers.insert(stem.to_string());
            if let Some(tags) = EnvFile::load(&p).get("CC_TAGS") {
                for t in tags.split(',') {
                    let t = t.trim().trim_matches('"').to_lowercase();
                    if !t.is_empty() {
                        groups.insert(t);
                    }
                }
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(home.join("env")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("env") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if !stem.starts_with('.') {
                        groups.insert(stem.to_lowercase());
                    }
                }
            }
        }
    }
    (workers.into_iter().collect(), groups.into_iter().collect())
}

/// Per-profile access summary for the listing: who can use it.
pub(crate) struct AccessIndex {
    workers: Vec<(String, Policy)>,
    groups: Vec<(String, Policy)>,
    you: Option<(String, Policy)>,
}

impl AccessIndex {
    pub(crate) fn build(home: &Path, you: Option<&str>) -> AccessIndex {
        let (ws, gs) = known_workers_and_groups(home);
        AccessIndex {
            workers: ws
                .into_iter()
                .map(|w| {
                    let p = Policy::for_worker(home, &w);
                    (w, p)
                })
                .collect(),
            groups: gs
                .into_iter()
                .map(|g| {
                    let p = Policy::for_group(home, &g);
                    (g, p)
                })
                .collect(),
            you: you
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|w| (w.to_string(), Policy::for_worker(home, w))),
        }
    }

    pub(crate) fn for_profile(&self, profile: &str) -> Value {
        let denied_workers: Vec<&str> = self
            .workers
            .iter()
            .filter(|(_, p)| !p.decide(profile).allowed)
            .map(|(w, _)| w.as_str())
            .collect();
        let denied_groups: Vec<&str> = self
            .groups
            .iter()
            .filter(|(_, p)| !p.decide(profile).allowed)
            .map(|(g, _)| g.as_str())
            .collect();
        let you = self.you.as_ref().map(|(w, p)| {
            let v = p.decide(profile);
            json!({"worker": w, "allowed": v.allowed, "rule": v.rule, "reason": v.reason})
        });
        json!({
            "all_workers": denied_workers.is_empty(),
            "workers_total": self.workers.len(),
            "workers_allowed": self.workers.len() - denied_workers.len(),
            "denied_workers": denied_workers,
            "denied_groups": denied_groups,
            "allowed_for_you": you.as_ref().map(|v| v["allowed"].clone()).unwrap_or(json!(true)),
            "you": you,
        })
    }
}

#[derive(Deserialize)]
pub(crate) struct AccessQuery {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// `GET /api/browser/profile-access?level=worker|group|global&name=`
///
/// What this level sets (the raw allow/deny lists, which the generic scope
/// read masks because env values can be credentials; these two never are),
/// and the EFFECTIVE verdict for every profile at that level. The Scope tab's
/// profile editor renders this and writes back through `PUT /api/scope`
/// (capability `env`), so the write path and its authorization stay the one
/// the rest of the Scope tab uses.
pub(crate) async fn profile_access(
    headers: HeaderMap,
    Query(q): Query<AccessQuery>,
) -> Response {
    let home = super::session_verbs::home();
    let level = q.level.as_deref().map(str::trim).unwrap_or("").to_string();
    let level = if level.is_empty() { "worker".to_string() } else { level };
    let name = q.name.as_deref().map(str::trim).unwrap_or("").to_string();
    let name = if name.is_empty() && level == "worker" {
        headers
            .get("x-amux-session")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    } else {
        name
    };
    let valid_name = |n: &str| {
        !n.is_empty()
            && n
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !n.starts_with('.')
    };
    let (policy, here_path) = match level.as_str() {
        "global" => (Policy::for_global(&home), home.join("amux.env")),
        "group" if valid_name(&name) => (
            Policy::for_group(&home, &name),
            home.join("env").join(format!("{}.env", name.to_lowercase())),
        ),
        "worker" if valid_name(&name) => (
            Policy::for_worker(&home, &name),
            home.join("sessions").join(format!("{name}.env")),
        ),
        "group" | "worker" => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{level} level needs a valid name")})),
            )
                .into_response()
        }
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("unknown level '{other}' (worker, group, global)")})),
            )
                .into_response()
        }
    };
    let here = EnvFile::load(&here_path);
    let profiles: Vec<Value> = crate::integrations::browser::list_profiles(&home, false)
        .into_iter()
        .map(|p| {
            let v = policy.decide(&p.name);
            json!({"name": p.name, "allowed": v.allowed, "rule": v.rule, "reason": v.reason})
        })
        .collect();
    let n_allowed = profiles.iter().filter(|p| p["allowed"] == json!(true)).count();
    Json(json!({
        "level": level,
        "name": if level == "global" { Value::Null } else { json!(name) },
        "keys": {"allow": ALLOW_KEY, "deny": DENY_KEY},
        "set_here": {
            "allow": here.get(ALLOW_KEY).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            "deny": here.get(DENY_KEY).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        },
        "tiers": policy.tiers,
        "profiles": profiles,
        "n_allowed": n_allowed,
        "measured": true,
        "n_considered": profiles.len(),
        "precedence": "worker > group > global; at each scope a matching DENY refuses, then an \
                       ALLOW list (if set) allows on match and refuses otherwise; unset \
                       everywhere means allow. Owner requests are always allowed.",
        "write": "PUT /api/scope {level, name, capability:\"env\", value:{AMUX_BROWSER_PROFILES_ALLOW:..., \
                  AMUX_BROWSER_PROFILES_DENY:...}} (null removes a key)",
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn home_with(global: &str, groups: &[(&str, &str)], worker: (&str, &str)) -> tempfile::TempDir {
        let h = tempfile::tempdir().unwrap();
        write(&h.path().join("amux.env"), global);
        for (g, t) in groups {
            write(&h.path().join("env").join(format!("{g}.env")), t);
        }
        write(
            &h.path().join("sessions").join(format!("{}.env", worker.0)),
            worker.1,
        );
        h
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("persona-*", "persona-alice"));
        assert!(glob_match("persona-*", "persona-"));
        assert!(!glob_match("persona-*", "ethan-tubescience"));
        assert!(glob_match("*-tubescience", "ethan-tubescience"));
        assert!(glob_match("p?rsona-*", "PERSONA-x"), "case-insensitive");
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("default", "default"));
        assert!(!glob_match("default", "default2"));
        assert_eq!(
            parse_list(" \"a, b\" c,,"),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(parse_list("  ,"), None);
    }

    #[test]
    fn nothing_configured_allows_everything() {
        let h = home_with("", &[], ("w1", ""));
        let v = profile_allowed_in(h.path(), "w1", "persona-x").unwrap();
        assert_eq!(v.rule.scope, "default");
    }

    #[test]
    fn worker_overrides_group_overrides_global() {
        let h = home_with(
            "AMUX_BROWSER_PROFILES_DENY=netsuite\n",
            &[("ops", "AMUX_BROWSER_PROFILES_ALLOW=netsuite,default\n")],
            ("w1", "CC_TAGS=ops\nAMUX_BROWSER_PROFILES_ALLOW=persona-*\n"),
        );
        // worker allow list replaces the group's: netsuite not in it -> deny at worker.
        let e = profile_allowed_in(h.path(), "w1", "netsuite").unwrap_err();
        assert_eq!(e.rule.scope, "worker:w1");
        assert_eq!(e.rule.key, ALLOW_KEY);
        assert!(profile_allowed_in(h.path(), "w1", "persona-bob").is_ok());

        // a member without a worker override: group allow beats global deny.
        write(&h.path().join("sessions").join("w2.env"), "CC_TAGS=ops\n");
        let v = profile_allowed_in(h.path(), "w2", "netsuite").unwrap();
        assert_eq!(v.rule.scope, "group:ops");
        let e = profile_allowed_in(h.path(), "w2", "persona-bob").unwrap_err();
        assert_eq!(e.rule.scope, "group:ops");

        // not in the group: global deny applies, everything else defaults open.
        write(&h.path().join("sessions").join("w3.env"), "");
        let e = profile_allowed_in(h.path(), "w3", "netsuite").unwrap_err();
        assert_eq!((e.rule.key.as_str(), e.rule.scope.as_str()), (DENY_KEY, "global"));
        assert!(profile_allowed_in(h.path(), "w3", "default").is_ok());
    }

    #[test]
    fn deny_beats_allow_at_the_same_scope() {
        let h = home_with(
            "",
            &[],
            (
                "w1",
                "AMUX_BROWSER_PROFILES_ALLOW=*\nAMUX_BROWSER_PROFILES_DENY=persona-*\n",
            ),
        );
        let e = profile_allowed_in(h.path(), "w1", "persona-a").unwrap_err();
        assert_eq!(e.rule.key, DENY_KEY);
        assert_eq!(e.rule.value, "persona-*");
        assert!(profile_allowed_in(h.path(), "w1", "default").is_ok());
    }

    #[test]
    fn a_worker_allow_reopens_a_global_deny() {
        let h = home_with(
            "AMUX_BROWSER_PROFILES_DENY=ethan-*\n",
            &[],
            ("w1", "AMUX_BROWSER_PROFILES_ALLOW=ethan-tubescience\n"),
        );
        let v = profile_allowed_in(h.path(), "w1", "ethan-tubescience").unwrap();
        assert_eq!(v.rule.scope, "worker:w1");
    }

    #[test]
    fn owner_bypasses_every_rule() {
        let h = home_with("AMUX_BROWSER_PROFILES_DENY=*\n", &[], ("w1", ""));
        let v = profile_allowed_in(h.path(), "", "anything").unwrap();
        assert_eq!(v.rule.scope, "owner");
        assert!(profile_allowed_in(h.path(), "w1", "anything").is_err());
    }

    #[test]
    fn refusal_body_names_rule_scope_and_the_allowed_profiles() {
        let h = home_with(
            "AMUX_BROWSER_PROFILES_ALLOW=default,persona-*\n",
            &[],
            ("w1", ""),
        );
        for p in ["persona-a", "persona-b", "ethan-tubescience"] {
            std::fs::create_dir_all(h.path().join("playwright-auth/profiles").join(p)).unwrap();
        }
        std::fs::create_dir_all(h.path().join("playwright-auth/profile")).unwrap();
        let e = profile_allowed_in(h.path(), "w1", "ethan-tubescience").unwrap_err();
        let b = e.body();
        assert_eq!(b["code"], "profile_not_in_scope");
        assert_eq!(b["rule"]["scope"], "global");
        assert_eq!(b["rule"]["key"], ALLOW_KEY);
        assert_eq!(b["rule"]["value"], "default,persona-*");
        assert_eq!(b["allowed_profiles"], json!(["default", "persona-a", "persona-b"]));
        assert_eq!(e.response().status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn a_blank_profile_is_checked_as_default() {
        let h = home_with("AMUX_BROWSER_PROFILES_DENY=default\n", &[], ("w1", ""));
        let e = profile_allowed_in(h.path(), "w1", "  ").unwrap_err();
        assert_eq!(e.profile, "default");
    }

    #[test]
    fn access_index_names_denied_workers_and_groups() {
        let h = home_with(
            "",
            &[("gtm", "AMUX_BROWSER_PROFILES_DENY=netsuite\n")],
            ("w1", "CC_TAGS=gtm\n"),
        );
        write(&h.path().join("sessions").join("w2.env"), "");
        let idx = AccessIndex::build(h.path(), Some("w1"));
        let a = idx.for_profile("netsuite");
        assert_eq!(a["all_workers"], json!(false));
        assert_eq!(a["denied_workers"], json!(["w1"]));
        assert_eq!(a["denied_groups"], json!(["gtm"]));
        assert_eq!(a["allowed_for_you"], json!(false));
        let b = idx.for_profile("default");
        assert_eq!(b["all_workers"], json!(true));
        assert_eq!(b["allowed_for_you"], json!(true));
    }
}
