//! `golden verify` — creates a fresh instance of a golden and runs its
//! declared `healthcheck` command list inside it, reporting pass/fail per
//! check (not just "the VM booted").
//!
//! ## Rework note
//! Same underlying problem as `build.rs`: an earlier draft called a
//! nonexistent `backend.run(script_file.path())`. There is no
//! "run this script" primitive and no bare-path `run` overload on the real
//! `Backend` trait. `golden_verify`'s real control flow:
//!
//! 1. `create_from_golden` — provision a fresh instance from the golden
//!    under test (never verify against a golden's *build* VM — a golden
//!    represents what a freshly-created instance looks like, so verifying
//!    against anything else wouldn't prove what `golden verify` claims to
//!    prove).
//! 2. For each command in `golden.healthcheck`, call the real
//!    `Backend::run(vm_tag, &[cmd], timeout)` (splitting `cmd` into
//!    argv-shaped pieces via a shell tokenizer is exactly the kind of
//!    interpolation SPEC.md warns against elsewhere — instead each
//!    healthcheck string is run the same way the provisioning script is in
//!    `build.rs`: through an explicit `sh -c '<cmd>'` argv array, never
//!    shell-interpolated into a larger string), and record a
//!    `HealthcheckResult` per command based on its actual exit code, not on
//!    whether the `run` call itself returned `Ok` (an `Ok` `CommandOutput`
//!    with `exit_code != 0` is a **failed** healthcheck, not a successful
//!    `run` call that happens to report failure — proven, not just ran).
//! 3. `destroy` the verification instance when done, regardless of whether
//!    any healthcheck failed — a verify run must never leak a VM, since
//!    unlike `golden build` there is no `--no-cleanup` flag on `golden
//!    verify`'s CLI surface to justify leaving one running.
//!
//! A `Backend` call failure that isn't a healthcheck failure (e.g.
//! `create_from_golden` returning `BackendUnavailable`) still propagates as
//! an `Err` from `golden_verify` itself, since that's an infrastructure
//! failure preventing verification from running at all, not a
//! "verification ran and found a problem" result.

use crate::registry::GoldenConfig;
use lsbx_kernel::backend::{Backend, CreateFromGoldenRequest};
use lsbx_kernel::error::LsbxError;
use std::time::Duration;

/// Per-healthcheck timeout. Healthchecks are expected to be quick,
/// diagnostic commands (SPEC.md §2's "Verification" noun: "golden
/// healthchecks, VM readiness gates"), not long-running provisioning —
/// hence a much smaller window than `build.rs`'s script timeout.
const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(60);

pub struct HealthcheckResult {
    pub command: String,
    pub passed: bool,
    pub output: String,
}

/// Creates a fresh instance of `golden` and runs each of its declared
/// `healthcheck` commands inside it, returning one `HealthcheckResult` per
/// command. Always tears down the verification instance before returning,
/// on both the success and failure path.
///
/// `verify_name` is the identifier used for the freshly-created verification
/// instance (distinct from the golden's own `key`, since a verify run may
/// need to be distinguishable from a real, named sandbox in the same
/// backend — e.g. for the demo backend's deterministic vm_tag derivation,
/// or a libvirt backend's domain-name uniqueness).
///
/// ## Why this signature has two more parameters than the unit contract's
/// literal `golden_verify(backend, golden)` listing
/// `Backend::create_from_golden` requires both a `name` and a `pubkey` in
/// its `CreateFromGoldenRequest` (confirmed against the real, merged
/// `lsbx_kernel::backend::Backend` trait — see `build.rs`'s module doc
/// comment for the same issue in the build path). `golden_verify` cannot
/// provision the verification VM at all without them, and generating an
/// ephemeral keypair is out of scope for this crate (Unit 03/Unit 09's
/// job, per this unit's own Boundaries), so both are threaded through as
/// caller-supplied parameters rather than invented internally. This is a
/// deliberate, documented addition to the interface contract's function
/// signature, not an oversight.
pub async fn golden_verify(
    backend: &dyn Backend,
    golden: &GoldenConfig,
    verify_name: &str,
    pubkey: &str,
) -> Result<Vec<HealthcheckResult>, LsbxError> {
    let golden_key = crate::registry::ImageRegistry::validate_key(&golden.key)?;

    // Step 1: provision a fresh instance from the golden under test.
    let created = backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &golden_key,
            name: verify_name,
            pubkey,
            cpu: golden.cpu,
            memory: &golden.memory,
        })
        .await?;
    let vm_tag = created.vm_tag;

    // Step 2: run each declared healthcheck, recording pass/fail per
    // command based on its actual exit code.
    let mut results = Vec::with_capacity(golden.healthcheck.len());
    let mut run_error: Option<LsbxError> = None;

    for command in &golden.healthcheck {
        match backend
            .run(
                &vm_tag,
                &["sh".to_string(), "-c".to_string(), command.clone()],
                HEALTHCHECK_TIMEOUT,
                None,
            )
            .await
        {
            Ok(output) => {
                let passed = output.exit_code == 0;
                let combined = if output.stderr.is_empty() {
                    String::from_utf8_lossy(&output.stdout).to_string()
                } else {
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )
                };
                results.push(HealthcheckResult {
                    command: command.clone(),
                    passed,
                    output: combined,
                });
            }
            Err(e) => {
                // An infrastructure failure mid-verification (e.g. the
                // backend became unavailable partway through) aborts the
                // whole verify run rather than silently marking the
                // remaining healthchecks as "failed" — those checks were
                // never actually attempted, so reporting them as failed
                // would misrepresent what was proven.
                run_error = Some(e);
                break;
            }
        }
    }

    // Step 3: always destroy the verification instance, on both the
    // healthy-completion and infrastructure-failure path, so `golden
    // verify` never leaks a VM.
    let destroy_result = backend.destroy(&vm_tag).await;

    if let Some(e) = run_error {
        return Err(e);
    }
    if let Err(destroy_err) = destroy_result {
        return Err(LsbxError::ContractViolated(format!(
            "golden verify for '{}' ran its healthchecks but failed to destroy \
             verification VM '{}' afterward: {}",
            golden.key, vm_tag, destroy_err
        )));
    }

    Ok(results)
}

