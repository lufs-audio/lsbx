//! `lsbx-cli` — CLI Surface & Output Formatting (Unit 11).
//!
//! `src/main.rs`/`src/bin/lsbx.rs` is a thin binary shim; every real
//! behavior lives here so it is unit-testable without spawning a process
//! (`tests/test_backend_auto_probe.rs` still also spawns the real compiled
//! binary for an end-to-end check, per this unit's own Verification
//! section, but the logic itself does not require that to be exercised).
//!
//! ## What this crate replaces
//! Two prior Jules candidates for this unit both got the `Cli`/`Command`/
//! `GoldenCommand` clap struct definitions right (see `cli.rs` — used
//! verbatim, matching the unit contract's exact flag surface), but neither
//! one actually dispatched anything: both `main()`/`run()` implementations
//! printed a canned "success" message regardless of which subcommand was
//! parsed, never constructed a real `LsbxOps`, never called any of its
//! methods, and never matched on `args.command` at all. Everything in
//! [`run`] below — backend construction (including the real `--backend
//! auto` probe order), the other `LsbxOps` dependencies, and the actual
//! `match` translating every parsed subcommand into a typed `LsbxOps`
//! call — is what was missing and is this unit's real contribution.
//!
//! ## Backend construction
//! `--backend demo|libvirt|exedev|auto` is resolved by [`build_backend`].
//! `auto` probes `libvirt` then `exedev` then `demo`, matching the unit
//! contract's acceptance criterion and the existing system's documented
//! fallback order (SPEC.md §4.4). "Probing" `libvirt`/`exedev` means:
//! attempt to construct a real backend and confirm its control plane
//! actually answers (`Backend::list_vms()`), catching any failure and
//! falling through to the next candidate — never assuming a backend is
//! live just because construction itself didn't panic (`ExedevBackend::new`
//! is not fallible even when its auth is completely unconfigured, so
//! construction succeeding proves nothing on its own).
//!
//! ## Dispatch
//! [`run`] parses `Cli`, builds a real backend and the other `LsbxOps`
//! dependencies (`SandboxStore`, `CiJobStore`, `ImageRegistry`, a real
//! `SystemClock`) from the parsed `--state-dir`/`--images`/`--config`
//! flags (sensible defaults if unset — see [`resolve_state_dir`],
//! [`resolve_images_path`]), builds the real `LsbxOps`, then matches on
//! `args.command` and calls the corresponding method, translating CLI args
//! into the real typed request structs `lsbx-lifecycle`/`lsbx-golden`
//! actually declare (`lsbx_lifecycle::create::CreateRequest`,
//! `lsbx_golden::build::GoldenBuildRequest`, etc.) — never a guessed shape.
//!
//! `golden build`/`golden verify`/`golden register` need an ephemeral
//! keypair's public half (`Backend::create_from_golden` requires one, and
//! neither `lsbx-golden` nor `lsbx-ops` generates one internally for those
//! two façade methods — see `lsbx-ops`'s own module doc comment). This
//! crate generates one via `lsbx_keys::keygen::generate_ephemeral_keypair`
//! the same way `lsbx_lifecycle::create::create` already does internally
//! for `up`, and cleans it up afterward — a door-level responsibility
//! `lsbx-ops`'s PR description explicitly calls out as still open.
//!
//! Every result is routed through `format::render_result`/`format::render`
//! (never a second, independently-maintained rendering path per
//! subcommand), and `main`'s only remaining job is
//! `std::process::exit(error.exit_code() as i32)` on failure.
//!
//! ## Bare invocation / TUI handoff
//! A bare `lsbx` invocation (no subcommand) is supposed to delegate to
//! `lsbx-tui`'s dashboard when stdout is a TTY (Unit 12, built in parallel
//! with this one). `lsbx-tui` is not wired in as a dependency of this
//! crate — this crate does not add a dependency on a door outside this
//! pass's scope, so the bare-invocation path below calls `status` directly
//! and renders it through the same one formatting path every other
//! subcommand uses, regardless of TTY-ness, with a `// TODO` marking
//! exactly where the real handoff belongs once that wiring is taken up.
//!
//! ## Gap 1/3 (final integration wiring pass): `Serve`, `Bootstrap`, `Mcp`,
//! `CiBroker` are now real, not `ContractViolated` stubs
//!
//! As merged, `dispatch`'s `Serve`/`Bootstrap`/`Mcp` arms were honest
//! `ContractViolated` stubs naming the crate that didn't exist yet at the
//! time this unit was built (`lsbx-gateway`/`lsbx-bootstrap`/`lsbx-mcp`, all
//! unmerged Layer 6/8 crates as of PR #17). All three crates are now
//! merged, and this pass wires each stub to its real implementation. A
//! fourth subcommand, `CiBroker`, did not exist at all — the systemd unit
//! files `lsbx-bootstrap::systemd::generate_broker_units` generates
//! reference `lsbx ci-broker run --backend=<...>` as their `ExecStart`, a
//! subcommand this pass adds for the first time (see `cli.rs`'s
//! `CiBrokerCommand` and `dispatch_ci_broker` below).
//!
//! - **`Bootstrap`** calls the real `lsbx_bootstrap::systemd::bootstrap`
//!   with a real `BootstrapConfig` built from the parsed flags, and renders
//!   the resulting `BootstrapReport` through this crate's one formatting
//!   path via a new `BootstrapReportDto`/`Formattable` impl (see below) —
//!   `BootstrapReport` itself derives no `Serialize`/`Formattable`,
//!   matching this crate's existing convention of a small local DTO for
//!   every non-`Serialize` `LsbxOps`/sibling-crate response type it
//!   touches (`StatusReportDto`, `ReapReportDto`, etc., already below).
//! - **`Mcp`** builds the exact same `LsbxOps` this crate's own
//!   `build_deps()` already builds for every other subcommand (no separate
//!   construction path), wraps it in the `Arc` `lsbx_mcp::run_stdio_server`
//!   requires, and calls it — blocking on stdio for the rest of the
//!   process's life, which is correct MCP server behavior (the CLI itself
//!   becomes the MCP server process once this subcommand runs).
//! - **`Serve`** builds `lsbx_gateway::GatewayDeps { ops, state_dir }` from
//!   the same `LsbxOps`/`state_dir` `build_deps()` already resolved, and
//!   implements the documented design: a single merged gateway+stream
//!   listener by default (`stream_port` unset or equal to `port`), or two
//!   independent listeners (gateway-only on `port`, stream-only on
//!   `stream_port`) when `stream_port` is explicitly set to a different
//!   value. Either way, a background reap loop runs alongside serving. See
//!   `dispatch_serve`'s own doc comment for the full design writeup,
//!   including the `--daemon` semantics (deliberately NOT real Unix
//!   double-fork daemonization).
//! - **`CiBroker { action: CiBrokerCommand::Run { backend, queue_label } }`**
//!   builds a real `LsbxOps`/`CiJobStore`/`GitHubClient` the same way
//!   `build_deps()` already does for every other command, resolves GitHub
//!   App credentials from the `LSBX_GITHUB_APP_*` environment variables
//!   this pass documents in root `AGENTS.md` (falling back to
//!   `GitHubClient::from_gh_cli_fallback()` when they aren't set, exactly
//!   as `lsbx-broker` itself already supports), and calls the real
//!   `lsbx_broker::reconcile::run_broker(..., iterations: None)` — runs
//!   forever, exactly like the systemd units this closes the loop for
//!   expect. See `dispatch_ci_broker`'s own doc comment.

pub mod cli;
pub mod format;

use clap::Parser as _;
use cli::{BackendChoice, CiBrokerCommand, Cli, Command, GoldenCommand};
use format::Formattable;
use lsbx_backend_demo::DemoBackend;
use lsbx_backend_exedev::{ExedevAuth, ExedevBackend};
use lsbx_backend_libvirt::LibvirtBackend;
use lsbx_golden::registry::{GoldenConfig, GoldenFlavor, GoldenMode, ImageRegistry, StreamingMode};
use lsbx_kernel::backend::Backend;
use lsbx_kernel::clock::{Clock, SystemClock};
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::PublicSandbox;
use lsbx_ops::{LsbxOps, StatusReport};
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------

/// Parses argv, builds the real `LsbxOps`, dispatches the parsed
/// subcommand, prints the rendered result, and returns the process exit
/// code. `main`'s only remaining job is calling
/// `std::process::exit(run().await)`.
pub async fn run() -> i32 {
    let args = Cli::parse_from(std::env::args_os());
    run_with_args(args).await
}

/// Same as [`run`], but takes an already-parsed [`Cli`] — the seam that
/// makes dispatch testable without spawning a process (used by this
/// crate's own unit tests; the end-to-end `tests/test_backend_auto_probe.rs`
/// scenario still spawns the real binary per this unit's Verification
/// section, since that is the only way to observe the *actual compiled
/// binary's* JSON output end to end).
pub async fn run_with_args(args: Cli) -> i32 {
    let as_json = args.json;

    let deps = match build_deps(&args, None).await {
        Ok(deps) => deps,
        Err(e) => {
            println!("{}", format::render_error(&e, as_json));
            return e.exit_code() as i32;
        }
    };

    let state_dir = deps.state_dir.clone();
    let ops = deps.into_ops();

    match dispatch(&ops, state_dir, &args, as_json).await {
        Ok(code) => code,
        Err(e) => {
            println!("{}", format::render_error(&e, as_json));
            e.exit_code() as i32
        }
    }
}

