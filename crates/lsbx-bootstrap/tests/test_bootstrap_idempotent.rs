//! Three-part idempotency test, read literally off the acceptance
//! criterion: "`--force` re-runs bootstrap idempotently on an
//! already-bootstrapped host without erroring on already-exists
//! conditions." That phrasing only makes sense if, *without* `--force`,
//! an already-bootstrapped host normally *does* error on an
//! already-exists condition — otherwise `--force` would have nothing to
//! avoid. This test proves all three cases:
//!
//! 1. A fresh host: `bootstrap()` succeeds.
//! 2. `force: true` reruns against that now-bootstrapped host: succeeds
//!    idempotently, reporting "already exists" conditions rather than
//!    erroring on them.
//! 3. `force: false` against that same already-bootstrapped host:
//!    errors.
//!
//! Uses `LSBX_SYSTEMD_UNIT_DIR` to redirect unit-file writes into the
//! test's own tempdir rather than the real `/etc/systemd/system` (see
//! `src/systemd.rs`'s `systemd_unit_dir()` — this override exists
//! specifically so this suite can run as an unprivileged user in any
//! environment, this sandbox included, without touching real system
//! paths). Uses `install_services: false` for most cases and a
//! dedicated services-focused test for the unit-file overwrite path, to
//! keep the systemd-unit-dir env var mutation (which is process-global
//! and therefore must not race with other tests running in parallel in
//! the same process) scoped to as few tests as possible.

// See tests/test_verify_host.rs for the full rationale: this file-scoped
// allow matches the established convention in every other merged unit's
// tests/*.rs files (e.g. lsbx-kernel/tests/test_kernel.rs) — restriction-
// group lints fire on ordinary unwrap()/expect() test assertions, which
// #[cfg(test)] alone doesn't suppress for a tests/*.rs integration file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_bootstrap::systemd::{bootstrap, BootstrapConfig};
use lsbx_kernel::error::LsbxError;
use tokio::sync::Mutex;

// `std::env::set_var` mutates process-global state, and `cargo test` runs
// tests in the same process across threads by default. This mutex
// serializes only the tests in this file that must touch
// `LSBX_SYSTEMD_UNIT_DIR`, so they don't stomp on each other's directory
// override mid-run. `tokio::sync::Mutex` (not `std::sync::Mutex`) because
// the guard is held across `.await` points in these tests — a std mutex
// guard held across an await is a real correctness hazard (clippy's
// `await_holding_lock` catches exactly this), not just a style
// preference, since it can starve other tasks on the same runtime thread
// for the guard's entire lifetime instead of just the critical section.
static ENV_GUARD: Mutex<()> = Mutex::const_new(());

fn config(target: &str, force: bool, dry_run: bool) -> BootstrapConfig {
    BootstrapConfig {
        target: Some(target.to_string()),
        install_services: false,
        verify: false,
        force,
        dry_run,
    }
}

#[tokio::test]
async fn scenario_fresh_host_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    let report = bootstrap(config(&target, false, false))
        .await
        .expect("bootstrap on a fresh host must succeed even without --force");

    assert!(!report.actions_taken.is_empty());
    assert!(
        report.actions_would_take.is_empty(),
        "a real (non-dry-run) run must not report would-take actions"
    );
}

#[tokio::test]
async fn scenario_force_true_reruns_idempotently_without_already_exists_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    // First run bootstraps the host.
    bootstrap(config(&target, false, false))
        .await
        .expect("first run on a fresh host must succeed");

    // Second run, with --force, against the now-already-bootstrapped
    // host: must succeed, not error, and its actions_taken should
    // reflect that things already existed rather than blowing up on
    // that fact.
    let second_report = bootstrap(config(&target, true, false))
        .await
        .expect("force: true rerun on an already-bootstrapped host must succeed idempotently");

    assert!(
        second_report
            .actions_taken
            .iter()
            .any(|a| a.contains("already exists")),
        "an idempotent --force rerun should report 'already exists' conditions rather than silently redoing everything or omitting them; got: {:?}",
        second_report.actions_taken
    );
}

