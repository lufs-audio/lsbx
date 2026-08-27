//! Systemd unit generation and host bootstrap orchestration.
//!
//! Owns `SystemdUnitSpec`/`generate_broker_units`/`BootstrapConfig`/
//! `BootstrapReport`/`bootstrap` together, per the unit contract's own
//! module layout.
//!
//! ## A real gap this module generates content for, but does not close
//! Every `ExecStart` line this module generates
//! (`/usr/local/bin/lsbx ci-broker run --backend=<...>`) names a
//! subcommand that **does not exist anywhere in this codebase yet**. As of
//! `feature-unit-11-cli-surface-and-output-formatting` (PR #17, not yet
//! merged to `main`), `lsbx-cli`'s `Command` enum has no `CiBroker`/
//! `ci-broker` variant at all — only a top-level `Bootstrap { .. }`
//! variant this unit's own CLI wiring will eventually attach to. Units
//! 16–18 (the broker crate, also unmerged) only built an async library
//! entry point, `lsbx_broker::reconcile::run_broker(..)` — never a CLI
//! wrapper around it.
//!
//! This module still generates the unit content as intended/designed —
//! that is what a real deployment will eventually need, and inventing a
//! plausible-looking `ExecStart` line is the right shape for the artifact
//! even though the binary it names can't yet honor it. What this module
//! does *not* do is pretend the gap is closed: **wiring `lsbx ci-broker
//! run` as a real subcommand that calls `lsbx_broker::run_broker` is
//! unassigned by any of Units 16–19's contracts and must happen during
//! final integration**, before these generated unit files are installed
//! on a real host and started. See this crate's PR description for the
//! same note in a more visible place.

use lsbx_kernel::error::LsbxError;
use std::path::PathBuf;

/// One generated systemd unit — file *content*, one field. Deliberately
/// does not carry a `.service` suffix in `name` (see the doc comment on
/// [`generate_broker_units`] for why): the acceptance criterion's own
/// literal text is `"lsbx-ci-broker"`, `"lsbx-ci-broker-exe"`, with no
/// suffix. The suffix is appended only when a name is turned into a unit
/// *file path* — see [`unit_file_path`].
#[derive(Debug, Clone)]
pub struct SystemdUnitSpec {
    pub name: &'static str,
    pub content: String,
}

/// Where a given unit's file would live on the target host. The `.service`
/// suffix is appended here, and only here — never baked into
/// `SystemdUnitSpec::name` itself, so a caller that needs the bare name
/// (for `systemctl enable lsbx-ci-broker`, for instance, which accepts
/// either form but for which the bare unit name is the more natural
/// argument) doesn't have to strip a suffix back off first.
fn unit_file_path(unit_dir: &std::path::Path, name: &str) -> PathBuf {
    unit_dir.join(format!("{name}.service"))
}

const SYSTEMD_UNIT_DIR: &str = "/etc/systemd/system";

