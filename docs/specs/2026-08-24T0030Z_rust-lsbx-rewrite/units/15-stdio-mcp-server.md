# Unit 15 — Stdio MCP Server

## Objective
Implement the MCP door using `rmcp`, generating tools 1:1 from `lsbx-ops`'s operation set — the mechanism that makes "100% parity across all CLI tools" checkable rather than asserted (SPEC.md Deviation 12).

## Context
Layer 6, depends on Unit 10. This is Door 4 from SPEC.md §4.8, and the unit where the House of Process Registries' "verification" ethic gets applied to the MCP surface itself: parity is proven by a test that would fail on drift, not claimed in a commit message.

## Acceptance criteria
- [ ] Every public method on `LsbxOps` (Unit 10) has exactly one corresponding MCP tool — no more, no fewer — verified by a test that enumerates both lists and fails the build if they diverge.
- [ ] Tool input schemas are derived from each operation's request struct (via `rmcp-macros` or an equivalent schema-generation path), never hand-written JSON Schema that could silently drift from the real Rust types.
- [ ] Tool output uses the same `Envelope<T>` shape (Unit 01) that CLI `--json` and the HTTP gateway produce — an agent calling this MCP server and an agent parsing `lsbx --json` output see the same envelope shape.
- [ ] Runs over stdio transport (`rmcp`'s `transport-io` feature) — `lsbx mcp` with no other arguments launches it and blocks on stdio until the client disconnects.
- [ ] A misused tool call (e.g. `destroy` with a missing required field) returns an MCP-level error whose message and code map back to the same `LsbxError`/`ExitCode` taxonomy used everywhere else in the system, never a generic "invalid input" string.

## Interface contract
```rust
// src/tools.rs
use rmcp::tool;

pub struct LsbxMcpServer {
    ops: std::sync::Arc<lsbx_ops::LsbxOps>,
}

impl LsbxMcpServer {
    pub fn new(ops: std::sync::Arc<lsbx_ops::LsbxOps>) -> Self;

    // One #[tool]-annotated method per LsbxOps operation, same name, e.g.:
    #[tool(description = "Create a new ephemeral sandbox from a profile")]
    async fn create(&self, params: CreateParams) -> Result<rmcp::model::CallToolResult, rmcp::Error>;

    #[tool(description = "Destroy a sandbox by id")]
    async fn destroy(&self, params: DestroyParams) -> Result<rmcp::model::CallToolResult, rmcp::Error>;

    // ... continues for every operation listed in Unit 10's interface contract.
}

// src/lib.rs
pub async fn run_stdio_server(ops: std::sync::Arc<lsbx_ops::LsbxOps>) -> Result<(), lsbx_kernel::error::LsbxError>;

/// Test-only: the list of tool names this server registers, for the CLI-parity check.
pub fn registered_tool_names() -> Vec<&'static str>;
```

## Boundaries — do NOT touch
Implements no operation logic — every tool body is a direct, thin call into `LsbxOps`. Does not define the `Envelope<T>` shape (Unit 01) — reuses it as-is.

## Output
- `crates/lsbx-mcp/Cargo.toml`
- `crates/lsbx-mcp/src/lib.rs`
- `crates/lsbx-mcp/src/tools.rs`
- `crates/lsbx-mcp/tests/test_cli_parity.rs`
- `crates/lsbx-mcp/tests/test_error_taxonomy_mapping.rs`

## Verification
```bash
cargo check -p lsbx-mcp --message-format=json
cargo clippy -p lsbx-mcp --all-targets --all-features -- -D warnings
cargo test -p lsbx-mcp --test test_cli_parity
cargo test -p lsbx-mcp --test test_error_taxonomy_mapping
```
Scenario: `test_cli_parity` enumerates `lsbx-cli`'s `Command` variants (Unit 11) and this crate's `registered_tool_names()`, and fails the build if any CLI operation lacks a same-named MCP tool or vice versa.
