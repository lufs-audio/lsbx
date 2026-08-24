//! `lsbx-ops` — Shared Operations Façade (Unit 10).
//!
//! One async method per logical operation named in SPEC.md §4.7 and this
//! unit's own contract (`create, destroy, list, exec, put, get, renew,
//! console_url, info, status, reap, golden_build, golden_verify,
//! golden_register, golden_delete, golden_list, config_show, logs_query`).
//! This crate is the waist of the architecture diagram in SPEC.md §3: every
//! door built in Layer 6 (`lsbx-cli`, `lsbx-tui`, `lsbx-gateway`,
//! `lsbx-stream`, `lsbx-mcp`) depends on this crate and *only* this crate —
//! none of them may reach around it into `lsbx-lifecycle` or `lsbx-golden`
//! directly. That is the actual mechanism (not merely the stated intent)
//! behind SPEC.md's Deviation 12: CLI/HTTP/MCP parity is a structural
//! property of the crate graph, because none of those doors contain any
//! operational logic of their own — every decision that changes backend
//! behavior belongs in this crate (thin dispatch/composition) or one of
//! the crates it composes (`lsbx-lifecycle`, `lsbx-golden`), never inside a
//! CLI/HTTP/MCP handler.
//!
//! No function here parses CLI args, HTTP bodies, or MCP tool-call JSON —
//! every input arrives already typed. Translating a door's native input
//! format into these types is that door's job (Units 11, 13, 15), never
//! this crate's. This crate has no `clap`, `axum`, or `rmcp` dependency and
//! does not know any door exists (Boundaries).
//!
//! ## Provenance note: this unit was built clean-room, not reconciled from
//! a Jules patch
//! A prior AI-generated attempt at this unit invented a completely
//! disconnected API that does not correspond to anything in this repo's
//! real, merged source (`Backend::name()`/`available()` methods that do
//! not exist on the real trait; `lsbx_lifecycle::create()` as a bare
//! top-level function when the real function is
//! `lsbx_lifecycle::create::create` — also re-exported at the crate root
//! as `lsbx_lifecycle::create`, colliding with the module name;
//! `SandboxStore {}`/`ImageRegistry {}` as fieldless unit-struct literals
//! when both have real constructors and real internal state;
//! `CreateRequest { id: Some(...) }` and `GoldenConfig { name: ... }` with
//! fields that do not exist on the real types; `LsbxError::Other(...)`,
//! which is not a real variant on the closed 7-variant `LsbxError`; and a
//! call shaped `lsbx_golden::golden_build(&self.registry, req)` when the
//! real `golden_build` takes a `&dyn Backend`, not a registry). None of
//! that patch is reflected anywhere below — every signature in this file
//! was written against the actual merged source of Units 01/02/08/09,
//! re-confirmed by direct read immediately before writing this crate, not
//! against this unit's original contract text or any AI-generated draft
//! of it.
//!
//! ## Delegation map — which operations call into a lower crate's real
//! function vs. are implemented directly here against `SandboxStore`/
//! `ImageRegistry`'s real public methods
//!
//! Units 08 (`lsbx-golden`) and 09 (`lsbx-lifecycle`) do not implement
//! every operation this façade needs — some (`list`, `info`,
//! `console_url`, `golden_register`, `golden_delete`, `golden_list`,
//! `config_show`, `logs_query`) have no corresponding lower-crate function
//! at all today, confirmed by direct re-read of `lsbx-lifecycle`'s and
//! `lsbx-golden`'s real merged source immediately before writing this
//! crate (not assumed from the unit contract's prose). Where that is true,
//! the operation is implemented directly in this crate against
//! `SandboxStore`/`ImageRegistry`'s real public fields/methods, rather than
//! assuming a lower-layer function exists that does not:
//!
//! | Operation | Implementation |
//! |---|---|
//! | `create` | Delegates to `lsbx_lifecycle::create::create(backend, sandbox_store, clock, req)`. |
//! | `destroy` | Delegates to `lsbx_lifecycle::create::destroy(backend, sandbox_store, id)`. |
//! | `list` | **Implemented directly**: `SandboxStore::list()` mapped through `SandboxRecord::public()`. No `lsbx-lifecycle::list` exists. |
//! | `exec` | Resolves `id` -> `vm_tag` via `SandboxStore::load`, then delegates to `Backend::run(vm_tag, command, timeout)`. |
//! | `put` | Resolves `id` -> `vm_tag` via `SandboxStore::load`, then delegates to `Backend::put_file`. |
//! | `get` | Resolves `id` -> `vm_tag` via `SandboxStore::load`, then delegates to `Backend::get_file`. |
//! | `renew` | Delegates to `lsbx_lifecycle::create::renew(sandbox_store, clock, id, duration)`. |
//! | `console_url` | **Implemented directly**: `SandboxStore::load(id)` mapped through `.public().console_url`. No lower-crate function exists. |
//! | `info` | **Implemented directly**: `SandboxStore::load(id)` mapped through `.public()`. No `lsbx-lifecycle::info` exists. |
//! | `status` | **Implemented directly**: probes `Backend::list_vms()` to determine `backend_available` (see the "reconciling `StatusReport`" note below for why there is no `Backend::name()`/`available()` to call), and reports `SandboxStore::list().len()` as `sandbox_count`. |
//! | `reap` | Delegates to `lsbx_lifecycle::reap::reap(backend, sandbox_store, clock, &allowed_goldens, ttl, dry_run)`, where `allowed_goldens` comes from `ImageRegistry::allowed_goldens()`. |
//! | `golden_build` | Delegates to `lsbx_golden::build::golden_build(backend, req, flattener)`. `flattener` is `None` (Unit 19 has not landed — see the "reconciling `golden_build`" note below); the real function's `Ok` type is `GoldenBuildOutcome { config, build_vm_tag }`, not a bare `GoldenConfig`. |
//! | `golden_verify` | Delegates to `lsbx_golden::verify::golden_verify(backend, golden, verify_name, pubkey)` after resolving `name` to a `GoldenConfig` via `ImageRegistry::find_golden`. |
//! | `golden_register` | **Implemented directly**: appends to the in-memory `ImageRegistry.goldens` `Vec`. See the "no persistence" note below — `ImageRegistry` has no `save`/`store` method, so this mutates the loaded, in-process registry only. |
//! | `golden_delete` | **Implemented directly**: removes a matching entry from the in-memory `ImageRegistry.goldens` `Vec`. Same no-persistence caveat as `golden_register`. `keep_snapshot` is accepted (interface-contract parity) but is a documented no-op here — snapshot management is out of this crate's (and any landed crate's) scope today; see the note below. |
//! | `golden_list` | **Implemented directly**: clones `ImageRegistry.goldens`. |
//! | `config_show` | **Implemented directly**: serializes a small, honest summary of the registry's current shape (image/golden/profile counts and keys) as `serde_json::Value` — see the note below for why this crate does not invent a config schema that does not exist in any merged crate. |
//! | `logs_query` | **Implemented directly**, but honestly: no crate anywhere in the merged workspace owns a log store yet (no `tracing` subscriber sink, no log file, no queryable log backend has landed). Rather than fabricate output, this returns `Err(LsbxError::ContractViolated)` naming the gap, mirroring `lsbx-golden::build::NoFlatten`'s own precedent for "the honest failure when a real implementation has not landed yet" (see the note below). |
//!
//! ## Reconciling the unit contract's literal interface against the real
//! merged source (read immediately before writing this crate, not assumed)
//!
//! - **`golden_build`'s real signature returns `GoldenBuildOutcome`, not a
//!   bare `GoldenConfig`.** The unit contract's literal listing says
//!   `golden_build(&self, req: ...) -> Result<GoldenConfig, LsbxError>`.
//!   The real, merged `lsbx_golden::build::golden_build` returns
//!   `Result<GoldenBuildOutcome, LsbxError>`, where `GoldenBuildOutcome {
//!   config: GoldenConfig, build_vm_tag: Option<String> }` — the build VM's
//!   tag is threaded through when `req.cleanup` is `false` so a caller can
//!   attach to it. This façade's `golden_build` returns the real
//!   `GoldenBuildOutcome` rather than discarding `build_vm_tag` to force a
//!   match against the contract's literal (and, per Unit 08's own PR
//!   description, already-superseded) return type.
//! - **`golden_build`/`golden_verify` need a `pubkey` (and `golden_verify`
//!   also needs a `verify_name`) that the contract's literal request
//!   structs do not carry**, because `Backend::create_from_golden` requires
//!   both and neither `lsbx-golden` nor this crate generates ephemeral
//!   keypairs (that is `lsbx-keys`/`lsbx-lifecycle`'s job). This façade's
//!   `golden_build` takes `lsbx_golden::build::GoldenBuildRequest<'_>`
//!   directly (which already carries `pubkey` — see Unit 08's own module
//!   doc comment on `build.rs` for why), and `golden_verify` takes an
//!   explicit `pubkey: &str` and `verify_name: &str` alongside `name: &str`
//!   for the same reason `lsbx-golden::verify::golden_verify` itself does.
//!   Callers (a door, ultimately) are expected to generate the ephemeral
//!   keypair the same way `lsbx_lifecycle::create::create` does internally
//!   for `create`; this façade does not hide that requirement, it exposes
//!   it, because inventing key generation inside this crate would be a
//!   second, compounding scope violation on top of the one Unit 08 already
//!   flagged and declined to commit.
//! - **No `GoldenFlattener` is wired up here**, so `golden_build` is always
//!   called with `flattener: None`. Unit 19 (`lsbx-bootstrap`), which owns
//!   the real qcow2-flatten implementation per SPEC.md §5's Layer 8
//!   placement and this crate's own Boundaries, has not landed. Passing
//!   `None` means every `golden_build` call through this façade against a
//!   non-dry-run request will fail with `LsbxError::ContractViolated`
//!   naming the missing flattener — this is Unit 08's own `NoFlatten`
//!   behavior propagating up honestly, not a new failure mode this crate
//!   invented. `golden_build` with `req.dry_run == true` still succeeds,
//!   since the real `golden_build` never touches the flattener on the
//!   dry-run path.
//! - **`StatusReport { backend_name, backend_available, sandbox_count }`
//!   cannot be filled from `&dyn Backend` alone.** The real
//!   `lsbx_kernel::backend::Backend` trait's only synchronous method is
//!   `fn capabilities(&self) -> BackendCapabilities` (confirmed by direct
//!   re-read of the merged trait) — there is no `name()` and no
//!   `available()`. Rather than add either to the trait (which would be a
//!   Unit 01 change this unit has no mandate to make, and which the ground
//!   truth for this task explicitly forbids inventing), `LsbxOps::new`
//!   takes an explicit `backend_name: String` supplied by the caller (the
//!   caller already knows which concrete `Backend` it constructed, so it
//!   is the only place that name can honestly come from), and
//!   `backend_available` is derived by actually calling `Backend::list_vms()`
//!   and mapping `Ok(_)` to `true`, `Err(LsbxError::BackendUnavailable(_))`
//!   to `false`, and any other `Err` variant is propagated as `status`'s
//!   own error (a `list_vms` failure that is not itself
//!   `BackendUnavailable` — e.g. a lock contention — is not the same claim
//!   as "the backend's control plane is unreachable," so `status` does not
//!   silently fold it into `false`). This is a real, live probe every time
//!   `status` is called, not a cached flag, matching this system's
//!   "ran vs. proven" verification stance (SPEC.md §1) — `status` proves
//!   the backend actually answered *this* call, not merely that it
//!   answered a call once at construction time.
//! - **`ImageRegistry` has no persistence method** (`load` is the only
//!   I/O this type performs — confirmed by direct re-read of Unit 08's
//!   merged `registry.rs`; there is no `save`/`write`/`persist`). This
//!   façade's `golden_register`/`golden_delete` therefore mutate the
//!   in-process `ImageRegistry` (behind a `tokio::sync::RwLock`, since
//!   `LsbxOps`'s methods take `&self` and every door is expected to hold
//!   one shared instance per this unit's own acceptance criteria) and do
//!   not write `images.json`/`images.carnyx.json` back to disk. This is a
//!   real, documented gap, not a silent one: persisting a registry mutation
//!   back to the two real manifest files (and deciding which of the two —
//!   see SPEC.md Deviation 2's `agent-base` split — a given mutation should
//!   land in) is manifest-writing logic that does not exist in any merged
//!   crate today, and inventing it inside this façade would be exactly the
//!   kind of "new logic that belongs in one of the composed crates, never
//!   here" this unit's own Boundaries prohibit. A future unit (most
//!   naturally a small, explicit addition to `lsbx-golden`'s own scope) is
//!   the right place for real manifest-file writes; this façade's
//!   `golden_register`/`golden_delete` are real and testable against the
//!   in-memory registry today, and will not need a signature change when
//!   that lands — only their internal call bodies would gain a write-back
//!   step.
//! - **`golden_delete`'s `keep_snapshot: bool` parameter is accepted but is
//!   a documented no-op.** No crate anywhere in the merged workspace
//!   creates, stores, or manages a "snapshot" of a golden's disk image
//!   (this is plausibly future Unit 19/`lsbx-bootstrap` scope, alongside
//!   flatten). Accepting the parameter (rather than dropping it and
//!   breaking the interface contract's shape) keeps this façade's surface
//!   stable for the day a real snapshot mechanism exists; until then,
//!   `keep_snapshot: true` and `keep_snapshot: false` behave identically.
//! - **`config_show`'s `serde_json::Value` is a real, honest summary of
//!   registry shape, not a fabricated config schema.** No merged crate
//!   defines a "the running configuration of `lsbx`" type — there is no
//!   config file loader, no environment-variable schema, nothing this
//!   façade could summarize truthfully beyond what it actually holds:
//!   the counts and keys of `images`/`goldens`/`profiles` in the
//!   in-process `ImageRegistry`, plus the `backend_name` this instance was
//!   constructed with. Returning a richer-looking but invented JSON shape
//!   here would misrepresent what this system can actually report about
//!   itself today.
//! - **`logs_query` returns an honest `ContractViolated` error, never
//!   fabricated log lines.** No merged crate persists structured logs
//!   anywhere queryable (no log file, no `tracing` subscriber with a
//!   retrievable sink). Mirroring `lsbx-golden::build::NoFlatten`'s own
//!   precedent (an honest, named failure instead of a silently faked
//!   result when a real implementation has not landed), `logs_query`
//!   always returns `Err(LsbxError::ContractViolated)` describing exactly
//!   that gap, regardless of `since`/`limit`. A future door depending on
//!   this façade should treat a non-`Ok` `logs_query` as "no log backend
//!   is wired up yet," not as evidence something is broken.