/// Where systemd unit files are actually written. Real deployments always
/// use [`SYSTEMD_UNIT_DIR`]; `LSBX_SYSTEMD_UNIT_DIR` is the same kind of
/// override `LSBX_LIBVIRT_SOCKET` is in `verify_host` — an escape hatch a
/// containerized or test environment genuinely needs (this crate's own
/// `#[cfg(test)]` suite has no business writing into `/etc` on whatever
/// host runs `cargo test`), not a speculative knob invented for this unit.
fn systemd_unit_dir() -> PathBuf {
    std::env::var_os("LSBX_SYSTEMD_UNIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(SYSTEMD_UNIT_DIR))
}

/// Generates the systemd unit specs for both broker services.
///
/// Names preserved exactly, per the unit contract and the existing
/// `AGENTS.md`: `"lsbx-ci-broker"` (Carnyx/libvirt) and
/// `"lsbx-ci-broker-exe"` (Molimo/exedev) — bare names, no `.service`
/// suffix baked in (see [`SystemdUnitSpec`]'s doc comment: the acceptance
/// criterion's own literal text has no suffix, and appending it only at
/// file-path-construction time means a caller wanting the bare name for
/// `systemctl` never has to strip one back off).
pub fn generate_broker_units(config: &BootstrapConfig) -> Vec<SystemdUnitSpec> {
    vec![
        SystemdUnitSpec {
            name: "lsbx-ci-broker",
            content: render_unit(
                config,
                "lsbx CI broker (Carnyx / local+remote libvirt backend)",
                "libvirt",
            ),
        },
        SystemdUnitSpec {
            name: "lsbx-ci-broker-exe",
            content: render_unit(config, "lsbx CI broker (Molimo / exedev backend)", "exedev"),
        },
    ]
}

fn service_user(base: &std::path::Path) -> String {
    if let Ok(user) = std::env::var("LSBX_SERVICE_USER") {
        if !user.is_empty() {
            return user;
        }
    }

    let mut components = base.components();
    if components.next().is_some()
        && components.next().and_then(|c| c.as_os_str().to_str()) == Some("home")
    {
        if let Some(user) = components.next().and_then(|c| c.as_os_str().to_str()) {
            return user.to_string();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "lsbx".to_string())
}

fn render_unit(config: &BootstrapConfig, description: &str, backend: &str) -> String {
    let base = state_base(config.target.as_deref());
    let base_display = base.display().to_string();
    let user = service_user(&base);
    let home = format!("/home/{user}");
    let working_directory = std::env::var("LSBX_WORKING_DIRECTORY")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{home}/repos/lsbx"));
    let binary = std::env::var("LSBX_BIN")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/bin/lsbx".to_string());
    let images = std::env::var("LSBX_IMAGES_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{working_directory}/images.json"));
    let environment_file = std::env::var("LSBX_BROKER_ENV_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| base.join("broker.env").display().to_string());

    format!(
        "[Unit]\n\
         Description={description}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         User={user}\n\
         Group={user}\n\
         WorkingDirectory={working_directory}\n\
         Environment=HOME={home}\n\
         Environment=PATH={home}/.local/bin:/usr/local/bin:/usr/bin:/bin\n\
         Environment=LSBX_STATE_DIR={base_display}\n\
         EnvironmentFile=-{environment_file}\n\
         ExecStart={binary} --images={images} --state-dir={base_display} ci-broker run --backend={backend}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         NoNewPrivileges=true\n\
         PrivateTmp=true\n\
         ProtectSystem=full\n\
         ReadWritePaths={base_display}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Configuration for a single `bootstrap` invocation, matching the
/// existing `lsbx bootstrap [--target --no-services --no-verify --force
/// --dry-run]` flag surface exactly.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub target: Option<String>,
    /// `false` when `--no-services` is passed.
    pub install_services: bool,
    /// `false` when `--no-verify` is passed.
    pub verify: bool,
    pub force: bool,
    pub dry_run: bool,
}

/// What a `bootstrap` run did (or, under `--dry-run`, would do).
#[derive(Debug)]
pub struct BootstrapReport {
    pub actions_taken: Vec<String>,
    pub actions_would_take: Vec<String>,
}

/// The marker file `bootstrap` writes into the state-directory base once a
/// host has been successfully bootstrapped — its presence is exactly the
/// "already-bootstrapped host" condition the idempotency acceptance
/// criterion is about. Its content is unused; only presence/absence
/// matters.
const BOOTSTRAP_MARKER_FILENAME: &str = ".lsbx-bootstrapped";

fn state_base(target: Option<&str>) -> PathBuf {
    target.map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".local/share/lsbx")
    })
}

