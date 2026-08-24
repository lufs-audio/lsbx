use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecordEnvelope {
    pub schema_version: u32, // always 1
    pub kind: String,        // always "sandbox"
    pub sandbox: SandboxRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub id: String,
    pub name: String,
    pub host: String,
    pub profile: String,
    pub flavor: String,
    pub streaming: String, // "none" | "novnc"
    pub username: Option<String>,
    pub key_name: Option<String>,
    pub key_path: Option<String>,
    pub key_dir: Option<String>,
    pub pubkey: Option<String>,
    pub task_id: Option<String>,
    pub created_at: Option<String>,       // RFC3339
    pub lease_expires_at: Option<String>, // RFC3339
    pub vm_tag: Option<String>,
    pub https_url: Option<String>,
    pub cleanup_failed: bool,
    pub repository_key: Option<String>,
    pub repository: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl SandboxRecord {
    pub fn from_legacy_flat(value: serde_json::Value) -> Result<Self, crate::error::LsbxError> {
        serde_json::from_value(value)
            .map_err(|e| crate::error::LsbxError::ContractViolated(format!("failed to parse legacy flat record: {}", e)))
    }

    pub fn public(&self) -> PublicSandbox {
        let console_url = match (self.streaming.as_str(), &self.https_url) {
            ("novnc", Some(url)) => Some(format!("{}/vnc.html", url.trim_end_matches('/'))),
            _ => None,
        };

        PublicSandbox {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            profile: self.profile.clone(),
            flavor: self.flavor.clone(),
            streaming: self.streaming.clone(),
            task_id: self.task_id.clone(),
            created_at: self.created_at.clone(),
            lease_expires_at: self.lease_expires_at.clone(),
            console_url,
            cleanup_failed: self.cleanup_failed,
            repository: self.repository.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicSandbox {
    pub id: String,
    pub name: String,
    pub host: String,
    pub profile: String,
    pub flavor: String,
    pub streaming: String,
    pub task_id: Option<String>,
    pub created_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub console_url: Option<String>, // computed, never persisted
    pub cleanup_failed: bool,
    pub repository: Option<String>,
}

/// Validated against `^[a-z][a-z0-9._-]{0,63}$`.
///
/// The inner field is deliberately private: this unit (01) owns the shape
/// only, not the validation regex, which is Unit 08's (`lsbx-golden`,
/// `ImageRegistry::validate_key`). Since Unit 08 lives in a *different*
/// crate than this type, it cannot construct a `GoldenKey` at all without
/// some public entry point here — `new_unchecked` is that entry point.
/// Callers MUST validate against the regex above before calling it; this
/// constructor performs no validation itself, on purpose (that's what
/// "unchecked" signals) so it can't be mistaken for a substitute for
/// `ImageRegistry::validate_key`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoldenKey(String);

impl GoldenKey {
    /// Wraps `s` as a `GoldenKey` without checking it against the key regex.
    /// Validate first (see `ImageRegistry::validate_key` in Unit 08); this
    /// exists so a crate that already validated a key has a way to construct
    /// the type at all, not as a way to skip validation.
    pub fn new_unchecked(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GoldenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated against `^[a-z][a-z0-9-]{0,63}$`; a trailing `.qcow2` is stripped before matching.
///
/// Same rationale as `GoldenKey` above: shape lives here, validation lives in
/// Unit 08, and `new_unchecked` is the only way a different crate can
/// construct one at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseKey(String);

impl BaseKey {
    /// Wraps `s` as a `BaseKey` without checking it against the base regex.
    /// Validate first (see `ImageRegistry::validate_base` in Unit 08).
    pub fn new_unchecked(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BaseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
