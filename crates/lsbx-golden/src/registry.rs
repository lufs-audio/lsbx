//! Parses the existing `images.json` / `images.carnyx.json` golden-image
//! manifest schema with byte-identical fidelity (SPEC.md §4.5, Deviation 1/2).
//!
//! Error-variant convention used throughout this crate (there is no
//! `LsbxError::Io`, `::Json`, `::ValidationError`, or `::BackendError` --
//! those don't exist on the real `LsbxError` in `lsbx-kernel`, which is a
//! closed 7-variant enum with no `#[from]` impls):
//!   - I/O failure reading the manifest file (e.g. not found, permission
//!     denied) -> `LsbxError::NotFound`, since the overwhelmingly common
//!     case is "the path doesn't resolve," and folding every I/O error into
//!     one variant matches this crate's other "doesn't resolve" cases.
//!   - A key/base string that fails its regex -> `LsbxError::Usage`. This is
//!     bad *input* handed to us by a caller (a golden name that doesn't
//!     match `^[a-z][a-z0-9._-]{0,63}$`, a base that doesn't match
//!     `^[a-z][a-z0-9-]{0,63}$`), not an internal fault or a broken
//!     contract with infrastructure -- `Usage` (exit code 2, "bad CLI
//!     arguments or a malformed request" per SPEC.md §6) is the more
//!     precise fit than `ContractViolated` (exit code 5, reserved for
//!     verification failures like a failed healthcheck or readiness
//!     timeout). Picked consistently: every regex-validation failure in
//!     this crate maps to `Usage`, never `ContractViolated`.
//!   - Malformed/unparseable JSON in an otherwise-readable manifest file ->
//!     `LsbxError::ContractViolated`, since "readable, but the actual
//!     content doesn't honor the schema this system depends on" is exactly
//!     what `ContractViolated` names.

use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::{BaseKey, GoldenKey};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageConfig {
    pub key: String,
    pub os: String,
    pub arch: String,
    pub iso_path: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GoldenConfig {
    pub key: String,
    pub flavor: GoldenFlavor,
    pub os: String,
    pub base: String,
    pub mode: GoldenMode,
    pub cpu: u32,
    pub memory: String,
    pub disk: Option<String>,
    pub streaming: StreamingMode,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub healthcheck: Vec<String>,
    pub repo: Option<String>,
    pub content_hash: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoldenFlavor {
    Desktop,
    Agent,
    CiRunner,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GoldenMode {
    Copy,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamingMode {
    None,
    Novnc,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ProfileConfig {
    Golden { golden: String },
    Iso { iso: String, flavor: String },
}

pub struct ImageRegistry {
    pub images: Vec<ImageConfig>,
    pub goldens: Vec<GoldenConfig>,
    pub profiles: HashMap<String, ProfileConfig>,
}

/// Raw on-disk shape of `images.json` / `images.carnyx.json`.
#[derive(Debug, Deserialize, Serialize)]
struct RawImageRegistry {
    #[serde(default)]
    images: Vec<ImageConfig>,
    #[serde(default)]
    goldens: Vec<GoldenConfig>,
    #[serde(default)]
    profiles: HashMap<String, ProfileConfig>,
}

/// The exact existing golden-key regex: `^[a-z][a-z0-9._-]{0,63}$`.
fn golden_key_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"^[a-z][a-z0-9._-]{0,63}$").unwrap()
    })
}

/// The exact existing base-key regex: `^[a-z][a-z0-9-]{0,63}$`
/// (applied after stripping a trailing `.qcow2`).
fn base_key_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"^[a-z][a-z0-9-]{0,63}$").unwrap()
    })
}

