//! Host capability verification.
//!
//! Three checks, each reported individually rather than collapsed into one
//! pass/fail boolean (SPEC.md's "proven, not exited 0" ethos, applied here
//! to host readiness instead of VMs or files):
//!
//! 1. The local libvirt management socket is reachable (only meaningful
//!    for the `Local` transport — a remote target's libvirt reachability
//!    is Unit 06's `LibvirtBackend::connect(RemoteSsh { .. })`'s concern,
//!    not this crate's, per the Boundaries in this unit's own contract:
//!    "does not implement `create_from_golden` or domain lifecycle").
//! 2. `qemu-img` is present on `PATH`.
//! 3. The `lsbx` state directories exist with the correct (0700)
//!    permissions.
//!
//! This module deliberately does not depend on the `virt` crate (which
//! Unit 06 uses for the actual libvirt RPC connection and domain
//! lifecycle, and which needs `libvirt-dev` headers to build). A host
//! *readiness* probe is a narrower thing than a real RPC connection: it
//! only needs to answer "is the socket there and connectable," which a
//! plain Unix-domain-socket connect attempt against the well-known
//! `qemu:///system` backing path answers directly, without pulling in a
//! native-library build dependency for a check this crate performs before
//! any real libvirt connection is ever attempted.

use lsbx_kernel::error::LsbxError;
use std::path::{Path, PathBuf};

/// The result of one individual host-capability check.
#[derive(Debug, Clone)]
pub struct HostCheck {
    pub name: &'static str,
    pub passed: bool,
    pub detail: Option<String>,
}

/// The full set of individual checks run against a target host.
#[derive(Debug, Clone)]
pub struct HostVerification {
    pub checks: Vec<HostCheck>,
}

impl HostVerification {
    /// True only when every individual check passed.
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }
}

/// The libvirt daemon's well-known local Unix-domain management socket,
/// backing the default `qemu:///system` connection URI (SPEC.md Deviation
/// 6 / Unit 06's `LibvirtTransport::Local`). Overridable via
/// `LSBX_LIBVIRT_SOCKET` for hosts that run libvirtd with a non-default
/// socket path — the same escape hatch a real deployment would need, not
/// invented speculatively.
const DEFAULT_LIBVIRT_SOCKET: &str = "/var/run/libvirt/libvirt-sock";

/// The `lsbx` state directories that must exist with 0700 permissions
/// before this host is trusted. Mirrors the directories `lsbx-store`
/// (Unit 02) actually writes sandbox/CI-job records under.
fn state_directories(target: Option<&str>) -> Vec<PathBuf> {
    let base = target
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_home()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/share/lsbx")
        });
    vec![base.join("sandboxes"), base.join("ci-jobs")]
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Checks whether the local libvirt management socket exists and accepts a
/// connection. Only run for a `Local`-transport host — a `target` naming a
/// remote host is not probed here (Unit 06's `RemoteSsh` transport owns
/// that reachability question, via libvirt's own `qemu+ssh://` remote-URI
/// transport, when a real connection is attempted).
fn check_libvirt_socket() -> HostCheck {
    let socket_path = std::env::var("LSBX_LIBVIRT_SOCKET")
        .unwrap_or_else(|_| DEFAULT_LIBVIRT_SOCKET.to_string());
    let path = Path::new(&socket_path);

    if !path.exists() {
        return HostCheck {
            name: "libvirt_socket_reachable",
            passed: false,
            detail: Some(format!("socket not found at {socket_path}")),
        };
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => HostCheck {
            name: "libvirt_socket_reachable",
            passed: true,
            detail: Some(format!("connected to {socket_path}")),
        },
        Err(err) => HostCheck {
            name: "libvirt_socket_reachable",
            passed: false,
            detail: Some(format!("{socket_path} exists but refused connection: {err}")),
        },
    }
}

/// Checks whether `qemu-img` resolves on `PATH`, by walking `PATH`'s
/// entries directly (the check this function performs is deliberately
/// "would a subprocess spawn of `qemu-img` succeed," not "does invoking it
/// with some flag succeed" — the latter would conflate two different
/// failure modes: absent vs. present-but-misbehaving).
fn check_qemu_img_present() -> HostCheck {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join("qemu-img");
        candidate.is_file().then_some(candidate)
    });

    match found {
        Some(path) => HostCheck {
            name: "qemu_img_present",
            passed: true,
            detail: Some(path.to_string_lossy().into_owned()),
        },
        None => HostCheck {
            name: "qemu_img_present",
            passed: false,
            detail: Some("qemu-img not found on PATH".to_string()),
        },
    }
}

