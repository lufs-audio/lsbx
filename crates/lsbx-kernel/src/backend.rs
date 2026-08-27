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
