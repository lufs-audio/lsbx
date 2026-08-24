// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and every prior merged unit's own
// tests/*.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::DemoBackend;
use lsbx_kernel::clock::FakeClock;
use lsbx_kernel::types::SandboxRecord;
use lsbx_lifecycle::lease::is_expired;
use lsbx_lifecycle::reap::reap;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

fn record(id: &str, lease_expires_at: Option<String>) -> SandboxRecord {
    SandboxRecord {
        id: id.to_string(),
        name: id.to_string(),
        host: "localhost".to_string(),
        profile: "demo".to_string(),
        flavor: "default".to_string(),
        streaming: "none".to_string(),
        username: None,
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at,
        vm_tag: None,
        https_url: None,
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
}

/// Core determinism proof required by this unit's acceptance criteria: two
/// records with the *same* stored `lease_expires_at` flip from "not
/// expired" to "expired" purely by moving a `FakeClock`'s `now` forward --
/// no real sleep, no wall-clock dependency, and the outcome is exactly
/// reproducible across runs.
#[test]
fn fake_clock_drives_deterministic_expiry_without_real_sleep() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let expires_at = base + Duration::from_secs(60);
    let sbx = record("sbx-fake-clock", Some(rfc3339(expires_at)));

    let before = FakeClock {
        now: expires_at - Duration::from_secs(1),
    };
    assert!(
        !is_expired(&sbx, &before),
        "one second before expiry must not be expired"
    );

    let after = FakeClock {
        now: expires_at + Duration::from_secs(1),
    };
    assert!(
        is_expired(&sbx, &after),
        "one second after expiry must be expired"
    );

    // Re-running the exact same before/after checks must produce the exact
    // same answers -- nothing here depends on when the test happens to run.
    assert!(!is_expired(&sbx, &before));
    assert!(is_expired(&sbx, &after));
}

#[test]
fn no_lease_never_expires_regardless_of_clock_advancement() {
    let sbx = record("sbx-no-lease", None);
    let early = FakeClock {
        now: SystemTime::UNIX_EPOCH,
    };
    let late = FakeClock {
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
    };
    assert!(!is_expired(&sbx, &early));
    assert!(!is_expired(&sbx, &late));
}

/// End-to-end proof that lease-expiry sweeping in `reap` itself is driven
/// entirely by the injected `Clock`, not real time: a sandbox saved with a
/// lease "60 seconds from a fixed FakeClock instant" is untouched by a
/// `reap` call using that same instant, then swept once a `reap` call uses
/// a *later* `FakeClock` instant -- all without the test ever sleeping.
#[tokio::test]
async fn reap_sweep_is_deterministic_under_fake_clock() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let backend = DemoBackend::new();

    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
    let clock_t0 = FakeClock { now: t0 };

    let expires_at = t0 + Duration::from_secs(60);
    store
        .save(&record("sbx-deterministic", Some(rfc3339(expires_at))))
        .unwrap();

    let allowed = HashSet::new();

    // At t0, the lease has not yet expired: nothing should be swept.
    let report_before = reap(&backend, &store, &clock_t0, &allowed, Duration::ZERO, false)
        .await
        .unwrap();
    assert!(report_before.destroyed.is_empty());
    assert!(store.load("sbx-deterministic").is_ok());

    // Advance only the FakeClock (never a real sleep) past the lease.
    let clock_t1 = FakeClock {
        now: expires_at + Duration::from_secs(1),
    };
    let report_after = reap(&backend, &store, &clock_t1, &allowed, Duration::ZERO, false)
        .await
        .unwrap();
    assert_eq!(
        report_after.destroyed,
        vec!["sbx-deterministic".to_string()]
    );
    assert!(matches!(
        store.load("sbx-deterministic"),
        Err(lsbx_kernel::error::LsbxError::NotFound(_))
    ));
}
