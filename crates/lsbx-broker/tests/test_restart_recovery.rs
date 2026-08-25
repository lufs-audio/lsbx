//! Broker restart recovery: seeds a `CiJobStore` with a `phase: "running"`
//! record (simulating a broker that crashed mid-job), constructs a fresh
//! `Reconciler`, calls `reconcile_on_startup()`, and asserts the record is
//! picked back up for tailing rather than merely listed — exactly the
//! scenario this unit's own Verification section names.

use lsbx_backend_demo::DemoBackend;
use lsbx_broker::github_client::GitHubClient;
use lsbx_broker::reconcile::{Reconciler, RUNNER_LOG_PATH};
use lsbx_golden::registry::ImageRegistry;
use lsbx_kernel::backend::{Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm};
use lsbx_kernel::clock::SystemClock;
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::{CiJobRecord, CiJobStore};
use lsbx_store::sandbox_store::SandboxStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Same log-injection decorator as `test_dispatch_and_tail.rs` — see that
/// file's module doc comment for why this exists instead of relying on
/// `DemoBackend::run`'s fixed always-exit-0-empty-output behavior, and for
/// why this wraps its internals in `Arc` and implements `Backend` on the
/// (locally-defined, orphan-rule-safe) struct itself rather than on
/// `Arc<LogInjectingBackend>` directly.
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

    async fn run(&self, vm_tag: &str, command: &[String], timeout: Duration) -> Result<CommandOutput, LsbxError> {
        if command == ["cat".to_string(), RUNNER_LOG_PATH.to_string()] {
            #[allow(clippy::unwrap_used)]
            let content = self.log_content.lock().unwrap().clone();
            return Ok(CommandOutput {
                exit_code: 0,
                stdout: content.into_bytes(),
                stderr: vec![],
            });
        }
        self.inner.run(vm_tag, command, timeout).await
    }

    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str) -> Result<(), LsbxError> {
        self.inner.put_file(vm_tag, source, destination).await
    }

    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path) -> Result<(), LsbxError> {
        self.inner.get_file(vm_tag, source, destination).await
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

fn empty_registry() -> ImageRegistry {
    ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: std::collections::HashMap::new(),
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn reconcile_on_startup_resumes_tailing_for_a_running_record_not_just_lists_it() {
    let backend = LogInjectingBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store_for_seed = CiJobStore::new(state_dir.path().to_path_buf());

    // Seed a real VM in the backend first, so the "running" record's
    // sandbox_id actually resolves to something tail_and_update can exec
    // against — mirrors what a real prior dispatch would have already done
    // before the (simulated) crash.
    let created = backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &lsbx_kernel::types::GoldenKey::new_unchecked("ci".to_string()),
            name: "restart-recovery-test",
            pubkey: "ssh-ed25519 AAAA fake",
            cpu: 1,
            memory: "1G",
        })
        .await
        .expect("seed VM should provision");

    // Seed a SandboxRecord matching what `dispatch` would have persisted,
    // so `LsbxOps::exec`'s id -> vm_tag resolution succeeds.
    let sandbox_record = lsbx_kernel::types::SandboxRecord {
        id: "sbx-restart-recovery-test".to_string(),
        name: "restart-recovery-test".to_string(),
        host: created.host.clone(),
        profile: "ci".to_string(),
        flavor: "default".to_string(),
        streaming: "none".to_string(),
        username: None,
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: Some("55555".to_string()),
        created_at: Some(now_rfc3339()),
        lease_expires_at: Some(now_rfc3339()),
        vm_tag: Some(created.vm_tag.clone()),
        https_url: created.https_url.clone(),
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    };
    sandbox_store.save(&sandbox_record).expect("save sandbox record");

    // Seed a CiJobRecord whose phase is "running" — simulating a broker
    // that crashed mid-job, per this unit's own restart-recovery scenario.
    let seeded_record = CiJobRecord {
        job_id: "55555".to_string(),
        queue_label: "lsbx-default".to_string(),
        runner_group: None,
        host_prefix: None,
        phase: "running".to_string(),
        sandbox_id: Some(sandbox_record.id.clone()),
        runner_name: Some("lsbx-ci-runner-preexisting".to_string()),
        dispatched_job_name: None,
        actual_job_id: None,
        actual_job_name: None,
        diverged: false,
        repository: "lufs-audio/lsbx".to_string(),
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
        lease_expires_at: Some(now_rfc3339()),
        last_error: None,
    };
    ci_job_store_for_seed.save(&seeded_record).expect("seed running record");

    // Set the log content the resumed tail will observe: the job has since
    // completed while no broker was watching.
    backend.set_log(
        "Runner registered: lsbx-ci-runner-preexisting\n\
         Listening for Jobs\n\
         Running job: resumed-job\n\
         Job resumed-job completed with result: Succeeded\n",
    );

    // Fresh Reconciler, fresh LsbxOps, fresh CiJobStore handle — simulating
    // an actual broker restart, not just reusing in-memory state from
    // whatever dispatched the original record.
    let ops = LsbxOps::new(
        Box::new(backend.clone()),
        "log-injecting-demo".to_string(),
        sandbox_store,
        CiJobStore::new(state_dir.path().to_path_buf()),
        empty_registry(),
        Box::new(SystemClock),
    );
    let job_store = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store, &github);

    let resumed = reconciler
        .reconcile_on_startup()
        .await
        .expect("reconcile_on_startup should succeed");

    assert_eq!(resumed.len(), 1, "exactly the one seeded in-flight record should be returned");
    let resumed_record = &resumed[0];
    assert_eq!(resumed_record.job_id, "55555");

    // The whole point of this scenario: the record must show evidence that
    // tail_and_update actually ran against it (phase moved past "running"
    // to the terminal state the injected log implies), not merely that it
    // was read back out of the store unchanged.
    assert_eq!(
        resumed_record.phase, "completed",
        "reconcile_on_startup must have called tail_and_update, not just listed the record \
         (a record merely listed would still show phase == \"running\", not \"completed\")"
    );
    assert_eq!(resumed_record.actual_job_name, Some("resumed-job".to_string()));

    // The persisted store must reflect this too — a caller (or a later
    // reconcile_on_startup call after a second crash) reading fresh from
    // disk sees the same resumed state, not just the in-memory Vec this
    // call happened to return.
    let reloaded = job_store.load("55555").expect("load");
    assert_eq!(reloaded.phase, "completed");
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn reconcile_on_startup_returns_empty_when_no_in_flight_jobs_exist() {
    let backend = DemoBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(state_dir.path().to_path_buf());
    let ops = LsbxOps::new(
        Box::new(backend),
        "demo".to_string(),
        sandbox_store,
        ci_job_store,
        empty_registry(),
        Box::new(SystemClock),
    );

    let job_store = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store, &github);

    let resumed = reconciler
        .reconcile_on_startup()
        .await
        .expect("reconcile_on_startup should succeed even with nothing in flight");
    assert!(resumed.is_empty());
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn reconcile_on_startup_skips_terminal_records() {
    let backend = DemoBackend::new();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(state_dir.path().to_path_buf());
    let ci_job_store_for_seed = CiJobStore::new(state_dir.path().to_path_buf());

    let completed_record = CiJobRecord {
        job_id: "99999".to_string(),
        queue_label: "lsbx-default".to_string(),
        runner_group: None,
        host_prefix: None,
        phase: "completed".to_string(),
        sandbox_id: None,
        runner_name: None,
        dispatched_job_name: None,
        actual_job_id: None,
        actual_job_name: None,
        diverged: false,
        repository: "lufs-audio/lsbx".to_string(),
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
        lease_expires_at: None,
        last_error: None,
    };
    ci_job_store_for_seed.save(&completed_record).expect("seed completed record");

    let ops = LsbxOps::new(
        Box::new(backend),
        "demo".to_string(),
        sandbox_store,
        CiJobStore::new(state_dir.path().to_path_buf()),
        empty_registry(),
        Box::new(SystemClock),
    );
    let job_store = CiJobStore::new(state_dir.path().to_path_buf());
    let github = GitHubClient::from_gh_cli_fallback();
    let reconciler = Reconciler::new(&ops, &job_store, &github);

    let resumed = reconciler
        .reconcile_on_startup()
        .await
        .expect("reconcile_on_startup should succeed");
    assert!(
        resumed.is_empty(),
        "a completed (terminal) record must not be resumed — CiJobStore::list_in_flight already \
         filters it out on the read side"
    );
}
