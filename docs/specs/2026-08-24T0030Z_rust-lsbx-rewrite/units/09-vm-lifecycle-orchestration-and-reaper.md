# Unit 09 — VM Lifecycle Orchestration & Reaper

## Objective
Implement create/destroy/renew/list/info and the TTL-based reap loop, composing Unit 02 (store), Unit 03 (keys), and the generic `Backend` trait (exercised in this unit's own tests via Unit 05's demo backend).

## Context
Layer 4, parallel with Unit 08. This is the state machine at the center of the system — every "ran vs. proven" distinction this spec cares about for VMs (a readiness timeout that actually blocks on a healthcheck, not just "the backend call returned") lives here.

## Acceptance criteria
- [ ] `create(request)`: generates an ephemeral keypair (Unit 03), calls `Backend::create_from_golden`, persists a `SandboxRecord` (Unit 02) **before** returning to the caller — durability-before-ack, matching the pattern already established in `snuze` — then polls readiness up to `--ready-timeout` unless `--no-verify` is passed.
- [ ] Readiness is proven, not assumed: a VM is only reported ready after its golden's healthchecks (if any) pass via `Backend::run`, never merely after `create_from_golden` returns `Ok`.
- [ ] `destroy(id)` calls `Backend::destroy`, then `cleanup_keypair` (Unit 03), then `SandboxStore::delete` — in that exact order, so a failure partway through leaves a diagnosable state instead of silently losing the record while the VM still exists.
- [ ] `renew(id, duration)` extends `lease_expires_at` and persists the update; refuses to renew a sandbox whose `cleanup_failed` flag is set — matches the existing safety property of not extending the life of something already known to be broken.
- [ ] `reap(ttl, dry_run)` sweeps sandboxes past `lease_expires_at`, destroys them, and separately reconciles orphaned ephemeral keys (Unit 03) against the set of currently-known labels. `--dry-run` reports what *would* be destroyed without calling `Backend::destroy`.
- [ ] The reaper consults `lsbx-golden::allowed_goldens()` before considering any golden for cleanup, so a golden a live sandbox still depends on is never removed out from under it.
- [ ] `list()`/`info(id)` return `SandboxRecord::public()` projections only — key material never crosses this boundary.
- [ ] A `FakeClock`-driven test proves lease-expiry sweeping is deterministic and does not depend on a real wall-clock sleep inside the test.

## Interface contract
```rust
// src/create.rs
use lsbx_kernel::{backend::Backend, clock::Clock, error::LsbxError, types::PublicSandbox};
use lsbx_store::SandboxStore;

pub struct CreateRequest<'a> {
    pub profile: &'a str,
    pub name: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub lease: std::time::Duration,
    pub ready_timeout: std::time::Duration,
    pub verify: bool, // false when --no-verify
}

pub async fn create(
    backend: &dyn Backend,
    store: &SandboxStore,
    clock: &dyn Clock,
    req: CreateRequest<'_>,
) -> Result<PublicSandbox, LsbxError>;

pub async fn destroy(backend: &dyn Backend, store: &SandboxStore, id: &str) -> Result<(), LsbxError>;

pub async fn renew(
    store: &SandboxStore,
    clock: &dyn Clock,
    id: &str,
    duration: std::time::Duration,
) -> Result<PublicSandbox, LsbxError>;

// src/reap.rs
pub struct ReapReport {
    pub destroyed: Vec<String>,      // sandbox ids
    pub would_destroy: Vec<String>,  // populated only when dry_run
    pub keys_reconciled: usize,
}

pub async fn reap(
    backend: &dyn Backend,
    store: &SandboxStore,
    clock: &dyn Clock,
    allowed_goldens: &std::collections::HashSet<String>,
    ttl: std::time::Duration,
    dry_run: bool,
) -> Result<ReapReport, LsbxError>;

// src/lease.rs
pub fn is_expired(record: &lsbx_kernel::types::SandboxRecord, clock: &dyn Clock) -> bool;
```

## Boundaries — do NOT touch
Does not define `SandboxRecord`'s shape (Unit 01) or its persistence mechanics (Unit 02) — only orchestrates calls into them. Implements no `Backend` itself — takes `&dyn Backend` generically, exercised via Unit 05's `DemoBackend` in this unit's own tests. Does not parse the golden registry (Unit 08) — only consumes `allowed_goldens()`'s output as an opaque set.

## Output
- `crates/lsbx-lifecycle/Cargo.toml`
- `crates/lsbx-lifecycle/src/lib.rs`
- `crates/lsbx-lifecycle/src/create.rs`
- `crates/lsbx-lifecycle/src/reap.rs`
- `crates/lsbx-lifecycle/src/lease.rs`
- `crates/lsbx-lifecycle/tests/test_create_destroy.rs`
- `crates/lsbx-lifecycle/tests/test_reap.rs`
- `crates/lsbx-lifecycle/tests/test_lease_expiry.rs`

## Verification
```bash
cargo check -p lsbx-lifecycle --message-format=json
cargo clippy -p lsbx-lifecycle --all-targets --all-features -- -D warnings
cargo test -p lsbx-lifecycle --test test_create_destroy
cargo test -p lsbx-lifecycle --test test_reap
cargo test -p lsbx-lifecycle --test test_lease_expiry
```
Scenario: `test_reap` uses `DemoBackend::with_fault(FaultMode::PartialDestroyFailure)` and asserts a sandbox whose destroy call fails is NOT removed from the store — so it is retried on the next reap pass instead of silently forgotten.
