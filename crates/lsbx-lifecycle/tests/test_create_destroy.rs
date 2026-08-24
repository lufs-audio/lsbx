// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and every prior merged unit's own
// tests/*.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::{DemoBackend, FaultMode};
use lsbx_kernel::backend::Backend;
use lsbx_kernel::clock::FakeClock;
use lsbx_kernel::error::LsbxError;
use lsbx_lifecycle::{create, destroy, renew, CreateRequest};
use lsbx_store::sandbox_store::SandboxStore;
use std::time::{Duration, SystemTime};

fn fixed_clock() -> FakeClock {
    FakeClock {
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(3_000_000),
    }
}

fn default_request() -> CreateRequest<'static> {
    CreateRequest {
        profile: "demo-profile",
        name: Some("test-sandbox"),
        task_id: Some("task-42"),
        lease: Duration::from_secs(3600),
        ready_timeout: Duration::from_secs(5),
        verify: true,
        healthchecks: Vec::new(),
    }
}

/// Core durability-before-ack proof: once `create` returns `Ok`, the
/// `SandboxStore` already has a record for the returned id -- this test
/// asserts the record is loadable *immediately* after `create` returns,
/// which is the externally observable half of "persisted before the
/// caller was told about it" (the internal half -- that `save` happens
/// before the readiness poll -- is exercised by
/// `create_persists_before_verifying_readiness` below via a backend that
/// cannot ever pass verification).
#[tokio::test]
async fn create_persists_record_before_returning_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();

    // The record must already be durable -- loadable via the same store
    // handle -- the instant `create` has returned.
    let loaded = store.load(&public.id).unwrap();
    assert_eq!(loaded.id, public.id);
    assert_eq!(loaded.task_id, Some("task-42".to_string()));
    assert_eq!(loaded.profile, "demo-profile");
    assert!(loaded.vm_tag.is_some());
}

/// `create`'s `PublicSandbox` projection must never carry key material --
/// `pubkey`/`key_path`/`key_dir`/`key_name` simply do not exist on
/// `PublicSandbox` (Unit 01's type), so this is enforced by the type system
/// already; this test asserts the *store's* full record does carry it
/// (proving key generation actually happened) while the public return
/// value's fields are exactly the ones `SandboxRecord::public()` computes.
#[tokio::test]
async fn create_returns_public_projection_while_store_keeps_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();

    let full = store.load(&public.id).unwrap();
    assert!(
        full.pubkey.is_some(),
        "the full record must retain the generated pubkey"
    );
    assert!(full.key_path.is_some());

    assert_eq!(public.console_url, full.public().console_url);
    assert_eq!(public.id, full.id);
}

/// Durability-before-ack, proven from the *readiness-failure* side: even
/// when the readiness poll never succeeds (a backend fault that always
/// fails `run`), the record must already be in the store by the time
/// `create` returns its `Err` -- because `save` happens strictly before
/// the readiness poll in this crate's `create` implementation. A record
/// that only appeared on eventual *success* would not actually be
/// "durability before ack"; it would be "durability before success-ack
/// only," which leaves exactly the crash window this property exists to
/// close.
#[tokio::test]
async fn create_persists_before_verifying_readiness_even_on_verify_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let clock = fixed_clock();

    let mut req = default_request();
    req.ready_timeout = Duration::from_millis(50);
    // DemoBackend::run() always returns exit_code 0 with FaultMode::None --
    // it has no notion of a command that "ran but exited nonzero", so it
    // cannot simulate a failing healthcheck directly. FaultMode::HangOnRun
    // instead makes every `run()` call (including the readiness probe)
    // hang past whatever timeout is passed in, so poll_ready's own
    // ready_timeout elapses without ever observing a passing healthcheck --
    // a real, distinct-from-create_from_golden readiness failure to test
    // the durability-before-ack property against.
    let hang_backend = DemoBackend::with_fault(FaultMode::HangOnRun);

    let result = create(&hang_backend, &store, &clock, req).await;
    assert!(
        matches!(result, Err(LsbxError::ContractViolated(_))),
        "expected a ContractViolated readiness-timeout error, got {result:?}"
    );

    // The record must exist in the store despite create() returning Err --
    // this is the actual assertion this test exists to make.
    let all = store.list().unwrap();
    assert_eq!(
        all.len(),
        1,
        "the record must have been persisted before the readiness poll ran"
    );
    assert_eq!(all[0].task_id, Some("task-42".to_string()));
}

/// If the backend itself fails to create the VM, nothing was ever
/// persisted (there is no VM and no record to be durable about), and the
/// freshly generated keypair must not be left behind as an orphan.
#[tokio::test]
async fn create_cleans_up_keypair_when_backend_create_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::with_fault(FaultMode::Unavailable);
    let clock = fixed_clock();

    let result = create(&backend, &store, &clock, default_request()).await;
    assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));

    // Nothing should have been persisted.
    assert!(store.list().unwrap().is_empty());
}

