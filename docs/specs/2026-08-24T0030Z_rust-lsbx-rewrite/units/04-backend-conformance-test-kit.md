# Unit 04 — Backend Conformance Test Kit

## Objective
Define one shared conformance test suite every `Backend` implementation (demo, libvirt, exedev) must pass, so "implements the trait" and "behaves correctly" are the same claim instead of three independently-asserted ones.

## Context
Layer 2, depends only on Unit 01's `Backend` trait. This unit ships no production behavior — it's test infrastructure — but it's substantial and independently reviewable, and Units 05/06/07's own Verification sections reference it directly. This operationalizes the "Verification" noun from SPEC.md §2 for backend *implementations* specifically, the same way Unit 09's healthchecks operationalize it for running VMs.

## Acceptance criteria
- [ ] Exposes one entry point, `run_conformance_suite`, callable from any backend crate's own test file in one line.
- [ ] Covers: create→run→destroy completes without error against a backend-supplied minimal golden reference; `list_vms()` includes a freshly created VM and excludes it after destroy; a backend claiming `capabilities().console == true` is required to produce `Some(https_url)` from at least one successful create, or the check fails; destroying a nonexistent `vm_tag` returns `LsbxError::NotFound`, never panics; `run()` against a destroyed VM returns `NotFound` or `BackendUnavailable`, never hangs past the given timeout.
- [ ] Idempotence-within-tolerance: calling `destroy` twice on the same `vm_tag` is safe — second call returns `Ok(())` or `NotFound`, never panics or corrupts state usable by later tests.
- [ ] `ConformanceReport` records pass/fail per named check individually, not one aggregate boolean, so a failing backend's test output names exactly which invariant broke.
- [ ] The suite is runnable against infrastructure-requiring backends (libvirt, exedev) by feature-gating those backends' own test files with `#[ignore]` plus a documented `--ignored` invocation, while `demo` runs the full suite unconditionally in normal CI.

## Interface contract
```rust
// src/lib.rs
use lsbx_kernel::backend::Backend;
use lsbx_kernel::types::GoldenKey;

pub struct ConformanceCheck {
    pub name: &'static str,
    pub passed: bool,
    pub detail: Option<String>,
}

pub struct ConformanceReport {
    pub checks: Vec<ConformanceCheck>,
}

impl ConformanceReport {
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }
}

/// Runs the full backend conformance suite against any `Backend` implementation.
/// `golden_ref` is a minimal, backend-appropriate golden identifier — a fixed
/// fixture key for `demo`, a real small test golden for `libvirt`/`exedev`.
pub async fn run_conformance_suite<B: Backend>(
    backend: &B,
    golden_ref: &GoldenKey,
) -> ConformanceReport;
```

## Boundaries — do NOT touch
Implements no `Backend` itself — this crate contains only the suite, never a production implementation. Does not decide what a "minimal test golden" contains for libvirt/exedev; each backend unit supplies its own fixture appropriate to its own infrastructure.

## Output
- `crates/lsbx-backend-testkit/Cargo.toml`
- `crates/lsbx-backend-testkit/src/lib.rs`
- `crates/lsbx-backend-testkit/tests/test_kit_self_check.rs`

## Verification
```bash
cargo check -p lsbx-backend-testkit --message-format=json
cargo clippy -p lsbx-backend-testkit --all-targets --all-features -- -D warnings
cargo test -p lsbx-backend-testkit --test test_kit_self_check
```
Scenario: `test_kit_self_check` includes an in-crate fake `Backend` whose `destroy` silently no-ops without removing the VM from `list_vms()`, and asserts `run_conformance_suite` reports that specific check as failed — proving the suite catches the defect class it exists to catch, not merely that it runs to completion against a correct implementation.
