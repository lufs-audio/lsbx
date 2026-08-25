//! Exedev SSH backend (Unit 07) — `Backend` against exe.dev's real SSH-first
//! control plane, with its HTTPS `/exec` API as a fallback.
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
//! ## The 422-to-SSH fallback (`run()` only)
//! `POST /exec` against a VM directly can return `422 Unprocessable Entity`
//! for some shell invocations — a documented exe.dev API limitation where
//! only a real SSH session reliably reaches a VM's shell. `run()` detects
//! this and falls back to SSH transparently, rather than surfacing a bare
//! 422 to the caller (Unit 07 acceptance criteria) — but *only* when this
//! backend has been configured with somewhere to get an SSH key from for
//! that fallback. See `ExedevAuth::fallback_ssh_key_path` below for why that
//! configuration is explicit rather than assumed.
use lsbx_kernel::backend::*;
use lsbx_kernel::error::LsbxError;

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub mod http_fallback;
pub mod ssh;

use http_fallback::{HttpExecOutcome, HttpFallbackClient};
use ssh::SshSession;

/// How this backend authenticates to exe.dev, and — for the two HTTP-based
/// variants — where it should look for an SSH key if it needs to fall back
/// from a 422 HTTPS response.
///
/// ## Why the fallback key path is explicit, not assumed
///
/// The unit contract's own interface contract shows this enum with three
/// bare variants (`AccountToken(String)`, `VmScopedToken(String)`,
/// `Ssh { key_path: PathBuf }`) and no fourth field anywhere for a fallback
/// key. That leaves a real gap: when `run()` is using the HTTPS path and
/// hits a 422, what SSH key does it use to retry?
///
/// The two tempting-but-wrong answers:
/// - Guess at the *operator's own* personal key (e.g. `~/.ssh/id_ed25519`).
///   Wrong because there's no reason to believe the account or VM-scoped
///   token holder's personal workstation key is even registered with the
///   specific VM being talked to — and reaching into an operator's personal
///   SSH identity from an automated backend is exactly the kind of
///   scope-widening this house's conventions push back on.
/// - Silently do nothing and let every 422 be a hard failure when running
///   under an HTTP-based auth mode. This satisfies "don't guess" but doesn't
///   satisfy the unit's actual acceptance criterion, which requires the
///   fallback to work transparently, not just "when the caller happens to
///   also have configured `ExedevAuth::Ssh` some other way."
///
/// This backend does not receive an ephemeral keypair from anywhere today —
/// `Backend::run()`'s signature (owned by Unit 01, unchanged here) takes no
/// key material at all, and Unit 03's `EphemeralKeypair` is generated and
/// owned by whatever calls into a `Backend`, not passed through the trait.
/// The actually-correct long-term fix is Unit 09 (VM Lifecycle Orchestration)
/// threading the same `EphemeralKeypair` it already generates for
/// `create_from_golden`'s `pubkey` through to `ExedevBackend`'s
/// construction, since that key's private half is guaranteed to be
/// registered on the VM this backend is about to talk to. That's a
/// `Backend` trait / call-site change outside this unit's boundary (Unit 07
/// owns `src/lib.rs`, `src/ssh.rs`, `src/http_fallback.rs` — not the trait
/// signature in `lsbx-kernel`, and not Unit 09's orchestration code) — see
/// the crate-level flagged gap below.
///
/// What this unit *can* do within its own boundary: make the fallback key
/// path an explicit, caller-supplied configuration value on the auth enum
/// itself, so whoever constructs `ExedevBackend` (today: whoever wires up
/// this backend by hand; later: Unit 09) decides where that key comes from,
/// rather than this crate silently assuming a fixed path. When no fallback
/// path is configured, a 422 under an HTTP-based auth mode is a clear,
/// typed `BackendUnavailable` error naming exactly what's missing — not a
/// silent guess and not a panic.
pub enum ExedevAuth {
    /// Account-wide `EXE_TOKEN`. `fallback_ssh_key_path` is the SSH private
    /// key `run()` uses if an `/exec` call under this token hits a 422.
    AccountToken {
        token: String,
        fallback_ssh_key_path: Option<PathBuf>,
    },
    /// A VM-scoped token (`v0@VMNAME.exe.xyz`), per exe.dev's documented
    /// token model — narrows credential blast radius to one VM rather than
    /// the whole account. Same fallback-key story as `AccountToken`.
    VmScopedToken {
        token: String,
        fallback_ssh_key_path: Option<PathBuf>,
    },
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
    /// shape the unit's interface contract shows, for callers that don't
    /// need a 422-fallback path (e.g. tests, or a deployment that only ever
    /// expects account-level verbs that never 422).
    pub fn account_token(token: impl Into<String>) -> Self {
        Self::AccountToken {
            token: token.into(),
            fallback_ssh_key_path: None,
        }
    }

