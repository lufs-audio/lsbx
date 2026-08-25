//! `create` / `destroy` / `renew` (Unit 09).
//!
//! The state machine at the center of the system: generate a keypair, call
//! into a `Backend`, persist durably, then — unless the caller opted out —
//! prove readiness before handing anything back. "Ran" (a backend call
//! returned `Ok`) and "proven" (the golden's declared healthchecks actually
//! passed) are never conflated here; see [`create`]'s doc comment.

use lsbx_kernel::backend::{Backend, CreateFromGoldenRequest};
use lsbx_kernel::clock::Clock;
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::{GoldenKey, PublicSandbox, SandboxRecord};
use lsbx_store::sandbox_store::SandboxStore;
use std::path::Path;
use std::time::Duration;

/// Request to provision a new sandbox.
///
/// `healthchecks` is this crate's own stand-in for "the golden's declared
/// healthchecks" (see the module-level note in `lib.rs` on why this crate
/// does not depend on `lsbx-golden`): a list of commands run inside the VM
/// via `Backend::run`. An empty list means "no healthchecks are declared for
/// this golden" — readiness is then proven by the weaker but still-real
/// signal of the VM accepting and completing a single trivial command
/// (`Backend::run` succeeding at all), never by `create_from_golden`
/// returning `Ok` alone.
pub struct CreateRequest<'a> {
    pub profile: &'a str,
    pub name: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub lease: Duration,
    pub ready_timeout: Duration,
    pub verify: bool, // false when --no-verify
    /// Optional healthcheck commands to run via `Backend::run` while
    /// polling readiness. See the struct-level doc comment above; this
    /// field exists only because this unit does not depend on
    /// `lsbx-golden` (Unit 08), which is where a real golden's declared
    /// healthchecks actually live.
    pub healthchecks: Vec<Vec<String>>,
}

/// How long to sleep between readiness polls. Short enough that
/// `ready_timeout`s used in tests (milliseconds) still get at least one
/// retry; not so short that a real poll loop busy-spins.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

fn golden_key_for_profile(profile: &str) -> GoldenKey {
    // This unit does not parse the golden registry (Unit 08's job) — it
    // only needs *some* `GoldenKey` to pass through `Backend::create_from_golden`.
    // Resolving a profile name to its actual golden is Unit 08's
    // `ImageRegistry`/`Profile` responsibility and, per this unit's own
    // Boundaries, out of scope here; until `lsbx-ops` (Unit 10) wires that
    // resolution in, the profile string is used directly as the golden key.
    GoldenKey::new_unchecked(profile.to_string())
}

fn rfc3339_now_plus(clock: &dyn Clock, duration: Duration) -> String {
    let now: chrono::DateTime<chrono::Utc> = clock.now().into();
    (now + duration).to_rfc3339()
}

fn rfc3339_now(clock: &dyn Clock) -> String {
    let now: chrono::DateTime<chrono::Utc> = clock.now().into();
    now.to_rfc3339()
}

/// Runs every command in `healthchecks` against `vm_tag` via
/// `Backend::run`, treating a nonzero exit code the same as a
/// `Backend::run` `Err` — both mean "not ready yet." Returns `true` only if
/// every command ran and exited 0. An empty `healthchecks` list is handled
/// by the caller ([`poll_ready`]), not here.
///
/// `per_call_timeout` is passed straight through as the `timeout` argument
/// to `Backend::run` (data the backend may use to bound its own internal
/// wait), but is *not* what actually bounds this function's own wall-clock
/// duration — a backend is free to ignore that hint entirely (a demo/mock
/// backend under a hang fault is exactly this case). The real enforcement
/// of "this attempt must not run longer than what's left of the caller's
/// overall deadline" happens in [`poll_ready`], which races this whole
/// future against the remaining budget via `tokio::time::timeout` — that
/// is what actually catches a `Backend::run` call that never returns in
/// time, independent of whatever the backend itself decided to do with
/// the timeout value it was handed.
async fn healthchecks_pass(
    backend: &dyn Backend,
    vm_tag: &str,
    healthchecks: &[Vec<String>],
    per_call_timeout: Duration,
    identity_file: Option<&Path>,
) -> bool {
    for command in healthchecks {
        match backend
            .run(vm_tag, command, per_call_timeout, identity_file)
            .await
        {
            Ok(output) if output.exit_code == 0 => continue,
            _ => return false,
        }
    }
    true
}

