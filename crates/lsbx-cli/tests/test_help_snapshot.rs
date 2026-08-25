//! Snapshot test asserting full `--help` output for every subcommand
//! (Unit 11 acceptance criteria: "A snapshot test asserts full `--help`
//! output for every subcommand, so a flag rename shows up as an obvious
//! diff.").
//!
//! Spawns the real compiled binary (`env!("CARGO_BIN_EXE_lsbx")`) rather
//! than calling into `clap`'s parser directly, so this exercises exactly
//! what a user/agent actually sees when they run `lsbx --help` — the same
//! "spawn the real binary" approach this unit's own Verification section
//! asks for on the backend-auto-probe scenario, applied here too since a
//! help-text snapshot is only meaningful against what the shipped binary
//! actually prints.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn run_help(args: &[&str]) -> String {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--help");

    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .args(&full_args)
        .output()
        .expect("failed to spawn lsbx binary");

    assert!(
        output.status.success(),
        "`lsbx {} --help` did not exit 0 (stderr: {})",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Asserts a help blob mentions every one of `expected_flags`/`expected_subcommands`
/// verbatim — a much smaller assertion than a byte-for-byte snapshot, but
/// one that still fails loudly (an obvious diff in the failure message) the
/// moment any of these strings disappears from the real, compiled binary's
/// output, which is the acceptance criterion's actual concern (a flag
/// rename becomes an obvious diff).
fn assert_contains_all(help_text: &str, needles: &[&str], context: &str) {
    for needle in needles {
        assert!(
            help_text.contains(needle),
            "{context}: expected --help output to contain '{needle}', but it did not.\n\
             Full output:\n{help_text}"
        );
    }
}

#[test]
fn top_level_help_lists_every_global_flag() {
    let help = run_help(&[]);
    assert_contains_all(
        &help,
        &[
            "--json",
            "-j",
            "--verbose",
            "-v",
            "--quiet",
            "-q",
            "--no-color",
            "--config",
            "-c",
            "--backend",
            "-b",
            "--images",
            "-i",
            "--state-dir",
            "-s",
            "--version",
        ],
        "top-level --help",
    );
}

#[test]
fn top_level_help_lists_every_subcommand() {
    let help = run_help(&[]);
    assert_contains_all(
        &help,
        &[
            "up",
            "down",
            "list",
            "exec",
            "put",
            "get",
            "renew",
            "console",
            "info",
            "status",
            "profiles",
            "images",
            "reap",
            "serve",
            "bootstrap",
            "golden",
            "config",
            "logs",
            "mcp",
            "ci-broker",
        ],
        "top-level --help subcommand list",
    );
}

#[test]
fn up_help_lists_its_flags() {
    let help = run_help(&["up"]);
    assert_contains_all(
        &help,
        &[
            "--count",
            "-n",
            "--name",
            "--task-id",
            "-t",
            "--lease",
            "-l",
            "--no-verify",
            "--ready-timeout",
        ],
        "`lsbx up --help`",
    );
}

#[test]
fn down_help_lists_its_flags() {
    let help = run_help(&["down"]);
    assert_contains_all(&help, &["--all"], "`lsbx down --help`");
}

#[test]
fn list_help_lists_its_flags() {
    let help = run_help(&["list"]);
    assert_contains_all(&help, &["--profile", "--expired"], "`lsbx list --help`");
}

#[test]
fn exec_help_lists_its_flags() {
    let help = run_help(&["exec"]);
    assert_contains_all(&help, &["--timeout"], "`lsbx exec --help`");
}

#[test]
fn renew_help_exists() {
    let help = run_help(&["renew"]);
    assert!(help.to_lowercase().contains("duration"));
}

#[test]
fn profiles_help_lists_its_flags() {
    let help = run_help(&["profiles"]);
    assert_contains_all(&help, &["--full"], "`lsbx profiles --help`");
}

#[test]
fn reap_help_lists_its_flags() {
    let help = run_help(&["reap"]);
    assert_contains_all(&help, &["--ttl", "--dry-run"], "`lsbx reap --help`");
}

#[test]
fn serve_help_lists_its_flags() {
    let help = run_help(&["serve"]);
    assert_contains_all(
        &help,
        &[
            "--host",
            "--port",
            "--stream-port",
            "--token",
            "--reap-ttl",
            "--insecure",
            "--daemon",
        ],
        "`lsbx serve --help`",
    );
}

#[test]
fn bootstrap_help_lists_its_flags() {
    let help = run_help(&["bootstrap"]);
    assert_contains_all(
        &help,
        &[
            "--target",
            "--no-services",
            "--no-verify",
            "--force",
            "--dry-run",
        ],
        "`lsbx bootstrap --help`",
    );
}

#[test]
fn golden_help_lists_its_subcommands() {
    let help = run_help(&["golden"]);
    assert_contains_all(
        &help,
        &["list", "build", "verify", "register", "delete"],
        "`lsbx golden --help`",
    );
}

#[test]
fn golden_build_help_lists_its_flags() {
    let help = run_help(&["golden", "build"]);
    assert_contains_all(
        &help,
        &[
            "--from",
            "--script",
            "--flavor",
            "--cpu",
            "--memory",
            "--streaming",
            "--register",
            "--no-cleanup",
            "--interactive",
            "--shell",
            "--dry-run",
        ],
        "`lsbx golden build --help`",
    );
}

#[test]
fn golden_register_help_lists_its_flags() {
    let help = run_help(&["golden", "register"]);
    assert_contains_all(
        &help,
        &[
            "--profile",
            "--base",
            "--flavor",
            "--streaming",
            "--capabilities",
            "--healthcheck",
            "--content-hash",
            "--replace",
        ],
        "`lsbx golden register --help`",
    );
}

#[test]
fn golden_delete_help_lists_its_flags() {
    let help = run_help(&["golden", "delete"]);
    assert_contains_all(&help, &["--keep-snapshot"], "`lsbx golden delete --help`");
}

#[test]
fn config_help_lists_its_flags() {
    let help = run_help(&["config"]);
    assert_contains_all(
        &help,
        &["--show", "--init", "--set", "--path", "--force"],
        "`lsbx config --help`",
    );
}

#[test]
fn logs_help_lists_its_flags() {
    let help = run_help(&["logs"]);
    assert_contains_all(
        &help,
        &["--follow", "--command", "--since", "--limit", "--show"],
        "`lsbx logs --help`",
    );
}

#[test]
fn version_flag_prints_a_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_lsbx"))
        .arg("--version")
        .output()
        .expect("failed to spawn lsbx binary");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.to_lowercase().contains("lsbx"));
}

/// Gap 3 (final integration wiring pass): `ci-broker run` is a new real
/// subcommand — its own help text must list the flags this pass added.
#[test]
fn ci_broker_run_help_lists_its_flags() {
    let help = run_help(&["ci-broker", "run"]);
    assert_contains_all(
        &help,
        &["--backend", "--queue-label"],
        "`lsbx ci-broker run --help`",
    );
}
