//! Real end-to-end `golden_build` test against the real, merged
//! `lsbx-backend-demo::DemoBackend` (Unit 05) — no hand-rolled mock
//! `Backend` implementation, per this unit's task instructions, since a
//! real, already-merged conformance-tested backend is available and using
//! it exercises the actual trait signature this crate depends on rather
//! than a possibly-drifted local double.
//!
//! This is the compile-time proof that `golden_build`'s rewritten control
//! flow (`create_from_golden` -> `put_file` -> `run` -> optional `destroy`
//! -> flatten-seam -> `content_hash`) actually satisfies the real
//! `Backend` trait's method signatures end to end, not just that the
//! individual unit tests inside `build.rs` pass in isolation.

// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test]/#[tokio::test], so a failed unwrap()/expect() only ever panics
// inside `cargo test`, never in a shipped code path. clippy::unwrap_used /
// expect_used are restriction-group lints that don't understand "this whole
// file is test code" the way #[cfg(test)] does, so they fire here even
// though this unit's own acceptance criteria (and every other unit's test
// files) rely on idiomatic unwrap()-based assertions. Allow both, scoped to
// this file only — crates/lsbx-golden/src/**/*.rs (the real production code
// path) is unwrap/expect/panic-free under the same workspace lints with no
// allow needed. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::DemoBackend;
use lsbx_golden::build::{golden_build, GoldenBuildRequest, GoldenFlattener};
use lsbx_golden::registry::{GoldenFlavor, StreamingMode};
use lsbx_kernel::backend::Backend as _;
use lsbx_kernel::error::LsbxError;
use std::path::PathBuf;

/// Minimal `GoldenFlattener` stand-in for this integration test. This is
/// NOT a substitute for Unit 19's real qemu-img-backed flatten
/// implementation — it exists purely so this test can exercise steps 1-4
/// and 6 of `golden_build`'s flow against the real `DemoBackend` without
/// also depending on Unit 19, which has not landed. It simply returns a
/// pre-written file as the "flattened" disk.
struct StubFlattener {
    flattened_path: PathBuf,
}

#[async_trait::async_trait]
impl GoldenFlattener for StubFlattener {
    async fn flatten(&self, _vm_tag: &str) -> Result<PathBuf, LsbxError> {
        Ok(self.flattened_path.clone())
    }
}

#[tokio::test]
async fn golden_build_end_to_end_against_real_demo_backend() {
    let backend = DemoBackend::new();
    let dir = tempfile::tempdir().expect("tempdir");

    let script_path = dir.path().join("provision.sh");
    std::fs::write(&script_path, "#!/bin/sh\napt-get install -y openssh-server\n")
        .expect("write provisioning script");

    let flattened_path = dir.path().join("agent-base.qcow2");
    std::fs::write(&flattened_path, b"pretend flattened golden qcow2 bytes")
        .expect("write flattened disk stand-in");
    let flattener = StubFlattener {
        flattened_path: flattened_path.clone(),
    };

    let outcome = golden_build(
        &backend,
        GoldenBuildRequest {
            name: "agent-base",
            from: "lsbx-default-v1",
            script: &script_path,
            flavor: GoldenFlavor::Agent,
            cpu: 2,
            memory: "2G",
            streaming: StreamingMode::None,
            register: false,
            cleanup: true,
            dry_run: false,
            pubkey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGRlbW8ta2V5 lsbx:unit08-test",
        },
        Some(&flattener),
    )
    .await
    .expect("golden_build should succeed end-to-end against the real DemoBackend");

    assert_eq!(outcome.config.key, "agent-base");
    assert_eq!(outcome.config.base, "lsbx-default-v1");
    assert_eq!(outcome.config.flavor, GoldenFlavor::Agent);

    // Content hash must be the real sha256-derived value over the
    // "flattened" disk, matching SPEC.md Deviation 3 ("populated on every
    // golden this build path produces").
    let expected_hash = lsbx_golden::content_hash(&flattened_path).expect("compute expected hash");
    assert_eq!(outcome.config.content_hash, Some(expected_hash));

    // cleanup: true, so the real DemoBackend must show zero VMs afterward
    // -- proof that `destroy` was actually called against the real trait
    // method, not skipped.
    let remaining_vms = backend.list_vms().await.expect("list_vms should succeed");
    assert!(
        remaining_vms.is_empty(),
        "build VM must be destroyed when cleanup=true, found: {:?}",
        remaining_vms
    );
}

#[tokio::test]
async fn golden_build_with_cleanup_false_leaves_a_real_demo_vm_alive() {
    let backend = DemoBackend::new();
    let dir = tempfile::tempdir().expect("tempdir");

    let script_path = dir.path().join("provision.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho done\n").expect("write script");

    let flattened_path = dir.path().join("agent-base.qcow2");
    std::fs::write(&flattened_path, b"more pretend flattened bytes").expect("write flattened disk");
    let flattener = StubFlattener {
        flattened_path: flattened_path.clone(),
    };

    let outcome = golden_build(
        &backend,
        GoldenBuildRequest {
            name: "agent-base",
            from: "lsbx-default-v1",
            script: &script_path,
            flavor: GoldenFlavor::Agent,
            cpu: 2,
            memory: "2G",
            streaming: StreamingMode::None,
            register: false,
            cleanup: false,
            dry_run: false,
            pubkey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGRlbW8ta2V5 lsbx:unit08-test",
        },
        Some(&flattener),
    )
    .await
    .expect("golden_build should succeed");

    let build_vm_tag = outcome.build_vm_tag.expect("cleanup=false should return the build VM tag");
    let remaining_vms = backend.list_vms().await.expect("list_vms should succeed");
    assert_eq!(remaining_vms, vec![build_vm_tag]);
}

#[tokio::test]
async fn golden_build_dry_run_never_calls_the_real_backend() {
    let backend = DemoBackend::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("provision.sh");
    std::fs::write(&script_path, "#!/bin/sh\n").expect("write script");

    let outcome = golden_build(
        &backend,
        GoldenBuildRequest {
            name: "agent-base",
            from: "lsbx-default-v1",
            script: &script_path,
            flavor: GoldenFlavor::Agent,
            cpu: 2,
            memory: "2G",
            streaming: StreamingMode::None,
            register: false,
            cleanup: true,
            dry_run: true,
            pubkey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGRlbW8ta2V5 lsbx:unit08-test",
        },
        None,
    )
    .await
    .expect("dry run should succeed without a flattener");

    assert_eq!(outcome.config.content_hash, Some("lufs-dryrun".to_string()));
    assert!(backend.list_vms().await.expect("list_vms").is_empty());
}
