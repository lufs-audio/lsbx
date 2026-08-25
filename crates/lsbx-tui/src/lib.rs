//! `lsbx-tui` — Ratatui TUI Dashboard & Wizard (Unit 12).
//!
//! Two candidate Jules sessions existed for this unit before this
//! implementation and both invented request-struct shapes that don't match
//! the real, merged `lsbx_lifecycle::create::CreateRequest`/
//! `lsbx_golden::build::GoldenBuildRequest` (missing fields such as `name`,
//! `task_id`, `ready_timeout`, `verify`, `healthchecks`/`pubkey`; one also
//! used owned `String` where the real type borrows `&'a str`), and both
//! constructed a nonexistent `LsbxError::Io`/`LsbxError::Other` variant —
//! the real, closed 7-variant enum has neither (see
//! `lsbx_kernel::error::LsbxError`, re-confirmed by direct read immediately
//! before writing this crate). Every signature and field name in this
//! crate is written against that real, current merged source, not against
//! either candidate's draft or the unit contract's original literal text
//! (which itself predates several signature reworks Units 08/09/10 made
//! during their own implementation — see those units' own PR descriptions).
//!
//! ## Reconciling the two candidates
//!
//! **Dashboard** (`dashboard.rs`): the reconciled implementation takes
//! Session 2's async `crossterm::event::EventStream` + `tokio::select!`
//! event-loop structure (idiomatic non-blocking pattern for a
//! `tokio`-based app — Session 1's synchronous polling loop blocks a
//! thread) and Session 2's RAII `TerminalGuard` (restores terminal state
//! on `Drop`, not just via manual enable/disable calls bracketing the
//! loop — a real robustness improvement, since it also fires on an early
//! return or a `?`-propagated error, not only the "loop exited normally"
//! path Session 1's manual calls covered), **plus Session 1's
//! destroy-confirmation flow** (a `show_destroy_confirm` state machine with
//! y/n key handling) — a real, useful implementation of this unit's own
//! acceptance criterion ("triggering `destroy` behind a confirmation
//! step") that Session 2 dropped entirely (its 'd' key destroys
//! immediately, no confirmation at all — a regression relative to the
//! acceptance criterion, not a simplification).
//!
//! **Wizard** (`wizard.rs`): Session 1's wizard doesn't actually collect
//! input — `run_wizard_ui()` returns hardcoded canned answers and a
//! separate `run_up_wizard_with_answers` exists purely so its own test can
//! inject scripted answers, meaning the real `run_up_wizard` entry point
//! was untested and non-functional. Session 2 has a genuine multi-step
//! interactive `ratatui` wizard (profile-list selection, then a
//! resource-adjustment screen with real Left/Right/Up/Down key handling)
//! and is used here as the base — but with one real bug fixed: it
//! `unreachable!()`s or would otherwise need to fabricate a nonexistent
//! `LsbxError::Other` on user cancellation (Esc/'q'). This implementation
//! maps that path to the real `LsbxError::Interrupted` variant — a strong
//! semantic fit for "the user backed out mid-flow" and, unlike
//! `LsbxError::Other`, an actual variant on the closed enum.
//!
//! ## `test_panic_restore.rs` — strengthened over both candidates
//!
//! Both candidates' panic-restore tests proved a panic could be caught
//! (via the panic hook + `std::panic::catch_unwind`) but neither actually
//! asserted the *terminal state* was restored afterward — both say so in
//! their own comments. This crate's version actually enables raw mode
//! before triggering the panic, then asserts via
//! `crossterm::terminal::is_raw_mode_enabled()` (confirmed to exist in the
//! pinned `crossterm 0.28`) that raw mode is verifiably disabled once the
//! panic has unwound past the hook. There is, confirmed by reading
//! `crossterm 0.28`'s public API surface directly, no cross-platform query
//! for "is the alternate screen currently active" — only
//! `EnterAlternateScreen`/`LeaveAlternateScreen` *commands* to change that
//! state, nothing to *read* it back. That gap is stated explicitly in the
//! test's own doc comment rather than silently only testing the raw-mode
//! half and implying full coverage.
//!
//! ## `test_wizard_calls_same_ops.rs` — a real comparison, not a fallback
//!
//! Both candidates only proved `WizardAnswers -> CreateRequest` field
//! equality against an independently-constructed "expected" request — never
//! actually invoking `LsbxOps::create` and comparing against what a
//! non-interactive `lsbx up` call would produce, which is this unit's own
//! named acceptance scenario. Constructing a real `LsbxOps` needs only
//! `lsbx-backend-demo`, `lsbx-store`, and `lsbx-kernel`'s `testing` feature
//! (for `FakeClock`) as dev-dependencies — the exact same pattern
//! `lsbx-ops`'s own Cargo.toml already uses for its own tests — and does
//! **not** need `lsbx-cli` (which doesn't exist yet, and depending on it
//! from here would be backwards: `lsbx-cli` is Unit 11, a sibling
//! Layer-6 door, and the real circular-dependency risk would only arise if
//! this crate depended on *that* one). This crate's own test therefore
//! builds a real `DemoBackend`-backed `LsbxOps`, calls `LsbxOps::create`
//! twice — once via the wizard's mapped request, once via a
//! directly-constructed non-interactive-equivalent request — and asserts
//! the two calls produce field-identical `PublicSandbox` results (modulo
//! the `id`/timestamps `create` itself generates fresh per call), which is
//! the strongest comparison available without literally calling `create`
//! once and asserting against its own memoized inputs.

