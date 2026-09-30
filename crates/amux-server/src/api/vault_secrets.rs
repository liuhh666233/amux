//! Vault secrets: encrypted key/value items scoped to the whole fleet, a group
//! or one worker, delivered into a worker's environment at launch (AMUX-5375,
//! Ethan 2026-09-30: "an encrypted, local, editable secrets vault scoped per
//! key/value at global, group or worker level").
//!
//! # Where it fits
//!
//! The same vault as the payment cards in `vault.rs`: same `/api/vault` family,
//! same `~/.amux/vault/` directory, same `logs/vault-audit.jsonl`. Scoping is
//! the scope env files' model (`scope_env_layers`): global, then each group the
//! worker is in (sorted, later wins), then the worker. A worker gets one value
//! per key, the most specific one. The difference from those files is storage:
//! they are plaintext, these are not.
//!
//! # Encryption
//!
//! Each value is sealed with AES-256-GCM under a random 96-bit nonce. The
//! associated data binds the ciphertext to its item id, key and scope, so a
//! ciphertext copied onto another item (say, from a worker scope to global)
//! fails to open instead of delivering a value to the wrong place.
//!
//! The 32-byte master key never touches the vault directory on macOS: it lives
//! in the login Keychain (service `amux-vault-master`, one account per
//! AMUX_HOME so a throwaway test home can never read or replace the real key).
//! It is written through `security -i` on stdin, so the key is never an argv
//! element. Elsewhere, or with `AMUX_VAULT_KEYSTORE=file`, it is a 0600 file
//! beside the store, which is the same trust level as `server.env`, and every
//! load of that backend says so in the log.
//!
//! What this protects against: a copied or backed-up `~/.amux` (the restic
//! backup ships it to B2), a stray `cat`, a grep over the home directory, a
//! worker reading the store file. What it does not: a process running as this
//! user that asks the Keychain, which is also what the server itself does.
//!
//! # Delivery
//!
//! `launch_values` resolves a worker's items and the start path hands them to
//! its existing `deferred_secrets` route (AMUX-4803): `tmux set-environment`
//! after the session exists, imported by a typed command that carries no
//! value. So a value never appears in the tmux server's argv, the pane's
//! command line, the board, a log line or history. The audit line names keys
//! and scopes, never values.
//!
//! # Who can write
//!
//! Only the owner (a request with no worker origin, which is the dashboard).
//! A worker origin is refused on every write and sees only the metadata of
//! items that apply to it. No endpoint returns a value, to anyone.

use crate::config::{amux_home, now_f64};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::{Path as AxPath, RawQuery};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use p256::elliptic_curve::rand_core::{OsRng, RngCore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const STORE_VERSION: u64 = 1;
const KEYCHAIN_SERVICE: &str = "amux-vault-master";
const MAX_VALUE_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Global,
    Group(String),
    Worker(String),
}

impl Scope {
    fn kind(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Group(_) => "group",
            Scope::Worker(_) => "worker",
        }
    }
    fn target(&self) -> &str {
        match self {
            Scope::Global => "",
            Scope::Group(g) | Scope::Worker(g) => g,
        }
    }
    fn parse(kind: &str, target: &str) -> Result<Scope, String> {
        let target = target.trim();
        let name_ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        match kind.trim() {
            "global" => Ok(Scope::Global),
            // Group names are compared lowercased, as scope_env_layers does.
            "group" if name_ok(target) => Ok(Scope::Group(target.to_lowercase())),
            "worker" if name_ok(target) => Ok(Scope::Worker(target.to_string())),
            "group" | "worker" => Err(format!("scope {kind:?} needs a valid target name")),
            other => Err(format!("scope must be global, group or worker, not {other:?}")),
        }
    }
    /// Order within a worker's resolution: higher wins.
    fn rank(&self) -> u8 {
        match self {
            Scope::Global => 0,
            Scope::Group(_) => 1,
            Scope::Worker(_) => 2,
        }
    }
}

