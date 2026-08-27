//! Runs `lsbx-backend-testkit::run_conformance_suite` against a real
//! `ExedevBackend` talking to a real exe.dev account. `#[ignore]`d by
//! default per Unit 07's acceptance criteria — normal CI has no exe.dev
//! credentials and no reachable exe.dev endpoint.
//!
//! Run against the live exe.dev SSH alias (the default on Molimo), or an
//! account token:
//! ```bash
//! cargo test -p lsbx-backend-exedev --test test_conformance -- --ignored
//! # or: EXE_TOKEN=<account-token> cargo test -p lsbx-backend-exedev --test test_conformance -- --ignored
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]
use lsbx_backend_exedev::{ExedevAuth, ExedevBackend};
use lsbx_backend_testkit::run_conformance_suite;
use lsbx_kernel::backend::Backend;
use lsbx_kernel::types::GoldenKey;

#[tokio::test]
#[ignore = "requires a reachable exe.dev control plane; uses EXE_TOKEN or the configured SSH alias"]
async fn exedev_backend_passes_conformance_suite() {
    let auth = match std::env::var("EXE_TOKEN") {
        Ok(token) if !token.is_empty() => ExedevAuth::account_token(token),
        _ => ExedevAuth::ssh_alias(
            std::env::var("LSBX_EXEDEV_SSH_ALIAS").unwrap_or_else(|_| "exe.dev".to_string()),
        ),
    };
    let backend = ExedevBackend::new(auth);

    let golden_ref = GoldenKey::new_unchecked(
        std::env::var("LSBX_EXEDEV_TEST_GOLDEN").unwrap_or_else(|_| "lsbx-default-v1".to_string()),
    );

    let report = run_conformance_suite(&backend, &golden_ref).await;

    // The shared suite intentionally calls plain `destroy`, so remove the
    // fixed conformance key explicitly. The removal is idempotent: it also
    // cleans up a key left by an interrupted prior run or a failed create.
    let cleanup = backend
        .destroy_with_key(
            "lsbx-conformance-test-vm",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFcFi7kjVO1u+zN87aUSxqktiGksMgqfNe/o5ICyMeSi conformance@lsbx",
        )
        .await;
    assert!(
        matches!(
            cleanup,
            Ok(()) | Err(lsbx_kernel::error::LsbxError::NotFound(_))
        ),
        "conformance cleanup failed: {cleanup:?}"
    );

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
