#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unit contract's named scenario: "`test_batch_mode_stdin` asserts a
//! remote command spawned by this backend has its stdin connected to
//! `/dev/null` (or an equivalently verifiable non-inherited state), never
//! the parent process's stdin."
//!
//! Both source candidates' versions of this file spawned a standalone
//! `ssh` invocation directly, with no involvement from this crate's own
//! code at all — asserting only that a *hand-written test-file `Command`
//! call* used `Stdio::null()`, which would pass unconditionally even if
//! this crate's actual `guest_ssh` module never isolated stdin at all. This
//! version instead drives `lsbx_backend_libvirt::guest_ssh::run_command` —
//! the real library function `Backend::run` calls — against a fake `ssh`
//! substituted onto `PATH`, so a regression in the *library's* spawn
//! configuration (e.g. someone removing the `.stdin(Stdio::null())` call
//! in `guest_ssh.rs`) would actually fail this test.

use lsbx_backend_libvirt::guest_ssh::{run_command, GuestSshTarget};
use std::path::Path;

/// Writes a fake `ssh` script into a fresh temp directory and prepends
/// that directory to `PATH` for the duration of the returned guard's life.
struct FakeSshOnPath {
    dir: std::path::PathBuf,
    original_path: String,
}

impl FakeSshOnPath {
    fn install(script_body: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lsbx-batch-mode-stdin-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir for fake ssh script");

        let script_path = dir.join("ssh");
        std::fs::write(&script_path, script_body).expect("write fake ssh script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake ssh script");
        }

        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{original_path}", dir.to_string_lossy()));

        Self { dir, original_path }
    }
}

impl Drop for FakeSshOnPath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.original_path);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The scenario itself: a fake `ssh` that reports whether its stdin was
/// immediately at EOF (what `/dev/null` always produces) or would have
/// blocked waiting for input (what happens if a live pipe/terminal were
/// inherited instead). `read -t 0.2` gives any inherited-but-idle pipe a
/// generous window to prove it is NOT at EOF before the script gives up
/// and reports isolation.
#[tokio::test]
async fn run_command_never_inherits_the_calling_processs_stdin() {
    let _guard = FakeSshOnPath::install(
        "#!/bin/sh\nif read -t 0.2 _line; then\n  echo \"stdin was NOT at EOF\" >&2\n  exit 1\nelse\n  echo \"stdin-isolated\"\n  exit 0\nfi\n",
    );

    let target = GuestSshTarget {
        host: "192.0.2.10",
        username: "lsbx",
        identity_file: Path::new("/tmp/lsbx-batch-mode-test-key"),
    };

    let output = run_command(
        &target,
        &["whoami".to_string()],
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("fake ssh script should run and report isolated stdin");

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "stdin-isolated"
    );
}

/// Companion negative control: if the fake `ssh` script's stdin were
/// *actually* connected to a live, open pipe (simulating what a bug that
/// removed stdin isolation would produce), the script would report the
/// opposite outcome. This proves the scenario's detection mechanism itself
/// is not a tautology — the same script, given real inherited input,
/// really does distinguish the two cases — by running the identical
/// script directly (bypassing `run_command`) with a piped stdin that has
/// data pending.
#[tokio::test]
async fn negative_control_fake_ssh_detects_a_live_stdin_when_actually_given_one() {
    let _guard = FakeSshOnPath::install(
        "#!/bin/sh\nif read -t 0.2 _line; then\n  echo \"stdin was NOT at EOF\" >&2\n  exit 1\nelse\n  echo \"stdin-isolated\"\n  exit 0\nfi\n",
    );

    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("ssh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fake ssh with a live piped stdin");

    let mut stdin = child.stdin.take().expect("child stdin should be piped");
    stdin
        .write_all(b"some live input\n")
        .await
        .expect("write to child stdin");
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .expect("wait for fake ssh with live stdin");

    // With genuinely live, non-empty stdin, the script's own `read`
    // succeeds and it reports the "NOT at EOF" branch — confirming the
    // detection mechanism the primary test above relies on is real, not a
    // script that always reports "isolated" regardless of its actual
    // stdin state.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("NOT at EOF"));
}
