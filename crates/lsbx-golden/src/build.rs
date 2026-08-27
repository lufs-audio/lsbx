//! `golden build` — composes a base image or base golden with a
//! provisioning script, executed inside a VM launched through the
//! `Backend` trait, and (optionally) registers the result.
//!
//! ## Rework note (this file was substantially rewritten from an earlier
//! draft that called a nonexistent `Backend::run(&self, script: &Path) ->
//! Result<(), LsbxError>`)
//!
//! The real `Backend` trait (`lsbx_kernel::backend::Backend`, landed by
//! Unit 01 and implemented for real by Unit 05's `DemoBackend`) has no
//! method that takes a bare script path. Its actual shape is:
//!
//! ```ignore
//! async fn create_from_golden(&self, req: CreateFromGoldenRequest<'_>) -> Result<CreatedVm, LsbxError>;
//! async fn run(&self, vm_tag: &str, command: &[String], timeout: Duration) -> Result<CommandOutput, LsbxError>;
//! async fn put_file(&self, vm_tag: &str, source: &Path, destination: &str) -> Result<(), LsbxError>;
//! async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError>;
//! ```
//!
//! There is no "run this script" primitive on the trait at all — running a
//! provisioning script inside a VM is a *composition* of `create_from_golden`
//! (get a VM), `put_file` (copy the script onto it), and `run` (invoke it as
//! a command on the guest), not a single call. `golden_build`'s control flow
//! is therefore:
//!
//! 1. `create_from_golden` — provision a fresh build VM from `req.from`.
//! 2. `put_file` — copy the provisioning script onto the build VM at a fixed
//!    remote path.
//! 3. `run` — execute the script on the guest via `["sh", "<remote_path>"]`,
//!    and treat a nonzero `exit_code` in the returned `CommandOutput` as a
//!    build failure (`LsbxError::ContractViolated`), since a `run` call that
//!    itself returns `Ok` only proves the command executor was reached, not
//!    that the provisioning script exited successfully — that's precisely
//!    the "ran vs. proven" distinction SPEC.md §1 calls out.
//! 4. `destroy` (only when `req.cleanup` is true) — tear down the build VM.
//!    When `req.cleanup` is false, the build VM is deliberately left
//!    running (e.g. for `--interactive`/`--shell` follow-up per the unit's
//!    documented CLI surface) and its `vm_tag` is threaded through so a
//!    caller retains a handle to it.
//! 5. Flatten the resulting disk into a single self-contained image. This
//!    unit's own Boundaries section says flatten belongs to Unit 19
//!    ("delegated to Unit 19's flatten operation through a narrow
//!    trait/callback, not reimplemented here"), and Unit 19 has not landed
//!    yet (SPEC.md §5 places it in Layer 8, after this unit's Layer 4).
//!    `GoldenFlattener` below is that narrow seam: a one-method trait this
//!    crate depends on and calls through, but does not implement. Passing
//!    `None` for it is honest about the gap (see `NoFlatten` and the
//!    "Deferred to Unit 19" doc comment on the trait) rather than
//!    reimplementing `qemu-img` invocation here, which SPEC.md §0 Deviation
//!    7 explicitly reserves for the flatten path.
//! 6. `content_hash` — compute the real `lufs-<sha256[:8]>` hash over the
//!    *final* (post-flatten, when flattening ran) disk bytes, per
//!    SPEC.md Deviation 3.
//!
//! ## Why `GoldenBuildRequest` carries a `pubkey` field
//! The unit contract's literal `GoldenBuildRequest` struct (written before
//! the real `Backend` trait was confirmed) has no `pubkey` field, but
//! `Backend::create_from_golden` requires one (`CreateFromGoldenRequest`
//! has a mandatory `pubkey: &'a str`). Generating an ephemeral keypair is
//! explicitly out of scope for this crate (that's Unit 03's
//! `lsbx-keys`/Unit 09's VM-lifecycle orchestration) — so rather than
//! reimplementing key generation here (a second real scope violation on
//! top of the one already flagged), the caller (eventually Unit 09 via
//! `lsbx-ops::golden_build`) is expected to generate the ephemeral keypair
//! and pass the public half through. This is a deliberate, documented
//! addition to the interface contract's struct, not an oversight.

use crate::registry::{GoldenConfig, GoldenFlavor, GoldenMode, StreamingMode};
use lsbx_kernel::backend::{Backend, CreateFromGoldenRequest};
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::GoldenKey;
use std::path::Path;
use std::time::Duration;

/// Default timeout for the provisioning-script `run` call inside the build
/// VM. Provisioning scripts (package installs, image customization) run
/// materially longer than a healthcheck command, hence the much larger
/// window than `verify.rs`'s per-healthcheck timeout.
const BUILD_SCRIPT_TIMEOUT: Duration = Duration::from_secs(1800);

