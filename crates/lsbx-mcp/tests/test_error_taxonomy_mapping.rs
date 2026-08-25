//! `test_error_taxonomy_mapping` — a misused tool call must surface an
//! MCP-level error whose code/message trace back to the same
//! `LsbxError`/`ExitCode` taxonomy used everywhere else in this system,
//! never a generic "invalid input" string. Exercised at both of the two
//! real layers a misuse can be caught at:
//!
//! 1. **Missing required field / malformed arguments** — never reaches a
//!    tool body at all. `rmcp`'s own `Parameters<T>` extraction
//!    (`rmcp::handler::server::tool::parse_json_object`, the exact
//!    function every `#[tool]` method's `Parameters<T>` argument calls
//!    through, confirmed by direct read of
//!    `rmcp-3.1.4/src/handler/server/tool.rs`) maps a deserialization
//!    failure onto `ErrorCode::INVALID_PARAMS` (`-32602`, a real,
//!    documented JSON-RPC code, not a bare string) automatically, before
//!    this crate's own code runs. This suite exercises that exact real
//!    function directly against each params type this crate defines, so
//!    "a required field is actually enforced" is proven per tool, not
//!    merely asserted once for one example type.
//! 2. **Unknown id / other domain misuse that reaches `LsbxOps`** —
//!    caught inside a tool body, mapped through
//!    [`lsbx_mcp::error_map::lsbx_error_to_mcp_error`]'s equivalent path
//!    (via `envelope_result`, exercised end to end by calling the real
//!    `#[tool]`-generated async methods on `LsbxMcpServer` directly — they
//!    are plain async methods once macro-expanded, requiring no
//!    `RequestContext` since none of this crate's tool bodies read one).
//!    The resulting `CallToolResult`'s content carries the real
//!    `Envelope::Error { code, message }` shape whose `code` is the same
//!    numeric `ExitCode` the CLI/HTTP doors would report for the
//!    identical `LsbxError`, per this unit's own acceptance criterion.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's own
//! established house convention for test code (see e.g.
//! `crates/lsbx-golden/src/registry.rs`'s identically-worded rationale
//! above its own `#[cfg(test)] mod tests` block): the restriction-group
//! clippy lints `unwrap_used`/`expect_used` don't distinguish test code
//! from production code the way `#[cfg(test)]` gating does, so every
//! merged crate in this workspace scopes an explicit allow to its test
//! modules/files rather than either disabling the lints workspace-wide or
//! writing test assertions in an unidiomatic, unwrap-free style.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_backend_demo::DemoBackend;
use lsbx_kernel::exit_code::ExitCode;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use rmcp::handler::server::tool::parse_json_object;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::JsonObject;
use std::sync::Arc;

fn build_test_server() -> lsbx_mcp::LsbxMcpServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = lsbx_golden::registry::ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: std::collections::HashMap::new(),
    };
    let ops = LsbxOps::new(
        Box::new(DemoBackend::new()),
        "demo".to_string(),
        SandboxStore::new(dir.path().to_path_buf()),
        CiJobStore::new(dir.path().to_path_buf()),
        registry,
        Box::new(lsbx_kernel::clock::SystemClock),
    );
    // Leak the tempdir so its path stays valid for the lifetime of a test
    // process — acceptable in a short-lived test binary, avoids threading
    // a TempDir guard through every call site in this file.
    std::mem::forget(dir);
    lsbx_mcp::LsbxMcpServer::new(Arc::new(ops))
}

