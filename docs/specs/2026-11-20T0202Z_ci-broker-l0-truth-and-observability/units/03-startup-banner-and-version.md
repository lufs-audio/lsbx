# Unit 03 — Startup banner + git commit in `--version`

## Objective
Make "what is this process configured to do, and which build is it" answerable from the log and the
binary itself, so neither a config mismatch nor a stale binary can hide again.

## Context
- The running binary was built 2026-11-13T22:46 and `main` moved on 2026-11-14T20:04 — six days of
  drift that nothing surfaced. `lsbx --version` prints `lsbx 0.1.0` with no commit.
- No git-commit mechanism exists in the tree today (verified: no `GIT_COMMIT`, no `git rev-parse` in
  any `build.rs` or crate source).
- Broker startup currently logs **only** systemd's `Started lsbx CI broker ...` line — the process
  itself emits nothing at startup.
- Effective config is discoverable only from `/proc/<pid>/environ`, which is how this was diagnosed.
- Phase SPEC §5.1 explains why the banner logs config rather than validating it against workflow
  files (REST budget).

## Acceptance criteria
- [ ] `lsbx --version` prints `lsbx <semver> (<git-commit>)`, falling back to `unknown` when the
  build had no git metadata, and never panics or fails the build when git is unavailable.
- [ ] The commit is baked at compile time from `git rev-parse --short HEAD` via a `build.rs`, and
  exposed as `LSBX_GIT_COMMIT`.
- [ ] On startup the broker emits one `info!` banner containing: version, commit, queue labels,
  runner labels, runner group, repo mode (`static N repos` vs `discovery`), poll interval, fallback
  delay.
- [ ] The banner is emitted before the first tick.
- [ ] A `warn!` is emitted at startup if `queue_labels` is empty (defensive; the parser already
  falls back, so this should be unreachable) — proving the banner path is not silently skipped.
- [ ] `cargo build --release` succeeds with no build script warnings; `cargo clippy --workspace
  --all-targets -- -D warnings` passes.

## Interface contract
```rust
// crates/lsbx-cli/build.rs
//   emits:  cargo:rustc-env=LSBX_GIT_COMMIT=<short sha | "unknown">
//   re-runs when: .git/HEAD or .git/refs/heads changes (cargo:rerun-if-changed)

// crates/lsbx-cli/src/lib.rs (version string)
//   "lsbx {CARGO_PKG_VERSION} ({LSBX_GIT_COMMIT})"

// Broker startup banner (tracing, target = "lsbx_broker"):
//   version=.. commit=.. queue_labels=.. runner_labels=.. runner_group=..
//   repo_mode=static|discovery repos=N poll_interval=.. fallback_delay=..
```

## Boundaries
- **Touches:** `crates/lsbx-cli/build.rs` (new), `crates/lsbx-cli/src/lib.rs` (version string only),
  `crates/lsbx-broker/src/reconcile.rs` (banner emission at loop start).
- **Does NOT touch:** `poll.rs` counters (Unit 02), `labels.rs`, env files (Unit 01), workflows.

## Verification
```bash
cargo build --release -p lsbx-cli
./target/release/lsbx --version          # expect "lsbx 0.1.0 (<sha>)"
git rev-parse --short HEAD               # must match the parenthesised value
cargo clippy --workspace --all-targets -- -D warnings
```