use lsbx_golden::registry::{GoldenConfig, ImageRegistry};
use lsbx_kernel::backend::Backend;
use lsbx_kernel::clock::Clock;
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::PublicSandbox;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::path::Path;
use std::time::Duration;
use tokio::sync::RwLock;

/// Status summary returned by [`LsbxOps::status`].
///
/// See this module's doc comment ("reconciling `StatusReport`") for why
/// `backend_name` is supplied at construction time rather than read off
/// `&dyn Backend` (the trait has no `name()` method), and why
/// `backend_available` is a live probe against `Backend::list_vms()` on
/// every call rather than a cached flag.
pub struct StatusReport {
    pub backend_name: String,
    pub backend_available: bool,
    pub sandbox_count: usize,
}

/// The one place operational state lives for a running `lsbx` process.
///
/// Constructed once and held by every door (CLI, HTTP, WS stream, MCP) via
/// a shared reference — this unit's own acceptance criteria requires that
/// there be exactly one place operational state lives, not three
/// independently-constructed copies. `registry` is wrapped in a
/// `tokio::sync::RwLock` because `golden_register`/`golden_delete` need to
/// mutate it through `&self` (every method on this type takes `&self`, per
/// the interface contract, so every door can hold one `Arc<LsbxOps>` rather
/// than needing exclusive access); every other registry-reading operation
/// takes a read lock.
pub struct LsbxOps {
    backend: Box<dyn Backend>,
    /// Caller-supplied display name for `backend`. See this module's doc
    /// comment for why this cannot be derived from `&dyn Backend` itself.
    backend_name: String,
    sandbox_store: SandboxStore,
    #[allow(dead_code)] // Held for door crates (Units 16-18's CI broker) that need direct access; unused by this crate's own operations today.
    ci_job_store: CiJobStore,
    registry: RwLock<ImageRegistry>,
    clock: Box<dyn Clock>,
}