fn extract_json_text(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    assert_eq!(result.content.len(), 1, "expected exactly one content block");
    match &result.content[0] {
        rmcp::model::ContentBlock::Text(text_content) => {
            serde_json::from_str(&text_content.text).expect("tool response content must be valid JSON")
        }
        other => panic!("expected a text content block, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Layer 1: malformed/missing-field arguments never reach LsbxOps at all —
// rmcp's own Parameters<T> extraction (parse_json_object) rejects them
// with a real ErrorCode::INVALID_PARAMS, not a generic string.
// -----------------------------------------------------------------------

#[test]
fn destroy_with_missing_required_id_field_is_rejected_before_reaching_lsbx_ops() {
    let empty_args: JsonObject = serde_json::Map::new();
    let result: Result<lsbx_mcp::tools::DestroyParams, rmcp::ErrorData> = parse_json_object(empty_args);

    let err = result.expect_err("missing required 'id' field must be rejected");
    assert_eq!(
        err.code,
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "a malformed tool call must map to the real INVALID_PARAMS code, not a generic error"
    );
    assert!(
        err.message.contains("failed to deserialize parameters"),
        "expected a real deserialization diagnostic, got: {}",
        err.message
    );
}

#[test]
fn create_with_missing_required_profile_field_is_rejected_before_reaching_lsbx_ops() {
    let mut args = serde_json::Map::new();
    args.insert("lease_secs".to_string(), serde_json::json!(3600));
    args.insert("ready_timeout_secs".to_string(), serde_json::json!(30));
    // `profile` is deliberately omitted — it has no #[serde(default)].

    let result: Result<lsbx_mcp::tools::CreateParams, rmcp::ErrorData> = parse_json_object(args);
    let err = result.expect_err("missing required 'profile' field must be rejected");
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn renew_with_missing_required_duration_field_is_rejected() {
    let mut args = serde_json::Map::new();
    args.insert("id".to_string(), serde_json::json!("sbx-whatever"));
    // `duration_secs` deliberately omitted.

    let result: Result<lsbx_mcp::tools::RenewParams, rmcp::ErrorData> = parse_json_object(args);
    let err = result.expect_err("missing required 'duration_secs' field must be rejected");
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn golden_verify_with_missing_pubkey_is_rejected() {
    let mut args = serde_json::Map::new();
    args.insert("name".to_string(), serde_json::json!("agent-base"));
    args.insert("verify_name".to_string(), serde_json::json!("verify-1"));
    // `pubkey` deliberately omitted.

    let result: Result<lsbx_mcp::tools::GoldenVerifyParams, rmcp::ErrorData> = parse_json_object(args);
    let err = result.expect_err("missing required 'pubkey' field must be rejected");
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn exec_with_wrong_type_for_command_field_is_rejected() {
    let mut args = serde_json::Map::new();
    args.insert("id".to_string(), serde_json::json!("sbx-whatever"));
    // `command` must be an array of strings, not a bare string.
    args.insert("command".to_string(), serde_json::json!("not-an-array"));
    args.insert("timeout_secs".to_string(), serde_json::json!(30));

    let result: Result<lsbx_mcp::tools::ExecParams, rmcp::ErrorData> = parse_json_object(args);
    let err = result.expect_err("wrong-typed 'command' field must be rejected");
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

// -----------------------------------------------------------------------
// Layer 2: well-formed arguments that reach LsbxOps but reference
// something that doesn't exist (unknown id / unknown golden key) surface
// the real LsbxError::NotFound, mapped to the real Envelope shape whose
// `code` is ExitCode::NotFound (4) — not folded into a generic failure.
// -----------------------------------------------------------------------

#[tokio::test]
async fn destroy_unknown_id_surfaces_not_found_exit_code_in_envelope() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::DestroyParams {
        id: "sbx-does-not-exist".to_string(),
    };

    let result = server
        .destroy(Parameters(params))
        .await
        .expect("destroy itself must return Ok(CallToolResult) for a domain-level failure");

    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["code"], ExitCode::NotFound as i32);
    assert_eq!(envelope["code"], 4);
    assert!(
        envelope["message"].as_str().unwrap().contains("sbx-does-not-exist"),
        "expected the real sandbox id in the error message, got: {envelope}"
    );
}

#[tokio::test]
async fn info_unknown_id_surfaces_not_found_exit_code_in_envelope() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::InfoParams {
        id: "sbx-also-does-not-exist".to_string(),
    };

    let result = server.info(Parameters(params)).await.expect("info must return Ok(CallToolResult)");
    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["code"], ExitCode::NotFound as i32);
}

#[tokio::test]
async fn golden_verify_unknown_golden_key_surfaces_not_found_exit_code_in_envelope() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::GoldenVerifyParams {
        name: "no-such-golden".to_string(),
        verify_name: "verify-1".to_string(),
        pubkey: "ssh-ed25519 AAAA fake".to_string(),
    };

    let result = server
        .golden_verify(Parameters(params))
        .await
        .expect("golden_verify must return Ok(CallToolResult)");
    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["code"], ExitCode::NotFound as i32);
    assert!(envelope["message"]
        .as_str()
        .unwrap()
        .contains("no-such-golden"));
}

#[tokio::test]
async fn golden_delete_unknown_golden_key_surfaces_not_found_exit_code_in_envelope() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::GoldenDeleteParams {
        name: "no-such-golden".to_string(),
        keep_snapshot: false,
    };

    let result = server
        .golden_delete(Parameters(params))
        .await
        .expect("golden_delete must return Ok(CallToolResult)");
    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["code"], ExitCode::NotFound as i32);
}

