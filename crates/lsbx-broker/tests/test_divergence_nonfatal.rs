//! Forced-divergence test: mock GitHub returns a different `job_for_runner`
//! than the dispatched `job_id`; asserts `CiJobRecord.diverged` becomes
//! `true` and that `check_divergence`'s own `Result` is `Ok` (the process
//! keeps running rather than erroring out) — matching the acceptance
//! criterion's literal wording exactly.
//!
//! Uses `wiremock` (already a dev-dependency from Units 16/17's own auth
//! tests) to mock `GET /repos/{repo}/actions/runs` and
//! `GET /repos/{repo}/actions/runs/{run_id}/jobs`, and
//! `GitHubClient::from_installation_token_with_base_uri` (this unit's own
//! `test-util`-gated addition — see `github_client.rs`'s doc comment) to
//! point `job_for_runner`'s underlying `octocrab` client at the mock server
//! instead of real `api.github.com`.

use lsbx_backend_demo::DemoBackend;
use lsbx_broker::github_client::GitHubClient;
use lsbx_broker::poll::QueuedJob;
use lsbx_broker::reconcile::Reconciler;
use lsbx_golden::registry::ImageRegistry;
use lsbx_kernel::clock::SystemClock;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn empty_registry() -> ImageRegistry {
    ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: std::collections::HashMap::new(),
    }
}

fn sample_job() -> QueuedJob {
    QueuedJob {
        job_id: 111,
        run_id: 555,
        repository: "lufs-audio/lsbx".to_string(),
        labels: vec!["lsbx-default".to_string()],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    }
}

/// Mocks the two-step `workflow_runs`-then-`run_jobs` traversal
/// `GitHubClient::job_for_runner` performs: one workflow run with `id:
/// mock_run_id`, and that run's one job with `id: actual_job_id, runner_name:
/// Some(runner_name)`. Registered for both the `"queued"` and
/// `"in_progress"` statuses `job_for_runner` scans, since a real GitHub
/// runner in this state could be reported under either depending on timing
/// — the mock doesn't need to be picky about which one the implementation
/// checks first.
async fn mock_job_for_runner_response(
    server: &MockServer,
    repo_path_segment: &str,
    mock_run_id: u64,
    actual_job_id: u64,
    runner_name: &str,
) {
    let runs_body = serde_json::json!({
        "total_count": 1,
        "workflow_runs": [ { "id": mock_run_id } ]
    });

    Mock::given(method("GET"))
        .and(path(format!("/repos/{repo_path_segment}/actions/runs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(&runs_body))
        .mount(server)
        .await;

    let jobs_body = serde_json::json!({
        "total_count": 1,
        "jobs": [
            {
                "id": actual_job_id,
                "run_id": mock_run_id,
                "status": "in_progress",
                "labels": ["lsbx-default"],
                "created_at": chrono::Utc::now().to_rfc3339(),
                "runner_name": runner_name,
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path(format!(
            "/repos/{repo_path_segment}/actions/runs/{mock_run_id}/jobs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(&jobs_body))
        .mount(server)
        .await;
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn check_divergence_sets_diverged_true_and_returns_ok_on_mismatch() {
    let server = MockServer::start().await;

    let job = sample_job();
    let dispatched_job_id = job.job_id; // 111
    let actual_job_id = 999u64; // Deliberately different from dispatched_job_id.
    let runner_name = "lsbx-ci-runner-diverged";

    mock_job_for_runner_response(&server, "lufs-audio/lsbx", 777, actual_job_id, runner_name).await;

    let github = GitHubClient::from_installation_token_with_base_uri(
        "fake-installation-token".to_string(),
        &server.uri(),
    )
    .expect("client should build");

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

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let mut record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");
    assert_eq!(record.job_id, dispatched_job_id.to_string());

    // Runner has registered (tail_and_update would normally set this from
    // the log; set it directly here since this test is only exercising
    // check_divergence's own cross-check, not the log-tailing path).
    record.runner_name = Some(runner_name.to_string());

    let result = reconciler.check_divergence(&mut record).await;

    // Divergence is never fatal: the call itself must succeed.
    assert!(result.is_ok(), "check_divergence must return Ok even on a divergence finding");

    assert!(record.diverged, "diverged must be set true when GitHub's actual job_for_runner differs");
    assert_eq!(record.actual_job_id, Some(actual_job_id.to_string()));

    // The record must still be loadable afterward (process keeps running,
    // state keeps advancing normally) — this is what "not fatal" means in
    // concrete, checkable terms beyond just the Result being Ok.
    let reloaded = job_store_for_reconciler.load(&record.job_id).expect("load");
    assert!(reloaded.diverged);
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn check_divergence_leaves_diverged_false_when_job_ids_match() {
    let server = MockServer::start().await;

    let job = sample_job();
    let dispatched_job_id = job.job_id; // 111
    let runner_name = "lsbx-ci-runner-matching";

    // GitHub reports the runner assigned to the SAME job_id lsbx dispatched
    // for — no divergence.
    mock_job_for_runner_response(&server, "lufs-audio/lsbx", 777, dispatched_job_id, runner_name).await;

    let github = GitHubClient::from_installation_token_with_base_uri(
        "fake-installation-token".to_string(),
        &server.uri(),
    )
    .expect("client should build");

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

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let mut record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");
    record.runner_name = Some(runner_name.to_string());

    reconciler
        .check_divergence(&mut record)
        .await
        .expect("check_divergence should succeed");

    assert!(!record.diverged);
}

#[tokio::test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn check_divergence_is_a_noop_when_runner_name_is_not_yet_known() {
    // No mock server call is registered at all — if check_divergence called
    // GitHub without a runner_name, this test would fail with a connection
    // error against an unmocked route (wiremock's MockServer 404s any
    // unregistered request), proving the no-op path really never calls out.
    let server = MockServer::start().await;
    let github = GitHubClient::from_installation_token_with_base_uri(
        "fake-installation-token".to_string(),
        &server.uri(),
    )
    .expect("client should build");

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

    let job_store_for_reconciler = CiJobStore::new(state_dir.path().to_path_buf());
    let reconciler = Reconciler::new(&ops, &job_store_for_reconciler, &github);

    let job = sample_job();
    let mut record = reconciler
        .dispatch(&job, Duration::from_secs(3600))
        .await
        .expect("dispatch should succeed");
    assert!(record.runner_name.is_none());

    reconciler
        .check_divergence(&mut record)
        .await
        .expect("check_divergence should be a no-op success, not an error");

    assert!(!record.diverged);
}
