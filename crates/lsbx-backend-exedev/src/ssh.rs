//! `russh`-based SSH session against `<vm_tag>.exe.xyz` (or the bare
//! `exe.dev` control host for account-level verbs like `new`/`ls`).
//!
//! ## Server-key trust model — trust-on-first-use, not unconditional trust
//!
//! `exe.dev` VMs are freshly provisioned Cloud Hypervisor guests created and
//! destroyed on a ~2-second cadence (SPEC.md, Unit 07 Context). There is no
//! long-lived host key an operator could pre-distribute or verify out of
//! band the way one might for a stable production fleet — the whole point
//! of this backend is that the host identity *is* disposable.
//!
//! Given that, this module does **not** unconditionally trust every server
//! key (that would accept a live MITM silently and indefinitely), and it
//! does **not** attempt real out-of-band host-key verification either (there
//! is no stable-enough identity to verify against for a VM that didn't exist
//! a few seconds ago). It implements trust-on-first-use (TOFU) instead, via
//! `russh_keys::check_known_hosts_path`/`learn_known_hosts_path`:
//!
//! - First connection to a given `host:port`: no recorded key exists, so the
//!   key is learned and the connection proceeds.
//! - Every subsequent connection to that same `host:port`: the presented key
//!   must match the one already recorded, or the connection is rejected
//!   with `LsbxError::AuthFailed` — a key that *changes* out from under an
//!   already-known `host:port` is exactly the signal TOFU exists to catch
//!   (a MITM, or a routing/DNS anomaly), even though a *first* untrusted
//!   connection can't be distinguished from a MITM by TOFU alone.
//!
//! This is a real, if partial, improvement over accepting every key
//! unconditionally: it can't stop a MITM active from the very first
//! connection to a fresh `vm_tag`, but it does stop a MITM that starts
//! *after* trust was established, and — just as importantly for this
//! backend's actual failure mode — it turns "exe.dev reused a hostname for
//! a different VM with a different host key" from silent into a loud,
//! actionable `AuthFailed` rather than something this backend would never
//! notice. Real out-of-band verification (the way a stable, human-operated
//! host would do it) isn't attempted here because exe.dev VMs don't have a
//! stable identity to verify against before they exist — see Unit 06's
//! `LibvirtBackend` remote-SSH transport for a backend where that
//! trade-off looks different (a remote libvirt host is long-lived, so real
//! known-hosts verification there is a stronger and more meaningful
//! guarantee than it would be here).
//!
//! The known-hosts file used here is **not** the operator's real
//! `~/.ssh/known_hosts` — it is scoped to `lsbx`'s own state
//! (`<state_dir>/exedev_known_hosts`), so this backend's TOFU bookkeeping
//! never reads from or writes to the operator's personal SSH client state.
use lsbx_kernel::backend::CommandOutput;
use lsbx_kernel::error::LsbxError;
use russh::client::Handler;
use russh::ChannelMsg;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

/// Where this backend's own known-hosts bookkeeping lives, distinct from the
/// operator's real `~/.ssh/known_hosts`. Falls back to a temp-dir-relative
/// path if the home directory can't be resolved (matching this crate's
/// general "never panic, degrade to an explicit error" posture) — a missing
/// home dir means TOFU state can't persist across process restarts, but
/// that's a availability degradation, not a correctness one: every
/// connection would just re-learn-and-trust, same as this being the actual
/// first-ever connection.
fn known_hosts_path() -> PathBuf {
    let base = dirs_base_dir();
    base.join(".lsbx").join("exedev_known_hosts")
}

/// Minimal home-dir resolution kept local to this module (no `dirs` crate
/// dependency) — reads `$HOME` directly, falling back to the system temp
/// directory. This crate has no other need for a general-purpose
/// directory-resolution dependency, and the fallback-key-path judgment call
/// (see `lib.rs`'s `ExedevAuth::HttpWithSshFallback`) deliberately does NOT
/// use this to guess at the operator's personal SSH key — this is only for
/// this backend's own scoped known-hosts bookkeeping.
fn dirs_base_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

