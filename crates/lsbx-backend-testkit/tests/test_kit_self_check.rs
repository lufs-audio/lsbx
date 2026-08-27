// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside `cargo test`,
// never in a shipped code path. clippy::unwrap_used / expect_used are
// restriction-group lints that don't understand "this whole file is test
// code" the way #[cfg(test)] does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this file only —
// crates/lsbx-backend-testkit/src/**/*.rs (the real production code path) is
// unwrap/expect/panic-free under the same workspace lints with no allow
// needed. Pattern established in Unit 01's crates/lsbx-kernel/tests/test_kernel.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use lsbx_backend_testkit::run_conformance_suite;
use lsbx_kernel::backend::{Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm};
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::GoldenKey;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

fn test_golden() -> GoldenKey {
    GoldenKey::new_unchecked("fake-golden".to_string())
}

/// A fully conformant in-memory fake `Backend`, used as a control: proves the
/// suite reports a clean pass against an implementation that actually
/// respects every invariant, so the broken-backend test below is exercising
/// the suite's ability to *detect* a defect, not just its willingness to
/// report failures no matter what.
struct ConformantFakeBackend {
    vms: Mutex<Vec<String>>,
    counter: AtomicU64,
    capabilities: BackendCapabilities,
}

impl ConformantFakeBackend {
    fn new(capabilities: BackendCapabilities) -> Self {
        Self {
            vms: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            capabilities,
        }
    }

    fn next_tag(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("conformant-vm-{n}")
    }
}

#[async_trait]
impl Backend for ConformantFakeBackend {
    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities.clone()
    }

    async fn create_from_golden(&self, _req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError> {
        let vm_tag = self.next_tag();
        self.vms.lock().expect("lock poisoned").push(vm_tag.clone());
        let https_url = if self.capabilities.console {
            Some(format!("https://console.example/{vm_tag}"))
        } else {
            None
        };
        Ok(CreatedVm {
            vm_tag,
            host: "fake-host".to_string(),
            https_url,
        })
    }

    async fn run(&self, vm_tag: &str, _command: &[String], _timeout: Duration, _identity_file: Option<&std::path::Path>) -> Result<CommandOutput, LsbxError> {
        let exists = self.vms.lock().expect("lock poisoned").iter().any(|t| t == vm_tag);
        if exists {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
            })
        } else {
            Err(LsbxError::NotFound(format!("no such vm: {vm_tag}")))
        }
    }

    async fn put_file(&self, _vm_tag: &str, _source: &std::path::Path, _destination: &str, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        Ok(())
    }

    async fn get_file(&self, _vm_tag: &str, _source: &str, _destination: &std::path::Path, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        Ok(())
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        let mut vms = self.vms.lock().expect("lock poisoned");
        let before = vms.len();
        vms.retain(|t| t != vm_tag);
        if vms.len() == before {
            Err(LsbxError::NotFound(format!("no such vm: {vm_tag}")))
        } else {
            Ok(())
        }
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        Ok(self.vms.lock().expect("lock poisoned").clone())
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        Ok(())
    }
}

/// A deliberately broken fake `Backend`: `destroy` reports success but is a
/// silent no-op — it never actually removes the VM from the list `list_vms()`
/// returns. This is exactly the defect class Unit 04 exists to catch (per
/// the unit's own Verification scenario), and the assertion below checks
/// that the suite names the specific invariant this defect breaks rather
/// than merely noticing *something* is wrong.
struct DestroyNoOpsBackend {
    vms: Mutex<Vec<String>>,
    counter: AtomicU64,
}

impl DestroyNoOpsBackend {
    fn new() -> Self {
        Self {
            vms: Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
        }
    }

    fn next_tag(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("noop-vm-{n}")
    }
}

