//! Thin binary shim (Unit 11 acceptance criteria: "`main()`'s only job
//! after arg parsing is: call the matching `LsbxOps` method, format the
//! result, `std::process::exit(error.exit_code() as i32)` on failure. No
//! operational branching lives in `main.rs` or `cli.rs`.").
//!
//! Every real behavior — argument parsing, backend construction, request
//! translation, dispatch, formatting — lives in `lsbx_cli::run` (`lib.rs`),
//! so it is unit-testable without spawning a process. This file's only job
//! is calling that and exiting with the resulting code.

#[tokio::main]
async fn main() {
    let code = lsbx_cli::run().await;
    std::process::exit(code);
}
