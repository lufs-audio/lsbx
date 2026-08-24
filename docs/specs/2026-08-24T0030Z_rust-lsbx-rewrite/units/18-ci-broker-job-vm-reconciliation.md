# Unit 18 — CI Broker: Job↔VM Reconciliation

## Objective
Implement dispatch (create a VM for a queued job), runner-lifecycle tailing, divergence detection, and broker restart recovery — the third of the CI broker's three units.

## Context
Layer 7, depends on Unit 17 (poll results) and Unit 10 (`lsbx-ops`, to create the VM). Owns `src/reconcile.rs` and `src/job_record.rs` in the shared `lsbx-broker` crate.

## Acceptance criteria
- [ ] `dispatch(job)` calls `LsbxOps::create` with `profile: "ci"`, `task_id: job.job_id`, and the configured lease — the broker is just another `LsbxOps` caller with no special access path.
- [ ] Persists a `CiJobRecord` (Unit 02's `CiJobStore`) with `phase: "dispatched"` before returning, then tails runner lifecycle via `LsbxOps::exec` reading a log file (`/tmp/lsbx-ci-broker-runner.log`, existing convention preserved), parsing the same lifecycle markers as today: `Runner registered: (\S+)`, `Listening for Jobs`, `Running job: `, `Job (.+) completed with result: (\S+)`.
- [ ] Divergence detection cross-checks `github.job_for_runner(runner_name)` against the dispatched `job_id`, since GitHub assigns a runner to a job by label match, not by id — sets `CiJobRecord.diverged = true` and logs a warning; **never** treats divergence as fatal, matching existing behavior exactly.
- [ ] Broker restart recovery: `reconcile_on_startup()` calls `CiJobStore::list_in_flight()` and resumes tailing for every record whose `phase` isn't terminal, rather than starting with a blank slate and orphaning in-flight jobs.
- [ ] The broker's own process-level lock (`CiJobStore::broker_lock()`, Unit 02) is acquired via `try_acquire` at startup and held for the process lifetime; failing to acquire it exits with `LsbxError::LockContention` naming that another broker instance already owns this state directory — matches the existing `BrokerLock` fail-closed message.
- [ ] A forced-divergence test (mock GitHub returning a different `job_for_runner` than the dispatched `job_id`) asserts `diverged` becomes `true` and the process keeps running rather than erroring out.

## Interface contract
```rust
// src/reconcile.rs
use lsbx_kernel::error::LsbxError;
use lsbx_store::ci_job_store::{CiJobStore, CiJobRecord};
use super::poll::QueuedJob;

pub struct Reconciler<'a> {
    ops: &'a lsbx_ops::LsbxOps,
    job_store: &'a CiJobStore,
    github: &'a super::github_client::GitHubClient,
}

impl<'a> Reconciler<'a> {
    pub fn new(ops: &'a lsbx_ops::LsbxOps, job_store: &'a CiJobStore, github: &'a super::github_client::GitHubClient) -> Self;

    pub async fn dispatch(&self, job: &QueuedJob, lease: std::time::Duration) -> Result<CiJobRecord, LsbxError>;

    /// Tails the runner log via LsbxOps::exec, parses lifecycle markers, updates `phase`.
    pub async fn tail_and_update(&self, record: &mut CiJobRecord) -> Result<(), LsbxError>;

    /// Cross-checks GitHub's actual runner->job assignment against the dispatched job_id.
    pub async fn check_divergence(&self, record: &mut CiJobRecord) -> Result<(), LsbxError>;

    /// Called once at broker startup; resumes tailing every non-terminal in-flight record.
    pub async fn reconcile_on_startup(&self) -> Result<Vec<CiJobRecord>, LsbxError>;
}

// src/job_record.rs — lifecycle-marker regexes and phase-transition helpers, kept
// separate from Reconciler's control flow so the parsing logic is independently testable.
pub struct LifecycleMarkers;
impl LifecycleMarkers {
    pub fn parse_runner_registered(line: &str) -> Option<String>;
    pub fn parse_job_completed(line: &str) -> Option<(String, String)>; // (job_name, result)
}
```

## Boundaries — do NOT touch
Does not implement polling or eligibility (Unit 17). Does not implement GitHub auth (Unit 16). Does not implement VM creation itself — calls `LsbxOps::create`/`exec`, exactly like the CLI or HTTP door would.

## Output
- `crates/lsbx-broker/src/reconcile.rs`
- `crates/lsbx-broker/src/job_record.rs`
- `crates/lsbx-broker/tests/test_dispatch_and_tail.rs`
- `crates/lsbx-broker/tests/test_divergence_nonfatal.rs`
- `crates/lsbx-broker/tests/test_restart_recovery.rs`

## Verification
```bash
cargo check -p lsbx-broker --message-format=json
cargo clippy -p lsbx-broker --all-targets --all-features -- -D warnings
cargo test -p lsbx-broker --test test_dispatch_and_tail
cargo test -p lsbx-broker --test test_divergence_nonfatal
cargo test -p lsbx-broker --test test_restart_recovery
```
Scenario: `test_restart_recovery` seeds a `CiJobStore` with a `phase: "running"` record (simulating a broker that crashed mid-job), constructs a fresh `Reconciler`, calls `reconcile_on_startup()`, and asserts the record is picked back up for tailing rather than ignored.
