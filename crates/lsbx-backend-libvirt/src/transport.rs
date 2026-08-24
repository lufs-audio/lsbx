//! Transport selection for [`crate::LibvirtBackend`] (SPEC.md Deviation 6:
//! one `Backend` implementation parameterized by transport, not two).
//!
//! ## The architectural correction this module encodes
//!
//! The unit contract's own interface-contract snippet describes the remote
//! case as connecting "over SSH via `russh`". That framing conflates two
//! genuinely different kinds of "SSH":
//!
//! 1. Reaching a *remote libvirt host's management socket* — this is a
//!    libvirt RPC channel, and libvirt's own C library already knows how to
//!    tunnel that RPC over SSH itself, given a `qemu+ssh://` connection URI.
//!    `virt::connect::Connect::open` drives that whole transport internally;
//!    there is no supported way to hand libvirt an externally-driven
//!    channel (a `russh` session, for instance) to carry its RPC traffic
//!    instead. Hand-rolling an SSH session here and trying to bridge it into
//!    `Connect::open` would not work — libvirt's RPC framing is not a
//!    generic byte-stream protocol you can shim a transport under from the
//!    outside.
//! 2. Reaching *inside a guest VM* to run a command (`Backend::run` /
//!    `put_file` / `get_file`) — this is a completely different SSH
//!    session, to the guest's own sshd, independent of whether the
//!    hypervisor managing that guest is local or remote. This is real,
//!    hand-rolled SSH client work, and it is what the unit's batch-mode /
//!    stdin-isolation acceptance criterion is actually about. See
//!    `crate::guest_ssh` for that half.
//!
//! This module only builds the connection URI for case 1. It deliberately
//! contains no SSH client code at all — `LibvirtBackend::connect` passes the
//! resulting URI straight to `Connect::open`, and libvirt's own `ssh`
//! transport driver (compiled into every libvirt client, driven by its
//! remote-URI parser) does the rest.

/// How to reach the libvirt management socket for this backend instance.
///
/// Mirrors the unit contract's interface contract exactly — this is the
/// existing `ssh_target`/`ProxyJump` design from the current Python
/// `libvirt.py`, expressed as a Rust enum rather than a mode flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibvirtTransport {
    /// The local libvirt socket. `uri` overrides the default
    /// `qemu:///system`; `None` uses the default.
    Local { uri: Option<String> },
    /// A remote libvirt host, reached via libvirt's native `qemu+ssh://`
    /// remote-URI transport (see module docs above — NOT a hand-rolled
    /// `russh` session driving the RPC channel itself).
    RemoteSsh {
        /// `user@host` or bare `host` to connect to.
        host: String,
        /// Private key libvirt's internal SSH client should present.
        /// Passed through to the URI's `keyfile=` query parameter.
        ssh_key_path: std::path::PathBuf,
        /// An optional jump/bastion host. See
        /// [`LibvirtTransport::to_connect_uri`] for the honest limitation
        /// on how this is currently expressed.
        jump_host: Option<String>,
        /// Overrides the driver+path portion of the URI (default
        /// `qemu:///system` semantics, i.e. `system` mode on the remote
        /// host). Expected shape: something like `"qemu:///system"` — this
        /// module splits it into the `driver+transport://host/path` shape
        /// libvirt's remote URI syntax expects.
        uri: Option<String>,
    },
}

/// The driver+path libvirt uses for `qemu:///system`-style local access,
/// reused as the default remote path segment too (matches the existing
/// Python `libvirt.py`'s default of managing the system-wide QEMU/KVM
/// driver, not the per-user session driver).
const DEFAULT_LOCAL_URI: &str = "qemu:///system";
const DEFAULT_REMOTE_PATH: &str = "/system";

