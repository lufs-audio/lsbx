#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Live HTTP-path smoke test for the fixed `/exec` wire format (lsbx#30).
//!
//! Lighter than `test_conformance`: it needs only an account token with the
//! `ls` and `ssh` command scopes — no sandbox-lifecycle verbs (`cp`, `tag`,
//! `ssh-key add`, `rm`) — so it can verify the two wire paths that #30 fixed
//! (verbatim-body control JSON, and guest execution via `ssh <vm> <cmd>`
//! with the in-band exit sentinel) against the real endpoint without the
//! ability to create or destroy anything.
//!
//! ```bash
//! EXE_TOKEN=<account-token> cargo test -p lsbx-backend-exedev \
//!     --test test_http_live -- --ignored
//! ```
//!
//! Skipped unless `EXE_TOKEN` is set; read-only against the account except
//! for running one `echo` and one `false` on the target VM.
use lsbx_backend_exedev::{ExedevAuth, ExedevBackend};
use lsbx_kernel::backend::Backend;
use std::time::Duration;

fn live_backend() -> Option<ExedevBackend> {
    let token = std::env::var("EXE_TOKEN").ok()?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some(ExedevBackend::new(ExedevAuth::account_token(token)))
}

/// The VM every guest-execution check runs on. Read-only usage: two trivial
/// commands (`echo`, `false`). Override with `LSBX_EXEDEV_LIVE_VM`.
fn live_vm() -> String {
    std::env::var("LSBX_EXEDEV_LIVE_VM").unwrap_or_else(|_| "molimo".to_string())
}

#[tokio::test]
#[ignore = "requires a reachable exe.dev control plane and EXE_TOKEN with ls+ssh scopes"]
async fn account_level_ls_json_parses_over_the_verbatim_wire_format() {
    let Some(backend) = live_backend() else {
        panic!("set EXE_TOKEN to run this test");
    };
    // Pre-#30 this call posted a JSON envelope the lobby answered with
    // {"error":"unknown command"} and then failed response parsing.
    let vms = backend.list_vms().await.expect("list_vms over HTTPS");
    assert!(
        vms.iter().any(|tag| tag == &live_vm()),
        "expected '{vm}' in the account inventory, got {vms:?}",
        vm = live_vm()
    );
}

#[tokio::test]
#[ignore = "requires a reachable exe.dev control plane and EXE_TOKEN with ls+ssh scopes"]
async fn guest_run_returns_the_remote_exit_code_via_the_inband_sentinel() {
    let Some(backend) = live_backend() else {
        panic!("set EXE_TOKEN to run this test");
    };
    let vm = live_vm();

    let ok = backend
        .run(
            &vm,
            &["echo".to_string(), "lsbx-http-live-ok".to_string()],
            Duration::from_secs(30),
            None,
        )
        .await
        .expect("guest echo over HTTPS");
    assert_eq!(
        ok.exit_code,
        0,
        "echo must exit 0 (stdout: {})",
        String::from_utf8_lossy(&ok.stdout)
    );
    assert!(
        String::from_utf8_lossy(&ok.stdout).contains("lsbx-http-live-ok"),
        "guest stdout must carry the echo payload: {:?}",
        String::from_utf8_lossy(&ok.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&ok.stdout).contains("__LSBX_EXIT"),
        "sentinel must be stripped from guest output: {:?}",
        String::from_utf8_lossy(&ok.stdout)
    );

    let failed = backend
        .run(&vm, &["false".to_string()], Duration::from_secs(30), None)
        .await
        .expect("guest false over HTTPS");
    assert_eq!(
        failed.exit_code,
        1,
        "guest exit code must come from the in-band sentinel, not the HTTP status (stdout: {})",
        String::from_utf8_lossy(&failed.stdout)
    );
}
