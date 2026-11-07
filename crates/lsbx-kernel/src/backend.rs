#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub console: bool,
    pub remote_transport: bool,
    pub snapshot: bool,
}

pub struct CreatedVm {
    pub vm_tag: String,
    pub host: String,
    pub https_url: Option<String>,
}

pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

use crate::types::GoldenKey;

pub struct CreateFromGoldenRequest<'a> {
    pub golden: &'a GoldenKey,
    pub name: &'a str,
    pub pubkey: &'a str,
    pub cpu: u32,
    pub memory: &'a str,
    /// The golden's declared `os` string (manifest `GoldenConfig.os`). The
    /// backend uses it to render guest-appropriate domain XML (e.g.
    /// `"windows"` — UEFI/TPM/SecureBoot instead of BIOS, and no cloud-init
    /// seed ISO since Windows guests don't run cloud-init); every other
    /// value is treated as a Linux-style BIOS guest.
    pub os: &'a str,
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    async fn create_from_golden(
        &self,
        req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, crate::error::LsbxError>;
    async fn run(
        &self,
        vm_tag: &str,
        command: &[String],
        timeout: std::time::Duration,
        identity_file: Option<&std::path::Path>,
    ) -> Result<CommandOutput, crate::error::LsbxError>;
    async fn put_file(
        &self,
        vm_tag: &str,
        source: &std::path::Path,
        destination: &str,
        identity_file: Option<&std::path::Path>,
    ) -> Result<(), crate::error::LsbxError>;
    async fn get_file(
        &self,
        vm_tag: &str,
        source: &str,
        destination: &std::path::Path,
        identity_file: Option<&std::path::Path>,
    ) -> Result<(), crate::error::LsbxError>;
    async fn destroy(&self, vm_tag: &str) -> Result<(), crate::error::LsbxError>;
    /// Associate ephemeral key material with a VM for backends that cache it.
    /// Backends that accept identity paths per call may inherit this no-op.
    async fn register_vm_key(
        &self,
        _vm_tag: &str,
        _key_path: &std::path::Path,
    ) -> Result<(), crate::error::LsbxError> {
        Ok(())
    }
    async fn list_vms(&self) -> Result<Vec<String>, crate::error::LsbxError>;
    /// The golden artifacts this backend can actually see right now, as the
    /// keys `list_golden_artifacts`'s caller compares against a manifest's
    /// `base` field.
    ///
    /// "Where a golden lives" is backend-specific and the trait has to say so
    /// once rather than leave every caller to assume one model:
    ///
    /// - Backends where a golden **is a long-lived VM** (exedev) contribute
    ///   their VM inventory, filtered to the golden-base naming convention
    ///   (`lsbx-<name>-v<n>`) so per-job sandboxes don't look like goldens —
    ///   that is this default implementation.
    /// - Backends where a golden **is a disk image** (libvirt) override this
    ///   with the image directory's contents; their `list_vms` returns
    ///   *domains*, which says nothing about which goldens still exist.
    ///
    /// Getting this wrong is not subtle in the output: a caller that used
    /// `list_vms` on a libvirt host would report every manifest golden as
    /// missing, because golden images are qcow2 files and the only domains
    /// are the VMs built from them.
    async fn list_golden_artifacts(&self) -> Result<Vec<String>, crate::error::LsbxError> {
        Ok(self
            .list_vms()
            .await?
            .into_iter()
            .filter(|vm| vm.starts_with("lsbx-"))
            .collect())
    }
    /// Reconcile backend-specific ephemeral credentials left by interrupted
    /// lifecycle operations. Backends without account-level key state are a
    /// no-op; the lifecycle reaper still invokes this uniformly.
    async fn reconcile_orphaned_keys(
        &self,
        _known_labels: &[String],
    ) -> Result<usize, crate::error::LsbxError> {
        Ok(0)
    }
    /// Remove a VM and, when supported, revoke its per-sandbox public key.
    async fn destroy_with_key(
        &self,
        vm_tag: &str,
        _pubkey: &str,
    ) -> Result<(), crate::error::LsbxError> {
        self.destroy(vm_tag).await
    }
    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), crate::error::LsbxError>;
}