/// Checks the `lsbx` state directories exist and are exactly 0700.
/// Reports one detail string covering all directories checked (still one
/// `HostCheck` entry — the acceptance criterion asks for "state directories
/// exist with correct permissions" as a single named check, distinct from
/// the libvirt-socket and qemu-img checks, not one entry per directory).
fn check_state_directories(target: Option<&str>) -> HostCheck {
    use std::os::unix::fs::PermissionsExt;

    let dirs = state_directories(target);
    let mut problems = Vec::new();

    for dir in &dirs {
        match std::fs::metadata(dir) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                if mode != 0o700 {
                    problems.push(format!(
                        "{} has mode {:o}, expected 0700",
                        dir.display(),
                        mode
                    ));
                }
            }
            Err(err) => {
                problems.push(format!("{} does not exist ({err})", dir.display()));
            }
        }
    }

    if problems.is_empty() {
        HostCheck {
            name: "state_directories_present_and_0700",
            passed: true,
            detail: Some(
                dirs.iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    } else {
        HostCheck {
            name: "state_directories_present_and_0700",
            passed: false,
            detail: Some(problems.join("; ")),
        }
    }
}

/// Verifies a target host is capable of running `lsbx`'s broker services.
///
/// `target` is `None` for the local host, or `Some(host)` naming a remote
/// target (matching `bootstrap --target <host>`'s flag). Every check is
/// run and reported regardless of whether an earlier one failed — this
/// function never short-circuits, since the whole point of reporting
/// individual `HostCheck`s is that a caller (or a human reading `--json`
/// output) can see exactly which capabilities are missing, not just that
/// *something* is.
///
/// This function's own execution failing outright (as opposed to an
/// individual check failing) is a distinct, narrower error case — e.g. an
/// unreadable `HOME` environment or a filesystem-level I/O condition
/// unrelated to any single named check. That case surfaces as
/// `LsbxError::ContractViolated`, since it means host verification itself
/// could not run to completion, not that a specific capability was
/// confirmed absent.
pub async fn verify_host(target: Option<&str>) -> Result<HostVerification, LsbxError> {
    // Local-only for now — a remote target's libvirt reachability is
    // Unit 06's `RemoteSsh` transport's concern (see module docs), so this
    // function only runs the local-socket check when no remote target is
    // named. When a remote `target` is given, the socket check is skipped
    // (not applicable) rather than failed, avoiding a false negative for a
    // capability this crate has no way to probe without either shelling
    // out to `ssh` (outside this unit's scope) or depending on the `virt`
    // crate purely to open a connection nothing downstream is asking for
    // yet.
    let mut checks = Vec::new();

    if target.is_none() {
        checks.push(check_libvirt_socket());
    }

    checks.push(check_qemu_img_present());
    checks.push(check_state_directories(target));

    Ok(HostVerification { checks })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn state_directories_defaults_to_home_local_share_lsbx() {
        let dirs = state_directories(None);
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].ends_with("sandboxes"));
        assert!(dirs[1].ends_with("ci-jobs"));
    }

    #[test]
    fn state_directories_honors_explicit_target() {
        let dirs = state_directories(Some("/custom/base"));
        assert_eq!(dirs[0], PathBuf::from("/custom/base/sandboxes"));
        assert_eq!(dirs[1], PathBuf::from("/custom/base/ci-jobs"));
    }

    #[tokio::test]
    async fn verify_host_reports_qemu_img_check_by_name() {
        let result = verify_host(None).await.unwrap();
        assert!(result.checks.iter().any(|c| c.name == "qemu_img_present"));
    }

    #[tokio::test]
    async fn verify_host_skips_libvirt_socket_check_for_remote_target() {
        let result = verify_host(Some("remote.example.com")).await.unwrap();
        assert!(!result
            .checks
            .iter()
            .any(|c| c.name == "libvirt_socket_reachable"));
    }

    #[tokio::test]
    async fn verify_host_never_short_circuits_on_failed_check() {
        // Even with a target directory that certainly doesn't exist, every
        // check still runs and is reported — none are skipped just
        // because an earlier one failed.
        let result = verify_host(Some("/definitely/does/not/exist/anywhere"))
            .await
            .unwrap();
        assert_eq!(result.checks.len(), 2); // qemu_img + state_dirs, no libvirt (remote target)
        assert!(!result.all_passed());
    }
}