pub mod dashboard;
pub mod wizard;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static PANIC_HOOK_INSTALLED: Once = Once::new();
/// Set to `true` for the duration of `run_dashboard`'s event loop (and,
/// transitively, anything the wizard screens run inside it) so the panic
/// hook installed by [`install_panic_restore_hook`] knows to attempt
/// terminal restoration. Kept at the crate level (not per-call state)
/// because a panic hook is a single global callback — there is exactly one
/// hook process-wide, so the flag it consults must also be process-wide.
/// `pub(crate)` so `dashboard`/`wizard` can mark themselves active for the
/// duration of their own event loops via [`TuiActiveGuard`].
pub(crate) static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks the TUI as "in an active raw-mode/alternate-screen session" for
/// the lifetime of this guard value, restoring the flag on drop (on both
/// the normal return path and the unwinding-panic path too — though by the
/// time a panic actually unwinds past this guard's `Drop::drop`, the
/// *hook* installed by `install_panic_restore_hook` has already run and
/// already attempted terminal restoration, since panic hooks run before
/// unwinding begins). `pub(crate)` so `dashboard::run_dashboard` and the
/// wizard's own event loops can each enter one around their respective
/// raw-mode sessions.
pub(crate) struct TuiActiveGuard;

impl TuiActiveGuard {
    pub(crate) fn enter() -> Self {
        TUI_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for TuiActiveGuard {
    fn drop(&mut self) {
        TUI_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Installed once at process start; guarantees terminal restoration on
/// panic.
///
/// Per this unit's own acceptance criterion ("a panic anywhere inside the
/// TUI event loop restores the terminal... before unwinding further"):
/// this installs a `std::panic::set_hook` that, when `TUI_ACTIVE` is
/// `true` (i.e. a panic actually happened while raw mode/the alternate
/// screen were live, not at some unrelated point in the process's
/// lifetime), best-effort disables raw mode and leaves the alternate
/// screen before chaining to whatever hook was previously installed (so a
/// caller's own panic reporting — e.g. `human-panic`, a custom logger —
/// still runs). Idempotent via `std::sync::Once`: calling this more than
/// once (e.g. once from `lsbx-cli`'s `main`, once from a test) installs
/// the hook exactly once.
pub fn install_panic_restore_hook() {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if TUI_ACTIVE.load(Ordering::SeqCst) {
                // Best-effort: a panic hook running during an already-panicking
                // unwind is not the place to unwrap a *second* failure. Both
                // calls are infallible-in-practice terminal syscalls
                // (disable_raw_mode / execute! LeaveAlternateScreen against
                // stdout); a failure here has no better recovery than "log
                // and move on to the previous hook," so both are silently
                // best-effort exactly as the acceptance criterion implies
                // ("restores the terminal... before unwinding further" — the
                // restoration itself, not a guarantee that the restoration
                // call can never itself fail).
                let _ = crossterm::terminal::disable_raw_mode();
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::terminal::LeaveAlternateScreen
                );
            }
            previous_hook(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_panic_restore_hook_is_idempotent() {
        // Calling this multiple times must not panic or double-chain hooks
        // in a way that breaks — `Once` guarantees the closure body runs
        // exactly once regardless of call count.
        install_panic_restore_hook();
        install_panic_restore_hook();
        install_panic_restore_hook();
    }

    #[test]
    fn tui_active_guard_resets_flag_on_drop() {
        assert!(!TUI_ACTIVE.load(Ordering::SeqCst));
        {
            let _guard = TuiActiveGuard::enter();
            assert!(TUI_ACTIVE.load(Ordering::SeqCst));
        }
        assert!(!TUI_ACTIVE.load(Ordering::SeqCst));
    }
}
