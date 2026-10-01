//! DESKT-68: amux must never run the `ollama` binary.
//!
//! On macOS the `ollama` CLI starts Ollama.app whenever its daemon is not
//! running (it calls `open -j -a Ollama --args --fast-startup`). amux used to
//! shell out to it for `ollama list` (the dashboard's model pickers) and
//! `ollama show` (an ollama worker's start), so the app came back every time
//! the owner opened Settings, after he had quit it. Every amux probe now goes
//! through `provider::static_providers::ollama_http`, which cannot launch
//! anything. This test fails the build if a CLI call is reintroduced.
//!
//! It scans the server's own sources for a process spawn whose program is the
//! literal "ollama". A launch command that tells codex to use ollama
//! (`codex --oss --local-provider ollama`) passes "ollama" as an ARGUMENT, not
//! the program, and is not matched.

use std::path::Path;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable source dir") {
        let p = entry.expect("dir entry").path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// True if `src` spawns the ollama binary: `Command::new("ollama")` in any of
/// its spellings (std, tokio, a `use`d `Command`), with any whitespace.
fn spawns_ollama(src: &str) -> bool {
    let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("Command::new(\"ollama\")")
}

#[test]
fn the_detector_sees_every_spelling_it_must() {
    assert!(spawns_ollama(r#"tokio::process::Command::new("ollama").arg("list")"#));
    assert!(spawns_ollama("std::process::Command::new(\n    \"ollama\"\n)"));
    assert!(spawns_ollama(r#"Command::new( "ollama" )"#));
    assert!(!spawns_ollama(r#"vec!["codex".into(), "--local-provider".into(), "ollama".into()]"#));
    assert!(!spawns_ollama(r#"Command::new("codex").arg("ollama")"#));
}

#[test]
fn no_server_source_runs_the_ollama_binary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(files.len() > 50, "scanned {} files; the walk is broken", files.len());
    let offenders: Vec<String> = files
        .iter()
        .filter(|p| spawns_ollama(&std::fs::read_to_string(p).unwrap_or_default()))
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "these files run the `ollama` CLI, which launches Ollama.app on macOS when its daemon is down \
         (DESKT-68); use provider::static_providers::ollama_http instead: {offenders:?}"
    );
}
