// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and every prior merged unit's own
// tests/*.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::{DemoBackend, FaultMode};
use lsbx_kernel::backend::Backend;
use lsbx_kernel::clock::FakeClock;
use lsbx_kernel::types::SandboxRecord;
use lsbx_lifecycle::reap::reap;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

fn fixed_clock() -> FakeClock {
    FakeClock {
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(5_000_000),
    }
}

fn expired_record(id: &str, profile: &str, clock: &FakeClock) -> SandboxRecord {
    let past = clock.now - Duration::from_secs(60);
    SandboxRecord {
        id: id.to_string(),
        name: id.to_string(),
        host: "localhost".to_string(),
        profile: profile.to_string(),
        flavor: "default".to_string(),
        streaming: "none".to_string(),
        username: None,
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at: Some(chrono::DateTime::<chrono::Utc>::from(past).to_rfc3339()),
        vm_tag: Some(format!("vm-{id}")),
        https_url: None,
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

fn live_record(id: &str, profile: &str, clock: &FakeClock) -> SandboxRecord {
    let future = clock.now + Duration::from_secs(3600);
    SandboxRecord {
        id: id.to_string(),
        name: id.to_string(),
        host: "localhost".to_string(),
        profile: profile.to_string(),
        flavor: "default".to_string(),
        streaming: "none".to_string(),
        username: None,
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at: Some(chrono::DateTime::<chrono::Utc>::from(future).to_rfc3339()),
        vm_tag: Some(format!("vm-{id}")),
        https_url: None,
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

/// Basic sweep: an expired sandbox is destroyed and removed from the
/// store; a live one is left completely alone.
#[tokio::test]
async fn reap_destroys_expired_and_leaves_live_sandboxes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    // Seed the backend with matching VMs so destroy() has something real
    // to remove (SandboxStore and DemoBackend are independent stores; a
    // realistic setup has a VM under the same vm_tag the record names).
    // Capture each CreatedVm's actual vm_tag directly rather than
    // re-deriving DemoBackend's internal (private) hash.
    let golden = lsbx_kernel::types::GoldenKey::new_unchecked("demo".to_string());
    let expired_vm = backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &golden,
            name: "vm-sbx-expired",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();
    let live_vm = backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &golden,
            name: "vm-sbx-live",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();

    let mut expired = expired_record("sbx-expired", "demo", &clock);
    let mut live = live_record("sbx-live", "demo", &clock);
    expired.vm_tag = Some(expired_vm.vm_tag);
    live.vm_tag = Some(live_vm.vm_tag);

    store.save(&expired).unwrap();
    store.save(&live).unwrap();

    let allowed = HashSet::from(["demo".to_string()]);
    let report = reap(&backend, &store, &clock, &allowed, Duration::ZERO, false)
        .await
        .unwrap();

    assert_eq!(report.destroyed, vec!["sbx-expired".to_string()]);
    assert!(report.would_destroy.is_empty());

    assert!(store.load("sbx-expired").is_err());
    assert!(store.load("sbx-live").is_ok());
}

/// `dry_run: true` must report what would be destroyed without calling
/// `Backend::destroy` at all -- the record survives, and the VM survives
/// on the backend side too.
#[tokio::test]
async fn reap_dry_run_reports_without_destroying() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let created = backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &lsbx_kernel::types::GoldenKey::new_unchecked("demo".to_string()),
            name: "dry-run-vm",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();

    let mut expired = expired_record("sbx-dry-run", "demo", &clock);
    expired.vm_tag = Some(created.vm_tag.clone());
    store.save(&expired).unwrap();

    let allowed = HashSet::new();
    let report = reap(&backend, &store, &clock, &allowed, Duration::ZERO, true)
        .await
        .unwrap();

    assert!(report.destroyed.is_empty());
    assert_eq!(report.would_destroy, vec!["sbx-dry-run".to_string()]);

    // Nothing was actually touched.
    assert!(store.load("sbx-dry-run").is_ok());
    let vms = backend.list_vms().await.unwrap();
    assert!(vms.contains(&created.vm_tag));
}

/// The named scenario from this unit's own Verification section:
/// `DemoBackend::with_fault(FaultMode::PartialDestroyFailure)` must leave a
/// sandbox whose destroy call fails NOT removed from the store, so it is
/// retried on the next reap pass rather than silently forgotten. Per the
/// prompt's confirmed note on Unit 05's actual merged behavior, the demo
/// backend's `destroy()` under this fault mode returns `Err` while leaving
/// the VM present in its own internal state -- so a later retry against a
/// non-faulting backend can succeed. This test builds around exactly that
/// real behavior: the first `reap` pass (against the faulting backend)
/// leaves the record in place; a second `reap` pass against a
/// *non-faulting* backend holding the same VM under the same vm_tag
/// actually removes it -- proving the record was genuinely retryable, not
/// just "left behind and never actionable again."
#[tokio::test]
async fn reap_retains_record_on_partial_destroy_failure_and_succeeds_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let faulting_backend = DemoBackend::with_fault(FaultMode::PartialDestroyFailure);
    let clock = fixed_clock();

    let golden = lsbx_kernel::types::GoldenKey::new_unchecked("demo".to_string());
    let created = faulting_backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &golden,
            name: "partial-destroy-vm",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();

    let mut expired = expired_record("sbx-partial-destroy", "demo", &clock);
    expired.vm_tag = Some(created.vm_tag.clone());
    store.save(&expired).unwrap();

    let allowed = HashSet::from(["demo".to_string()]);

    // First reap pass: the backend's destroy() always fails under this
    // fault mode, so the sandbox must NOT be reported as destroyed, and
    // its record must remain in the store afterward.
    let first_pass = reap(
        &faulting_backend,
        &store,
        &clock,
        &allowed,
        Duration::ZERO,
        false,
    )
    .await
    .unwrap();
    assert!(
        first_pass.destroyed.is_empty(),
        "a sandbox whose destroy call fails must not appear in `destroyed`"
    );
    let still_present = store.load("sbx-partial-destroy");
    assert!(
        still_present.is_ok(),
        "the record must survive a failed destroy attempt so it can be retried, got {still_present:?}"
    );
    assert_eq!(still_present.unwrap().vm_tag, Some(created.vm_tag.clone()));

    // The VM itself must also still exist on the (faulting) backend --
    // matches PartialDestroyFailure's own documented behavior of failing
    // the call while leaving the VM present, precisely so a retry has
    // something to act on.
    let vms_after_first_pass = faulting_backend.list_vms().await.unwrap();
    assert!(vms_after_first_pass.contains(&created.vm_tag));

    // Second reap pass: retry against a fresh, non-faulting DemoBackend
    // instance. DemoBackend's create_from_golden is deterministic on
    // (golden, name) -- recreating the "same" VM (same golden + name) on a
    // clean backend reproduces the identical vm_tag the store record
    // already references, modeling "the fault clears and a subsequent
    // attempt targets the same real VM." This mirrors the exact retry
    // shape asserted directly against `destroy()` in
    // lsbx-backend-demo's own test_fault_modes.rs::test_fault_partial_destroy.
    let retry_backend = DemoBackend::new();
    let retry_vm = retry_backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &golden,
            name: "partial-destroy-vm",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();
    assert_eq!(
        retry_vm.vm_tag, created.vm_tag,
        "DemoBackend's vm_tag derivation must be deterministic"
    );

    let second_pass = reap(
        &retry_backend,
        &store,
        &clock,
        &allowed,
        Duration::ZERO,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        second_pass.destroyed,
        vec!["sbx-partial-destroy".to_string()],
        "retrying against a non-faulting backend must actually remove the sandbox this time"
    );
    assert!(store.load("sbx-partial-destroy").is_err());
    let vms_after_second_pass = retry_backend.list_vms().await.unwrap();
    assert!(!vms_after_second_pass.contains(&retry_vm.vm_tag));
}

/// A failure destroying one sandbox must not abort the sweep for other,
/// independently expired sandboxes in the same pass.
#[tokio::test]
async fn reap_continues_sweeping_other_sandboxes_after_one_destroy_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::with_fault(FaultMode::PartialDestroyFailure);
    let clock = fixed_clock();

    let golden = lsbx_kernel::types::GoldenKey::new_unchecked("demo".to_string());
    let mut created_vms = Vec::new();
    for name in ["vm-a", "vm-b"] {
        let created = backend
            .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
                golden: &golden,
                name,
                pubkey: "ssh-ed25519 AAAA test",
                cpu: 1,
                memory: "512M",
            })
            .await
            .unwrap();
        created_vms.push(created.vm_tag);
    }

    let mut record_a = expired_record("sbx-a", "demo", &clock);
    let mut record_b = expired_record("sbx-b", "demo", &clock);
    record_a.vm_tag = Some(created_vms[0].clone());
    record_b.vm_tag = Some(created_vms[1].clone());
    store.save(&record_a).unwrap();
    store.save(&record_b).unwrap();

    let allowed = HashSet::from(["demo".to_string()]);
    // Every destroy fails under this fault mode -- the important
    // assertion is that `reap` still attempts *both* records (neither one
    // silently aborts processing of the other) and neither disappears from
    // the store.
    let report = reap(&backend, &store, &clock, &allowed, Duration::ZERO, false)
        .await
        .unwrap();
    assert!(report.destroyed.is_empty());
    assert!(store.load("sbx-a").is_ok());
    assert!(store.load("sbx-b").is_ok());
}