impl ImageRegistry {
    /// Loads and parses a golden-image manifest (`images.json` or
    /// `images.carnyx.json`) from `path`.
    ///
    /// I/O errors (file missing, unreadable) map to `LsbxError::NotFound`;
    /// a readable-but-malformed-JSON file maps to
    /// `LsbxError::ContractViolated` -- see the module doc comment for why.
    pub fn load(path: &Path) -> Result<Self, LsbxError> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            LsbxError::NotFound(format!(
                "could not read golden image manifest at {}: {}",
                path.display(),
                e
            ))
        })?;

        let raw: RawImageRegistry = serde_json::from_str(&contents).map_err(|e| {
            LsbxError::ContractViolated(format!(
                "golden image manifest at {} does not match the expected schema: {}",
                path.display(),
                e
            ))
        })?;

        Ok(Self {
            images: raw.images,
            goldens: raw.goldens,
            profiles: raw.profiles,
        })
    }

    /// Serializes this registry back to the manifest schema and writes it
    /// to `path` (creating/overwriting the file). The in-memory
    /// `images`/`goldens`/`profiles` sections are round-tripped — a
    /// registry loaded via [`load`](Self::load) writes back with the same
    /// shape it was read with, plus any `golden register`/`golden delete`
    /// mutations made in between.
    ///
    /// I/O failures (unwritable path, disk error) map to
    /// `LsbxError::BackendUnavailable`, treating "could not persist the
    /// registry this process mutated" as an infrastructure fault rather
    /// than an input problem.
    pub fn save(&self, path: &Path) -> Result<(), LsbxError> {
        let raw = RawImageRegistry {
            images: self.images.clone(),
            goldens: self.goldens.clone(),
            profiles: self.profiles.clone(),
        };
        let contents = serde_json::to_string_pretty(&raw).map_err(|e| {
            LsbxError::ContractViolated(format!(
                "failed to serialize golden image manifest for {}: {}",
                path.display(),
                e
            ))
        })?;
        std::fs::write(path, format!("{contents}\n")).map_err(|e| {
            LsbxError::BackendUnavailable(format!(
                "failed to write golden image manifest at {}: {}",
                path.display(),
                e
            ))
        })
    }

    /// Validates `key` against the golden-key regex
    /// (`^[a-z][a-z0-9._-]{0,63}$`) and, if it matches, wraps it as a
    /// `GoldenKey` via `GoldenKey::new_unchecked` -- the only public
    /// cross-crate constructor `lsbx-kernel` exposes, precisely because the
    /// regex check just above is the validation that makes `new_unchecked`
    /// safe to call here.
    pub fn validate_key(key: &str) -> Result<GoldenKey, LsbxError> {
        if golden_key_regex().is_match(key) {
            Ok(GoldenKey::new_unchecked(key.to_string()))
        } else {
            Err(LsbxError::Usage(format!(
                "golden key '{}' does not match the required pattern ^[a-z][a-z0-9._-]{{0,63}}$",
                key
            )))
        }
    }

    /// Validates `base` against the base-key regex
    /// (`^[a-z][a-z0-9-]{0,63}$`), stripping a trailing `.qcow2` suffix
    /// first, and wraps the result as a `BaseKey` via `BaseKey::new_unchecked`
    /// once validation has passed.
    pub fn validate_base(base: &str) -> Result<BaseKey, LsbxError> {
        let stripped = base.strip_suffix(".qcow2").unwrap_or(base);
        if base_key_regex().is_match(stripped) {
            Ok(BaseKey::new_unchecked(stripped.to_string()))
        } else {
            Err(LsbxError::Usage(format!(
                "base '{}' does not match the required pattern ^[a-z][a-z0-9-]{{0,63}}$ (after stripping a trailing .qcow2)",
                base
            )))
        }
    }

    /// Every distinct `base` value across all loaded goldens -- the set
    /// Unit 09's reaper must protect from ever being destroyed while a live
    /// sandbox still depends on it.
    pub fn allowed_goldens(&self) -> HashSet<String> {
        self.goldens.iter().map(|g| g.base.clone()).collect()
    }

    /// Looks up a golden by its `key` field.
    pub fn find_golden(&self, key: &str) -> Option<&GoldenConfig> {
        self.goldens.iter().find(|g| g.key == key)
    }

    /// Resolves a public profile name (or direct golden key) to its full
    /// golden configuration. ISO-backed profiles are not provisionable via
    /// the golden lifecycle.
    pub fn resolve_golden(&self, name: &str) -> Result<&GoldenConfig, LsbxError> {
        if let Some(profile) = self.profiles.get(name) {
            return match profile {
                ProfileConfig::Golden { golden } => self.find_golden(golden).ok_or_else(|| {
                    LsbxError::ContractViolated(format!(
                        "profile '{name}' references unknown golden '{golden}'"
                    ))
                }),
                ProfileConfig::Iso { iso, .. } => Err(LsbxError::Usage(format!(
                    "profile '{name}' installs ISO '{iso}' and is not provisionable from a golden"
                ))),
            };
        }

        self.find_golden(name).ok_or_else(|| {
            LsbxError::NotFound(format!(
                "no profile or golden registered under key '{name}'"
            ))
        })
    }

    /// Resolves a profile name to the golden key it maps to. Returns `None`
    /// if the profile doesn't exist or is an ISO-type profile (no golden
    /// mapping).
    pub fn resolve_profile_golden_key(&self, profile: &str) -> Option<&str> {
        match self.profiles.get(profile)? {
            ProfileConfig::Golden { golden } => Some(golden.as_str()),
            ProfileConfig::Iso { .. } => None,
        }
    }
}

