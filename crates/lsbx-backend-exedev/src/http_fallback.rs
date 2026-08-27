//! `POST https://exe.dev/exec` bearer-token path — exe.dev's own framing of
//! this API as "the SSH API shoved into a POST body" (Unit 07 Context).
//!
//! ## The 422 limitation
//!
//! A raw-VM-shell invocation through this endpoint can return `422
//! Unprocessable Entity` for some shell invocations that a real SSH session
//! reaches without issue — a known, documented exe.dev API limitation, not a
//! bug in this client. This module surfaces that specific case as a typed
//! [`HttpExecOutcome::UnprocessableFallbackToSsh`] rather than folding it
//! into a generic error, so `lib.rs::run()` can detect it and transparently
//! retry over SSH instead of surfacing a bare 422 to the caller (Unit 07
//! acceptance criteria).
use lsbx_kernel::backend::CommandOutput;
use lsbx_kernel::error::LsbxError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
struct ExecRequest {
    command: String,
}

#[derive(Deserialize)]
struct ExecResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// The outcome of one `/exec` call, distinguishing the documented 422
/// raw-shell limitation from every other failure mode.
pub enum HttpExecOutcome {
    Completed(CommandOutput),
    /// `POST /exec` returned 422. The caller (`lib.rs::run()`) should retry
    /// over SSH rather than surface this to whoever called `Backend::run`.
    UnprocessableFallbackToSsh,
}

pub struct HttpFallbackClient {
    client: Client,
    token: String,
    url: String,
}

impl HttpFallbackClient {
    /// `vm_tag`, when present, scopes the request to a specific VM's own
    /// `/exec` endpoint (`https://<vm_tag>.exe.xyz/exec`) rather than the
    /// account-level `https://exe.dev/exec`. Account-level verbs (`new`,
    /// `ls`, `rm <tag>`) always go to the bare host; VM-scoped commands
    /// (`run`) go to the VM-scoped host when a `vm_tag` is available.
    pub fn new(token: String, vm_tag: Option<&str>) -> Self {
        let url = match vm_tag {
            Some(tag) => format!("https://{}.exe.xyz/exec", tag),
            None => "https://exe.dev/exec".to_string(),
        };

        Self {
            client: Client::new(),
            token,
            url,
        }
    }

    pub async fn exec(&self, command: &str) -> Result<HttpExecOutcome, LsbxError> {
        self.exec_with_timeout(command, Duration::from_secs(120))
            .await
    }

    pub async fn exec_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<HttpExecOutcome, LsbxError> {
        let res = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&ExecRequest {
                command: command.to_string(),
            })
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| LsbxError::BackendUnavailable(format!("http exec failed: {}", e)))?;

        if res.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Ok(HttpExecOutcome::UnprocessableFallbackToSsh);
        }

        if !res.status().is_success() {
            return Err(LsbxError::BackendUnavailable(format!(
                "http exec returned status: {}",
                res.status()
            )));
        }

        let body: ExecResponse = res.json().await.map_err(|e| {
            LsbxError::BackendUnavailable(format!("failed to parse http exec response: {}", e))
        })?;

        Ok(HttpExecOutcome::Completed(CommandOutput {
            exit_code: body.exit_code,
            // The wire format is JSON text, so exe.dev's `/exec` response
            // necessarily hands back `String`, not raw bytes — but the
            // kernel `CommandOutput.stdout`/`.stderr` contract is `Vec<u8>`
            // (lossless; a guest command's output isn't guaranteed to be
            // valid UTF-8). Converting via `.into_bytes()` here is lossless
            // on this side; whatever *display* layer wants a printable
            // string can do the (potentially lossy) UTF-8 decode itself —
            // that decision doesn't belong baked into this kernel type.
            stdout: body.stdout.into_bytes(),
            stderr: body.stderr.into_bytes(),
        }))
    }
}