// ---------------------------------------------------------------------
// Backend construction — the real `--backend demo|libvirt|exedev|auto`
// resolution this unit's dispatch layer was missing entirely.
// ---------------------------------------------------------------------

/// The constructed backend plus a caller-supplied display name —
/// `LsbxOps::new` needs both (see `lsbx-ops`'s own module doc comment for
/// why `&dyn Backend` alone cannot supply a name: the real `Backend` trait
/// has no `name()`/`available()` method).
struct BuiltBackend {
    backend: Box<dyn Backend>,
    name: String,
}

/// Resolves `--backend` into a real, constructed `Backend`.
///
/// `demo` and `libvirt` are unconditional: `demo` always succeeds
/// (in-memory mock, SPEC.md §4.4); `libvirt` attempts a real connection and
/// propagates failure as `LsbxError::BackendUnavailable` — an explicit
/// `--backend libvirt` should fail loudly if libvirt isn't actually
/// reachable, not silently fall back to something else. `exedev` likewise
/// propagates its own probe failure rather than falling back, for the same
/// reason. `auto` is the only choice that falls through candidates — see
/// [`probe_auto`].
async fn build_backend(
    choice: &BackendChoice,
    state_dir: &std::path::Path,
) -> Result<BuiltBackend, LsbxError> {
    match choice {
        BackendChoice::Demo => Ok(BuiltBackend {
            backend: Box::new(DemoBackend::new()),
            name: "demo".to_string(),
        }),
        BackendChoice::Libvirt => {
            let backend = connect_libvirt(state_dir).await?;
            Ok(BuiltBackend {
                backend: Box::new(backend),
                name: "libvirt".to_string(),
            })
        }
        BackendChoice::Exedev => {
            let backend = build_exedev()?;
            // An explicitly requested `--backend exedev` should prove the
            // control plane actually answers before this CLI trusts it,
            // same "ran vs. proven" stance the rest of this system takes
            // (SPEC.md §1) — not just that construction didn't panic.
            probe_backend(&backend).await?;
            Ok(BuiltBackend {
                backend: Box::new(backend),
                name: "exedev".to_string(),
            })
        }
        BackendChoice::Auto => probe_auto(state_dir).await,
    }
}

/// `--backend auto`: probes `libvirt` then `exedev` then `demo`, matching
/// this unit's own acceptance criterion and the existing system's
/// documented fallback order (SPEC.md §4.4) exactly. Each candidate beyond
/// `demo` is both *constructed* and *live-probed* (`Backend::list_vms()`)
/// before being trusted — a `LibvirtBackend`/`ExedevBackend` that
/// constructs without error but whose control plane doesn't actually
/// answer must still fall through, not be selected on construction success
/// alone. `demo` is the unconditional final fallback and never itself
/// fails to probe (it has no real infrastructure to be unavailable).
async fn probe_auto(state_dir: &std::path::Path) -> Result<BuiltBackend, LsbxError> {
    if let Ok(backend) = connect_libvirt(state_dir).await {
        if probe_backend(&backend).await.is_ok() {
            return Ok(BuiltBackend {
                backend: Box::new(backend),
                name: "libvirt".to_string(),
            });
        }
    }

    if let Ok(backend) = build_exedev() {
        if probe_backend(&backend).await.is_ok() {
            return Ok(BuiltBackend {
                backend: Box::new(backend),
                name: "exedev".to_string(),
            });
        }
    }

    Ok(BuiltBackend {
        backend: Box::new(DemoBackend::new()),
        name: "demo".to_string(),
    })
}

/// Attempts a real local libvirt connection. Deferred behind its own
/// function (per this unit's own instructions) so `probe_auto` can catch a
/// connection failure and fall through rather than propagating it.
///
/// `LSBX_LIBVIRT_URI` overrides the connection URI (default
/// `qemu:///system` via `LibvirtTransport::Local { uri: None }`);
/// `LSBX_LIBVIRT_IMAGES_DIR`/`LSBX_LIBVIRT_VM_DIR` override where golden
/// disks are read from and per-VM disks are written, defaulting to
/// `<state_dir>/images` and `<state_dir>/vms` respectively when unset —
/// this crate has no mandate to invent a `/var/lib/lsbx`-rooted deployment
/// convention (that's Unit 19/host bootstrap's job), so it roots libvirt's
/// working storage under whatever state directory this CLI was already
/// told to use.
async fn connect_libvirt(state_dir: &std::path::Path) -> Result<LibvirtBackend, LsbxError> {
    let uri = std::env::var("LSBX_LIBVIRT_URI").ok();
    let transport = lsbx_backend_libvirt::transport::LibvirtTransport::Local { uri };

    let images_dir = std::env::var("LSBX_LIBVIRT_IMAGES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state_dir.join("images"));
    let vm_dir = std::env::var("LSBX_LIBVIRT_VM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state_dir.join("vms"));

    let backend = LibvirtBackend::connect(transport)
        .await?
        .with_images_dir(images_dir)
        .with_work_dir(vm_dir);
    Ok(backend)
}

/// Builds an `ExedevBackend` from whatever auth this CLI can find in the
/// environment. No door in this workspace has landed a real exedev
/// configuration convention yet, so this reads the same shape of
/// environment variables the existing Python system's exedev integration
/// documents (`EXE_TOKEN` for the account-wide token) plus an explicit
/// fallback SSH key path (`LSBX_EXEDEV_SSH_KEY`) for the documented
/// 422-to-SSH retry (see `ExedevAuth`'s own doc comment for why that path
/// must be explicit rather than guessed). Falls back to SSH-only auth via
/// `LSBX_EXEDEV_SSH_KEY` alone when no token is set. Never itself fails —
/// construction of `ExedevAuth`/`ExedevBackend` is not fallible — so
/// callers that need to know whether this backend can actually do
/// anything must still call [`probe_backend`].
fn build_exedev() -> Result<ExedevBackend, LsbxError> {
    let ssh_key = std::env::var("LSBX_EXEDEV_SSH_KEY").ok().map(PathBuf::from);

    let auth = if let Ok(token) = std::env::var("EXE_TOKEN") {
        match ssh_key {
            Some(key) => ExedevAuth::account_token_with_fallback(token, key),
            None => ExedevAuth::account_token(token),
        }
    } else if let Some(key) = ssh_key {
        ExedevAuth::Ssh { key_path: key }
    } else {
        return Err(LsbxError::BackendUnavailable(
            "no exedev auth configured (set EXE_TOKEN and/or LSBX_EXEDEV_SSH_KEY)".to_string(),
        ));
    };

    Ok(ExedevBackend::new(auth))
}

/// Live-probes a constructed backend's control plane via
/// `Backend::list_vms()`, matching `lsbx-ops::status`'s own "ran vs.
/// proven" stance (SPEC.md §1) — a backend that constructs without error
/// but whose control plane doesn't actually answer is not "available" in
/// any sense this CLI should trust for `--backend auto`'s fallthrough or
/// for an explicit `--backend libvirt|exedev` request.
async fn probe_backend(backend: &dyn Backend) -> Result<(), LsbxError> {
    backend.list_vms().await.map(|_| ())
}

// ---------------------------------------------------------------------
// The other LsbxOps dependencies: SandboxStore, CiJobStore, ImageRegistry,
// SystemClock — constructed from --state-dir/--images/--config with
// sensible defaults when unset.
// ---------------------------------------------------------------------

/// Resolves `--state-dir`, defaulting to `~/.local/share/lsbx` (falling
/// back to `/tmp/lsbx` if `$HOME` can't be resolved at all — this CLI must
/// still be able to run somewhere rather than hard-fail on a missing home
/// directory, e.g. inside a minimal container).
fn resolve_state_dir(explicit: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(from_env) = std::env::var("LSBX_STATE_DIR") {
        return PathBuf::from(from_env);
    }
    dirs_home()
        .map(|home| home.join(".local/share/lsbx"))
        .unwrap_or_else(|| PathBuf::from("/tmp/lsbx"))
}

/// Resolves `--images`, defaulting to `<state_dir>/images.json`. When the
/// file doesn't exist yet (no unit anywhere in this workspace has landed a
/// bootstrap step that creates one — that's plausibly Unit 19/20's job),
/// this CLI must still be usable for every subcommand that doesn't depend
/// on a populated golden catalog (`up`, `down`, `list`, `exec`, `status`,
/// etc. all work against an empty catalog just fine), so [`build_deps`]
/// falls back to an empty, in-memory `ImageRegistry` rather than making
/// every invocation of this CLI fail before a real `images.json` exists.
fn resolve_images_path(explicit: Option<&std::path::Path>, state_dir: &std::path::Path) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(from_env) = std::env::var("LSBX_IMAGES_PATH") {
        return PathBuf::from(from_env);
    }
    state_dir.join("images.json")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Everything `LsbxOps::new` needs, resolved from parsed args, plus the
/// resolved `state_dir` itself — carried alongside for the `serve`/
/// `ci-broker run` subcommands (Gap 1/3), which each need a second,
/// independent `SandboxStore`/`CiJobStore` pointed at the same directory
/// `LsbxOps`'s own store was built from (`lsbx-gateway`'s `GatewayDeps` and
/// `lsbx-broker`'s `run_broker` both need direct `CiJobStore`/`SandboxStore`
/// access that `LsbxOps` itself does not expose — see those crates' own
/// module doc comments for why).
struct OpsDeps {
    backend: Box<dyn Backend>,
    backend_name: String,
    sandbox_store: SandboxStore,
    ci_job_store: CiJobStore,
    registry: ImageRegistry,
    clock: Box<dyn Clock>,
    state_dir: PathBuf,
}

impl OpsDeps {
    fn into_ops(self) -> LsbxOps {
        LsbxOps::new(
            self.backend,
            self.backend_name,
            self.sandbox_store,
            self.ci_job_store,
            self.registry,
            self.clock,
        )
    }
}

/// Builds every `LsbxOps` dependency from the parsed global flags. This is
/// the composition root this unit's acceptance criteria describes: "match
/// on `args.command` and actually call the corresponding `LsbxOps`
/// method" only works once a real `LsbxOps` exists to call it on, and this
/// is where that instance is actually assembled — the exact gap neither
/// prior Jules candidate closed.
///
/// `backend_override`, when `Some`, takes precedence over `args.backend` —
/// added by Gap 3's `ci-broker run` subcommand, whose own `--backend` flag
/// is scoped to `CiBrokerCommand::Run` (not the top-level `Cli::backend`
/// global flag every other subcommand reads), so `dispatch_ci_broker` calls
/// this function with `Some(backend)` from its own parsed flag rather than
/// needing `Cli`/`Command` to derive `Clone` just to reconstruct a
/// synthetic top-level `Cli` value (neither type derives it, and adding it
/// solely for this one internal call site would be a needless surface
/// change to `cli.rs`, whose own module doc comment reserves it for
/// argument-parsing definitions only).
async fn build_deps(args: &Cli, backend_override: Option<BackendChoice>) -> Result<OpsDeps, LsbxError> {
    let state_dir = resolve_state_dir(args.state_dir.as_deref());
    let images_path = resolve_images_path(args.images.as_deref(), &state_dir);

    // `--config` is accepted (interface-contract parity with the existing
    // CLI surface) but no merged crate defines a real config-file schema
    // yet (see `lsbx-ops::config_show`'s own honest-gap note) — there is
    // nothing to load from it today beyond what `--state-dir`/`--images`
    // already cover. Recorded so a future config loader has an obvious
    // seam to land in rather than this crate silently ignoring the flag.
    let _config_path = args.config.clone();

    let backend_choice = backend_override.or_else(|| args.backend.clone()).unwrap_or(BackendChoice::Demo);
    let built = build_backend(&backend_choice, &state_dir).await?;

    let sandbox_store = SandboxStore::new(state_dir.clone());
    let ci_job_store = CiJobStore::new(state_dir.clone());

    let registry = match ImageRegistry::load(&images_path) {
        Ok(registry) => registry,
        // No populated catalog yet is not this CLI's problem to fail on —
        // see `resolve_images_path`'s doc comment. A malformed (but
        // present) file is a real problem and still propagates.
        Err(LsbxError::NotFound(_)) => ImageRegistry {
            images: Vec::new(),
            goldens: Vec::new(),
            profiles: std::collections::HashMap::new(),
        },
        Err(e) => return Err(e),
    };

    if args.verbose {
        eprintln!("lsbx: selected backend '{}'", built.name);
    }

    Ok(OpsDeps {
        backend: built.backend,
        backend_name: built.name,
        sandbox_store,
        ci_job_store,
        registry,
        clock: Box::new(SystemClock),
        state_dir,
    })
}

// ---------------------------------------------------------------------
// Dispatch: match on args.command, call the corresponding LsbxOps method,
// translate CLI args into the real typed request structs.
// ---------------------------------------------------------------------

/// Parses a duration string. Accepts a bare integer (seconds) or a
/// `<n><unit>` suffix (`s`, `m`, `h`, `d`) — the smallest parser that
/// covers `--lease`/`renew <duration>`/`--ready-timeout`/`reap --ttl`'s
/// documented shapes without inventing an external duration-parsing
/// dependency this crate doesn't otherwise need.
fn parse_duration(input: &str) -> Result<Duration, LsbxError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(LsbxError::Usage("duration must not be empty".to_string()));
    }

    let (num_part, unit) = match input.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&input[..input.len() - 1], c),
        _ => (input, 's'),
    };

    let value: u64 = num_part.parse().map_err(|_| {
        LsbxError::Usage(format!("invalid duration '{input}': not a valid number"))
    })?;

    let seconds = match unit {
        's' => value,
        'm' => value.saturating_mul(60),
        'h' => value.saturating_mul(3600),
        'd' => value.saturating_mul(86400),
        other => {
            return Err(LsbxError::Usage(format!(
                "invalid duration unit '{other}' in '{input}' (expected s, m, h, or d)"
            )))
        }
    };

    Ok(Duration::from_secs(seconds))
}