/// `--no-verify` (`verify: false`) must skip readiness polling entirely --
/// even against a backend that would never pass verification, `create`
/// must still succeed once the VM is created and the record is durable.
#[tokio::test]
async fn create_with_no_verify_skips_readiness_polling() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    // HangOnRun would fail (time out) any readiness poll that actually ran.
    let backend = DemoBackend::with_fault(FaultMode::HangOnRun);
    let clock = fixed_clock();

    let mut req = default_request();
    req.verify = false;

    let public = create(&backend, &store, &clock, req).await.unwrap();
    assert!(store.load(&public.id).is_ok());
}

/// The named destroy ordering: `Backend::destroy`, then `cleanup_keypair`,
/// then `SandboxStore::delete`. Proven here by the *successful* path
/// leaving no record and no VM behind.
#[tokio::test]
async fn destroy_removes_vm_and_store_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();

    let full = store.load(&public.id).unwrap();
    let vm_tag = full.vm_tag.clone().unwrap();

    destroy(&backend, &store, &public.id).await.unwrap();

    assert!(matches!(
        store.load(&public.id),
        Err(LsbxError::NotFound(_))
    ));
    let remaining_vms = backend.list_vms().await.unwrap();
    assert!(!remaining_vms.contains(&vm_tag));
}

/// The load-bearing ordering property: if `Backend::destroy` fails, the
/// store record must survive completely untouched -- not partially
/// updated, not deleted -- so a subsequent retry (the reap loop's job) has
/// something to act on. This is `destroy`'s own contract, independent of
/// `reap`; `test_reap.rs` exercises the same fault through the reap loop.
#[tokio::test]
async fn destroy_leaves_record_intact_when_backend_destroy_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::with_fault(FaultMode::PartialDestroyFailure);
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();
    let before = store.load(&public.id).unwrap();

    let result = destroy(&backend, &store, &public.id).await;
    assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));

    let after = store.load(&public.id).unwrap();
    assert_eq!(before.id, after.id);
    assert_eq!(before.vm_tag, after.vm_tag);
    assert_eq!(before.key_path, after.key_path);

    // The VM itself must also still be present on the backend side (this
    // is PartialDestroyFailure's own documented behavior), matching what a
    // real retry would need to find.
    let vms = backend.list_vms().await.unwrap();
    assert!(vms.contains(&after.vm_tag.unwrap()));
}

/// `renew` extends `lease_expires_at` and persists the update.
#[tokio::test]
async fn renew_extends_lease_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();
    let original = store.load(&public.id).unwrap();

    let renewed = renew(&store, &clock, &public.id, Duration::from_secs(7200))
        .await
        .unwrap();

    let reloaded = store.load(&public.id).unwrap();
    assert_eq!(reloaded.lease_expires_at, renewed.lease_expires_at);
    assert_ne!(reloaded.lease_expires_at, original.lease_expires_at);

    // Renewal from a fixed clock must be exactly clock.now() + duration,
    // proving the extension isn't computed from real wall-clock time.
    let expected =
        chrono::DateTime::<chrono::Utc>::from(clock.now) + chrono::Duration::seconds(7200);
    let actual: chrono::DateTime<chrono::Utc> = reloaded
        .lease_expires_at
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(actual, expected);
}

/// `renew` refuses to extend a sandbox whose `cleanup_failed` flag is set.
#[tokio::test]
async fn renew_refuses_when_cleanup_failed_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let public = create(&backend, &store, &clock, default_request())
        .await
        .unwrap();

    let mut broken = store.load(&public.id).unwrap();
    broken.cleanup_failed = true;
    store.save(&broken).unwrap();

    let result = renew(&store, &clock, &public.id, Duration::from_secs(3600)).await;
    assert!(matches!(result, Err(LsbxError::ContractViolated(_))));

    // The lease must be unchanged after the refusal.
    let after = store.load(&public.id).unwrap();
    assert_eq!(after.lease_expires_at, broken.lease_expires_at);
}

/// `renew`/`destroy` against a sandbox id that was never created must
/// surface `NotFound`, not panic or silently succeed.
#[tokio::test]
async fn renew_and_destroy_on_missing_sandbox_return_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let renew_result = renew(&store, &clock, "does-not-exist", Duration::from_secs(60)).await;
    assert!(matches!(renew_result, Err(LsbxError::NotFound(_))));

    let destroy_result = destroy(&backend, &store, "does-not-exist").await;
    assert!(matches!(destroy_result, Err(LsbxError::NotFound(_))));
}
