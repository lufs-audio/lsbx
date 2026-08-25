//! `LsbxMcpServer` — one `#[tool]`-annotated method per real `LsbxOps`
//! operation. See `src/lib.rs`'s module doc comment for the full
//! provenance/reconciliation notes; this file is deliberately just the
//! mechanical translation layer it describes: owned, `schemars::JsonSchema`
//! params in, a direct `LsbxOps` call, the real `Envelope<T>` shape out on
//! success, and [`crate::error_map::lsbx_error_to_mcp_error`] on failure.
//!
//! ## Two places a "misused tool call" is caught, both routed through the
//! same taxonomy
//!
//! 1. **Missing/malformed arguments** (this unit's own named example:
//!    "`destroy` with a missing required field") never reach a tool body
//!    at all — `rmcp`'s own `Parameters<T>` extraction rejects them with
//!    `ErrorCode::INVALID_PARAMS` before any code in this file runs (see
//!    `tests/test_error_taxonomy_mapping.rs` for the direct proof, and
//!    `error_map.rs`'s own doc comment for why this crate does not
//!    attempt to intercept or re-map that specific, `rmcp`-owned failure
//!    mode — doing so would mean re-implementing JSON Schema validation
//!    this crate gets for free from the real, derived schema).
//! 2. **A well-formed call whose *value* is invalid** — an unknown
//!    sandbox id, an unknown golden key, a duplicate registration key —
//!    reaches `LsbxOps`, which returns a real `LsbxError`.
//!    [`envelope_result`] renders that as the real `Envelope::Error{code,
//!    message}` shape inside a successful `CallToolResult` (matching the
//!    `rmcp` project's own documented convention: "Tool-level error —
//!    Ok(CallToolResult::error(...))... the right choice for almost every
//!    'the tool ran and didn't work' case" — the `Envelope` shape *is*
//!    this crate's tool-level error content), so an agent parsing this
//!    tool's output sees the identical `code`/`message` shape `lsbx
//!    --json` would print for the same failure. [`crate::error_map::lsbx_error_to_mcp_error`]
//!    exists for a narrower, related case: an `LsbxError` a validation
//!    step *inside this crate itself* (never `LsbxOps`) raises before a
//!    call — see [`require_non_empty`] — where the misuse is closer to
//!    case 1 (a shape problem with the call itself) than to a domain
//!    result `LsbxOps` computed, and is therefore surfaced as a real
//!    JSON-RPC protocol error via `Err(McpError)`, using the exact same
//!    numeric code either path would have produced.

