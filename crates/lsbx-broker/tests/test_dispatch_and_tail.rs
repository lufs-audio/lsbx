//! `dispatch(job)` calls `LsbxOps::create` with `profile: "ci"`, `task_id:
//! job.job_id`, and the configured lease, persists a `CiJobRecord` with
//! `phase: "dispatched"` before returning, then `tail_and_update` reads the
//! runner log via `LsbxOps::exec` and applies `LifecycleMarkers` to move
//! `phase`/`runner_name`/`actual_job_name` forward.
//!
//! Built against a real `LsbxOps` (constructed with the real 6-arg
//! `LsbxOps::new(backend, backend_name, sandbox_store, ci_job_store,
//! registry, clock)`, confirmed by direct read of the merged
//! `crates/lsbx-ops/src/lib.rs`) wrapping a real `DemoBackend` — never a
//! hand-rolled fake standing in for the whole façade, matching this unit's
//! own boundary ("calls `LsbxOps::create`/`exec`, exactly like the CLI or
//! HTTP door would").
//!
//! `DemoBackend::run` always returns `CommandOutput { exit_code: 0, stdout:
//! vec![], stderr: vec![] }` regardless of the command given — it has no
//! notion of a "runner log file" to `cat`. So this test seeds the runner log
//! content the way `tail_and_update` will actually read it in production
//! (via `Backend::run`'s returned `stdout`) by wrapping `DemoBackend` in a
//! small local `Backend` decorator that intercepts the exact `["cat",
//! RUNNER_LOG_PATH]` command `tail_and_update` issues and returns
//! configurable log content instead, while forwarding every other call
//! (`create_from_golden`, `put_file`, `get_file`, `destroy`, `list_vms`,
//! `rename_vm`) straight through to a real `DemoBackend`. This keeps
//! `dispatch`'s own VM-provisioning path fully real (a real
//! `create_from_golden` call, a real persisted `SandboxRecord`) while still
//! letting the test control exactly what `tail_and_update` "reads."
//!
//! `Backend` is implemented directly on `Arc<LogInjectingBackend>` (rather
//! than on a bare `LogInjectingBackend` plus a second wrapper struct per
//! test) so one `Arc` clone gives both `LsbxOps::new` (which needs `Box<dyn
//! Backend>`) and the test body (which needs to call `set_log` after
//! `dispatch` while `ops` still owns the boxed trait object) their own
//! handle to the same underlying backend.

use lsbx_backend_demo::DemoBackend;
use lsbx_broker::github_client::GitHubClient;
use lsbx_broker::poll::QueuedJob;
use lsbx_broker::reconcile::{Reconciler, RUNNER_LOG_PATH};
use lsbx_golden::registry::ImageRegistry;
use lsbx_kernel::backend::{Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm};
use lsbx_kernel::clock::SystemClock;
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Wraps a real `DemoBackend`, intercepting only the `["cat",
/// RUNNER_LOG_PATH]` command `tail_and_update` issues and returning
/// caller-configured log content for it. Every other call passes straight
/// through to the wrapped `DemoBackend`.
///
/// `#[derive(Clone)]` over `Arc`-wrapped internals (rather than wrapping the
/// whole struct in `Arc` at each call site) so the test body can hold one
/// handle to call `set_log` on, `.clone()` a second cheap handle for
/// `LsbxOps::new` to box as `Box<dyn Backend>`, and both handles share the
/// same underlying `DemoBackend`/log content — while still implementing the
/// orphan-rule-safe `impl Backend for LogInjectingBackend` (implementing a
/// local trait for `Arc<LogInjectingBackend>` directly would hit E0117,
/// since neither `Backend` nor `Arc` is defined in this test crate).
#[derive(Clone)]
struct LogInjectingBackend {
    inner: Arc<DemoBackend>,
    log_content: Arc<Mutex<String>>,
}

impl LogInjectingBackend {
    fn new() -> Self {
        Self {
            inner: Arc::new(DemoBackend::new()),
            log_content: Arc::new(Mutex::new(String::new())),
        }
    }

    fn set_log(&self, content: &str) {
        #[allow(clippy::unwrap_used)]
        let mut guard = self.log_content.lock().unwrap();
        *guard = content.to_string();
    }
}

#[async_trait::async_trait]
impl Backend for LogInjectingBackend {
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError> {
        self.inner.create_from_golden(req).await
    }

    async fn run(&self, vm_tag: &str, command: &[String], timeout: Duration, identity_file: Option<&std::path::Path>) -> Result<CommandOutput, LsbxError> {
        if command == ["cat".to_string(), RUNNER_LOG_PATH.to_string()] {
            #[allow(clippy::unwrap_used)]
            let content = self.log_content.lock().unwrap().clone();
            return Ok(CommandOutput {
                exit_code: 0,
                stdout: content.into_bytes(),
                stderr: vec![],
            });
        }
        self.inner.run(vm_tag, command, timeout, identity_file).await
    }

    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str, identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        self.inner.put_file(vm_tag, source, destination, identity_file).await
    }

    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path, identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        self.inner.get_file(vm_tag, source, destination, identity_file).await
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        self.inner.destroy(vm_tag).await
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        self.inner.list_vms().await
    }

    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError> {
        self.inner.rename_vm(old_tag, new_tag).await
    }
}