/// Runs (or, under `--dry-run`, previews) host bootstrap: verifying host
/// capability (unless `--no-verify`), creating the `lsbx` state
/// directories with 0700 permissions, and generating + installing the
/// broker systemd units (unless `--no-services`).
///
/// ## Idempotency semantics (see this unit's own acceptance criterion,
/// read literally)
/// The acceptance criterion says `--force` re-runs bootstrap idempotently
/// on an already-bootstrapped host "without erroring on already-exists
/// conditions" — which only makes sense if, *without* `--force`, an
/// already-bootstrapped host normally *does* error on an already-exists
/// condition. This function implements exactly that three-way behavior:
///
/// - A **fresh** host (no `.lsbx-bootstrapped` marker under the state
///   base): bootstrap succeeds regardless of `force`, creating everything
///   needed and writing the marker.
/// - An **already-bootstrapped** host with `force: true`: bootstrap
///   succeeds again, idempotently — directories that already exist with
///   correct permissions and unit files that already exist with identical
///   content are reported as "already exists" / left alone; unit files
///   whose content differs from what would be generated now are
///   overwritten and reported as "overwrote". No "already exists"
///   condition is treated as an error.
/// - An **already-bootstrapped** host with `force: false`: bootstrap
///   returns `Err(LsbxError::ContractViolated(..))` without writing
///   anything — the whole point of `--force` is to skip exactly this
///   error, so it must exist for `--force` to have anything to avoid.
///
/// Permission correctness is checked by delegating to
/// [`crate::verify_host::verify_host`] rather than re-implementing a
/// second, separate comparison here — `verify_host()` already reports a
/// `state_directories_present_and_0700` check with the exact same 0700
/// expectation this function needs, so this function reuses that check's
/// output instead of duplicating the `PermissionsExt` logic inline.
pub async fn bootstrap(config: BootstrapConfig) -> Result<BootstrapReport, LsbxError> {
    let base = state_base(config.target.as_deref());
    let marker = base.join(BOOTSTRAP_MARKER_FILENAME);
    let already_bootstrapped = marker.exists();

    if already_bootstrapped && !config.force && !config.dry_run {
        return Err(LsbxError::ContractViolated(format!(
            "host at {} is already bootstrapped; pass --force to re-run idempotently",
            base.display()
        )));
    }

    let mut actions_taken = Vec::new();
    let mut actions_would_take = Vec::new();

    // --- 1. State directories -------------------------------------------
    let dirs = vec![base.join("sandboxes"), base.join("ci-jobs")];
    for dir in &dirs {
        if dir.exists() {
            if config.dry_run {
                actions_would_take.push(format!("verify permissions on {}", dir.display()));
            } else {
                actions_taken.push(format!("directory already exists: {}", dir.display()));
            }
        } else if config.dry_run {
            actions_would_take.push(format!("create directory {} (mode 0700)", dir.display()));
        } else {
            create_dir_0700(dir)?;
            actions_taken.push(format!("created directory {} (mode 0700)", dir.display()));
        }
    }

    // --- 2. Host verification (unless --no-verify) ----------------------
    if config.verify {
        if config.dry_run {
            actions_would_take.push(
                "verify host capability (libvirt socket, qemu-img, state dir permissions)"
                    .to_string(),
            );
        } else {
            // Reuse verify_host()'s own permission check rather than a
            // second, separately-implemented comparison — see this
            // function's doc comment.
            let verification = crate::verify_host::verify_host(config.target.as_deref()).await?;
            let failed: Vec<&str> = verification
                .checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| c.name)
                .collect();
            if !failed.is_empty() {
                return Err(LsbxError::ContractViolated(format!(
                    "host verification failed: {}",
                    failed.join(", ")
                )));
            }
            actions_taken.push(format!(
                "host verification passed ({} checks)",
                verification.checks.len()
            ));
        }
    }

    // --- 3. Systemd units (unless --no-services) -------------------------
    if config.install_services {
        let unit_dir = systemd_unit_dir();
        for unit in generate_broker_units(&config) {
            let file_path = unit_file_path(&unit_dir, unit.name);

            if config.dry_run {
                actions_would_take.push(format!("write systemd unit file {}", file_path.display()));
                continue;
            }

            match std::fs::read_to_string(&file_path) {
                Ok(existing) if existing == unit.content => {
                    actions_taken.push(format!(
                        "unit file already exists (unchanged): {}",
                        file_path.display()
                    ));
                }
                Ok(_) => {
                    write_unit_file(&file_path, &unit.content)?;
                    actions_taken.push(format!("overwrote unit file: {}", file_path.display()));
                }
                Err(_) => {
                    write_unit_file(&file_path, &unit.content)?;
                    actions_taken.push(format!("wrote unit file: {}", file_path.display()));
                }
            }
        }
    }

    // --- 4. Marker (unless --dry-run) ------------------------------------
    if config.dry_run {
        actions_would_take.push(format!("write bootstrap marker {}", marker.display()));
    } else {
        std::fs::write(&marker, b"").map_err(|err| {
            LsbxError::ContractViolated(format!(
                "failed to write bootstrap marker {}: {err}",
                marker.display()
            ))
        })?;
        actions_taken.push(format!("wrote bootstrap marker {}", marker.display()));
    }

    Ok(BootstrapReport {
        actions_taken,
        actions_would_take,
    })
}

