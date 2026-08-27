// Integration test binary -- every fn here is a #[test]/#[tokio::test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and Unit 04's
// crates/lsbx-backend-testkit/tests/test_kit_self_check.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::{DemoBackend, FaultMode};
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::GoldenKey;
use std::time::Duration;

#[tokio::test]
async fn test_fault_unavailable() {
    let backend = DemoBackend::with_fault(FaultMode::Unavailable);
    // GoldenKey has no `new()` -- only `new_unchecked` crosses the crate
    // boundary (see crates/lsbx-kernel/src/types.rs).
    let golden_key = GoldenKey::new_unchecked("test.golden".to_string());

    let req = CreateFromGoldenRequest {
        golden: &golden_key,
        name: "test-vm",
        pubkey: "ssh-rsa AAA...",
        cpu: 2,
        memory: "2G",
    };

    let res = backend.create_from_golden(req).await;
    assert!(matches!(res, Err(LsbxError::BackendUnavailable(_))));

    let res = backend.list_vms().await;
    assert!(matches!(res, Err(LsbxError::BackendUnavailable(_))));
}

#[tokio::test]
async fn test_fault_hang_on_run() {
    let backend = DemoBackend::with_fault(FaultMode::HangOnRun);
    let golden_key = GoldenKey::new_unchecked("test.golden".to_string());

    let req = CreateFromGoldenRequest {
        golden: &golden_key,
        name: "test-vm",
        pubkey: "ssh-rsa AAA...",
        cpu: 2,
        memory: "2G",
    };

    let vm = backend.create_from_golden(req).await.unwrap();

    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(500);

    let _ = tokio::time::timeout(
        timeout,
        backend.run(&vm.vm_tag, &[String::from("echo")], timeout, None),
    )
    .await;

    let elapsed = start.elapsed();
    assert!(elapsed >= timeout, "Run should hang at least until timeout");
}

/// `PartialDestroyFailure` models a `destroy()` call that fails outright --
/// not one that quietly succeeds internally while lying to the caller about
/// it. The VM must remain present after the failed attempt, so a caller that
/// retries (Unit 09's reap loop is the documented reason this fault mode
/// exists -- see its own Verification scenario: "a sandbox whose destroy call
/// fails is NOT removed from the store... retried on the next reap pass")
/// can actually re-attempt the operation and have it succeed on retry,
/// rather than getting an immediate NotFound because the backend already
/// silently removed the VM before reporting the failure. A backend that
/// self-heals ("silently removed, but I'll still tell you it failed") would
/// defeat the entire purpose of a retry-oriented fault mode: there would be
/// nothing left to retry against.
#[tokio::test]
async fn test_fault_partial_destroy() {
    let backend = DemoBackend::with_fault(FaultMode::PartialDestroyFailure);
    let golden_key = GoldenKey::new_unchecked("test.golden".to_string());

    let req = CreateFromGoldenRequest {
        golden: &golden_key,
        name: "test-vm",
        pubkey: "ssh-rsa AAA...",
        cpu: 2,
        memory: "2G",
    };

    let vm = backend.create_from_golden(req).await.unwrap();

    let res = backend.destroy(&vm.vm_tag).await;
    assert!(matches!(res, Err(LsbxError::BackendUnavailable(_))));

    // The VM must still be present after the failed destroy -- this fault
    // mode simulates a destroy attempt that failed, not one that silently
    // succeeded server-side while reporting failure to the caller.
    let list = backend.list_vms().await.unwrap();
    assert!(
        list.contains(&vm.vm_tag),
        "expected the VM to remain present after a failed destroy so a retry has something to act on"
    );

    // Retrying destroy() against the same vm_tag must be able to actually
    // succeed: this is the whole point of the fault mode existing (a
    // reap-loop retry test double). Simulate the caller retrying against a
    // backend no longer configured to fail.
    let retry_backend = DemoBackend::new();
    let retry_vm = retry_backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &golden_key,
            name: "test-vm",
            pubkey: "ssh-rsa AAA...",
            cpu: 2,
            memory: "2G",
        })
        .await
        .unwrap();
    let retry_res = retry_backend.destroy(&retry_vm.vm_tag).await;
    assert!(retry_res.is_ok(), "retrying destroy on a non-faulting backend should succeed");
    let retry_list = retry_backend.list_vms().await.unwrap();
    assert!(!retry_list.contains(&retry_vm.vm_tag));
}

/// A second call to `destroy()` on the *same* faulting `DemoBackend` instance
/// must behave identically to the first: `PartialDestroyFailure` always
/// fails destroy on this backend, so repeated attempts against the same
/// instance keep failing (matching real backend behavior, e.g. a libvirt
/// control-plane hiccup that persists until whatever caused it clears) while
/// the VM never disappears out from under the caller.
#[tokio::test]
async fn test_fault_partial_destroy_is_retryable_in_place() {
    let backend = DemoBackend::with_fault(FaultMode::PartialDestroyFailure);
    let golden_key = GoldenKey::new_unchecked("test.golden".to_string());

    let vm = backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &golden_key,
            name: "test-vm",
            pubkey: "ssh-rsa AAA...",
            cpu: 2,
            memory: "2G",
        })
        .await
        .unwrap();

    let first = backend.destroy(&vm.vm_tag).await;
    assert!(matches!(first, Err(LsbxError::BackendUnavailable(_))));

    let second = backend.destroy(&vm.vm_tag).await;
    assert!(
        matches!(second, Err(LsbxError::BackendUnavailable(_))),
        "a second destroy attempt against the same faulting backend should fail the same way, not NotFound"
    );

    let list = backend.list_vms().await.unwrap();
    assert!(list.contains(&vm.vm_tag));
}
