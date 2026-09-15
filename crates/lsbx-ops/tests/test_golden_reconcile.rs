//! Tests for `LsbxOps::golden_reconcile` — the golden-reconciliation
//! operation added 2026-09-15 (exedev-golden-discovery gap).
//!
//! `DemoBackend` cannot impersonate the scenario under test: its
//! `list_vms()` returns only the VM tags of sandboxes it actually created,
//! and golden base VMs (`lsbx-default-v1` et al.) are, by definition, VMs
//! the CLI did NOT create through this process. So these tests drive a
//! minimal, purpose-built backend stub whose VM inventory is settable per
//! test — the same "implement the trait, control the world" pattern the
//! backend-auto-probe tests use, kept honest by implementing the real
//! `lsbx_kernel::backend::Backend` trait rather than mocking the façade.
//!
//! Classification under test (see `golden_reconcile`'s own doc comment):
//! - manifest golden whose `base` is live on the backend → `present`
//! - manifest golden whose `base` is not live → `missing`
//! - live VM following the golden-base naming convention (`lsbx-*-v*`)
//!   with no manifest entry → listed in `unregistered_vms`
//! - a `list_vms` failure propagates verbatim (never masquerades as an
//!   empty inventory)
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use lsbx_golden::registry::{GoldenConfig, GoldenFlavor, GoldenMode, ImageRegistry, StreamingMode};
use lsbx_kernel::backend::{
    Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm,
};
use lsbx_kernel::clock::FakeClock;
use lsbx_kernel::error::LsbxError;
use lsbx_ops::{GoldenReconcileReport, LsbxOps};
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// A backend whose only interesting behavior is a fixed, caller-chosen VM
/// inventory — everything else is the documented "not this test's concern"
/// error surface, exactly like `DemoBackend`'s fault modes.
struct FixedInventoryBackend {
    vms: Vec<String>,
    unavailable: bool,
}

impl FixedInventoryBackend {
    fn with_vms(vms: &[&str]) -> Self {
        Self {
            vms: vms.iter().map(|s| s.to_string()).collect(),
            unavailable: false,
        }
    }

    fn unavailable() -> Self {
        Self {
            vms: Vec::new(),
            unavailable: true,
        }
    }
}

#[async_trait]
impl Backend for FixedInventoryBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn create_from_golden(
        &self,
        _req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot create VMs".to_string(),
        ))
    }

    async fn run(
        &self,
        _vm_tag: &str,
        _command: &[String],
        _timeout: Duration,
        _identity_file: Option<&std::path::Path>,
    ) -> Result<CommandOutput, LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot run commands".to_string(),
        ))
    }

    async fn put_file(
        &self,
        _vm_tag: &str,
        _source: &std::path::Path,
        _destination: &str,
        _identity_file: Option<&std::path::Path>,
    ) -> Result<(), LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot put files".to_string(),
        ))
    }

    async fn get_file(
        &self,
        _vm_tag: &str,
        _source: &str,
        _destination: &std::path::Path,
        _identity_file: Option<&std::path::Path>,
    ) -> Result<(), LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot get files".to_string(),
        ))
    }

    async fn destroy(&self, _vm_tag: &str) -> Result<(), LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot destroy VMs".to_string(),
        ))
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        if self.unavailable {
            return Err(LsbxError::BackendUnavailable(
                "FixedInventoryBackend is configured to be unavailable".to_string(),
            ));
        }
        Ok(self.vms.clone())
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        Err(LsbxError::ContractViolated(
            "FixedInventoryBackend is a reconcile-test stub; it cannot rename VMs".to_string(),
        ))
    }
}

/// The two-golden manifest shape the real `images.json` uses: `agent-base`
/// cloned from `lsbx-default-v1`, `ci-runner` from `lsbx-ci-v1`.
fn golden(key: &str, base: &str) -> GoldenConfig {
    GoldenConfig {
        key: key.to_string(),
        flavor: GoldenFlavor::Agent,
        os: "linux".to_string(),
        base: base.to_string(),
        mode: GoldenMode::Copy,
        cpu: 2,
        memory: "4GB".to_string(),
        disk: None,
        streaming: StreamingMode::None,
        capabilities: vec![],
        healthcheck: vec![],
        repo: None,
        content_hash: None,
        description: format!("{key} test golden"),
    }
}