/// Polls readiness up to `ready_timeout`, returning `Ok(())` once proven
/// ready or `Err(LsbxError::ContractViolated)` once the timeout elapses
/// without proof.
///
/// "Proven ready" means:
/// - `healthchecks` is non-empty: every command in it has run via
///   `Backend::run` and exited 0 (see [`healthchecks_pass`]).
/// - `healthchecks` is empty (no golden healthchecks declared): a single
///   trivial `Backend::run` call against the VM has completed successfully.
///   This is deliberately weaker than a real healthcheck, but it is still a
///   real signal proven via `Backend::run` — never merely "the earlier
///   `create_from_golden` call returned `Ok`," which is exactly the
///   "ran vs. proven" gap this unit exists to close.
///
/// The overall `ready_timeout` budget is enforced with
/// `tokio::time::timeout` wrapped around each attempt at
/// [`healthchecks_pass`], using whatever time remains until the original
/// deadline. This matters for a backend whose `Backend::run` can hang past
/// whatever timeout value it was handed (the `timeout` argument is data the
/// backend receives, not a guarantee this caller can enforce from the
/// outside) — without racing the attempt itself against the remaining
/// budget, a single hung call could silently outlast `ready_timeout` and
/// still eventually return `Ok`, defeating the whole point of a bounded
/// readiness check.
async fn poll_ready(
    backend: &dyn Backend,
    vm_tag: &str,
    healthchecks: &[Vec<String>],
    ready_timeout: Duration,
    identity_file: Option<&Path>,
) -> Result<(), LsbxError> {
    let deadline = std::time::Instant::now() + ready_timeout;
    let probe: Vec<Vec<String>> = if healthchecks.is_empty() {
        vec![vec!["true".to_string()]]
    } else {
        healthchecks.to_vec()
    };

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(LsbxError::ContractViolated(format!(
                "sandbox {vm_tag} did not become ready within {ready_timeout:?} (healthchecks did not pass)"
            )));
        }

        let attempt = tokio::time::timeout(
            remaining,
            healthchecks_pass(backend, vm_tag, &probe, remaining, identity_file),
        )
        .await;

        // `Ok(true)` (healthchecks actually passed) is the only attempt
        // outcome that ends the loop early. `Ok(false)` (healthchecks ran
        // and failed) and `Err(_)` (the attempt itself timed out against
        // the remaining budget) both mean "not proven ready yet" and fall
        // through to the deadline re-check below, which returns the
        // ContractViolated error once the budget is actually exhausted.
        if let Ok(true) = attempt {
            return Ok(());
        }

        let remaining_after_attempt = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining_after_attempt.is_zero() {
            return Err(LsbxError::ContractViolated(format!(
                "sandbox {vm_tag} did not become ready within {ready_timeout:?} (healthchecks did not pass)"
            )));
        }

        tokio::time::sleep(READINESS_POLL_INTERVAL.min(remaining_after_attempt)).await;
    }
}

/// Provisions a new sandbox end to end:
///
/// 1. Generates an ephemeral Ed25519 keypair (Unit 03's
///    `generate_ephemeral_keypair`), labeled with the sandbox id.
/// 2. Calls `Backend::create_from_golden` with that keypair's public half.
/// 3. Builds a `SandboxRecord` and calls `SandboxStore::save` — **before**
///    returning to the caller. This is durability-before-ack: once `create`
///    has returned `Ok`, the record is already on disk, so a crash the
///    instant after `create` returns can never lose track of a VM that was
///    actually provisioned.
/// 4. Unless `req.verify` is `false` (`--no-verify`), polls readiness up to
///    `req.ready_timeout` (see [`poll_ready`]) before returning.
///
/// If `create_from_golden` itself fails, the freshly generated keypair is
/// cleaned up (nothing was persisted yet, so there is nothing else to roll
/// back) and the backend's error is returned.
///
/// If step 4's readiness proof fails (times out, or a healthcheck never
/// passes), `create` returns `Err(LsbxError::ContractViolated)` — but the
/// `SandboxRecord` from step 3 is **not** rolled back or deleted. The VM
/// was actually created and its record is already durable (step 3 ran
/// first); deleting it here would mean either leaking the real VM (a
/// `destroy` was never attempted) or attempting a `destroy` inside an
/// already-failing call, compounding a diagnosable "created but never
/// proven ready" state into a much harder-to-diagnose partial-failure
/// state. The record's presence with `console_url`/health unproven is
/// itself the diagnosable signal; a caller (Unit 10's `lsbx-ops::create`,
/// ultimately the CLI/HTTP/MCP doors) can inspect it via `info`/`list` and
/// decide whether to retry readiness or call `destroy` explicitly.
pub async fn create(
    backend: &dyn Backend,
    store: &SandboxStore,
    clock: &dyn Clock,
    req: CreateRequest<'_>,
) -> Result<PublicSandbox, LsbxError> {
    let id = uuid_like_id(clock);
    let name = req.name.unwrap_or(id.as_str()).to_string();

    let keypair = lsbx_keys::keygen::generate_ephemeral_keypair(&id)?;

    let golden = golden_key_for_profile(req.profile);
    let create_result = backend
        .create_from_golden(CreateFromGoldenRequest {
            golden: &golden,
            name: &name,
            pubkey: &keypair.public_key_line,
            cpu: 1,
            memory: "1G",
        })
        .await;

    let created_vm = match create_result {
        Ok(vm) => vm,
        Err(e) => {
            // Nothing was persisted yet — only the keypair needs cleanup.
            let _ = lsbx_keys::keygen::cleanup_keypair(&keypair);
            return Err(e);
        }
    };

    let created_at = rfc3339_now(clock);
    let lease_expires_at = rfc3339_now_plus(clock, req.lease);

    let record = SandboxRecord {
        id: id.clone(),
        name: name.clone(),
        host: created_vm.host.clone(),
        profile: req.profile.to_string(),
        flavor: "default".to_string(),
        streaming: if created_vm.https_url.is_some() {
            "novnc".to_string()
        } else {
            "none".to_string()
        },
        username: None,
        key_name: Some(keypair.label.clone()),
        key_path: Some(keypair.private_key_path.to_string_lossy().into_owned()),
        key_dir: keypair
            .private_key_path
            .parent()
            .map(|p| p.to_string_lossy().into_owned()),
        pubkey: Some(keypair.public_key_line.clone()),
        task_id: req.task_id.map(str::to_string),
        created_at: Some(created_at),
        lease_expires_at: Some(lease_expires_at),
        vm_tag: Some(created_vm.vm_tag.clone()),
        https_url: created_vm.https_url.clone(),
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    };

    // Durability-before-ack: this save must complete before `create`
    // returns anything to the caller, success or (readiness) failure alike.
    store.save(&record)?;

    if req.verify {
        poll_ready(
            backend,
            &created_vm.vm_tag,
            &req.healthchecks,
            req.ready_timeout,
            Some(&keypair.private_key_path),
        )
        .await?;
    }

    Ok(record.public())
}