#[tokio::test]
async fn golden_register_duplicate_key_surfaces_usage_exit_code_in_envelope() {
    let server = build_test_server();
    let make_params = || lsbx_mcp::tools::GoldenRegisterParams {
        key: "agent-base".to_string(),
        flavor: lsbx_mcp::tools::GoldenFlavorParam::Agent,
        os: "linux".to_string(),
        base: "lsbx-default-v1".to_string(),
        mode: lsbx_mcp::tools::GoldenModeParam::Copy,
        cpu: 2,
        memory: "2G".to_string(),
        disk: None,
        streaming: lsbx_mcp::tools::StreamingModeParam::None,
        capabilities: vec![],
        healthcheck: vec![],
        repo: None,
        content_hash: None,
        description: "test golden".to_string(),
    };

    let first = server
        .golden_register(Parameters(make_params()))
        .await
        .expect("first registration must return Ok(CallToolResult)");
    let first_envelope = extract_json_text(&first);
    assert_eq!(first_envelope["status"], "success");

    let second = server
        .golden_register(Parameters(make_params()))
        .await
        .expect("duplicate registration must return Ok(CallToolResult), not Err");
    let second_envelope = extract_json_text(&second);
    assert_eq!(second_envelope["status"], "error");
    assert_eq!(second_envelope["code"], ExitCode::Usage as i32);
    assert_eq!(second_envelope["code"], 2);
}

/// `logs_query` is the one operation that fails unconditionally today
/// (no merged crate owns a queryable log store — see `lsbx-ops`'s own doc
/// comment). Its failure must still surface as the real
/// `ContractViolated` code, not a special-cased different shape.
#[tokio::test]
async fn logs_query_always_fails_with_contract_violated_exit_code_in_envelope() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::LogsQueryParams {
        since: None,
        limit: 10,
    };

    let result = server
        .logs_query(Parameters(params))
        .await
        .expect("logs_query must return Ok(CallToolResult)");
    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["code"], ExitCode::ContractViolated as i32);
    assert_eq!(envelope["code"], 5);
}

/// A well-formed, successful call must still use the exact `Envelope<T>`
/// shape (`{"status":"success","data":{...}}`) — the same shape the CLI's
/// `--json` output and the HTTP gateway use, per this unit's own
/// acceptance criterion, not merely on the error path.
#[tokio::test]
async fn successful_call_uses_the_real_envelope_success_shape() {
    let server = build_test_server();
    let params = lsbx_mcp::tools::ListParams {};

    let result = server.list(Parameters(params)).await.expect("list must return Ok(CallToolResult)");
    let envelope = extract_json_text(&result);
    assert_eq!(envelope["status"], "success");
    assert!(envelope.get("data").is_some());
    assert!(envelope.get("code").is_none(), "success envelope must not carry a `code` field");
}
