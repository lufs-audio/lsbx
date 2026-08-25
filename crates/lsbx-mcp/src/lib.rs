//! `lsbx-mcp` — Stdio MCP Server (Unit 15).
//!
//! Exposes every real public method on `lsbx_ops::LsbxOps` (Unit 10) as one
//! MCP tool, generated via the `rmcp` crate's `#[tool_router]`/`#[tool]`
//! macros, using tool input types whose JSON Schema is derived by
//! `schemars::JsonSchema` — never a hand-written schema that could drift
//! from the real Rust types. Tool bodies are thin, direct call-throughs
//! into `LsbxOps`; this crate implements no operation logic of its own,
//! per this unit's own Boundaries ("Implements no operation logic — every
//! tool body is a direct, thin call into `LsbxOps`").
//!
//! ## Provenance note: this unit was built clean-room
//!
//! Both Google Jules AI sessions for this unit never reached a usable
//! state (one failed outright, one is stuck on an unanswered question).
//! Nothing from either session is reflected anywhere in this crate. Every
//! signature below was written directly against `crates/lsbx-ops/src/lib.rs`
//! at `main`'s tip as of this unit's own branch point
//! (commit `86506e28d0b4448527379e4487e7cab03341180d`, PR #15,
//! "feat(unit-10): implement shared operations facade"), re-confirmed by
//! direct re-read of that file immediately before writing this crate — not
//! against this unit's own contract text's older, now-superseded literal
//! operation listing (`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/
//! 15-stdio-mcp-server.md`), which predates several signature reworks that
//! happened during Units 08/09/10's own implementation (see
//! `crates/lsbx-ops/src/lib.rs`'s own module doc comment, "Reconciling the
//! unit contract's literal interface against the real merged source," for
//! the full list of what changed and why).
//!
//! As of this branch's base commit, Unit 11 (`lsbx-cli`) has **not**
//! merged — confirmed by searching every merged PR against `main`
//! (PRs #1-#15, none titled or bodied for unit-11/`lsbx-cli`) and by the
//! root `Cargo.toml`'s `members` list, which ends at `crates/lsbx-ops`
//! (the 10th and last entry) with no `crates/lsbx-cli` member. This
//! crate's CLI-parity acceptance criterion is therefore built against
//! `LsbxOps`'s own method list — the more fundamental source of truth in
//! any case, since a CLI's `Command` enum is itself just one more thin
//! adapter over the same façade (SPEC.md §3's architecture diagram; the
//! CLI and this MCP server are siblings under `lsbx-ops`, not one derived
//! from the other). `tests/test_cli_parity.rs` documents this explicitly
//! and is written so that, once Unit 11 lands, extending it to also
//! cross-check `lsbx_cli::cli::Command`'s real variants is a small,
//! additive change rather than a rewrite.
//!
//! ## The 18 real `LsbxOps` operations, and their tool names
//!
//! `create, destroy, renew, reap, list, info, console_url, exec, put, get,
//! status, golden_build, golden_verify, golden_register, golden_delete,
//! golden_list, config_show, logs_query` — every one of `LsbxOps`'s 18 real
//! `pub async fn` methods, confirmed by direct count against
//! `impl LsbxOps` in `crates/lsbx-ops/src/lib.rs`. This is a strictly
//! larger and differently-shaped set than the unit contract's own literal
//! interface-contract snippet (which shows only `create`/`destroy` as
//! illustrative examples, not an exhaustive list) — this crate's own
//! `tests/test_cli_parity.rs` hardcodes the real 18-name list (transcribed
//! directly from that file, not from the contract's prose) and fails the
//! build if it and `registered_tool_names()` ever diverge.
//!
//! ## Tool input types are owned mirrors, not the borrowed façade types
//!
//! `LsbxOps`'s own request types (`lsbx_lifecycle::create::CreateRequest`,
//! `lsbx_golden::build::GoldenBuildRequest`) borrow `&str`/`&Path` fields,
//! which cannot implement `serde::Deserialize` (deserializing into a
//! borrowed field requires borrowing from the deserializer's own input
//! buffer, which `rmcp`'s JSON-object-argument deserialization path does
//! not thread through generically). `src/tools.rs` therefore defines one
//! owned params struct per tool — e.g. `CreateParams` for `create` — whose
//! fields are a straight, undiminished transcription of the real request
//! struct's fields (same names, same meaning, `Duration` fields expressed
//! as `_secs: u64` since `std::time::Duration` has no `JsonSchema` impl),
//! and each tool body converts the owned params into the real borrowed
//! request type at the call site, immediately before calling into
//! `LsbxOps`. This is the same shape every other door in this system uses
//! (a CLI's `clap`-parsed args, an HTTP gateway's deserialized JSON body)
//! to cross the boundary from "untyped wire input" to "the façade's own
//! typed request" — translating a door's native input format into
//! `lsbx-ops`'s types is explicitly this door's job, never `lsbx-ops`'s
//! own (`crates/lsbx-ops/src/lib.rs`'s own module doc comment: "No
//! function here parses CLI args, HTTP bodies, or MCP tool-call JSON —
//! every input arrives already typed... this crate's job").
//! `GoldenConfig` (used directly by `golden_register`) has the same
//! problem for a different reason: it derives `Deserialize`/`Serialize`
//! but not `schemars::JsonSchema` (confirmed against
//! `crates/lsbx-golden/src/registry.rs`), so `GoldenRegisterParams` is an
//! owned, field-identical mirror that gets converted into a real
//! `GoldenConfig` at the call site — never a hand-edited copy of the type
//! that could silently drift from its fields.

pub mod error_map;
pub mod tools;

pub use tools::{registered_tool_names, LsbxMcpServer};

/// Launches the MCP server over stdio (`rmcp`'s `transport-io` feature),
/// blocking until the client disconnects. `lsbx mcp` (Unit 11, once it
/// lands) is expected to call this with no other setup beyond constructing
/// the shared `Arc<LsbxOps>` every door in this system holds one of.
pub async fn run_stdio_server(
    ops: std::sync::Arc<lsbx_ops::LsbxOps>,
) -> Result<(), lsbx_kernel::error::LsbxError> {
    use rmcp::ServiceExt;

    let server = LsbxMcpServer::new(ops);

    let service = server.serve(rmcp::transport::stdio()).await.map_err(|e| {
        lsbx_kernel::error::LsbxError::ContractViolated(format!(
            "failed to start stdio MCP server: {e}"
        ))
    })?;

    service.waiting().await.map_err(|e| {
        lsbx_kernel::error::LsbxError::Interrupted(format!(
            "stdio MCP server terminated unexpectedly: {e}"
        ))
    })?;

    Ok(())
}
