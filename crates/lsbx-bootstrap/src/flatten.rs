//! Golden flattening: collapsing a qcow2 backing-file chain into a single
//! self-contained image, via `qemu-img convert` — explicit argv, matching
//! Unit 06's subprocess discipline (never a shell string a path
//! containing a space or apostrophe could corrupt).
//!
//! ## Ordering guarantee this module exists to provide
//! A golden's content hash (Unit 08's `content_hash`) is computed only on
//! this function's output — never on an image that still depends on an
//! external backing file that could change underneath it. This module
//! does not compute that hash itself (Unit 08 owns `content_hash`); it
//! only guarantees flattening happens *before* that hash is computed, via
//! ordering in `golden_build`'s call sequence (Unit 08 calls this
//! function through a narrow trait/callback rather than this crate
//! reaching into Unit 08's registry).

use lsbx_kernel::error::LsbxError;
use std::path::Path;
use std::process::Stdio;

/// Collapses a qcow2 backing-file chain rooted at `source_with_backing`
/// into a single standalone qcow2 image at `dest_standalone`, via
/// `qemu-img convert -O qcow2 <source> <dest>`.
///
/// `qemu-img convert` (unlike `qemu-img rebase -b ""`, which flattens
/// in place) always produces a fresh, standalone output file — reading
/// every backing-file byte through into `dest_standalone` regardless of
/// how deep the backing chain is, which is exactly the "single
/// self-contained image" the unit contract asks for. `source_with_backing`
/// itself is left untouched.
pub async fn flatten(
    source_with_backing: &Path,
    dest_standalone: &Path,
) -> Result<(), LsbxError> {
    if !source_with_backing.exists() {
        return Err(LsbxError::NotFound(format!(
            "flatten source not found: {}",
            source_with_backing.display()
        )));
    }

    let output = tokio::process::Command::new("qemu-img")
        .arg("convert")
        .arg("-O")
        .arg("qcow2")
        .arg(source_with_backing)
        .arg(dest_standalone)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            LsbxError::BackendUnavailable(format!(
                "failed to spawn qemu-img (not on PATH?): {err}"
            ))
        })?;

    if !output.status.success() {
        return Err(LsbxError::ContractViolated(format!(
            "qemu-img convert failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn flatten_errors_not_found_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("does-not-exist.qcow2");
        let dest = dir.path().join("out.qcow2");

        let result = flatten(&source, &dest).await;
        assert!(matches!(result, Err(LsbxError::NotFound(_))));
    }
}