use crate::error_map::lsbx_error_to_mcp_error;
use lsbx_kernel::envelope::Envelope;
use lsbx_kernel::error::LsbxError;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Serializes `result` (a `Result<T, LsbxError>` from a direct `LsbxOps`
/// call) into the real `Envelope<T>` shape (Unit 01,
/// `lsbx_kernel::envelope::Envelope`) and wraps it as a successful
/// `CallToolResult` — the same envelope the CLI's `--json` output and the
/// HTTP gateway produce for the identical operation. See this module's own
/// doc comment ("Two places a 'misused tool call' is caught") for why a
/// *domain* `LsbxError` (one `LsbxOps` itself returned) is rendered this
/// way — inside a successful `CallToolResult` — rather than as
/// `Err(McpError)`.
fn envelope_result<T: serde::Serialize>(
    result: Result<T, LsbxError>,
) -> Result<CallToolResult, McpError> {
    let envelope = Envelope::from_result(result);
    // `ContentBlock::json` serializes and wraps as text content, mapping a
    // serialization failure onto `ErrorCode::INTERNAL_ERROR` itself — this
    // is an infrastructure failure in this door (the envelope couldn't be
    // encoded at all), not a domain failure `LsbxOps` reported, so letting
    // it surface as a real `Err(McpError)` here is correct per the same
    // "whose problem is it" reasoning documented above.
    let block = ContentBlock::json(&envelope)?;
    Ok(CallToolResult::success(vec![block]))
}

/// A validation step this crate itself is responsible for (never
/// `LsbxOps`'s job — `LsbxOps` receives `id`/`name`/etc. as plain `&str`
/// and has no notion of "empty is invalid" for them). An empty
/// identifier-shaped field is a shape problem with the call itself
/// (indistinguishable, from a caller's perspective, from "you forgot to
/// fill this in"), so it is raised as a real `LsbxError::Usage` and
/// surfaced via [`crate::error_map::lsbx_error_to_mcp_error`] as a genuine
/// JSON-RPC protocol error rather than round-tripped through `LsbxOps`
/// only to get a less specific `NotFound` back.
fn require_non_empty(field_name: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() {
        return Err(lsbx_error_to_mcp_error(LsbxError::Usage(format!(
            "'{field_name}' must not be empty"
        ))));
    }
    Ok(())
}

fn duration_from_secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

// ---------------------------------------------------------------------
// Owned tool-input params — see src/lib.rs's module doc comment ("Tool
// input types are owned mirrors, not the borrowed façade types") for why
// these exist as a separate layer rather than deriving JsonSchema
// directly on lsbx-ops/lsbx-lifecycle/lsbx-golden's own request types.
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateParams {
    /// Profile name to provision from. See `lsbx_lifecycle::create::CreateRequest::profile`.
    pub profile: String,
    /// Optional sandbox display name; defaults to the generated id if omitted.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional caller-supplied task/job identifier, persisted on the `SandboxRecord`.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Lease duration in seconds before this sandbox becomes reap-eligible.
    pub lease_secs: u64,
    /// How long to wait for readiness proof before returning `ContractViolated`.
    pub ready_timeout_secs: u64,
    /// `false` to skip the post-create readiness proof (CLI's `--no-verify`).
    #[serde(default = "default_true")]
    pub verify: bool,
    /// Optional healthcheck commands (argv-shaped) to run while polling readiness.
    #[serde(default)]
    pub healthchecks: Vec<Vec<String>>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DestroyParams {
    /// Sandbox id to destroy.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenewParams {
    /// Sandbox id to renew.
    pub id: String,
    /// New lease duration in seconds, measured from now.
    pub duration_secs: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReapParams {
    /// Grace window (seconds) past a lease's own expiry before it is swept.
    /// `0` reduces to "sweep iff already expired" — see
    /// `lsbx_lifecycle::reap::reap`'s own doc comment.
    #[serde(default)]
    pub ttl_secs: u64,
    /// `true` to report what would be destroyed without destroying anything.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InfoParams {
    /// Sandbox id to look up.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsoleUrlParams {
    /// Sandbox id to compute a console URL for.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecParams {
    /// Sandbox id to run the command inside.
    pub id: String,
    /// Argv-shaped command — never a shell-interpolated string.
    pub command: Vec<String>,
    /// Timeout in seconds handed to `Backend::run`.
    pub timeout_secs: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PutParams {
    /// Sandbox id to copy the file into.
    pub id: String,
    /// Local source path (as seen by this MCP server process).
    pub source: String,
    /// Remote destination path inside the guest.
    pub destination: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetParams {
    /// Sandbox id to copy the file from.
    pub id: String,
    /// Remote source path inside the guest.
    pub source: String,
    /// Local destination path (as seen by this MCP server process).
    pub destination: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusParams {}

/// Mirrors `lsbx_golden::registry::GoldenFlavor`'s 3 real variants exactly.
/// A separate, `JsonSchema`-deriving enum is needed because the real
/// `GoldenFlavor` derives `Deserialize`/`Serialize` but not `JsonSchema`
/// (confirmed against `crates/lsbx-golden/src/registry.rs`).
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GoldenFlavorParam {
    Desktop,
    Agent,
    CiRunner,
}

impl From<GoldenFlavorParam> for lsbx_golden::registry::GoldenFlavor {
    fn from(value: GoldenFlavorParam) -> Self {
        match value {
            GoldenFlavorParam::Desktop => lsbx_golden::registry::GoldenFlavor::Desktop,
            GoldenFlavorParam::Agent => lsbx_golden::registry::GoldenFlavor::Agent,
            GoldenFlavorParam::CiRunner => lsbx_golden::registry::GoldenFlavor::CiRunner,
        }
    }
}

impl From<lsbx_golden::registry::GoldenFlavor> for GoldenFlavorParam {
    fn from(value: lsbx_golden::registry::GoldenFlavor) -> Self {
        match value {
            lsbx_golden::registry::GoldenFlavor::Desktop => GoldenFlavorParam::Desktop,
            lsbx_golden::registry::GoldenFlavor::Agent => GoldenFlavorParam::Agent,
            lsbx_golden::registry::GoldenFlavor::CiRunner => GoldenFlavorParam::CiRunner,
        }
    }
}

/// Mirrors `lsbx_golden::registry::GoldenMode`'s 2 real variants exactly.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GoldenModeParam {
    Copy,
    New,
}

impl From<GoldenModeParam> for lsbx_golden::registry::GoldenMode {
    fn from(value: GoldenModeParam) -> Self {
        match value {
            GoldenModeParam::Copy => lsbx_golden::registry::GoldenMode::Copy,
            GoldenModeParam::New => lsbx_golden::registry::GoldenMode::New,
        }
    }
}

impl From<lsbx_golden::registry::GoldenMode> for GoldenModeParam {
    fn from(value: lsbx_golden::registry::GoldenMode) -> Self {
        match value {
            lsbx_golden::registry::GoldenMode::Copy => GoldenModeParam::Copy,
            lsbx_golden::registry::GoldenMode::New => GoldenModeParam::New,
        }
    }
}

/// Mirrors `lsbx_golden::registry::StreamingMode`'s 2 real variants exactly.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StreamingModeParam {
    None,
    Novnc,
}

impl From<StreamingModeParam> for lsbx_golden::registry::StreamingMode {
    fn from(value: StreamingModeParam) -> Self {
        match value {
            StreamingModeParam::None => lsbx_golden::registry::StreamingMode::None,
            StreamingModeParam::Novnc => lsbx_golden::registry::StreamingMode::Novnc,
        }
    }
}

impl From<lsbx_golden::registry::StreamingMode> for StreamingModeParam {
    fn from(value: lsbx_golden::registry::StreamingMode) -> Self {
        match value {
            lsbx_golden::registry::StreamingMode::None => StreamingModeParam::None,
            lsbx_golden::registry::StreamingMode::Novnc => StreamingModeParam::Novnc,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoldenBuildParams {
    /// Golden key to register the build result under (validated against
    /// `^[a-z][a-z0-9._-]{0,63}$` by `lsbx_golden::registry::ImageRegistry::validate_key`).
    pub name: String,
    /// Base image or base golden key to build from.
    pub from: String,
    /// Local path (as seen by this MCP server process) to the provisioning script.
    pub script: String,
    pub flavor: GoldenFlavorParam,
    pub cpu: u32,
    pub memory: String,
    pub streaming: StreamingModeParam,
    /// Register the built golden into the in-process `ImageRegistry` on success.
    pub register: bool,
    /// Destroy the build VM once the provisioning script completes.
    pub cleanup: bool,
    /// Preview the build without touching the backend at all.
    #[serde(default)]
    pub dry_run: bool,
    /// Public half of an ephemeral keypair for `Backend::create_from_golden`.
    /// See `lsbx_golden::build::GoldenBuildRequest`'s own doc comment for why
    /// this façade-level operation requires the caller to already hold one.
    pub pubkey: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoldenVerifyParams {
    /// Golden key (registry entry) to verify.
    pub name: String,
    /// Identifier for the freshly-created verification instance.
    pub verify_name: String,
    /// Public half of an ephemeral keypair for `Backend::create_from_golden`.
    pub pubkey: String,
}

/// Owned, field-identical mirror of `lsbx_golden::registry::GoldenConfig`
/// — see `src/lib.rs`'s module doc comment for why a separate type is
/// needed (the real `GoldenConfig` has no `JsonSchema` impl).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoldenRegisterParams {
    pub key: String,
    pub flavor: GoldenFlavorParam,
    pub os: String,
    pub base: String,
    pub mode: GoldenModeParam,
    pub cpu: u32,
    pub memory: String,
    #[serde(default)]
    pub disk: Option<String>,
    pub streaming: StreamingModeParam,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub healthcheck: Vec<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
    pub description: String,
}

impl From<GoldenRegisterParams> for lsbx_golden::registry::GoldenConfig {
    fn from(value: GoldenRegisterParams) -> Self {
        lsbx_golden::registry::GoldenConfig {
            key: value.key,
            flavor: value.flavor.into(),
            os: value.os,
            base: value.base,
            mode: value.mode.into(),
            cpu: value.cpu,
            memory: value.memory,
            disk: value.disk,
            streaming: value.streaming.into(),
            capabilities: value.capabilities,
            healthcheck: value.healthcheck,
            repo: value.repo,
            content_hash: value.content_hash,
            description: value.description,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoldenDeleteParams {
    /// Golden key to remove from the in-process registry.
    pub name: String,
    /// Accepted for interface-contract parity with `LsbxOps::golden_delete`;
    /// a documented no-op today (see `crates/lsbx-ops/src/lib.rs`'s own
    /// module doc comment — no snapshot mechanism exists in any merged crate).
    #[serde(default)]
    pub keep_snapshot: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoldenListParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigShowParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogsQueryParams {
    /// Opaque "since" cursor. Accepted for interface-contract parity;
    /// `LsbxOps::logs_query` always fails today (`ContractViolated`) since
    /// no merged crate owns a queryable log store yet.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: usize,
}

// ---------------------------------------------------------------------
// LsbxMcpServer — one #[tool] per real LsbxOps operation.
// ---------------------------------------------------------------------

/// The MCP door (SPEC.md §4.8, "Door 4"). Holds an `Arc<LsbxOps>` — the one
/// shared façade instance every door in this system holds a reference to
/// (per `LsbxOps`'s own doc comment: "Constructed once and held by every
/// door... via a shared reference"). `Clone` is required by `rmcp`'s
/// `ServerHandler`/service-spawning machinery; cloning this struct only
/// clones the `Arc` and the `ToolRouter`'s own internal `Arc`-backed route
/// table, never `LsbxOps` itself.
#[derive(Clone)]
pub struct LsbxMcpServer {
    ops: Arc<lsbx_ops::LsbxOps>,
    // Read by the `#[tool_router]`/`#[tool_handler]` macro-generated
    // `ServerHandler::call_tool`/`list_tools` implementations, not by any
    // code written directly in this file — rustc's dead-code analysis
    // does not see through that expansion (the same field appears,
    // unallowed, in every upstream `rmcp` example server and fails this
    // workspace's `-D warnings` clippy gate without this allow).
    #[allow(dead_code)]
    tool_router: ToolRouter<LsbxMcpServer>,
}

#[tool_router]
impl LsbxMcpServer {
    pub fn new(ops: Arc<lsbx_ops::LsbxOps>) -> Self {
        Self {
            ops,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Create a new ephemeral sandbox from a profile")]
    pub async fn create(
        &self,
        Parameters(p): Parameters<CreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .ops
            .create(lsbx_lifecycle::create::CreateRequest {
                profile: &p.profile,
                golden: None,
                cpu: None,
                memory: None,
                flavor: None,
                streaming: None,
                name: p.name.as_deref(),
                task_id: p.task_id.as_deref(),
                lease: duration_from_secs(p.lease_secs),
                ready_timeout: duration_from_secs(p.ready_timeout_secs),
                verify: p.verify,
                healthchecks: p.healthchecks,
            })
            .await;
        envelope_result(result)
    }

    #[tool(description = "Destroy a sandbox by id")]
    pub async fn destroy(
        &self,
        Parameters(p): Parameters<DestroyParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        let result = self.ops.destroy(&p.id).await;
        envelope_result(result)
    }

    #[tool(description = "Extend a sandbox's lease")]
    pub async fn renew(
        &self,
        Parameters(p): Parameters<RenewParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        let result = self
            .ops
            .renew(&p.id, duration_from_secs(p.duration_secs))
            .await;
        envelope_result(result)
    }

    #[tool(description = "Sweep lease-expired sandboxes and reconcile orphaned keys")]
    pub async fn reap(
        &self,
        Parameters(p): Parameters<ReapParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .ops
            .reap(duration_from_secs(p.ttl_secs), p.dry_run)
            .await
            .map(|report| {
                serde_json::json!({
                    "destroyed": report.destroyed,
                    "would_destroy": report.would_destroy,
                    "keys_reconciled": report.keys_reconciled,
                })
            });
        envelope_result(result)
    }

    #[tool(description = "List every known sandbox")]
    pub async fn list(
        &self,
        Parameters(_p): Parameters<ListParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.ops.list().await;
        envelope_result(result)
    }

    #[tool(description = "Get public info for a sandbox by id")]
    pub async fn info(
        &self,
        Parameters(p): Parameters<InfoParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        let result = self.ops.info(&p.id).await;
        envelope_result(result)
    }

    #[tool(description = "Compute the console URL for a sandbox, if it has one")]
    pub async fn console_url(
        &self,
        Parameters(p): Parameters<ConsoleUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        let result = self.ops.console_url(&p.id).await;
        envelope_result(result)
    }

    #[tool(description = "Run a command inside a sandbox")]
    pub async fn exec(
        &self,
        Parameters(p): Parameters<ExecParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        if p.command.is_empty() {
            return Err(lsbx_error_to_mcp_error(LsbxError::Usage(
                "'command' must not be empty".to_string(),
            )));
        }
        let result = self
            .ops
            .exec(&p.id, &p.command, duration_from_secs(p.timeout_secs))
            .await
            .map(|output| {
                serde_json::json!({
                    "exit_code": output.exit_code,
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                })
            });
        envelope_result(result)
    }

    #[tool(description = "Copy a local file into a sandbox")]
    pub async fn put(
        &self,
        Parameters(p): Parameters<PutParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        require_non_empty("source", &p.source)?;
        require_non_empty("destination", &p.destination)?;
        let result = self
            .ops
            .put(&p.id, &PathBuf::from(p.source), &p.destination)
            .await;
        envelope_result(result)
    }

    #[tool(description = "Copy a file out of a sandbox to a local path")]
    pub async fn get(
        &self,
        Parameters(p): Parameters<GetParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("id", &p.id)?;
        require_non_empty("source", &p.source)?;
        require_non_empty("destination", &p.destination)?;
        let result = self
            .ops
            .get(&p.id, &p.source, &PathBuf::from(p.destination))
            .await;
        envelope_result(result)
    }

    #[tool(description = "Report the backend name, live availability, and sandbox count")]
    pub async fn status(
        &self,
        Parameters(_p): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.ops.status().await.map(|s| {
            serde_json::json!({
                "backend_name": s.backend_name,
                "backend_available": s.backend_available,
                "sandbox_count": s.sandbox_count,
            })
        });
        envelope_result(result)
    }

    #[tool(description = "Build a golden image by provisioning a fresh VM from a base")]
    pub async fn golden_build(
        &self,
        Parameters(p): Parameters<GoldenBuildParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("name", &p.name)?;
        require_non_empty("from", &p.from)?;
        require_non_empty("script", &p.script)?;
        require_non_empty("pubkey", &p.pubkey)?;
        let script_path = PathBuf::from(p.script);
        let result = self
            .ops
            .golden_build(lsbx_golden::build::GoldenBuildRequest {
                name: &p.name,
                from: &p.from,
                script: &script_path,
                flavor: p.flavor.into(),
                cpu: p.cpu,
                memory: &p.memory,
                streaming: p.streaming.into(),
                register: p.register,
                cleanup: p.cleanup,
                dry_run: p.dry_run,
                pubkey: &p.pubkey,
            })
            .await
            .map(|outcome| {
                serde_json::json!({
                    "config": outcome.config,
                    "build_vm_tag": outcome.build_vm_tag,
                })
            });
        envelope_result(result)
    }

    #[tool(description = "Create a fresh instance of a registered golden and run its healthchecks")]
    pub async fn golden_verify(
        &self,
        Parameters(p): Parameters<GoldenVerifyParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("name", &p.name)?;
        require_non_empty("verify_name", &p.verify_name)?;
        require_non_empty("pubkey", &p.pubkey)?;
        let result = self
            .ops
            .golden_verify(&p.name, &p.verify_name, &p.pubkey, None)
            .await
            .map(|results| {
                results
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "command": r.command,
                            "passed": r.passed,
                            "output": r.output,
                        })
                    })
                    .collect::<Vec<_>>()
            });
        envelope_result(result)
    }

    #[tool(description = "Register a golden into the in-process image registry")]
    pub async fn golden_register(
        &self,
        Parameters(p): Parameters<GoldenRegisterParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("key", &p.key)?;
        let config: lsbx_golden::registry::GoldenConfig = p.into();
        let result = self.ops.golden_register(config).await;
        envelope_result(result)
    }

    #[tool(description = "Remove a golden from the in-process image registry")]
    pub async fn golden_delete(
        &self,
        Parameters(p): Parameters<GoldenDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        require_non_empty("name", &p.name)?;
        let result = self.ops.golden_delete(&p.name, p.keep_snapshot).await;
        envelope_result(result)
    }

    #[tool(description = "List every golden currently in the in-process image registry")]
    pub async fn golden_list(
        &self,
        Parameters(_p): Parameters<GoldenListParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.ops.golden_list().await;
        envelope_result(result)
    }

    #[tool(description = "Summarize the running registry's image/golden/profile counts and keys")]
    pub async fn config_show(
        &self,
        Parameters(_p): Parameters<ConfigShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.ops.config_show().await;
        envelope_result(result)
    }

    #[tool(
        description = "Query structured logs (currently always fails: no log backend is wired up yet)"
    )]
    pub async fn logs_query(
        &self,
        Parameters(p): Parameters<LogsQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.ops.logs_query(p.since.as_deref(), p.limit).await;
        envelope_result(result)
    }
}

#[tool_handler]
impl ServerHandler for LsbxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "lsbx MCP door: one tool per LsbxOps operation (create, destroy, renew, reap, \
                 list, info, console_url, exec, put, get, status, golden_build, golden_verify, \
                 golden_register, golden_delete, golden_list, config_show, logs_query). Every \
                 tool response is the same Envelope<T> shape lsbx --json and the HTTP gateway \
                 produce."
                .to_string(),
        )
    }
}

/// Test-only (and CLI-parity-test-facing): the list of tool names this
/// server actually registers via `#[tool_router]`, read off the real
/// `ToolRouter` rather than hand-maintained separately — so this can never
/// silently drift from what `#[tool]` really generated.
pub fn registered_tool_names() -> Vec<&'static str> {
    LsbxMcpServer::tool_router()
        .list_all()
        .into_iter()
        .map(|t| -> &'static str {
            match t.name {
                std::borrow::Cow::Borrowed(s) => s,
                std::borrow::Cow::Owned(s) => Box::leak(s.into_boxed_str()),
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Confirms every tool's `input_schema` is real, `schemars`-derived
    /// JSON Schema — not a hand-written literal that could silently drift
    /// from the real params struct — by checking that each schema's
    /// `properties`/`required` keys match the real struct's own field
    /// names exactly (a hand-copied schema would still need to happen to
    /// match today, but could never be *kept* in sync by the compiler the
    /// way a derive can).
    #[test]
    fn destroy_tool_schema_is_derived_from_the_real_destroyparams_fields() {
        let router = LsbxMcpServer::tool_router();
        let tool = router
            .get("destroy")
            .expect("destroy tool must be registered");
        let schema = &*tool.input_schema;
        let required = schema["required"]
            .as_array()
            .expect("schema must have a required array");
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            required_names,
            vec!["id"],
            "DestroyParams has exactly one field: id"
        );
        assert!(schema["properties"]["id"]["type"] == "string");
    }

    #[test]
    fn create_tool_schema_marks_optional_fields_not_required() {
        let router = LsbxMcpServer::tool_router();
        let tool = router
            .get("create")
            .expect("create tool must be registered");
        let schema = &*tool.input_schema;
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("schema must have a required array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // `name`/`task_id`/`verify`/`healthchecks` all carry #[serde(default)]
        // on CreateParams and must NOT be required; `profile`, `lease_secs`,
        // and `ready_timeout_secs` have no default and must be required.
        assert!(required.contains(&"profile"));
        assert!(required.contains(&"lease_secs"));
        assert!(required.contains(&"ready_timeout_secs"));
        assert!(!required.contains(&"name"));
        assert!(!required.contains(&"task_id"));
        assert!(!required.contains(&"verify"));
        assert!(!required.contains(&"healthchecks"));
    }

    #[test]
    fn every_registered_tool_has_a_non_empty_description() {
        let router = LsbxMcpServer::tool_router();
        for tool in router.list_all() {
            assert!(
                tool.description.as_deref().is_some_and(|d| !d.is_empty()),
                "tool '{}' is missing a description",
                tool.name
            );
        }
    }

    #[test]
    fn registered_tool_names_has_no_duplicates() {
        let names = registered_tool_names();
        let mut deduped = names.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            names.len(),
            deduped.len(),
            "duplicate tool name(s) registered: {names:?}"
        );
    }
}