#[async_trait]
impl Backend for DestroyNoOpsBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn create_from_golden(&self, _req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError> {
        let vm_tag = self.next_tag();
        self.vms.lock().expect("lock poisoned").push(vm_tag.clone());
        Ok(CreatedVm {
            vm_tag,
            host: "fake-host".to_string(),
            https_url: None,
        })
    }

    async fn run(&self, vm_tag: &str, _command: &[String], _timeout: Duration, _identity_file: Option<&std::path::Path>) -> Result<CommandOutput, LsbxError> {
        // Deliberately reports the VM as still runnable even after a
        // "successful" destroy, matching the no-op destroy defect: from the
        // outside, nothing about this backend's state ever changed.
        let exists = self.vms.lock().expect("lock poisoned").iter().any(|t| t == vm_tag);
        if exists {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
            })
        } else {
            Err(LsbxError::NotFound(format!("no such vm: {vm_tag}")))
        }
    }

    async fn put_file(&self, _vm_tag: &str, _source: &std::path::Path, _destination: &str, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        Ok(())
    }

    async fn get_file(&self, _vm_tag: &str, _source: &str, _destination: &std::path::Path, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        Ok(())
    }

    /// The injected defect: always reports success, never actually removes
    /// the tag from `self.vms`. `list_vms()` after this call will still
    /// contain the "destroyed" VM.
    async fn destroy(&self, _vm_tag: &str) -> Result<(), LsbxError> {
        Ok(())
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        Ok(self.vms.lock().expect("lock poisoned").clone())
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        Ok(())
    }
}

#[tokio::test]
async fn suite_passes_cleanly_against_a_conformant_backend_without_console() {
    let backend = ConformantFakeBackend::new(BackendCapabilities::default());
    let report = run_conformance_suite(&backend, &test_golden()).await;

    assert!(
        report.all_passed(),
        "expected a fully conformant backend to pass every check, got: {:#?}",
        report.checks
    );
    // Sanity: every check this suite is documented to run against a
    // create-succeeding, non-console backend actually appeared.
    let names: Vec<&str> = report.checks.iter().map(|c| c.name).collect();
    for expected in [
        "destroy_nonexistent_returns_notfound",
        "run_against_nonexistent_vm_errors",
        "create_completes",
        "list_vms_includes_created",
        "run_on_live_vm_succeeds",
        "destroy_completes",
        "list_vms_excludes_destroyed",
        "destroy_idempotent",
        "run_against_destroyed_vm_errors",
    ] {
        assert!(
            names.contains(&expected),
            "expected check '{expected}' to have run, got checks: {names:?}"
        );
    }
    // console_capability_produces_url should NOT appear when
    // capabilities().console is false.
    assert!(report.check("console_capability_produces_url").is_none());
}

#[tokio::test]
async fn suite_checks_console_capability_when_claimed() {
    let backend = ConformantFakeBackend::new(BackendCapabilities {
        console: true,
        remote_transport: false,
        snapshot: false,
    });
    let report = run_conformance_suite(&backend, &test_golden()).await;

    assert!(
        report.all_passed(),
        "expected a conformant console-capable backend to pass every check, got: {:#?}",
        report.checks
    );
    let console_check = report
        .check("console_capability_produces_url")
        .expect("console_capability_produces_url should have run when capabilities().console is true");
    assert!(console_check.passed);
}

/// The scenario the unit contract names explicitly: a `destroy()` that
/// silently no-ops without removing the VM from `list_vms()`. This proves
/// `run_conformance_suite` actually catches this defect class — names the
/// specific broken invariant — rather than merely running to completion
/// against a correct implementation.
#[tokio::test]
async fn suite_catches_a_destroy_that_silently_no_ops() {
    let backend = DestroyNoOpsBackend::new();
    let report = run_conformance_suite(&backend, &test_golden()).await;

    assert!(
        !report.all_passed(),
        "expected the no-op-destroy defect to fail at least one check, but the suite reported all_passed()"
    );

    let list_vms_excludes_destroyed = report
        .check("list_vms_excludes_destroyed")
        .expect("list_vms_excludes_destroyed should have run — create_from_golden succeeds for this backend");
    assert!(
        !list_vms_excludes_destroyed.passed,
        "expected list_vms_excludes_destroyed to fail against a destroy() that never actually removes the VM"
    );
    assert!(
        list_vms_excludes_destroyed
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("still contained destroyed vm_tag"),
        "expected a detail message naming the still-present vm_tag, got: {:?}",
        list_vms_excludes_destroyed.detail
    );

    // destroy_completes itself should still be reported as passing — the
    // backend's destroy() call really did return Ok(()), it just didn't do
    // anything. That's precisely why this defect needs the *separate*
    // list-membership check to catch it: "returned Ok" and "actually
    // destroyed" are different claims, and this suite is not allowed to
    // conflate them.
    let destroy_completes = report
        .check("destroy_completes")
        .expect("destroy_completes should have run");
    assert!(destroy_completes.passed);
}

