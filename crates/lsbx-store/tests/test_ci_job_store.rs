// See crates/lsbx-kernel/tests/test_kernel.rs for why this allow is scoped
// to test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_kernel::error::LsbxError;
use lsbx_store::ci_job_store::{CiJobRecord, CiJobStore};
use std::os::unix::fs::PermissionsExt;

fn sample_job(job_id: &str, phase: &str) -> CiJobRecord {
    CiJobRecord {
        job_id: job_id.to_string(),
        queue_label: "lsbx-default".to_string(),
        runner_group: None,
        host_prefix: None,
        phase: phase.to_string(),
        sandbox_id: Some("sbx-1".to_string()),
        runner_name: Some("runner-1".to_string()),
        dispatched_job_name: Some("build".to_string()),
        actual_job_id: None,
        actual_job_name: None,
        diverged: false,
        repository: "lufs-audio/lsbx".to_string(),
        created_at: "2026-08-24T00:00:00Z".to_string(),
        updated_at: "2026-08-24T00:00:00Z".to_string(),
        lease_expires_at: None,
        last_error: None,
    }
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());
    let job = sample_job("job-round-trip", "dispatched");

    store.save(&job).unwrap();
    let loaded = store.load(&job.job_id).unwrap();

    assert_eq!(loaded, job);
}

#[test]
fn save_writes_ci_job_envelope_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());
    let job = sample_job("job-envelope-shape", "running");

    store.save(&job).unwrap();

    let raw = std::fs::read_to_string(dir.path().join("ci-broker").join("job-envelope-shape.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value.get("schema_version").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(value.get("kind").and_then(|v| v.as_str()), Some("ci-job"));
    assert_eq!(
        value.get("job").and_then(|j| j.get("job_id")).and_then(|v| v.as_str()),
        Some("job-envelope-shape")
    );
}

#[test]
fn save_sets_directory_and_file_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());
    let job = sample_job("job-perms", "dispatched");

    store.save(&job).unwrap();

    let ci_broker_dir = dir.path().join("ci-broker");
    let dir_mode = std::fs::metadata(&ci_broker_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);

    let file_mode = std::fs::metadata(ci_broker_dir.join("job-perms.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
}

#[test]
fn load_missing_job_returns_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());

    match store.load("does-not-exist") {
        Err(LsbxError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn list_in_flight_returns_empty_vec_when_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());

    assert_eq!(store.list_in_flight().unwrap(), Vec::new());
}

#[test]
fn list_in_flight_excludes_completed_and_failed() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());

    store.save(&sample_job("job-dispatched", "dispatched")).unwrap();
    store.save(&sample_job("job-running", "running")).unwrap();
    store.save(&sample_job("job-completed", "completed")).unwrap();
    store.save(&sample_job("job-failed", "failed")).unwrap();

    let mut ids: Vec<String> = store.list_in_flight().unwrap().into_iter().map(|j| j.job_id).collect();
    ids.sort();
    assert_eq!(ids, vec!["job-dispatched".to_string(), "job-running".to_string()]);
}

#[test]
fn broker_lock_uses_lock_sentinel_try_acquire_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let store = CiJobStore::new(dir.path().to_path_buf());

    let guard = store.broker_lock().unwrap();
    assert!(dir.path().join("ci-broker.lock").exists());

    // A second attempt must fail closed with LockContention while the
    // first guard is held — matching the existing `BrokerLock`'s
    // fail-closed behavior, now backed by the shared `LockSentinel`
    // primitive instead of a separately hand-rolled mechanism.
    match store.broker_lock() {
        Err(LsbxError::LockContention(_)) => {}
        other => panic!("expected LockContention, got {:?}", other.map(|_| ())),
    }

    drop(guard);
    let _guard2 = store.broker_lock().unwrap();
}
