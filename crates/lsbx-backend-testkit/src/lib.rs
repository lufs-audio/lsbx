//! Shared backend conformance test suite (Unit 04).
//!
//! Every `Backend` implementation (`lsbx-backend-demo`, `lsbx-backend-libvirt`,
//! `lsbx-backend-exedev`) is expected to run [`run_conformance_suite`] against
//! itself from its own crate's test files, so "implements the `Backend` trait"
//! and "behaves correctly" are the same claim instead of three independently
//! asserted ones (SPEC.md §2, noun 6 — Verification, applied to backend
//! implementations).
//!
//! This crate ships no production behavior. It defines the suite only; each
//! backend supplies its own minimal test golden appropriate to its own
//! infrastructure (a fixed fixture key for `demo`, a real small golden for
//! `libvirt`/`exedev`) and its own `Backend` instance.
//!
//! ## Convention for infrastructure-requiring backends
//! `demo` has no real infrastructure and is expected to run the full suite
//! unconditionally in normal CI. `libvirt` and `exedev` require real
//! infrastructure (a libvirt socket, an exe.dev endpoint) that CI does not
//! provide by default; those backends' own test files should call this suite
//! from a `#[tokio::test]` annotated `#[ignore]`, with a doc comment
//! documenting the `cargo test -- --ignored` invocation needed to run it
//! against real infrastructure. This crate does not and cannot enforce that
//! convention (it has no visibility into how a downstream crate structures
//! its own test files) — it is asserted here so Units 05/06/07 have one
//! place to find it.
use lsbx_kernel::backend::{Backend, CommandOutput, CreateFromGoldenRequest};
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::GoldenKey;
use std::time::Duration;

/// Renders a `run()` result for a failure-detail message without requiring
/// `CommandOutput` to implement `Debug` (it deliberately doesn't — it's a
/// kernel data type, not a diagnostics type). Only the `exit_code` is worth
/// surfacing here; `stdout`/`stderr` bytes aren't relevant to *which
/// invariant broke*, which is all a `ConformanceCheck::detail` needs to say.
fn describe_run_result(result: &Result<CommandOutput, LsbxError>) -> String {
    match result {
        Ok(out) => format!("Ok(exit_code={})", out.exit_code),
        Err(e) => format!("Err({e:?})"),
    }
}

/// The outcome of a single named invariant check.
///
/// Reported individually — never folded into one aggregate boolean — so a
/// failing backend's test output names exactly which invariant broke
/// (Unit 04 acceptance criteria).
#[derive(Debug, Clone)]
pub struct ConformanceCheck {
    pub name: &'static str,
    pub passed: bool,
    pub detail: Option<String>,
}

/// The full result of running [`run_conformance_suite`] against one backend.
#[derive(Debug, Clone)]
pub struct ConformanceReport {
    pub checks: Vec<ConformanceCheck>,
}

impl ConformanceReport {
    /// True only if every check that ran, passed. A check that was skipped
    /// because a prerequisite (e.g. `create_completes`) failed is not
    /// counted as passed — it simply never appears in `checks` — so
    /// `all_passed()` still correctly returns `false` whenever the
    /// prerequisite itself failed.
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// Look up a specific named check's result, for callers (including this
    /// crate's own self-check test) that need to assert on one invariant by
    /// name rather than on the aggregate.
    pub fn check(&self, name: &str) -> Option<&ConformanceCheck> {
        self.checks.iter().find(|c| c.name == name)
    }
}

/// Bound placed on every backend call the suite itself makes (independent of
/// whatever timeout a test passes into `Backend::run`), so a backend that
/// hangs instead of returning an error cannot hang the conformance suite —
/// and, transitively, cannot hang the CI job of whichever backend crate is
/// calling this suite from its own tests.
const SUITE_CALL_BOUND: Duration = Duration::from_secs(30);

async fn bounded<T, F>(fut: F) -> Result<T, LsbxError>
where
    F: std::future::Future<Output = Result<T, LsbxError>>,
{
    match tokio::time::timeout(SUITE_CALL_BOUND, fut).await {
        Ok(inner) => inner,
        Err(_) => Err(LsbxError::BackendUnavailable(format!(
            "backend call did not return within the suite's {SUITE_CALL_BOUND:?} bound"
        ))),
    }
}

fn record_result<T>(
    checks: &mut Vec<ConformanceCheck>,
    name: &'static str,
    result: &Result<T, LsbxError>,
) {
    match result {
        Ok(_) => checks.push(ConformanceCheck {
            name,
            passed: true,
            detail: None,
        }),
        Err(e) => checks.push(ConformanceCheck {
            name,
            passed: false,
            detail: Some(e.to_string()),
        }),
    }
}

fn record_condition(
    checks: &mut Vec<ConformanceCheck>,
    name: &'static str,
    condition: bool,
    detail_if_false: impl Into<String>,
) {
    checks.push(ConformanceCheck {
        name,
        passed: condition,
        detail: if condition {
            None
        } else {
            Some(detail_if_false.into())
        },
    });
}

