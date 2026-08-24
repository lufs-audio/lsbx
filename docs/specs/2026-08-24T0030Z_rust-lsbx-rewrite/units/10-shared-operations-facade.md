# Unit 10 — Shared Operations Façade

## Objective
Provide exactly one async function per logical operation — the mechanism that makes CLI/HTTP/MCP parity a structural property of the crate graph instead of a promise kept by convention across three independently-maintained implementations.

## Context
Layer 5, depends on Units 08 and 09. This crate is the waist of the architecture diagram in SPEC.md §3. Every door built in Layer 6 depends on this and only this — none of them may reach around it into `lsbx-lifecycle` or `lsbx-golden` directly.

## Acceptance criteria
- [ ] Every operation named in SPEC.md §4.7 (`create, destroy, list, exec, put, get, renew, console_url, info, status, reap, golden_build, golden_verify, golden_register, golden_delete, golden_list, config_show, logs_query`) has exactly one function here, taking a typed request and returning `Result<Response, LsbxError>`.
- [ ] No function here parses CLI args, HTTP bodies, or MCP tool-call JSON — all input arrives already typed; translating a door's native input format into these types is that door's job (Units 11, 13, 15), never this crate's.
- [ ] `LsbxOps` is constructed once (holding the chosen `Box<dyn Backend>`, `SandboxStore`, `CiJobStore`, `ImageRegistry`, and a `Clock`), and every door holds a reference to one shared instance — proof there's exactly one place operational state lives, not three.
- [ ] A test instantiates `LsbxOps` with `DemoBackend` and exercises every public method at least once, asserting `Ok` for a valid request and a specific `LsbxError` variant for an invalid one (e.g. `destroy` on an unknown id returns `NotFound`).
- [ ] The crate root doc comment states explicitly: no door may contain operational logic; a decision that changes backend behavior belongs in this crate (or the ones it composes), never inside a CLI/HTTP/MCP handler.

## Interface contract
```rust
// src/lib.rs
use lsbx_kernel::{backend::Backend, clock::Clock, error::LsbxError};
use lsbx_store::{SandboxStore, CiJobStore};
use lsbx_golden::registry::ImageRegistry;

pub struct LsbxOps {
    backend: Box<dyn Backend>,
    sandbox_store: SandboxStore,
    ci_job_store: CiJobStore,
    registry: ImageRegistry,
    clock: Box<dyn Clock>,
}

pub struct StatusReport {
    pub backend_name: String,
    pub backend_available: bool,
    pub sandbox_count: usize,
}

impl LsbxOps {
    pub fn new(
        backend: Box<dyn Backend>,
        sandbox_store: SandboxStore,
        ci_job_store: CiJobStore,
        registry: ImageRegistry,
        clock: Box<dyn Clock>,
    ) -> Self;

    pub async fn create(&self, req: lsbx_lifecycle::create::CreateRequest<'_>) -> Result<lsbx_kernel::types::PublicSandbox, LsbxError>;
    pub async fn destroy(&self, id: &str) -> Result<(), LsbxError>;
    pub async fn list(&self) -> Result<Vec<lsbx_kernel::types::PublicSandbox>, LsbxError>;
    pub async fn exec(&self, id: &str, command: &[String], timeout: std::time::Duration) -> Result<lsbx_kernel::backend::CommandOutput, LsbxError>;
    pub async fn put(&self, id: &str, source: &std::path::Path, destination: &str) -> Result<(), LsbxError>;
    pub async fn get(&self, id: &str, source: &str, destination: &std::path::Path) -> Result<(), LsbxError>;
    pub async fn renew(&self, id: &str, duration: std::time::Duration) -> Result<lsbx_kernel::types::PublicSandbox, LsbxError>;
    pub async fn console_url(&self, id: &str) -> Result<Option<String>, LsbxError>;
    pub async fn info(&self, id: &str) -> Result<lsbx_kernel::types::PublicSandbox, LsbxError>;
    pub async fn status(&self) -> Result<StatusReport, LsbxError>;
    pub async fn reap(&self, ttl: std::time::Duration, dry_run: bool) -> Result<lsbx_lifecycle::reap::ReapReport, LsbxError>;
    pub async fn golden_build(&self, req: lsbx_golden::build::GoldenBuildRequest<'_>) -> Result<lsbx_golden::registry::GoldenConfig, LsbxError>;
    pub async fn golden_verify(&self, name: &str) -> Result<Vec<lsbx_golden::verify::HealthcheckResult>, LsbxError>;
    pub async fn golden_register(&self, config: lsbx_golden::registry::GoldenConfig) -> Result<(), LsbxError>;
    pub async fn golden_delete(&self, name: &str, keep_snapshot: bool) -> Result<(), LsbxError>;
    pub async fn golden_list(&self) -> Result<Vec<lsbx_golden::registry::GoldenConfig>, LsbxError>;
    pub async fn config_show(&self) -> Result<serde_json::Value, LsbxError>;
    pub async fn logs_query(&self, since: Option<&str>, limit: usize) -> Result<Vec<String>, LsbxError>;
}
```

## Boundaries — do NOT touch
Contains no `clap`, `axum`, or `rmcp` dependency — this crate doesn't know any door exists. Implements no operation's actual logic beyond composing calls into `lsbx-lifecycle`/`lsbx-golden` — new logic goes in one of those crates, never here.

## Output
- `crates/lsbx-ops/Cargo.toml`
- `crates/lsbx-ops/src/lib.rs`
- `crates/lsbx-ops/tests/test_all_operations.rs`

## Verification
```bash
cargo check -p lsbx-ops --message-format=json
cargo clippy -p lsbx-ops --all-targets --all-features -- -D warnings
cargo test -p lsbx-ops --test test_all_operations
```
Scenario: `test_all_operations` calls every public method on `LsbxOps` at least once against a `DemoBackend`-backed instance, in one test file — so an operation added to the façade later without a corresponding test line is an obvious, reviewable diff, not a silent gap.
