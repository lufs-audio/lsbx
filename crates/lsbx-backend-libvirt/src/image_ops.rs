//! Subprocess wrappers around `qemu-img` (SPEC.md Deviation 7: no mature
//! native Rust `qemu-img` binding exists, so this one operation still
//! shells out — but only ever via explicit argv, never a shell string a
//! path containing a space or apostrophe could corrupt).
//!
//! Both Jules candidates for this unit implemented essentially this same
//! module and both invented `LsbxError::Internal`, which does not exist on
//! the real, merged `lsbx_kernel::error::LsbxError` (7 variants only, no
//! `Internal`). Every failure here is mapped to
//! [`lsbx_kernel::error::LsbxError::BackendUnavailable`]: a failing
//! `qemu-img` invocation is exactly "the external tool this backend depends
//! on to do its job did not work" — the same shape of failure as a libvirt
//! socket being down, which is what `BackendUnavailable` already means
//! elsewhere in this crate. `ContractViolated` was the other candidate
//! variant worth considering (in the sense that a corrupt/wrong-format
//! input image is a violated expectation about the golden), but that
//! framing fits better at the *golden registry* layer (Unit 08, which
//! already owns the golden's declared shape) than here, where all this
//! module sees is "a subprocess exited non-zero" — it has no way to
//! distinguish "qemu-img itself is missing/broken" from "the input file was
//! bad" from its own vantage point, and `BackendUnavailable` is the
//! correct default for that ambiguity.

use lsbx_kernel::error::LsbxError;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Runs `qemu-img`, explicit argv only, with stdin from `/dev/null` (the
/// same non-interactive isolation policy the unit contract requires for
/// guest SSH commands — a subprocess that never expects input shouldn't
/// ever be able to block on inherited stdin either).
async fn run_qemu_img(args: &[&std::ffi::OsStr], action: &str) -> Result<(), LsbxError> {
    let output = Command::new("qemu-img")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            LsbxError::BackendUnavailable(format!("failed to spawn qemu-img {action}: {e}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LsbxError::BackendUnavailable(format!(
            "qemu-img {action} failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        )));
    }

    Ok(())
}

/// Converts `source` to `dest` in the given target `format` (e.g. `qcow2`,
/// `raw`) via `qemu-img convert -O <format> <source> <dest>`.
///
/// Used by [`crate::create_from_golden`] when a golden's `mode` is `"new"`:
/// rather than a copy-on-write clone against the golden's own qcow2, a
/// fresh, independent disk is materialized for the new VM.
pub async fn qemu_img_convert(source: &Path, dest: &Path, format: &str) -> Result<(), LsbxError> {
    run_qemu_img(
        &[
            std::ffi::OsStr::new("convert"),
            std::ffi::OsStr::new("-O"),
            std::ffi::OsStr::new(format),
            source.as_os_str(),
            dest.as_os_str(),
        ],
        "convert",
    )
    .await
}

/// Creates a new qcow2 file at `dest` backed by `backing_file`, via
/// `qemu-img create -f qcow2 -F qcow2 -b <backing_file> <dest>`.
///
/// Used by [`crate::create_from_golden`] when a golden's `mode` is
/// `"copy"`: the new VM's disk is a copy-on-write overlay on top of the
/// golden's own qcow2, so the golden itself is never mutated and multiple
/// VMs can share one golden's bytes on disk.
pub async fn qemu_img_create_cow(backing_file: &Path, dest: &Path) -> Result<(), LsbxError> {
    run_qemu_img(
        &[
            std::ffi::OsStr::new("create"),
            std::ffi::OsStr::new("-f"),
            std::ffi::OsStr::new("qcow2"),
            std::ffi::OsStr::new("-F"),
            std::ffi::OsStr::new("qcow2"),
            std::ffi::OsStr::new("-b"),
            backing_file.as_os_str(),
            dest.as_os_str(),
        ],
        "create",
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// `qemu-img` isn't necessarily installed in every environment this
    /// crate's unit tests run in (it isn't in this sandbox either — see the
    /// PR description). This test only asserts the *failure path* maps to
    /// the right `LsbxError` variant when the binary can't even be found,
    /// which requires no real qemu-img install and exercises the same
    /// `map_err` path a genuinely broken qemu-img would hit.
    #[tokio::test]
    async fn missing_qemu_img_binary_maps_to_backend_unavailable() {
        let result = run_qemu_img(
            &[std::ffi::OsStr::new("--this-flag-does-not-exist")],
            "probe",
        )
        .await;
        // Either qemu-img isn't on PATH at all (spawn fails -> BackendUnavailable)
        // or it is and rejects the bogus flag (non-zero exit -> BackendUnavailable).
        // Both paths must land on the same variant.
        assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));
    }
}
