// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside `cargo test`,
// never in a shipped code path. clippy::unwrap_used / expect_used are
// restriction-group lints that don't understand "this whole file is test
// code" the way #[cfg(test)] does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this file only —
// crates/lsbx-kernel/src/**/*.rs (the real production code path) is unwrap/
// expect/panic-free under the same workspace lints with no allow needed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_kernel::types::SandboxRecord;
use lsbx_kernel::envelope::Envelope;
use lsbx_kernel::error::LsbxError;

#[test]
fn legacy_flat_migrates() {
    let raw_json = r#"{
        "id": "foo-bar",
        "name": "foo",
        "host": "localhost",
        "profile": "default",
        "flavor": "default",
        "streaming": "none",
        "cleanup_failed": false,
        "extra": {}
    }"#;

    let value: serde_json::Value = serde_json::from_str(raw_json).unwrap();
    let record = SandboxRecord::from_legacy_flat(value).unwrap();

    assert_eq!(record.id, "foo-bar");
    assert_eq!(record.streaming, "none");
}

#[test]
fn envelope_success() {
    let success_res: Result<String, LsbxError> = Ok("test_data".to_string());
    let env = Envelope::from_result(success_res);
    let serialized = serde_json::to_string(&env).unwrap();
    assert_eq!(serialized, r#"{"status":"success","data":"test_data"}"#);
}

#[test]
fn envelope_error() {
    let err_res: Result<String, LsbxError> = Err(LsbxError::NotFound("test error".to_string()));
    let env = Envelope::from_result(err_res);
    let serialized = serde_json::to_string(&env).unwrap();
    assert_eq!(serialized, r#"{"status":"error","code":4,"message":"not found: test error"}"#);
}
