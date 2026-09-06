//! Exedev SSH backend (Unit 07) — `Backend` against exe.dev's real SSH-first
//! control plane, with its HTTPS `/exec` API as a bearer-token alternative.
//!
//! ## Dual control path
//! Every mutating verb (`new`, `run`, `rm`, `ls`) can go over either:
//! - **SSH** (via `russh`), matching the `ssh exe.dev <verb>` convention —
//!   selected directly when [`ExedevAuth::Ssh`] is configured.
//! - **HTTPS `/exec`** (bearer-token auth), exe.dev's own framing of this as
//!   "the SSH API shoved into a POST body" — selected when
//!   [`ExedevAuth::AccountToken`] or [`ExedevAuth::VmScopedToken`] is
//!   configured.
//!
//! ## The real `/exec` wire format (verified live 2026-09-04 — fixes lsbx#30)
//! The lobby parses the POST body **verbatim** as an exe.dev command string
//! (no JSON envelope) and returns stdout+stderr combined as plain text.
//! Guest execution rides `ssh <vm> <cmd>` — a first-class lobby command
//! since exe.dev's "Run commands on VM" launch — with an in-band
//! `__LSBX_EXIT:$?` sentinel for exit codes (the `X-Exe-Exit` trailer is
//! not reliably exposed through proxy chains). Control verbs carry their
//! own `--json` and surface errors as HTTP statuses. See
//! `http_fallback.rs` for the full contract and `wrap_guest_command` for
//! the two-layer quoting.
//!
//! ## Historical note: the 422-to-SSH fallback is obsolete
//! This backend originally treated 422 over `/exec` as a documented
//! "raw VM shell needs a real SSH session" limitation and fell back to SSH
//! transparently. exe.dev's run-on-vm launch removed that limitation:
//! `ssh <vm> <cmd>` over `/exec` is now supported behavior, errors arrive
//! as typed HTTP statuses, and the old 422 shape no longer occurs. The
//! `fallback_ssh_key_path` fields and `HttpExecOutcome::UnprocessableFallbackToSsh`
//! variant are retained for source compatibility but no longer participate
//! in any live code path (scheduled for removal in a follow-up).
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub mod http_fallback;
pub mod ssh;

use http_fallback::HttpFallbackClient;
use ssh::SshSession;

/// How this backend authenticates to exe.dev.
///
/// The two token variants authenticate the HTTPS `/exec` path (bearer
/// auth, no SSH key material involved — since the #31 wire-format fix,
/// token auth is fully self-sufficient for every account-level verb and
/// for short guest commands). The two SSH variants authenticate the SSH
/// transport via `russh`, which remains the right door for interactive
/// tooling, file transfer, and anything longer than the exec endpoint's
/// ~30 s cap. `ExedevBackend::new` accepts an optional per-VM key map for
/// SSH-side guest access regardless of the auth variant chosen.
///
/// Historical note: this enum once carried an optional
/// `fallback_ssh_key_path` on both token variants, bridging a then-real
/// 422-to-SSH retry. exe.dev's run-on-vm launch removed the 422 premise
/// and the #31 follow-up removed the field; SSH access under token auth
/// is deliberately NOT bridged by guessed key paths (the original
/// scope-widening objection stands — an operator's personal key must
/// never be silently adopted by an automated backend).
pub enum ExedevAuth {
    /// Account-wide `EXE_TOKEN` (bearer auth for the HTTPS `/exec` path).
    AccountToken { token: String },
    /// A VM-scoped token (`v0@VMNAME.exe.xyz`), per exe.dev's documented
    /// token model — narrows credential blast radius to one VM rather than
    /// the whole account.
    VmScopedToken { token: String },
    /// SSH-only with an explicit private key path.
    Ssh { key_path: PathBuf },
    /// SSH-only through the operator's configured OpenSSH alias. This is
    /// the compatibility path used by the Python service (`ssh exe.dev ...`)
    /// when no token is selected; the alias supplies the key and user from
    /// the host's existing SSH config/agent.
    SshAlias { alias: String },
}