impl LibvirtTransport {
    /// Builds the exact URI string to hand to `virt::connect::Connect::open`.
    ///
    /// For `Local`, this is just the configured override or
    /// `qemu:///system` — no SSH involved at all.
    ///
    /// For `RemoteSsh`, this builds a `qemu+ssh://` URI per libvirt's own
    /// remote-URI documentation
    /// (<https://libvirt.org/uri.html#remote-uris>):
    /// `qemu+ssh://[user@]host[:port]/system?keyfile=/path/to/key`.
    ///
    /// ### Known limitation: `jump_host`
    /// libvirt's remote-URI query parameters accept a `command=` override
    /// for the transport helper it shells out to (default: the system
    /// `ssh` binary) and a `sshauth=` parameter for the auth methods to try,
    /// but there is no documented, stable query-parameter spelling for
    /// "add `-o ProxyJump=<jump host>` to that invocation" as of libvirt's
    /// published remote-URI reference. `command=ssh -o ProxyJump=...` is
    /// the shape that *would* work if libvirt's URI parser tokenizes
    /// `command`'s value as a full argv the way its docs suggest for
    /// overriding the transport binary, but this has not been verified
    /// against a real remote libvirt host with a jump host in this
    /// environment (no libvirt daemon is reachable here — see the crate's
    /// `#[ignore]`d conformance test). Rather than guess at unverified URI
    /// syntax and silently produce a URI that might not actually route
    /// through the jump host, this function takes the conservative,
    /// honest path: when `jump_host` is set, it appends
    /// `command=ssh%20-o%20ProxyJump%3D<jump_host>` to the query string
    /// (percent-encoded, single `command=` value, consistent with libvirt's
    /// documented pattern for overriding the ssh helper's invocation) and
    /// returns `Ok`, but this specific combination is flagged in the PR as
    /// an unverified, best-effort construction — a real remote host with a
    /// jump host is required to confirm it actually works, and this is
    /// tracked as an open gap rather than a settled fact.
    pub fn to_connect_uri(&self) -> String {
        match self {
            LibvirtTransport::Local { uri } => {
                uri.clone().unwrap_or_else(|| DEFAULT_LOCAL_URI.to_string())
            }
            LibvirtTransport::RemoteSsh {
                host,
                ssh_key_path,
                jump_host,
                uri,
            } => {
                let path = uri
                    .as_deref()
                    .and_then(|u| u.rsplit_once("///"))
                    .map(|(_, path)| format!("/{}", path.trim_start_matches('/')))
                    .unwrap_or_else(|| DEFAULT_REMOTE_PATH.to_string());

                let mut query = vec![format!(
                    "keyfile={}",
                    urlencode(&ssh_key_path.to_string_lossy())
                )];

                if let Some(jump) = jump_host {
                    // See doc comment above: best-effort, unverified against
                    // a real host. `ssh -o ProxyJump=<jump>` is what a plain
                    // shell invocation would need; we percent-encode the
                    // whole helper command as one `command=` value per
                    // libvirt's documented override mechanism for the
                    // transport helper binary+args.
                    query.push(format!(
                        "command={}",
                        urlencode(&format!("ssh -o ProxyJump={jump}"))
                    ));
                }

                format!("qemu+ssh://{host}{path}?{}", query.join("&"))
            }
        }
    }
}

/// Minimal percent-encoding for the URI query-parameter values this module
/// builds (key file paths, jump-host ssh option strings). Only the
/// characters that are unsafe inside a URI query component are escaped;
/// this is intentionally narrow rather than a general-purpose URL encoder,
/// since the inputs here are always filesystem paths or simple `ssh -o ...`
/// option strings, never arbitrary user text.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn local_default_uri_is_qemu_system() {
        let t = LibvirtTransport::Local { uri: None };
        assert_eq!(t.to_connect_uri(), "qemu:///system");
    }

    #[test]
    fn local_override_uri_is_used_verbatim() {
        let t = LibvirtTransport::Local {
            uri: Some("qemu:///session".to_string()),
        };
        assert_eq!(t.to_connect_uri(), "qemu:///session");
    }

    #[test]
    fn remote_ssh_builds_qemu_ssh_uri_with_keyfile() {
        let t = LibvirtTransport::RemoteSsh {
            host: "vmhost.example.com".to_string(),
            ssh_key_path: std::path::PathBuf::from("/home/ops/.ssh/lsbx_key"),
            jump_host: None,
            uri: None,
        };
        let uri = t.to_connect_uri();
        assert!(uri.starts_with("qemu+ssh://vmhost.example.com/system?"));
        // `/` is left unescaped by `urlencode` on purpose (it's valid
        // unencoded inside a URI query-component value per RFC 3986, and
        // an unescaped path reads far more legibly in a connection URI a
        // human might log or paste) — only genuinely unsafe characters get
        // percent-encoded, which this asserts by checking the path is
        // present verbatim rather than expecting a fully escaped form.
        assert!(uri.contains("keyfile=/home/ops/.ssh/lsbx_key"));
        assert!(!uri.contains("command="));
    }

    #[test]
    fn remote_ssh_with_user_at_host_is_preserved() {
        let t = LibvirtTransport::RemoteSsh {
            host: "ops@vmhost.example.com".to_string(),
            ssh_key_path: std::path::PathBuf::from("/keys/id_ed25519"),
            jump_host: None,
            uri: None,
        };
        let uri = t.to_connect_uri();
        assert!(uri.starts_with("qemu+ssh://ops@vmhost.example.com/system?"));
    }

    #[test]
    fn remote_ssh_with_jump_host_appends_command_param() {
        let t = LibvirtTransport::RemoteSsh {
            host: "vmhost.example.com".to_string(),
            ssh_key_path: std::path::PathBuf::from("/keys/id_ed25519"),
            jump_host: Some("bastion.example.com".to_string()),
            uri: None,
        };
        let uri = t.to_connect_uri();
        assert!(uri.contains("command=ssh%20-o%20ProxyJump%3Dbastion.example.com"));
    }

    #[test]
    fn remote_ssh_uri_override_replaces_path_segment() {
        let t = LibvirtTransport::RemoteSsh {
            host: "vmhost.example.com".to_string(),
            ssh_key_path: std::path::PathBuf::from("/keys/id_ed25519"),
            jump_host: None,
            uri: Some("qemu:///session".to_string()),
        };
        let uri = t.to_connect_uri();
        assert!(uri.starts_with("qemu+ssh://vmhost.example.com/session?"));
    }

    #[test]
    fn local_variant_never_mentions_ssh() {
        // Regression guard for the architectural correction: a `Local`
        // transport must never produce a URI that routes through SSH at
        // all, since it has no host/key material to build one from.
        let t = LibvirtTransport::Local { uri: None };
        assert!(!t.to_connect_uri().contains("ssh"));
    }
}
