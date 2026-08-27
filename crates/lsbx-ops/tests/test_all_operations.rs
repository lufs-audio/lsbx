//! Exercises every public method on `LsbxOps` at least once, against a
//! `DemoBackend`-backed instance — this unit's own explicitly named
//! acceptance criterion (see `docs/specs/.../units/10-shared-operations-facade.md`'s
//! "Verification" section): "so an operation added to the façade later
//! without a corresponding test line is an obvious, reviewable diff, not a
//! silent gap."
//!
//! Each operation gets at least one `Ok` case against valid input and, for
//! every operation with a meaningful "invalid input" shape, one case
//! asserting a specific `LsbxError` variant (e.g. `destroy` on an unknown
//! id returns `NotFound`, as the acceptance criteria names explicitly).
//! Operations with no meaningful invalid-input shape at the façade level
//! (`list`, `golden_list`, `config_show`) are exercised for their `Ok`
//! behavior only; `logs_query` — which is *always* an honest, documented
//! failure today (see `src/lib.rs`'s module doc comment) — is exercised for
//! that specific `ContractViolated` outcome, which *is* its correct-case
//! behavior.

// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and every prior merged unit's own
// tests/*.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::{DemoBackend, FaultMode};
use lsbx_golden::registry::{GoldenConfig, GoldenFlavor, GoldenMode, ImageRegistry, StreamingMode};
use lsbx_kernel::clock::FakeClock;
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// Builds an `LsbxOps` backed by a fresh `DemoBackend` (no fault), an
/// isolated temp-dir-backed `SandboxStore`/`CiJobStore`, an empty
/// `ImageRegistry`, and a `FakeClock` pinned to `now` — so every test gets
/// a deterministic, isolated instance and controls time explicitly rather
/// than depending on real wall-clock timing.
fn build_ops(now: SystemTime) -> (LsbxOps, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(dir.path().to_path_buf());
    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: HashMap::new(),
    };
    let clock = Box::new(FakeClock { now });
    let ops = LsbxOps::new(
        Box::new(DemoBackend::new()),
        "demo".to_string(),
        sandbox_store,
        ci_job_store,
        registry,
        clock,
    );
    (ops, dir)
}

/// Same as [`build_ops`] but with a `DemoBackend` configured to report
/// itself unavailable — used for the `status`/`create` invalid-backend
/// cases.
fn build_ops_with_fault(now: SystemTime, fault: FaultMode) -> (LsbxOps, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(dir.path().to_path_buf());
    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: HashMap::new(),
    };
    let clock = Box::new(FakeClock { now });
    let ops = LsbxOps::new(
        Box::new(DemoBackend::with_fault(fault)),
        "demo".to_string(),
        sandbox_store,
        ci_job_store,
        registry,
        clock,
    );
    (ops, dir)
}

fn sample_golden_config(key: &str) -> GoldenConfig {
    GoldenConfig {
        key: key.to_string(),
        flavor: GoldenFlavor::Agent,
        os: "linux".to_string(),
        base: "lsbx-default-v1".to_string(),
        mode: GoldenMode::Copy,
        cpu: 1,
        memory: "512M".to_string(),
        disk: None,
        streaming: StreamingMode::None,
        capabilities: vec![],
        healthcheck: vec![],
        repo: None,
        content_hash: Some("lufs-abcd1234".to_string()),
        description: "test golden".to_string(),
    }
}

fn create_request<'a>(
    profile: &'a str,
    name: &'a str,
) -> lsbx_lifecycle::create::CreateRequest<'a> {
    lsbx_lifecycle::create::CreateRequest {
        profile,
        golden: None,
        cpu: None,
        memory: None,
        flavor: None,
        streaming: None,
        name: Some(name),
        task_id: None,
        lease: Duration::from_secs(3600),
        ready_timeout: Duration::from_millis(200),
        // `verify: false` — DemoBackend's `run` always exits 0 so
        // readiness would pass anyway, but skipping it keeps this test
        // file's `create` calls fast and deterministic regardless of
        // that backend detail.
        verify: false,
        healthchecks: vec![],
    }
}

// ---- create / destroy ----

#[tokio::test]
async fn create_valid_request_succeeds() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let sandbox = ops
        .create(create_request("lsbx-default-v1", "test-sandbox"))
        .await
        .expect("create should succeed against a healthy DemoBackend");
    assert_eq!(sandbox.name, "test-sandbox");
    assert_eq!(sandbox.profile, "lsbx-default-v1");
}

