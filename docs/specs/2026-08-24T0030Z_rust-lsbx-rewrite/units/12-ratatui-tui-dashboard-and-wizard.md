# Unit 12 — Ratatui TUI Dashboard & Wizard

## Objective
Implement the interactive TUI: a live dashboard (bare `lsbx` invocation on a TTY, matching existing behavior) and a new guided wizard for `up` and `golden build` (SPEC.md Deviation 14).

## Context
Layer 6, depends on Unit 10. The dashboard is a port of existing behavior; the wizard is new functionality requested by the brief, not confirmed present in the existing CLI's flag surface.

## Acceptance criteria
- [ ] Bare `lsbx` on an interactive TTY launches the `ratatui` dashboard; on a non-TTY (piped/redirected) it falls back to the same JSON/table status summary Unit 11 produces for `lsbx status` — it must never launch a full-screen TUI inside a script.
- [ ] Dashboard shows a live sandbox list (id, profile, host, lease remaining, streaming) refreshed on an interval, driven entirely by `LsbxOps::list()`/`status()` — no direct backend or store access from TUI code.
- [ ] Dashboard supports, at minimum: navigating the list, viewing `info` detail for the selected sandbox, triggering `destroy` behind a confirmation step, and quitting (`q` / Ctrl-C) with clean terminal restoration — no raw-mode leak on exit or panic.
- [ ] Wizard mode (`lsbx up --wizard`, `lsbx golden build --wizard`) guides profile/golden selection, resource sizing, and lease duration through a step-by-step form, then calls the exact same `LsbxOps::create`/`golden_build` a non-interactive invocation would — the wizard is a UI, not a second validation path.
- [ ] A panic anywhere inside the TUI event loop restores the terminal (raw mode off, alternate screen closed) before unwinding further — proven by a test with a deliberately panicking code path, not just "it worked when I tried it."

## Interface contract
```rust
// src/dashboard.rs
pub struct DashboardState {
    sandboxes: Vec<lsbx_kernel::types::PublicSandbox>,
    selected: usize,
}

pub async fn run_dashboard(ops: &lsbx_ops::LsbxOps) -> Result<(), lsbx_kernel::error::LsbxError>;

// src/wizard.rs
pub struct WizardAnswers {
    pub profile: String,
    pub cpu: u32,
    pub memory: String,
    pub lease: std::time::Duration,
}

pub async fn run_up_wizard(ops: &lsbx_ops::LsbxOps) -> Result<lsbx_kernel::types::PublicSandbox, lsbx_kernel::error::LsbxError>;
pub async fn run_golden_build_wizard(ops: &lsbx_ops::LsbxOps) -> Result<lsbx_golden::registry::GoldenConfig, lsbx_kernel::error::LsbxError>;

/// Installed once at process start; guarantees terminal restoration on panic.
pub fn install_panic_restore_hook();
```

## Boundaries — do NOT touch
Does not parse CLI flags itself — Unit 11 decides when to invoke `run_dashboard`/the wizard functions and passes already-parsed arguments in. Contains no operational logic beyond input collection and rendering — every mutation goes through `LsbxOps`.

## Output
- `crates/lsbx-tui/Cargo.toml`
- `crates/lsbx-tui/src/lib.rs`
- `crates/lsbx-tui/src/dashboard.rs`
- `crates/lsbx-tui/src/wizard.rs`
- `crates/lsbx-tui/tests/test_panic_restore.rs`
- `crates/lsbx-tui/tests/test_wizard_calls_same_ops.rs`

## Verification
```bash
cargo check -p lsbx-tui --message-format=json
cargo clippy -p lsbx-tui --all-targets --all-features -- -D warnings
cargo test -p lsbx-tui --test test_panic_restore
cargo test -p lsbx-tui --test test_wizard_calls_same_ops
```
Scenario: `test_wizard_calls_same_ops` feeds scripted `WizardAnswers` (bypassing real terminal input) and asserts the resulting `LsbxOps::create` request struct is byte-identical to what `lsbx up <profile> --cpu N --memory M --lease D` would construct — proving the wizard carries no parallel validation logic.
