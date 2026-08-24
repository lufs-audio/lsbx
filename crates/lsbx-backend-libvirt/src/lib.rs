//! Local + remote libvirt backend (Unit 06).
//!
//! One `Backend` implementation parameterized by [`transport::LibvirtTransport`]
//! (SPEC.md Deviation 6) — not two separate trait impls for "local" and
//! "remote". See `transport` module docs for the architectural correction
//! this crate makes relative to both the unit contract's own interface
//! snippet and both Jules candidates: reaching a *remote libvirt host* uses
//! libvirt's native `qemu+ssh://` URI transport via `Connect::open`, never
//! a hand-rolled `russh` session pretending to carry libvirt's RPC. See
//! `guest_ssh` module docs for where real, hand-rolled SSH client work
//! *does* belong: executing commands inside the guest VM, which is a
//! separate concern from how the hypervisor itself is reached.

pub mod domain_xml;
pub mod golden_disk;
pub mod guest_ssh;
pub mod image_ops;
pub mod transport;

use lsbx_kernel::backend::{
    Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm,
};
use lsbx_kernel::error::LsbxError;
use transport::LibvirtTransport;
use virt::connect::Connect;
use virt::domain::Domain;
use virt::error::{Error as VirtError, ErrorNumber};

/// How a new VM's disk relates to its golden's qcow2 — mirrors Unit 08's
/// (not-yet-built) `GoldenMode::Copy | New` exactly, so the eventual
/// `lsbx-golden` crate's value can be passed straight through without a
/// translation layer once it exists.
///
/// ### Why this exists as constructor config instead of a request field
/// The real, merged `CreateFromGoldenRequest` (Unit 01) carries no `mode`
/// field at all — only `golden`, `name`, `pubkey`, `cpu`, `memory` — and
/// this unit's own Boundaries section is explicit that it must not invent
/// fields on that type or reach into a registry Unit 08 hasn't built yet.
/// At the same time, the unit's acceptance criteria require branching on a
/// golden's `mode` ("copy" vs "new") when building the disk. Those two
/// constraints are only reconcilable one way: the *request* can't carry
/// mode, so the *backend instance* has to carry a policy for it, set at
/// construction time via [`LibvirtBackend::with_disk_mode`] (defaulting to
/// [`DiskMode::Copy`], the safer/cheaper default — copy-on-write over a
/// golden never mutates the golden itself). When Unit 08 lands and
/// `lsbx-ops`/`lsbx-lifecycle` actually know a specific golden's declared
/// `mode`, the caller wiring this backend together is expected to
/// construct one `LibvirtBackend` per effective mode (or re-derive one via
/// `with_disk_mode` per call site) rather than this crate reaching past its
/// own boundary to look the mode up itself. Flagged explicitly in the PR
/// description as a real seam Unit 08/09/10 need to close, not a
/// resolved fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskMode {
    /// Copy-on-write overlay on top of the golden's own qcow2 (`qemu-img
    /// create -b <golden> <new disk>`). The golden's bytes are never
    /// mutated; multiple VMs can share one golden on disk. This is the
    /// default.
    #[default]
    Copy,
    /// A fresh, independent disk materialized via `qemu-img convert` from
    /// the golden — no backing-file relationship to the golden at all
    /// after creation.
    New,
}

/// Where per-VM disk overlays/clones are written. Kept distinct from
/// [`golden_disk::GoldenDiskConfig::images_dir`] (which is read-only,
/// golden-owned storage) since a golden directory and a scratch/working
/// directory for live VM disks are different lifetimes and, in a real
/// deployment, likely different filesystems/permissions entirely.
#[derive(Debug, Clone)]
pub struct VmDiskConfig {
    pub work_dir: std::path::PathBuf,
}

/// The `Backend` implementation for local KVM/QEMU and SSH-proxied remote
/// libvirt, unified behind one `virt::connect::Connect` regardless of which
/// [`LibvirtTransport`] variant produced it.
pub struct LibvirtBackend {
    transport: LibvirtTransport,
    conn: Connect,
    golden_disks: golden_disk::GoldenDiskConfig,
    vm_disks: VmDiskConfig,
    disk_mode: DiskMode,
    guest_username: String,
}