/// A key a worker's environment can carry. Harness variables are refused: a
/// vault value named CC_DIR or AMUX_URL would silently re-route the worker.
pub(crate) fn valid_key(k: &str) -> Result<(), String> {
    let ok = !k.is_empty()
        && k.len() <= 128
        && k.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err("key must be an environment variable name: A-Z, 0-9 and _, not starting with a digit".into());
    }
    if k.starts_with("CC_") || k.starts_with("AMUX_") || ["PATH", "HOME", "SHELL", "USER", "TMPDIR"].contains(&k) {
        return Err(format!("{k} is a harness or shell variable; a vault value there would change how the worker runs, not what it can reach"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Master key
// ---------------------------------------------------------------------------

fn keystore_is_file() -> bool {
    match std::env::var("AMUX_VAULT_KEYSTORE").ok().as_deref().map(str::trim) {
        Some("file") => true,
        Some("keychain") => false,
        _ => !cfg!(target_os = "macos"),
    }
}

/// One Keychain account per home, so a test home never shares the real key.
fn keychain_account(home: &Path) -> String {
    let canon = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let h = Sha256::digest(canon.to_string_lossy().as_bytes());
    format!("home-{}", hex::encode(&h[..8]))
}

fn key_file(home: &Path) -> PathBuf {
    super::vault::vault_dir(home).join("master.key")
}

/// Short fingerprint of a key, stored on each item so a replaced Keychain key
/// is reported as "the key changed", not as a generic decryption failure.
fn fingerprint(key: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(key)[..4])
}

fn parse_key_hex(s: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(s.trim()).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

fn read_keychain(home: &Path) -> Result<Option<[u8; 32]>, String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", &keychain_account(home), "-w"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run security: {e}"))?;
    if out.status.code() == Some(44) {
        return Ok(None); // errSecItemNotFound
    }
    if !out.status.success() {
        return Err(format!(
            "security find-generic-password exited {:?}: {} (is the login keychain locked?)",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_key_hex(&String::from_utf8_lossy(&out.stdout))
        .map(Some)
        .ok_or_else(|| "the Keychain item is not a 32-byte hex key".to_string())
}

fn write_keychain(home: &Path, key: &[u8; 32]) -> Result<(), String> {
    use std::io::Write;
    // `security -i` reads commands from stdin: the key never becomes an argv
    // element of any process.
    let mut child = std::process::Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run security: {e}"))?;
    let line = format!(
        "add-generic-password -U -s {KEYCHAIN_SERVICE} -a {} -l \"amux vault master key\" -w {}\n",
        keychain_account(home),
        hex::encode(key)
    );
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("security add-generic-password failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn read_key_file(home: &Path) -> Result<Option<[u8; 32]>, String> {
    match std::fs::read_to_string(key_file(home)) {
        Ok(s) => parse_key_hex(&s).map(Some).ok_or_else(|| "master.key is not a 32-byte hex key".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read master.key: {e}")),
    }
}

fn write_key_file(home: &Path, key: &[u8; 32]) -> Result<(), String> {
    let dir = super::vault::vault_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".master.key.{}", std::process::id()));
    std::fs::write(&tmp, hex::encode(key)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, key_file(home)).map_err(|e| e.to_string())
}

fn key_cache() -> &'static std::sync::Mutex<BTreeMap<PathBuf, [u8; 32]>> {
    static C: std::sync::OnceLock<std::sync::Mutex<BTreeMap<PathBuf, [u8; 32]>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// The master key for this home. `create` mints one when none exists; only a
/// write asks for that, so reading an empty vault never touches the Keychain.
pub(crate) fn master_key(home: &Path, create: bool) -> Result<Option<[u8; 32]>, String> {
    if let Some(k) = key_cache().lock().ok().and_then(|c| c.get(home).copied()) {
        return Ok(Some(k));
    }
    let file = keystore_is_file();
    let found = if file { read_key_file(home)? } else { read_keychain(home)? };
    let key = match found {
        Some(k) => k,
        None if create => {
            let mut k = [0u8; 32];
            OsRng.fill_bytes(&mut k);
            if file { write_key_file(home, &k)? } else { write_keychain(home, &k)? }
            // Read back: a write the store silently dropped must not become
            // a vault whose values nothing can open.
            let back = if file { read_key_file(home)? } else { read_keychain(home)? };
            if back != Some(k) {
                return Err("the new master key did not read back from the keystore".into());
            }
            tracing::info!(backend = if file { "file" } else { "keychain" }, measured = true,
                n_considered = 1, verdict = "vault_key_created", "vault: master key created (AMUX-5375)");
            k
        }
        None => return Ok(None),
    };
    if file {
        tracing::warn!(path = %key_file(home).display(), measured = true, n_considered = 1,
            verdict = "vault_key_file_backend",
            "vault: master key is a 0600 file beside the store (no Keychain); a copy of ~/.amux carries both (AMUX-5375)");
    }
    if let Ok(mut c) = key_cache().lock() {
        c.insert(home.to_path_buf(), key);
    }
    Ok(Some(key))
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

fn store_path(home: &Path) -> PathBuf {
    super::vault::vault_dir(home).join("secrets.json")
}

fn load(home: &Path) -> Result<Vec<Value>, String> {
    match std::fs::read_to_string(store_path(home)) {
        Ok(s) => {
            let v: Value = serde_json::from_str(&s).map_err(|e| format!("secrets.json is not valid JSON: {e}"))?;
            Ok(v.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("could not read secrets.json: {e}")),
    }
}

fn save(home: &Path, items: &[Value]) -> Result<(), String> {
    let dir = super::vault::vault_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".secrets.json.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(&json!({"version": STORE_VERSION, "items": items})).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, store_path(home)).map_err(|e| e.to_string())
}

fn item_scope(item: &Value) -> Option<Scope> {
    Scope::parse(item["scope"].as_str()?, item["target"].as_str().unwrap_or("")).ok()
}

fn aad(id: &str, key: &str, scope: &Scope) -> String {
    format!("amux-vault-v1|{id}|{key}|{}|{}", scope.kind(), scope.target())
}

fn seal(master: &[u8; 32], id: &str, key: &str, scope: &Scope, value: &str) -> Result<(String, String), String> {
    let cipher = Aes256Gcm::new_from_slice(master).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let a = aad(id, key, scope);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: value.as_bytes(), aad: a.as_bytes() })
        .map_err(|_| "encryption failed".to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok((b64.encode(nonce), b64.encode(ct)))
}

fn open(master: &[u8; 32], item: &Value) -> Result<String, String> {
    let scope = item_scope(item).ok_or("item has no valid scope")?;
    let id = item["id"].as_str().unwrap_or("");
    let key = item["key"].as_str().unwrap_or("");
    if item["key_fp"].as_str().is_some_and(|fp| fp != fingerprint(master)) {
        return Err("sealed under a different master key (the Keychain key was replaced)".into());
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let nonce = b64.decode(item["nonce"].as_str().unwrap_or("")).map_err(|_| "bad nonce")?;
    let ct = b64.decode(item["ct"].as_str().unwrap_or("")).map_err(|_| "bad ciphertext")?;
    if nonce.len() != 12 {
        return Err("bad nonce length".into());
    }
    let cipher = Aes256Gcm::new_from_slice(master).map_err(|e| e.to_string())?;
    let a = aad(id, key, &scope);
    let pt = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad: a.as_bytes() })
        .map_err(|_| "ciphertext does not open for this item (tampered, or moved between items)".to_string())?;
    String::from_utf8(pt).map_err(|_| "value is not UTF-8".into())
}

fn audit(home: &Path, entry: Value) {
    super::vault::audit_line(home, entry);
}

/// Metadata only. There is deliberately no field that could carry a value.
fn public_view(item: &Value) -> Value {
    json!({
        "id": item["id"], "key": item["key"], "scope": item["scope"], "target": item["target"],
        "created": item["created"], "updated": item["updated"], "by": item["by"], "note": item["note"],
        "source": item["source"],
    })
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

fn worker_groups(home: &Path, worker: &str) -> std::collections::BTreeSet<String> {
    super::session_verbs::EnvFile::load(&home.join("sessions").join(format!("{worker}.env")))
        .get("CC_TAGS")
        .map(|v| {
            v.split(',')
                .map(|t| t.trim().trim_matches('"').to_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Does this item apply to `worker`?
fn applies(scope: &Scope, worker: &str, groups: &std::collections::BTreeSet<String>) -> bool {
    match scope {
        Scope::Global => true,
        Scope::Group(g) => groups.contains(g),
        Scope::Worker(w) => w == worker,
    }
}

/// Per key, the winning item for a worker plus the ones it shadows. Same
/// precedence as `scope_env_layers`: worker > group > global, and among the
/// worker's groups the one that sorts LAST wins, because that is the file
/// sourced last.
pub(crate) fn resolve<'a>(items: &'a [Value], home: &Path, worker: &str) -> BTreeMap<String, (&'a Value, Vec<&'a Value>)> {
    let groups = worker_groups(home, worker);
    let mut by_key: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for it in items {
        let Some(scope) = item_scope(it) else { continue };
        if applies(&scope, worker, &groups) {
            by_key.entry(it["key"].as_str().unwrap_or("").to_string()).or_default().push(it);
        }
    }
    by_key
        .into_iter()
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, mut v)| {
            v.sort_by(|a, b| {
                let (sa, sb) = (item_scope(a).unwrap(), item_scope(b).unwrap());
                sa.rank().cmp(&sb.rank()).then_with(|| sa.target().cmp(sb.target()))
            });
            let win = v.pop().expect("non-empty");
            (k, (win, v))
        })
        .collect()
}

/// The values a worker launches with, and the audit of that delivery. Keys
/// only in the audit; `Err` is logged by the caller and the launch goes on.
pub(crate) fn launch_values(home: &Path, worker: &str) -> Result<Vec<(String, String)>, String> {
    let items = load(home)?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let resolved = resolve(&items, home, worker);
    if resolved.is_empty() {
        return Ok(Vec::new());
    }
    let master = master_key(home, false)?.ok_or("the vault has items but no master key (Keychain item missing)")?;
    let mut out = Vec::new();
    let mut delivered = Vec::new();
    let mut failed = Vec::new();
    for (key, (win, shadowed)) in &resolved {
        match open(&master, win) {
            Ok(v) => {
                out.push((key.clone(), v));
                delivered.push(json!({"key": key, "scope": win["scope"], "target": win["target"],
                    "shadowed": shadowed.iter().map(|s| json!({"scope": s["scope"], "target": s["target"]})).collect::<Vec<_>>()}));
            }
            Err(e) => {
                tracing::warn!(session = worker, key = %key, item = %win["id"], error = %e, measured = true,
                    n_considered = 1, verdict = "vault_decrypt_failed",
                    "vault: an item did not open, so the worker launches without it (AMUX-5375)");
                failed.push(json!({"key": key, "item": win["id"], "error": e}));
            }
        }
    }
    audit(home, json!({"ts": now_f64(), "event": "secrets_delivered", "session": worker,
        "delivered": delivered, "failed": failed}));
    tracing::info!(session = worker, delivered = out.len(), failed = failed.len(), measured = true,
        n_considered = resolved.len(), verdict = "vault_delivered",
        "vault: secrets resolved for launch (keys only in vault-audit.jsonl) (AMUX-5375)");
    Ok(out)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn worker_origin(headers: &HeaderMap) -> Option<String> {
    ["x-amux-session", "x-amux-worker"].iter().find_map(|h| {
        headers
            .get(*h)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn err(code: StatusCode, v: Value) -> Response {
    (code, Json(v)).into_response()
}

fn refuse_worker(home: &Path, who: &str, action: &str) -> Response {
    tracing::warn!(session = who, action, measured = true, n_considered = 1,
        verdict = "vault_write_refused_worker_origin",
        "vault: a worker origin tried to change a secret; only the owner can (AMUX-5375)");
    audit(home, json!({"ts": now_f64(), "event": "secret_write_refused", "action": action, "by": who}));
    err(StatusCode::FORBIDDEN, json!({"error": "only the owner can add, change or delete vault secrets",
        "why": "a worker that could write a secret could re-point another lane's credentials"}))
}

fn qs(q: &Option<String>, key: &str) -> Option<String> {
    crate::api::fs::parse_qs(q.as_deref().unwrap_or(""))
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

async fn list(headers: HeaderMap, RawQuery(q): RawQuery) -> Response {
    let home = amux_home();
    let items = match load(&home) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, measured = false, n_considered = 0, verdict = "vault_store_unreadable",
                "vault: secrets.json could not be read (AMUX-5375)");
            return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"measured": false, "n_considered": 0, "why_unmeasured": e}));
        }
    };
    // A worker sees the metadata of what applies to it, nothing else.
    let origin = worker_origin(&headers);
    let viewer = origin.clone().or_else(|| qs(&q, "worker"));
    let visible: Vec<&Value> = match &origin {
        Some(w) => {
            let groups = worker_groups(&home, w);
            items.iter().filter(|i| item_scope(i).is_some_and(|s| applies(&s, w, &groups))).collect()
        }
        None => items.iter().collect(),
    };
    let mut body = json!({
        "measured": true,
        "n_considered": items.len(),
        "items": visible.iter().map(|i| public_view(i)).collect::<Vec<_>>(),
        "keystore": if keystore_is_file() { "file" } else { "keychain" },
    });
    if let Some(w) = viewer.filter(|w| !w.is_empty()) {
        let res = resolve(&items, &home, &w);
        body["resolved_for"] = json!(w);
        body["resolved"] = json!(res.iter().map(|(k, (win, sh))| json!({
            "key": k, "id": win["id"], "scope": win["scope"], "target": win["target"],
            "shadows": sh.iter().map(|s| json!({"id": s["id"], "scope": s["scope"], "target": s["target"]})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>());
    }
    Json(body).into_response()
}

fn body_value(body: &Value) -> Result<String, String> {
    let v = body.get("value").and_then(Value::as_str).ok_or("value is required")?;
    if v.is_empty() {
        return Err("value is empty".into());
    }
    if v.len() > MAX_VALUE_BYTES {
        return Err(format!("value is over {MAX_VALUE_BYTES} bytes"));
    }
    if v.contains('\0') {
        return Err("value contains a NUL byte, which no environment variable can carry".into());
    }
    Ok(v.to_string())
}

fn new_id() -> String {
    let mut b = [0u8; 6];
    OsRng.fill_bytes(&mut b);
    format!("sec_{}", hex::encode(b))
}

fn create_item(home: &Path, key: &str, scope: &Scope, value: &str, by: &str, note: &str, source: Option<&str>) -> Result<Value, (StatusCode, Value)> {
    let mut items = load(home).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})))?;
    if let Some(dup) = items.iter().find(|i| i["key"] == key && item_scope(i).as_ref() == Some(scope)) {
        return Err((StatusCode::CONFLICT, json!({"error": "this key already has a value at this scope; replace it instead",
            "id": dup["id"]})));
    }
    let master = master_key(home, true)
        .map_err(|e| {
            tracing::warn!(error = %e, measured = true, n_considered = 1, verdict = "vault_key_unavailable",
                "vault: no master key, so nothing can be sealed (AMUX-5375)");
            (StatusCode::SERVICE_UNAVAILABLE, json!({"error": format!("vault key unavailable: {e}")}))
        })?
        .expect("create=true returns a key or an error");
    let id = new_id();
    let (nonce, ct) = seal(&master, &id, key, scope, value).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})))?;
    let now = now_f64();
    let item = json!({"id": id, "key": key, "scope": scope.kind(), "target": scope.target(),
        "nonce": nonce, "ct": ct, "key_fp": fingerprint(&master), "created": now, "updated": now,
        "by": by, "note": note, "source": source});
    items.push(item.clone());
    save(home, &items).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})))?;
    Ok(item)
}

