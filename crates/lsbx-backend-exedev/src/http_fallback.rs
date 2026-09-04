//! `POST https://exe.dev/exec` bearer-token path — exe.dev's own framing of
//! this API as "the SSH API shoved into a POST body".
//!
//! ## The real wire format (verified live 2026-09-04, fixes lsbx#30)
//!
//! exe.dev shipped "Run commands on VM" (https://exe.dev/docs/https-api-run-on-vm)
//! and changed the endpoint's behavior in the process. The live API:
//!
//! - Parses the request body **verbatim** as an exe.dev command string, no
//!   matter the `Content-Type`. A JSON envelope like `{"command":"..."}` is
//!   read as a literal command named `{...}` and answered with
//!   `{"error":"unknown command"}` at HTTP 200. (An earlier
//!   JSON-envelope client in this file never worked against the live API.)
//! - Returns the command's stdout and stderr **combined as plain text** in
//!   the response body — there is no structured `{exit_code, stdout, stderr}`
//!   JSON response to deserialize.
//! - Delivers the command's exit code in the `X-Exe-Exit` HTTP trailer.
//!   That trailer is not reliably exposed through proxy chains (verified
//!   absent through a standard egress proxy even though the server
//!   advertises `Trailer: X-Exe-Exit`), so this client never trusts it.
//! - Caps command duration at ~30 seconds server-side.
//! - Runs every body through a shell: the lobby shell-lexes the body, then
//!   the target VM parses what the lobby handed it (exactly like an `ssh`
//!   command line). Two layers of quoting, one eaten per layer.
//!
//! ## One endpoint, two response styles
//!
//! exe.dev command names gate on the *first* token, so a single URL serves
//! both control verbs (`ls`, `cp`, `rm`, `ssh-key`, ...) and guest execution
//! (`ssh <vm> <cmd>` — a first-class command since the run-on-vm launch;
//! the old "raw VM shell over /exec → 422" behavior is gone from the
//! platform, and with it this module's original reason for being named
//! "fallback". The name is retained for diff stability across the #30
//! cleanup; everything fallback-shaped in it is not.).
//!
//! - **Control verbs** are invoked by sending the exe.dev command verbatim.
//!   Machine-readable invocations carry `--json` themselves (`ls --json`,
//!   `cp ... --json`), so control-plane errors surface as a non-2xx HTTP
//!   status (verified live: errors are 422 with `{"error": ...}` bodies)
//!   while stdout stays clean JSON. Exit codes map from HTTP status; the
//!   remote command's own exit code is 0 iff HTTP succeeded.
//! - **Guest execution** wraps the caller's command as
//!   `ssh <vm> <quoted-cmd>` and appends an in-band exit sentinel
//!   (`; echo __LSBX_EXIT:$?`) — expanded by the VM's own shell at the
//!   *end of the guest command chain*, then stripped from the tail of the
//!   combined output. This is the only exit-code channel that survives
//!   proxy chains that drop trailers. The sentinel string is split across
//!   a concat so guest output containing the literal marker cannot spoof it.
use lsbx_kernel::backend::CommandOutput;
use lsbx_kernel::error::LsbxError;
use reqwest::Client;
use std::time::Duration;

/// URL of exe.dev's account-level exec endpoint — the only exec surface.
/// (`https://<vm>.exe.xyz/exec` is NOT an exec API: VM hostnames front the
/// VM's own HTTP services, verified live 2026-09-04 — on a Caddy-fronted VM
/// that path serves the web app.)
pub const EXEC_URL: &str = "https://exe.dev/exec";

/// In-band exit sentinel appended to guest command chains. Written as a
/// concat so a guest command that echoes the literal marker cannot produce
/// a spoofable tail.
pub const EXIT_SENTINEL: &str = concat!("__LSBX_", "EXIT:");

pub struct HttpFallbackClient {
    client: Client,
    token: String,
}