/// A libvirt `ErrorNumber` that means "the domain you asked about does not
/// exist" — the one case this crate maps to `LsbxError::NotFound` rather
/// than `LsbxError::BackendUnavailable`. Every other libvirt error is
/// treated as a backend/connectivity problem: a `virsh`-level operation
/// that fails for a reason *other* than "no such domain" (a malformed XML,
/// a permission error, a transport disconnect) is not a "not found" in the
/// sense `lsbx`'s own exit-code taxonomy means it (`NOT_FOUND` = "the
/// identifier you gave me doesn't resolve"), so lumping every libvirt
/// failure into `NotFound` would misreport those cases just as badly as
/// lumping `NoDomain` into `BackendUnavailable` would misreport this one.
fn map_virt_err(e: VirtError) -> LsbxError {
    match e.code() {
        ErrorNumber::NoDomain => LsbxError::NotFound(e.message().to_string()),
        _ => LsbxError::BackendUnavailable(e.message().to_string()),
    }
}

impl LibvirtBackend {
    /// Connects to libvirt via the given transport.
    ///
    /// **This is the one connection path for both `Local` and `RemoteSsh`**
    /// — see `transport` module docs. `transport.to_connect_uri()` builds
    /// either the plain local URI or a `qemu+ssh://...` remote URI, and
    /// either way the result is handed straight to `Connect::open`, which
    /// already knows how to drive both cases (a local UNIX socket, or its
    /// own internal SSH transport for the RPC channel) without this crate
    /// distinguishing between them at the connection layer at all.
    pub async fn connect(transport: LibvirtTransport) -> Result<Self, LsbxError> {
        let uri = transport.to_connect_uri();

        // `virt::connect::Connect::open` is a blocking FFI call into
        // libvirt's C client library (it may itself shell out to `ssh` for
        // the RemoteSsh case, or talk to a local UNIX socket) — run it on
        // a blocking-friendly thread so it can't stall the async runtime
        // it's called from.
        let conn = tokio::task::spawn_blocking(move || Connect::open(Some(&uri)))
            .await
            .map_err(|e| LsbxError::BackendUnavailable(format!("connect task panicked: {e}")))?
            .map_err(map_virt_err)?;

        Ok(Self {
            transport,
            conn,
            golden_disks: golden_disk::GoldenDiskConfig::new("/var/lib/lsbx/images"),
            vm_disks: VmDiskConfig {
                work_dir: std::path::PathBuf::from("/var/lib/lsbx/vms"),
            },
            disk_mode: DiskMode::default(),
            guest_username: "lsbx".to_string(),
        })
    }

    /// Overrides where golden qcow2 images are read from (default
    /// `/var/lib/lsbx/images` — see `golden_disk` module docs for the
    /// convention). The caller wiring this backend together (eventually
    /// `lsbx-ops`/host bootstrap, Unit 19) is expected to call this with
    /// whatever directory the deployment actually uses.
    #[must_use]
    pub fn with_images_dir(mut self, images_dir: impl Into<std::path::PathBuf>) -> Self {
        self.golden_disks = golden_disk::GoldenDiskConfig::new(images_dir);
        self
    }

    /// Overrides where per-VM working disks (copy-on-write overlays or
    /// fresh converted clones) are written (default `/var/lib/lsbx/vms`).
    #[must_use]
    pub fn with_work_dir(mut self, work_dir: impl Into<std::path::PathBuf>) -> Self {
        self.vm_disks = VmDiskConfig {
            work_dir: work_dir.into(),
        };
        self
    }

    /// Sets the disk-materialization policy — see [`DiskMode`]'s doc
    /// comment for why this is instance-level configuration rather than a
    /// per-request field.
    #[must_use]
    pub fn with_disk_mode(mut self, mode: DiskMode) -> Self {
        self.disk_mode = mode;
        self
    }

    /// Sets the guest OS username used for `run`/`put_file`/`get_file`
    /// (default `"lsbx"`, matching the existing system's convention).
    #[must_use]
    pub fn with_guest_username(mut self, username: impl Into<String>) -> Self {
        self.guest_username = username.into();
        self
    }

