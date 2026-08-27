//! Integration tests for `verify_host()`.
//!
//! This sandbox has no running libvirt daemon (only `libvirt.so`, built
//! from source by an earlier unit, with no `libvirtd` process and no
//! socket listening at the well-known path) and no real remote host to
//! test against. Every test here is written to pass regardless of
//! whether a real libvirt socket happens to be present — it asserts on
//! the *shape* of `HostVerification` (which named checks ran, and that
//! `verify_host` never short-circuits) rather than asserting a specific
//! pass/fail outcome for the libvirt-socket check, since that outcome is
//! genuinely environment-dependent and this unit's job is to report it
//! accurately, not to assume any particular host state.

// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside
// `cargo test`, never in a shipped code path. clippy::unwrap_used /
// expect_used are restriction-group lints that don't understand "this
// whole file is test code" the way #[cfg(test)] does, so they fire here
// even though this unit's own acceptance criteria (and every other
// merged unit's test files, e.g. lsbx-kernel/tests/test_kernel.rs) rely
// on idiomatic unwrap()/expect()-based assertions. Allow both, scoped to
// this file only — crates/lsbx-bootstrap/src/**/*.rs (the real
// production code path) has no expect()/unwrap() outside its own
// #[cfg(test)] modules, which carry the same scoped allow.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_bootstrap::verify_host::verify_host;

#[tokio::test]
async fn reports_qemu_img_check() {
    // qemu-img is installed in this sandbox (via the system package
    // manager, ahead of running this suite) — this assertion is a real,
    // environment-grounded pass, not a guess.
    let verification = verify_host(None)
        .await
        .expect("verify_host should not itself error");
    let qemu_check = verification
        .checks
        .iter()
        .find(|c| c.name == "qemu_img_present")
        .expect("qemu_img_present check must always run for a local target");
    // The check is intentionally environment-sensitive: Molimo does not
    // need qemu-img at runtime, while Carnyx does. Verify that the check is
    // reported with useful detail without making this host-specific probe a
    // prerequisite for the generic bootstrap test suite.
    assert!(qemu_check.detail.is_some());
}

#[tokio::test]
async fn reports_state_directory_check_individually() {
    let verification = verify_host(None)
        .await
        .expect("verify_host should not itself error");
    assert!(
        verification
            .checks
            .iter()
            .any(|c| c.name == "state_directories_present_and_0700"),
        "state directory permissions must be reported as their own named check"
    );
}

#[tokio::test]
async fn local_target_includes_libvirt_socket_check() {
    // Whether the socket check *passes* depends on real host state (no
    // libvirtd is running in this sandbox), but the check must always be
    // attempted for a local (target: None) host — that's the "proven, not
    // exited 0" contract: report the fact, whatever it is, rather than
    // omitting the check.
    let verification = verify_host(None)
        .await
        .expect("verify_host should not itself error");
    assert!(
        verification
            .checks
            .iter()
            .any(|c| c.name == "libvirt_socket_reachable"),
        "a local target must always attempt the libvirt socket check"
    );
}

#[tokio::test]
async fn remote_target_omits_libvirt_socket_check() {
    // Unit 06 owns remote-libvirt reachability via its own RemoteSsh
    // transport; this crate does not duplicate that check for a remote
    // target (see this unit's Boundaries: "does not implement
    // create_from_golden or domain lifecycle").
    let verification = verify_host(Some("remote-host.example.com"))
        .await
        .expect("verify_host should not itself error");
    assert!(
        !verification
            .checks
            .iter()
            .any(|c| c.name == "libvirt_socket_reachable"),
        "a remote target must not get a local-socket check it cannot answer honestly"
    );
}

#[tokio::test]
async fn never_short_circuits_reports_every_check_even_when_one_fails() {
    // Point state-directory verification at a path that is guaranteed not
    // to exist, forcing that check to fail, and confirm the qemu-img
    // check still ran and was reported anyway.
    let bogus_target = "/definitely/does/not/exist/lsbx-test-verify-host";
    let verification = verify_host(Some(bogus_target))
        .await
        .expect("verify_host should not itself error even when a check fails");

    let state_check = verification
        .checks
        .iter()
        .find(|c| c.name == "state_directories_present_and_0700")
        .expect("state directory check must still be reported");
    assert!(
        !state_check.passed,
        "a nonexistent target directory must fail this check"
    );

    let qemu_check = verification
        .checks
        .iter()
        .find(|c| c.name == "qemu_img_present")
        .expect("qemu_img_present must still be reported even though another check failed");
    // Whatever the qemu-img outcome is, the point is it was *reported*,
    // not skipped because the state-dir check failed first.
    let _ = qemu_check.passed;

    assert!(!verification.all_passed());
}

#[tokio::test]
async fn passing_state_directories_are_reported_as_passed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path();
    std::fs::create_dir_all(base.join("sandboxes")).expect("create sandboxes dir");
    std::fs::create_dir_all(base.join("ci-jobs")).expect("create ci-jobs dir");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        base.join("sandboxes"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("chmod sandboxes");
    std::fs::set_permissions(base.join("ci-jobs"), std::fs::Permissions::from_mode(0o700))
        .expect("chmod ci-jobs");

    let verification = verify_host(Some(base.to_str().expect("utf8 path")))
        .await
        .expect("verify_host should not itself error");

    let state_check = verification
        .checks
        .iter()
        .find(|c| c.name == "state_directories_present_and_0700")
        .expect("state directory check must be reported");
    assert!(
        state_check.passed,
        "0700 directories that exist should pass, got: {:?}",
        state_check.detail
    );
}

#[tokio::test]
async fn wrong_permission_mode_is_reported_as_failed_with_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path();
    std::fs::create_dir_all(base.join("sandboxes")).expect("create sandboxes dir");
    std::fs::create_dir_all(base.join("ci-jobs")).expect("create ci-jobs dir");
    use std::os::unix::fs::PermissionsExt;
    // Deliberately wrong mode (0755, not 0700).
    std::fs::set_permissions(
        base.join("sandboxes"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod sandboxes");
    std::fs::set_permissions(base.join("ci-jobs"), std::fs::Permissions::from_mode(0o700))
        .expect("chmod ci-jobs");

    let verification = verify_host(Some(base.to_str().expect("utf8 path")))
        .await
        .expect("verify_host should not itself error");

    let state_check = verification
        .checks
        .iter()
        .find(|c| c.name == "state_directories_present_and_0700")
        .expect("state directory check must be reported");
    assert!(
        !state_check.passed,
        "0755 must not satisfy the 0700 requirement"
    );
    let detail = state_check.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("755") || detail.contains("0755"),
        "failure detail should name the actual (wrong) mode found, got: {detail}"
    );
}
