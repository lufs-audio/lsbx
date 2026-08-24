# Unit 01 — Kernel Domain Types & Exit Codes

## Objective
Define the shared substrate every other crate depends on and none of them redefines: domain types, the `Backend` trait, the `Clock` trait, the error taxonomy, exit codes, and the JSON envelope.

## Context
Layer 1 — the only unit with no internal dependencies. `SandboxRecord`'s fields must match the existing `lufs-audio/lufs-sandbox-server` Python schema exactly (see SPEC.md §4.1) so Unit 02's store and Unit 20's compatibility fixtures round-trip real on-disk data without a translation layer. The `Backend` trait defined here is what Units 05/06/07 implement and Unit 04's conformance kit tests against — this is the one interface in the whole workspace that is expensive to change later, because six other units are built against it.

## Acceptance criteria
- [ ] `SandboxRecord` serializes/deserializes to the exact existing JSON shape via serde, wrapped in a `{"schema_version":1,"kind":"sandbox","sandbox":{...}}` envelope matching the real on-disk format.
- [ ] A legacy flat (unversioned, unwrapped) `SandboxRecord` JSON sample deserializes successfully via `SandboxRecord::from_legacy_flat`, not a hard parse error.
- [ ] `SandboxRecord::public()` strips `key_path`, `key_dir`, `pubkey` while computing a `console_url` field that isn't stored on disk — matches the existing `Sandbox.public()` security property.
- [ ] `Backend` is object-safe (`Box<dyn Backend>` compiles) — the ops façade selects a backend at runtime from a CLI flag, not at compile time.
- [ ] `ExitCode` has exactly the 9 variants from SPEC.md §6; `impl From<ExitCode> for i32` matches the table exactly; no variant maps to `1`.
- [ ] `LsbxError` (via `thiserror`) has one variant per non-`Success` `ExitCode`, and a single `exit_code(&self) -> ExitCode` method — no call site anywhere else in the workspace hand-picks an exit code from an error string.
- [ ] `Envelope<T>` serializes to `{"status":"success","data":T}` or `{"status":"error","code":N,"message":"..."}`, where `code` is always `error.exit_code() as i32` — never a value that could disagree with the process's real exit status.
- [ ] `Clock` trait ships a `SystemClock` (real time); a `FakeClock` test double lives behind `#[cfg(test)]` or a `testing` feature, since Unit 09's lease-expiry tests need it too.
- [ ] Zero `unwrap()` / `expect()` / `panic!()` outside `#[cfg(test)]` code in this crate.

## Interface contract
```rust
// src/types.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecordEnvelope {
    pub schema_version: u32, // always 1
    pub kind: String,        // always "sandbox"
    pub sandbox: SandboxRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub id: String,
    pub name: String,
    pub host: String,
    pub profile: String,
    pub flavor: String,
    pub streaming: String, // "none" | "novnc"
    pub username: Option<String>,
    pub key_name: Option<String>,
    pub key_path: Option<String>,
    pub key_dir: Option<String>,
    pub pubkey: Option<String>,
    pub task_id: Option<String>,
    pub created_at: Option<String>,       // RFC3339
    pub lease_expires_at: Option<String>, // RFC3339
    pub vm_tag: Option<String>,
    pub https_url: Option<String>,
    pub cleanup_failed: bool,
    pub repository_key: Option<String>,
    pub repository: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl SandboxRecord {
    pub fn from_legacy_flat(value: serde_json::Value) -> Result<Self, crate::error::LsbxError>;
    pub fn public(&self) -> PublicSandbox;
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicSandbox {
    pub id: String,
    pub name: String,
    pub host: String,
    pub profile: String,
    pub flavor: String,
    pub streaming: String,
    pub task_id: Option<String>,
    pub created_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub console_url: Option<String>, // computed, never persisted
    pub cleanup_failed: bool,
    pub repository: Option<String>,
}

/// Validated against `^[a-z][a-z0-9._-]{0,63}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoldenKey(String);

/// Validated against `^[a-z][a-z0-9-]{0,63}$`; a trailing `.qcow2` is stripped before matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseKey(String);

// src/backend.rs
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub console: bool,
    pub remote_transport: bool,
    pub snapshot: bool,
}