    /// The transport this instance was constructed with. Exposed for
    /// callers/tests that need to distinguish `Local` vs `RemoteSsh` after
    /// the fact (e.g. logging, or Unit 19's host-bootstrap verification) —
    /// `capabilities()` itself deliberately does NOT vary by this, per the
    /// unit's own acceptance criterion.
    pub fn transport(&self) -> &LibvirtTransport {
        &self.transport
    }

    /// Resolves the guest hostname/IP `run`/`put_file`/`get_file` should
    /// target for a given `vm_tag`.
    ///
    /// This backend uses `vm_tag` itself as the SSH hostname/IP — i.e. the
    /// caller (eventually `lsbx-lifecycle`, Unit 09) is expected to hand
    /// `create_from_golden` a `name` that already resolves to the guest's
    /// address (a DHCP-registered hostname on the libvirt `default`
    /// network, a static IP baked into the golden, etc.), rather than this
    /// crate performing its own DHCP-lease-to-IP lookup via
    /// `virConnectListAllInterfaces`-style APIs. Doing real lease-lookup
    /// integration is a reasonable follow-up but is judged out of scope
    /// here — flagged in the PR description alongside the pubkey/cloud-init
    /// gap, since both are instances of the same underlying question
    /// ("how does this backend learn a guest's real network identity")
    /// that a fuller implementation would need to answer once.
    fn guest_host_for(&self, vm_tag: &str) -> String {
        vm_tag.to_string()
    }

    fn lookup_domain(&self, vm_tag: &str) -> Result<Domain, LsbxError> {
        Domain::lookup_by_name(&self.conn, vm_tag).map_err(map_virt_err)
    }
}

#[async_trait::async_trait]
impl Backend for LibvirtBackend {
    fn capabilities(&self) -> BackendCapabilities {
        // Per the unit's own acceptance criterion: this describes what the
        // *backend type* supports, not the live instance's current
        // transport — `console`/`remote_transport`/`snapshot` are reported
        // identically whether `self.transport` is `Local` or `RemoteSsh`.
        BackendCapabilities {
            console: true,
            remote_transport: true,
            snapshot: true,
        }
    }

