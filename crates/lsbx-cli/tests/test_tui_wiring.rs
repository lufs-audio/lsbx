#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn dashboard_subcommand_refuses_when_not_a_tty() {
    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .arg("dashboard")
        .output()
        .expect("failed to spawn lsbx binary");

    assert_eq!(output.status.code(), Some(2)); // ExitCode::Usage = 2
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("stdout is not a TTY") || combined.contains("interactive terminal"),
        "expected error about TTY requirement, got:\n{combined}"
    );
}