/// `destroy()` on a `vm_tag` that was never created at all must still be
/// exercised, and must still return `NotFound`, even independent of whether
/// create/destroy of a *real* VM behaves correctly.
#[tokio::test]
async fn suite_checks_destroy_of_never_created_vm_independently() {
    let backend = ConformantFakeBackend::new(BackendCapabilities::default());
    let report = run_conformance_suite(&backend, &test_golden()).await;

    let check = report
        .check("destroy_nonexistent_returns_notfound")
        .expect("destroy_nonexistent_returns_notfound should always run");
    assert!(check.passed);
}

/// When `create_from_golden` itself fails, checks that structurally require
/// a live `vm_tag` cannot run — but the checks that only need a `vm_tag`
/// known never to exist still must, so a backend that can't even create a
/// VM still surfaces every invariant it can be measured against instead of
/// a single opaque failure.
#[tokio::test]
async fn suite_still_runs_never_created_checks_when_create_fails() {
    struct AlwaysFailsCreateBackend;

    #[async_trait]
    impl Backend for AlwaysFailsCreateBackend {
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn create_from_golden(&self, _req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError> {
            Err(LsbxError::BackendUnavailable("control plane unreachable".to_string()))
        }

        async fn run(&self, _vm_tag: &str, _command: &[String], _timeout: Duration, _identity_file: Option<&std::path::Path>) -> Result<CommandOutput, LsbxError> {
            Err(LsbxError::NotFound("no such vm".to_string()))
        }

        async fn put_file(&self, _vm_tag: &str, _source: &std::path::Path, _destination: &str, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
            Err(LsbxError::BackendUnavailable("control plane unreachable".to_string()))
        }

        async fn get_file(&self, _vm_tag: &str, _source: &str, _destination: &std::path::Path, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
            Err(LsbxError::BackendUnavailable("control plane unreachable".to_string()))
        }

        async fn destroy(&self, _vm_tag: &str) -> Result<(), LsbxError> {
            Err(LsbxError::NotFound("no such vm".to_string()))
        }

        async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
            Ok(Vec::new())
        }

        async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
            Err(LsbxError::NotFound("no such vm".to_string()))
        }
    }

    let backend = AlwaysFailsCreateBackend;
    let report = run_conformance_suite(&backend, &test_golden()).await;

    assert!(!report.all_passed());
    let create_check = report.check("create_completes").expect("create_completes should always run");
    assert!(!create_check.passed);

    // These do not depend on create succeeding and must still have run.
    assert!(report.check("destroy_nonexistent_returns_notfound").is_some());
    assert!(report.check("run_against_nonexistent_vm_errors").is_some());

    // These structurally require a live vm_tag and must NOT have run (not
    // "run and marked failed" — genuinely absent, since there is nothing
    // for them to have exercised).
    for skipped in [
        "list_vms_includes_created",
        "run_on_live_vm_succeeds",
        "destroy_completes",
        "list_vms_excludes_destroyed",
        "destroy_idempotent",
        "run_against_destroyed_vm_errors",
        "console_capability_produces_url",
    ] {
        assert!(
            report.check(skipped).is_none(),
            "expected '{skipped}' to be absent (skipped) when create fails, but it was recorded: {:?}",
            report.check(skipped)
        );
    }
}

#[test]
fn golden_key_is_constructed_via_new_unchecked() {
    // Regression guard: GoldenKey's inner field is private outside
    // lsbx-kernel (see crates/lsbx-kernel/src/types.rs doc comment) — the
    // only cross-crate construction path is `new_unchecked`. This test
    // exists so a future edit that reintroduces a direct tuple-literal
    // `GoldenKey(...)` construction in this crate's test files fails to
    // compile here first, loudly, instead of silently breaking `cargo check`
    // for the whole crate.
    let key = GoldenKey::new_unchecked("regression-check".to_string());
    assert_eq!(key.as_str(), "regression-check");
}