    /// As `account_token`, plus a fallback SSH key path for `run()`'s
    /// 422-to-SSH retry.
    pub fn account_token_with_fallback(
        token: impl Into<String>,
        fallback_ssh_key_path: PathBuf,
    ) -> Self {
        Self::AccountToken {
            token: token.into(),
            fallback_ssh_key_path: Some(fallback_ssh_key_path),
        }
    }

    pub fn ssh_alias(alias: impl Into<String>) -> Self {
        Self::SshAlias {
            alias: alias.into(),
        }
    }

    /// Convenience constructor matching the plain `VmScopedToken(String)`
    /// shape, no fallback path configured.
    pub fn vm_scoped_token(token: impl Into<String>) -> Self {
        Self::VmScopedToken {
            token: token.into(),
            fallback_ssh_key_path: None,
        }
    }

    /// As `vm_scoped_token`, plus a fallback SSH key path.
    pub fn vm_scoped_token_with_fallback(
        token: impl Into<String>,
        fallback_ssh_key_path: PathBuf,
    ) -> Self {
        Self::VmScopedToken {
            token: token.into(),
            fallback_ssh_key_path: Some(fallback_ssh_key_path),
        }
    }

    fn http_token(&self) -> Option<&str> {
        match self {
            Self::AccountToken { token, .. } | Self::VmScopedToken { token, .. } => {
                Some(token.as_str())
            }
            Self::Ssh { .. } | Self::SshAlias { .. } => None,
        }
    }

