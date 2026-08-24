//! Resolves a [`lsbx_kernel::types::GoldenKey`] to an on-disk qcow2 path.
//!
//! ## Why this crate has to own this at all
//!
//! `CreateFromGoldenRequest` (the real, merged kernel type) carries no disk
//! path field whatsoever — only `golden: &GoldenKey`, `name`, `pubkey`,
//! `cpu`, `memory`. Per the unit's own Boundaries section, Unit 08
//! (`lsbx-golden`) owns manifest parsing and is not built yet; this unit
//! only ever receives an already-validated `GoldenKey`. Something still has
//! to turn that key into a real file path before `qemu-img`/libvirt can do
//! anything with it, and the unit contract explicitly assigns that job to
//! this crate ("resolving a golden's on-disk qcow2 path from its
//! `GoldenKey`/`name` is this crate's own job").
//!
//! ## The path convention (Unit 08 needs to agree with this later)
//!
//! **A configured images directory, joined with the golden's key plus a
//! `.qcow2` suffix**: `{images_dir}/{golden_key}.qcow2`.
//!
//! This mirrors Unit 08's own contract almost exactly: `GoldenConfig.base`
//! is validated with "a trailing `.qcow2` stripped before matching" — i.e.
//! Unit 08's own manifest schema already treats "golden key/base plus
//! `.qcow2` suffix, relative to *some* base images directory" as the
//! natural on-disk shape for a golden, this module just makes that
//! directory configurable and explicit rather than assumed. `images_dir` is
//! deliberately a parameter here (via [`GoldenDiskConfig`]), not a
//! hardcoded path, so whatever directory convention Unit 08 (or Unit 19's
//! host bootstrap, which owns "state directories exist with correct
//! permissions") settles on for real can be threaded in without changing
//! this module's logic — this crate does not invent a system-wide default
//! path itself.
//!
//! **Flagged explicitly for Unit 08**: when Unit 08 lands, it should either
//! (a) adopt this exact convention for wherever it writes a golden's qcow2
//! after `golden build`/`golden register`, or (b) if it needs something
//! different (e.g. a hash-namespaced layout for the new content-hash
//! naming scheme, SPEC.md Deviation 3), this module's `resolve()` should be
//! updated to match at that point — this crate has no test fixture proving
//! the two agree yet, since Unit 08's registry doesn't exist to compare
//! against. That reconciliation is out of scope for this unit and is
//! called out again in the PR description.

use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::GoldenKey;
use std::path::PathBuf;

/// Where this backend should look for golden qcow2 images.
#[derive(Debug, Clone)]
pub struct GoldenDiskConfig {
    pub images_dir: PathBuf,
}

impl GoldenDiskConfig {
    pub fn new(images_dir: impl Into<PathBuf>) -> Self {
        Self {
            images_dir: images_dir.into(),
        }
    }

    /// Resolves `golden` to `{images_dir}/{golden_key}.qcow2`.
    ///
    /// This function does not check the path exists on disk — callers
    /// (e.g. [`crate::create_from_golden`]) are expected to attempt the
    /// actual `qemu-img`/libvirt operation against the resolved path and
    /// surface *that* failure, since "the file doesn't exist" and "the file
    /// exists but `qemu-img` can't read it" both deserve the same
    /// `LsbxError::BackendUnavailable` treatment `image_ops.rs` already
    /// gives every qemu-img failure — duplicating an existence check here
    /// would just be a second place that mapping could drift from the
    /// first.
    pub fn resolve(&self, golden: &GoldenKey) -> Result<PathBuf, LsbxError> {
        // GoldenKey's own regex (`^[a-z][a-z0-9._-]{0,63}$`, enforced by
        // Unit 08's `ImageRegistry::validate_key` before a `GoldenKey` is
        // ever constructed via `new_unchecked`) already rules out path
        // separators and `..` — but this crate doesn't control that
        // validation and shouldn't trust a cross-crate invariant blindly
        // for something as consequential as a filesystem path used
        // directly in a `qemu-img`/domain-XML invocation. Re-checking here
        // costs nothing and turns "a future crate change loosens the
        // regex" into a contained error instead of a silent path-traversal
        // opportunity.
        let key = golden.as_str();
        if key.is_empty() || key.contains('/') || key.contains('\\') || key.contains("..") {
            return Err(LsbxError::ContractViolated(format!(
                "golden key '{key}' is not a safe path component"
            )));
        }

        Ok(self.images_dir.join(format!("{key}.qcow2")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolves_key_to_images_dir_joined_with_qcow2_suffix() {
        let cfg = GoldenDiskConfig::new("/var/lib/lsbx/images");
        let key = GoldenKey::new_unchecked("lsbx-default-v1".to_string());
        let path = cfg.resolve(&key).expect("should resolve");
        assert_eq!(
            path,
            std::path::PathBuf::from("/var/lib/lsbx/images/lsbx-default-v1.qcow2")
        );
    }

    #[test]
    fn rejects_key_with_path_separator() {
        let cfg = GoldenDiskConfig::new("/var/lib/lsbx/images");
        // A regex-valid GoldenKey would never contain '/', but this
        // exercises the defense-in-depth check independent of that
        // upstream invariant holding.
        let key = GoldenKey::new_unchecked("../etc/passwd".to_string());
        let result = cfg.resolve(&key);
        assert!(matches!(result, Err(LsbxError::ContractViolated(_))));
    }

    #[test]
    fn rejects_empty_key() {
        let cfg = GoldenDiskConfig::new("/var/lib/lsbx/images");
        let key = GoldenKey::new_unchecked(String::new());
        assert!(cfg.resolve(&key).is_err());
    }
}