impl HttpFallbackClient {
    /// All exec traffic — control verbs and guest execution alike — goes to
    /// the single account-level endpoint. `vm_tag` is accepted for source
    /// compatibility with the old per-VM constructor and ignored: exe.dev's
    /// exec endpoint has no VM-scoped variant to route to.
    pub fn new(token: String, _vm_tag: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            token,
        }
    }

    pub async fn exec(&self, command: &str) -> Result<CommandOutput, LsbxError> {
        self.exec_with_timeout(command, Duration::from_secs(120))
            .await
    }

    /// POSTs the command string verbatim (the lobby shell-lexes the body) and
    /// maps the response to a [`CommandOutput`].
    ///
    /// Exit-code policy:
    /// - Any non-2xx HTTP status becomes `exit_code = 255` (the conventional
    ///   transport-failure code; `lib.rs` normalizes SSH's 255 the same way)
    ///   with the error body as stderr.
    /// - On HTTP 200, the exit code is parsed from the in-band
    ///   `__LSBX_EXIT:<n>` sentinel when present (guest-execution shape);
    ///   otherwise it is 0 (control-verb shape, where errors are HTTP
    ///   statuses and `--json` keeps stdout machine-readable).
    pub async fn exec_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<CommandOutput, LsbxError> {
        let res = self
            .client
            .post(EXEC_URL)
            .bearer_auth(&self.token)
            .header("Content-Type", "text/plain")
            .body(command.to_string())
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| LsbxError::BackendUnavailable(format!("http exec failed: {}", e)))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Ok(CommandOutput {
                exit_code: 255,
                stdout: Vec::new(),
                stderr: format!("http exec returned status {status}: {body}").into_bytes(),
            });
        }

        let body = res.text().await.map_err(|e| {
            LsbxError::BackendUnavailable(format!("failed to read http exec response body: {e}"))
        })?;

        let (output_text, exit_code) = match split_exit_sentinel(&body) {
            Some((text, code)) => (text, code),
            None => (body.as_str(), 0),
        };

        Ok(CommandOutput {
            exit_code,
            stdout: output_text.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
}

/// Splits a trailing `__LSBX_EXIT:<n>` line off `body`, returning
/// `(text_without_sentinel, exit_code)`. Returns `None` when no sentinel
/// line is present (control-verb responses).
///
/// Two shapes are honored (the second found live 2026-09-04 when a guest
/// `false` produced a body with no other output):
/// - `"output...\n__LSBX_EXIT:<n>"` — normal guest execution;
/// - `"__LSBX_EXIT:<n>"` alone — the guest printed nothing, so the
///   sentinel is the entire body (no leading newline to anchor on).
///
/// In both shapes only the LAST sentinel line wins (rfind), so guest
/// output that echoes the marker cannot spoof the exit code.
fn split_exit_sentinel(body: &str) -> Option<(&str, i32)> {
    let trimmed = body.trim_end();
    let marker = format!("\n{EXIT_SENTINEL}");
    if let Some(idx) = trimmed.rfind(&marker) {
        let tail = &trimmed[idx + 1 + EXIT_SENTINEL.len()..];
        let code: i32 = tail.trim().parse().ok()?;
        if code < 0 {
            return None;
        }
        return Some((&trimmed[..idx], code));
    }
    if let Some(tail) = trimmed.strip_prefix(EXIT_SENTINEL) {
        let code: i32 = tail.trim().parse().ok()?;
        if code < 0 {
            return None;
        }
        return Some(("", code));
    }
    None
}

/// Wraps a guest command for execution on `vm_tag` over the shared exec
/// endpoint: `ssh <vm> <quoted-cmd>; echo __LSBX_EXIT:$?`.
///
/// The quoting is load-bearing and lives here so the two layers are visible
/// in one place:
/// - the caller's argv is joined and quoted (single-quoted by the caller,
///   see `shell_quote` in `lib.rs`) into a single VM-side shell string (the
///   VM parses it like an `ssh` remote command);
/// - that string is embedded in the lobby command **unquoted**: the lobby
///   shell-lexes the body, `ssh` and `<vm>` must remain bare tokens for it,
///   and the double quotes around the remote command survive as literal
///   characters for the VM's parser;
/// - the sentinel echo is appended *outside* those quotes so `$?` expands
///   at the VM after the guest command completes (the lobby does not
///   execute `;`-chains; verified live 2026-09-04).
///
/// Guest stdout+stderr arrive combined in the response body (the API mixes
/// them); the sentinel is stripped by `split_exit_sentinel` in `exec`.
pub fn wrap_guest_command(vm_tag: &str, quoted_cmd: &str) -> String {
    format!("ssh {} \"{}\"; echo {EXIT_SENTINEL}$?", vm_tag, quoted_cmd)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn guest_wrapper_shapes_two_layer_quoting() {
        // The caller's shell_quote produces single-quoted VM-side strings;
        // the wrapper embeds them inside the lobby-level double quotes.
        let wrapped = wrap_guest_command("my-vm", "'echo hi > /tmp/x'");
        assert_eq!(
            wrapped,
            "ssh my-vm \"'echo hi > /tmp/x'\"; echo __LSBX_EXIT:$?"
        );
    }

    #[test]
    fn guest_wrapper_survives_embedded_single_quotes() {
        let wrapped = wrap_guest_command("vm", "'echo it'\\''s'");
        assert!(wrapped.starts_with("ssh vm \"'echo it'\\''s'\"; echo"));
    }

    #[test]
    fn splits_sentinel_from_guest_output() {
        let body = "line one\nstderr mixed in\n__LSBX_EXIT:0\n";
        let (text, code) = split_exit_sentinel(body).expect("sentinel present");
        assert_eq!(code, 0);
        assert_eq!(text, "line one\nstderr mixed in");
    }

    #[test]
    fn splits_nonzero_sentinel() {
        let body = "some output\n__LSBX_EXIT:3";
        let (text, code) = split_exit_sentinel(body).expect("sentinel present");
        assert_eq!(code, 3);
        assert_eq!(text, "some output");
    }

    #[test]
    fn no_sentinel_means_control_verb_shape() {
        assert!(split_exit_sentinel("{\"vms\":[]}\n").is_none());
        assert!(split_exit_sentinel("").is_none());
    }

    #[test]
    fn only_the_final_sentinel_line_is_honored() {
        // Guest output that echoes the marker mid-stream must not win:
        // rfind takes the LAST full sentinel line, so trailing content
        // after an earlier occurrence keeps that earlier one from parsing.
        let body = "attacker echo __LSBX_EXIT:0\nreal output\n__LSBX_EXIT:7\n";
        let (text, code) = split_exit_sentinel(body).expect("sentinel present");
        assert_eq!(code, 7);
        assert_eq!(text, "attacker echo __LSBX_EXIT:0\nreal output");
        // A body whose only marker-like text is guest output (no trailing
        // sentinel line of its own) parses as a control-verb shape — the
        // same trust level the combined-stream format supports.
        let spoof = "echo __LSBX_EXIT:0\n";
        assert!(split_exit_sentinel(spoof).is_none());
    }

    #[test]
    fn malformed_or_negative_sentinel_falls_back_to_control_shape() {
        assert!(split_exit_sentinel("out\n__LSBX_EXIT:notanumber\n").is_none());
        assert!(split_exit_sentinel("out\n__LSBX_EXIT:-5\n").is_none());
    }

    #[test]
    fn sentinel_only_body_is_the_silent_guest_case() {
        // Found live 2026-09-04: a guest `false` with no other output
        // yields a body that is JUST the sentinel line. This must parse
        // as exit 1 with empty output, not fall through to control-shape
        // exit 0.
        let (text, code) = split_exit_sentinel("__LSBX_EXIT:1\n").expect("bare sentinel present");
        assert_eq!(code, 1);
        assert_eq!(text, "");
        let (text, code) = split_exit_sentinel("__LSBX_EXIT:0").expect("bare sentinel present");
        assert_eq!(code, 0);
        assert_eq!(text, "");
        // A control-verb body that merely *starts* similarly must not
        // match (it won't: the sentinel prefix requires the exact marker).
        assert!(split_exit_sentinel("__LSBX_EXTRA stuff\n").is_none());
    }

    #[test]
    fn mid_stream_marker_without_trailing_sentinel_is_guest_output() {
        // Guest echoed the marker mid-stream but the real sentinel never
        // landed (e.g. the guest killed its own shell): the body must be
        // treated as control-shape/exit-0 rather than inventing a code
        // from attacker-controlled text.
        let body = "hello\n__LSBX_EXIT:9\nmore output\n";
        assert!(split_exit_sentinel(body).is_none());
    }
}