#[tokio::test]
async fn scenario_force_false_on_already_bootstrapped_host_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    // First run bootstraps the host.
    bootstrap(config(&target, false, false))
        .await
        .expect("first run on a fresh host must succeed");

    // Second run, WITHOUT --force, against the now-already-bootstrapped
    // host: must error. This is the case --force exists to let a caller
    // skip — if this doesn't error, --force has nothing to avoid, which
    // is exactly the literal-reading argument for this three-way
    // behavior.
    let result = bootstrap(config(&target, false, false)).await;
    assert!(
        matches!(result, Err(LsbxError::ContractViolated(_))),
        "force: false against an already-bootstrapped host must error with ContractViolated, got: {result:?}"
    );
}

#[tokio::test]
async fn second_run_produces_the_same_report_shape_as_documented_by_the_unit_scenario() {
    // Mirrors the unit contract's own documented scenario almost exactly:
    // "bootstrap(config) twice in a row with force: true against the
    // same temp target directory... asserts the second run succeeds
    // without an 'already exists' error and produces the same
    // BootstrapReport shape." Read literally: "without an 'already
    // exists' error" means the run doesn't fail because of that
    // condition — not that the phrase never appears in its successful
    // report (the previous test already asserts it appears, informing
    // the caller what happened). This test checks the *shape* claim:
    // both runs return non-empty actions_taken and empty
    // actions_would_take (a real run, not a dry-run).
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    let first = bootstrap(config(&target, true, false))
        .await
        .expect("first force:true run must succeed");
    let second = bootstrap(config(&target, true, false))
        .await
        .expect("second force:true run must succeed without erroring on already-exists conditions");

    assert!(!first.actions_taken.is_empty());
    assert!(!second.actions_taken.is_empty());
    assert!(first.actions_would_take.is_empty());
    assert!(second.actions_would_take.is_empty());
}

#[tokio::test]
async fn dry_run_reports_actions_without_writing_anything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    let report = bootstrap(config(&target, false, true))
        .await
        .expect("dry-run must succeed and never error");

    assert!(
        !report.actions_would_take.is_empty(),
        "dry-run must report actions it would take"
    );
    assert!(
        report.actions_taken.is_empty(),
        "dry-run must not report any action as actually taken"
    );

    // The actual behavioral claim: nothing was genuinely written to
    // disk. Check every path a real run would have created.
    assert!(
        !dir.path().join("sandboxes").exists(),
        "dry-run must not create the sandboxes directory"
    );
    assert!(
        !dir.path().join("ci-jobs").exists(),
        "dry-run must not create the ci-jobs directory"
    );
    assert!(
        !dir.path().join(".lsbx-bootstrapped").exists(),
        "dry-run must not write the bootstrap marker"
    );

    // Confirm the directory is genuinely still empty — not just that the
    // three specific paths above are absent, but that dry-run created
    // nothing at all under the target.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("target dir must still be readable")
        .collect();
    assert!(
        entries.is_empty(),
        "dry-run must leave the target directory completely empty, found: {entries:?}"
    );
}

#[tokio::test]
async fn dry_run_on_already_bootstrapped_host_still_only_previews() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().to_str().expect("utf8 path").to_string();

    // Really bootstrap the host first (not dry-run).
    bootstrap(config(&target, false, false))
        .await
        .expect("first real run must succeed");

    let before_snapshot: Vec<_> = walk_files(dir.path());

    // A dry-run against an already-bootstrapped host, even without
    // --force, must preview rather than error or mutate anything further
    // — dry-run's whole purpose is safe inspection.
    let report = bootstrap(config(&target, false, true))
        .await
        .expect("dry-run against an already-bootstrapped host must not error");
    assert!(!report.actions_would_take.is_empty());
    assert!(report.actions_taken.is_empty());

    let after_snapshot: Vec<_> = walk_files(dir.path());
    assert_eq!(
        before_snapshot, after_snapshot,
        "dry-run must not change the on-disk state of an already-bootstrapped host at all"
    );
}