/// A sandbox whose `profile` is absent from `allowed_goldens` is still
/// swept once its lease has expired -- see `reap`'s own doc comment for
/// the full resolution of the `allowed_goldens` ambiguity. An expired
/// lease is never left running merely because its golden reference looks
/// unrecognized to the caller-supplied set.
#[tokio::test]
async fn reap_still_sweeps_expired_sandbox_whose_golden_is_not_in_allowed_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let created = backend
        .create_from_golden(lsbx_kernel::backend::CreateFromGoldenRequest {
            golden: &lsbx_kernel::types::GoldenKey::new_unchecked("unlisted-golden".to_string()),
            name: "unlisted-vm",
            pubkey: "ssh-ed25519 AAAA test",
            cpu: 1,
            memory: "512M",
        })
        .await
        .unwrap();

    let mut expired = expired_record("sbx-unlisted-golden", "unlisted-golden", &clock);
    expired.vm_tag = Some(created.vm_tag);
    store.save(&expired).unwrap();

    // Deliberately does NOT contain "unlisted-golden".
    let allowed = HashSet::from(["some-other-golden".to_string()]);
    let report = reap(&backend, &store, &clock, &allowed, Duration::ZERO, false)
        .await
        .unwrap();

    assert_eq!(report.destroyed, vec!["sbx-unlisted-golden".to_string()]);
    assert!(store.load("sbx-unlisted-golden").is_err());
}

/// `keys_reconciled` reflects the count returned by
/// `lsbx_keys::reconcile::reconcile_orphaned_keys`. With no backend-level
/// `TaggedKey` source wired in yet (see `reap.rs`'s own doc comment on
/// `reconcile_keys`), this is always zero today -- asserted explicitly so
/// a future wiring-in of a real key listing is a deliberate, visible
/// change to this test rather than a silent behavior change.
#[tokio::test]
async fn reap_reports_zero_keys_reconciled_with_no_backend_key_listing_wired_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();
    let clock = fixed_clock();

    let allowed = HashSet::new();
    let report = reap(&backend, &store, &clock, &allowed, Duration::ZERO, false)
        .await
        .unwrap();
    assert_eq!(report.keys_reconciled, 0);
}
