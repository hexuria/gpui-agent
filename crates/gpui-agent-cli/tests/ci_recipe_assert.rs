//! Claim tests for P4: receipt gate requires ok + session_reused; token stays
//! off the cargo-test workflow step so unset-token unit tests do not flake.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assert_py() -> PathBuf {
    repo_root().join("scripts/ci_recipe_assert.py")
}

fn ci_recipe_sh() -> PathBuf {
    repo_root().join("scripts/ci-recipe.sh")
}

fn run_assert(json: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "gpui-agent-ci-assert-{}-{:x}",
        std::process::id(),
        {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in json.as_bytes() {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        }
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("receipt.json");
    fs::write(&path, json).unwrap();
    let output = Command::new("python3")
        .arg(assert_py())
        .arg(&path)
        .output()
        .expect("python3 ci_recipe_assert.py");
    let _ = fs::remove_dir_all(&dir);
    output
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn receipt_ok_and_session_reused_passes() {
    let output = run_assert(r#"{"ok": true, "session_reused": true}"#);
    assert!(output.status.success(), "{}", stderr_of(&output));
}

#[test]
fn receipt_without_ok_fails() {
    let output = run_assert(r#"{"session_reused": true}"#);
    assert!(!output.status.success());
    let err = stderr_of(&output);
    assert!(err.contains("ok="), "{err}");
}

#[test]
fn receipt_ok_false_fails() {
    let output = run_assert(r#"{"ok": false, "session_reused": true}"#);
    assert!(!output.status.success());
    let err = stderr_of(&output);
    assert!(err.contains("ok="), "{err}");
}

#[test]
fn receipt_without_session_reused_fails() {
    let output = run_assert(r#"{"ok": true}"#);
    assert!(
        !output.status.success(),
        "missing session_reused must not be treated as skip"
    );
    let err = stderr_of(&output);
    assert!(err.contains("session_reused="), "{err}");
}

#[test]
fn receipt_session_reused_false_fails() {
    let output = run_assert(r#"{"ok": true, "session_reused": false}"#);
    assert!(!output.status.success());
    let err = stderr_of(&output);
    assert!(err.contains("session_reused="), "{err}");
}

#[test]
fn ci_recipe_sh_fails_without_token_before_build() {
    let output = Command::new("bash")
        .arg(ci_recipe_sh())
        .env_remove("GPUI_AGENT_TOKEN")
        .env("GPUI_AGENT_TOKEN", "")
        .output()
        .expect("ci-recipe.sh");
    assert!(
        !output.status.success(),
        "empty token must fail closed\n{}",
        stderr_of(&output)
    );
    let err = stderr_of(&output);
    assert!(
        err.contains("GPUI_AGENT_TOKEN") || err.contains("token"),
        "{err}"
    );
    assert!(
        !err.contains("building CLI"),
        "must fail before cargo build: {err}"
    );
}

#[test]
fn ci_workflow_token_is_only_on_the_recipe_step() {
    let yml = include_str!("../../../.github/workflows/ci.yml");
    assert!(
        yml.contains("ubuntu-latest"),
        "P4 is visual-free ubuntu CI, not macOS pixels"
    );
    assert!(
        !yml.to_ascii_lowercase().contains("macos-"),
        "must not add a paid macOS runner: {yml}"
    );
    let unit_idx = yml.find("name: Unit tests").expect("unit tests step");
    let recipe_idx = yml
        .find("name: Headless recipe receipt")
        .expect("recipe step");
    assert!(unit_idx < recipe_idx);
    let unit_block = &yml[unit_idx..recipe_idx];
    let recipe_block = &yml[recipe_idx..];
    assert!(
        !unit_block.contains("GPUI_AGENT_TOKEN"),
        "token on cargo test would flake tests that unset it: {unit_block}"
    );
    assert!(
        recipe_block.contains("GPUI_AGENT_TOKEN"),
        "recipe step must set the test token"
    );
    assert!(recipe_block.contains("ci-p4-token"));
}