    fn fallback_ssh_key_path(&self) -> Option<&PathBuf> {
        match self {
            Self::AccountToken {
                fallback_ssh_key_path,
                ..
            }
            | Self::VmScopedToken {
                fallback_ssh_key_path,
                ..
            } => fallback_ssh_key_path.as_ref(),
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
        self.vm_key_path(vm_tag).or_else(|| match &self.auth {
            ExedevAuth::Ssh { key_path } => Some(key_path.clone()),
            ExedevAuth::AccountToken { .. } | ExedevAuth::VmScopedToken { .. } => {
                self.auth.fallback_ssh_key_path().cloned()
            }
            ExedevAuth::SshAlias { .. } => None,
        })
    }

    async fn run_http_account_level(&self, cmd: &str) -> Result<CommandOutput, LsbxError> {
        let token = self.auth.http_token().ok_or_else(|| {
            LsbxError::BackendUnavailable("no HTTP token configured for this auth mode".to_string())
        })?;
        let client = HttpFallbackClient::new(token.to_string(), None);
        match client.exec(cmd).await? {
            HttpExecOutcome::Completed(out) => Ok(out),
            HttpExecOutcome::UnprocessableFallbackToSsh => Err(LsbxError::BackendUnavailable(
                "exe.dev returned 422 for an account-level command; no SSH fallback target for a non-VM-scoped verb".to_string(),
            )),
        }
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

        let metadata = parse_json_object(&out.stdout, "cp")?;
        let host = metadata
            .get("ssh_dest")
            .or_else(|| metadata.get("host"))
            .or_else(|| metadata.get("ssh_host"))
            .or_else(|| metadata.get("ssh"))
            .or_else(|| metadata.get("dns_name"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                LsbxError::BackendUnavailable(
                    "exe.dev cp response omitted SSH host metadata".to_string(),
                )
            })?;
        let host = host.split_once('@').map(|(_, value)| value).unwrap_or(host);

        let tag_cmd = format!(
            "tag {} {} --json",
            shell_quote(req.name),
            shell_quote(&golden_vm_tag(req.golden.as_str(), req.name)),
        );
        let key_cmd = format!("ssh-key add {} --json", shell_quote(req.pubkey));
        for follow_up in [tag_cmd, key_cmd] {
            let result = match &self.auth {
                ExedevAuth::AccountToken { .. } => self.run_http_account_level(&follow_up).await?,
                ExedevAuth::VmScopedToken { .. } => {
                    unreachable!("require_not_vm_scoped already rejected this")
                }
                ExedevAuth::Ssh { key_path } => {
                    self.run_ssh("exe.dev", &follow_up, Duration::from_secs(35), key_path)
                        .await?
                }
                ExedevAuth::SshAlias { alias } => self.run_ssh_alias(alias, &follow_up).await?,
            };
            if result.exit_code != 0 {
                return Err(LsbxError::BackendUnavailable(format!(
                    "exe.dev command failed after cloning '{}': {}",
                    req.name,
                    String::from_utf8_lossy(&result.stderr)
                )));
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

    /// Falls back from HTTPS to SSH transparently when exe.dev's documented
    /// raw-VM-shell 422 is detected, rather than surfacing a bare 422 to the
    /// caller (Unit 07 acceptance criteria) — but only when a fallback SSH
    /// key path has been configured on this backend's `ExedevAuth`. See the
    /// doc comment on `ExedevAuth` for why that path is explicit rather than
    /// assumed to be the operator's personal key.
    async fn run(
        &self,
        vm_tag: &str,
        command: &[String],
        timeout: Duration,
    ) -> Result<CommandOutput, LsbxError> {
        let cmd = command
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let host = format!("{}.exe.xyz", vm_tag);

        let result = match &self.auth {
            ExedevAuth::AccountToken { .. } | ExedevAuth::VmScopedToken { .. } => {
                let token = self.auth.http_token().ok_or_else(|| {
                    LsbxError::BackendUnavailable("no HTTP token configured".to_string())
                })?;
                let client = HttpFallbackClient::new(token.to_string(), Some(vm_tag));
                match client.exec(&cmd).await? {
                    HttpExecOutcome::Completed(out) => return Ok(out),
                    HttpExecOutcome::UnprocessableFallbackToSsh => {}
                }
                self.vm_key_path(vm_tag)
                    .or_else(|| self.auth.fallback_ssh_key_path().cloned())
                    .ok_or_else(|| {
                        LsbxError::BackendUnavailable(format!(
                            "exe.dev returned 422 for vm_tag '{vm_tag}' and no SSH key is available"
                        ))
                    })?
            }
            ExedevAuth::Ssh { key_path } => {
                self.vm_key_path(vm_tag).unwrap_or_else(|| key_path.clone())
            }
            ExedevAuth::SshAlias { .. } => {
                if let Some(key_path) = self.vm_key_path(vm_tag) {
                    return self.run_ssh(&host, &cmd, timeout, &key_path).await;
                }
                return run_open_ssh(
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
                .await;
            }
        };
        self.run_ssh(&host, &cmd, timeout, &result).await
    }

    async fn put_file(
        &self,
        vm_tag: &str,
        source: &std::path::Path,
        destination: &str,
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
        if let Some(key_path) = self.guest_key_path(vm_tag) {
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
    ) -> Result<(), LsbxError> {
        let host = format!("{}.exe.xyz", vm_tag);
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ];
        if let Some(key_path) = self.guest_key_path(vm_tag) {
            args.extend(["-i".to_string(), key_path.display().to_string()]);
        }
        args.push(format!("{}:{}", host, source));
        args.push(destination.display().to_string());
        run_scp(args, Duration::from_secs(120)).await
    }

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        self.require_not_vm_scoped("delete a VM")?;
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
            let detail = String::from_utf8_lossy(&result.stderr).to_lowercase();
            if !detail.contains("not found") && !detail.contains("no such") {
                return Err(LsbxError::BackendUnavailable(format!(
                    "failed to revoke ephemeral SSH key: {}",
                    String::from_utf8_lossy(&result.stderr)
                )));
            }
        }
        self.destroy(vm_tag).await
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

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        Err(LsbxError::BackendUnavailable(
            "exe.dev's CLI/API does not document a rename verb for provisioned VMs".to_string(),
        ))
    }
}

/// Reconciles orphaned `lsbx:<label>`-tagged keys against exe.dev's own
/// key-listing call, via Unit 03's `lsbx_keys::reconcile::reconcile_orphaned_keys`
/// (Unit 07 acceptance criteria).
///
/// ## A real gap, flagged rather than papered over
///
/// Unit 07's interface contract lists exactly five output files (`Cargo.toml`,
/// `src/lib.rs`, `src/ssh.rs`, `src/http_fallback.rs`, two test files) and
/// gives no wire format for exe.dev's key-listing call anywhere in the unit
/// contract, the companion SPEC.md, or this repo's `docs/` tree (checked —
/// `docs/infra/exe-dev/` is referenced by the unit's Context section as
/// living in a *different*, companion repo, not this one). There is
/// therefore no confirmed real response shape to parse here.
///
/// This function implements the *mechanism* the acceptance criterion asks
/// for — call a key-listing verb, turn each line into a `TaggedKey` whose
/// `revoke` closure calls the corresponding revoke verb, hand the batch to
/// `reconcile_orphaned_keys` — against the most plausible reading of
/// exe.dev's own `ssh exe.dev <verb>` convention (`ls-keys` / `rm-key
/// <fingerprint>`, mirroring the already-confirmed `ls`/`rm <tag>` shape for
/// VMs). It has **not** been verified against a real exe.dev account, and
/// should not be trusted to be exactly right until it is — this is exactly
/// the kind of gap this house's "ran, but was it proven" standard exists to
/// name explicitly rather than let ride silently. `reconcile_key_leases`'s
/// own doc comment repeats this caveat at the call site.
async fn list_tagged_keys(
    backend: &ExedevBackend,
    run_cmd: &str,
) -> Result<Vec<lsbx_keys::reconcile::TaggedKey>, LsbxError> {
    let out = match &backend.auth {
        ExedevAuth::AccountToken { .. } => backend.run_http_account_level(run_cmd).await?,
        ExedevAuth::VmScopedToken { .. } => {
            return Err(LsbxError::BackendUnavailable(
                "cannot list account-level keys using a VM-scoped token".to_string(),
            ))
        }
        ExedevAuth::Ssh { key_path } => {
            backend
                .run_ssh("exe.dev", run_cmd, Duration::from_secs(35), key_path)
                .await?
        }
        ExedevAuth::SshAlias { alias } => backend.run_ssh_alias(alias, run_cmd).await?,
    };
    if out.exit_code != 0 {
        return Err(LsbxError::BackendUnavailable(format!(
            "failed to list exedev keys: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let auth_token = backend.auth.http_token().map(str::to_string);
    let ssh_key_path = match &backend.auth {
        ExedevAuth::Ssh { key_path } => Some(key_path.clone()),
        ExedevAuth::SshAlias { .. } => None,
        _ => backend.auth.fallback_ssh_key_path().cloned(),
    };

    let mut tagged_keys = Vec::new();
    // Expected line shape: "<fingerprint> <comment>" — one key per line,
    // mirroring the plain-text-line convention `ls -f json`'s fallback path
    // in `list_vms()` already assumes for this same CLI family. See this
    // function's doc comment: unverified against a real account.
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((fingerprint, comment)) = line.split_once(' ') else {
            continue;
        };
        let fingerprint = fingerprint.to_string();
        let comment = comment.trim().to_string();
        let revoke_cmd = format!("rm-key {}", fingerprint);
        let auth_token = auth_token.clone();
        let ssh_key_path = ssh_key_path.clone();

        tagged_keys.push(lsbx_keys::reconcile::TaggedKey {
            comment,
            revoke: Box::new(move || {
                // `TaggedKey::revoke` is a synchronous `FnOnce`
                // (`reconcile_orphaned_keys`'s own signature, Unit 03) —
                // this backend's actual exe.dev calls are async, so a
                // revoke spins up a short-lived Tokio runtime to bridge the
                // sync/async boundary rather than requiring the caller to
                // already be inside one. `block_on` inside an existing
                // Tokio runtime would panic, so this deliberately builds a
                // brand-new current-thread runtime rather than reaching for
                // `Handle::current()` — this closure has no way to know
                // whether its caller is already inside a runtime, and a new
                // runtime is always safe, just slightly more expensive.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| LsbxError::ContractViolated(format!("failed to start runtime for key revoke: {e}")))?;
                rt.block_on(async {
                    let out = match (&auth_token, &ssh_key_path) {
                        (Some(token), _) => {
                            let client = HttpFallbackClient::new(token.clone(), None);
                            match client.exec(&revoke_cmd).await? {
                                HttpExecOutcome::Completed(out) => out,
                                HttpExecOutcome::UnprocessableFallbackToSsh => {
                                    return Err(LsbxError::BackendUnavailable(format!(
                                        "exe.dev returned 422 revoking key '{fingerprint}' with no SSH fallback available"
                                    )))
                                }
                            }
                        }
                        (None, Some(key_path)) => {
                            let mut session = SshSession::connect(key_path, "root", "exe.dev", 22).await?;
                            session.exec(&revoke_cmd, Duration::from_secs(30)).await?
                        }
                        (None, None) => {
                            return Err(LsbxError::BackendUnavailable(
                                "no auth mode available to revoke key".to_string(),
                            ))
                        }
                    };
                    if out.exit_code == 0 {
                        Ok(())
                    } else {
                        Err(LsbxError::BackendUnavailable(format!(
                            "failed to revoke key '{fingerprint}': {}",
                            String::from_utf8_lossy(&out.stderr)
                        )))
                    }
                })
            }),
        });
    }

    Ok(tagged_keys)
}

/// Public entry point: lists exe.dev's currently registered keys, then
/// revokes every `lsbx:<label>`-tagged one not present in `known_labels`,
/// via Unit 03's `reconcile_orphaned_keys`. Returns the number of keys
/// revoked.
///
/// See `list_tagged_keys`'s doc comment: the underlying key-listing wire
/// format is this function's own best-effort interpretation, not something
/// verified against a real exe.dev account yet.
pub async fn reconcile_exedev_keys(
    backend: &ExedevBackend,
    known_labels: &[String],
) -> Result<usize, LsbxError> {
    let tagged_keys = list_tagged_keys(backend, "ls-keys").await?;
    lsbx_keys::reconcile::reconcile_orphaned_keys(tagged_keys, known_labels)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The fallback-key-path judgment call, exercised directly: an
    /// `AccountToken`/`VmScopedToken` auth mode with no
    /// `fallback_ssh_key_path` configured must report `None`, never a
    /// guessed default such as `~/.ssh/id_ed25519`. This is the single
    /// most important assertion for the judgment call documented on
    /// `ExedevAuth` above — it's what makes "no fallback path configured
    /// means a named error, not a silent guess" a checked fact rather than
    /// a claim in a doc comment.
    #[test]
    fn no_fallback_path_configured_means_none_not_a_guessed_default() {
        let account = ExedevAuth::account_token("EXE_TOKEN_VALUE");
        assert_eq!(account.fallback_ssh_key_path(), None);

        let vm_scoped = ExedevAuth::vm_scoped_token("v0@my-vm.exe.xyz");
        assert_eq!(vm_scoped.fallback_ssh_key_path(), None);
    }

    /// The explicit-opt-in half of the same judgment call: when a caller
    /// *does* configure a fallback path, it's threaded through exactly,
    /// unmodified — proving the configuration actually reaches the field
    /// `run()` reads from, not just that the constructor accepts an
    /// argument.
    #[test]
    fn configured_fallback_path_is_threaded_through_unmodified() {
        let path = PathBuf::from("/var/lib/lsbx/keys/some-ephemeral-key");
        let account = ExedevAuth::account_token_with_fallback("EXE_TOKEN_VALUE", path.clone());
        assert_eq!(account.fallback_ssh_key_path(), Some(&path));

        let vm_scoped = ExedevAuth::vm_scoped_token_with_fallback("v0@my-vm.exe.xyz", path.clone());
        assert_eq!(vm_scoped.fallback_ssh_key_path(), Some(&path));
    }

    /// `ExedevAuth::Ssh` has no fallback-path concept at all (there is no
    /// HTTP path to fall back *from* in this mode) — asserting `None` here
    /// specifically distinguishes "no fallback configured" (the
    /// HTTP-variant case above) from "fallback doesn't apply to this
    /// variant" (this case), even though both currently read as `None`.
    #[test]
    fn ssh_variant_has_no_fallback_path_concept() {
        let ssh = ExedevAuth::Ssh {
            key_path: PathBuf::from("/tmp/key"),
        };
        assert_eq!(ssh.fallback_ssh_key_path(), None);
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