const DEFAULT_LEASE: Duration = Duration::from_secs(3600);
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_REAP_TTL: Duration = Duration::ZERO;
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// Generates an ephemeral keypair for a golden-build/verify/register call
/// that needs a `pubkey` — see this module's own doc comment for why this
/// crate (not `lsbx-ops`/`lsbx-golden`) is responsible for that, mirroring
/// what `lsbx_lifecycle::create::create` already does internally for `up`.
/// Returns just the public key line; the private half is cleaned up
/// immediately after use since a golden-build/verify VM's own lifecycle
/// (not a persisted `SandboxRecord`) is what would otherwise need it again.
fn generate_pubkey_for(label: &str) -> Result<(String, lsbx_keys::keygen::EphemeralKeypair), LsbxError> {
    let keypair = lsbx_keys::keygen::generate_ephemeral_keypair(label)?;
    Ok((keypair.public_key_line.clone(), keypair))
}

/// Matches `args.command` and calls the corresponding `LsbxOps` method,
/// rendering the result through the one formatting path. Returns the
/// process exit code (`0` on success, or `error.exit_code()` when a
/// non-fatal-to-dispatch subcommand still reports failure, e.g. `down`
/// against a nonexistent id).
///
/// `state_dir` is threaded through alongside `ops`/`args` (Gap 1/3): the
/// `Serve`/`CiBroker` arms each need a second, independent
/// `SandboxStore`/`CiJobStore` pointed at the same directory `ops`'s own
/// stores were built from, which `LsbxOps` itself has no accessor for (its
/// `sandbox_store`/`ci_job_store` fields are private by design — see
/// `lsbx-ops`'s own module doc comment).
async fn dispatch(ops: &LsbxOps, state_dir: PathBuf, args: &Cli, as_json: bool) -> Result<i32, LsbxError> {
    match &args.command {
        None => {
            // TODO: hand off to lsbx-tui once wired as a dependency of this
            // crate (Unit 12's dashboard, bare `lsbx` when stdout is a TTY
            // — SPEC.md §4.8, this unit's own Boundaries). This
            // deliberately does not add a dependency on `lsbx-tui` as part
            // of this integration-wiring pass (out of scope for the four
            // gaps this pass closes); falling back to `status`
            // unconditionally (not just when stdout is not a TTY) keeps
            // this crate's dependency graph honest about what it actually
            // ships today.
            let status = ops.status().await?;
            println!("{}", format::render(&StatusReportDto::from(status), as_json));
            Ok(0)
        }
        Some(Command::Up {
            profile,
            count,
            name,
            task_id,
            lease,
            no_verify,
            ready_timeout,
        }) => {
            let lease_duration = match lease {
                Some(s) => parse_duration(s)?,
                None => DEFAULT_LEASE,
            };
            let ready_timeout_duration = match ready_timeout {
                Some(secs) => Duration::from_secs(*secs),
                None => DEFAULT_READY_TIMEOUT,
            };
            let n = count.unwrap_or(1).max(1);

            let mut exit_code = 0;
            for i in 0..n {
                // `--name` for a `--count > 1` request is disambiguated
                // with a numeric suffix, matching the existing CLI's own
                // multi-instance naming convention rather than silently
                // reusing one name for every created sandbox.
                let per_instance_name = match (name, n) {
                    (Some(base), n) if n > 1 => Some(format!("{base}-{i}")),
                    (Some(base), _) => Some(base.clone()),
                    (None, _) => None,
                };

                let req = lsbx_lifecycle::create::CreateRequest {
                    profile,
                    name: per_instance_name.as_deref(),
                    task_id: task_id.as_deref(),
                    lease: lease_duration,
                    ready_timeout: ready_timeout_duration,
                    verify: !no_verify,
                    healthchecks: Vec::new(),
                };

                let result = ops.create(req).await;
                match &result {
                    Ok(sandbox) => println!("{}", format::render(sandbox, as_json)),
                    Err(e) => {
                        println!("{}", format::render_error(e, as_json));
                        exit_code = e.exit_code() as i32;
                    }
                }
            }
            Ok(exit_code)
        }
        Some(Command::Down { ids, all }) => {
            let targets: Vec<String> = if *all {
                ops.list().await?.into_iter().map(|s| s.id).collect()
            } else {
                ids.clone()
            };

            if targets.is_empty() {
                println!("{}", format::render(&EmptyList::default(), as_json));
                return Ok(0);
            }

            let mut exit_code = 0;
            for id in &targets {
                match ops.destroy(id).await {
                    Ok(()) => println!("{}", format::render(&DestroyedId(id.clone()), as_json)),
                    Err(e) => {
                        println!("{}", format::render_error(&e, as_json));
                        exit_code = e.exit_code() as i32;
                    }
                }
            }
            Ok(exit_code)
        }
        Some(Command::List { profile, expired }) => {
            let mut sandboxes = ops.list().await?;
            if let Some(p) = profile {
                sandboxes.retain(|s| &s.profile == p);
            }
            if *expired {
                let now = std::time::SystemTime::now();
                sandboxes.retain(|s| is_expired_public(s, now));
            }
            println!("{}", format::render(&SandboxList(sandboxes), as_json));
            Ok(0)
        }
        Some(Command::Exec { id, timeout, command }) => {
            let timeout_duration = timeout
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_EXEC_TIMEOUT);
            let output = ops.exec(id, command, timeout_duration).await?;
            let dto = CommandOutputDto::from(&output);
            println!("{}", format::render(&dto, as_json));
            Ok(output.exit_code)
        }
        Some(Command::Put { id, source, destination }) => {
            ops.put(id, source, destination).await?;
            println!("{}", format::render(&PutGetResult { id: id.clone() }, as_json));
            Ok(0)
        }
        Some(Command::Get { id, source, destination }) => {
            ops.get(id, source, destination).await?;
            println!("{}", format::render(&PutGetResult { id: id.clone() }, as_json));
            Ok(0)
        }
        Some(Command::Renew { id, duration }) => {
            let d = parse_duration(duration)?;
            let sandbox = ops.renew(id, d).await?;
            println!("{}", format::render(&sandbox, as_json));
            Ok(0)
        }
        Some(Command::Console { id }) => {
            let url = ops.console_url(id).await?;
            println!("{}", format::render(&ConsoleUrlDto(url), as_json));
            Ok(0)
        }
        Some(Command::Info { id }) => {
            let sandbox = ops.info(id).await?;
            println!("{}", format::render(&sandbox, as_json));
            Ok(0)
        }
        Some(Command::Status) => {
            let status = ops.status().await?;
            println!("{}", format::render(&StatusReportDto::from(status), as_json));
            Ok(0)
        }
        Some(Command::Profiles { full }) => {
            let registry_summary = ops.config_show().await?;
            let profiles = registry_summary
                .get("profiles")
                .and_then(|p| p.get("keys"))
                .cloned()
                .unwrap_or(serde_json::json!([]));
            let dto = ProfilesDto {
                profiles,
                full: *full,
            };
            println!("{}", format::render(&dto, as_json));
            Ok(0)
        }
        Some(Command::Images) => {
            let registry_summary = ops.config_show().await?;
            let images = registry_summary
                .get("images")
                .and_then(|i| i.get("keys"))
                .cloned()
                .unwrap_or(serde_json::json!([]));
            println!("{}", format::render(&ImagesDto(images), as_json));
            Ok(0)
        }
        Some(Command::Reap { ttl, dry_run }) => {
            let ttl_duration = match ttl {
                Some(s) => parse_duration(s)?,
                None => DEFAULT_REAP_TTL,
            };
            let report = ops.reap(ttl_duration, *dry_run).await?;
            println!("{}", format::render(&ReapReportDto::from(report), as_json));
            Ok(0)
        }
        Some(Command::Serve {
            host,
            port,
            stream_port,
            token,
            reap_ttl,
            daemon,
            insecure,
        }) => {
            dispatch_serve(
                args,
                state_dir,
                host.as_deref(),
                *port,
                *stream_port,
                token.clone(),
                reap_ttl.as_deref(),
                *daemon,
                *insecure,
            )
            .await
        }
        Some(Command::Bootstrap {
            target,
            no_services,
            no_verify,
            force,
            dry_run,
        }) => dispatch_bootstrap(target.clone(), *no_services, *no_verify, *force, *dry_run, as_json).await,
        Some(Command::Golden { action }) => dispatch_golden(ops, action, as_json).await,
        Some(Command::Config { show, init, set, path, force }) => {
            dispatch_config(ops, *show, *init, set.as_deref(), *path, *force, as_json).await
        }
        Some(Command::Logs { since, limit, .. }) => {
            let result = ops.logs_query(since.as_deref(), limit.unwrap_or(100)).await;
            match result {
                Ok(lines) => {
                    println!("{}", format::render(&LogLines(lines), as_json));
                    Ok(0)
                }
                Err(e) => Err(e),
            }
        }
        Some(Command::Mcp) => dispatch_mcp(args).await,
        Some(Command::CiBroker { action }) => dispatch_ci_broker(args, action).await,
    }
}

