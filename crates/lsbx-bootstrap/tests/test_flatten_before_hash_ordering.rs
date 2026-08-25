//! Verifies `flatten()` actually collapses a qcow2 backing-file chain,
//! not just that "some error occurs" or "the function returns Ok".
//!
//! This builds a genuine two-level backing-file chain with real
//! `qemu-img create -b` invocations (base image -> mid image backed by
//! base -> confirming the mid image really does have a
//! `backing-filename` before flatten runs), flattens the mid image, and
//! then asks `qemu-img info --output=json` on the *flattened output*
//! whether a `backing-filename` field is present at all. A flatten that
//! silently no-ops (e.g. a bug that just copies the file byte-for-byte
//! without actually running `qemu-img convert`) would leave the backing
//! reference in the copied file and this test would catch it; a flatten
//! that fails outright would be caught by `flatten()` returning `Err`.
//! Only a flatten that both succeeds *and* actually produces a
//! standalone image passes.
//!
//! `qemu-img` availability: installed into this sandbox from the system
//! package manager (`sudo dnf install -y qemu-img`) ahead of running this
//! suite — version 9.2.3, confirmed via `qemu-img --version` before
//! writing this test. If a CI environment lacks `qemu-img` entirely,
//! `flatten()` itself will surface that as
//! `LsbxError::BackendUnavailable` (see `src/flatten.rs`), which is a
//! separate, already-covered unit-test case
//! (`flatten_errors_not_found_when_source_missing` covers the missing-
//! source path; a missing-`qemu-img` environment is covered by
//! `flatten`'s own `map_err` on subprocess spawn failure). This
//! integration test assumes `qemu-img` is present, since verifying the
//! real flatten *behavior* — not just its error handling — requires it.

// See tests/test_verify_host.rs for the full rationale: this file-scoped
// allow matches the established convention in every other merged unit's
// tests/*.rs files (e.g. lsbx-kernel/tests/test_kernel.rs) — restriction-
// group lints fire on ordinary unwrap()/expect() test assertions, which
// #[cfg(test)] alone doesn't suppress for a tests/*.rs integration file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_bootstrap::flatten;
use std::path::Path;
use std::process::Command;

fn qemu_img_available() -> bool {
    Command::new("qemu-img")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn qemu_img_create_raw_base(path: &Path, size: &str) {
    let status = Command::new("qemu-img")
        .args(["create", "-f", "qcow2", path.to_str().unwrap(), size])
        .status()
        .expect("failed to spawn qemu-img create (base)");
    assert!(status.success(), "qemu-img create (base) failed");
}

fn qemu_img_create_backed(path: &Path, backing: &Path) {
    let status = Command::new("qemu-img")
        .args([
            "create",
            "-f",
            "qcow2",
            "-b",
            backing.to_str().unwrap(),
            "-F",
            "qcow2",
            path.to_str().unwrap(),
        ])
        .status()
        .expect("failed to spawn qemu-img create (backed)");
    assert!(status.success(), "qemu-img create -b (backed image) failed");
}

fn qemu_img_info_json(path: &Path) -> serde_json::Value {
    let output = Command::new("qemu-img")
        .args(["info", "--output=json", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn qemu-img info");
    assert!(output.status.success(), "qemu-img info failed");
    serde_json::from_slice(&output.stdout).expect("qemu-img info --output=json produced invalid JSON")
}

#[tokio::test]
async fn flatten_collapses_backing_chain_into_standalone_image() {
    if !qemu_img_available() {
        eprintln!("SKIP: qemu-img not available in this environment — cannot exercise real flatten behavior");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let base_path = dir.path().join("base.qcow2");
    let backed_path = dir.path().join("backed.qcow2");
    let flattened_path = dir.path().join("flattened.qcow2");

    // 1. Build a genuine base image.
    qemu_img_create_raw_base(&base_path, "16M");

    // 2. Build a genuine backing-file chain: backed.qcow2 -> base.qcow2.
    qemu_img_create_backed(&backed_path, &base_path);

    // 3. Sanity-check the *input* really does have a backing file before
    //    we claim flatten removed one — otherwise this test would pass
    //    trivially against an already-standalone image.
    let backed_info = qemu_img_info_json(&backed_path);
    assert!(
        backed_info.get("backing-filename").is_some(),
        "test setup invariant violated: the backed image must have a backing-filename before flatten runs, got: {backed_info}"
    );

    // 4. Run the real flatten() under test.
    flatten::flatten(&backed_path, &flattened_path)
        .await
        .expect("flatten() should succeed against a real backing-file chain");

    // 5. The flattened output must exist and have NO backing-filename at
    //    all — this is the actual behavioral claim, not just "flatten
    //    returned Ok".
    assert!(flattened_path.exists(), "flatten() must produce dest_standalone");
    let flattened_info = qemu_img_info_json(&flattened_path);
    assert!(
        flattened_info.get("backing-filename").is_none(),
        "flattened image must have no backing-filename field, got: {flattened_info}"
    );

    // 6. The flattened image must still be readable as a valid,
    //    complete qcow2 (not just an empty stub) — its declared virtual
    //    size should match the original chain's.
    assert_eq!(
        flattened_info.get("virtual-size"),
        backed_info.get("virtual-size"),
        "flattened image's virtual size must match the original backed image"
    );
}

#[tokio::test]
async fn flatten_leaves_source_untouched() {
    if !qemu_img_available() {
        eprintln!("SKIP: qemu-img not available in this environment");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let base_path = dir.path().join("base2.qcow2");
    let backed_path = dir.path().join("backed2.qcow2");
    let flattened_path = dir.path().join("flattened2.qcow2");

    qemu_img_create_raw_base(&base_path, "16M");
    qemu_img_create_backed(&backed_path, &base_path);

    flatten::flatten(&backed_path, &flattened_path)
        .await
        .expect("flatten() should succeed");

    // The source (backed_path) must still have its backing-filename —
    // flatten() must not have rebased or mutated the source in place.
    let source_info_after = qemu_img_info_json(&backed_path);
    assert!(
        source_info_after.get("backing-filename").is_some(),
        "flatten() must not mutate its source image in place"
    );
}
