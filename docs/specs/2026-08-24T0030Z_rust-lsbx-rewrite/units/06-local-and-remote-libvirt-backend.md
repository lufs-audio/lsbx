# Unit 06 — Local + Remote Libvirt Backend

## Objective
Implement `Backend` for local libvirt/KVM and SSH-proxied remote libvirt as one implementation parameterized by transport (SPEC.md Deviation 6), using the `virt` crate for domain lifecycle and subprocess `qemu-img`/`virsh` (explicit argv, never shell interpolation) for image operations.

## Context
Layer 3. This is the "Carnyx" backend in the existing system's internal codenames — its CI label is `lsbx-carnyx`; preserve that label exactly at the CI-contract boundary even though the Rust code itself doesn't need to reference the codename. Needs batch-mode SSH with stdin isolation (`/dev/null`) for non-interactive remote commands — an explicit brief requirement, not optional polish.

## Acceptance criteria
- [ ] `LibvirtBackend::connect(LibvirtTransport::Local { uri })` connects to the local libvirt socket (`qemu:///system` default, overridable) via the `virt` crate.
- [ ] `LibvirtBackend::connect(LibvirtTransport::RemoteSsh { host, ssh_key_path, jump_host, uri })` connects to a remote libvirt host over SSH via `russh` (not shelling to `ssh`), with explicit `ProxyJump`-equivalent support when `jump_host` is set — matching the existing `ssh_target`/ProxyJump design in the current Python `libvirt.py`.
- [ ] Every non-interactive remote command runs in batch mode with stdin redirected from `/dev/null` — never inherits the calling process's stdin, which could otherwise hang a remote command forever waiting on input that will never arrive.
- [ ] `qemu-img`/`virsh` invocations use `tokio::process::Command` with explicit argv (`Command::new("qemu-img").arg("convert").arg(...)`) — never a shell string that a path containing a space or apostrophe could corrupt.
- [ ] Passes `lsbx-backend-testkit::run_conformance_suite`, `#[ignore]`d by default (no real libvirt host in normal CI), runnable with `--ignored` on a host that has one.
- [ ] `capabilities()` reports `console: true, remote_transport: true` regardless of which transport variant is active at runtime — the trait-level capability describes what the backend type supports, not the live instance's current transport.
- [ ] `create_from_golden` boots from the golden's qcow2 via a libvirt domain XML template with a `<backingStore>` for copy-on-write when the golden's `mode` is `"copy"`, or clones a fresh disk when `mode` is `"new"` — matching the existing manifest semantics exactly.

## Interface contract
```rust
// src/transport.rs
pub enum LibvirtTransport {
    Local { uri: Option<String> }, // defaults to "qemu:///system"
    RemoteSsh {
        host: String,
        ssh_key_path: std::path::PathBuf,
        jump_host: Option<String>,
        uri: Option<String>,
    },
}

// src/lib.rs
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;

pub struct LibvirtBackend {
    transport: LibvirtTransport,
    // internal: virt::connect::Connect, or a lazily-established remote session
}

impl LibvirtBackend {
    pub async fn connect(transport: LibvirtTransport) -> Result<Self, LsbxError>;
}

#[async_trait::async_trait]
impl Backend for LibvirtBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { console: true, remote_transport: true, snapshot: true }
    }
    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError>;
    async fn run(&self, vm_tag: &str, command: &[String], timeout: std::time::Duration) -> Result<CommandOutput, LsbxError>;
    async fn put_file(&self, vm_tag: &str, source: &std::path::Path, destination: &str) -> Result<(), LsbxError>;
    async fn get_file(&self, vm_tag: &str, source: &str, destination: &std::path::Path) -> Result<(), LsbxError>;
    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError>;
    async fn list_vms(&self) -> Result<Vec<String>, LsbxError>;
    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError>;
}

// src/image_ops.rs
/// Shells to `qemu-img`, explicit argv only, stdin from /dev/null.
pub async fn qemu_img_convert(source: &std::path::Path, dest: &std::path::Path, format: &str) -> Result<(), LsbxError>;
pub async fn qemu_img_create_cow(backing_file: &std::path::Path, dest: &std::path::Path) -> Result<(), LsbxError>;
```

## Boundaries — do NOT touch
Does not parse `images.json`/`images.carnyx.json` (Unit 08 owns the registry; this unit only receives an already-resolved golden's disk path). Does not implement key generation (Unit 03) or state persistence (Unit 02) — receives a pubkey string to inject and returns a `CreatedVm`, nothing more.

## Output
- `crates/lsbx-backend-libvirt/Cargo.toml`
- `crates/lsbx-backend-libvirt/src/lib.rs`
- `crates/lsbx-backend-libvirt/src/transport.rs`
- `crates/lsbx-backend-libvirt/src/image_ops.rs`
- `crates/lsbx-backend-libvirt/tests/test_conformance.rs` (`#[ignore]` by default)
- `crates/lsbx-backend-libvirt/tests/test_batch_mode_stdin.rs`

## Verification
```bash
cargo check -p lsbx-backend-libvirt --message-format=json
cargo clippy -p lsbx-backend-libvirt --all-targets --all-features -- -D warnings
cargo test -p lsbx-backend-libvirt --test test_batch_mode_stdin
cargo test -p lsbx-backend-libvirt --test test_conformance -- --ignored   # requires a real libvirt host
```
Scenario: `test_batch_mode_stdin` asserts a remote command spawned by this backend has its stdin connected to `/dev/null` (or an equivalently verifiable non-inherited state), never the parent process's stdin.