/// The fixed remote path a provisioning script is copied to inside the
/// build VM before being executed via `sh`.
const REMOTE_SCRIPT_PATH: &str = "/tmp/lsbx-golden-build-script.sh";

/// Narrow seam for the qcow2-flatten operation this unit's contract
/// explicitly defers to Unit 19 (`lsbx-bootstrap`'s `src/flatten.rs`).
///
/// Deferred to Unit 19: this crate depends on the *shape* of "flatten a
/// disk owned by this VM into a single self-contained image," not on the
/// actual `qemu-img convert`/`virsh` subprocess invocation, which SPEC.md
/// §0 Deviation 7 places outside native Rust and outside this unit's
/// Boundaries ("Does not implement the actual qcow2 flatten operation").
/// Once Unit 19 lands a concrete implementation of this trait (or replaces
/// this trait with whatever seam it actually ships), `golden_build` calls
/// through it unchanged.
#[async_trait::async_trait]
pub trait GoldenFlattener: Send + Sync {
    /// Flattens the disk backing `vm_tag` into a single self-contained
    /// qcow2 image and returns its path.
    async fn flatten(&self, vm_tag: &str) -> Result<std::path::PathBuf, LsbxError>;
}

/// A `GoldenFlattener` that always reports the gap honestly rather than
/// silently faking a flatten result.
///
/// Used when no real flattener is available yet (the common case until
/// Unit 19 lands). Returns `LsbxError::ContractViolated` naming the disk
/// that was never flattened, rather than returning `Ok` with an unflattened
/// path pretending the operation happened — an unflattened qcow2 (still
/// backed by its base image's backing-file chain) is not the "single
/// self-contained image" a golden is supposed to be, and content-hashing
/// it would silently produce a hash that isn't stable across the backing
/// chain's lifetime.
pub struct NoFlatten;

#[async_trait::async_trait]
impl GoldenFlattener for NoFlatten {
    async fn flatten(&self, vm_tag: &str) -> Result<std::path::PathBuf, LsbxError> {
        Err(LsbxError::ContractViolated(format!(
            "no GoldenFlattener is available to flatten the build disk for VM '{}' — \
             qcow2 flatten is Unit 19's (lsbx-bootstrap) responsibility per this unit's \
             Boundaries and has not landed yet; pass a real GoldenFlattener impl once it does",
            vm_tag
        )))
    }
}

pub struct GoldenBuildRequest<'a> {
    pub name: &'a str,
    pub from: &'a str,
    pub script: &'a Path,
    pub flavor: GoldenFlavor,
    pub cpu: u32,
    pub memory: &'a str,
    pub streaming: StreamingMode,
    pub register: bool,
    pub cleanup: bool,
    pub dry_run: bool,
    /// Public half of an ephemeral keypair to hand `Backend::create_from_golden`.
    pub pubkey: &'a str,
    /// Private half corresponding to `pubkey`, when the caller has one.
    /// Exedev uses it for upload/run and key revocation; other backends may
    /// ignore it.
    pub key_path: Option<&'a Path>,
}

/// Result of a non-dry-run build: the registered `GoldenConfig`, plus the
/// `vm_tag` of the build VM when it was deliberately left running
/// (`req.cleanup == false`) so a caller can attach to it (e.g. for
/// `--interactive`/`--shell`).
pub struct GoldenBuildOutcome {
    pub config: GoldenConfig,
    pub build_vm_tag: Option<String>,
}

