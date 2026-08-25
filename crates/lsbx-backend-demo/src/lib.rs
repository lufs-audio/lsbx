use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    None,
    Unavailable,
    HangOnRun,
    PartialDestroyFailure,
}

#[derive(Clone)]
#[allow(dead_code)]
struct DemoVm {
    vm_tag: String,
    host: String,
    https_url: Option<String>,
}

pub struct DemoBackend {
    internal: Arc<Mutex<HashMap<String, DemoVm>>>,
    fault: FaultMode,
}

/// Turns a poisoned-mutex `.lock()` failure into an `LsbxError`.
///
/// A poisoned `Mutex` means some *earlier* operation on this backend panicked
/// while holding the lock -- it is not "another caller currently holds this
/// lock" (that's what `LockContention` names, and `std::sync::Mutex::lock`
/// blocks rather than erroring in that case anyway). Poisoning is this
/// backend's own internal invariant having been violated by a prior panic,
/// so `ContractViolated` is the semantically correct mapping; `LockContention`
/// is reserved for real contention (e.g. `lsbx-store`'s `flock`-based sentinel
/// finding another process already holds the lock).
fn lock_poisoned_error<T>(_e: std::sync::PoisonError<T>) -> LsbxError {
    LsbxError::ContractViolated(
        "DemoBackend internal mutex was poisoned by a prior panic while holding the lock".to_string(),
    )
}

impl DemoBackend {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            internal: Arc::new(Mutex::new(HashMap::new())),
            fault: FaultMode::None,
        }
    }

    pub fn with_fault(mode: FaultMode) -> Self {
        Self {
            internal: Arc::new(Mutex::new(HashMap::new())),
            fault: mode,
        }
    }
}

#[async_trait::async_trait]
impl Backend for DemoBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            console: true,
            remote_transport: false,
            snapshot: false,
        }
    }

    async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError> {
        if self.fault == FaultMode::Unavailable {
            return Err(LsbxError::BackendUnavailable(
                "DemoBackend is configured to be unavailable".to_string(),
            ));
        }

        // Deterministic vm_tag/host: identical (golden, name) inputs must
        // produce identical output across independent DemoBackend instances
        // (Unit 05 acceptance criteria), so this hashes only the inputs that
        // define VM identity, never anything instance-specific (no random
        // seed, no counter, no clock read).
        let mut hasher = Sha256::new();
        hasher.update(req.golden.as_str().as_bytes());
        hasher.update(b":");
        hasher.update(req.name.as_bytes());
        let result = hasher.finalize();
        let hash_hex = hex::encode(result);

        let vm_tag = format!("demo-{}", &hash_hex[..12]);
        let host = format!("{}.demo.local", &hash_hex[..12]);
        let https_url = Some(format!("https://{}/novnc", host));

        let vm = DemoVm {
            vm_tag: vm_tag.clone(),
            host: host.clone(),
            https_url: https_url.clone(),
        };

        {
            let mut map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.insert(vm_tag.clone(), vm);
        }

        Ok(CreatedVm {
            vm_tag,
            host,
            https_url,
        })
    }

    async fn run(&self, vm_tag: &str, _command: &[String], timeout: Duration, _identity_file: Option<&std::path::Path>) -> Result<CommandOutput, LsbxError> {
        if self.fault == FaultMode::Unavailable {
            return Err(LsbxError::BackendUnavailable(
                "DemoBackend is configured to be unavailable".to_string(),
            ));
        }

        let exists = {
            let map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.contains_key(vm_tag)
        };

        if !exists {
            return Err(LsbxError::NotFound(format!("VM {} not found", vm_tag)));
        }

        if self.fault == FaultMode::HangOnRun {
            tokio::time::sleep(timeout + Duration::from_secs(1)).await;
        }

        Ok(CommandOutput {
            exit_code: 0,
            stdout: vec![],
            stderr: vec![],
        })
    }

    async fn put_file(&self, vm_tag: &str, _source: &std::path::Path, _destination: &str, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        let exists = {
            let map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.contains_key(vm_tag)
        };
        if !exists {
            return Err(LsbxError::NotFound(format!("VM {} not found", vm_tag)));
        }
        Ok(())
    }

    async fn get_file(&self, vm_tag: &str, _source: &str, _destination: &std::path::Path, _identity_file: Option<&std::path::Path>) -> Result<(), LsbxError> {
        let exists = {
            let map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.contains_key(vm_tag)
        };
        if !exists {
            return Err(LsbxError::NotFound(format!("VM {} not found", vm_tag)));
        }
        Ok(())
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        if self.fault == FaultMode::Unavailable {
            return Err(LsbxError::BackendUnavailable(
                "DemoBackend is configured to be unavailable".to_string(),
            ));
        }

        // PartialDestroyFailure models "the destroy attempt failed" -- not
        // "the destroy attempt silently succeeded but we lied about it." The
        // VM must still exist in the map afterward, so a caller that retries
        // destroy() on the same vm_tag (e.g. Unit 09's reap loop, which is
        // the documented reason this fault mode exists -- see its own
        // Verification scenario: "a sandbox whose destroy call fails is NOT
        // removed from the store... retried on the next reap pass") can
        // actually re-attempt the operation and have it succeed, rather than
        // immediately getting NotFound because this backend already deleted
        // the VM behind the caller's back before reporting failure.
        if self.fault == FaultMode::PartialDestroyFailure {
            let exists = {
                let map = self.internal.lock().map_err(lock_poisoned_error)?;
                map.contains_key(vm_tag)
            };
            if !exists {
                return Err(LsbxError::NotFound(format!("VM {} not found", vm_tag)));
            }
            return Err(LsbxError::BackendUnavailable(
                "Partial destroy failure".to_string(),
            ));
        }

        let removed = {
            let mut map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.remove(vm_tag)
        };

        if removed.is_none() {
            return Err(LsbxError::NotFound(format!("VM {} not found", vm_tag)));
        }

        Ok(())
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        if self.fault == FaultMode::Unavailable {
            return Err(LsbxError::BackendUnavailable(
                "DemoBackend is configured to be unavailable".to_string(),
            ));
        }

        let keys = {
            let map = self.internal.lock().map_err(lock_poisoned_error)?;
            map.keys().cloned().collect()
        };

        Ok(keys)
    }

    async fn rename_vm(&self, old_tag: &str, new_tag: &str) -> Result<(), LsbxError> {
        if self.fault == FaultMode::Unavailable {
            return Err(LsbxError::BackendUnavailable(
                "DemoBackend is configured to be unavailable".to_string(),
            ));
        }

        let mut map = self.internal.lock().map_err(lock_poisoned_error)?;
        if let Some(vm) = map.remove(old_tag) {
            let mut new_vm = vm;
            new_vm.vm_tag = new_tag.to_string();
            map.insert(new_tag.to_string(), new_vm);
            Ok(())
        } else {
            Err(LsbxError::NotFound(format!("VM {} not found", old_tag)))
        }
    }
}
