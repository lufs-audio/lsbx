# Unit 19 — Golden Flattening & Host Bootstrap

## Objective
Verify host capability, generate and install systemd units for the broker services, and implement golden flattening (qcow2 backing-file collapse) ahead of content-hash computation.

## Context
Layer 8, depends on Unit 06 (libvirt backend, for local capability checks) and Unit 08 (golden, which calls into this unit's flatten operation through the narrow trait/callback named in Unit 08's Boundaries).

## Acceptance criteria
- [ ] `verify_host()` checks: libvirt socket reachable (for the `Local` transport), `qemu-img` present on `PATH`, and state directories exist with correct permissions (0700) — reporting each check individually, matching the "proven, not exited 0" ethos rather than a single pass/fail boolean.
- [ ] `lsbx bootstrap [--target --no-services --no-verify --force --dry-run]` matches the existing flag surface exactly.
- [ ] Generates systemd unit files for `lsbx-ci-broker` (Carnyx/libvirt) and `lsbx-ci-broker-exe` (Molimo/exedev) — names preserved exactly from the existing `AGENTS.md` — installed only when `--no-services` is absent.
- [ ] `flatten(qcow2_with_backing_file) -> qcow2_standalone` collapses a backing-file chain into a single self-contained image via `qemu-img convert` (explicit argv, matching Unit 06's subprocess discipline). A golden's content hash (Unit 08) is computed only on this function's output — never on an image that still depends on an external backing file that could change underneath it.
- [ ] `--dry-run` on `bootstrap` reports every action that would be taken (service files that would be written, directories that would be created) without writing anything.
- [ ] `--force` re-runs bootstrap idempotently on an already-bootstrapped host without erroring on "already exists" conditions.

## Interface contract
```rust
// src/verify_host.rs
use lsbx_kernel::error::LsbxError;

pub struct HostCheck { pub name: &'static str, pub passed: bool, pub detail: Option<String> }
pub struct HostVerification { pub checks: Vec<HostCheck> }

pub async fn verify_host(target: Option<&str>) -> Result<HostVerification, LsbxError>;

// src/systemd.rs
pub struct SystemdUnitSpec { pub name: &'static str, pub content: String }

/// Names preserved exactly: "lsbx-ci-broker", "lsbx-ci-broker-exe".
pub fn generate_broker_units(config: &BootstrapConfig) -> Vec<SystemdUnitSpec>;

pub struct BootstrapConfig {
    pub target: Option<String>,
    pub install_services: bool, // false when --no-services
    pub verify: bool,           // false when --no-verify
    pub force: bool,
    pub dry_run: bool,
}

pub struct BootstrapReport { pub actions_taken: Vec<String>, pub actions_would_take: Vec<String> }

pub async fn bootstrap(config: BootstrapConfig) -> Result<BootstrapReport, LsbxError>;

// src/flatten.rs
/// Collapses a qcow2 backing-file chain into one standalone image.
/// The caller (Unit 08's golden_build) computes content_hash only on this function's output.
pub async fn flatten(source_with_backing: &std::path::Path, dest_standalone: &std::path::Path) -> Result<(), LsbxError>;
```

## Boundaries — do NOT touch
Does not implement `create_from_golden` or domain lifecycle (Unit 06) — only verifies host capability and performs the flatten step as a standalone operation. Does not compute content hashes itself (Unit 08 owns `content_hash`) — this unit only guarantees flattening happens before that hash is computed, via ordering in `golden_build`'s call sequence.

## Output
- `crates/lsbx-bootstrap/Cargo.toml`
- `crates/lsbx-bootstrap/src/lib.rs`
- `crates/lsbx-bootstrap/src/verify_host.rs`
- `crates/lsbx-bootstrap/src/systemd.rs`
- `crates/lsbx-bootstrap/src/flatten.rs`
- `crates/lsbx-bootstrap/tests/test_verify_host.rs`
- `crates/lsbx-bootstrap/tests/test_flatten_before_hash_ordering.rs`
- `crates/lsbx-bootstrap/tests/test_bootstrap_idempotent.rs`

## Verification
```bash
cargo check -p lsbx-bootstrap --message-format=json
cargo clippy -p lsbx-bootstrap --all-targets --all-features -- -D warnings
cargo test -p lsbx-bootstrap --test test_verify_host
cargo test -p lsbx-bootstrap --test test_flatten_before_hash_ordering
cargo test -p lsbx-bootstrap --test test_bootstrap_idempotent
```
Scenario: `test_bootstrap_idempotent` runs `bootstrap(config)` twice in a row with `force: true` against the same temp target directory and asserts the second run succeeds without an "already exists" error and produces the same `BootstrapReport` shape.
