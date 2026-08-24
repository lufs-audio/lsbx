# Unit 05 — Demo/Mock Backend

## Objective
Implement a fully in-memory, zero-real-infrastructure `Backend` so every later unit (golden, lifecycle, ops, all four doors, the broker) can be built and tested end-to-end before `libvirt` or `exedev` exist or are reachable from CI.

## Context
Layer 3, depends on Unit 01 (trait) and Unit 04 (must pass the conformance suite). Built deliberately first among the three backends — it's the dependency that unblocks nearly everything in Layers 4–7 from ever needing real infrastructure in CI.

## Acceptance criteria
- [ ] Passes `lsbx-backend-testkit::run_conformance_suite` unconditionally in normal `cargo test` — no `#[ignore]`.
- [ ] Deterministic: identical inputs (golden reference, name) produce an identical fake `vm_tag`/`host` (e.g. derived by hashing the inputs), not a random one — so tests asserting exact output values are reproducible.
- [ ] Ships fault-injection knobs (`DemoBackend::with_fault(FaultMode)`) simulating `BackendUnavailable`, a `run()` that never completes (for timeout-path tests), and a partially-failing `destroy` — so failure paths elsewhere in the workspace can be tested without needing a real backend to misbehave on cue.
- [ ] `capabilities()` reports `console: true` and produces a well-formed fake `https_url` on create, so the HTTP gateway and WS stream door units can be built and tested against it without real VNC infrastructure.
- [ ] Internally thread-safe for concurrent use from multiple async tasks, since the ops façade and the CI broker may both drive it concurrently in tests.

## Interface contract
```rust
// src/lib.rs
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    None,
    Unavailable,
    HangOnRun,
    PartialDestroyFailure,
}

pub struct DemoBackend {
    // internal: Arc<Mutex<HashMap<String, DemoVm>>>, fault: FaultMode
}

impl DemoBackend {
    pub fn new() -> Self;
    pub fn with_fault(mode: FaultMode) -> Self;
}

#[async_trait::async_trait]
impl Backend for DemoBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { console: true, remote_transport: false, snapshot: false }
    }
    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError>;
    async fn run(&self, vm_tag: &str, command: &[String], timeout: std::time::Duration) -> Result<CommandOutput, LsbxError>;
    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str) -> Result<(), LsbxError>;
    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path) -> Result<(), LsbxError>;
    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError>;
    async fn list_vms(&self) -> Result<Vec<String>, LsbxError>;
    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError>;
}
```

## Boundaries — do NOT touch
Never selected on a production path implicitly — `--backend demo` (or `LSBX_BACKEND=demo`) must be explicit. The `--backend auto` probing order (libvirt → exedev → demo fallback) is Unit 10/11's decision to implement; this unit only provides the leaf backend, never the fallback policy around it.

## Output
- `crates/lsbx-backend-demo/Cargo.toml`
- `crates/lsbx-backend-demo/src/lib.rs`
- `crates/lsbx-backend-demo/tests/test_conformance.rs`
- `crates/lsbx-backend-demo/tests/test_fault_modes.rs`

## Verification
```bash
cargo check -p lsbx-backend-demo --message-format=json
cargo clippy -p lsbx-backend-demo --all-targets --all-features -- -D warnings
cargo test -p lsbx-backend-demo --test test_conformance
cargo test -p lsbx-backend-demo --test test_fault_modes
```
Scenario: `cargo test -p lsbx-backend-demo deterministic_vm_tag` creates two independent `DemoBackend` instances, calls `create_from_golden` with identical inputs on each, and asserts identical `vm_tag` output.