async fn dispatch_golden(
    ops: &LsbxOps,
    action: &GoldenCommand,
    as_json: bool,
) -> Result<i32, LsbxError> {
    match action {
        GoldenCommand::List => {
            let goldens = ops.golden_list().await?;
            println!("{}", format::render(&GoldenList(goldens), as_json));
            Ok(0)
        }
        GoldenCommand::Build {
            name,
            from,
            script,
            flavor,
            cpu,
            memory,
            streaming,
            register,
            no_cleanup,
            interactive: _,
            shell: _,
            dry_run,
        } => {
            let flavor = parse_flavor(flavor)?;
            let streaming_mode = parse_streaming(streaming.as_deref());

            let (pubkey, keypair) = if *dry_run {
                // A dry-run build never calls `Backend::create_from_golden`
                // (see `lsbx_golden::build::golden_build`'s own dry-run
                // short-circuit), so generating a real ephemeral keypair
                // for it would be pure waste — a fixed placeholder is
                // sufficient since it is never actually presented to
                // anything.
                (
                    "ssh-ed25519 AAAA lsbx-dry-run-placeholder".to_string(),
                    None,
                )
            } else {
                let (pk, kp) = generate_pubkey_for(&format!("golden-build-{name}"))?;
                (pk, Some(kp))
            };

            let req = lsbx_golden::build::GoldenBuildRequest {
                name,
                from,
                script,
                flavor,
                cpu: *cpu,
                memory,
                streaming: streaming_mode,
                register: *register,
                cleanup: !no_cleanup,
                dry_run: *dry_run,
                pubkey: &pubkey,
            };

            let result = ops.golden_build(req).await;

            if let Some(kp) = keypair {
                let _ = lsbx_keys::keygen::cleanup_keypair(&kp);
            }

            let outcome = result?;
            println!(
                "{}",
                format::render(&GoldenBuildOutcomeDto::from(outcome), as_json)
            );
            Ok(0)
        }
        GoldenCommand::Verify { name } => {
            let (pubkey, keypair) = generate_pubkey_for(&format!("golden-verify-{name}"))?;
            let verify_name = format!("verify-{name}");
            let result = ops.golden_verify(name, &verify_name, &pubkey).await;
            let _ = lsbx_keys::keygen::cleanup_keypair(&keypair);

            let results = result?;
            let dto = HealthcheckResultsDto(
                results.into_iter().map(HealthcheckResultDto::from).collect(),
            );
            let all_passed = dto.0.iter().all(|r| r.passed);
            println!("{}", format::render(&dto, as_json));
            if all_passed {
                Ok(0)
            } else {
                Ok(LsbxError::ContractViolated(String::new()).exit_code() as i32)
            }
        }
        GoldenCommand::Register {
            name,
            profile: _,
            base,
            flavor,
            streaming,
            capabilities,
            healthcheck,
            content_hash,
            replace,
        } => {
            let flavor = parse_flavor(flavor)?;
            let streaming_mode = parse_streaming(streaming.as_deref());

            if *replace {
                // Best-effort: a golden that doesn't exist yet is fine to
                // "replace" (there's nothing to remove first); any other
                // failure from the delete attempt should not block the
                // subsequent register, since `replace` promises "make sure
                // this exists afterward, whatever was there before"
                // rather than "the delete step itself must succeed."
                let _ = ops.golden_delete(name, false).await;
            }

            let config = GoldenConfig {
                key: name.clone(),
                flavor,
                os: "linux".to_string(),
                base: base.clone(),
                mode: GoldenMode::Copy,
                cpu: 1,
                memory: "1G".to_string(),
                disk: None,
                streaming: streaming_mode,
                capabilities: capabilities.clone(),
                healthcheck: healthcheck.clone(),
                repo: None,
                content_hash: content_hash.clone(),
                description: format!("Registered via lsbx golden register ({base})"),
            };

            ops.golden_register(config).await?;
            println!("{}", format::render(&RegisteredGolden(name.clone()), as_json));
            Ok(0)
        }
        GoldenCommand::Delete { name, keep_snapshot } => {
            ops.golden_delete(name, *keep_snapshot).await?;
            println!("{}", format::render(&DeletedGolden(name.clone()), as_json));
            Ok(0)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_config(
    ops: &LsbxOps,
    show: bool,
    init: bool,
    set: Option<&str>,
    path: bool,
    _force: bool,
    as_json: bool,
) -> Result<i32, LsbxError> {
    if let Some(kv) = set {
        // No merged crate defines a real, writable config schema yet (see
        // `lsbx-ops::config_show`'s own honest-gap note) — there is
        // nowhere real to persist a `KEY=VALUE` pair today. Reported as a
        // named gap rather than silently accepted and discarded.
        return Err(LsbxError::ContractViolated(format!(
            "config --set is not implemented — no merged crate owns a writable config \
             schema yet (attempted to set '{kv}')"
        )));
    }
    if init {
        return Err(LsbxError::ContractViolated(
            "config --init is not implemented — no merged crate owns a config file format \
             to initialize yet"
                .to_string(),
        ));
    }
    if path {
        println!("{}", format::render(&ConfigPathDto, as_json));
        return Ok(0);
    }

    // `--show` and the bare `config` invocation behave identically: both
    // just show the current, real config summary this façade can
    // honestly report (`lsbx-ops::config_show`).
    let _ = show;
    let summary = ops.config_show().await?;
    println!("{}", format::render(&ConfigShowDto(summary), as_json));
    Ok(0)
}

// ---------------------------------------------------------------------
// dispatch_bootstrap — Gap 3: wires `lsbx bootstrap` to the real
// `lsbx_bootstrap::systemd::bootstrap`.
// ---------------------------------------------------------------------

/// `lsbx bootstrap [--target --no-services --no-verify --force --dry-run]`
/// -> `lsbx_bootstrap::systemd::bootstrap(BootstrapConfig { .. })`.
///
/// Field names/types confirmed by direct re-read of
/// `crates/lsbx-bootstrap/src/systemd.rs`'s real `BootstrapConfig` (`target:
/// Option<String>, install_services: bool, verify: bool, force: bool,
/// dry_run: bool`) immediately before writing this function — `--no-services`/
/// `--no-verify` are the CLI's own negated-flag spelling of
/// `install_services`/`verify`, so the two booleans below are inverted from
/// the CLI flag names, matching `BootstrapConfig`'s own field semantics
/// exactly (`install_services: !no_services`, `verify: !no_verify`).
///
/// This function has no `LsbxOps`/backend dependency at all — bootstrap is
/// entirely host-verification and systemd-unit-file work
/// (`lsbx-bootstrap`'s own Boundaries: "does not implement
/// `create_from_golden` or any domain VM lifecycle"), so it does not take
/// `ops` as a parameter the way every other dispatch function here does.
async fn dispatch_bootstrap(
    target: Option<String>,
    no_services: bool,
    no_verify: bool,
    force: bool,
    dry_run: bool,
    as_json: bool,
) -> Result<i32, LsbxError> {
    let config = lsbx_bootstrap::systemd::BootstrapConfig {
        target,
        install_services: !no_services,
        verify: !no_verify,
        force,
        dry_run,
    };

    let report = lsbx_bootstrap::systemd::bootstrap(config).await?;
    println!("{}", format::render(&BootstrapReportDto::from(report), as_json));
    Ok(0)
}

// ---------------------------------------------------------------------
// dispatch_mcp — Gap 3: wires `lsbx mcp` to the real
// `lsbx_mcp::run_stdio_server`.
// ---------------------------------------------------------------------

/// `lsbx mcp` -> `lsbx_mcp::run_stdio_server(Arc<LsbxOps>)`.
///
/// `run_stdio_server`'s real signature (confirmed by direct re-read of
/// `crates/lsbx-mcp/src/lib.rs`) takes `ops: std::sync::Arc<lsbx_ops::LsbxOps>`
/// and blocks until the MCP client disconnects — this is correct MCP
/// server behavior, not a hang: once this subcommand runs, the CLI process
/// *becomes* the MCP server for the rest of its life, taking over stdio.
///
/// The real `dispatch`/`run` signatures here build one `ops: LsbxOps` (an
/// owned value, not `Arc`-wrapped) in `run_with_args` and pass a borrowed
/// `&LsbxOps` into `dispatch`, matching every other subcommand's own needs
/// — none of them need to own or share it. `run_stdio_server` needs an
/// owned `Arc`, which a bare `&LsbxOps` cannot produce without either
/// changing every other dispatch function's signature (unnecessary churn
/// for one subcommand) or constructing a *fresh* `LsbxOps` from the same
/// parsed args. Since `mcp` is a terminal subcommand (nothing else in this
/// process runs after or alongside it — it blocks on stdio until exit),
/// this function re-resolves a fresh `OpsDeps`/`LsbxOps` from `args` (the
/// same `Cli` value `run_with_args` already parsed once — passed through
/// here rather than re-parsed from `std::env::args_os()`, which would
/// silently diverge from whatever `Cli` value a caller of `run_with_args`
/// actually supplied, e.g. this crate's own unit tests), wraps *that*
/// instance in the `Arc` `run_stdio_server` requires, and hands it off.
/// This means `mcp` builds its backend/store/registry twice (once in
/// `run_with_args`'s initial `build_deps()` call, whose resulting
/// `ops: &LsbxOps` `dispatch` receives but cannot reuse across an
/// ownership boundary, and once more here) — a real, small inefficiency,
/// documented here rather than hidden, and harmless in practice since both
/// constructions are cheap (`SandboxStore`/`CiJobStore`/`ImageRegistry` are
/// plain, synchronous, non-pooled handles, and
/// `DemoBackend`/`LibvirtBackend`/`ExedevBackend` construction does no
/// expensive work up front beyond what `build_backend` already does for
/// the first call).
async fn dispatch_mcp(args: &Cli) -> Result<i32, LsbxError> {
    let deps = build_deps(args, None).await?;
    let mcp_ops = std::sync::Arc::new(deps.into_ops());

    lsbx_mcp::run_stdio_server(mcp_ops).await?;
    Ok(0)
}

// ---------------------------------------------------------------------
// dispatch_serve — Gap 1/3: wires `lsbx serve` to the real
// `lsbx_gateway`/`lsbx_stream`, with a background reap loop.
// ---------------------------------------------------------------------

/// Default port `lsbx serve` binds when `--port` is not given. Chosen to
/// match the existing gateway's own documented default
/// (SPEC.md §4.8's Door 2 preserves "the exact existing route table" —
/// the existing Python gateway's own default port).
const DEFAULT_SERVE_PORT: u16 = 8080;

/// Floor for the background reap-loop interval — see [`reap_loop_interval`]
/// for the full design rationale.
const MIN_REAP_LOOP_INTERVAL: Duration = Duration::from_secs(30);

/// Default reap TTL when `--reap-ttl` is not given, matching the same
/// zero-TTL-means-"sweep iff expired" default every other TTL-accepting
/// subcommand (`reap --ttl`) already uses in this crate.
const DEFAULT_SERVE_REAP_TTL: Duration = Duration::ZERO;

/// `lsbx serve [--host --port --stream-port --token --reap-ttl --daemon]`.
///
/// ## Design (already decided; this function implements it)
///
/// **Single vs. dual listener.** When `stream_port` is unset, or set to the
/// same value as `port`, this runs ONE bound server on `port` serving the
/// gateway's own merged router — per Gap 1 (`lsbx-gateway`'s
/// `GatewayDeps::build_router`), that router already includes
/// `lsbx-stream`'s mounted `/stream/*`, `/console`, `/consoles/*` routes
/// alongside the gateway's own REST routes, so "the merged router" and
/// "gateway + stream together" are the same thing at this point in the
/// integration. This is the default, simplest mode — most deployments have
/// no reason to split gateway and stream traffic onto separate ports.
///
/// When `stream_port` is explicitly set to a *different* value than `port`,
/// this runs TWO independent bound servers concurrently, via `tokio::join!`:
/// one gateway-*only* listener on `port` (the plain `routes::build_router`
/// router, deliberately bypassing the Gap 1 merge — this is the one place
/// in this codebase that constructs the gateway's route table without also
/// mounting `lsbx-stream`, precisely because the caller asked for the two
/// to be reachable on separate ports), and one stream-*only* listener on
/// `stream_port` (`lsbx_stream::router` constructed directly, with its own
/// second `Arc<SandboxStore>` — the same one `GatewayDeps` would have built
/// internally, just constructed here instead since this path never calls
/// `GatewayDeps::build_router` at all).
///
/// **Background reap loop.** Either way, this also spawns a background
/// task that calls `LsbxOps::reap(reap_ttl, dry_run: false)` on an
/// interval, logging the result via `tracing` and never letting a reap
/// error crash the serve loop (a failed reap pass is logged and retried
/// next interval, not propagated up to tear down the whole `serve`
/// invocation — a transient backend hiccup during a reap sweep must not
/// take down request-serving). The interval is `reap_ttl / 4`, floored at
/// `MIN_REAP_LOOP_INTERVAL` (30s) — see [`reap_loop_interval`]'s own doc
/// comment for why this specific formula was chosen; it is a judgment
/// call, documented here and at the call site, not a value derived from
/// any acceptance criterion this task names literally.
///
/// **`--daemon` semantics — a deliberate deviation from what the flag name
/// might suggest.** This does **not** implement real Unix double-fork
/// daemonization. Forking a multi-threaded Tokio process is unsafe (only
/// async-signal-safe syscalls may run between `fork()` and `exec()`/`exit()`
/// in the child, and a Tokio runtime's worker threads, timers, and I/O
/// driver are not in any way fork-safe once the runtime has started) and is
/// not how this system is meant to be deployed — systemd already supervises
/// the two broker services per Unit 19/AGENTS.md's "Broker operations"
/// section (`lsbx-ci-broker`, `lsbx-ci-broker-exe`), and the same
/// operational pattern applies here: a real deployment runs `lsbx serve`
/// under its own systemd unit (or `nohup`, or an equivalent process
/// supervisor), which already provides real backgrounding, restart-on-
/// failure, and log capture — all the things a real daemonization
/// implementation would otherwise need to reinvent. Concretely, `--daemon`
/// here means: suppress the human-readable startup banner this function
/// would otherwise print to stdout (so a systemd unit's captured stdout
/// stays clean of a banner meant for an interactive terminal), and nothing
/// else. Real backgrounding remains the operator's/systemd's job. This is
/// documented here, in the PR description, and is a deliberate deviation
/// from what the flag name might otherwise suggest.
#[allow(clippy::too_many_arguments)]
async fn dispatch_serve(
    args: &Cli,
    state_dir: PathBuf,
    host: Option<&str>,
    port: Option<u16>,
    stream_port: Option<u16>,
    token: Option<String>,
    reap_ttl: Option<&str>,
    daemon: bool,
    insecure: bool,
) -> Result<i32, LsbxError> {
    let host_str = host.unwrap_or("127.0.0.1");
    let host_ip: std::net::IpAddr = host_str.parse().map_err(|e| {
        LsbxError::Usage(format!("invalid --host value '{host_str}': {e}"))
    })?;
    let gateway_port = port.unwrap_or(DEFAULT_SERVE_PORT);
    let reap_ttl_duration = match reap_ttl {
        Some(s) => parse_duration(s)?,
        None => DEFAULT_SERVE_REAP_TTL,
    };

    let gateway_config = || lsbx_gateway::GatewayConfig {
        token: token.clone(),
        allow_local_files: false,
        insecure,
        rate_limit: lsbx_gateway::RateLimitConfig::default(),
    };

    // Re-resolve a second, independent `LsbxOps` for the same reason
    // `dispatch_mcp` does (see that function's own doc comment): `serve`
    // needs an owned value to move into the background reap task and,
    // in the dual-listener branch, into a second concurrently-running
    // server — a bare `&LsbxOps` borrow cannot outlive this function's own
    // stack frame across `tokio::spawn`/`tokio::join!`. `GatewayDeps`
    // itself takes ownership of an `Arc<LsbxOps>`, so the *serving* side
    // needs an `Arc` either way; re-resolving from `args` (the same `Cli`
    // value `run_with_args` already parsed, threaded through rather than
    // re-parsed from `std::env::args_os()` — see `dispatch_mcp`'s own doc
    // comment for why) keeps every other subcommand's signature exactly as
    // simple as it already is, rather than requiring `run_with_args`'s
    // original `ops` binding to already be an `Arc` just for this one
    // subcommand's sake.
    let serve_deps = build_deps(args, None).await?;
    let serve_state_dir = state_dir.clone();
    let reap_ops = std::sync::Arc::new(serve_deps.into_ops());

    if !daemon {
        println!("lsbx serve: starting on {host_str}:{gateway_port}");
    }

    // Background reap loop, spawned once regardless of single- vs.
    // dual-listener mode — see this function's own doc comment for the
    // interval formula and the "never let a reap error crash serving"
    // guarantee.
    let reap_loop_ops = std::sync::Arc::clone(&reap_ops);
    let reap_interval = reap_loop_interval(reap_ttl_duration);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(reap_interval);
        loop {
            ticker.tick().await;
            match reap_loop_ops.reap(reap_ttl_duration, false).await {
                Ok(report) => {
                    tracing::info!(
                        destroyed = ?report.destroyed,
                        keys_reconciled = report.keys_reconciled,
                        "lsbx serve: background reap pass completed"
                    );
                }
                Err(e) => {
                    // Never let a reap error crash the serve loop — log
                    // and retry on the next interval tick.
                    tracing::warn!(error = %e, "lsbx serve: background reap pass failed; will retry next interval");
                }
            }
        }
    });

    let dual_listener = matches!(stream_port, Some(sp) if sp != gateway_port);

    if !dual_listener {
        // Single merged listener: the gateway's own router already
        // includes lsbx-stream's mounted routes (Gap 1).
        let deps = lsbx_gateway::GatewayDeps {
            ops: reap_ops,
            state_dir: serve_state_dir,
        };
        let addr = std::net::SocketAddr::new(host_ip, gateway_port);
        let bound = lsbx_gateway::run_server(deps, gateway_config(), addr).await?;
        if !daemon {
            println!(
                "lsbx serve: listening on {} (gateway + stream, merged)",
                bound.local_addr
            );
        }
        bound
            .serve()
            .await
            .map_err(|e| LsbxError::ContractViolated(format!("gateway server error: {e}")))?;
        Ok(0)
    } else {
        // Dual listener: gateway-only on `port`, stream-only on
        // `stream_port` — two independent bound servers run concurrently.
        #[allow(clippy::unwrap_used)] // `dual_listener` above already proved `stream_port` is `Some`.
        let stream_port_value = stream_port.unwrap();

        let gateway_router = lsbx_gateway::build_router(std::sync::Arc::clone(&reap_ops), gateway_config());
        let gateway_addr = std::net::SocketAddr::new(host_ip, gateway_port);
        let gateway_listener = tokio::net::TcpListener::bind(gateway_addr).await.map_err(|e| {
            LsbxError::ContractViolated(format!("failed to bind gateway listener on {gateway_addr}: {e}"))
        })?;

        let stream_store = std::sync::Arc::new(lsbx_store::sandbox_store::SandboxStore::new(state_dir.clone()));
        let stream_router = lsbx_stream::router(lsbx_stream::StreamState {
            ops: reap_ops,
            store: stream_store,
        });
        let stream_addr = std::net::SocketAddr::new(host_ip, stream_port_value);
        let stream_listener = tokio::net::TcpListener::bind(stream_addr).await.map_err(|e| {
            LsbxError::ContractViolated(format!("failed to bind stream listener on {stream_addr}: {e}"))
        })?;

        if !daemon {
            println!("lsbx serve: listening on {gateway_addr} (gateway) and {stream_addr} (stream)");
        }

        let gateway_serve = axum::serve(
            gateway_listener,
            gateway_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );
        let stream_serve = axum::serve(
            stream_listener,
            stream_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );

        let (gateway_result, stream_result) = tokio::join!(gateway_serve, stream_serve);
        gateway_result
            .map_err(|e| LsbxError::ContractViolated(format!("gateway server error: {e}")))?;
        stream_result
            .map_err(|e| LsbxError::ContractViolated(format!("stream server error: {e}")))?;
        Ok(0)
    }
}

/// The background reap loop's interval: `reap_ttl / 4`, floored at
/// [`MIN_REAP_LOOP_INTERVAL`] (30s).
///
/// This is a judgment call — no acceptance criterion this task names
/// specifies an exact formula, only "every some sensible interval (say,
/// `reap_ttl / 4`, floored at a reasonable minimum like 30s — use your
/// judgment, document it)." `reap_ttl / 4` means a lease is swept, on
/// average, within a quarter of its own grace window after becoming
/// eligible — frequent enough that `reap`'s own TTL configuration remains
/// meaningful (a TTL of 10 minutes but a reap loop that only runs once an
/// hour would make the TTL setting nearly decorative), without polling so
/// aggressively that a `reap_ttl` of 0 (the default — "sweep iff expired")
/// would imply an interval of 0 and busy-loop the reap task. The 30-second
/// floor exists precisely for that zero/near-zero-TTL case: a `reap_ttl` of
/// 0 or a few seconds still gets a real, bounded reap cadence (30s) rather
/// than an unboundedly tight loop hammering the backend's `list_vms()`/
/// `destroy()` calls.
fn reap_loop_interval(reap_ttl: Duration) -> Duration {
    let quarter = Duration::from_secs_f64(reap_ttl.as_secs_f64() / 4.0);
    quarter.max(MIN_REAP_LOOP_INTERVAL)
}

// ---------------------------------------------------------------------
// dispatch_ci_broker — Gap 3: the new `lsbx ci-broker run` subcommand,
// wiring `lsbx-broker`'s real `run_broker` to a real GitHub client built
// from the LSBX_GITHUB_APP_* environment variables (documented in root
// AGENTS.md) or the gh CLI fallback.
// ---------------------------------------------------------------------

/// `LSBX_QUEUE_LABEL` is already documented in root `AGENTS.md` (used by
/// `lsbx_broker::poll::PollConfig::from_queue_label_and_env`, which this
/// function's own `PollConfig` construction below also reads through
/// indirectly via `--queue-label`/this env var). The four
/// `LSBX_GITHUB_APP_*` variables below are new, added by this pass,
/// following the same `LSBX_*` naming convention — see root `AGENTS.md`'s
/// new "CI broker environment variables" section for the authoritative
/// documentation this doc comment summarizes.
const QUEUE_LABEL_ENV: &str = "LSBX_QUEUE_LABEL";
const GITHUB_APP_ID_ENV: &str = "LSBX_GITHUB_APP_ID";
const GITHUB_APP_PRIVATE_KEY_PATH_ENV: &str = "LSBX_GITHUB_APP_PRIVATE_KEY_PATH";
const GITHUB_APP_INSTALLATION_ID_ENV: &str = "LSBX_GITHUB_APP_INSTALLATION_ID";
const GITHUB_APP_OWNER_ENV: &str = "LSBX_GITHUB_APP_OWNER";

/// Default lease every CI-dispatched sandbox gets, when no more specific
/// configuration exists for it. `lsbx-broker`'s own `Reconciler::dispatch`
/// takes a `lease: Duration` parameter with no built-in default (the
/// interface contract leaves lease policy to the caller) — one hour is a
/// reasonable default bound for a CI job's own runner VM lifetime, matching
/// the same 1-hour default `lsbx up`'s own `DEFAULT_LEASE` already uses
/// elsewhere in this file for the identical "no explicit lease given"
/// case.
const DEFAULT_CI_BROKER_LEASE: Duration = Duration::from_secs(3600);

/// `lsbx ci-broker run --backend=<libvirt|exedev|demo|auto> [--queue-label]`.
///
/// Builds a real `LsbxOps`/`CiJobStore`/`GitHubClient` the same way
/// `build_deps()` already does for every other command (reusing that exact
/// function, not a parallel construction path), then calls the real
/// `lsbx_broker::reconcile::run_broker(&job_store, &ops, &github,
/// BrokerConfig { poll, lease }, iterations: None)` — runs forever, per
/// `run_broker`'s own documented `iterations: Option<u32>` contract
/// (`None` means "run until the process is killed," matching how systemd
/// supervises `lsbx-ci-broker`/`lsbx-ci-broker-exe` per AGENTS.md's
/// "Broker operations" section).
///
/// ## GitHub App credential resolution (new: `LSBX_GITHUB_APP_*`)
///
/// When both `LSBX_GITHUB_APP_ID` and `LSBX_GITHUB_APP_PRIVATE_KEY_PATH`
/// are set, this builds a real `lsbx_broker::auth::GitHubAppConfig` (`app_id`
/// parsed from the former, `private_key_pem` read from the file named by
/// the latter, `installation_id` parsed from `LSBX_GITHUB_APP_INSTALLATION_ID`
/// when present or left `None` for `GitHubAppAuth` to discover on first use
/// — its own real, already-implemented behavior, confirmed against
/// `crates/lsbx-broker/src/auth.rs`), and calls
/// `GitHubClient::from_app_auth(&auth, owner)`, where `owner` comes from
/// the also-new `LSBX_GITHUB_APP_OWNER` (required whenever the App-credential
/// path is used at all, since `from_app_auth`'s real signature needs an
/// `owner: &str` to scope the installation-token exchange to).
///
/// When either `LSBX_GITHUB_APP_ID` or `LSBX_GITHUB_APP_PRIVATE_KEY_PATH`
/// is unset, this falls back to `GitHubClient::from_gh_cli_fallback()` —
/// exactly the same fallback `lsbx-broker` itself already documents and
/// implements for local dev/testing without full GitHub App credentials
/// (confirmed against `crates/lsbx-broker/src/github_client.rs`'s real,
/// already-merged `from_gh_cli_fallback` constructor — this function does
/// not invent a new fallback mechanism, it invokes the existing one).
async fn dispatch_ci_broker(args: &Cli, action: &CiBrokerCommand) -> Result<i32, LsbxError> {
    match action {
        CiBrokerCommand::Run { backend, queue_label } => {
            // Reuse build_deps()'s exact construction path for
            // backend/store/registry, the same way dispatch_mcp/dispatch_serve
            // do — see those functions' own doc comments for the general
            // pattern. `ci-broker run`'s own `--backend` flag (scoped to
            // `CiBrokerCommand::Run`, distinct from the top-level `Cli`'s
            // global `--backend`) is passed as `build_deps`'s
            // `backend_override` parameter, taking precedence over
            // whatever `args.backend` happens to hold (which is `None`
            // for this subcommand in practice, since a caller invoking
            // `lsbx ci-broker run --backend=X` has no reason to also pass
            // the top-level `--backend` flag — but the override parameter
            // makes the precedence explicit either way, rather than
            // requiring `Cli`/`Command` to derive `Clone` just to
            // reconstruct a synthetic top-level `Cli` value the way an
            // earlier draft of this function did).
            let deps = build_deps(args, Some(backend.clone())).await?;
            let state_dir = deps.state_dir.clone();
            let job_store = CiJobStore::new(state_dir);
            let ops = deps.into_ops();

            let queue_label_value = queue_label
                .clone()
                .or_else(|| std::env::var(QUEUE_LABEL_ENV).ok())
                .unwrap_or_else(|| lsbx_broker::poll::FALLBACK_QUEUE_LABEL.to_string());
            let poll_config = lsbx_broker::poll::PollConfig::from_queue_label_and_env(&queue_label_value);

            let github = build_github_client().await?;

            let broker_config = lsbx_broker::reconcile::BrokerConfig {
                poll: poll_config,
                lease: DEFAULT_CI_BROKER_LEASE,
            };

            lsbx_broker::reconcile::run_broker(&job_store, &ops, &github, broker_config, None).await?;
            Ok(0)
        }
    }
}

/// Resolves a real `lsbx_broker::github_client::GitHubClient`: the GitHub
/// App credential path when `LSBX_GITHUB_APP_ID`/`LSBX_GITHUB_APP_PRIVATE_KEY_PATH`
/// are both set, falling back to `GitHubClient::from_gh_cli_fallback()`
/// otherwise. See [`dispatch_ci_broker`]'s own doc comment for the full
/// design writeup.
async fn build_github_client() -> Result<lsbx_broker::github_client::GitHubClient, LsbxError> {
    let app_id = std::env::var(GITHUB_APP_ID_ENV).ok();
    let private_key_path = std::env::var(GITHUB_APP_PRIVATE_KEY_PATH_ENV).ok();

    match (app_id, private_key_path) {
        (Some(app_id_str), Some(key_path)) => {
            let app_id: u64 = app_id_str.parse().map_err(|_| {
                LsbxError::Usage(format!(
                    "{GITHUB_APP_ID_ENV} value '{app_id_str}' is not a valid u64"
                ))
            })?;
            let private_key_pem = std::fs::read_to_string(&key_path).map_err(|e| {
                LsbxError::BackendUnavailable(format!(
                    "failed to read {GITHUB_APP_PRIVATE_KEY_PATH_ENV} file '{key_path}': {e}"
                ))
            })?;
            let installation_id = std::env::var(GITHUB_APP_INSTALLATION_ID_ENV)
                .ok()
                .map(|s| {
                    s.parse::<u64>().map_err(|_| {
                        LsbxError::Usage(format!(
                            "{GITHUB_APP_INSTALLATION_ID_ENV} value '{s}' is not a valid u64"
                        ))
                    })
                })
                .transpose()?;
            let owner = std::env::var(GITHUB_APP_OWNER_ENV).map_err(|_| {
                LsbxError::Usage(format!(
                    "{GITHUB_APP_OWNER_ENV} is required when {GITHUB_APP_ID_ENV} and \
                     {GITHUB_APP_PRIVATE_KEY_PATH_ENV} are set (the installation-token \
                     exchange is scoped per-owner)"
                ))
            })?;

            let auth = lsbx_broker::auth::GitHubAppAuth::new(lsbx_broker::auth::GitHubAppConfig {
                app_id,
                private_key_pem,
                installation_id,
            })?;
            lsbx_broker::github_client::GitHubClient::from_app_auth(&auth, &owner).await
        }
        _ => Ok(lsbx_broker::github_client::GitHubClient::from_gh_cli_fallback()),
    }
}

fn parse_flavor(input: &str) -> Result<GoldenFlavor, LsbxError> {
    match input.to_lowercase().as_str() {
        "desktop" => Ok(GoldenFlavor::Desktop),
        "agent" => Ok(GoldenFlavor::Agent),
        "ci-runner" | "ci_runner" | "cirunner" => Ok(GoldenFlavor::CiRunner),
        other => Err(LsbxError::Usage(format!(
            "invalid --flavor '{other}' (expected desktop, agent, or ci-runner)"
        ))),
    }
}

fn parse_streaming(input: Option<&str>) -> StreamingMode {
    match input.map(str::to_lowercase).as_deref() {
        Some("novnc") => StreamingMode::Novnc,
        _ => StreamingMode::None,
    }
}

/// `--expired` filtering for `list`: a `PublicSandbox` carries
/// `lease_expires_at` as an `Option<String>` (RFC3339), never a typed
/// `Duration`/`SystemTime` — this mirrors `lsbx_lifecycle::lease::is_expired`'s
/// own fail-closed parsing (an absent or unparseable deadline is never
/// treated as expired) without needing that crate's private `Clock`
/// plumbing, since this filter only needs "is it in the past right now."
fn is_expired_public(sandbox: &PublicSandbox, now: std::time::SystemTime) -> bool {
    let Some(expires_at) = sandbox.lease_expires_at.as_deref() else {
        return false;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };
    let now: chrono::DateTime<chrono::Utc> = now.into();
    parsed.with_timezone(&chrono::Utc) < now
}

// ---------------------------------------------------------------------
// Formattable impls
//
// Every real LsbxOps response type this crate touches is routed through
// `format::render`/`format::render_result`. Types that are not themselves
// `Serialize` (StatusReport, ReapReport, HealthcheckResult, CommandOutput,
// BootstrapReport — none of the real `lsbx-ops`/`lsbx-lifecycle`/
// `lsbx-golden`/`lsbx-bootstrap` types derive it) are translated into a
// small local DTO defined here that does, rather than this crate reaching
// into those crates to add a derive they didn't ask for.
// ---------------------------------------------------------------------

impl Formattable for PublicSandbox {
    fn to_human_table(&self) -> String {
        format::kv_table(&[
            ("id", self.id.clone()),
            ("name", self.name.clone()),
            ("host", self.host.clone()),
            ("profile", self.profile.clone()),
            ("flavor", self.flavor.clone()),
            ("streaming", self.streaming.clone()),
            ("task_id", self.task_id.clone().unwrap_or_default()),
            ("created_at", self.created_at.clone().unwrap_or_default()),
            (
                "lease_expires_at",
                self.lease_expires_at.clone().unwrap_or_default(),
            ),
            (
                "console_url",
                self.console_url.clone().unwrap_or_default(),
            ),
            ("cleanup_failed", self.cleanup_failed.to_string()),
            ("repository", self.repository.clone().unwrap_or_default()),
        ])
    }
}

#[derive(Serialize)]
struct SandboxList(Vec<PublicSandbox>);

impl Formattable for SandboxList {
    fn to_human_table(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .0
            .iter()
            .map(|s| {
                vec![
                    s.id.clone(),
                    s.name.clone(),
                    s.profile.clone(),
                    s.streaming.clone(),
                    s.lease_expires_at.clone().unwrap_or_default(),
                ]
            })
            .collect();
        format::row_table(&["ID", "NAME", "PROFILE", "STREAMING", "LEASE_EXPIRES_AT"], &rows)
    }
}

#[derive(Serialize)]
struct StatusReportDto {
    backend_name: String,
    backend_available: bool,
    sandbox_count: usize,
}

impl From<StatusReport> for StatusReportDto {
    fn from(r: StatusReport) -> Self {
        Self {
            backend_name: r.backend_name,
            backend_available: r.backend_available,
            sandbox_count: r.sandbox_count,
        }
    }
}

impl Formattable for StatusReportDto {
    fn to_human_table(&self) -> String {
        format::kv_table(&[
            ("backend", self.backend_name.clone()),
            ("backend_available", self.backend_available.to_string()),
            ("sandbox_count", self.sandbox_count.to_string()),
        ])
    }
}

#[derive(Serialize)]
struct CommandOutputDto {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

impl CommandOutputDto {
    fn from(out: &lsbx_kernel::backend::CommandOutput) -> Self {
        Self {
            exit_code: out.exit_code,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

impl Formattable for CommandOutputDto {
    fn to_human_table(&self) -> String {
        let mut out = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        out
    }
}

#[derive(Serialize)]
struct ReapReportDto {
    destroyed: Vec<String>,
    would_destroy: Vec<String>,
    keys_reconciled: usize,
}

impl From<lsbx_lifecycle::reap::ReapReport> for ReapReportDto {
    fn from(r: lsbx_lifecycle::reap::ReapReport) -> Self {
        Self {
            destroyed: r.destroyed,
            would_destroy: r.would_destroy,
            keys_reconciled: r.keys_reconciled,
        }
    }
}

impl Formattable for ReapReportDto {
    fn to_human_table(&self) -> String {
        format::kv_table(&[
            ("destroyed", self.destroyed.join(", ")),
            ("would_destroy", self.would_destroy.join(", ")),
            ("keys_reconciled", self.keys_reconciled.to_string()),
        ])
    }
}

/// DTO for `lsbx_bootstrap::systemd::BootstrapReport` (Gap 3) — that type
/// derives no `Serialize`, matching every other non-`Serialize`
/// sibling-crate response type this crate already wraps in a local DTO
/// above.
#[derive(Serialize)]
struct BootstrapReportDto {
    actions_taken: Vec<String>,
    actions_would_take: Vec<String>,
}

impl From<lsbx_bootstrap::systemd::BootstrapReport> for BootstrapReportDto {
    fn from(r: lsbx_bootstrap::systemd::BootstrapReport) -> Self {
        Self {
            actions_taken: r.actions_taken,
            actions_would_take: r.actions_would_take,
        }
    }
}

impl Formattable for BootstrapReportDto {
    fn to_human_table(&self) -> String {
        let mut lines = Vec::new();
        if !self.actions_taken.is_empty() {
            lines.push("actions taken:".to_string());
            for action in &self.actions_taken {
                lines.push(format!("  - {action}"));
            }
        }
        if !self.actions_would_take.is_empty() {
            lines.push("actions that would be taken (--dry-run):".to_string());
            for action in &self.actions_would_take {
                lines.push(format!("  - {action}"));
            }
        }
        if lines.is_empty() {
            lines.push("(no actions)".to_string());
        }
        lines.join("\n")
    }
}

impl Formattable for GoldenConfig {
    fn to_human_table(&self) -> String {
        format::kv_table(&[
            ("key", self.key.clone()),
            ("base", self.base.clone()),
            ("cpu", self.cpu.to_string()),
            ("memory", self.memory.clone()),
            (
                "content_hash",
                self.content_hash.clone().unwrap_or_default(),
            ),
            ("description", self.description.clone()),
        ])
    }
}

#[derive(Serialize)]
struct GoldenList(Vec<GoldenConfig>);

impl Formattable for GoldenList {
    fn to_human_table(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .0
            .iter()
            .map(|g| {
                vec![
                    g.key.clone(),
                    g.base.clone(),
                    g.content_hash.clone().unwrap_or_default(),
                    g.description.clone(),
                ]
            })
            .collect();
        format::row_table(&["KEY", "BASE", "CONTENT_HASH", "DESCRIPTION"], &rows)
    }
}

#[derive(Serialize)]
struct GoldenBuildOutcomeDto {
    key: String,
    content_hash: Option<String>,
    build_vm_tag: Option<String>,
}

impl From<lsbx_golden::build::GoldenBuildOutcome> for GoldenBuildOutcomeDto {
    fn from(o: lsbx_golden::build::GoldenBuildOutcome) -> Self {
        Self {
            key: o.config.key,
            content_hash: o.config.content_hash,
            build_vm_tag: o.build_vm_tag,
        }
    }
}

impl Formattable for GoldenBuildOutcomeDto {
    fn to_human_table(&self) -> String {
        format::kv_table(&[
            ("key", self.key.clone()),
            (
                "content_hash",
                self.content_hash.clone().unwrap_or_default(),
            ),
            (
                "build_vm_tag",
                self.build_vm_tag.clone().unwrap_or_default(),
            ),
        ])
    }
}

#[derive(Serialize)]
struct HealthcheckResultDto {
    command: String,
    passed: bool,
    output: String,
}

impl From<lsbx_golden::verify::HealthcheckResult> for HealthcheckResultDto {
    fn from(r: lsbx_golden::verify::HealthcheckResult) -> Self {
        Self {
            command: r.command,
            passed: r.passed,
            output: r.output,
        }
    }
}

#[derive(Serialize)]
struct HealthcheckResultsDto(Vec<HealthcheckResultDto>);

impl Formattable for HealthcheckResultsDto {
    fn to_human_table(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .0
            .iter()
            .map(|r| {
                vec![
                    r.command.clone(),
                    if r.passed { "pass".to_string() } else { "fail".to_string() },
                ]
            })
            .collect();
        format::row_table(&["COMMAND", "RESULT"], &rows)
    }
}

#[derive(Serialize)]
struct ConsoleUrlDto(Option<String>);

impl Formattable for ConsoleUrlDto {
    fn to_human_table(&self) -> String {
        self.0.clone().unwrap_or_else(|| "(no console available)".to_string())
    }
}

#[derive(Serialize)]
struct EmptyList {
    message: &'static str,
}

impl Default for EmptyList {
    fn default() -> Self {
        Self {
            message: "no matching sandboxes",
        }
    }
}

impl Formattable for EmptyList {
    fn to_human_table(&self) -> String {
        format!("({})", self.message)
    }
}

#[derive(Serialize)]
struct DestroyedId(String);

impl Formattable for DestroyedId {
    fn to_human_table(&self) -> String {
        format!("destroyed {}", self.0)
    }
}

#[derive(Serialize)]
struct PutGetResult {
    id: String,
}

impl Formattable for PutGetResult {
    fn to_human_table(&self) -> String {
        format!("ok ({})", self.id)
    }
}

#[derive(Serialize)]
struct ProfilesDto {
    profiles: serde_json::Value,
    full: bool,
}

impl Formattable for ProfilesDto {
    fn to_human_table(&self) -> String {
        let names = self
            .profiles
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if names.is_empty() {
            "(no profiles registered)".to_string()
        } else if self.full {
            format!("{names}\n(--full has no additional detail beyond what this registry currently holds)")
        } else {
            names
        }
    }
}

#[derive(Serialize)]
struct ImagesDto(serde_json::Value);

impl Formattable for ImagesDto {
    fn to_human_table(&self) -> String {
        self.0
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| "(no images registered)".to_string())
    }
}

#[derive(Serialize)]
struct RegisteredGolden(String);

impl Formattable for RegisteredGolden {
    fn to_human_table(&self) -> String {
        format!("registered golden '{}'", self.0)
    }
}

#[derive(Serialize)]
struct DeletedGolden(String);

impl Formattable for DeletedGolden {
    fn to_human_table(&self) -> String {
        format!("deleted golden '{}'", self.0)
    }
}

#[derive(Serialize)]
struct LogLines(Vec<String>);

impl Formattable for LogLines {
    fn to_human_table(&self) -> String {
        if self.0.is_empty() {
            "(no log lines)".to_string()
        } else {
            self.0.join("\n")
        }
    }
}

#[derive(Serialize)]
struct ConfigShowDto(serde_json::Value);

impl Formattable for ConfigShowDto {
    fn to_human_table(&self) -> String {
        serde_json::to_string_pretty(&self.0).unwrap_or_else(|_| self.0.to_string())
    }
}

#[derive(Serialize)]
struct ConfigPathDto;

impl Formattable for ConfigPathDto {
    fn to_human_table(&self) -> String {
        "no merged crate owns a writable config file path yet".to_string()
    }
}