async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let home = amux_home();
    if let Some(w) = worker_origin(&headers) {
        return refuse_worker(&home, &w, "create");
    }
    let key = body.get("key").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if let Err(e) = valid_key(&key) {
        return err(StatusCode::BAD_REQUEST, json!({"error": e}));
    }
    let scope = match Scope::parse(body["scope"].as_str().unwrap_or("global"), body["target"].as_str().unwrap_or("")) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({"error": e})),
    };
    if let Scope::Worker(w) = &scope {
        if !home.join("sessions").join(format!("{w}.env")).exists() {
            return err(StatusCode::NOT_FOUND, json!({"error": format!("no worker named {w}")}));
        }
    }
    let value = match body_value(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({"error": e})),
    };
    let note = body.get("note").and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>();
    match create_item(&home, &key, &scope, &value, "owner", &note, None) {
        Ok(item) => {
            audit(&home, json!({"ts": now_f64(), "event": "secret_added", "item": item["id"], "key": key,
                "scope": scope.kind(), "target": scope.target(), "by": "owner"}));
            (StatusCode::CREATED, Json(json!({"ok": true, "item": public_view(&item)}))).into_response()
        }
        Err((code, v)) => err(code, v),
    }
}

/// Replace a value. The key and scope are the item's identity (and part of its
/// sealed associated data); moving one is delete plus add.
async fn replace(headers: HeaderMap, AxPath(id): AxPath<String>, Json(body): Json<Value>) -> Response {
    let home = amux_home();
    if let Some(w) = worker_origin(&headers) {
        return refuse_worker(&home, &w, "replace");
    }
    let value = match body_value(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({"error": e})),
    };
    let mut items = match load(&home) {
        Ok(i) => i,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})),
    };
    let Some(pos) = items.iter().position(|i| i["id"] == id.as_str()) else {
        return err(StatusCode::NOT_FOUND, json!({"error": "no such secret"}));
    };
    let master = match master_key(&home, true) {
        Ok(Some(k)) => k,
        Ok(None) | Err(_) => {
            tracing::warn!(measured = true, n_considered = 1, verdict = "vault_key_unavailable",
                "vault: no master key, so nothing can be sealed (AMUX-5375)");
            return err(StatusCode::SERVICE_UNAVAILABLE, json!({"error": "vault key unavailable"}));
        }
    };
    let it = &items[pos];
    let (key, scope) = (it["key"].as_str().unwrap_or("").to_string(), item_scope(it).unwrap_or(Scope::Global));
    let (nonce, ct) = match seal(&master, &id, &key, &scope, &value) {
        Ok(x) => x,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})),
    };
    let it = &mut items[pos];
    it["nonce"] = json!(nonce);
    it["ct"] = json!(ct);
    it["key_fp"] = json!(fingerprint(&master));
    it["updated"] = json!(now_f64());
    it["by"] = json!("owner");
    if let Some(n) = body.get("note").and_then(Value::as_str) {
        it["note"] = json!(n.chars().take(200).collect::<String>());
    }
    let view = public_view(it);
    if let Err(e) = save(&home, &items) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e}));
    }
    audit(&home, json!({"ts": now_f64(), "event": "secret_replaced", "item": id, "key": key,
        "scope": scope.kind(), "target": scope.target(), "by": "owner"}));
    Json(json!({"ok": true, "item": view})).into_response()
}

