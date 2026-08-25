//! Job <-> VM reconciliation (Unit 18): dispatch, runner-lifecycle tailing,
//! divergence detection, and broker restart recovery — the third of the CI
//! broker's three units, built on top of Unit 17's poll results
//! (`super::poll::QueuedJob`) and Unit 10's `lsbx-ops` (to create/exec
//! against the VM).
//!
//! # Ground truth this file was written against (read directly, not assumed)
//!
//! Per this task's own instructions, every signature below was confirmed by
//! reading the real, already-merged/already-open-PR source immediately
//! before writing this file:
//!
//! - `lsbx_kernel::error::LsbxError` (`crates/lsbx-kernel/src/error.rs` on
//!   `main`) is a closed 7-variant enum: `Usage, BackendUnavailable,
//!   NotFound, ContractViolated, LockContention, AuthFailed, Interrupted`.
//!   No `Internal`/`Api`/`Other` variant exists. Every error this file
//!   surfaces is mapped onto one of these seven, and unmapped cases become
//!   `ContractViolated` per the house convention every other merged unit in
//!   this workspace already follows (see e.g. `lsbx-store`'s
//!   `ci_job_store.rs`/`lock.rs`, `lsbx-golden`'s `registry.rs`).
//! - `lsbx_store::ci_job_store::CiJobRecord` (`crates/lsbx-store/src/ci_job_store.rs`
//!   on `main`) already carries every field this unit needs — no schema
//!   migration, and this file never adds or renames a field on it. Notably
//!   `CiJobRecord.job_id` is a `String`, while
//!   `super::poll::QueuedJob.job_id` (Unit 17) is a `u64` — every
//!   `QueuedJob.job_id` this file touches goes through an explicit
//!   `.to_string()` before it reaches a `CiJobRecord`, never an implicit or
//!   inferred conversion.
//! - `lsbx_store::ci_job_store::CiJobStore::broker_lock()` really does build
//!   on `LockSentinel::try_acquire` (`crates/lsbx-store/src/lock.rs`), which
//!   really does return
//!   `LsbxError::LockContention(format!("lock held elsewhere: {}", path.display()))`
//!   on contention — confirmed by direct read, not assumed. This file's own
//!   `run_broker` entry point (see below) relies on that exact behavior
//!   rather than re-wrapping it.
//! - `lsbx_ops::LsbxOps::new` takes six arguments in this exact order —
//!   `(backend, backend_name, sandbox_store, ci_job_store, registry,
//!   clock)` — confirmed against the real, merged `crates/lsbx-ops/src/lib.rs`.
//!   `LsbxOps::create` takes `lsbx_lifecycle::create::CreateRequest<'_>` and
//!   returns `Result<PublicSandbox, LsbxError>`; `LsbxOps::exec` takes
//!   `(id: &str, command: &[String], timeout: Duration)` and returns
//!   `Result<lsbx_kernel::backend::CommandOutput, LsbxError>`
//!   (`CommandOutput { exit_code: i32, stdout: Vec<u8>, stderr: Vec<u8> }`).
//!   `CreateRequest.healthchecks` is `Vec<Vec<String>>` (confirmed by direct
//!   read of `crates/lsbx-lifecycle/src/create.rs` — each element is one
//!   command's argv, not a single flat `Vec<String>`), which is why
//!   `dispatch` below passes `vec![]` rather than a bare empty
//!   `Vec::<String>::new()`.
//!
//! # Gap 1 — `check_divergence`'s run/job lookup
//!
//! See `github_client.rs`'s "Unit 18 addition" doc comment for the full
//! design writeup. Summary: `GitHubClient::job_for_runner(repo, runner_name)`
//! answers "what job is GitHub's ground truth currently assigning to this
//! runner" by scanning `repo`'s queued+in-progress runs' jobs for a
//! `runner_name` match — it needs only `repo` and `runner_name`, not the
//! original dispatch's `run_id`, so `CiJobRecord`'s real schema (which has
//! no `run_id` field, and which this unit was told not to modify) needed no
//! change and no field got overloaded with a second, undocumented meaning.
//! `dispatch` stores `record.repository` (already on the real schema) and,
//! once `tail_and_update` learns the runner's name from the tailed log,
//! `record.runner_name` (also already on the real schema) — together those
//! two are exactly what `check_divergence` needs to call
//! `job_for_runner(&record.repository, runner_name)`.
//!
//! # Gap 5 — the broker's own entry point and process-level lock
//!
//! No `lsbx-broker` binary or `main`/entry-point function exists anywhere in
//! the merged workspace as of Unit 17 (confirmed: `crates/lsbx-broker/src/lib.rs`
//! is `pub mod auth; mod error_map; pub mod github_client; pub mod labels;
//! pub mod poll;` — module wiring only, no `run`/`main` of any kind). The
//! acceptance criteria describe real startup behavior ("the broker's own
//! process-level lock... is acquired via `try_acquire` at startup and held
//! for the process lifetime; failing to acquire it exits with
//! `LsbxError::LockContention`...") that has to live somewhere concrete, the
//! same kind of "acceptance criteria describes behavior the interface
//! contract doesn't fully name" gap Unit 17 already flagged and filled with
//! its own `Poller` type (see `poll.rs`'s module doc comment). [`run_broker`]
//! below fills the equivalent gap for this unit: it acquires
//! `CiJobStore::broker_lock()` once, holds the returned `LockGuard` for its
//! own async-fn-stack-frame lifetime (which *is* "the process lifetime" for
//! a broker whose entire job is to run this one loop — there is no other
//! concurrent responsibility in this crate that would need the lock
//! released earlier), calls `reconcile_on_startup()` to resume any
//! in-flight jobs a prior crash left behind, then drives `Poller::tick` and
//! `Reconciler::dispatch`/`tail_and_update`/`check_divergence` in one loop.
//! A `LockContention` from `broker_lock()` propagates directly (already
//! naming the state directory's lock path via `lock.rs`'s own message — see
//! above), matching "matches the existing `BrokerLock` fail-closed message"
//! without this file re-wrapping or restating it.
//!
//! Divergence is never fatal anywhere in this chain: `check_divergence`
//! itself never returns `Err` for a mismatch (only for an actual GitHub-call
//! failure, mapped through the normal error taxonomy), and `run_broker`'s
//! loop treats a `check_divergence` call the same as any other per-job
//! reconciliation step — an error from it is logged and that job's own
//! iteration is skipped, never propagated up to tear down the whole loop or
//! release the broker lock. See `test_divergence_nonfatal.rs`.