fn sample_job() -> QueuedJob {
    QueuedJob {
        job_id: 424242,
        run_id: 999,
        repository: "lufs-audio/lsbx".to_string(),
        labels: vec!["lsbx-default".to_string()],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    }
}

fn empty_registry() -> ImageRegistry {
    ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: std::collections::HashMap::new(),
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn dispatch_calls_create_with_ci_profile_and_persists_dispatched_phase() {
    let backend = LogInjectingBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(state_dir.path().to_path_buf());

    let ops = LsbxOps::new(
        Box::new(backend),
        "log-injecting-demo".to_string(),
        sandbox_store,
        ci_job_store,
        empty_registry(),
        Box::new(SystemClock),
    );

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let job = sample_job();
    let record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");

    assert_eq!(record.job_id, "424242");
    assert_eq!(record.phase, "dispatched");
    assert_eq!(record.repository, "lufs-audio/lsbx");
    assert!(record.sandbox_id.is_some());
    assert!(!record.diverged);

    // Persisted before dispatch returned — loadable via a fresh CiJobStore
    // handle pointed at the same state dir.
    let reloaded = job_store_for_reconciler.load(&record.job_id).expect("load");
    assert_eq!(reloaded.phase, "dispatched");
    assert_eq!(reloaded.sandbox_id, record.sandbox_id);
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn tail_and_update_advances_phase_and_runner_name_from_lifecycle_markers() {
    let backend = LogInjectingBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(state_dir.path().to_path_buf());

    let ops = LsbxOps::new(
        Box::new(backend.clone()),
        "log-injecting-demo".to_string(),
        sandbox_store,
        ci_job_store,
        empty_registry(),
        Box::new(SystemClock),
    );

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let job = sample_job();
    let mut record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");
    assert_eq!(record.phase, "dispatched");

    // Simulate the runner log having reached "registered" + "listening".
    backend.set_log(
        "2026-08-24T00:00:00Z Runner registered: lsbx-ci-runner-abc123\n\
         2026-08-24T00:00:01Z Listening for Jobs\n",
    );

    reconciler
        .tail_and_update(&mut record)
        .await
        .expect("tail_and_update should succeed");

    assert_eq!(record.runner_name, Some("lsbx-ci-runner-abc123".to_string()));
    assert_eq!(record.phase, "listening");

    // Persisted after this meaningful transition — not only at the very end.
    let reloaded = job_store_for_reconciler.load(&record.job_id).expect("load");
    assert_eq!(reloaded.phase, "listening");
    assert_eq!(reloaded.runner_name, Some("lsbx-ci-runner-abc123".to_string()));

    // Advance further: the runner picks up and completes a job.
    backend.set_log(
        "2026-08-24T00:00:00Z Runner registered: lsbx-ci-runner-abc123\n\
         2026-08-24T00:00:01Z Listening for Jobs\n\
         2026-08-24T00:00:02Z Running job: build-and-test\n\
         2026-08-24T00:00:10Z Job build-and-test completed with result: Succeeded\n",
    );

    reconciler
        .tail_and_update(&mut record)
        .await
        .expect("tail_and_update should succeed");

    assert_eq!(record.phase, "completed");
    assert_eq!(record.actual_job_name, Some("build-and-test".to_string()));
    assert!(record.last_error.is_none());

    let reloaded_final = job_store_for_reconciler.load(&record.job_id).expect("load");
    assert_eq!(reloaded_final.phase, "completed");
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn tail_and_update_marks_failed_result_with_last_error() {
    let backend = LogInjectingBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(state_dir.path().to_path_buf());

    let ops = LsbxOps::new(
        Box::new(backend.clone()),
        "log-injecting-demo".to_string(),
        sandbox_store,
        ci_job_store,
        empty_registry(),
        Box::new(SystemClock),
    );

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let job = sample_job();
    let mut record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");

    backend.set_log(
        "Runner registered: lsbx-ci-runner-xyz\n\
         Listening for Jobs\n\
         Running job: flaky-test\n\
         Job flaky-test completed with result: Failed\n",
    );

    reconciler
        .tail_and_update(&mut record)
        .await
        .expect("tail_and_update should succeed");

    assert_eq!(record.phase, "failed");
    assert!(record.last_error.is_some());
    #[allow(clippy::unwrap_used)]
    let last_error = record.last_error.as_ref().unwrap();
    assert!(last_error.contains("flaky-test"));
}