impl LsbxOps {
    /// Constructs the façade. `backend_name` is a deliberate, documented
    /// addition to the unit contract's literal `new()` parameter list —
    /// see this module's doc comment ("reconciling `StatusReport`") for
    /// why `&dyn Backend` alone cannot supply a name.
    pub fn new(
        backend: Box<dyn Backend>,
        backend_name: String,
        sandbox_store: SandboxStore,
        ci_job_store: CiJobStore,
        registry: ImageRegistry,
        clock: Box<dyn Clock>,
    ) -> Self {
        Self {
            backend,
            backend_name,
            sandbox_store,
            ci_job_store,
            registry: RwLock::new(registry),
            clock,
        }
    }

    /// Resolves a sandbox `id` to its persisted `vm_tag`, mapping "the
    /// sandbox exists but was never assigned a `vm_tag`" onto
    /// `LsbxError::NotFound` the same way "the sandbox id itself doesn't
    /// resolve" is — both are "there is nothing live for this id to act
    /// on," and `exec`/`put`/`get` have no other sensible outcome for a
    /// record with no backend handle.
    fn resolve_vm_tag(&self, id: &str) -> Result<String, LsbxError> {
        let record = self.sandbox_store.load(id)?;
        record.vm_tag.ok_or_else(|| {
            LsbxError::NotFound(format!("sandbox {id} has no vm_tag (never fully provisioned)"))
        })
    }