use lsbx_kernel::error::LsbxError;
use lsbx_store::ci_job_store::{CiJobRecord, CiJobStore};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::{Builder as TempDirBuilder, TempDir};
use tracing::{info, warn};

use super::github_client::GitHubClient;
use super::job_record::LifecycleMarkers;
use super::poll::QueuedJob;

/// Path convention preserved from the existing Python broker (`ci_broker.py`)
/// — every dispatched CI runner VM writes its lifecycle log here.
pub const RUNNER_LOG_PATH: &str = "/tmp/lsbx-ci-broker-runner.log";

/// Profile every CI-dispatched sandbox is created under. Fixed, not
/// configurable per job — the acceptance criterion names it literally
/// (`profile: "ci"`).
const CI_PROFILE: &str = "ci";

/// Bounds the `LsbxOps::exec` call `tail_and_update` uses to read the runner
/// log. A bounded `timeout` on the exec call itself — not an unbounded
/// stream — is the mechanism that keeps a growing log from hanging this
/// call forever; see the module doc comment and this unit's own acceptance
/// criteria for why a `cat`-and-return snapshot (never a `tail -f`-style
/// unbounded follow) is the right shape here.
const TAIL_EXEC_TIMEOUT: Duration = Duration::from_secs(10);

/// Terminal `CiJobRecord.phase` values. `CiJobStore::list_in_flight` already
/// filters these out on the read side (`lsbx-store`'s own convention); this
/// unit checks the same two strings on the write side when deciding whether
/// a `tail_and_update` transition just reached one, so `reconcile_on_startup`
/// and a live tailing loop agree on exactly what "terminal" means.
const PHASE_COMPLETED: &str = "completed";
const PHASE_FAILED: &str = "failed";

/// Job<->VM reconciler: dispatch, tail, and divergence-check CI jobs against
/// real `LsbxOps`/`CiJobStore`/`GitHubClient` instances, exactly matching
/// this unit's own interface contract's struct shape and lifetimes.
pub struct Reconciler<'a> {
    ops: &'a lsbx_ops::LsbxOps,
    job_store: &'a CiJobStore,
    github: &'a GitHubClient,
}

impl<'a> Reconciler<'a> {
    pub fn new(
        ops: &'a lsbx_ops::LsbxOps,
        job_store: &'a CiJobStore,
        github: &'a GitHubClient,
    ) -> Self {
        Self {
            ops,
            job_store,
            github,
        }
    }

    /// Calls `LsbxOps::create` with `profile: "ci"`, `task_id: job.job_id`,
    /// and the configured `lease` — the broker is just another `LsbxOps`
    /// caller with no special access path, per the acceptance criterion's
    /// own wording. Persists a `CiJobRecord` with `phase: "dispatched"`
    /// *before* returning, matching `lsbx_lifecycle::create::create`'s own
    /// durability-before-ack discipline one layer up.
    ///
    /// `req.verify` is `true` and `req.healthchecks` is empty (`vec![]`):
    /// this unit does not own golden-specific healthchecks (that is Unit
    /// 08/09's concern, reached through `lsbx-ops::create` the same way any
    /// other caller would resolve them) — an empty list still proves
    /// readiness via the weaker-but-real "one trivial command succeeded"
    /// signal `lsbx_lifecycle::create::create` already implements for that
    /// case (see that function's own doc comment). `ready_timeout` is fixed
    /// at a conservative bound rather than threaded through the interface
    /// contract's `dispatch(job, lease)` signature, which has no readiness-
    /// timeout parameter to accept one.
    pub async fn dispatch(
        &self,
        job: &QueuedJob,
        lease: Duration,
    ) -> Result<CiJobRecord, LsbxError> {
        let job_id_str = job.job_id.to_string();
        let sandbox = self
            .ops
            .create(lsbx_lifecycle::create::CreateRequest {
                profile: CI_PROFILE,
                golden: None,
                cpu: None,
                memory: None,
                flavor: None,
                streaming: None,
                name: None,
                task_id: Some(job_id_str.as_str()),
                lease,
                ready_timeout: Duration::from_secs(120),
                verify: true,
                healthchecks: vec![],
            })
            .await?;

        let now = now_rfc3339();
        let lease_expires_at = rfc3339_plus(lease);

        let record = CiJobRecord {
            job_id: job_id_str,
            queue_label: job.labels.first().cloned().unwrap_or_default(),
            runner_group: None,
            host_prefix: None,
            phase: "dispatched".to_string(),
            sandbox_id: Some(sandbox.id.clone()),
            runner_name: None,
            dispatched_job_name: job.name.clone(),
            actual_job_id: None,
            actual_job_name: None,
            diverged: false,
            repository: job.repository.clone(),
            created_at: now.clone(),
            updated_at: now,
            lease_expires_at: Some(lease_expires_at),
            last_error: None,
        };

        self.job_store.save(&record)?;
        Ok(record)
    }

