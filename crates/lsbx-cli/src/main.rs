//! Thin binary shim (Unit 11 acceptance criteria: "`main()`'s only job
//! after arg parsing is: call the matching `LsbxOps` method, format the
//! result, `std::process::exit(error.exit_code() as i32)` on failure. No
//! operational branching lives in `main.rs` or `cli.rs`.").
//!
//! Every real behavior — argument parsing, backend construction, request
//! translation, dispatch, formatting — lives in `lsbx_cli::run` (`lib.rs`),
//! so it is unit-testable without spawning a process. This file's only job
//! is calling that and exiting with the resulting code.
//!
//! Tracing is the one exception to "nothing operational here": a subscriber is
//! process-wide setup, and it must exist before the first `tracing` call or
//! every one of them — including every pre-existing warning in
//! `lsbx-broker` — is a silent no-op. That was true until 2026-11-20: the
//! workspace emitted no logs at all, which is a large part of why a label
//! mismatch could stop CI for five days without anything saying so.

/// Installs the global tracing subscriber.
///
/// Default filter is `info` rather than `warn`: the whole point of the L0
/// work is that the broker states what it is doing once per poll. A `warn`
/// default would reproduce the original silence. `RUST_LOG` overrides it when
/// set, and `RUST_LOG=debug` turns on the per-cell poll lines.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // `try_init` rather than `init`: a second initialisation (tests, embedded
    // use) must not panic the process.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn main() {
    init_tracing();
    let code = lsbx_cli::run().await;
    std::process::exit(code);
}