impl ExedevAuth {
    /// Convenience constructor matching the plain `AccountToken(String)`
    /// shape the unit's interface contract shows.
    pub fn account_token(token: impl Into<String>) -> Self {
        Self::AccountToken {
            token: token.into(),
        }
    }

    pub fn ssh_alias(alias: impl Into<String>) -> Self {
        Self::SshAlias {
            alias: alias.into(),
        }
    }

    /// Convenience constructor matching the plain `VmScopedToken(String)`
    /// shape.
    pub fn vm_scoped_token(token: impl Into<String>) -> Self {
        Self::VmScopedToken {
            token: token.into(),
        }
    }

    fn http_token(&self) -> Option<&str> {
        match self {
            Self::AccountToken { token } | Self::VmScopedToken { token } => Some(token.as_str()),
            Self::Ssh { .. } | Self::SshAlias { .. } => None,
        }
    }

    /// True when this auth mode is scoped to a single VM (only `run`-shaped
    /// verbs are valid; account-level verbs like `new`/`rm`/`ls` are not).
    fn is_vm_scoped(&self) -> bool {
        matches!(self, Self::VmScopedToken { .. })
    }
}

pub struct ExedevBackend {
    auth: ExedevAuth,
    vm_key_paths: Arc<Mutex<HashMap<String, PathBuf>>>,
}

