//! `test_cli_parity` — enumerates `LsbxOps`'s real public method names
//! against this crate's own `registered_tool_names()`, and fails the
//! build if they diverge. This is Unit 15's own reason to exist (SPEC.md
//! Deviation 12: "100% parity" made checkable, not asserted).
//!
//! ## Why this is hardcoded against `LsbxOps`, not `lsbx-cli`'s `Command` enum
//!
//! As of this crate's own branch point (commit
//! `86506e28d0b4448527379e4487e7cab03341180d`, `main`'s tip immediately
//! before this branch was cut), Unit 11 (`lsbx-cli`) has **not** merged:
//!
//! - Every merged PR against `lufs-audio/lsbx` was enumerated (PRs #1-#15)
//!   and none is titled or bodied for unit-11/`lsbx-cli`
//!   (`feat(unit-01)` through `feat(unit-10)` are the full merged set).
//! - The root `Cargo.toml`'s `members` list ends at `crates/lsbx-ops`
//!   (the 10th entry) with no `crates/lsbx-cli` member.
//!
//! Per this unit's own mechanics instructions, the parity test is
//! therefore built against `LsbxOps`'s own method list — "the more
//! fundamental source of truth in any case: the CLI is itself just one
//! more thin adapter over the same façade" (SPEC.md §3's architecture
//! diagram: `lsbx-cli` and `lsbx-mcp` are *siblings* under `lsbx-ops`,
//! neither derived from the other). If/when Unit 11 lands, this file's
//! own `LSBX_OPS_PUBLIC_METHODS` constant does not need to change — only
//! a new, additive cross-check against `lsbx_cli::cli::Command`'s real
//! variants would need to be added alongside it.
//!
//! ## Where `LSBX_OPS_PUBLIC_METHODS` comes from
//!
//! Rust has no runtime reflection over `impl` blocks, so this list is a
//! hardcoded transcription — cross-checked directly against
//! `crates/lsbx-ops/src/lib.rs`'s real `impl LsbxOps` block at the same
//! commit named above, not against this unit's own contract text's
//! older, superseded literal operation listing. Counted directly: 18
//! `pub async fn` methods on `LsbxOps`.

const LSBX_OPS_PUBLIC_METHODS: &[&str] = &[
    "create",
    "destroy",
    "renew",
    "reap",
    "list",
    "info",
    "console_url",
    "exec",
    "put",
    "get",
    "status",
    "golden_build",
    "golden_verify",
    "golden_register",
    "golden_delete",
    "golden_list",
    "config_show",
    "logs_query",
];

#[test]
fn registered_tool_names_matches_lsbx_ops_public_methods_exactly() {
    let mut registered: Vec<&str> = lsbx_mcp::registered_tool_names();
    let mut expected: Vec<&str> = LSBX_OPS_PUBLIC_METHODS.to_vec();

    registered.sort_unstable();
    expected.sort_unstable();

    let missing_tools: Vec<&&str> = expected.iter().filter(|op| !registered.contains(op)).collect();
    let extra_tools: Vec<&&str> = registered.iter().filter(|t| !expected.contains(t)).collect();

    assert!(
        missing_tools.is_empty(),
        "LsbxOps operation(s) with no corresponding registered MCP tool: {missing_tools:?}. \
         Every public LsbxOps method needs exactly one #[tool]-annotated method on \
         LsbxMcpServer with the same name."
    );
    assert!(
        extra_tools.is_empty(),
        "Registered MCP tool(s) with no corresponding LsbxOps operation: {extra_tools:?}. \
         A tool exists on LsbxMcpServer with no matching public method on LsbxOps — either \
         LsbxOps gained a method this test's LSBX_OPS_PUBLIC_METHODS list needs updating for, \
         or this crate registered a tool that doesn't belong."
    );

    assert_eq!(
        registered, expected,
        "registered_tool_names() must be exactly LsbxOps's public method list, no more, no fewer"
    );
    assert_eq!(
        registered.len(),
        18,
        "expected exactly 18 registered tools (LsbxOps's real public method count as of \
         commit 86506e28d0b4448527379e4487e7cab03341180d); if this fails because LsbxOps \
         gained or lost a method, update LSBX_OPS_PUBLIC_METHODS above to match the real, \
         current crates/lsbx-ops/src/lib.rs before touching this assertion"
    );
}

/// Every tool name must also be a valid MCP tool identifier — not itself
/// an acceptance criterion, but a cheap sanity check that guards against a
/// silently-mangled name (e.g. `pastey`/macro-generated raw-string
/// artifacts) passing the set-equality check above by accident.
#[test]
fn every_registered_tool_name_is_non_empty_and_matches_a_lsbx_ops_method_verbatim() {
    let registered = lsbx_mcp::registered_tool_names();
    assert_eq!(registered.len(), LSBX_OPS_PUBLIC_METHODS.len());
    for name in &registered {
        assert!(!name.is_empty());
        assert!(
            LSBX_OPS_PUBLIC_METHODS.contains(name),
            "tool name '{name}' does not verbatim-match any LsbxOps method name"
        );
    }
}