async fn remove(headers: HeaderMap, AxPath(id): AxPath<String>) -> Response {
    let home = amux_home();
    if let Some(w) = worker_origin(&headers) {
        return refuse_worker(&home, &w, "delete");
    }
    let mut items = match load(&home) {
        Ok(i) => i,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})),
    };
    let Some(pos) = items.iter().position(|i| i["id"] == id.as_str()) else {
        return err(StatusCode::NOT_FOUND, json!({"error": "no such secret"}));
    };
    let gone = items.remove(pos);
    if let Err(e) = save(&home, &items) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e}));
    }
    audit(&home, json!({"ts": now_f64(), "event": "secret_deleted", "item": id, "key": gone["key"],
        "scope": gone["scope"], "target": gone["target"], "by": "owner"}));
    Json(json!({"ok": true, "deleted": id})).into_response()
}

/// The plaintext file a scope's key lives in today, for an opt-in import.
fn env_source(home: &Path, scope: &Scope, source: Option<&str>) -> PathBuf {
    match scope {
        Scope::Global if source == Some("server.env") => home.join("server.env"),
        Scope::Global => home.join("amux.env"),
        Scope::Group(g) => home.join("env").join(format!("{g}.env")),
        Scope::Worker(w) => home.join("sessions").join(format!("{w}.env")),
    }
}