// This module is #[cfg(test)]-gated: every fn in it is a #[test], so a
// failed unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. clippy::unwrap_used / expect_used are restriction-group
// lints that don't understand "this whole module is test code" the way
// #[cfg(test)] gating alone does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this test
// module only — the rest of this file (the real production code path above)
// is unwrap/expect/panic-free under the same workspace lints with no allow
// needed. Pattern established in Unit 01's crates/lsbx-kernel/tests/test_kernel.rs
// (restated for a #[cfg(test)] mod rather than a separate tests/*.rs file,
// since the rationale is identical either way).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_accepts_conforming_keys() {
        assert!(ImageRegistry::validate_key("agent-base").is_ok());
        assert!(ImageRegistry::validate_key("a").is_ok());
        assert!(ImageRegistry::validate_key("lufs-abc123.def_ghi-1").is_ok());
    }

    #[test]
    fn validate_key_rejects_non_conforming_keys() {
        // Uppercase, leading digit, empty, and too-long are all rejected.
        assert!(matches!(
            ImageRegistry::validate_key("Agent-Base"),
            Err(LsbxError::Usage(_))
        ));
        assert!(matches!(
            ImageRegistry::validate_key("1agent"),
            Err(LsbxError::Usage(_))
        ));
        assert!(matches!(
            ImageRegistry::validate_key(""),
            Err(LsbxError::Usage(_))
        ));
        let too_long = "a".repeat(65);
        assert!(matches!(
            ImageRegistry::validate_key(&too_long),
            Err(LsbxError::Usage(_))
        ));
    }

    #[test]
    fn validate_base_strips_qcow2_suffix_before_matching() {
        let base = ImageRegistry::validate_base("lsbx-default-v1.qcow2").expect("should validate");
        assert_eq!(base.as_str(), "lsbx-default-v1");
    }

    #[test]
    fn validate_base_rejects_non_conforming_base() {
        assert!(matches!(
            ImageRegistry::validate_base("Lsbx-Default"),
            Err(LsbxError::Usage(_))
        ));
        assert!(matches!(
            ImageRegistry::validate_base("lsbx_default"),
            Err(LsbxError::Usage(_))
        ));
    }

    #[test]
    fn load_missing_file_maps_to_not_found() {
        let result = ImageRegistry::load(Path::new("/nonexistent/path/images.json"));
        assert!(matches!(result, Err(LsbxError::NotFound(_))));
    }

    #[test]
    fn load_malformed_json_maps_to_contract_violated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("images.json");
        std::fs::write(&path, "{ not valid json").expect("write");
        let result = ImageRegistry::load(&path);
        assert!(matches!(result, Err(LsbxError::ContractViolated(_))));
    }

    #[test]
    fn allowed_goldens_collects_distinct_base_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("images.json");
        std::fs::write(
            &path,
            r#"{
                "images": [],
                "goldens": [
                    {"key": "a", "flavor": "agent", "os": "linux", "base": "base-one", "mode": "copy", "cpu": 2, "memory": "2G", "disk": null, "streaming": "none", "capabilities": [], "healthcheck": [], "repo": null, "content_hash": null, "description": "a"},
                    {"key": "b", "flavor": "agent", "os": "linux", "base": "base-one", "mode": "copy", "cpu": 2, "memory": "2G", "disk": null, "streaming": "none", "capabilities": [], "healthcheck": [], "repo": null, "content_hash": null, "description": "b"},
                    {"key": "c", "flavor": "desktop", "os": "linux", "base": "base-two", "mode": "new", "cpu": 4, "memory": "4G", "disk": null, "streaming": "novnc", "capabilities": [], "healthcheck": [], "repo": null, "content_hash": null, "description": "c"}
                ],
                "profiles": {}
            }"#,
        )
        .expect("write");
        let registry = ImageRegistry::load(&path).expect("load");
        let allowed = registry.allowed_goldens();
        assert_eq!(allowed.len(), 2);
        assert!(allowed.contains("base-one"));
        assert!(allowed.contains("base-two"));
    }

    #[test]
    fn save_round_trips_goldens_images_and_profiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("images.json");
        std::fs::write(
            &path,
            r#"{
                "images": [
                    {"key": "win11-iso", "os": "windows", "arch": "x86_64", "iso_path": "isos/win11.iso", "description": "install iso"}
                ],
                "goldens": [
                    {"key": "win11-desktop", "flavor": "desktop", "os": "windows", "base": "win11-desktop", "mode": "copy", "cpu": 4, "memory": "8GB", "disk": "goldens/win11-desktop.qcow2", "streaming": "novnc", "capabilities": ["desktop"], "healthcheck": ["echo lsbx-windows-ok"], "repo": null, "content_hash": "sha256:abcd", "description": "windows golden"}
                ],
                "profiles": {}
            }"#,
        )
        .expect("write");
        let registry = ImageRegistry::load(&path).expect("load");

        let out_path = dir.path().join("images.out.json");
        registry.save(&out_path).expect("save");

        let reloaded = ImageRegistry::load(&out_path).expect("reload");
        assert_eq!(reloaded.images.len(), 1);
        assert_eq!(reloaded.goldens.len(), 1);
        let golden = &reloaded.goldens[0];
        assert_eq!(golden.key, "win11-desktop");
        assert_eq!(golden.os, "windows");
        assert_eq!(golden.cpu, 4);
        assert_eq!(golden.memory, "8GB");
        assert_eq!(golden.disk.as_deref(), Some("goldens/win11-desktop.qcow2"));
        assert_eq!(golden.healthcheck, vec!["echo lsbx-windows-ok"]);
        assert_eq!(golden.content_hash.as_deref(), Some("sha256:abcd"));
    }

    #[test]
    fn save_failure_maps_to_backend_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ImageRegistry {
            images: vec![],
            goldens: vec![],
            profiles: HashMap::new(),
        };
        let result = registry.save(&dir.path().join("no-such-dir").join("images.json"));
        assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));
    }
}