/// Generates a sandbox id. Not a cryptographically-relevant identifier —
/// just needs to be unique enough not to collide within a single store, so
/// a clock-derived, monotonically-non-decreasing-in-practice value plus a
/// short random suffix is sufficient. Deliberately not `uuid` (not a
/// workspace dependency for this crate; adding one for an id format the
/// interface contract doesn't specify would be scope creep this unit's
/// Boundaries don't call for).
fn uuid_like_id(clock: &dyn Clock) -> String {
    let now: chrono::DateTime<chrono::Utc> = clock.now().into();
    let nanos = now.timestamp_nanos_opt().unwrap_or(0);
    let suffix: u32 = rand::random();
    format!("sbx-{nanos:x}-{suffix:08x}")
}

/// Destroys a sandbox in the exact order the interface contract specifies:
/// `Backend::destroy`, then `cleanup_keypair`, then `SandboxStore::delete`.
///
/// This order is load-bearing, not incidental: if `Backend::destroy` fails,
/// `destroy` returns immediately (propagating the backend's error) without
/// touching the keypair or the store record — the VM still exists, so both
/// must remain intact for a retry to have something to act on and for an
/// operator to diagnose the failure against a store record that still
/// accurately reflects "this VM is still live." If `Backend::destroy`
/// succeeds but `cleanup_keypair` fails, `destroy` again returns before
/// touching the store — the record survives so the caller isn't left unable
/// to find out an ephemeral key directory needs manual attention. Only once
/// both the backend call and the keypair cleanup have both succeeded does
/// the record get deleted, which is also why this function loads the record
/// first: it needs the record's `key_path`/`key_dir` before it has any
/// grounds to delete anything.
pub async fn destroy(
    backend: &dyn Backend,
    store: &SandboxStore,
    id: &str,
) -> Result<(), LsbxError> {
    let record = store.load(id)?;

    if let Some(vm_tag) = record.vm_tag.as_deref() {
        backend.destroy(vm_tag).await?;
    }

    if let Some(key_path) = record.key_path.clone() {
        let keypair = lsbx_keys::keygen::EphemeralKeypair {
            private_key_path: std::path::PathBuf::from(key_path),
            public_key_line: record.pubkey.clone().unwrap_or_default(),
            label: record.key_name.clone().unwrap_or_default(),
        };
        lsbx_keys::keygen::cleanup_keypair(&keypair)?;
    }

    store.delete(id)
}

/// Extends `lease_expires_at` to `clock.now() + duration` and persists the
/// update. Refuses to renew a sandbox with `cleanup_failed: true` — a
/// sandbox already known to be in a broken cleanup state must not have its
/// life extended, matching the existing system's safety property (see the
/// interface contract).
pub async fn renew(
    store: &SandboxStore,
    clock: &dyn Clock,
    id: &str,
    duration: Duration,
) -> Result<PublicSandbox, LsbxError> {
    let mut record = store.load(id)?;

    if record.cleanup_failed {
        return Err(LsbxError::ContractViolated(format!(
            "refusing to renew sandbox {id}: cleanup_failed is set"
        )));
    }

    record.lease_expires_at = Some(rfc3339_now_plus(clock, duration));
    store.save(&record)?;

    Ok(record.public())
}