/// Opt-in, one key at a time: copy a value that lives in a plaintext env file
/// into the vault at the matching scope. The file is not edited: removing the
/// plaintext line is the owner's step, once the worker has relaunched with the
/// vault value (which wins over the file for that key).
async fn import(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let home = amux_home();
    if let Some(w) = worker_origin(&headers) {
        return refuse_worker(&home, &w, "import");
    }
    let key = body.get("key").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if let Err(e) = valid_key(&key) {
        return err(StatusCode::BAD_REQUEST, json!({"error": e}));
    }
    let scope = match Scope::parse(body["scope"].as_str().unwrap_or("global"), body["target"].as_str().unwrap_or("")) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({"error": e})),
    };
    let src = env_source(&home, &scope, body.get("source").and_then(Value::as_str));
    let env = super::session_verbs::EnvFile::load(&src);
    let Some(value) = env.get(&key).map(str::to_string).filter(|v| !v.is_empty()) else {
        return err(StatusCode::NOT_FOUND, json!({"error": format!("{key} is not set in {}", src.display())}));
    };
    let src_s = src.to_string_lossy().into_owned();
    match create_item(&home, &key, &scope, &value, "owner", "", Some(&src_s)) {
        Ok(item) => {
            audit(&home, json!({"ts": now_f64(), "event": "secret_imported", "item": item["id"], "key": key,
                "scope": scope.kind(), "target": scope.target(), "from": src_s, "by": "owner"}));
            (StatusCode::CREATED, Json(json!({"ok": true, "item": public_view(&item),
                "still_in": src_s,
                "next": "the plaintext line is untouched; the vault value wins for this key from the worker's next launch. Remove the line yourself when you are ready."}))).into_response()
        }
        Err((code, v)) => err(code, v),
    }
}

