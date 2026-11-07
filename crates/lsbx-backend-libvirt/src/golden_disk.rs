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

    /// Lists every golden key present in `images_dir`, by filename stem.
    ///
    /// Two properties matter to the caller (`lsbx-ops::golden_reconcile`):
    ///
    /// - **Symlinks are deduplicated by canonical target.** Hosts carry
    ///   profile-shaped convenience links (`agent-base.qcow2 ->
    ///   lsbx-agent-v1.qcow2`) next to the real image; reporting both would
    ///   claim two artifacts where there is one, and the symlink name is not
    ///   a key any manifest uses as `base`.
    /// - **Only regular `.qcow2` targets count.** A dangling symlink, a
    ///   `-cidata.iso` seed, or a half-written temp file is not a golden.
    ///
    /// A missing `images_dir` yields an empty list rather than an error:
    /// "this host has no goldens yet" is a real, reportable state, and the
    /// caller is the thing that decides whether that is alarming (it is, when
    /// the manifest names goldens).
    pub fn list(&self) -> Result<Vec<String>, LsbxError> {
        let entries = match std::fs::read_dir(&self.images_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(LsbxError::BackendUnavailable(format!(
                    "failed to read golden image directory '{}': {e}",
                    self.images_dir.display()
                )));
            }
        };

        let mut seen: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut keys: Vec<String> = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| {
                LsbxError::BackendUnavailable(format!(
                    "failed to read an entry in '{}': {e}",
                    self.images_dir.display()
                ))
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("qcow2") {
                continue;
            }
            // `canonicalize` both resolves symlinks and proves the target
            // exists, which is exactly the "is this a real golden file"
            // question. A dangling link fails here and is skipped.
            let Ok(canonical) = std::fs::canonicalize(&path) else {
                continue;
            };
            if !canonical.is_file() || !seen.insert(canonical.clone()) {
                continue;
            }
            // Use the canonical target's name, never the entry's: a
            // `agent-base.qcow2 -> lsbx-agent-v1.qcow2` convenience link must
            // report the key the manifest actually uses as `base`.
            if let Some(stem) = canonical.file_stem().and_then(|s| s.to_str()) {
                keys.push(stem.to_string());
            }
        }

        keys.sort();
        Ok(keys)
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

    /// The inventory a `golden reconcile` compares a manifest against: real
    /// qcow2 files only, symlink targets deduplicated to their canonical
    /// name, and nothing else in the directory counted.
    #[test]
    fn list_reports_real_images_deduped_by_canonical_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let images = dir.path().join("goldens");
        std::fs::create_dir(&images).expect("mkdir");

        // Two real images.
        std::fs::write(images.join("lsbx-agent-v1.qcow2"), b"x").expect("write");
        std::fs::write(images.join("lsbx-ci-v1.qcow2"), b"x").expect("write");

        // A profile-shaped convenience link to one of them: same file, so it
        // must not appear as a second artifact (and its name is not a key any
        // manifest uses as `base`).
        std::os::unix::fs::symlink(
            images.join("lsbx-agent-v1.qcow2"),
            images.join("agent-base.qcow2"),
        )
        .expect("symlink");

        // A dangling link: a golden was deleted out from under its alias.
        std::os::unix::fs::symlink(
            images.join("does-not-exist.qcow2"),
            images.join("ghost.qcow2"),
        )
        .expect("symlink");

        // Non-image noise the same directory accumulates.
        std::fs::write(images.join("lsbx-agent-v1-cidata.iso"), b"x").expect("write");
        std::fs::write(images.join("scratch.qcow2.part"), b"x").expect("write");

        let cfg = GoldenDiskConfig::new(&images);
        let keys = cfg.list().expect("list");
        assert_eq!(keys, vec!["lsbx-agent-v1", "lsbx-ci-v1"]);
    }

    /// A host with no golden directory yet is a reportable state, not an
    /// error: the caller decides whether an empty inventory is alarming.
    #[test]
    fn list_of_missing_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = GoldenDiskConfig::new(dir.path().join("absent"));
        assert!(cfg.list().expect("list").is_empty());
    }
}
