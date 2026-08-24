# Unit 08 — Golden Image Registry & Build Lifecycle

## Objective
Parse the existing `images.json`/`images.carnyx.json` manifests with byte-identical schema fidelity, and implement `golden build`/`verify`/`register`/`delete`, including real (not aspirational) content-hash naming.

## Context
Layer 4, parallel with Unit 09. Depends on `lsbx-kernel`'s `Backend` trait generically (exercised in this unit's own tests via Unit 05's demo backend). This is the "Registry" noun from SPEC.md §2 — the actual growing catalog in this system. Deliberately preserves the real `agent-base` base-name mismatch between the two manifest files (SPEC.md Deviation 2) rather than harmonizing it.

## Acceptance criteria
- [ ] `ImageRegistry::load(path)` parses the exact existing schema: `images[]` (`{key, os, arch, iso_path, description}`), `goldens[]` (`{key, flavor: "desktop"|"agent"|"ci-runner", os, base, mode: "copy"|"new", cpu, memory, disk?, streaming: "none"|"novnc", capabilities[], healthcheck[], repo: Option<String>, content_hash: Option<String>, description}`), `profiles{}` (`{golden}` or `{iso, flavor}`).
- [ ] Golden `key` validated against `^[a-z][a-z0-9._-]{0,63}$`; `base` validated against `^[a-z][a-z0-9-]{0,63}$` with a trailing `.qcow2` stripped before matching — the exact existing regexes, not approximations of them.
- [ ] Loading both `images.json` and `images.carnyx.json` as separate `ImageRegistry` instances preserves the real `agent-base.base` mismatch (`lsbx-default-v1` in one, `lsbx-agent-v1` in the other) — a test asserts the mismatch is present, not silently normalized.
- [ ] `golden build <name> --from <base> --script <path> --flavor <f> --cpu <n> --memory <m> [--streaming] [--register] [--no-cleanup] [--interactive|--shell] [--dry-run]` matches the existing CLI flag surface exactly.
- [ ] `golden build` launches a VM via a generic `&dyn Backend`, runs the provisioning script inside it via `Backend::run`, then flattens the resulting disk (delegated to Unit 19's flatten operation through a narrow trait/callback, not reimplemented here) before registering.
- [ ] Content hash is computed as `sha256(golden qcow2 bytes)[:8]`, formatted `lufs-<hash>`, and is **populated** on every golden this build path produces — the first real implementation of a naming scheme that exists today only as a CLI help string (SPEC.md Deviation 3).
- [ ] `golden verify <name>` executes the golden's `healthcheck` command list inside a freshly-created instance and reports pass/fail per check — not just "the VM booted."
- [ ] `golden register`/`golden delete` match the existing flag surfaces exactly (`--profile --base --flavor --streaming --capabilities --healthcheck --content-hash --replace` / `--keep-snapshot`).
- [ ] `allowed_goldens()` returns the set of every distinct `base` value across all loaded goldens, for Unit 09's reaper to consult before ever destroying anything a live sandbox still depends on.

## Interface contract
```rust
// src/registry.rs
use serde::{Deserialize, Serialize};
use lsbx_kernel::types::{GoldenKey, BaseKey};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageConfig { pub key: String, pub os: String, pub arch: String, pub iso_path: String, pub description: String }

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

#[derive(Debug, Clone, Deserialize, Serialize)] #[serde(rename_all = "kebab-case")]
pub enum GoldenFlavor { Desktop, Agent, CiRunner }
#[derive(Debug, Clone, Deserialize, Serialize)] #[serde(rename_all = "lowercase")]
pub enum GoldenMode { Copy, New }
#[derive(Debug, Clone, Deserialize, Serialize)] #[serde(rename_all = "lowercase")]
pub enum StreamingMode { None, Novnc }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ProfileConfig {
    Golden { golden: String },
    Iso { iso: String, flavor: String },
}

pub struct ImageRegistry {
    pub images: Vec<ImageConfig>,
    pub goldens: Vec<GoldenConfig>,
    pub profiles: std::collections::HashMap<String, ProfileConfig>,
}

impl ImageRegistry {
    pub fn load(path: &std::path::Path) -> Result<Self, lsbx_kernel::error::LsbxError>;
    pub fn validate_key(key: &str) -> Result<GoldenKey, lsbx_kernel::error::LsbxError>;
    pub fn validate_base(base: &str) -> Result<BaseKey, lsbx_kernel::error::LsbxError>;
    /// Every distinct `base` value across all goldens — protected from reaping.
    pub fn allowed_goldens(&self) -> std::collections::HashSet<String>;
}

// src/build.rs
pub struct GoldenBuildRequest<'a> {
    pub name: &'a str,
    pub from: &'a str,
    pub script: &'a std::path::Path,
    pub flavor: GoldenFlavor,
    pub cpu: u32,
    pub memory: &'a str,
    pub streaming: StreamingMode,
    pub register: bool,
    pub cleanup: bool,
    pub dry_run: bool,
}

pub async fn golden_build(
    backend: &dyn lsbx_kernel::backend::Backend,
    req: GoldenBuildRequest<'_>,
) -> Result<GoldenConfig, lsbx_kernel::error::LsbxError>;

// src/hash.rs
/// Computes `lufs-<sha256[:8]>` over the given qcow2 file's bytes.
pub fn content_hash(qcow2_path: &std::path::Path) -> Result<String, lsbx_kernel::error::LsbxError>;

// src/verify.rs
pub struct HealthcheckResult { pub command: String, pub passed: bool, pub output: String }

pub async fn golden_verify(
    backend: &dyn lsbx_kernel::backend::Backend,
    golden: &GoldenConfig,
) -> Result<Vec<HealthcheckResult>, lsbx_kernel::error::LsbxError>;
```

## Boundaries — do NOT touch
Does not implement the actual qcow2 flatten operation (Unit 19 owns flatten via `qemu-img`; this unit calls it through a narrow trait/callback rather than duplicating the subprocess logic). Does not decide reap TTL or lease-expiry policy (Unit 09) — only exposes `allowed_goldens()` for the reaper to consult.

## Output
- `crates/lsbx-golden/Cargo.toml`
- `crates/lsbx-golden/src/lib.rs`
- `crates/lsbx-golden/src/registry.rs`
- `crates/lsbx-golden/src/build.rs`
- `crates/lsbx-golden/src/hash.rs`
- `crates/lsbx-golden/src/verify.rs`
- `crates/lsbx-golden/tests/test_registry_schema.rs`
- `crates/lsbx-golden/tests/test_build_via_demo_backend.rs`
- `crates/lsbx-golden/tests/test_content_hash.rs`

## Verification
```bash
cargo check -p lsbx-golden --message-format=json
cargo clippy -p lsbx-golden --all-targets --all-features -- -D warnings
cargo test -p lsbx-golden --test test_registry_schema
cargo test -p lsbx-golden --test test_build_via_demo_backend
cargo test -p lsbx-golden --test test_content_hash
```
Scenario: `test_registry_schema` loads real copies of `images.json` and `images.carnyx.json` (inlined until Unit 20's fixtures land) and asserts `agent-base.base` differs between the two files (`lsbx-default-v1` vs. `lsbx-agent-v1`). This test is written to FAIL if someone "fixes" the mismatch — that's the point.
