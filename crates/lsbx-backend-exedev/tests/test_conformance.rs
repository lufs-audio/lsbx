//! Runs `lsbx-backend-testkit::run_conformance_suite` against a real
//! `ExedevBackend` talking to a real exe.dev account. `#[ignore]`d by
//! default per Unit 07's acceptance criteria — normal CI has no exe.dev
//! credentials and no reachable exe.dev endpoint.
//!
//! Run against a real account with:
//! ```bash
//! EXE_TOKEN=<your account token> \
//!   cargo test -p lsbx-backend-exedev --test test_conformance -- --ignored
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]
use lsbx_backend_exedev::{ExedevAuth, ExedevBackend};
use lsbx_backend_testkit::run_conformance_suite;
use lsbx_kernel::types::GoldenKey;

#[tokio::test]
#[ignore = "requires a real exe.dev account and EXE_TOKEN — run with `cargo test -- --ignored`"]
async fn exedev_backend_passes_conformance_suite() {
    let token = std::env::var("EXE_TOKEN")
        .expect("EXE_TOKEN must be set to run this ignored test against a real exe.dev account");
    let backend = ExedevBackend::new(ExedevAuth::account_token(token));

    // A minimal, backend-appropriate golden identifier — exe.dev's smallest
    // provisionable image, per this crate's own `golden_ref` contract from
    // Unit 04 (this crate does not decide what golden this key resolves to;
    // that's Unit 08's registry job, this is just a string exe.dev's `new`
    // verb accepts as a base image name against a real account).
    let golden_ref = GoldenKey::new_unchecked("exeuntu".to_string());

    let report = run_conformance_suite(&backend, &golden_ref).await;

    for check in &report.checks {
        println!(
            "{}: {}{}",
            check.name,
            if check.passed { "PASS" } else { "FAIL" },
            check
                .detail
                .as_ref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
    }

    assert!(
        report.all_passed(),
        "exedev backend failed one or more conformance checks: {:?}",
        report
            .checks
            .iter()
            .filter(|c| !c.passed)
            .collect::<Vec<_>>()
    );
}