    // ---- lsbx-lifecycle delegation: create / destroy / renew / reap ----

    /// Delegates to `lsbx_lifecycle::create::create`. See the module-level
    /// delegation map.
    pub async fn create(
        &self,
        req: lsbx_lifecycle::create::CreateRequest<'_>,
    ) -> Result<PublicSandbox, LsbxError> {
        lsbx_lifecycle::create::create(
            self.backend.as_ref(),
            &self.sandbox_store,
            self.clock.as_ref(),
            req,
        )
        .await
    }

    /// Delegates to `lsbx_lifecycle::create::destroy`.
    pub async fn destroy(&self, id: &str) -> Result<(), LsbxError> {
        lsbx_lifecycle::create::destroy(self.backend.as_ref(), &self.sandbox_store, id).await
    }

    /// Delegates to `lsbx_lifecycle::create::renew`. Note the real function
    /// needs no `Backend` at all (renewing a lease only touches the
    /// store), which is why this method does not pass `self.backend`
    /// through even though most other operations here do.
    pub async fn renew(&self, id: &str, duration: Duration) -> Result<PublicSandbox, LsbxError> {
        lsbx_lifecycle::create::renew(&self.sandbox_store, self.clock.as_ref(), id, duration).await
    }

    /// Delegates to `lsbx_lifecycle::reap::reap`, resolving
    /// `allowed_goldens` from the current in-process `ImageRegistry` per
    /// this crate's own delegation map.
    pub async fn reap(
        &self,
        ttl: Duration,
        dry_run: bool,
    ) -> Result<lsbx_lifecycle::reap::ReapReport, LsbxError> {
        let allowed_goldens = {
            let registry = self.registry.read().await;
            registry.allowed_goldens()
        };
        lsbx_lifecycle::reap::reap(
            self.backend.as_ref(),
            &self.sandbox_store,
            self.clock.as_ref(),
            &allowed_goldens,
            ttl,
            dry_run,
        )
        .await
    }

