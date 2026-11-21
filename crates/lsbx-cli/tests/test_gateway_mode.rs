#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .env_remove("LSBX_GATEWAY_URL")
        .env_remove("LSBX_GATEWAY_TOKEN")
        .args(args)
        .output()
        .expect("spawn lsbx")
}

#[test]
fn gateway_and_backend_are_mutually_exclusive() {
    let output = run(&[
        "--gateway",
        "https://gateway.example.invalid",
        "--token",
        "secret",
        "--backend",
        "libvirt",
        "status",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(
        body.contains("cannot be combined"),
        "unexpected output: {body}"
    );
    assert!(!body.contains("secret"));
}

#[test]
fn missing_gateway_token_exits_with_auth_failed() {
    let output = run(&[
        "--gateway",
        "https://gateway.example.invalid",
        "status",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(7));
    let body = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(body.trim()).expect("JSON error envelope");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["code"], 7);
}

#[test]
fn plaintext_gateway_requires_insecure_opt_in() {
    let output = run(&[
        "--gateway",
        "http://127.0.0.1:1",
        "--token",
        "secret",
        "status",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(body.contains("--insecure"), "unexpected output: {body}");
    assert!(!body.contains("secret"));
}