/// `LsbxOps` over the given backend, with a registry containing exactly the
/// two real goldens above, and an isolated temp-dir-backed store.
fn build_ops_with(
    backend: FixedInventoryBackend,
) -> (LsbxOps, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![
            golden("agent-base", "lsbx-default-v1"),
            golden("ci-runner", "lsbx-ci-v1"),
        ],
        profiles: HashMap::new(),
    };
    let ops = LsbxOps::new(
        Box::new(backend),
        "stub".to_string(),
        SandboxStore::new(dir.path().to_path_buf()),
        CiJobStore::new(dir.path().to_path_buf()),
        registry,
        Box::new(FakeClock {
            now: SystemTime::now(),
        }),
    );
    (ops, dir)
}

/// Both real goldens present, one golden-shaped VM unregistered, and a
/// non-golden-shaped VM (`molimo`) correctly excluded from the
/// unregistered list.
#[tokio::test]
async fn reconcile_classifies_present_missing_and_unregistered() {
    let (ops, _dir) = build_ops_with(FixedInventoryBackend::with_vms(&[
        "lsbx-default-v1", // agent-base's base → present
        "lsbx-web-v1",     // golden-shaped, no manifest entry → unregistered
        "molimo",          // agent's own VM, not golden-shaped → invisible
    ]));

    let report: GoldenReconcileReport = ops.golden_reconcile().await.expect("reconcile");

    assert_eq!(report.items.len(), 2, "one item per manifest golden");
    let agent = report
        .items
        .iter()
        .find(|item| item.key == "agent-base")
        .expect("agent-base item");
    assert_eq!(agent.base, "lsbx-default-v1");
    assert_eq!(agent.status, "present");

    let ci = report
        .items
        .iter()
        .find(|item| item.key == "ci-runner")
        .expect("ci-runner item");
    assert_eq!(ci.status, "missing", "lsbx-ci-v1 is not live in this scenario");

    assert_eq!(
        report.unregistered_vms,
        vec!["lsbx-web-v1".to_string()],
        "only golden-shaped VMs without a manifest entry are unregistered"
    );
}

/// Empty registry against a live golden-shaped inventory: no items, and
/// every golden-shaped VM lands in `unregistered_vms` — the exact
/// "manifest never loaded" scenario from the 2026-09-15 gap.
#[tokio::test]
async fn reconcile_empty_registry_lists_all_golden_shaped_vms_as_unregistered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ops = LsbxOps::new(
        Box::new(FixedInventoryBackend::with_vms(&[
            "lsbx-default-v1",
            "lsbx-web-v1",
            "lsbx-ci-v1",
        ])),
        "stub".to_string(),
        SandboxStore::new(dir.path().to_path_buf()),
        CiJobStore::new(dir.path().to_path_buf()),
        ImageRegistry {
            images: vec![],
            goldens: vec![],
            profiles: HashMap::new(),
        },
        Box::new(FakeClock {
            now: SystemTime::now(),
        }),
    );

    let report = ops.golden_reconcile().await.expect("reconcile");

    assert!(report.items.is_empty());
    assert_eq!(
        report.unregistered_vms,
        vec![
            "lsbx-default-v1".to_string(),
            "lsbx-web-v1".to_string(),
            "lsbx-ci-v1".to_string(),
        ]
    );
}

/// A control-plane failure is returned verbatim, never folded into an
/// empty/present-shaped report — the same honesty rule `status()` follows.
#[tokio::test]
async fn reconcile_propagates_backend_unavailable_verbatim() {
    let (ops, _dir) = build_ops_with(FixedInventoryBackend::unavailable());

    let result = ops.golden_reconcile().await;

    assert!(
        matches!(result, Err(LsbxError::BackendUnavailable(_))),
        "reconcile against a dead control plane must fail as BackendUnavailable, got: {result:?}"
    );
}
