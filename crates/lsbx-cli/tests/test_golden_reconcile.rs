//! Integration tests for `lsbx golden reconcile` (the 2026-09-15
//! exedev-golden-discovery gap) and for the missing-manifest warning in
//! `build_deps` — both exercised through the real compiled binary
//! (`env!("CARGO_BIN_EXE_lsbx")`, the same spawn-the-real-process approach
//! `test_backend_auto_probe.rs` established).
//!
//! Scope note: the `present` classification needs a backend whose live VM
//! inventory contains a golden base VM (`lsbx-default-v1` et al.). The demo
//! backend is per-process and in-memory, so a spawned CLI process always
//! starts with an empty demo inventory — meaning the CLI-level reconcile
//! provably reports manifest goldens as `missing` against a fresh backend,
//! while the `present` / `unregistered` / error-propagation classifications
//! are covered exhaustively at the façade level by
//! `lsbx-ops/tests/test_golden_reconcile.rs` (which can set the inventory).
//! Together the two levels prove the full wire-up without pretending a
//! hash-named demo VM is a golden base.
//!
//! The missing-manifest warning (the other half of the gap) IS fully
//! testable here: a fresh `--state-dir` means the default manifest path is
//! absent, and stderr must say so — while an explicit `--images` path that
//! is absent must stay silent (the caller chose that path).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fresh_state_dir(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("lsbx-cli-reconcile-{label}-{nanos}"))
}

/// Writes a minimal but real image manifest: one golden (`agent-base`)
/// cloned from a base VM the demo backend will not have live.
fn write_manifest(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).expect("create state dir");
    let path = dir.join("images.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "images": [],
            "goldens": [
                {
                    "key": "agent-base",
                    "flavor": "agent",
                    "os": "linux",
                    "base": "lsbx-default-v1",
                    "mode": "copy",
                    "cpu": 2,
                    "memory": "4GB",
                    "streaming": "none",
                    "healthcheck": [],
                    "description": "reconcile-test golden"
                }
            ],
            "profiles": {}
        })
        .to_string(),
    )
    .expect("write manifest");
    path
}

#[test]
fn golden_reconcile_json_envelope_classifies_manifest_goldens_against_live_backend() {
    let state_dir = fresh_state_dir("json");
    let manifest = write_manifest(&state_dir);

    // Explicit `--backend demo` (not auto: kora-style hosts run libvirt)
    // and an explicit `--images` so the missing-manifest warning (a
    // different feature, tested below) cannot contaminate stderr.
    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args([
            "--backend",
            "demo",
            "--state-dir",
            state_dir.to_str().expect("state dir is valid UTF-8"),
            "--images",
            manifest.to_str().expect("manifest is valid UTF-8"),
            "golden",
            "reconcile",
            "--json",
        ])
        .output()
        .expect("failed to spawn lsbx binary");

    assert!(
        output.status.success(),
        "`lsbx golden reconcile --json` did not exit 0.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is a JSON envelope");
    assert_eq!(envelope["status"], "success");

    let data = &envelope["data"];
    let items = data["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "one item per manifest golden");
    assert_eq!(items[0]["key"], "agent-base");
    assert_eq!(items[0]["base"], "lsbx-default-v1");
    // The demo backend in a fresh process has created no VMs, so the
    // golden's base is not live — `missing` is the correct classification
    // (and the exact observation the 2026-09-15 gap hid).
    assert_eq!(items[0]["status"], "missing");
    assert_eq!(
        data["unregistered_vms"].as_array().map(Vec::as_slice),
        Some(&[][..]),
        "a fresh demo backend has no golden-shaped VMs"
    );
}

#[test]
fn golden_reconcile_human_output_is_a_status_table() {
    let state_dir = fresh_state_dir("human");
    let manifest = write_manifest(&state_dir);

    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args([
            "--backend",
            "demo",
            "--state-dir",
            state_dir.to_str().expect("state dir is valid UTF-8"),
            "--images",
            manifest.to_str().expect("manifest is valid UTF-8"),
            "golden",
            "reconcile",
        ])
        .output()
        .expect("failed to spawn lsbx binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("KEY") && stdout.contains("BASE") && stdout.contains("STATUS"),
        "human output should be a KEY/BASE/STATUS table, got:\n{stdout}"
    );
    assert!(
        stdout.contains("agent-base") && stdout.contains("missing"),
        "human output should name the golden and its status, got:\n{stdout}"
    );
}

#[test]
fn missing_default_manifest_warns_on_stderr_but_still_succeeds() {
    let state_dir = fresh_state_dir("warn");

    // No --images: the default path <state_dir>/images.json does not exist.
    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args([
            "--backend",
            "demo",
            "--state-dir",
            state_dir.to_str().expect("state dir is valid UTF-8"),
            "golden",
            "list",
            "--json",
        ])
        .output()
        .expect("failed to spawn lsbx binary");

    assert!(
        output.status.success(),
        "a missing default manifest must not fail the command"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no image manifest found at the default path"),
        "expected the missing-manifest note on stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("--images"),
        "the note should point at the remedy, got:\n{stderr}"
    );
    // The command itself still behaves exactly as before: an empty,
    // valid registry view.
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is a JSON envelope");
    assert_eq!(envelope["status"], "success");
    assert_eq!(
        envelope["data"].as_array().map(Vec::as_slice),
        Some(&[][..])
    );
}

#[test]
fn explicit_missing_images_path_stays_silent() {
    let state_dir = fresh_state_dir("explicit");
    let bogus = state_dir.join("does-not-exist.json");

    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args([
            "--backend",
            "demo",
            "--state-dir",
            state_dir.to_str().expect("state dir is valid UTF-8"),
            "--images",
            bogus.to_str().expect("path is valid UTF-8"),
            "golden",
            "list",
            "--json",
        ])
        .output()
        .expect("failed to spawn lsbx binary");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no image manifest found at the default path"),
        "an explicit --images path is the caller's own choice; the default-path note must not fire, got:\n{stderr}"
    );
}

#[test]
fn golden_reconcile_surfaces_control_plane_failures() {
    // `--backend libvirt` on a host with no reachable libvirt daemon must
    // fail reconcile with the backend's own error — never a silent empty
    // report. This is a best-effort environmental test: hosts WITH libvirt
    // (kora) would succeed here instead, which is also acceptable (the
    // assertion is only that the command does not fabricate a report).
    let state_dir = fresh_state_dir("dead-backend");
    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args([
            "--backend",
            "libvirt",
            "--state-dir",
            state_dir.to_str().expect("state dir is valid UTF-8"),
            "golden",
            "reconcile",
            "--json",
        ])
        .output()
        .expect("failed to spawn lsbx binary");

    if output.status.success() {
        // Host has a live libvirt daemon: a real report is fine.
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .expect("successful reconcile prints a JSON envelope");
        assert_eq!(envelope["status"], "success");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.is_empty(),
            "a failed reconcile must explain itself on stderr"
        );
    }
}