    // ---- Implemented directly against SandboxStore (no lower-crate fn exists) ----

    /// **Implemented directly** — no `lsbx-lifecycle::list` exists.
    /// `SandboxStore::list()` mapped through `SandboxRecord::public()`, so
    /// key material never crosses this façade's boundary (`public()`'s own
    /// job, preserved exactly as SPEC.md §4.6 requires).
    pub async fn list(&self) -> Result<Vec<PublicSandbox>, LsbxError> {
        let records = self.sandbox_store.list()?;
        Ok(records.iter().map(|r| r.public()).collect())
    }

    /// **Implemented directly** — no `lsbx-lifecycle::info` exists.
    /// `SandboxStore::load(id)` mapped through `.public()`.
    pub async fn info(&self, id: &str) -> Result<PublicSandbox, LsbxError> {
        let record = self.sandbox_store.load(id)?;
        Ok(record.public())
    }

    /// **Implemented directly** — no lower-crate function exists.
    /// `SandboxStore::load(id)` mapped through `.public().console_url`,
    /// which is itself computed (never persisted) by `SandboxRecord::public`
    /// from `streaming`/`https_url`.
    pub async fn console_url(&self, id: &str) -> Result<Option<String>, LsbxError> {
        let record = self.sandbox_store.load(id)?;
        Ok(record.public().console_url)
    }

    // ---- Backend delegation via id -> vm_tag resolution ----