pub fn routes() -> Router<super::AppState> {
    Router::new()
        .route("/api/vault/secrets", get(list).post(create))
        .route("/api/vault/secrets/import", post(import))
        .route("/api/vault/secrets/{id}", axum::routing::put(replace).delete(remove))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sessions")).unwrap();
        d
    }

    fn put(items: &mut Vec<Value>, master: &[u8; 32], key: &str, scope: Scope, value: &str) {
        let id = format!("sec_{}", items.len());
        let (nonce, ct) = seal(master, &id, key, &scope, value).unwrap();
        items.push(json!({"id": id, "key": key, "scope": scope.kind(), "target": scope.target(),
            "nonce": nonce, "ct": ct, "key_fp": fingerprint(master)}));
    }

    #[test]
    fn keys_are_env_names_and_harness_variables_are_refused() {
        assert!(valid_key("GITHUB_TOKEN").is_ok());
        assert!(valid_key("_X1").is_ok());
        for bad in ["", "1X", "lower", "A-B", "A B", "CC_DIR", "AMUX_URL", "PATH", "HOME"] {
            assert!(valid_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_value_opens_only_on_its_own_item() {
        let master = [7u8; 32];
        let mut items = Vec::new();
        put(&mut items, &master, "TOKEN", Scope::Worker("w1".into()), "s3cret");
        assert_eq!(open(&master, &items[0]).unwrap(), "s3cret");
        // The ciphertext is not the value.
        assert!(!items[0].to_string().contains("s3cret"));
        // Moving it to another scope must fail, not deliver it there.
        let mut moved = items[0].clone();
        moved["scope"] = json!("global");
        moved["target"] = json!("");
        assert!(open(&master, &moved).is_err());
        // A different master key is reported as a key change.
        let err = open(&[8u8; 32], &items[0]).unwrap_err();
        assert!(err.contains("different master key"), "{err}");
    }

    #[test]
    fn worker_beats_group_beats_global_and_the_last_group_wins() {
        let h = home();
        std::fs::write(h.path().join("sessions/w1.env"), "CC_TAGS=\"alpha,beta\"\n").unwrap();
        let master = [1u8; 32];
        let mut items = Vec::new();
        put(&mut items, &master, "A", Scope::Global, "g");
        put(&mut items, &master, "A", Scope::Group("alpha".into()), "alpha");
        put(&mut items, &master, "A", Scope::Group("beta".into()), "beta");
        put(&mut items, &master, "B", Scope::Global, "g");
        put(&mut items, &master, "B", Scope::Worker("w1".into()), "mine");
        put(&mut items, &master, "C", Scope::Group("gamma".into()), "not mine");
        put(&mut items, &master, "D", Scope::Worker("w2".into()), "not mine");
        let r = resolve(&items, h.path(), "w1");
        let win = |k: &str| open(&master, r[k].0).unwrap();
        assert_eq!(win("A"), "beta", "beta sorts after alpha, as its file is sourced later");
        assert_eq!(r["A"].1.len(), 2, "global and alpha are shadowed");
        assert_eq!(win("B"), "mine");
        assert!(!r.contains_key("C") && !r.contains_key("D"), "other groups and workers never apply");
    }

    #[test]
    fn launch_delivers_resolved_values_and_audits_keys_only() {
        let h = home();
        std::env::set_var("AMUX_VAULT_KEYSTORE", "file");
        std::fs::write(h.path().join("sessions/w1.env"), "CC_TAGS=\"ops\"\n").unwrap();
        let s = Scope::Group("ops".into());
        create_item(h.path(), "API_TOKEN", &s, "tok-123", "owner", "", None).unwrap();
        create_item(h.path(), "API_TOKEN", &Scope::Global, "tok-global", "owner", "", None).unwrap();
        let dup = create_item(h.path(), "API_TOKEN", &Scope::Global, "again", "owner", "", None);
        assert_eq!(dup.unwrap_err().0, StatusCode::CONFLICT);
        let store = std::fs::read_to_string(store_path(h.path())).unwrap();
        assert!(!store.contains("tok-123") && !store.contains("tok-global"), "no plaintext at rest");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [store_path(h.path()), key_file(h.path())] {
                assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600, "{}", f.display());
            }
        }
        let got = launch_values(h.path(), "w1").unwrap();
        assert_eq!(got, vec![("API_TOKEN".to_string(), "tok-123".to_string())]);
        assert_eq!(launch_values(h.path(), "stranger").unwrap(), vec![("API_TOKEN".to_string(), "tok-global".to_string())]);
        let audit = std::fs::read_to_string(h.path().join("logs/vault-audit.jsonl")).unwrap();
        assert!(audit.contains("secrets_delivered") && audit.contains("API_TOKEN"));
        assert!(!audit.contains("tok-123") && !audit.contains("tok-global"), "no value in the audit");
    }
}