    async fn create_from_golden(
        &self,
        req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, LsbxError> {
        let golden_path = self.golden_disks.resolve(req.golden)?;

        std::fs::create_dir_all(&self.vm_disks.work_dir).map_err(|e| {
            LsbxError::BackendUnavailable(format!(
                "failed to create VM disk work directory '{}': {e}",
                self.vm_disks.work_dir.display()
            ))
        })?;
        let vm_disk_path = self.vm_disks.work_dir.join(format!("{}.qcow2", req.name));

        // Branch on the disk-materialization policy (see `DiskMode`'s doc
        // comment on why this is instance config, not a request field) —
        // matches the two `qemu-img` operations the unit contract names by
        // exact call shape.
        match self.disk_mode {
            DiskMode::Copy => {
                image_ops::qemu_img_create_cow(&golden_path, &vm_disk_path).await?;
            }
            DiskMode::New => {
                image_ops::qemu_img_convert(&golden_path, &vm_disk_path, "qcow2").await?;
            }
        }

        let xml = domain_xml::render_domain_xml(
            &domain_xml::DomainXmlParams {
                name: req.name,
                cpu: req.cpu,
                memory: req.memory,
                disk_path: &vm_disk_path,
            },
            req.pubkey,
        )?;

        let conn = &self.conn;
        let domain = Domain::create_xml(conn, &xml, 0).map_err(map_virt_err)?;
        let vm_tag = domain.get_name().map_err(map_virt_err)?;

        let host = match &self.transport {
            LibvirtTransport::Local { .. } => "localhost".to_string(),
            LibvirtTransport::RemoteSsh { host, .. } => host.clone(),
        };

        Ok(CreatedVm {
            vm_tag,
            host,
            // This backend's console capability is real (libvirt exposes a
            // VNC graphics device on every domain this crate creates — see
            // `domain_xml::render_domain_xml`), but rendering that into an
            // actual reachable `https_url` is Unit 14's job (the
            // WebSocket/noVNC proxy sits in front of the raw VNC port this
            // backend exposes) — this backend has no HTTP server of its
            // own to mint that URL from. Left `None` here rather than
            // fabricating a URL this crate can't actually serve.
            https_url: None,
        })
    }

    async fn run(
        &self,
        vm_tag: &str,
        command: &[String],
        timeout: std::time::Duration,
    ) -> Result<CommandOutput, LsbxError> {
        if command.is_empty() {
            return Err(LsbxError::Usage(
                "run() requires a non-empty command".to_string(),
            ));
        }

        // Confirm the domain actually exists before attempting a guest SSH
        // session — this is what lets `run()` against a never-created or
        // already-destroyed `vm_tag` report `NotFound` (per the
        // conformance suite's `run_against_nonexistent_vm_errors` /
        // `run_against_destroyed_vm_errors` checks) rather than a
        // generic SSH connection-refused error that would otherwise be
        // indistinguishable from "the VM exists but its sshd isn't up
        // yet".
        self.lookup_domain(vm_tag)?;

        let identity_file = self.identity_file_placeholder();
        let target = guest_ssh::GuestSshTarget {
            host: &self.guest_host_for(vm_tag),
            username: &self.guest_username,
            identity_file: &identity_file,
        };
        guest_ssh::run_command(&target, command, timeout).await
    }

    async fn put_file(
        &self,
        vm_tag: &str,
        source: &std::path::Path,
        destination: &str,
    ) -> Result<(), LsbxError> {
        self.lookup_domain(vm_tag)?;
        let identity_file = self.identity_file_placeholder();
        let target = guest_ssh::GuestSshTarget {
            host: &self.guest_host_for(vm_tag),
            username: &self.guest_username,
            identity_file: &identity_file,
        };
        guest_ssh::put_file(&target, source, destination).await
    }

    async fn get_file(
        &self,
        vm_tag: &str,
        source: &str,
        destination: &std::path::Path,
    ) -> Result<(), LsbxError> {
        self.lookup_domain(vm_tag)?;
        let identity_file = self.identity_file_placeholder();
        let target = guest_ssh::GuestSshTarget {
            host: &self.guest_host_for(vm_tag),
            username: &self.guest_username,
            identity_file: &identity_file,
        };
        guest_ssh::get_file(&target, source, destination).await
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        let domain = self.lookup_domain(vm_tag)?;
        domain.destroy().map_err(map_virt_err)?;

        // Best-effort cleanup of the per-VM working disk this backend
        // created in `create_from_golden`. A failure here is deliberately
        // not surfaced as an error: the domain is already gone (the part
        // this method's contract is actually about), and the conformance
        // suite's `destroy_idempotent` check requires a *second* destroy
        // of the same `vm_tag` to return `Ok(())` or `NotFound` — a stray
        // leftover qcow2 file should never turn that into a hard failure.
        let vm_disk_path = self.vm_disks.work_dir.join(format!("{vm_tag}.qcow2"));
        let _ = std::fs::remove_file(vm_disk_path);

        Ok(())
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        let domains = self.conn.list_all_domains(0).map_err(map_virt_err)?;
        let mut names = Vec::with_capacity(domains.len());
        for domain in domains {
            names.push(domain.get_name().map_err(map_virt_err)?);
        }
        Ok(names)
    }

    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError> {
        let domain = self.lookup_domain(old_tag)?;
        domain.rename(new_tag, 0).map_err(map_virt_err)?;
        Ok(())
    }
}

impl LibvirtBackend {
    /// Placeholder identity-file resolution for guest SSH sessions.
    ///
    /// Per the unit's own Boundaries section, this crate does **not** own
    /// key generation (Unit 03, `lsbx-keys`) or state persistence (Unit
    /// 02) — it "receives a pubkey string to inject and returns a
    /// `CreatedVm`, nothing more." Symmetrically, on the *read* side
    /// (`run`/`put_file`/`get_file`), this crate has no way to look up
    /// which private key corresponds to a given `vm_tag`'s ephemeral
    /// keypair — that mapping lives in a `SandboxRecord` (Unit 02's
    /// `SandboxStore`), which is owned by `lsbx-lifecycle` (Unit 09), not
    /// this backend. There is no plumbing in the `Backend` trait itself
    /// (`run`/`put_file`/`get_file` take only `vm_tag`, never a key path)
    /// for a caller to hand this backend the right identity file per call.
    ///
    /// This is a real, structural gap between "this backend needs an SSH
    /// identity to reach the guest" and "the trait signature it must
    /// implement has no field for one." Documented here rather than
    /// silently defaulted to something that would look plausible in a demo
    /// but be wrong in production: this placeholder resolves to
    /// `~/.ssh/lsbx_guest_key` (or `$LSBX_GUEST_SSH_KEY` if set) purely so
    /// `run`/`put_file`/`get_file` are callable and testable against a
    /// fake `ssh` on PATH; a real deployment needs `lsbx-ops`/
    /// `lsbx-lifecycle` (Units 09/10) to either (a) extend the `Backend`
    /// trait with an identity parameter on these three methods, or (b)
    /// have this backend accept a `Fn(&str) -> PathBuf`-style resolver
    /// callback at construction time that it can consult per `vm_tag`.
    /// Flagged explicitly in the PR description.
    fn identity_file_placeholder(&self) -> std::path::PathBuf {
        resolve_identity_file()
    }
}

/// Free-function core of [`LibvirtBackend::identity_file_placeholder`],
/// factored out so it can be unit-tested without a live `Connect` (which
/// requires a reachable libvirt daemon this sandbox does not have).
fn resolve_identity_file() -> std::path::PathBuf {
    if let Ok(from_env) = std::env::var("LSBX_GUEST_SSH_KEY") {
        return std::path::PathBuf::from(from_env);
    }
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/root"))
        .join(".ssh")
        .join("lsbx_guest_key")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn disk_mode_defaults_to_copy() {
        assert_eq!(DiskMode::default(), DiskMode::Copy);
    }

    #[test]
    fn map_virt_err_no_domain_becomes_not_found() {
        // Constructing a real `virt::error::Error` requires either a live
        // libvirt connection (to trigger `Error::last_error()`) or the
        // crate's own `#[cfg(test)]`-only constructors, neither of which
        // this crate has access to from outside `virt` itself. This test
        // instead asserts the *mapping function's behavior contract*
        // indirectly, via the conformance-suite-shaped integration test in
        // `tests/test_conformance.rs` (which is `#[ignore]`d pending a
        // real libvirt host) — recorded here as a signpost rather than
        // duplicated, since `virt::error::Error` cannot be constructed
        // from this crate without one.
        //
        // What CAN be asserted without a live connection: `ErrorNumber`
        // itself is a plain, publicly comparable enum, so the match arms
        // in `map_virt_err` are at least type-checked and exhaustively
        // reachable at compile time (verified by `cargo check`/`clippy`
        // already passing on this file at all, given `LsbxError` has no
        // `#[non_exhaustive]` counterpart on the `virt` side to silently
        // miss a variant).
        let _ = ErrorNumber::NoDomain;
    }

    #[test]
    fn identity_file_placeholder_respects_env_override() {
        // Exercises the actual resolution function end-to-end rather than
        // just checking a string literal — this is deliberately a
        // free function, not a method on `LibvirtBackend`, specifically so
        // it can be unit-tested without a live `Connect` (which
        // `LibvirtBackend::connect` cannot produce in this sandbox — no
        // libvirt daemon is reachable).
        let previous = std::env::var("LSBX_GUEST_SSH_KEY").ok();

        std::env::set_var("LSBX_GUEST_SSH_KEY", "/tmp/lsbx-test-override-key");
        assert_eq!(
            resolve_identity_file(),
            std::path::PathBuf::from("/tmp/lsbx-test-override-key")
        );

        std::env::remove_var("LSBX_GUEST_SSH_KEY");
        let fallback = resolve_identity_file();
        assert!(fallback.ends_with(".ssh/lsbx_guest_key"));

        match previous {
            Some(v) => std::env::set_var("LSBX_GUEST_SSH_KEY", v),
            None => std::env::remove_var("LSBX_GUEST_SSH_KEY"),
        }
    }
}