/// Provisions a golden image by running `req.script` inside a fresh VM
/// created from `req.from`, per the 5-step flow documented on this module.
///
/// `flattener` is the Unit-19 seam (see `GoldenFlattener` above); pass
/// `None` to get an honest `ContractViolated` error instead of a faked
/// flatten result when no real flattener is wired up yet.
pub async fn golden_build(
    backend: &dyn Backend,
    req: GoldenBuildRequest<'_>,
    flattener: Option<&dyn GoldenFlattener>,
) -> Result<GoldenBuildOutcome, LsbxError> {
    if req.dry_run {
        return Ok(GoldenBuildOutcome {
            config: GoldenConfig {
                key: req.name.to_string(),
                flavor: req.flavor,
                os: "linux".to_string(),
                base: req.from.to_string(),
                mode: GoldenMode::Copy,
                cpu: req.cpu,
                memory: req.memory.to_string(),
                disk: None,
                streaming: req.streaming,
                capabilities: vec![],
                healthcheck: vec![],
                repo: None,
                content_hash: Some("lufs-dryrun".to_string()),
                description: "Dry run build".to_string(),
            },
            build_vm_tag: None,
        });
    }

    // Validate `req.name` up front, before any backend call, so a bad name
    // fails fast as Usage rather than after provisioning a VM we'd then
    // have to unwind.
    let key = crate::registry::ImageRegistry::validate_key(req.name)?;

    // Step 1: provision a fresh build VM from `req.from`. `req.from` names
    // a base image or base golden (the unit's CLI surface: `--from
    // <base>`); `create_from_golden` takes a `&GoldenKey`, so validate it
    // the same way any other golden reference is validated in this crate.
    let from_key: GoldenKey = crate::registry::ImageRegistry::validate_key(req.from)?;

    let created = backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &from_key,
            name: req.name,
            pubkey: req.pubkey,
            cpu: req.cpu,
            memory: req.memory,
        })
        .await?;
    let vm_tag = created.vm_tag;

    // From this point on, any early return must still respect `req.cleanup`
    // for the VM we just created — a helper keeps that from being
    // duplicated (or forgotten) at every fallible step below.
    let result = run_build_steps(backend, &vm_tag, req.script, req.key_path, flattener).await;

    if req.cleanup {
        // Best-effort teardown: a destroy failure here must not mask the
        // *build's* real error (or silently report success when the build
        // itself failed) — but it also must not be swallowed entirely,
        // since a build VM nobody destroyed is exactly the kind of leak
        // Unit 09's reaper exists to catch, and this call already had a
        // clean chance to prevent it. Log-and-continue is what `Backend`
        // implementations themselves have no channel for, but a real
        // orchestration layer above this (`lsbx-ops`/Unit 09) does, via
        // `tracing`; this crate does not depend on `tracing` per its own
        // Cargo.toml scope, so surface the destroy failure by folding it
        // into the returned error only when the build itself otherwise
        // succeeded (never overwrite a real build failure with a cleanup
        // failure).
        let destroy_result = if req.key_path.is_some() {
            backend.destroy_with_key(&vm_tag, req.pubkey).await
        } else {
            backend.destroy(&vm_tag).await
        };
        if let Err(destroy_err) = destroy_result {
            if result.is_ok() {
                return Err(LsbxError::ContractViolated(format!(
                    "golden build for '{}' succeeded but cleanup of build VM '{}' failed: {}",
                    req.name, vm_tag, destroy_err
                )));
            }
            // Build already failed; the destroy failure is secondary and a
            // real caller's reaper/orphan-sweep will still catch this VM
            // via the `lsbx:<label>` convention. Fall through to return the
            // original build error, not the destroy error.
        }
    }

    let content_hash = result?;

    let config = GoldenConfig {
        key: key.as_str().to_string(),
        flavor: req.flavor,
        os: "linux".to_string(),
        base: req.from.to_string(),
        mode: GoldenMode::Copy,
        cpu: req.cpu,
        memory: req.memory.to_string(),
        disk: None,
        streaming: req.streaming,
        capabilities: vec![],
        healthcheck: vec![],
        repo: None,
        content_hash: Some(content_hash),
        description: format!("Built from {}", req.from),
    };

    Ok(GoldenBuildOutcome {
        config,
        build_vm_tag: if req.cleanup { None } else { Some(vm_tag) },
    })
}

