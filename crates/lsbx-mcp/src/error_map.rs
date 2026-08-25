//! Maps the real, closed 7-variant `lsbx_kernel::error::LsbxError` onto an
//! MCP-level `rmcp::ErrorData` whose numeric `code` is always the same
//! `ExitCode` value the CLI's process exit status and the HTTP gateway's
//! JSON envelope `code` field would report for the identical failure — the
//! same house convention `crates/lsbx-kernel/src/envelope.rs`'s
//! `Envelope::from_result` already establishes for the other two doors.
//!
//! This is deliberately the *only* place in this crate that constructs an
//! `rmcp::ErrorData` from an `LsbxError` — every tool method in
//! `tools.rs` funnels its `Result<_, LsbxError>` through
//! [`lsbx_error_to_mcp_error`] rather than each rolling its own mapping, so
//! there is exactly one place to audit for "does a misused tool call
//! surface a real code, not a generic string" (this unit's own acceptance
//! criterion).
//!
//! House convention (confirmed by direct re-read of every merged crate's
//! own error-mapping module doc comment — `lsbx-store`'s `sandbox_store.rs`/
//! `ci_job_store.rs`/`lock.rs`, `lsbx-golden`'s `registry.rs`/`hash.rs`):
//! anything that does not fit one of the real 7 variants maps onto
//! `LsbxError::ContractViolated`. `LsbxError` has no `#[from]` impl on any
//! variant (confirmed against `crates/lsbx-kernel/src/error.rs`) and no
//! catch-all `Other`/`Unknown` variant — inventing one here would be
//! exactly the kind of drift this system's "clean-room, not a training-era
//! guess" mandate exists to prevent (see `crates/lsbx-ops/src/lib.rs`'s own
//! module doc comment on the earlier, unusable Jules patch that invented
//! `LsbxError::Other(...)`).

use lsbx_kernel::error::LsbxError;
use rmcp::ErrorData as McpError;

/// Converts a real `LsbxError` into an `rmcp::ErrorData` whose `code` is
/// the identical numeric `ExitCode` value the CLI/HTTP doors would report
/// for the same failure, and whose `message` is `LsbxError`'s own
/// `Display` text (never a generic "invalid input" string).
///
/// MCP's JSON-RPC error `code` field is a bare `i32` with no enforced
/// namespace beyond the JSON-RPC spec's own reserved range
/// (-32768..=-32000, none of which this taxonomy's 0/2-8 values collide
/// with) — using `LsbxError::exit_code() as i32` directly here is what
/// makes "an agent calling this MCP server and an agent parsing `lsbx
/// --json`'s error envelope see the same code for the same failure" a
/// checkable fact rather than an aspiration, exactly mirroring
/// `Envelope::from_result`'s own `code: e.exit_code() as i32`.
pub fn lsbx_error_to_mcp_error(err: LsbxError) -> McpError {
    let code = err.exit_code() as i32;
    let message = err.to_string();
    McpError::new(rmcp::model::ErrorCode(code), message, None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use lsbx_kernel::exit_code::ExitCode;

    #[test]
    fn usage_maps_to_exit_code_2_with_real_message() {
        let err = LsbxError::Usage("missing required field 'id'".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::Usage as i32);
        assert_eq!(mcp_err.code.0, 2);
        assert!(mcp_err.message.contains("missing required field 'id'"));
        assert!(mcp_err.message.contains("usage:"));
    }

    #[test]
    fn backend_unavailable_maps_to_exit_code_3() {
        let err = LsbxError::BackendUnavailable("libvirt socket down".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::BackendUnavailable as i32);
        assert_eq!(mcp_err.code.0, 3);
    }

    #[test]
    fn not_found_maps_to_exit_code_4() {
        let err = LsbxError::NotFound("sandbox sbx-does-not-exist".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::NotFound as i32);
        assert_eq!(mcp_err.code.0, 4);
        assert!(mcp_err.message.contains("sbx-does-not-exist"));
    }

    #[test]
    fn contract_violated_maps_to_exit_code_5() {
        let err = LsbxError::ContractViolated("healthcheck did not pass".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::ContractViolated as i32);
        assert_eq!(mcp_err.code.0, 5);
    }

    #[test]
    fn lock_contention_maps_to_exit_code_6() {
        let err = LsbxError::LockContention("lock held elsewhere".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::LockContention as i32);
        assert_eq!(mcp_err.code.0, 6);
    }

    #[test]
    fn auth_failed_maps_to_exit_code_7() {
        let err = LsbxError::AuthFailed("bearer token rejected".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::AuthFailed as i32);
        assert_eq!(mcp_err.code.0, 7);
    }

    #[test]
    fn interrupted_maps_to_exit_code_8() {
        let err = LsbxError::Interrupted("signal received mid-flight".to_string());
        let mcp_err = lsbx_error_to_mcp_error(err);
        assert_eq!(mcp_err.code.0, ExitCode::Interrupted as i32);
        assert_eq!(mcp_err.code.0, 8);
    }

    /// Every one of the 7 real variants must produce a distinct code —
    /// this is what "traces back to the same taxonomy" means concretely,
    /// not just "returns *some* non-generic code."
    #[test]
    fn all_seven_variants_produce_distinct_codes() {
        let errs = vec![
            LsbxError::Usage("x".to_string()),
            LsbxError::BackendUnavailable("x".to_string()),
            LsbxError::NotFound("x".to_string()),
            LsbxError::ContractViolated("x".to_string()),
            LsbxError::LockContention("x".to_string()),
            LsbxError::AuthFailed("x".to_string()),
            LsbxError::Interrupted("x".to_string()),
        ];
        let codes: Vec<i32> = errs.into_iter().map(|e| lsbx_error_to_mcp_error(e).code.0).collect();
        let mut deduped = codes.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(codes.len(), deduped.len(), "expected all 7 codes to be distinct: {codes:?}");
    }
}