/// Runs the full backend conformance suite against any `Backend`
/// implementation.
///
/// `golden_ref` is a minimal, backend-appropriate golden identifier — a
/// fixed fixture key for `demo`, a real small test golden for
/// `libvirt`/`exedev`. This suite does not decide what that golden contains;
/// each backend supplies its own (Unit 04 Boundaries).
///
/// Checks that structurally require a successfully created VM (list
/// membership, live `run`, destroy, post-destroy list exclusion, destroy
/// idempotence, post-destroy `run` behavior) are skipped — not recorded as
/// failed — when `create_from_golden` itself fails, since there is no
/// `vm_tag` to exercise them against. Checks that do not depend on a live VM
/// (destroying and running against a `vm_tag` that was never created) always
/// run, so a backend that cannot even create a VM still surfaces every
/// invariant it *can* be measured against, rather than a single opaque
/// failure.
pub async fn run_conformance_suite<B: Backend>(
    backend: &B,
    golden_ref: &GoldenKey,
) -> ConformanceReport {
    let mut checks = Vec::new();

    // Checks that never require a live VM run first and unconditionally,
    // so even a backend whose `create_from_golden` is completely broken
    // still gets measured against every invariant that doesn't need a real
    // VM to exist.
    run_never_created_checks(backend, &mut checks).await;

    let req = CreateFromGoldenRequest {
        golden: golden_ref,
        name: "lsbx-conformance-test-vm",
        pubkey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIConformanceTestKeyPlaceholder conformance@lsbx",
        cpu: 1,
        memory: "512M",
    };

    let create_result = bounded(backend.create_from_golden(req)).await;
    record_result(&mut checks, "create_completes", &create_result);

    let created_vm = match create_result {
        Ok(vm) => vm,
        // create_from_golden failed: every remaining check needs a live
        // vm_tag to exercise, so there is nothing further this suite can
        // measure. `create_completes` above already carries the failure
        // detail.
        Err(_) => return ConformanceReport { checks },
    };

    let vm_tag = created_vm.vm_tag.clone();

    let caps = backend.capabilities();
    if caps.console {
        record_condition(
            &mut checks,
            "console_capability_produces_url",
            created_vm.https_url.is_some(),
            "capabilities().console reported true but create_from_golden did not return an https_url",
        );
    }

    let vms_after_create = bounded(backend.list_vms()).await;
    match &vms_after_create {
        Ok(vms) => record_condition(
            &mut checks,
            "list_vms_includes_created",
            vms.contains(&vm_tag),
            format!("list_vms() did not contain freshly created vm_tag '{vm_tag}'"),
        ),
        Err(e) => checks.push(ConformanceCheck {
            name: "list_vms_includes_created",
            passed: false,
            detail: Some(format!("list_vms() call failed: {e}")),
        }),
    }

    let run_result = bounded(backend.run(
        &vm_tag,
        &["echo".to_string(), "lsbx-conformance-check".to_string()],
        Duration::from_secs(10),
    ))
    .await;
    record_result(&mut checks, "run_on_live_vm_succeeds", &run_result);

    let destroy_result = bounded(backend.destroy(&vm_tag)).await;
    record_result(&mut checks, "destroy_completes", &destroy_result);

    let vms_after_destroy = bounded(backend.list_vms()).await;
    match &vms_after_destroy {
        Ok(vms) => record_condition(
            &mut checks,
            "list_vms_excludes_destroyed",
            !vms.contains(&vm_tag),
            format!("list_vms() still contained destroyed vm_tag '{vm_tag}'"),
        ),
        Err(e) => checks.push(ConformanceCheck {
            name: "list_vms_excludes_destroyed",
            passed: false,
            detail: Some(format!("list_vms() call failed: {e}")),
        }),
    }

    let second_destroy = bounded(backend.destroy(&vm_tag)).await;
    let idempotent = matches!(second_destroy, Ok(()) | Err(LsbxError::NotFound(_)));
    record_condition(
        &mut checks,
        "destroy_idempotent",
        idempotent,
        format!(
            "second destroy() of the same vm_tag returned {second_destroy:?}, expected Ok(()) or NotFound"
        ),
    );

    let run_after_destroy = bounded(backend.run(
        &vm_tag,
        &["echo".to_string(), "should-not-run".to_string()],
        Duration::from_secs(5),
    ))
    .await;
    let run_after_destroy_ok = matches!(
        run_after_destroy,
        Err(LsbxError::NotFound(_)) | Err(LsbxError::BackendUnavailable(_))
    );
    record_condition(
        &mut checks,
        "run_against_destroyed_vm_errors",
        run_after_destroy_ok,
        format!(
            "run() against a destroyed vm_tag returned {}, expected NotFound or BackendUnavailable",
            describe_run_result(&run_after_destroy)
        ),
    );

    ConformanceReport { checks }
}

/// The subset of checks that exercise a `vm_tag` known never to have been
/// created by this backend. These do not depend on `create_from_golden`
/// succeeding, so they always run.
async fn run_never_created_checks<B: Backend>(backend: &B, checks: &mut Vec<ConformanceCheck>) {
    let destroy_nonexistent = bounded(backend.destroy("lsbx-conformance-never-created-vm")).await;
    let destroy_nonexistent_ok = matches!(destroy_nonexistent, Err(LsbxError::NotFound(_)));
    record_condition(
        checks,
        "destroy_nonexistent_returns_notfound",
        destroy_nonexistent_ok,
        format!(
            "destroy() of a vm_tag that was never created returned {destroy_nonexistent:?}, expected NotFound"
        ),
    );

    let run_nonexistent = bounded(backend.run(
        "lsbx-conformance-never-created-vm",
        &["echo".to_string(), "should-not-run".to_string()],
        Duration::from_secs(5),
    ))
    .await;
    let run_nonexistent_ok = matches!(
        run_nonexistent,
        Err(LsbxError::NotFound(_)) | Err(LsbxError::BackendUnavailable(_))
    );
    record_condition(
        checks,
        "run_against_nonexistent_vm_errors",
        run_nonexistent_ok,
        format!(
            "run() against a vm_tag that was never created returned {}, expected NotFound or BackendUnavailable",
            describe_run_result(&run_nonexistent)
        ),
    );
}