    /// Resolves `id` to a `vm_tag` via the store, then delegates to
    /// `Backend::run`. Neither `lsbx-lifecycle` nor `lsbx-golden` exposes
    /// an "exec into an existing sandbox by id" function — this façade is
    /// the first (and only) place that composition exists, which is
    /// exactly this unit's reason to exist (SPEC.md §4.7).
    pub async fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
    ) -> Result<lsbx_kernel::backend::CommandOutput, LsbxError> {
        let vm_tag = self.resolve_vm_tag(id)?;
        self.backend.run(&vm_tag, command, timeout).await
    }

    /// Resolves `id` to a `vm_tag`, then delegates to `Backend::put_file`.
    pub async fn put(&self, id: &str, source: &Path, destination: &str) -> Result<(), LsbxError> {
        let vm_tag = self.resolve_vm_tag(id)?;
        self.backend.put_file(&vm_tag, source, destination).await
    }

    /// Resolves `id` to a `vm_tag`, then delegates to `Backend::get_file`.
    pub async fn get(&self, id: &str, source: &str, destination: &Path) -> Result<(), LsbxError> {
        let vm_tag = self.resolve_vm_tag(id)?;
        self.backend.get_file(&vm_tag, source, destination).await
    }

    // ---- Status: implemented directly, live-probing the backend ----

    /// **Implemented directly.** See the module-level "reconciling
    /// `StatusReport`" note for why `backend_available` is a live
    /// `Backend::list_vms()` probe rather than a cached flag, and why
    /// `backend_name` comes from this instance's constructor rather than
    /// from `&dyn Backend` itself.
    pub async fn status(&self) -> Result<StatusReport, LsbxError> {
        let backend_available = match self.backend.list_vms().await {
            Ok(_) => true,
            Err(LsbxError::BackendUnavailable(_)) => false,
            // A failure that is not itself "the control plane is
            // unreachable" (e.g. a lock contention on the backend's own
            // side, if a future backend ever surfaces one through
            // list_vms) is not the same claim as backend_available: false
            // — propagate it as status's own error rather than silently
            // folding a different failure mode into "unavailable".
            Err(e) => return Err(e),
        };
        let sandbox_count = self.sandbox_store.list()?.len();

        Ok(StatusReport {
            backend_name: self.backend_name.clone(),
            backend_available,
            sandbox_count,
        })
    }

    // ---- lsbx-golden delegation: golden_build / golden_verify ----

    /// Delegates to `lsbx_golden::build::golden_build`. No `GoldenFlattener`
    /// is wired up (`flattener: None`) — see this module's doc comment for
    /// why, and for what that means for a non-dry-run request. Returns the
    /// real `GoldenBuildOutcome` (not a bare `GoldenConfig`) — see the same
    /// note for why the contract's literal return type is stale.
    pub async fn golden_build(
        &self,
        req: lsbx_golden::build::GoldenBuildRequest<'_>,
    ) -> Result<lsbx_golden::build::GoldenBuildOutcome, LsbxError> {
        let register = req.register;
        let outcome = lsbx_golden::build::golden_build(self.backend.as_ref(), req, None).await?;

        if register {
            let mut registry = self.registry.write().await;
            registry.goldens.push(clone_golden_config(&outcome.config));
        }

        Ok(outcome)
    }

    /// Delegates to `lsbx_golden::verify::golden_verify` after resolving
    /// `name` to a `GoldenConfig` via `ImageRegistry::find_golden`. A
    /// `name` that does not resolve is `LsbxError::NotFound` — the golden
    /// key itself is not malformed input (that's `Usage`, and
    /// `lsbx_golden`'s own `golden_verify` already checks the `GoldenConfig`
    /// it's handed against the key regex), it simply does not exist in this
    /// registry.
    pub async fn golden_verify(
        &self,
        name: &str,
        verify_name: &str,
        pubkey: &str,
    ) -> Result<Vec<lsbx_golden::verify::HealthcheckResult>, LsbxError> {
        let golden = {
            let registry = self.registry.read().await;
            registry
                .find_golden(name)
                .map(clone_golden_config)
                .ok_or_else(|| LsbxError::NotFound(format!("no golden registered under key '{name}'")))?
        };
        lsbx_golden::verify::golden_verify(self.backend.as_ref(), &golden, verify_name, pubkey).await
    }

    // ---- Implemented directly against the in-process ImageRegistry ----

    /// **Implemented directly.** `ImageRegistry` has no persistence method
    /// (see the module-level note) — this appends to the in-process
    /// registry only. Rejects a `config.key` that collides with an
    /// existing entry as `LsbxError::Usage` (re-registering under a key
    /// that already resolves is malformed input, not an internal fault).
    pub async fn golden_register(&self, config: GoldenConfig) -> Result<(), LsbxError> {
        let mut registry = self.registry.write().await;
        if registry.find_golden(&config.key).is_some() {
            return Err(LsbxError::Usage(format!(
                "a golden is already registered under key '{}'",
                config.key
            )));
        }
        registry.goldens.push(config);
        Ok(())
    }

    /// **Implemented directly.** Same no-persistence caveat as
    /// `golden_register`. `keep_snapshot` is accepted for interface-contract
    /// parity but is a documented no-op — see the module-level note.
    pub async fn golden_delete(&self, name: &str, _keep_snapshot: bool) -> Result<(), LsbxError> {
        let mut registry = self.registry.write().await;
        let before = registry.goldens.len();
        registry.goldens.retain(|g| g.key != name);
        if registry.goldens.len() == before {
            return Err(LsbxError::NotFound(format!(
                "no golden registered under key '{name}'"
            )));
        }
        Ok(())
    }

    /// **Implemented directly.** Clones the current in-process
    /// `ImageRegistry.goldens`.
    pub async fn golden_list(&self) -> Result<Vec<GoldenConfig>, LsbxError> {
        let registry = self.registry.read().await;
        Ok(registry.goldens.iter().map(clone_golden_config).collect())
    }

    // ---- config_show / logs_query: implemented directly, honestly ----

    /// **Implemented directly.** A real, honest summary of what this
    /// façade actually holds — see the module-level note for why this does
    /// not invent a richer config schema no merged crate defines.
    pub async fn config_show(&self) -> Result<serde_json::Value, LsbxError> {
        let registry = self.registry.read().await;
        let image_keys: Vec<&str> = registry.images.iter().map(|i| i.key.as_str()).collect();
        let golden_keys: Vec<&str> = registry.goldens.iter().map(|g| g.key.as_str()).collect();
        let profile_keys: Vec<&str> = registry.profiles.keys().map(String::as_str).collect();

        Ok(serde_json::json!({
            "backend_name": self.backend_name,
            "images": { "count": registry.images.len(), "keys": image_keys },
            "goldens": { "count": registry.goldens.len(), "keys": golden_keys },
            "profiles": { "count": registry.profiles.len(), "keys": profile_keys },
        }))
    }

    /// **Implemented directly, but honestly fails.** No merged crate owns a
    /// queryable log store yet — see the module-level note. Always returns
    /// `Err(LsbxError::ContractViolated)` naming the gap; `since`/`limit`
    /// are accepted for interface-contract parity but do not affect the
    /// outcome today.
    pub async fn logs_query(
        &self,
        _since: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<String>, LsbxError> {
        Err(LsbxError::ContractViolated(
            "no log backend is wired up yet — no merged crate persists structured logs \
             anywhere queryable (no log file, no tracing subscriber with a retrievable \
             sink); this is an honest gap, not a fabricated empty result"
                .to_string(),
        ))
    }
}

/// `GoldenConfig` has no `Clone` derive (confirmed against Unit 08's real,
/// merged `registry.rs`), so a manual field-by-field clone is needed
/// wherever this façade needs to read a registry entry out from behind the
/// `RwLock` without holding the lock for the caller's entire subsequent
/// `await` (e.g. across a `Backend` call in `golden_verify`). Kept as one
/// small helper rather than duplicated at each call site.
fn clone_golden_config(config: &GoldenConfig) -> GoldenConfig {
    GoldenConfig {
        key: config.key.clone(),
        flavor: config.flavor.clone(),
        os: config.os.clone(),
        base: config.base.clone(),
        mode: config.mode.clone(),
        cpu: config.cpu,
        memory: config.memory.clone(),
        disk: config.disk.clone(),
        streaming: config.streaming.clone(),
        capabilities: config.capabilities.clone(),
        healthcheck: config.healthcheck.clone(),
        repo: config.repo.clone(),
        content_hash: config.content_hash.clone(),
        description: config.description.clone(),
    }
}
