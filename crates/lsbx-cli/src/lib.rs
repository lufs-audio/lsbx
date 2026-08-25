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
//! with this one). `lsbx-tui` is not on `main` yet at the time this crate
//! was written — this crate does not add a dependency on an unmerged
//! crate, so the bare-invocation path below calls `status` directly and
//! renders it through the same one formatting path every other subcommand
//! uses, regardless of TTY-ness, with a `// TODO` marking exactly where the
//! real handoff belongs once Unit 12 lands.

pub mod cli;
pub mod format;

use clap::Parser as _;
use cli::{BackendChoice, Cli, Command, GoldenCommand};
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

    let deps = match build_deps(&args).await {
        Ok(deps) => deps,
        Err(e) => {
            println!("{}", format::render_error(&e, as_json));
            return e.exit_code() as i32;
        }
    };

    let ops = deps.into_ops();

    match dispatch(&ops, &args, as_json).await {
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

/// Everything `LsbxOps::new` needs, resolved from parsed args.
struct OpsDeps {
    backend: Box<dyn Backend>,
    backend_name: String,
    sandbox_store: SandboxStore,
    ci_job_store: CiJobStore,
    registry: ImageRegistry,
    clock: Box<dyn Clock>,
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
async fn build_deps(args: &Cli) -> Result<OpsDeps, LsbxError> {
    let state_dir = resolve_state_dir(args.state_dir.as_deref());
    let images_path = resolve_images_path(args.images.as_deref(), &state_dir);

    // `--config` is accepted (interface-contract parity with the existing
    // CLI surface) but no merged crate defines a real config-file schema
    // yet (see `lsbx-ops::config_show`'s own honest-gap note) — there is
    // nothing to load from it today beyond what `--state-dir`/`--images`
    // already cover. Recorded so a future config loader has an obvious
    // seam to land in rather than this crate silently ignoring the flag.
    let _config_path = args.config.clone();

    let backend_choice = args.backend.clone().unwrap_or(BackendChoice::Demo);
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
async fn dispatch(ops: &LsbxOps, args: &Cli, as_json: bool) -> Result<i32, LsbxError> {
    match &args.command {
        None => {
            // TODO: hand off to lsbx-tui once merged (Unit 12's dashboard,
            // bare `lsbx` when stdout is a TTY — SPEC.md §4.8, this unit's
            // own Boundaries). lsbx-tui is not on `main` yet at the time
            // this crate was written, so this deliberately does not add a
            // dependency on an unmerged crate; falling back to `status`
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
        Some(Command::Serve { .. }) => {
            // Boundaries: this unit does not implement serve's HTTP server
            // (Unit 13) — only constructs and hands off. Unit 13's crate
            // (`lsbx-gateway`) is not a dependency of this crate for the
            // same reason `lsbx-tui` isn't (avoid a hard dependency on an
            // unmerged crate); this subcommand is accepted for
            // interface-contract parity and reports the real gap rather
            // than pretending to have started a server.
            Err(LsbxError::ContractViolated(
                "serve is not implemented in this unit — Unit 13 (lsbx-gateway) owns the \
                 HTTP server; this subcommand exists on the CLI surface for parity but has \
                 nothing to construct and hand off to until that crate is merged"
                    .to_string(),
            ))
        }
        Some(Command::Bootstrap { .. }) => {
            // Same boundary as `serve`, for Unit 19 (`lsbx-bootstrap`).
            Err(LsbxError::ContractViolated(
                "bootstrap is not implemented in this unit — Unit 19 (lsbx-bootstrap) owns \
                 host verification and golden flattening; this subcommand exists on the CLI \
                 surface for parity but has nothing to construct and hand off to until that \
                 crate is merged"
                    .to_string(),
            ))
        }
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
        Some(Command::Mcp) => {
            // Boundaries: this unit does not implement mcp's MCP server
            // (Unit 15) — same shape of gap as `serve`/`bootstrap` above.
            Err(LsbxError::ContractViolated(
                "mcp is not implemented in this unit — Unit 15 (lsbx-mcp) owns the stdio MCP \
                 server; this subcommand exists on the CLI surface for parity but has nothing \
                 to construct and hand off to until that crate is merged"
                    .to_string(),
            ))
        }
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
                // this exists afterward, whatever was there before" rather
                // than "the delete step itself must succeed."
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
// `Serialize` (StatusReport, ReapReport, HealthcheckResult, CommandOutput —
// none of the real `lsbx-ops`/`lsbx-lifecycle`/`lsbx-golden` types derive
// it) are translated into a small local DTO defined here that does, rather
// than this crate reaching into those crates to add a derive they didn't
// ask for.
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