/// Steps 2-3 and 5-6 of the build flow: copy the script onto the VM, run
/// it, flatten the result (or honestly fail if no flattener is available),
/// and compute the final content hash. Factored out of `golden_build` so
/// the caller above can still run cleanup (step 4) uniformly whether these
/// steps succeed or fail.
async fn run_build_steps(
    backend: &dyn Backend,
    vm_tag: &str,
    script: &Path,
    key_path: Option<&Path>,
    flattener: Option<&dyn GoldenFlattener>,
) -> Result<String, LsbxError> {
    // Step 2: copy the provisioning script onto the build VM.
    backend
        .put_file(vm_tag, script, REMOTE_SCRIPT_PATH, key_path)
        .await?;

    // Step 3: actually execute it on the guest. `run`'s `Ok` only means the
    // command executor was reached and returned an exit code — the exit
    // code itself still has to be checked, since "ran" and "proven" are
    // deliberately different claims in this system (SPEC.md §1).
    let output = backend
        .run(
            vm_tag,
            &["sh".to_string(), REMOTE_SCRIPT_PATH.to_string()],
            BUILD_SCRIPT_TIMEOUT,
            key_path,
        )
        .await?;

    if output.exit_code != 0 {
        return Err(LsbxError::ContractViolated(format!(
            "provisioning script {} exited with status {} on build VM '{}': stderr: {}",
            script.display(),
            output.exit_code,
            vm_tag,
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Step 5: flatten. No real flattener wired up yet == honest failure,
    // not a faked hash over an unflattened disk.
    let flattener = flattener.ok_or_else(|| {
        LsbxError::ContractViolated(format!(
            "no GoldenFlattener supplied for build VM '{}' — flatten is Unit 19's \
             responsibility and no implementation has landed yet",
            vm_tag
        ))
    })?;
    let flattened_path = flattener.flatten(vm_tag).await?;

    // Step 6: real content hash over the final, flattened disk.
    crate::hash::content_hash(&flattened_path)
}

// See registry.rs's identically-worded comment above its own test module for
// why this scoped allow exists (Unit 01's crates/lsbx-kernel/tests/test_kernel.rs
// pattern, applied to a #[cfg(test)] mod instead of a separate tests/*.rs file).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use lsbx_backend_demo::DemoBackend;

    fn write_script(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let path = dir.path().join("provision.sh");
        std::fs::write(&path, "#!/bin/sh\necho provisioning\n").expect("write script");
        path
    }

    #[tokio::test]
    async fn dry_run_returns_canned_config_without_touching_backend() {
        let backend = DemoBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let outcome = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "agent-base",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: true,
                dry_run: true,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            None,
        )
        .await
        .expect("dry run should succeed");

        assert_eq!(outcome.config.key, "agent-base");
        assert_eq!(outcome.config.content_hash, Some("lufs-dryrun".to_string()));
        assert!(outcome.build_vm_tag.is_none());
        // Backend should have zero VMs — dry run must not call create_from_golden.
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    #[tokio::test]
    async fn build_without_flattener_fails_honestly_not_silently() {
        let backend = DemoBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let result = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "agent-base",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: true,
                dry_run: false,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            None, // no flattener supplied
        )
        .await;

        assert!(matches!(result, Err(LsbxError::ContractViolated(_))));
        // Cleanup still ran (cleanup: true) even though the build failed —
        // the build VM must not leak.
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    #[tokio::test]
    async fn build_rejects_invalid_golden_name_before_touching_backend() {
        let backend = DemoBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let result = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "Not Valid!",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: true,
                dry_run: false,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            None,
        )
        .await;

        assert!(matches!(result, Err(LsbxError::Usage(_))));
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    struct FakeFlattener {
        path: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl GoldenFlattener for FakeFlattener {
        async fn flatten(&self, _vm_tag: &str) -> Result<std::path::PathBuf, LsbxError> {
            Ok(self.path.clone())
        }
    }

    #[tokio::test]
    async fn build_with_flattener_end_to_end_via_demo_backend_produces_real_content_hash() {
        let backend = DemoBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let flattened = dir.path().join("flattened.qcow2");
        std::fs::write(&flattened, b"pretend flattened qcow2 bytes").expect("write flattened");
        let flattener = FakeFlattener {
            path: flattened.clone(),
        };

        let outcome = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "agent-base",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: true,
                dry_run: false,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            Some(&flattener),
        )
        .await
        .expect("build should succeed");

        let expected_hash = crate::hash::content_hash(&flattened).expect("hash");
        assert_eq!(outcome.config.content_hash, Some(expected_hash));
        assert_eq!(outcome.config.key, "agent-base");
        assert_eq!(outcome.config.base, "lsbx-default-v1");
        // cleanup: true means no leftover VM.
        assert!(outcome.build_vm_tag.is_none());
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    #[tokio::test]
    async fn build_with_cleanup_false_leaves_vm_running_and_returns_its_tag() {
        let backend = DemoBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let flattened = dir.path().join("flattened.qcow2");
        std::fs::write(&flattened, b"pretend flattened qcow2 bytes").expect("write flattened");
        let flattener = FakeFlattener {
            path: flattened.clone(),
        };

        let outcome = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "agent-base",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: false,
                dry_run: false,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            Some(&flattener),
        )
        .await
        .expect("build should succeed");

        assert!(outcome.build_vm_tag.is_some());
        // VM should still be alive since cleanup was false.
        let vms = backend.list_vms().await.expect("list_vms");
        assert_eq!(vms.len(), 1);
        assert_eq!(Some(vms[0].clone()), outcome.build_vm_tag);
    }

    #[tokio::test]
    async fn build_against_unavailable_backend_surfaces_backend_unavailable() {
        let backend = DemoBackend::with_fault(lsbx_backend_demo::FaultMode::Unavailable);
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(&dir);

        let result = golden_build(
            &backend,
            GoldenBuildRequest {
                key_path: None,
                name: "agent-base",
                from: "lsbx-default-v1",
                script: &script,
                flavor: GoldenFlavor::Agent,
                cpu: 2,
                memory: "2G",
                streaming: StreamingMode::None,
                register: false,
                cleanup: true,
                dry_run: false,
                pubkey: "ssh-ed25519 AAAA fake",
            },
            None,
        )
        .await;

        assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));
    }
}