#[tokio::test]
async fn create_against_unavailable_backend_returns_backend_unavailable() {
    let (ops, _dir) = build_ops_with_fault(SystemTime::now(), FaultMode::Unavailable);
    let result = ops
        .create(create_request("lsbx-default-v1", "test-sandbox"))
        .await;
    assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));
}

#[tokio::test]
async fn destroy_valid_id_succeeds() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let sandbox = ops
        .create(create_request("lsbx-default-v1", "to-destroy"))
        .await
        .expect("create");
    ops.destroy(&sandbox.id)
        .await
        .expect("destroy should succeed");
    // Confirm it's actually gone from the store.
    let info_result = ops.info(&sandbox.id).await;
    assert!(matches!(info_result, Err(LsbxError::NotFound(_))));
}

#[tokio::test]
async fn destroy_unknown_id_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops.destroy("sbx-does-not-exist").await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

// ---- list ----

#[tokio::test]
async fn list_reflects_created_sandboxes() {
    let (ops, _dir) = build_ops(SystemTime::now());
    assert!(ops.list().await.expect("list").is_empty());

    ops.create(create_request("lsbx-default-v1", "sandbox-a"))
        .await
        .expect("create a");
    ops.create(create_request("lsbx-default-v1", "sandbox-b"))
        .await
        .expect("create b");

    let listed = ops.list().await.expect("list");
    assert_eq!(listed.len(), 2);
    let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"sandbox-a"));
    assert!(names.contains(&"sandbox-b"));
}

// ---- info / console_url ----

#[tokio::test]
async fn info_valid_id_returns_public_sandbox() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let created = ops
        .create(create_request("lsbx-default-v1", "info-target"))
        .await
        .expect("create");
    let info = ops.info(&created.id).await.expect("info should succeed");
    assert_eq!(info.id, created.id);
    assert_eq!(info.name, "info-target");
}

#[tokio::test]
async fn info_unknown_id_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops.info("sbx-does-not-exist").await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

#[tokio::test]
async fn console_url_valid_id_returns_some_url_for_novnc_streaming() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let created = ops
        .create(create_request("lsbx-default-v1", "console-target"))
        .await
        .expect("create");
    // DemoBackend::create_from_golden always returns an https_url, so the
    // resulting record's `streaming` is "novnc" and console_url must be Some.
    let url = ops
        .console_url(&created.id)
        .await
        .expect("console_url should succeed");
    assert!(url.is_some());
}

#[tokio::test]
async fn console_url_unknown_id_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops.console_url("sbx-does-not-exist").await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

// ---- exec / put / get ----

