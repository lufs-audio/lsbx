# Unit 02 — Atomic State Store & Lock Sentinels

## Objective
Implement the atomic, file-per-record state stores for sandboxes and CI jobs, and the one lock-sentinel primitive every process-level lock in the system is built on — replacing both the current thread-`RLock`-only sandbox store and the CI broker's separately-invented `flock`.

## Context
Layer 2, depends only on Unit 01. The existing system has two independently-invented locking strategies: none at all on the sandbox store (thread `RLock`, single-process assumption) and a real `flock` on the CI broker only (`BrokerLock`). That split is itself the evidence this needed one correct primitive from the start, not two. The classic failure this must avoid: process A `flock`s path P; something unlinks P and a new file appears at the same path; a late process opens the "new" P and gets an uncontended lock while A still (incorrectly) believes it holds exclusivity. Fix: never unlink a lock file while it might be raced, and after acquiring, `fstat` the held fd and `stat` the path fresh — if `(dev, ino)` disagree, someone recreated the path underneath you; reopen and retry.

## Acceptance criteria
- [ ] `LockSentinel::acquire(path)` detects the flock-unlink race: after `flock(LOCK_EX)` succeeds, it compares the held fd's `fstat` against a fresh `stat` of the path by `(dev, ino)`; on mismatch it reopens and retries rather than returning a false success.
- [ ] `LockSentinel::try_acquire(path)` is the non-blocking form (`LOCK_EX | LOCK_NB`) and returns `LsbxError::LockContention` immediately on contention — this is what the broker lock uses, matching the existing `BrokerLock`'s fail-closed behavior.
- [ ] Dropping a `LockGuard` never unlinks the lock file — lock files are permanent sentinels once created, precisely so the fstat/stat comparison stays meaningful for the next acquirer.
- [ ] `SandboxStore` persists one JSON file per sandbox at `<state_dir>/state/<id>.json`, mode 0600, parent directory mode 0700, atomic write via temp-file + `rename`.
- [ ] `SandboxStore::load(id)` transparently migrates a legacy flat record (via Unit 01's `SandboxRecord::from_legacy_flat`) — the caller never needs to know which shape was on disk.
- [ ] `CiJobStore` persists one JSON file per job at `<state_dir>/ci-broker/<job_id>.json`, same atomicity guarantees, schema `{"schema_version":1,"kind":"ci-job","job": CiJobRecord}`.
- [ ] `CiJobStore::broker_lock()` (`<state_dir>/ci-broker.lock`) is built from `LockSentinel::try_acquire` — not a second hand-rolled mechanism. This is the point of the unit.
- [ ] A concurrency test proves a second `try_acquire` fails with `LockContention` while the first guard is held, and succeeds immediately after the first is dropped.

## Interface contract
```rust
// src/lock.rs
use std::path::{Path, PathBuf};
use lsbx_kernel::error::LsbxError;

pub struct LockGuard {
    _file: std::fs::File, // flock releases on drop (fd close); the file itself is never unlinked
    path: PathBuf,
}

pub struct LockSentinel;

impl LockSentinel {
    /// Blocking acquire. Retries internally if an unlink-and-recreate race is detected.
    pub fn acquire(path: &Path) -> Result<LockGuard, LsbxError>;

    /// Non-blocking acquire. Returns `LsbxError::LockContention` immediately if held elsewhere.
    pub fn try_acquire(path: &Path) -> Result<LockGuard, LsbxError>;
}

// src/sandbox_store.rs
use lsbx_kernel::types::SandboxRecord;

pub struct SandboxStore {
    state_dir: PathBuf,
}

impl SandboxStore {
    pub fn new(state_dir: PathBuf) -> Self;
    pub fn save(&self, record: &SandboxRecord) -> Result<(), LsbxError>;
    pub fn load(&self, id: &str) -> Result<SandboxRecord, LsbxError>; // LsbxError::NotFound if absent
    pub fn delete(&self, id: &str) -> Result<(), LsbxError>;
    pub fn list(&self) -> Result<Vec<SandboxRecord>, LsbxError>;
}

// src/ci_job_store.rs
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CiJobRecord {
    pub job_id: String,
    pub queue_label: String,
    pub runner_group: Option<String>,
    pub host_prefix: Option<String>,
    pub phase: String, // "dispatched" | "running" | "completed" | "failed"
    pub sandbox_id: Option<String>,
    pub runner_name: Option<String>,
    pub dispatched_job_name: Option<String>,
    pub actual_job_id: Option<String>,
    pub actual_job_name: Option<String>,
    pub diverged: bool,
    pub repository: String,
    pub created_at: String,
    pub updated_at: String,
    pub lease_expires_at: Option<String>,
    pub last_error: Option<String>,
}

pub struct CiJobStore {
    state_dir: PathBuf,
}

impl CiJobStore {
    pub fn new(state_dir: PathBuf) -> Self;
    pub fn save(&self, record: &CiJobRecord) -> Result<(), LsbxError>;
    pub fn load(&self, job_id: &str) -> Result<CiJobRecord, LsbxError>;
    pub fn list_in_flight(&self) -> Result<Vec<CiJobRecord>, LsbxError>; // phase not in {completed, failed}
    pub fn broker_lock(&self) -> Result<LockGuard, LsbxError>; // <state_dir>/ci-broker.lock
}
```

## Boundaries — do NOT touch
Does not define `SandboxRecord` or its field shape (Unit 01 owns that; this unit owns persistence only). Does not decide what `CiJobRecord.phase` transitions mean or when `diverged` gets set (Unit 18 owns reconciliation semantics — this unit only guarantees the record round-trips to disk atomically). Does not implement reap TTL policy (Unit 09 owns that; this unit only exposes `list()`).

## Output
- `crates/lsbx-store/Cargo.toml`
- `crates/lsbx-store/src/lib.rs`
- `crates/lsbx-store/src/lock.rs`
- `crates/lsbx-store/src/sandbox_store.rs`
- `crates/lsbx-store/src/ci_job_store.rs`
- `crates/lsbx-store/tests/test_lock.rs`
- `crates/lsbx-store/tests/test_sandbox_store.rs`
- `crates/lsbx-store/tests/test_ci_job_store.rs`

## Verification
```bash
cargo check -p lsbx-store --message-format=json
cargo clippy -p lsbx-store --all-targets --all-features -- -D warnings
cargo test -p lsbx-store --test test_lock
cargo test -p lsbx-store --test test_sandbox_store
cargo test -p lsbx-store --test test_ci_job_store
```
Scenario: `cargo test -p lsbx-store race_unlink_recreate_detected` must actively unlink-and-recreate the lock path mid-hold from a second thread, and assert the comparison catches it rather than silently succeeding.
