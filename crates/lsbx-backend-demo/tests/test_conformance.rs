// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and Unit 04's
// crates/lsbx-backend-testkit/tests/test_kit_self_check.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::DemoBackend;
use lsbx_backend_testkit::run_conformance_suite;
use lsbx_kernel::backend::*;
use lsbx_kernel::types::GoldenKey;

#[tokio::test]
async fn test_demo_conformance() {
    let backend = DemoBackend::new();
    // GoldenKey's inner field is private outside lsbx-kernel; the only
    // cross-crate constructor is `new_unchecked` (no validation performed,
    // no Result -- see crates/lsbx-kernel/src/types.rs). There is no
    // `GoldenKey::new`.
    let golden_key = GoldenKey::new_unchecked("base.golden".to_string());
    let report = run_conformance_suite(&backend, &golden_key).await;

    assert!(
        report.all_passed(),
        "DemoBackend failed conformance suite: {:?}",
        report.checks
    );
}

#[tokio::test]
async fn deterministic_vm_tag() {
    let backend1 = DemoBackend::new();
    let backend2 = DemoBackend::new();
    let golden_key = GoldenKey::new_unchecked("test.golden".to_string());

    let req1 = CreateFromGoldenRequest {
        golden: &golden_key,
        name: "test-vm",
        pubkey: "ssh-rsa AAA...",
        cpu: 2,
        memory: "2G",
    };

    let req2 = CreateFromGoldenRequest {
        golden: &golden_key,
        name: "test-vm",
        pubkey: "ssh-rsa AAA...",
        cpu: 2,
        memory: "2G",
    };

    let vm1 = backend1.create_from_golden(req1).await.unwrap();
    let vm2 = backend2.create_from_golden(req2).await.unwrap();

    assert_eq!(vm1.vm_tag, vm2.vm_tag);
    assert_eq!(vm1.host, vm2.host);
}