pub struct CreateFromGoldenRequest<'a> {
    pub golden: &'a GoldenKey,
    pub name: &'a str,
    pub pubkey: &'a str,
    pub cpu: u32,
    pub memory: &'a str,
}

pub struct CreatedVm {
    pub vm_tag: String,
    pub host: String,
    pub https_url: Option<String>,
}

pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, crate::error::LsbxError>;
    async fn run(&self, vm_tag: &str, command: &[String], timeout: std::time::Duration) -> Result<CommandOutput, crate::error::LsbxError>;
    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str) -> Result<(), crate::error::LsbxError>;
    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path) -> Result<(), crate::error::LsbxError>;
    async fn destroy(&self, vm_tag: &str) -> Result<(), crate::error::LsbxError>;
    async fn list_vms(&self) -> Result<Vec<String>, crate::error::LsbxError>;
    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), crate::error::LsbxError>;
}

// src/clock.rs
pub trait Clock: Send + Sync {
    fn now(&self) -> std::time::SystemTime;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> std::time::SystemTime { std::time::SystemTime::now() }
}

// src/exit_code.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Usage = 2,
    BackendUnavailable = 3,
    NotFound = 4,
    ContractViolated = 5,
    LockContention = 6,
    AuthFailed = 7,
    Interrupted = 8,
}

// src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum LsbxError {
    #[error("usage: {0}")]
    Usage(String),
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("contract violated: {0}")]
    ContractViolated(String),
    #[error("lock contention: {0}")]
    LockContention(String),
    #[error("auth failed: {0}")]
    AuthFailed(String),
    #[error("interrupted: {0}")]
    Interrupted(String),
}

impl LsbxError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::Usage,
            Self::BackendUnavailable(_) => ExitCode::BackendUnavailable,
            Self::NotFound(_) => ExitCode::NotFound,
            Self::ContractViolated(_) => ExitCode::ContractViolated,
            Self::LockContention(_) => ExitCode::LockContention,
            Self::AuthFailed(_) => ExitCode::AuthFailed,
            Self::Interrupted(_) => ExitCode::Interrupted,
        }
    }
}

// src/envelope.rs
#[derive(serde::Serialize)]
#[serde(tag = "status")]
pub enum Envelope<T: serde::Serialize> {
    #[serde(rename = "success")]
    Success { data: T },
    #[serde(rename = "error")]
    Error { code: i32, message: String },
}
impl<T: serde::Serialize> Envelope<T> {
    pub fn from_result(r: Result<T, LsbxError>) -> Self {
        match r {
            Ok(data) => Self::Success { data },
            Err(e) => Self::Error { code: e.exit_code() as i32, message: e.to_string() },
        }
    }
}
```

## Boundaries — do NOT touch
No other unit defines or shadows `SandboxRecord`, `Backend`, `ExitCode`, `LsbxError`, or `Envelope` — every later unit imports these. Backend implementations (Units 05/06/07) reuse `LsbxError::BackendUnavailable(String)` and similar variants with their own message text; they never add new `LsbxError` variants. Unit 02 owns *persisting* `SandboxRecord` to disk; this unit only owns its shape.

## Output
- `crates/lsbx-kernel/Cargo.toml`
- `crates/lsbx-kernel/src/lib.rs`
- `crates/lsbx-kernel/src/types.rs`
- `crates/lsbx-kernel/src/backend.rs`
- `crates/lsbx-kernel/src/clock.rs`
- `crates/lsbx-kernel/src/exit_code.rs`
- `crates/lsbx-kernel/src/error.rs`
- `crates/lsbx-kernel/src/envelope.rs`
- `crates/lsbx-kernel/tests/test_kernel.rs`

## Verification
```bash
cargo check -p lsbx-kernel --message-format=json
cargo clippy -p lsbx-kernel --all-targets --all-features -- -D warnings
cargo test -p lsbx-kernel --test test_kernel
```
Scenario: `cargo test -p lsbx-kernel legacy_flat_migrates` must pass against a literal legacy-flat JSON sample (no `schema_version`/`kind` wrapper) shaped like the real existing state-store format, inlined in the test until Unit 20's fixtures land.
