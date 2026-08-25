//! `clap` v4 CLI surface (Unit 11).
//!
//! Preserves the exact existing subcommand/flag surface found in the
//! current `lufs_sandbox/lsbx/cli.py` (SPEC.md §4.8, Door 1). This module
//! contains **only** argument parsing — no operational branching lives
//! here (that's `lib.rs::run`'s job, per this unit's own acceptance
//! criteria: "`main()`'s only job after arg parsing is: call the matching
//! `LsbxOps` method, format the result, `std::process::exit(...)` on
//! failure. No operational branching lives in `main.rs` or `cli.rs`.").

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lsbx", version)]
pub struct Cli {
    #[arg(long, short = 'j', global = true)]
    pub json: bool,
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,
    #[arg(long, global = true)]
    pub no_color: bool,
    #[arg(long, short = 'c', global = true)]
    pub config: Option<std::path::PathBuf>,
    #[arg(long, short = 'b', global = true)]
    pub backend: Option<BackendChoice>,
    #[arg(long, short = 'i', global = true)]
    pub images: Option<std::path::PathBuf>,
    #[arg(long, short = 's', global = true)]
    pub state_dir: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum BackendChoice {
    Libvirt,
    Exedev,
    Demo,
    Auto,
}

#[derive(Subcommand)]
pub enum Command {
    Up {
        profile: String,
        #[arg(long, short = 'n')]
        count: Option<u32>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, short = 't')]
        task_id: Option<String>,
        #[arg(long, short = 'l')]
        lease: Option<String>,
        #[arg(long)]
        no_verify: bool,
        #[arg(long)]
        ready_timeout: Option<u64>,
    },
    Down {
        ids: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    List {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        expired: bool,
    },
    Exec {
        id: String,
        #[arg(long)]
        timeout: Option<u64>,
        command: Vec<String>,
    },
    Put {
        id: String,
        source: std::path::PathBuf,
        destination: String,
    },
    Get {
        id: String,
        source: String,
        destination: std::path::PathBuf,
    },
    Renew {
        id: String,
        duration: String,
    },
    Console {
        id: String,
    },
    Info {
        id: String,
    },
    Status,
    Profiles {
        #[arg(long)]
        full: bool,
    },
    Images,
    Reap {
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        stream_port: Option<u16>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        reap_ttl: Option<String>,
        #[arg(long)]
        daemon: bool,
    },
    Bootstrap {
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        no_services: bool,
        #[arg(long)]
        no_verify: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Golden {
        #[command(subcommand)]
        action: GoldenCommand,
    },
    Config {
        #[arg(long)]
        show: bool,
        #[arg(long)]
        init: bool,
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        path: bool,
        #[arg(long)]
        force: bool,
    },
    Logs {
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        show: bool,
    },
    Mcp,
}

#[derive(Subcommand)]
pub enum GoldenCommand {
    List,
    Build {
        name: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        script: std::path::PathBuf,
        #[arg(long)]
        flavor: String,
        #[arg(long)]
        cpu: u32,
        #[arg(long)]
        memory: String,
        #[arg(long)]
        streaming: Option<String>,
        #[arg(long)]
        register: bool,
        #[arg(long)]
        no_cleanup: bool,
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        shell: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Verify {
        name: String,
    },
    Register {
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        base: String,
        #[arg(long)]
        flavor: String,
        #[arg(long)]
        streaming: Option<String>,
        #[arg(long)]
        capabilities: Vec<String>,
        #[arg(long)]
        healthcheck: Vec<String>,
        #[arg(long)]
        content_hash: Option<String>,
        #[arg(long)]
        replace: bool,
    },
    Delete {
        name: String,
        #[arg(long)]
        keep_snapshot: bool,
    },
}