#[tokio::test]
async fn exec_valid_id_succeeds() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let created = ops
        .create(create_request("lsbx-default-v1", "exec-target"))
        .await
        .expect("create");
    let output = ops
        .exec(
            &created.id,
            &["echo".to_string(), "hi".to_string()],
            Duration::from_secs(5),
        )
        .await
        .expect("exec should succeed against a live DemoBackend VM");
    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn exec_unknown_id_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops
        .exec(
            "sbx-does-not-exist",
            &["echo".to_string(), "hi".to_string()],
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

#[tokio::test]
async fn put_valid_id_succeeds() {
    let (ops, dir) = build_ops(SystemTime::now());
    let created = ops
        .create(create_request("lsbx-default-v1", "put-target"))
        .await
        .expect("create");

    let source = dir.path().join("upload.txt");
    std::fs::write(&source, b"hello").expect("write source file");

    ops.put(&created.id, &source, "/tmp/uploaded.txt")
        .await
        .expect("put should succeed against a live DemoBackend VM");
}

#[tokio::test]
async fn put_unknown_id_returns_not_found() {
    let (ops, dir) = build_ops(SystemTime::now());
    let source = dir.path().join("upload.txt");
    std::fs::write(&source, b"hello").expect("write source file");

    let result = ops
        .put("sbx-does-not-exist", &source, "/tmp/uploaded.txt")
        .await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

#[tokio::test]
async fn get_valid_id_succeeds() {
    let (ops, dir) = build_ops(SystemTime::now());
    let created = ops
        .create(create_request("lsbx-default-v1", "get-target"))
        .await
        .expect("create");

    let destination = dir.path().join("downloaded.txt");
    ops.get(&created.id, "/tmp/remote.txt", &destination)
        .await
        .expect("get should succeed against a live DemoBackend VM");
}

#[tokio::test]
async fn get_unknown_id_returns_not_found() {
    let (ops, dir) = build_ops(SystemTime::now());
    let destination = dir.path().join("downloaded.txt");
    let result = ops
        .get("sbx-does-not-exist", "/tmp/remote.txt", &destination)
        .await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

// ---- renew ----

#[tokio::test]
async fn renew_valid_id_extends_lease() {
    let now = SystemTime::now();
    let (ops, _dir) = build_ops(now);
    let created = ops
        .create(create_request("lsbx-default-v1", "renew-target"))
        .await
        .expect("create");
    let original_lease = created.lease_expires_at.clone();

    let renewed = ops
        .renew(&created.id, Duration::from_secs(7200))
        .await
        .expect("renew should succeed");
    assert_ne!(renewed.lease_expires_at, original_lease);
}

#[tokio::test]
async fn renew_unknown_id_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops
        .renew("sbx-does-not-exist", Duration::from_secs(3600))
        .await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

// ---- status ----

#[tokio::test]
async fn status_reports_backend_available_and_sandbox_count() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.create(create_request("lsbx-default-v1", "status-target"))
        .await
        .expect("create");

    let status = ops.status().await.expect("status should succeed");
    assert_eq!(status.backend_name, "demo");
    assert!(status.backend_available);
    assert_eq!(status.sandbox_count, 1);
}

#[tokio::test]
async fn status_against_unavailable_backend_reports_backend_available_false() {
    let (ops, _dir) = build_ops_with_fault(SystemTime::now(), FaultMode::Unavailable);
    let status = ops
        .status()
        .await
        .expect("status itself should still succeed even when the backend is unavailable");
    assert!(!status.backend_available);
}

// ---- reap ----

#[tokio::test]
async fn reap_dry_run_with_no_expired_sandboxes_returns_empty_report() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.create(create_request("lsbx-default-v1", "not-expired"))
        .await
        .expect("create");

    let report = ops
        .reap(Duration::ZERO, true)
        .await
        .expect("dry-run reap should succeed");
    assert!(report.would_destroy.is_empty());
    assert!(report.destroyed.is_empty());
}

#[tokio::test]
async fn reap_sweeps_expired_sandbox_and_leaves_live_one() {
    let now = SystemTime::now();
    let (ops, _dir) = build_ops(now);

    // Created with a normal (future) lease via `create_request`'s
    // Duration::from_secs(3600) — this one must survive the sweep.
    let live = ops
        .create(create_request("lsbx-default-v1", "still-live"))
        .await
        .expect("create live");

    // Create a second sandbox, then directly age its persisted record's
    // lease into the past — `create`'s own request shape has no
    // "already-expired" option, so this is the same technique
    // lsbx-lifecycle's own reap tests use to force the expired case.
    let expiring = ops
        .create(create_request("lsbx-default-v1", "will-expire"))
        .await
        .expect("create expiring");

    // Directly load+mutate+save the expiring record's lease into the past.
    // (This test file has no direct SandboxStore handle since LsbxOps owns
    // it privately — proving the expiry via reap's own observable effect
    // is the point, so age it through a second, independently-constructed
    // SandboxStore pointed at the same directory.)
    let state_dir = _dir.path().to_path_buf();
    let raw_store = lsbx_store::sandbox_store::SandboxStore::new(state_dir);
    let mut record = raw_store.load(&expiring.id).expect("load expiring record");
    let past: chrono::DateTime<chrono::Utc> = (now - Duration::from_secs(3600)).into();
    record.lease_expires_at = Some(past.to_rfc3339());
    raw_store.save(&record).expect("save aged record");

    let report = ops
        .reap(Duration::ZERO, false)
        .await
        .expect("reap should succeed");
    assert_eq!(report.destroyed, vec![expiring.id.clone()]);

    // The live sandbox must still be findable; the expired one must not.
    assert!(ops.info(&live.id).await.is_ok());
    assert!(matches!(
        ops.info(&expiring.id).await,
        Err(LsbxError::NotFound(_))
    ));
}

// ---- golden_build ----

#[tokio::test]
async fn golden_build_dry_run_succeeds() {
    let (ops, dir) = build_ops(SystemTime::now());
    let script = dir.path().join("provision.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").expect("write script");

    let outcome = ops
        .golden_build(lsbx_golden::build::GoldenBuildRequest {
            key_path: None,
            name: "agent-base",
            from: "lsbx-default-v1",
            script: &script,
            flavor: GoldenFlavor::Agent,
            cpu: 1,
            memory: "512M",
            streaming: StreamingMode::None,
            register: false,
            cleanup: true,
            dry_run: true,
            pubkey: "ssh-ed25519 AAAA fake",
        })
        .await
        .expect("dry-run golden_build should succeed");
    assert_eq!(outcome.config.key, "agent-base");
}

#[tokio::test]
async fn golden_build_non_dry_run_without_flattener_fails_honestly() {
    let (ops, dir) = build_ops(SystemTime::now());
    let script = dir.path().join("provision.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").expect("write script");

    // Non-dry-run build: this façade never wires up a GoldenFlattener
    // (Unit 19 has not landed — see src/lib.rs's module doc comment), so
    // this must fail with the same ContractViolated error
    // lsbx-golden::build's own NoFlatten precedent establishes, not a
    // silently faked success.
    let result = ops
        .golden_build(lsbx_golden::build::GoldenBuildRequest {
            key_path: None,
            name: "agent-base",
            from: "lsbx-default-v1",
            script: &script,
            flavor: GoldenFlavor::Agent,
            cpu: 1,
            memory: "512M",
            streaming: StreamingMode::None,
            register: false,
            cleanup: true,
            dry_run: false,
            pubkey: "ssh-ed25519 AAAA fake",
        })
        .await;
    assert!(matches!(result, Err(LsbxError::ContractViolated(_))));
}

// ---- golden_verify ----

#[tokio::test]
async fn golden_verify_valid_registered_golden_succeeds() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.golden_register(sample_golden_config("agent-base"))
        .await
        .expect("register");

    let results = ops
        .golden_verify(
            "agent-base",
            "verify-agent-base",
            "ssh-ed25519 AAAA fake",
            None,
        )
        .await
        .expect("golden_verify should succeed");
    // sample_golden_config has no declared healthchecks.
    assert!(results.is_empty());
}

