# Unit 11 — CLI Surface & Output Formatting

## Objective
Implement the `clap` v4 CLI, mapping every existing subcommand and flag onto `lsbx-ops` calls, with human-table and JSON output sharing one formatting path.

## Context
Layer 6, depends on Unit 10. Preserves the exact existing subcommand/flag surface found in the current `lufs_sandbox/lsbx/cli.py` — this is Door 1 from SPEC.md §4.8.

## Acceptance criteria
- [ ] Global flags match exactly: `--json/-j`, `--verbose/-v`, `--quiet/-q`, `--no-color`, `--config/-c`, `--backend/-b` (`libvirt|exedev|demo|auto`), `--images/-i`, `--state-dir/-s`, `--version`.
- [ ] Subcommands match exactly: `up <profile> [--count/-n --name --task-id/-t --lease/-l --no-verify --ready-timeout]`, `down [id...] [--all]`, `list [--profile --expired]`, `exec <id> [--timeout] [command...]`, `put <id> <source> <destination>`, `get <id> <source> <destination>`, `renew <id> <duration>`, `console <id>`, `info <id>`, `status`, `profiles [--full]`, `images`, `reap [--ttl --dry-run]`, `serve [--host --port --stream-port --token --reap-ttl --daemon]`, `bootstrap [--target --no-services --no-verify --force --dry-run]`, `golden {list|build|verify|register|delete}` (flags per Unit 08), `config [--show --init --set KEY=VALUE --path --force]`, `logs [--follow --command --since --limit --show]`, `mcp`, and a bare invocation that shows a dashboard/status summary.
- [ ] `--backend auto` probes `libvirt` then `exedev` then `demo`, matching the existing fallback order, and reports which backend it selected via `status`/`--verbose`.
- [ ] Every subcommand's output passes through one formatting layer producing either a human table or the `Envelope<T>` JSON shape (Unit 01), selected by `--json` — never two independently-maintained rendering paths per command.
- [ ] `main()`'s only job after arg parsing is: call the matching `LsbxOps` method, format the result, `std::process::exit(error.exit_code() as i32)` on failure. No operational branching lives in `main.rs` or `cli.rs`.
- [ ] A snapshot test asserts full `--help` output for every subcommand, so a flag rename shows up as an obvious diff.

## Interface contract
```rust
// src/cli.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lsbx", version)]
pub struct Cli {
    #[arg(long, short = 'j', global = true)] pub json: bool,
    #[arg(long, short = 'v', global = true)] pub verbose: bool,
    #[arg(long, short = 'q', global = true)] pub quiet: bool,
    #[arg(long, global = true)] pub no_color: bool,
    #[arg(long, short = 'c', global = true)] pub config: Option<std::path::PathBuf>,
    #[arg(long, short = 'b', global = true)] pub backend: Option<BackendChoice>,
    #[arg(long, short = 'i', global = true)] pub images: Option<std::path::PathBuf>,
    #[arg(long, short = 's', global = true)] pub state_dir: Option<std::path::PathBuf>,
    #[command(subcommand)] pub command: Option<Command>,
}

#[derive(Clone, clap::ValueEnum)]
pub enum BackendChoice { Libvirt, Exedev, Demo, Auto }

#[derive(Subcommand)]
pub enum Command {
    Up { profile: String, #[arg(long, short = 'n')] count: Option<u32>, #[arg(long)] name: Option<String>, #[arg(long, short = 't')] task_id: Option<String>, #[arg(long, short = 'l')] lease: Option<String>, #[arg(long)] no_verify: bool, #[arg(long)] ready_timeout: Option<u64> },
    Down { ids: Vec<String>, #[arg(long)] all: bool },
    List { #[arg(long)] profile: Option<String>, #[arg(long)] expired: bool },
    Exec { id: String, #[arg(long)] timeout: Option<u64>, command: Vec<String> },
    Put { id: String, source: std::path::PathBuf, destination: String },
    Get { id: String, source: String, destination: std::path::PathBuf },
    Renew { id: String, duration: String },
    Console { id: String },
    Info { id: String },
    Status,
    Profiles { #[arg(long)] full: bool },
    Images,
    Reap { #[arg(long)] ttl: Option<String>, #[arg(long)] dry_run: bool },
    Serve { #[arg(long)] host: Option<String>, #[arg(long)] port: Option<u16>, #[arg(long)] stream_port: Option<u16>, #[arg(long)] token: Option<String>, #[arg(long)] reap_ttl: Option<String>, #[arg(long)] daemon: bool },
    Bootstrap { #[arg(long)] target: Option<String>, #[arg(long)] no_services: bool, #[arg(long)] no_verify: bool, #[arg(long)] force: bool, #[arg(long)] dry_run: bool },
    Golden { #[command(subcommand)] action: GoldenCommand },
    Config { #[arg(long)] show: bool, #[arg(long)] init: bool, #[arg(long)] set: Option<String>, #[arg(long)] path: bool, #[arg(long)] force: bool },
    Logs { #[arg(long)] follow: bool, #[arg(long)] command: Option<String>, #[arg(long)] since: Option<String>, #[arg(long)] limit: Option<usize>, #[arg(long)] show: bool },
    Mcp,
}

#[derive(Subcommand)]
pub enum GoldenCommand {
    List,
    Build { name: String, #[arg(long)] from: String, #[arg(long)] script: std::path::PathBuf, #[arg(long)] flavor: String, #[arg(long)] cpu: u32, #[arg(long)] memory: String, #[arg(long)] streaming: Option<String>, #[arg(long)] register: bool, #[arg(long)] no_cleanup: bool, #[arg(long)] interactive: bool, #[arg(long)] shell: bool, #[arg(long)] dry_run: bool },
    Verify { name: String },
    Register { name: String, #[arg(long)] profile: Option<String>, #[arg(long)] base: String, #[arg(long)] flavor: String, #[arg(long)] streaming: Option<String>, #[arg(long)] capabilities: Vec<String>, #[arg(long)] healthcheck: Vec<String>, #[arg(long)] content_hash: Option<String>, #[arg(long)] replace: bool },
    Delete { name: String, #[arg(long)] keep_snapshot: bool },
}

// src/format.rs
pub trait Formattable { fn to_human_table(&self) -> String; }
pub fn render<T: serde::Serialize + Formattable>(value: &T, as_json: bool) -> String;
```

## Boundaries — do NOT touch
Does not implement the TUI dashboard/wizard (Unit 12) — the bare-invocation and `--wizard` paths delegate to `lsbx-tui` when stdout is a TTY, falling back to this unit's own JSON/table summary otherwise. Does not implement `serve`'s HTTP server (Unit 13) or `mcp`'s MCP server (Unit 15) — those subcommands only construct and hand off to the relevant crate.

## Output
- `crates/lsbx-cli/Cargo.toml`
- `crates/lsbx-cli/src/main.rs`
- `crates/lsbx-cli/src/cli.rs`
- `crates/lsbx-cli/src/format.rs`
- `crates/lsbx-cli/tests/test_help_snapshot.rs`
- `crates/lsbx-cli/tests/test_backend_auto_probe.rs`

## Verification
```bash
cargo check -p lsbx-cli --message-format=json
cargo clippy -p lsbx-cli --all-targets --all-features -- -D warnings
cargo test -p lsbx-cli --test test_help_snapshot
cargo test -p lsbx-cli --test test_backend_auto_probe
```
Scenario: `lsbx up default --backend demo --json | jq .status` must print `"success"` end-to-end, exercised as a `std::process::Command`-spawned integration test rather than only a unit test of the arg parser.