fn walk_files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    fn visit(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out);
            } else {
                out.push(path.display().to_string());
            }
        }
    }
    visit(root, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn force_true_overwrites_changed_unit_file_and_reports_it() {
    let _guard = ENV_GUARD.lock().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("state");
    let unit_dir = dir.path().join("systemd");
    std::fs::create_dir_all(&unit_dir).expect("create unit dir");
    std::env::set_var("LSBX_SYSTEMD_UNIT_DIR", &unit_dir);

    let services_config = |force: bool| BootstrapConfig {
        target: Some(target.to_str().expect("utf8 path").to_string()),
        install_services: true,
        verify: false, // avoid depending on real host libvirt/qemu-img state for this unit-file-focused test
        force,
        dry_run: false,
    };

    // First run: writes both unit files fresh.
    let first = bootstrap(services_config(false))
        .await
        .expect("first run must succeed");
    assert!(first
        .actions_taken
        .iter()
        .any(|a| a.contains("wrote unit file")));

    let broker_unit_path = unit_dir.join("lsbx-ci-broker.service");
    assert!(broker_unit_path.exists());

    // Tamper with one unit file's content, simulating drift since the
    // last bootstrap.
    std::fs::write(&broker_unit_path, "TAMPERED CONTENT\n").expect("tamper with unit file");

    // force: true rerun must detect the drift and overwrite it, not
    // error, and not silently leave the tampered content in place.
    let second = bootstrap(services_config(true))
        .await
        .expect("force: true rerun must succeed even with a changed unit file on disk");
    assert!(
        second
            .actions_taken
            .iter()
            .any(|a| a.contains("overwrote unit file")),
        "a changed unit file must be reported as overwritten, got: {:?}",
        second.actions_taken
    );

    let restored_content =
        std::fs::read_to_string(&broker_unit_path).expect("read restored unit file");
    assert!(restored_content.contains("ExecStart=/usr/local/bin/lsbx --images="));
    assert!(restored_content.contains("ci-broker run --backend=libvirt"));

    std::env::remove_var("LSBX_SYSTEMD_UNIT_DIR");
}

#[tokio::test]
async fn force_true_rerun_leaves_unchanged_unit_file_alone_and_reports_it() {
    let _guard = ENV_GUARD.lock().await;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("state");
    let unit_dir = dir.path().join("systemd");
    std::fs::create_dir_all(&unit_dir).expect("create unit dir");
    std::env::set_var("LSBX_SYSTEMD_UNIT_DIR", &unit_dir);

    let services_config = |force: bool| BootstrapConfig {
        target: Some(target.to_str().expect("utf8 path").to_string()),
        install_services: true,
        verify: false,
        force,
        dry_run: false,
    };

    bootstrap(services_config(false))
        .await
        .expect("first run must succeed");

    // Rerun with force: true, with no tampering this time — the unit
    // files are byte-identical to what would be generated now, so they
    // should be reported as already existing (unchanged), not
    // overwritten.
    let second = bootstrap(services_config(true))
        .await
        .expect("force: true rerun must succeed");
    assert!(
        second
            .actions_taken
            .iter()
            .any(|a| a.contains("already exists (unchanged)")),
        "an unchanged unit file must be reported as already-exists-unchanged, not overwritten; got: {:?}",
        second.actions_taken
    );
    assert!(
        !second
            .actions_taken
            .iter()
            .any(|a| a.contains("overwrote unit file")),
        "an unchanged unit file must not be reported as overwritten"
    );

    std::env::remove_var("LSBX_SYSTEMD_UNIT_DIR");
}