#[tokio::test]
async fn golden_verify_unknown_name_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops
        .golden_verify("does-not-exist", "verify-x", "ssh-ed25519 AAAA fake", None)
        .await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

// ---- golden_register / golden_delete / golden_list ----

#[tokio::test]
async fn golden_register_valid_config_succeeds_and_is_listed() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.golden_register(sample_golden_config("new-golden"))
        .await
        .expect("register should succeed");

    let listed = ops.golden_list().await.expect("golden_list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, "new-golden");
}

#[tokio::test]
async fn golden_register_duplicate_key_returns_usage() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.golden_register(sample_golden_config("dup-golden"))
        .await
        .expect("first register should succeed");

    let result = ops
        .golden_register(sample_golden_config("dup-golden"))
        .await;
    assert!(matches!(result, Err(LsbxError::Usage(_))));
}

#[tokio::test]
async fn golden_delete_valid_key_succeeds() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.golden_register(sample_golden_config("to-delete"))
        .await
        .expect("register");

    ops.golden_delete("to-delete", false)
        .await
        .expect("delete should succeed");

    let listed = ops.golden_list().await.expect("golden_list");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn golden_delete_unknown_key_returns_not_found() {
    let (ops, _dir) = build_ops(SystemTime::now());
    let result = ops.golden_delete("does-not-exist", false).await;
    assert!(matches!(result, Err(LsbxError::NotFound(_))));
}

#[tokio::test]
async fn golden_list_reflects_registered_goldens() {
    let (ops, _dir) = build_ops(SystemTime::now());
    assert!(ops.golden_list().await.expect("golden_list").is_empty());

    ops.golden_register(sample_golden_config("golden-one"))
        .await
        .expect("register one");
    ops.golden_register(sample_golden_config("golden-two"))
        .await
        .expect("register two");

    let listed = ops.golden_list().await.expect("golden_list");
    assert_eq!(listed.len(), 2);
}

// ---- config_show ----

#[tokio::test]
async fn config_show_reflects_registry_shape() {
    let (ops, _dir) = build_ops(SystemTime::now());
    ops.golden_register(sample_golden_config("configured-golden"))
        .await
        .expect("register");

    let config = ops.config_show().await.expect("config_show should succeed");
    assert_eq!(config["backend_name"], "demo");
    assert_eq!(config["goldens"]["count"], 1);
    assert_eq!(config["goldens"]["keys"][0], "configured-golden");
}

// ---- logs_query ----

#[tokio::test]
async fn logs_query_returns_documented_contract_violated_gap() {
    let (ops, _dir) = build_ops(SystemTime::now());
    // logs_query's correct behavior today IS a specific, documented error —
    // no crate in the merged workspace owns a queryable log store yet (see
    // src/lib.rs's module doc comment). Asserting the specific variant
    // here is what keeps this an honest, reviewable gap rather than a
    // silently-passing no-op.
    let result = ops.logs_query(None, 100).await;
    assert!(matches!(result, Err(LsbxError::ContractViolated(_))));
}