// See registry.rs's identically-worded comment above its own test module for
// why this scoped allow exists (Unit 01's crates/lsbx-kernel/tests/test_kernel.rs
// pattern, applied to a #[cfg(test)] mod instead of a separate tests/*.rs file).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::registry::{GoldenFlavor, GoldenMode, StreamingMode};
    use lsbx_backend_demo::DemoBackend;

    fn sample_golden(healthcheck: Vec<String>) -> GoldenConfig {
        GoldenConfig {
            key: "agent-base".to_string(),
            flavor: GoldenFlavor::Agent,
            os: "linux".to_string(),
            base: "lsbx-default-v1".to_string(),
            mode: GoldenMode::Copy,
            cpu: 2,
            memory: "2G".to_string(),
            disk: None,
            streaming: StreamingMode::None,
            capabilities: vec![],
            healthcheck,
            repo: None,
            content_hash: Some("lufs-abcd1234".to_string()),
            description: "test golden".to_string(),
        }
    }

    #[tokio::test]
    async fn verify_runs_each_healthcheck_and_reports_pass_via_demo_backend() {
        let backend = DemoBackend::new();
        // DemoBackend::run always returns exit_code 0, so every declared
        // healthcheck should be reported as passed.
        let golden = sample_golden(vec![
            "test -f /etc/hostname".to_string(),
            "systemctl is-active sshd".to_string(),
        ]);

        let results = golden_verify(&backend, &golden, "verify-agent-base", "ssh-ed25519 AAAA fake")
            .await
            .expect("verify should succeed");

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.passed));
        assert_eq!(results[0].command, "test -f /etc/hostname");
        assert_eq!(results[1].command, "systemctl is-active sshd");

        // Verification instance must be destroyed afterward — no leak.
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    #[tokio::test]
    async fn verify_with_no_healthchecks_returns_empty_and_still_cleans_up() {
        let backend = DemoBackend::new();
        let golden = sample_golden(vec![]);

        let results = golden_verify(&backend, &golden, "verify-agent-base", "ssh-ed25519 AAAA fake")
            .await
            .expect("verify should succeed");

        assert!(results.is_empty());
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }

    #[tokio::test]
    async fn verify_against_unavailable_backend_propagates_error_not_a_failed_healthcheck() {
        let backend = DemoBackend::with_fault(lsbx_backend_demo::FaultMode::Unavailable);
        let golden = sample_golden(vec!["true".to_string()]);

        let result = golden_verify(&backend, &golden, "verify-agent-base", "ssh-ed25519 AAAA fake").await;
        assert!(matches!(result, Err(LsbxError::BackendUnavailable(_))));
    }

    #[tokio::test]
    async fn verify_rejects_golden_with_invalid_key_before_touching_backend() {
        let backend = DemoBackend::new();
        let mut golden = sample_golden(vec!["true".to_string()]);
        golden.key = "Not Valid!".to_string();

        let result = golden_verify(&backend, &golden, "verify-agent-base", "ssh-ed25519 AAAA fake").await;
        assert!(matches!(result, Err(LsbxError::Usage(_))));
        assert!(backend.list_vms().await.expect("list_vms").is_empty());
    }
}