    /// Tails the runner log via `LsbxOps::exec`, parses lifecycle markers
    /// with `LifecycleMarkers`, and updates `phase`/`runner_name`/
    /// `actual_job_name`, persisting via `CiJobStore::save` after each
    /// meaningful transition — not only once at the end — so
    /// `reconcile_on_startup` can resume mid-sequence after a crash (this
    /// unit's own restart-recovery acceptance criterion).
    ///
    /// Reads the log as one bounded snapshot
    /// (`exec(sandbox_id, ["cat", RUNNER_LOG_PATH], TAIL_EXEC_TIMEOUT)`) —
    /// never an unbounded follow/stream — and re-applies every marker found
    /// against the *whole* snapshot each call. This is deliberately
    /// idempotent re-application, not "only look at new lines since last
    /// call": `record.phase`/`runner_name`/`actual_job_name` end up as
    /// whatever the *last* matching marker in the snapshot says, which is
    /// correct whether this is the first tail after dispatch or a resumed
    /// tail after `reconcile_on_startup` picked the record back up with no
    /// memory of how much of the log a now-dead prior process had already
    /// consumed.
    ///
    /// A sandbox that has no `sandbox_id` (should not happen for a record
    /// this function is ever called on — `dispatch` always sets it before
    /// `phase` can be anything past `"dispatched"`) is a contract violation
    /// this function surfaces rather than silently no-ops through.
    pub async fn tail_and_update(&self, record: &mut CiJobRecord) -> Result<(), LsbxError> {
        let sandbox_id = record.sandbox_id.clone().ok_or_else(|| {
            LsbxError::ContractViolated(format!(
                "ci job {} has no sandbox_id to tail (never fully dispatched)",
                record.job_id
            ))
        })?;

        let output = self
            .ops
            .exec(
                &sandbox_id,
                &["cat".to_string(), RUNNER_LOG_PATH.to_string()],
                TAIL_EXEC_TIMEOUT,
            )
            .await?;

        if output.exit_code != 0 {
            return Err(LsbxError::BackendUnavailable(format!(
                "runner log read failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let log_text = String::from_utf8_lossy(&output.stdout);

        let mut changed = false;

        for line in log_text.lines() {
            if let Some(runner_name) = LifecycleMarkers::parse_runner_registered(line) {
                if record.runner_name.as_deref() != Some(runner_name.as_str()) {
                    record.runner_name = Some(runner_name);
                    changed = true;
                }
                if record.phase != "running"
                    && record.phase != PHASE_COMPLETED
                    && record.phase != PHASE_FAILED
                {
                    record.phase = "registered".to_string();
                    changed = true;
                }
            }

            if LifecycleMarkers::is_listening_for_jobs(line)
                && record.phase != PHASE_COMPLETED
                && record.phase != PHASE_FAILED
                && record.phase != "listening"
            {
                record.phase = "listening".to_string();
                changed = true;
            }

            if LifecycleMarkers::is_running_job(line)
                && record.phase != PHASE_COMPLETED
                && record.phase != PHASE_FAILED
                && record.phase != "running"
            {
                record.phase = "running".to_string();
                changed = true;
            }

            if let Some(running_job) = LifecycleMarkers::parse_running_job(line) {
                if record.actual_job_name.as_deref() != Some(running_job.as_str()) {
                    record.actual_job_name = Some(running_job);
                    changed = true;
                }
                if record.phase != PHASE_COMPLETED
                    && record.phase != PHASE_FAILED
                    && record.phase != "running"
                {
                    record.phase = "running".to_string();
                    changed = true;
                }
            }

            if let Some((job_name, result)) = LifecycleMarkers::parse_job_completed(line) {
                if record.actual_job_name.as_deref() != Some(job_name.as_str()) {
                    record.actual_job_name = Some(job_name.clone());
                    changed = true;
                }
                let terminal_phase = if result.eq_ignore_ascii_case("succeeded") {
                    PHASE_COMPLETED
                } else {
                    PHASE_FAILED
                };
                if record.phase != terminal_phase {
                    record.phase = terminal_phase.to_string();
                    changed = true;
                }
                if record.last_error.is_none() && terminal_phase == PHASE_FAILED {
                    record.last_error =
                        Some(format!("job {job_name} completed with result: {result}"));
                    changed = true;
                }
            }
        }

        if changed {
            record.updated_at = now_rfc3339();
            self.job_store.save(record)?;
        }

        Ok(())
    }

    /// Cross-checks GitHub's actual runner->job assignment
    /// (`GitHubClient::job_for_runner`) against the dispatched `job_id`,
    /// since GitHub assigns a runner to a job by label match, not by any id
    /// `lsbx` controls. Sets `CiJobRecord.diverged = true` and logs a
    /// warning on a mismatch; **never** treats divergence as fatal — this
    /// function's `Result::Err` path is reserved for an actual failure to
    /// reach GitHub (mapped through `GitHubClient`'s own error taxonomy),
    /// never for the divergence finding itself. See the module doc comment
    /// ("Gap 1") for why this looks up by `(repository, runner_name)`
    /// rather than needing a stored `run_id`.
    ///
    /// A no-op (returns `Ok(())` without calling GitHub) if `runner_name` is
    /// not yet known — there is nothing to cross-check divergence against
    /// until `tail_and_update` has observed a `Runner registered: (\S+)`
    /// line, and calling `job_for_runner` with no runner name to match would
    /// not answer any real question.
    pub async fn check_divergence(&self, record: &mut CiJobRecord) -> Result<(), LsbxError> {
        let Some(runner_name) = record.runner_name.clone() else {
            return Ok(());
        };

        let actual_job_id = self
            .github
            .job_for_runner(&record.repository, &runner_name)
            .await?;

        let dispatched_job_id: Option<u64> = record.job_id.parse().ok();

        let diverged = match (actual_job_id, dispatched_job_id) {
            (Some(actual), Some(dispatched)) => actual != dispatched,
            // GitHub hasn't attached this runner to any queued/in-progress
            // job yet (`None`), or the stored `job_id` isn't parseable as a
            // `u64` for some reason (should not happen — `dispatch` always
            // writes `QueuedJob.job_id.to_string()` — but treated as "cannot
            // prove divergence, so don't claim it" rather than papering over
            // it as a false negative or a hard error, either of which would
            // be a worse failure mode than simply not asserting divergence
            // yet).
            _ => false,
        };

        if diverged {
            warn!(
                job_id = record.job_id.as_str(),
                runner_name = runner_name.as_str(),
                actual_job_id = ?actual_job_id,
                "CI runner diverged: GitHub assigned this runner to a different job than lsbx dispatched it for \
                 (divergence is logged, not fatal — the dispatched VM keeps running)"
            );
        }

        let actual_job_id_value = actual_job_id.map(|id| id.to_string());
        if record.diverged != diverged || record.actual_job_id != actual_job_id_value {
            record.diverged = diverged;
            record.actual_job_id = actual_job_id_value;
            record.updated_at = now_rfc3339();
            self.job_store.save(record)?;
        }

        Ok(())
    }

    /// Called once at broker startup: `CiJobStore::list_in_flight()`, then
    /// actually resumes tailing (`tail_and_update`) for every record whose
    /// `phase` isn't terminal — this unit's own acceptance criterion is
    /// explicit that such a record "must be picked back up for tailing,"
    /// not merely enumerated. Returns the (possibly updated) records after
    /// one tail-and-update pass on each, so a caller can inspect what
    /// resuming actually observed.
    ///
    /// A `tail_and_update` failure for one record (e.g. its sandbox no
    /// longer exists — the VM could have been reaped, or the backend could
    /// be down) is recorded onto that record's own `last_error` and does not
    /// abort the whole recovery sweep: every other in-flight record still
    /// gets its own resume attempt. This mirrors this file's broader
    /// "divergence/per-job failure is never allowed to propagate up and take
    /// down the whole reconciliation process" stance (see the module doc
    /// comment's "Divergence is never fatal" note) — a startup sweep is
    /// exactly the place a single stale record must not block recovery of
    /// every other one.
    pub async fn reconcile_on_startup(&self) -> Result<Vec<CiJobRecord>, LsbxError> {
        let mut records = self.job_store.list_in_flight()?;

        for record in &mut records {
            info!(
                job_id = record.job_id.as_str(),
                phase = record.phase.as_str(),
                "resuming in-flight CI job after broker restart"
            );
            if let Err(e) = self.tail_and_update(record).await {
                warn!(
                    job_id = record.job_id.as_str(),
                    error = %e,
                    "failed to resume tailing for in-flight CI job during startup recovery; \
                     continuing with the remaining in-flight jobs"
                );
                record.last_error =
                    Some(format!("resume-tail failed during startup recovery: {e}"));
                record.updated_at = now_rfc3339();
                // Best-effort persistence of the failure note; if even this
                // save fails, the in-memory record returned to the caller
                // still carries the error, so nothing is silently lost from
                // this call's own return value.
                let _ = self.job_store.save(record);
            }
        }

        Ok(records)
    }
}

fn now_rfc3339() -> String {
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    now.to_rfc3339()
}

fn rfc3339_plus(duration: Duration) -> String {
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    (now + duration).to_rfc3339()
}

/// Configuration for [`run_broker`] — the parameters a real deployment needs
/// to supply that neither `Poller`/`Reconciler` construction alone captures
/// (how long to sleep between iterations, and the lease every dispatched CI
/// sandbox gets).
pub struct BrokerConfig {
    pub poll: super::poll::PollConfig,
    pub lease: Duration,
    /// `Some` enables full Python-equivalent guest runner provisioning;
    /// `None` retains the lower-level monitor-only behavior used by unit tests.
    pub runner: Option<RunnerConfig>,
}

/// Backend-neutral runner provisioning policy. Transport remains entirely in
/// `LsbxOps`; this struct only describes the guest-side Actions runner.
#[derive(Clone)]
pub struct RunnerConfig {
    pub app_id: u64,
    pub app_key_path: PathBuf,
    pub owner: String,
    pub scope: String,
    pub labels: String,
    pub group: Option<String>,
    pub runner_user: String,
    pub runner_dir: String,
    pub host_prefix: String,
    pub provision_script: PathBuf,
    pub runner_wait_timeout: Duration,
    pub job_timeout: Duration,
}

impl RunnerConfig {
    /// Loads both the new `LSBX_*` names and the Python broker's legacy names.
    /// The private key is required: a runner without App authentication cannot
    /// safely be registered by this broker.
    pub fn from_env(backend: &str, queue_label: &str) -> Result<Self, LsbxError> {
        let file_values = std::env::var("LSBX_CI_RUNNER_ENV_FILE")
            .or_else(|_| std::env::var("RUNNER_ENV_FILE"))
            .ok()
            .map(|path| load_simple_env_file(Path::new(&path)))
            .transpose()?;
        let file_values = file_values.unwrap_or_default();

        let value = |new_name: &str, old_name: &str, default: Option<&str>| {
            std::env::var(new_name)
                .or_else(|_| std::env::var(old_name))
                .ok()
                .or_else(|| file_values.get(new_name).cloned())
                .or_else(|| file_values.get(old_name).cloned())
                .or_else(|| default.map(str::to_string))
        };

        let app_id_value = value("LSBX_GITHUB_APP_ID", "GITHUB_APP_ID", None)
            .ok_or_else(|| LsbxError::AuthFailed("GitHub App id is not configured".to_string()))?;
        let app_id = app_id_value.parse::<u64>().map_err(|_| {
            LsbxError::Usage(format!("GitHub App id '{app_id_value}' is not a valid u64"))
        })?;
        let app_key_path = PathBuf::from(
            value("LSBX_GITHUB_APP_PRIVATE_KEY_PATH", "GITHUB_APP_KEY", None).ok_or_else(|| {
                LsbxError::AuthFailed("GitHub App private key path is not configured".to_string())
            })?,
        );
        if !app_key_path.is_file() {
            return Err(LsbxError::AuthFailed(format!(
                "GitHub App private key does not exist: {}",
                app_key_path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&app_key_path)
                .map_err(|e| {
                    LsbxError::AuthFailed(format!("cannot stat GitHub App private key: {e}"))
                })?
                .permissions()
                .mode()
                & 0o777;
            if mode != 0o600 {
                return Err(LsbxError::AuthFailed(format!(
                    "GitHub App private key must be mode 0600: {} (mode {mode:o})",
                    app_key_path.display()
                )));
            }
        }

        let owner = value("LSBX_GITHUB_APP_OWNER", "GITHUB_OWNER", None)
            .ok_or_else(|| LsbxError::Usage("GitHub App owner is not configured".to_string()))?;
        let scope = value("LSBX_CI_RUNNER_SCOPE", "GITHUB_SCOPE", Some("org"))
            .unwrap_or_else(|| "org".to_string());
        if scope != "org" && scope != "repo" {
            return Err(LsbxError::Usage(
                "runner scope must be 'org' or 'repo'".to_string(),
            ));
        }
        let group_default = if backend == "libvirt" {
            "continuo"
        } else {
            "exe"
        };
        let labels = value("LSBX_CI_RUNNER_LABELS", "RUNNER_LABELS", Some(queue_label))
            .unwrap_or_else(|| queue_label.to_string());
        let group = value("LSBX_CI_RUNNER_GROUP", "RUNNER_GROUP", Some(group_default));
        let host_prefix = value(
            "LSBX_CI_RUNNER_HOST_PREFIX",
            "LUFSS_VM_PREFIX",
            Some(if backend == "libvirt" {
                "lsbx-carnyx-"
            } else {
                "lsbx-molimo-"
            }),
        )
        .unwrap_or_default()
        .trim_start_matches("lsbx-")
        .trim_end_matches('-')
        .to_string();
        let provision_script = std::env::var("LSBX_CI_PROVISION_SCRIPT")
            .or_else(|_| std::env::var("RUNNER_SCRIPT"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("scripts/provision-ci-runner.sh"));
        let runner_wait_timeout = duration_env("LSBX_CI_RUNNER_WAIT", 240);
        let job_timeout = duration_env("LSBX_CI_JOB_TIMEOUT", 3600);

        Ok(Self {
            app_id,
            app_key_path,
            owner,
            scope,
            labels,
            group,
            runner_user: value("LSBX_CI_RUNNER_USER", "RUNNER_USER", Some("exedev"))
                .unwrap_or_else(|| "exedev".to_string()),
            runner_dir: value(
                "LSBX_CI_RUNNER_DIR",
                "RUNNER_DIR",
                Some("/opt/actions-runner"),
            )
            .unwrap_or_else(|| "/opt/actions-runner".to_string()),
            host_prefix,
            provision_script,
            runner_wait_timeout,
            job_timeout,
        })
    }
}

fn duration_env(name: &str, default_seconds: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_seconds))
}

fn load_simple_env_file(path: &Path) -> Result<HashMap<String, String>, LsbxError> {
    let text = fs::read_to_string(path).map_err(|e| {
        LsbxError::BackendUnavailable(format!(
            "failed to read runner env file '{}': {e}",
            path.display()
        ))
    })?;
    let mut values = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '\"' || c == '\'');
        values.insert(key.trim().to_string(), value.to_string());
    }
    Ok(values)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_assignment(key: &str, value: &str) -> String {
    format!("{key}={}\\n", shell_quote(value))
}

/// Creates the small, sanitized upload set consumed by the guest provisioner.
fn prepare_runner_upload(config: &RunnerConfig, repository: &str) -> Result<TempDir, LsbxError> {
    let directory = TempDirBuilder::new()
        .prefix("lsbx-ci-broker-")
        .tempdir()
        .map_err(|e| {
            LsbxError::ContractViolated(format!("failed to create runner upload directory: {e}"))
        })?;
    let key_bytes = fs::read(&config.app_key_path).map_err(|e| {
        LsbxError::AuthFailed(format!(
            "failed to read GitHub App private key '{}': {e}",
            config.app_key_path.display()
        ))
    })?;
    let repo_name = repository.rsplit('/').next().unwrap_or(repository);
    let mut env = String::new();
    for (key, value) in [
        ("GITHUB_APP_ID", config.app_id.to_string()),
        (
            "GITHUB_APP_KEY",
            "/tmp/lsbx-ci-broker/lsbx-runner-app.pem".to_string(),
        ),
        ("GITHUB_SCOPE", config.scope.clone()),
        ("GITHUB_OWNER", config.owner.clone()),
        ("GITHUB_REPO", repo_name.to_string()),
        ("RUNNER_LABELS", config.labels.clone()),
        ("RUNNER_USER", config.runner_user.clone()),
        ("RUNNER_DIR", config.runner_dir.clone()),
    ] {
        env.push_str(&shell_assignment(key, &value));
    }
    if let Some(group) = &config.group {
        env.push_str(&shell_assignment("RUNNER_GROUP", group));
    }
    let env_path = directory.path().join("lsbx-runner.env");
    let key_path = directory.path().join("lsbx-runner-app.pem");
    let script_path = directory.path().join("provision-ci-runner.sh");
    fs::write(&env_path, env)
        .map_err(|e| LsbxError::ContractViolated(format!("failed to write runner env: {e}")))?;
    fs::write(&key_path, key_bytes)
        .map_err(|e| LsbxError::ContractViolated(format!("failed to write runner key: {e}")))?;
    fs::copy(&config.provision_script, &script_path).map_err(|e| {
        LsbxError::ContractViolated(format!(
            "failed to copy runner provision script '{}': {e}",
            config.provision_script.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| LsbxError::ContractViolated(format!("failed to chmod runner env: {e}")))?;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| LsbxError::ContractViolated(format!("failed to chmod runner key: {e}")))?;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700)).map_err(|e| {
            LsbxError::ContractViolated(format!("failed to chmod runner script: {e}"))
        })?;
    }
    Ok(directory)
}

impl<'a> Reconciler<'a> {
    async fn exec_runner_command(
        &self,
        sandbox_id: &str,
        command: String,
    ) -> Result<lsbx_kernel::backend::CommandOutput, LsbxError> {
        let output = self
            .ops
            .exec(sandbox_id, &[command], Duration::from_secs(300))
            .await?;
        if output.exit_code != 0 {
            return Err(LsbxError::BackendUnavailable(format!(
                "runner command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output)
    }

    async fn read_runner_log(&self, sandbox_id: &str) -> Result<String, LsbxError> {
        let output = self
            .ops
            .exec(
                sandbox_id,
                &["cat".to_string(), RUNNER_LOG_PATH.to_string()],
                TAIL_EXEC_TIMEOUT,
            )
            .await?;
        if output.exit_code != 0 {
            return Err(LsbxError::BackendUnavailable(format!(
                "runner log read failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn provision_runner(
        &self,
        record: &mut CiJobRecord,
        config: &RunnerConfig,
    ) -> Result<(), LsbxError> {
        let sandbox_id = record.sandbox_id.as_deref().ok_or_else(|| {
            LsbxError::ContractViolated(format!(
                "CI job {} has no sandbox to provision",
                record.job_id
            ))
        })?;
        record.phase = "provisioning".to_string();
        record.updated_at = now_rfc3339();
        self.job_store.save(record)?;

        let upload = prepare_runner_upload(config, &record.repository)?;
        let _ = self
            .exec_runner_command(sandbox_id, "sudo rm -rf /tmp/lsbx-ci-broker".to_string())
            .await;
        self.ops
            .put(sandbox_id, upload.path(), "/tmp/lsbx-ci-broker")
            .await?;
        for command in [
            "sudo install -m 0600 /tmp/lsbx-ci-broker/lsbx-runner.env /etc/lsbx-runner.env",
            "sudo install -m 0600 /tmp/lsbx-ci-broker/lsbx-runner-app.pem /etc/lsbx-runner-app.pem",
            "sudo install -m 0755 /tmp/lsbx-ci-broker/provision-ci-runner.sh /usr/local/sbin/lsbx-provision-ci-runner",
        ] {
            self.exec_runner_command(sandbox_id, command.to_string()).await?;
        }
        let provision_command = format!(
            "sudo env RUNNER_HOST_PREFIX={} LUFSS_RUNNER_ENV=/etc/lsbx-runner.env bash /usr/local/sbin/lsbx-provision-ci-runner",
            shell_quote(&config.host_prefix)
        );
        let output = self
            .exec_runner_command(sandbox_id, provision_command)
            .await?;
        let runner_name = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(LifecycleMarkers::parse_runner_registered)
            .ok_or_else(|| {
                LsbxError::BackendUnavailable(
                    "runner provisioning did not report a runner name".to_string(),
                )
            })?;
        record.runner_name = Some(runner_name);
        record.updated_at = now_rfc3339();
        self.job_store.save(record)?;

        let start = format!(
            "sudo -u {} nohup {}/run.sh </dev/null >/tmp/lsbx-ci-broker-runner.log 2>&1 &",
            shell_quote(&config.runner_user),
            shell_quote(&config.runner_dir)
        );
        self.exec_runner_command(sandbox_id, start).await?;
        Ok(())
    }

    async fn monitor_runner(
        &self,
        record: &mut CiJobRecord,
        config: &RunnerConfig,
        lease: Duration,
    ) -> Result<(), LsbxError> {
        let sandbox_id = record.sandbox_id.clone().ok_or_else(|| {
            LsbxError::ContractViolated(format!(
                "CI job {} has no sandbox to monitor",
                record.job_id
            ))
        })?;
        let wait_deadline = std::time::Instant::now() + config.runner_wait_timeout;
        record.phase = "waiting_runner".to_string();
        record.updated_at = now_rfc3339();
        self.job_store.save(record)?;

        // Registration/listening is a separate bounded phase. A VM that
        // boots successfully but never registers must not consume the full
        // Actions job timeout.
        loop {
            if std::time::Instant::now() >= wait_deadline {
                return Err(LsbxError::Interrupted(format!(
                    "runner for Actions job {} did not reach Listening for Jobs",
                    record.job_id
                )));
            }
            let log = match self.read_runner_log(&sandbox_id).await {
                Ok(log) => log,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            let mut listening = false;
            let mut exited = false;
            for line in log.lines() {
                if let Some(name) = LifecycleMarkers::parse_runner_registered(line) {
                    record.runner_name = Some(name);
                }
                listening |= LifecycleMarkers::is_listening_for_jobs(line);
                exited |= LifecycleMarkers::is_exited(line);
            }
            if record.runner_name.is_some() {
                if let Err(error) = self.check_divergence(record).await {
                    warn!(job_id = record.job_id.as_str(), error = %error, "runner divergence check failed during registration; continuing");
                }
            }
            record.updated_at = now_rfc3339();
            self.job_store.save(record)?;
            if listening {
                record.phase = "running".to_string();
                record.updated_at = now_rfc3339();
                self.job_store.save(record)?;
                break;
            }
            if exited {
                return Err(LsbxError::BackendUnavailable(format!(
                    "runner {} exited before listening for jobs",
                    record.runner_name.as_deref().unwrap_or("unknown")
                )));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        let deadline = std::time::Instant::now() + config.job_timeout;
        let mut next_renew = std::time::Instant::now();
        while std::time::Instant::now() < deadline {
            let log = match self.read_runner_log(&sandbox_id).await {
                Ok(log) => log,
                Err(error) => {
                    warn!(job_id = record.job_id.as_str(), error = %error, "runner log read failed; retrying");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            let mut completion: Option<(String, String)> = None;
            let mut exited = false;
            for line in log.lines() {
                if let Some(name) = LifecycleMarkers::parse_runner_registered(line) {
                    record.runner_name = Some(name);
                }
                if LifecycleMarkers::is_listening_for_jobs(line) && record.phase == "waiting_runner"
                {
                    record.phase = "running".to_string();
                }
                if let Some(name) = LifecycleMarkers::parse_running_job(line) {
                    record.actual_job_name = Some(name.clone());
                    if record
                        .dispatched_job_name
                        .as_deref()
                        .is_some_and(|expected| expected != name)
                    {
                        record.diverged = true;
                    }
                }
                if completion.is_none() {
                    completion = LifecycleMarkers::parse_job_completed(line);
                }
                exited |= LifecycleMarkers::is_exited(line);
            }
            if record.runner_name.is_some() {
                if let Err(error) = self.check_divergence(record).await {
                    warn!(job_id = record.job_id.as_str(), error = %error, "runner divergence check failed; continuing");
                }
            }
            record.updated_at = now_rfc3339();
            self.job_store.save(record)?;

            if let Some((job_name, result)) = completion {
                record.actual_job_name = Some(job_name);
                if !record.diverged
                    && !result.eq_ignore_ascii_case("success")
                    && !result.eq_ignore_ascii_case("succeeded")
                {
                    return Err(LsbxError::BackendUnavailable(format!(
                        "runner reported job result {result}"
                    )));
                }
                break;
            }
            if exited {
                if record.diverged {
                    break;
                }
                return Err(LsbxError::BackendUnavailable(format!(
                    "runner {} exited before completing job {}",
                    record.runner_name.as_deref().unwrap_or("unknown"),
                    record.job_id
                )));
            }

            let job_id = record.job_id.parse::<u64>().map_err(|_| {
                LsbxError::ContractViolated(format!("invalid Actions job id {}", record.job_id))
            })?;
            match self.github.job(&record.repository, job_id).await {
                Ok(job) => {
                    if job.runner_name.is_some()
                        && record.runner_name.is_some()
                        && job.runner_name != record.runner_name
                    {
                        record.diverged = true;
                    }
                    if job.status == "completed" {
                        if record.diverged {
                            break;
                        }
                        if job.conclusion.as_deref() != Some("success") {
                            return Err(LsbxError::BackendUnavailable(format!(
                                "Actions job {} concluded {}",
                                record.job_id,
                                job.conclusion.as_deref().unwrap_or("without a conclusion")
                            )));
                        }
                        break;
                    }
                }
                Err(error) => {
                    warn!(job_id = record.job_id.as_str(), error = %error, "Actions job status lookup failed; continuing");
                }
            }

            if std::time::Instant::now() >= next_renew {
                let renewed = self.ops.renew(&sandbox_id, lease).await?;
                record.lease_expires_at = renewed.lease_expires_at;
                record.updated_at = now_rfc3339();
                self.job_store.save(record)?;
                next_renew = std::time::Instant::now() + (lease / 3).max(Duration::from_secs(30));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        if std::time::Instant::now() >= deadline {
            return Err(LsbxError::Interrupted(format!(
                "timed out waiting for Actions job {}",
                record.job_id
            )));
        }
        Ok(())
    }

    async fn cleanup_runner(&self, record: &mut CiJobRecord) -> Result<(), LsbxError> {
        let Some(sandbox_id) = record.sandbox_id.clone() else {
            return self.job_store.delete(&record.job_id);
        };
        record.phase = "cleaning".to_string();
        record.updated_at = now_rfc3339();
        self.job_store.save(record)?;
        let scrub = self.ops.exec(
            &sandbox_id,
            &["sudo rm -rf /tmp/lsbx-ci-broker /tmp/lsbx-ci-broker-runner.log /etc/lsbx-runner.env /etc/lsbx-runner-app.pem".to_string()],
            Duration::from_secs(60),
        ).await;
        let scrub_error = match scrub {
            Ok(output) if output.exit_code == 0 => None,
            Ok(output) => Some(format!(
                "scrub failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => Some(format!("scrub failed: {e}")),
        };
        let destroy_result = self.ops.destroy(&sandbox_id).await;
        let destroy_error = match destroy_result {
            Ok(()) | Err(LsbxError::NotFound(_)) => None,
            Err(e) => Some(format!("destroy failed: {e}")),
        };
        if scrub_error.is_none() && destroy_error.is_none() {
            return self.job_store.delete(&record.job_id);
        }
        let error = [scrub_error, destroy_error]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        record.last_error = Some(error.clone());
        record.updated_at = now_rfc3339();
        self.job_store.save(record)?;
        Err(LsbxError::BackendUnavailable(error))
    }

    /// Full Python-equivalent lifecycle: upload credentials/script, provision
    /// an ephemeral Actions runner, monitor/renew it, and scrub/destroy it on
    /// every exit path. The same method is used by libvirt and exe.dev.
    pub async fn run_runner_job(
        &self,
        record: &mut CiJobRecord,
        config: &RunnerConfig,
        lease: Duration,
    ) -> Result<(), LsbxError> {
        let primary = async {
            let existing_log = match record.sandbox_id.as_deref() {
                Some(sandbox_id) => self.read_runner_log(sandbox_id).await.ok(),
                None => None,
            };
            if existing_log.as_deref().is_some_and(|log| {
                log.lines()
                    .any(|line| LifecycleMarkers::parse_runner_registered(line).is_some())
            }) {
                self.monitor_runner(record, config, lease).await
            } else {
                self.provision_runner(record, config).await?;
                self.monitor_runner(record, config, lease).await
            }
        }
        .await;
        if let Err(error) = &primary {
            record.last_error = Some(error.to_string());
            record.updated_at = now_rfc3339();
            let _ = self.job_store.save(record);
        } else {
            record.phase = "completed".to_string();
            record.updated_at = now_rfc3339();
            let _ = self.job_store.save(record);
        }
        let cleanup = self.cleanup_runner(record).await;
        match (primary, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup_error)) => Err(LsbxError::BackendUnavailable(format!(
                "{error}; {cleanup_error}"
            ))),
        }
    }
}

/// `lsbx-broker` entry point existed anywhere in the merged workspace before
/// this unit. Acquires `CiJobStore::broker_lock()` once and holds the
/// returned `LockGuard` for this function's own stack frame — which is this
/// process's entire reason to run, so that lifetime *is* "the process
/// lifetime" the acceptance criterion names — calls `reconcile_on_startup`
/// to resume any in-flight jobs a prior crash left behind, then drives
/// `Poller::tick` (Unit 17) and this unit's own
/// `dispatch`/`tail_and_update`/`check_divergence` in one loop until
/// `iterations` steps have run.
///
/// `iterations: Option<u32>` rather than an unconditional `loop {}`: a real
/// deployment passes `None` (run forever, until the process is killed —
/// e.g. by systemd, matching SPEC.md §4.10's `lsbx-ci-broker`/
/// `lsbx-ci-broker-exe` systemd units); this unit's own tests pass
/// `Some(n)` so a test can assert the loop actually ran a bounded, known
/// number of steps without needing to kill a real infinite loop from the
/// outside.
///
/// A `LockContention` from `broker_lock()` propagates directly and ends this
/// function immediately, before `reconcile_on_startup` or any polling ever
/// runs — matching the acceptance criterion's "failing to acquire it exits
/// with `LsbxError::LockContention` naming that another broker instance
/// already owns this state directory" (the message is `lock.rs`'s own
/// `"lock held elsewhere: {path}"`, not re-wrapped here — see the module doc
/// comment's ground-truth note on why that message is trusted as-is).
///
/// Divergence is never fatal here either: a `check_divergence` (or
/// `tail_and_update`) error for one job is logged and that job's iteration
/// is skipped; it never propagates out of this loop and never causes the
/// broker lock to be released early.
pub async fn run_broker(
    job_store: &CiJobStore,
    ops: &lsbx_ops::LsbxOps,
    github: &GitHubClient,
    config: BrokerConfig,
    iterations: Option<u32>,
) -> Result<(), LsbxError> {
    let _lock_guard = job_store.broker_lock()?;

    let reconciler = Reconciler::new(ops, job_store, github);
    if let Some(runner) = config.runner.as_ref() {
        return run_runner_broker(
            &reconciler,
            job_store,
            github,
            config.poll,
            config.lease,
            runner,
            iterations,
        )
        .await;
    }

    let mut in_flight = reconciler.reconcile_on_startup().await?;

    let mut poller = super::poll::Poller::new(config.poll);
    let mut step: u32 = 0;

    loop {
        if let Some(max) = iterations {
            if step >= max {
                break;
            }
        }
        step += 1;

        let now = std::time::SystemTime::now();
        let queued = poller.tick(github, now).await?;

        for job in &queued {
            match reconciler.dispatch(job, config.lease).await {
                Ok(record) => in_flight.push(record),
                Err(e) => {
                    warn!(job_id = job.job_id, error = %e, "failed to dispatch CI job; will be retried on a later poll tick");
                }
            }
        }

        // Every record already known to be in flight (freshly dispatched
        // this step, or resumed at startup) gets one tail-and-update plus
        // one divergence check per loop step. A failure in either is logged
        // and this record's step is skipped — never propagated out of the
        // loop, matching the module doc comment's "divergence/per-job
        // failure never takes down the whole process" stance.
        for record in &mut in_flight {
            if record.phase == PHASE_COMPLETED || record.phase == PHASE_FAILED {
                continue;
            }
            if let Err(e) = reconciler.tail_and_update(record).await {
                warn!(job_id = record.job_id.as_str(), error = %e, "tail_and_update failed for in-flight CI job");
                continue;
            }
            if let Err(e) = reconciler.check_divergence(record).await {
                warn!(job_id = record.job_id.as_str(), error = %e, "check_divergence failed for in-flight CI job");
            }
        }

        if iterations.is_some() {
            // Test-driven bounded loop: never sleeps, so a test asserting a
            // fixed number of steps completes without a real wall-clock
            // wait.
            continue;
        }

        tokio::time::sleep(poller.config().poll_interval).await;
    }

    Ok(())
}
async fn run_runner_broker(
    reconciler: &Reconciler<'_>,
    job_store: &CiJobStore,
    github: &GitHubClient,
    poll_config: super::poll::PollConfig,
    lease: Duration,
    runner: &RunnerConfig,
    iterations: Option<u32>,
) -> Result<(), LsbxError> {
    let mut poller = super::poll::Poller::new(poll_config);
    let mut step = 0u32;

    // Resume guests left behind by a broker restart before claiming new work.
    for mut record in reconciler.reconcile_on_startup().await? {
        if let Err(error) = reconciler.run_runner_job(&mut record, runner, lease).await {
            warn!(job_id = record.job_id.as_str(), error = %error, "failed to resume CI runner after broker restart");
        }
    }

    loop {
        if let Some(max) = iterations {
            if step >= max {
                return Ok(());
            }
        }
        step += 1;
        let queued = poller.tick(github, std::time::SystemTime::now()).await?;
        let mut claimed = false;
        for job in queued {
            // A queued job may remain queued briefly after another broker
            // poll observes it. Durable records are the idempotency barrier.
            if job_store.load(&job.job_id.to_string()).is_ok() {
                continue;
            }
            let mut record = match reconciler.dispatch(&job, lease).await {
                Ok(record) => record,
                Err(error) => {
                    warn!(job_id = job.job_id, error = %error, "failed to dispatch CI job");
                    continue;
                }
            };
            claimed = true;
            if let Err(error) = reconciler.run_runner_job(&mut record, runner, lease).await {
                warn!(job_id = job.job_id, error = %error, "CI runner job failed");
            }
            // Match the Python broker's one-runner-at-a-time behavior. This
            // prevents two same-label jobs from racing for one fresh runner.
            break;
        }
        if iterations.is_none() && !claimed {
            tokio::time::sleep(poller.config().poll_interval).await;
        }
    }
}