fn create_dir_0700(dir: &std::path::Path) -> Result<(), LsbxError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).map_err(|err| {
        LsbxError::ContractViolated(format!("failed to create {}: {err}", dir.display()))
    })?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|err| {
        LsbxError::ContractViolated(format!(
            "failed to set 0700 permissions on {}: {err}",
            dir.display()
        ))
    })?;
    Ok(())
}

fn write_unit_file(path: &std::path::Path, content: &str) -> Result<(), LsbxError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            LsbxError::ContractViolated(format!(
                "failed to create unit directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, content).map_err(|err| {
        LsbxError::ContractViolated(format!(
            "failed to write unit file {}: {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn default_config(target: Option<String>) -> BootstrapConfig {
        BootstrapConfig {
            target,
            install_services: true,
            verify: true,
            force: false,
            dry_run: false,
        }
    }

    #[test]
    fn unit_names_have_no_service_suffix() {
        let config = default_config(None);
        let units = generate_broker_units(&config);
        assert_eq!(units[0].name, "lsbx-ci-broker");
        assert_eq!(units[1].name, "lsbx-ci-broker-exe");
        assert!(!units[0].name.ends_with(".service"));
        assert!(!units[1].name.ends_with(".service"));
    }

    #[test]
    fn unit_file_path_appends_service_suffix() {
        let dir = std::path::Path::new("/etc/systemd/system");
        assert_eq!(
            unit_file_path(dir, "lsbx-ci-broker"),
            std::path::PathBuf::from("/etc/systemd/system/lsbx-ci-broker.service")
        );
    }

    #[test]
    fn carnyx_unit_uses_explicit_libvirt_backend_flag() {
        let config = default_config(None);
        let units = generate_broker_units(&config);
        let carnyx = units.iter().find(|u| u.name == "lsbx-ci-broker").unwrap();
        assert!(carnyx
            .content
            .contains("ExecStart=/usr/local/bin/lsbx --images="));
        assert!(carnyx.content.contains("ci-broker run --backend=libvirt"));
        assert!(carnyx.content.contains("User="));
        assert!(carnyx.content.contains("WorkingDirectory="));
    }

    #[test]
    fn molimo_unit_uses_targeted_deployment_paths_and_hardening() {
        let config = BootstrapConfig {
            target: Some("/home/exedev/lsbx-state".to_string()),
            install_services: true,
            verify: false,
            force: false,
            dry_run: true,
        };
        let molimo = generate_broker_units(&config)
            .into_iter()
            .find(|unit| unit.name == "lsbx-ci-broker-exe")
            .unwrap();
        assert!(molimo.content.contains("User=exedev"));
        assert!(molimo
            .content
            .contains("WorkingDirectory=/home/exedev/repos/lsbx"));
        assert!(molimo
            .content
            .contains("--images=/home/exedev/repos/lsbx/images.json"));
        assert!(molimo
            .content
            .contains("--state-dir=/home/exedev/lsbx-state"));
        assert!(molimo
            .content
            .contains("EnvironmentFile=-/home/exedev/lsbx-state/broker.env"));
        assert!(molimo.content.contains("ProtectSystem=full"));
        assert!(molimo
            .content
            .contains("ReadWritePaths=/home/exedev/lsbx-state"));
    }
}