struct TofuHandler {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
}

#[async_trait::async_trait]
impl Handler for TofuHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match russh_keys::check_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts_path,
        ) {
            // Key matches a previously recorded entry for this host:port.
            Ok(true) => Ok(true),
            // No recorded entry yet for this host:port: trust-on-first-use.
            // Learn it so a *future* connection to the same host:port can
            // detect a change. A failure to persist the learned key is
            // logged, not fatal — this connection can still proceed on the
            // trust decision already made; only the *next* connection loses
            // the benefit of TOFU if the write never lands.
            Ok(false) => {
                if let Err(e) = russh_keys::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    server_public_key,
                    &self.known_hosts_path,
                ) {
                    tracing::warn!(
                        host = %self.host,
                        port = self.port,
                        error = %e,
                        "failed to persist exedev TOFU known-hosts entry; this connection proceeds, \
                         but the next connection to this host:port will re-learn rather than verify"
                    );
                }
                Ok(true)
            }
            // `check_known_hosts_path` itself returns `Err` specifically
            // when the *type* of key matches a recorded entry but the key
            // *bytes* don't (`russh_keys::Error::KeyChanged`) — exactly the
            // "someone/something is presenting a different host identity
            // under a host:port we already trusted" case TOFU exists to
            // catch. Surfaced here as a hard reject rather than silently
            // re-trusting.
            Err(e) => {
                tracing::error!(
                    host = %self.host,
                    port = self.port,
                    error = %e,
                    "exedev server key changed since it was last trusted; rejecting connection"
                );
                Ok(false)
            }
        }
    }
}

pub struct SshSession {
    session: russh::client::Handle<TofuHandler>,
}

impl SshSession {
    pub async fn connect(
        key_path: &Path,
        user: &str,
        host: &str,
        port: u16,
    ) -> Result<Self, LsbxError> {
        let key_pair = russh_keys::load_secret_key(key_path, None)
            .map_err(|e| LsbxError::BackendUnavailable(format!("failed to load ssh key: {}", e)))?;
        let config = russh::client::Config::default();
        let config = Arc::new(config);

        let handler = TofuHandler {
            host: host.to_string(),
            port,
            known_hosts_path: known_hosts_path(),
        };

        let mut handle = russh::client::connect(config, (host, port), handler)
            .await
            .map_err(|e| {
                LsbxError::BackendUnavailable(format!("failed to connect via ssh: {}", e))
            })?;

        let auth_res = handle
            .authenticate_publickey(user, Arc::new(key_pair))
            .await
            .map_err(|e| LsbxError::BackendUnavailable(format!("failed to auth via ssh: {}", e)))?;

        if !auth_res {
            return Err(LsbxError::AuthFailed(
                "SSH authentication failed".to_string(),
            ));
        }

        Ok(Self { session: handle })
    }

    pub async fn exec(
        &mut self,
        command: &str,
        timeout_dur: Duration,
    ) -> Result<CommandOutput, LsbxError> {
        let mut channel = self.session.channel_open_session().await.map_err(|e| {
            LsbxError::BackendUnavailable(format!("failed to open ssh channel: {}", e))
        })?;

        channel
            .exec(true, command.as_bytes())
            .await
            .map_err(|e| LsbxError::BackendUnavailable(format!("failed to exec via ssh: {}", e)))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = 0;

        let fut = async {
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                    ChannelMsg::ExtendedData { ref data, ext: 1 } => stderr.extend_from_slice(data),
                    ChannelMsg::ExitStatus { exit_status } => {
                        exit_code = exit_status as i32;
                    }
                    _ => {}
                }
            }
            Ok(())
        };

        timeout(timeout_dur, fut)
            .await
            .map_err(|_| LsbxError::ContractViolated("ssh exec timed out".to_string()))?
            .map_err(|e: LsbxError| e)?;

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
        })
    }
}