fn golden_vm_tag(golden_base: &str, name: &str) -> String {
    let source = golden_base
        .rsplit('/')
        .next()
        .unwrap_or(golden_base)
        .trim_end_matches(".qcow2");
    let stem = match source.rsplit_once("-v") {
        Some((prefix, version))
            if !version.is_empty() && version.chars().all(|c| c.is_ascii_digit()) =>
        {
            prefix
        }
        _ => source,
    };
    let stem = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let prefix = if stem.starts_with("lsbx-") {
        stem
    } else {
        format!("lsbx-{stem}")
    };
    let mut hasher = Sha256::new();
    hasher.update(format!("{golden_base}:{name}").as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{}-{}", prefix.trim_matches('-'), &digest[..12])
}

async fn run_open_ssh(args: Vec<String>, timeout: Duration) -> Result<CommandOutput, LsbxError> {
    use std::process::Stdio;
    use tokio::process::Command;

    let child = Command::new("ssh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| LsbxError::BackendUnavailable(format!("failed to start ssh: {e}")))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            LsbxError::ContractViolated(format!("ssh command timed out after {timeout:?}"))
        })?
        .map_err(|e| LsbxError::BackendUnavailable(format!("ssh command failed: {e}")))?;
    Ok(CommandOutput {
        exit_code: output.status.code().unwrap_or(255),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

async fn run_scp(args: Vec<String>, timeout: Duration) -> Result<(), LsbxError> {
    use std::process::Stdio;
    use tokio::process::Command;

    let child = Command::new("scp")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| LsbxError::BackendUnavailable(format!("failed to start scp: {e}")))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            LsbxError::ContractViolated(format!("scp command timed out after {timeout:?}"))
        })?
        .map_err(|e| LsbxError::BackendUnavailable(format!("scp command failed: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(LsbxError::BackendUnavailable(format!(
            "scp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn normalize_ssh_result(
    vm_tag: &str,
    result: Result<CommandOutput, LsbxError>,
) -> Result<CommandOutput, LsbxError> {
    let output = result?;
    if output.exit_code == 255 {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let lower = detail.to_ascii_lowercase();
        if lower.contains("could not resolve hostname")
            || lower.contains("name or service not known")
            || lower.contains("not found")
        {
            return Err(LsbxError::NotFound(format!("vm_tag '{vm_tag}' not found")));
        }
        return Err(LsbxError::BackendUnavailable(format!(
            "ssh connection to vm_tag '{vm_tag}' failed: {detail}"
        )));
    }
    Ok(output)
}

fn parse_json_value(bytes: &[u8], operation: &str) -> Result<serde_json::Value, LsbxError> {
    let text = String::from_utf8_lossy(bytes);
    let candidates = std::iter::once(text.trim()).chain(text.lines().rev().map(str::trim));
    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) {
            return Ok(value);
        }
    }
    Err(LsbxError::BackendUnavailable(format!(
        "exe.dev {operation} returned malformed JSON"
    )))
}

fn parse_json_object(
    bytes: &[u8],
    operation: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, LsbxError> {
    let value = parse_json_value(bytes, operation)?;
    value.as_object().cloned().ok_or_else(|| {
        LsbxError::BackendUnavailable(format!(
            "exe.dev {operation} returned a non-object JSON payload"
        ))
    })
}

impl ExedevBackend {
    pub fn new(auth: ExedevAuth) -> Self {
        Self {
            auth,
            vm_key_paths: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn vm_key_path(&self, vm_tag: &str) -> Option<PathBuf> {
        self.vm_key_paths
            .lock()
            .ok()
            .and_then(|keys| keys.get(vm_tag).cloned())
    }

    async fn run_ssh(
        &self,
        host: &str,
        cmd: &str,
        timeout: Duration,
        key_path: &std::path::Path,
    ) -> Result<CommandOutput, LsbxError> {
        if !key_path.is_file() {
            return Err(LsbxError::BackendUnavailable(format!(
                "ssh key does not exist: {}",
                key_path.display()
            )));
        }
        run_open_ssh(
            vec![
                "-n".to_string(),
                "-i".to_string(),
                key_path.display().to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                "-o".to_string(),
                "ConnectTimeout=30".to_string(),
                "-o".to_string(),
                "ClearAllForwardings=yes".to_string(),
                host.to_string(),
                cmd.to_string(),
            ],
            timeout,
        )
        .await
    }

    async fn run_ssh_alias(&self, alias: &str, cmd: &str) -> Result<CommandOutput, LsbxError> {
        run_open_ssh(
            vec![
                "-n".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                alias.to_string(),
                cmd.to_string(),
            ],
            Duration::from_secs(35),
        )
        .await
    }

    fn guest_key_path(&self, vm_tag: &str) -> Option<PathBuf> {
        // Token auth modes carry no SSH key material at all — exe.dev's
        // HTTPS path is the token carrier's remote-execution surface, and
        // scp/interactive SSH need the explicit `Ssh`/`SshAlias` modes.
        // (The old fallback_ssh_key_path bridge died with the 422-fallback
        // premise; see lsbx#31's follow-up cleanup.)
        match &self.auth {
            ExedevAuth::Ssh { key_path } => Some(key_path.clone()),
            _ => self.vm_key_path(vm_tag),
        }
    }

    fn explicit_or_guest_key(
        &self,
        vm_tag: &str,
        identity_file: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        identity_file
            .map(std::path::Path::to_path_buf)
            .or_else(|| self.guest_key_path(vm_tag))
    }

    async fn normalize_vm_ssh_result(
        &self,
        vm_tag: &str,
        result: Result<CommandOutput, LsbxError>,
    ) -> Result<CommandOutput, LsbxError> {
        let output = normalize_ssh_result(vm_tag, result)?;
        if output.exit_code != 0 {
            // Some exe.dev SSH routes return a shell-style nonzero status
            // instead of OpenSSH's 255 after a VM is deleted. Confirm the
            // VM's absence from the control-plane inventory before exposing
            // that transport failure as a normal guest exit code.
            if let Ok(vms) = self.list_vms().await {
                if !vms.iter().any(|tag| tag == vm_tag) {
                    return Err(LsbxError::NotFound(format!("vm_tag '{vm_tag}' not found")));
                }
            }
        }
        Ok(output)
    }
    async fn run_http_account_level(&self, cmd: &str) -> Result<CommandOutput, LsbxError> {
        let token = self.auth.http_token().ok_or_else(|| {
            LsbxError::BackendUnavailable("no HTTP token configured for this auth mode".to_string())
        })?;
        let client = HttpFallbackClient::new(token.to_string(), None);
        // Control verbs carry their own `--json` and surface errors as HTTP
        // statuses (mapped to exit 255 inside the client).
        client.exec(cmd).await
    }

    fn require_not_vm_scoped(&self, verb: &str) -> Result<(), LsbxError> {
        if self.auth.is_vm_scoped() {
            return Err(LsbxError::BackendUnavailable(format!(
                "cannot {verb} using a VM-scoped token — this token is authorized for exactly one VM's own verbs, not account-level operations"
            )));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Backend for ExedevBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            console: true,
            remote_transport: true,
            snapshot: false,
        }
    }

    async fn register_vm_key(
        &self,
        vm_tag: &str,
        key_path: &std::path::Path,
    ) -> Result<(), LsbxError> {
        if !key_path.is_file() {
            return Err(LsbxError::BackendUnavailable(format!(
                "ephemeral SSH key does not exist: {}",
                key_path.display()
            )));
        }
        let mut keys = self.vm_key_paths.lock().map_err(|_| {
            LsbxError::ContractViolated("exedev VM key map was poisoned".to_string())
        })?;
        keys.insert(vm_tag.to_string(), key_path.to_path_buf());
        Ok(())
    }

    async fn create_from_golden(
        &self,
        req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, LsbxError> {
        self.require_not_vm_scoped("create a VM")?;

        // Match the Python provider's real protocol: clone the registered
        // base with `cp`, then tag the clone and register the ephemeral key.
        // The lifecycle layer passes the resolved golden base in `req.golden`.
        let cmd = format!(
            "cp {} {} --copy-tags=false --cpu={} --memory={} --json",
            shell_quote(req.golden.as_str()),
            shell_quote(req.name),
            req.cpu,
            shell_quote(req.memory),
        );

        let out = match &self.auth {
            ExedevAuth::AccountToken { .. } => self.run_http_account_level(&cmd).await?,
            ExedevAuth::VmScopedToken { .. } => {
                unreachable!("require_not_vm_scoped already rejected this")
            }
            ExedevAuth::Ssh { key_path } => {
                self.run_ssh("exe.dev", &cmd, Duration::from_secs(35), key_path)
                    .await?
            }
            ExedevAuth::SshAlias { alias } => self.run_ssh_alias(alias, &cmd).await?,
        };

        if out.exit_code != 0 {
            return Err(LsbxError::BackendUnavailable(format!(
                "failed to clone VM from golden '{}': {}",
                req.golden,
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        let metadata = match parse_json_object(&out.stdout, "cp") {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = self.destroy(req.name).await;
                return Err(error);
            }
        };
        let host = match metadata
            .get("ssh_dest")
            .or_else(|| metadata.get("host"))
            .or_else(|| metadata.get("ssh_host"))
            .or_else(|| metadata.get("ssh"))
            .or_else(|| metadata.get("dns_name"))
            .and_then(serde_json::Value::as_str)
        {
            Some(host) => host,
            None => {
                let _ = self.destroy(req.name).await;
                return Err(LsbxError::BackendUnavailable(
                    "exe.dev cp response omitted SSH host metadata".to_string(),
                ));
            }
        };
        let host = host.split_once('@').map(|(_, value)| value).unwrap_or(host);

        let tag_cmd = format!(
            "tag {} {} --json",
            shell_quote(req.name),
            shell_quote(&golden_vm_tag(req.golden.as_str(), req.name)),
        );
        let key_cmd = format!("ssh-key add {} --json", shell_quote(req.pubkey));
        for follow_up in [tag_cmd, key_cmd] {
            let result = match &self.auth {
                ExedevAuth::AccountToken { .. } => {
                    match self.run_http_account_level(&follow_up).await {
                        Ok(result) => result,
                        Err(error) => {
                            let _ = self.destroy(req.name).await;
                            return Err(error);
                        }
                    }
                }
                ExedevAuth::VmScopedToken { .. } => {
                    unreachable!("require_not_vm_scoped already rejected this")
                }
                ExedevAuth::Ssh { key_path } => {
                    match self
                        .run_ssh("exe.dev", &follow_up, Duration::from_secs(35), key_path)
                        .await
                    {
                        Ok(result) => result,
                        Err(error) => {
                            let _ = self.destroy(req.name).await;
                            return Err(error);
                        }
                    }
                }
                ExedevAuth::SshAlias { alias } => match self.run_ssh_alias(alias, &follow_up).await
                {
                    Ok(result) => result,
                    Err(error) => {
                        let _ = self.destroy(req.name).await;
                        return Err(error);
                    }
                },
            };
            if result.exit_code != 0 {
                let error = LsbxError::BackendUnavailable(format!(
                    "exe.dev command failed after cloning '{}': {}",
                    req.name,
                    String::from_utf8_lossy(&result.stderr)
                ));
                let _ = self.destroy(req.name).await;
                return Err(error);
            }
        }

        let vm_tag = req.name.to_string();
        let https_url = Some(format!("https://{}", host.trim_end_matches('/')));
        Ok(CreatedVm {
            vm_tag,
            host: host.to_string(),
            https_url,
        })
    }

    /// Runs a guest command on `vm_tag`. The transport follows the auth
    /// variant: token modes go over HTTPS `/exec` (guest commands ride
    /// `ssh <vm> <cmd>` with an in-band exit sentinel — see
    /// `http_fallback::wrap_guest_command`); SSH modes go over `russh`
    /// with the explicit, per-VM, or alias-configured key. `identity_file`
    /// (when provided) overrides the key used by the SSH paths.
    async fn run(
        &self,
        vm_tag: &str,
        command: &[String],
        timeout: Duration,
        identity_file: Option<&std::path::Path>,
    ) -> Result<CommandOutput, LsbxError> {
        let cmd = command
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let host = format!("{}.exe.xyz", vm_tag);

        let result = match &self.auth {
            ExedevAuth::AccountToken { .. } | ExedevAuth::VmScopedToken { .. } => {
                // HTTPS guest execution over the shared account-level exec
                // endpoint (fixes lsbx#30): guest commands ride `ssh <vm>`
                // as a first-class lobby command with an in-band exit
                // sentinel — no VM-scoped exec URL (that path serves the
                // VM's own HTTP services, not an exec API) and no
                // JSON-envelope wire format (the lobby parses the body
                // verbatim). Both were the old client's premises; both are
                // obsolete. The exec endpoint's ~30s server cap means this
                // path serves short verification commands; longer work and
                // full interactive tooling still want the SSH paths below.
                let token = self.auth.http_token().ok_or_else(|| {
                    LsbxError::BackendUnavailable("no HTTP token configured".to_string())
                })?;
                let client = HttpFallbackClient::new(token.to_string(), None);
                let lobby_cmd = http_fallback::wrap_guest_command(vm_tag, &cmd);
                return client.exec_with_timeout(&lobby_cmd, timeout).await;
            }
            ExedevAuth::Ssh { key_path } => identity_file
                .map(std::path::Path::to_path_buf)
                .or_else(|| self.vm_key_path(vm_tag))
                .unwrap_or_else(|| key_path.clone()),
            ExedevAuth::SshAlias { .. } => {
                if let Some(key_path) = identity_file
                    .map(std::path::Path::to_path_buf)
                    .or_else(|| self.vm_key_path(vm_tag))
                {
                    return self
                        .normalize_vm_ssh_result(
                            vm_tag,
                            self.run_ssh(&host, &cmd, timeout, &key_path).await,
                        )
                        .await;
                }
                return self
                    .normalize_vm_ssh_result(
                        vm_tag,
                        run_open_ssh(
                            vec![
                                "-n".to_string(),
                                "-o".to_string(),
                                "BatchMode=yes".to_string(),
                                "-o".to_string(),
                                "StrictHostKeyChecking=accept-new".to_string(),
                                "-o".to_string(),
                                "ConnectTimeout=30".to_string(),
                                host,
                                cmd,
                            ],
                            timeout,
                        )
                        .await,
                    )
                    .await;
            }
        };
        self.normalize_vm_ssh_result(vm_tag, self.run_ssh(&host, &cmd, timeout, &result).await)
            .await
    }

    async fn put_file(
        &self,
        vm_tag: &str,
        source: &std::path::Path,
        destination: &str,
        identity_file: Option<&std::path::Path>,
    ) -> Result<(), LsbxError> {
        if !source.is_file() && !source.is_dir() {
            return Err(LsbxError::NotFound(format!(
                "local upload source does not exist: {}",
                source.display()
            )));
        }
        let host = format!("{}.exe.xyz", vm_tag);
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ];
        if source.is_dir() {
            args.push("-r".to_string());
        }
        if let Some(key_path) = self.explicit_or_guest_key(vm_tag, identity_file) {
            args.extend(["-i".to_string(), key_path.display().to_string()]);
        }
        args.push(source.display().to_string());
        args.push(format!("{}:{}", host, destination));
        run_scp(args, Duration::from_secs(120)).await
    }

    async fn get_file(
        &self,
        vm_tag: &str,
        source: &str,
        destination: &std::path::Path,
        identity_file: Option<&std::path::Path>,
    ) -> Result<(), LsbxError> {
        let host = format!("{}.exe.xyz", vm_tag);
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ];
        if let Some(key_path) = self.explicit_or_guest_key(vm_tag, identity_file) {
            args.extend(["-i".to_string(), key_path.display().to_string()]);
        }
        args.push(format!("{}:{}", host, source));
        args.push(destination.display().to_string());
        run_scp(args, Duration::from_secs(120)).await
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        self.require_not_vm_scoped("delete a VM")?;
        // exe.dev's `rm` is idempotent and exits 0 for a missing VM, but the
        // Backend contract distinguishes that case as NotFound. Probe the
        // authoritative inventory first so callers and the shared
        // conformance suite get the stable taxonomy regardless of transport.
        if !self.list_vms().await?.iter().any(|tag| tag == vm_tag) {
            return Err(LsbxError::NotFound(format!("vm_tag '{vm_tag}' not found")));
        }
        let cmd = format!("rm {}", shell_quote(vm_tag));
        let out = match &self.auth {
            ExedevAuth::AccountToken { .. } => self.run_http_account_level(&cmd).await?,
            ExedevAuth::VmScopedToken { .. } => {
                unreachable!("require_not_vm_scoped already rejected this")
            }
            ExedevAuth::Ssh { key_path } => {
                self.run_ssh("exe.dev", &cmd, Duration::from_secs(35), key_path)
                    .await?
            }
            ExedevAuth::SshAlias { alias } => self.run_ssh_alias(alias, &cmd).await?,
        };

        match out.exit_code {
            0 => {
                if let Ok(mut keys) = self.vm_key_paths.lock() {
                    keys.remove(vm_tag);
                }
                Ok(())
            }
            _ if String::from_utf8_lossy(&out.stderr)
                .to_lowercase()
                .contains("not found")
                || String::from_utf8_lossy(&out.stderr)
                    .to_lowercase()
                    .contains("no such") =>
            {
                Err(LsbxError::NotFound(format!("vm_tag '{vm_tag}' not found")))
            }
            _ => Err(LsbxError::BackendUnavailable(format!(
                "failed to delete VM: {}",
                String::from_utf8_lossy(&out.stderr)
            ))),
        }
    }

    /// Destroys a VM and revokes the per-sandbox key when the caller has
    /// persisted key material. This is the compatibility extension used by
    /// lifecycle/reaper cleanup; plain `destroy` remains available for
    /// account-level callers that do not have the public key.
    async fn destroy_with_key(&self, vm_tag: &str, pubkey: &str) -> Result<(), LsbxError> {
        self.require_not_vm_scoped("delete a VM")?;
        let cmd = format!("ssh-key remove {} --json", shell_quote(pubkey));
        let result = match &self.auth {
            ExedevAuth::AccountToken { .. } => self.run_http_account_level(&cmd).await?,
            ExedevAuth::VmScopedToken { .. } => {
                unreachable!("require_not_vm_scoped already rejected this")
            }
            ExedevAuth::Ssh { key_path } => {
                self.run_ssh("exe.dev", &cmd, Duration::from_secs(35), key_path)
                    .await?
            }
            ExedevAuth::SshAlias { alias } => self.run_ssh_alias(alias, &cmd).await?,
        };
        if result.exit_code != 0 {
            let detail = format!(
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let lower = detail.to_lowercase();
            if !lower.contains("not found")
                && !lower.contains("no such")
                && !lower.contains("no matching")
            {
                return Err(LsbxError::BackendUnavailable(format!(
                    "failed to revoke ephemeral SSH key: {detail}"
                )));
            }
        }
        match self.destroy(vm_tag).await {
            Err(LsbxError::NotFound(_)) => Ok(()),
            result => result,
        }
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        self.require_not_vm_scoped("list VMs")?;
        let cmd = "ls --json";
        let out = match &self.auth {
            ExedevAuth::AccountToken { .. } => self.run_http_account_level(cmd).await?,
            ExedevAuth::VmScopedToken { .. } => {
                unreachable!("require_not_vm_scoped already rejected this")
            }
            ExedevAuth::Ssh { key_path } => {
                self.run_ssh("exe.dev", cmd, Duration::from_secs(35), key_path)
                    .await?
            }
            ExedevAuth::SshAlias { alias } => self.run_ssh_alias(alias, cmd).await?,
        };
        if out.exit_code != 0 {
            return Err(LsbxError::BackendUnavailable(format!(
                "failed to list VMs: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let value = parse_json_value(&out.stdout, "ls")?;
        let entries = value
            .get("vms")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                LsbxError::BackendUnavailable("exe.dev ls response had no vms array".to_string())
            })?;
        Ok(entries
            .iter()
            .filter_map(|entry| entry.get("vm_name").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect())
    }

    async fn reconcile_orphaned_keys(&self, known_labels: &[String]) -> Result<usize, LsbxError> {
        reconcile_exedev_keys(self, known_labels).await
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        Err(LsbxError::BackendUnavailable(
            "exe.dev's CLI/API does not document a rename verb for provisioned VMs".to_string(),
        ))
    }
}

/// Lists exe.dev's real account-level SSH-key inventory and feeds it to
/// `lsbx_keys::reconcile_orphaned_keys`. exe.dev returns an object with an
/// `ssh_keys` array; each entry carries the public key and the key name, where
/// the name preserves the `lsbx:<label>` comment used by the lifecycle layer.
async fn list_tagged_keys(
    backend: &ExedevBackend,
) -> Result<Vec<lsbx_keys::reconcile::TaggedKey>, LsbxError> {
    let out = match &backend.auth {
        ExedevAuth::AccountToken { .. } => {
            backend
                .run_http_account_level("ssh-key list --json")
                .await?
        }
        ExedevAuth::VmScopedToken { .. } => {
            return Err(LsbxError::BackendUnavailable(
                "cannot list account-level keys using a VM-scoped token".to_string(),
            ))
        }
        ExedevAuth::Ssh { key_path } => {
            backend
                .run_ssh(
                    "exe.dev",
                    "ssh-key list --json",
                    Duration::from_secs(35),
                    key_path,
                )
                .await?
        }
        ExedevAuth::SshAlias { alias } => {
            backend.run_ssh_alias(alias, "ssh-key list --json").await?
        }
    };
    if out.exit_code != 0 {
        return Err(LsbxError::BackendUnavailable(format!(
            "failed to list exedev keys: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }

    let value = parse_json_value(&out.stdout, "ssh-key list")?;
    let entries = value
        .get("ssh_keys")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            LsbxError::BackendUnavailable(
                "exe.dev ssh-key list response had no ssh_keys array".to_string(),
            )
        })?;

    let auth_token = backend.auth.http_token().map(str::to_string);
    let ssh_key_path = match &backend.auth {
        ExedevAuth::Ssh { key_path } => Some(key_path.clone()),
        _ => None,
    };
    let ssh_alias = match &backend.auth {
        ExedevAuth::SshAlias { alias } => Some(alias.clone()),
        _ => None,
    };

    let mut tagged_keys = Vec::new();
    for entry in entries {
        let Some(public_key) = entry.get("public_key").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let comment = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| public_key.split_whitespace().nth(2))
            .unwrap_or_default()
            .to_string();
        let public_key = public_key.to_string();
        let revoke_cmd = format!("ssh-key remove {} --json", shell_quote(&public_key));
        let auth_token = auth_token.clone();
        let ssh_key_path = ssh_key_path.clone();
        let ssh_alias = ssh_alias.clone();

        tagged_keys.push(lsbx_keys::reconcile::TaggedKey {
            comment,
            revoke: Box::new(move || {
                // `TaggedKey::revoke` is synchronous, while exe.dev's
                // transport is async. Run the short-lived Tokio runtime on
                // a worker thread so this remains safe when the reaper
                // itself is already running inside Tokio.
                let join = std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            LsbxError::ContractViolated(format!(
                                "failed to start runtime for key revoke: {e}"
                            ))
                        })?;
                    rt.block_on(async {
                        let out = match (&auth_token, &ssh_key_path, &ssh_alias) {
                            (Some(token), _, _) => {
                                let client = HttpFallbackClient::new(token.clone(), None);
                                client.exec(&revoke_cmd).await?
                            }
                            (None, Some(key_path), _) => {
                                let mut session =
                                    SshSession::connect(key_path, "root", "exe.dev", 22).await?;
                                session.exec(&revoke_cmd, Duration::from_secs(30)).await?
                            }
                            (None, None, Some(alias)) => {
                                run_open_ssh(
                                    vec![
                                        "-n".to_string(),
                                        "-o".to_string(),
                                        "BatchMode=yes".to_string(),
                                        alias.clone(),
                                        revoke_cmd.clone(),
                                    ],
                                    Duration::from_secs(30),
                                )
                                .await?
                            }
                            (None, None, None) => {
                                return Err(LsbxError::BackendUnavailable(
                                    "no auth mode available to revoke key".to_string(),
                                ))
                            }
                        };
                        if out.exit_code == 0 {
                            return Ok(());
                        }
                        let detail = format!(
                            "{}\n{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        );
                        let lower = detail.to_lowercase();
                        if lower.contains("not found")
                            || lower.contains("no such")
                            || lower.contains("no matching")
                        {
                            Ok(())
                        } else {
                            Err(LsbxError::BackendUnavailable(format!(
                                "failed to revoke SSH key: {detail}"
                            )))
                        }
                    })
                });
                join.join().map_err(|_| {
                    LsbxError::ContractViolated("key revoke worker thread panicked".to_string())
                })?
            }),
        });
    }

    Ok(tagged_keys)
}

/// Public entry point for exe.dev-specific orphan-key cleanup.
pub async fn reconcile_exedev_keys(
    backend: &ExedevBackend,
    known_labels: &[String],
) -> Result<usize, LsbxError> {
    let tagged_keys = list_tagged_keys(backend).await?;
    lsbx_keys::reconcile::reconcile_orphaned_keys(tagged_keys, known_labels)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Token auth modes expose their token to the HTTPS path and nothing
    /// else — asserting both facts directly so a future refactor can't
    /// quietly re-bridge token auth to guessed SSH identity (the
    /// scope-widening `ExedevAuth`'s doc comment warns against).
    #[test]
    fn token_variants_expose_only_the_http_token() {
        let account = ExedevAuth::account_token("EXE_TOKEN_VALUE");
        assert_eq!(account.http_token(), Some("EXE_TOKEN_VALUE"));

        let vm_scoped = ExedevAuth::vm_scoped_token("v0@my-vm.exe.xyz");
        assert_eq!(vm_scoped.http_token(), Some("v0@my-vm.exe.xyz"));
    }

    /// `ExedevAuth::Ssh` has no HTTP token (there is no HTTPS path in this
    /// mode).
    #[test]
    fn ssh_variant_has_no_http_token() {
        let ssh = ExedevAuth::Ssh {
            key_path: PathBuf::from("/tmp/key"),
        };
        assert_eq!(ssh.http_token(), None);
    }

    /// `is_vm_scoped()` is the guard `require_not_vm_scoped` depends on for
    /// every account-level verb — asserted directly against all three
    /// variants so a future refactor of the match arms can't silently widen
    /// or narrow which auth modes count as VM-scoped without a test noticing.
    #[test]
    fn is_vm_scoped_is_true_for_exactly_the_vm_scoped_variant() {
        assert!(!ExedevAuth::account_token("t").is_vm_scoped());
        assert!(ExedevAuth::vm_scoped_token("t").is_vm_scoped());
        assert!(!ExedevAuth::Ssh {
            key_path: PathBuf::from("/tmp/key")
        }
        .is_vm_scoped());
    }
}
