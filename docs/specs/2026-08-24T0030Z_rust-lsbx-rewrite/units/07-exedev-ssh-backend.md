# Unit 07 — Exedev SSH Backend

## Objective
Implement `Backend` against exe.dev's real SSH-first control plane, with its HTTPS `/exec` API as a fallback (matching the existing dual-mode), preserving VM-scoped token support and the orphaned-key reconciliation the existing reaper depends on.

## Context
Layer 3. This is the "Molimo" backend in the existing system's internal codenames — its CI label is `lsbx-molimo`; preserve that label exactly. exe.dev is confirmed as a real, public, subscription VM-pool host: SSH-first (`ssh exe.dev new/ls/whoami`), with an HTTPS API exe.dev itself describes as "the SSH API shoved into a POST body" (`POST https://exe.dev/exec`, bearer auth), VMs built from a container image (`exeuntu`) on Cloud Hypervisor with ~2-second creation, and documented VM-scoped tokens (`v0@VMNAME.exe.xyz`). LUFS's own `docs/infra/exe-dev/` already models it as a provisionable pool, not a single fixed box — this backend is the first thing in the ecosystem to actually provision new exe.dev VMs on demand rather than treat one as a fixed always-on anchor (contrast `lufs-audio/lufs-runner`, which does the latter — see SPEC.md §0.9).

## Acceptance criteria
- [ ] `ExedevBackend` supports both control paths — SSH (via `russh`, matching the `ssh exe.dev <verb>` convention) and HTTPS `/exec` (bearer-token auth via `EXE_TOKEN`) — selected by configuration, matching the existing dual-mode exactly.
- [ ] `create_from_golden` provisions a new exe.dev VM and returns a `CreatedVm` whose `https_url` (when the golden's `streaming` is `"novnc"`) follows the existing convention `https://<host>:8000/vnc.html`.
- [ ] VM-scoped tokens (`v0@VMNAME.exe.xyz`) are supported as an alternative to the account-wide `EXE_TOKEN`, per exe.dev's documented token model — this is new capability exe.dev exposes that the existing Python backend doesn't yet use, worth adopting since it narrows credential blast radius per VM.
- [ ] Reconciles orphaned keys via Unit 03's `reconcile_orphaned_keys`, built from exe.dev's own key-listing call, preserving the existing `lsbx:<vm_name>` tag-matching exactly.
- [ ] Passes `lsbx-backend-testkit::run_conformance_suite`, `#[ignore]`d by default (needs a real exe.dev account), runnable with `--ignored`.
- [ ] Documents, in a doc comment, the known raw-VM-shell API limitation: `POST /exec` against a VM directly can return `422` for some shell invocations — only a real SSH session reliably reaches a VM's shell in those cases. `run()` falls back from HTTPS to SSH transparently when this is detected, rather than surfacing a bare `422` to the caller.

## Interface contract
```rust
// src/lib.rs
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;

pub enum ExedevAuth {
    AccountToken(String),   // EXE_TOKEN
    VmScopedToken(String),  // v0@VMNAME.exe.xyz
    Ssh { key_path: std::path::PathBuf },
}

pub struct ExedevBackend {
    auth: ExedevAuth,
    // internal: an HTTP client for the /exec fallback, a russh session for the SSH path
}

impl ExedevBackend {
    pub fn new(auth: ExedevAuth) -> Self;
}

#[async_trait::async_trait]
impl Backend for ExedevBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { console: true, remote_transport: true, snapshot: false }
    }
    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError>;
    async fn run(&self, vm_tag: &str, command: &[String], timeout: std::time::Duration) -> Result<CommandOutput, LsbxError>;
    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str) -> Result<(), LsbxError>;
    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path) -> Result<(), LsbxError>;
    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError>;
    async fn list_vms(&self) -> Result<Vec<String>, LsbxError>;
    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError>;
}

// src/ssh.rs        — russh-based session against `ssh exe.dev <verb>`
// src/http_fallback.rs — `POST https://exe.dev/exec` bearer-token path, with 422-detection fallback to ssh.rs
```

## Boundaries — do NOT touch
Does not touch `lufs-audio/lufs-runner` (the always-on fleet) — this backend is a new, independent consumer of exe.dev, not a modification of that repo's existing single-VM usage (SPEC.md §0.9). Does not build a shared SSH-transport abstraction with the libvirt-remote transport (Unit 06) — each backend owns its own SSH usage independently for now; unifying them into a shared `lsbx-ssh-transport` crate is a reasonable future refactor, not something to force here.

## Output
- `crates/lsbx-backend-exedev/Cargo.toml`
- `crates/lsbx-backend-exedev/src/lib.rs`
- `crates/lsbx-backend-exedev/src/ssh.rs`
- `crates/lsbx-backend-exedev/src/http_fallback.rs`
- `crates/lsbx-backend-exedev/tests/test_conformance.rs` (`#[ignore]` by default)
- `crates/lsbx-backend-exedev/tests/test_auth_modes.rs`

## Verification
```bash
cargo check -p lsbx-backend-exedev --message-format=json
cargo clippy -p lsbx-backend-exedev --all-targets --all-features -- -D warnings
cargo test -p lsbx-backend-exedev --test test_auth_modes
cargo test -p lsbx-backend-exedev --test test_conformance -- --ignored   # requires a real exe.dev account
```
Scenario: `test_auth_modes` asserts all three `ExedevAuth` variants construct a valid outbound request/session against a mock transport with no real network call, proving auth-selection logic is exercised independent of exe.dev's actual availability.
