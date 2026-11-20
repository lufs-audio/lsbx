# Unit 02 — `Poller::tick` observability: counters, per-cell debug, zero-eligible warning

## Objective
Make every poll self-describing: report what was scanned, how many jobs were seen, how many were
eligible, and — when jobs are seen but none are eligible — warn once with the configured and
observed labels so the mismatch names itself.

## Context
- `crates/lsbx-broker/src/poll.rs::tick` currently returns `Result<Vec<QueuedJob>, LsbxError>` and
  keeps **no** counts. It has **zero** `tracing` calls (verified).
- `crates/lsbx-broker/src/labels.rs::is_eligible` is the **rule** and is correct. Do not change the
  rule; only expose what it decides.
- Two call sites drive `tick`: `crates/lsbx-broker/src/reconcile.rs:1248` and `:1338`.
- `tracing` is already a dependency (`tracing.workspace = true`), and `reconcile.rs` already uses it
  (15 call sites), so the subscriber is wired.
- Loop shape today, for reference: `for repo { for label { queued_jobs(...) } }`.
- Rationale for not scanning workflows at startup: see the phase SPEC §5.1 (REST budget).

## Acceptance criteria
- [ ] `tick` returns a `TickOutcome` carrying `eligible: Vec<QueuedJob>`, `repos_scanned: usize`,
      `labels_scanned: usize`, `jobs_seen: usize`, `jobs_skipped: usize`.
- [ ] Both call sites in `reconcile.rs` are updated to use `outcome.eligible`; behaviour on dispatch
      is unchanged.
- [ ] One `info!` per tick: `repos`, `labels`, `seen`, `eligible` (and `skipped`).
- [ ] One `debug!` per (repo, label) cell with its job count, so an empty cell is attributable.
- [ ] After `>= 2` consecutive ticks with `jobs_seen > 0 && eligible == 0`, emit exactly **one**
      `warn!` naming `configured=<labels>` and `seen=<distinct job labels>`; reset the streak when a
      tick yields `eligible > 0`.
- [ ] The warning does not repeat every tick while the condition persists (one warning per streak).
- [ ] A unit test asserts the warning fires once and names both label sets, using a fake client or
      by testing the pure decision function.
- [ ] `tick` still returns eligible jobs in the same order/shape as before (no dispatch regression).

## Interface contract
```rust
pub struct TickOutcome {
    pub eligible: Vec<QueuedJob>,
    pub repos_scanned: usize,
    pub labels_scanned: usize,
    pub jobs_seen: usize,
    pub jobs_skipped: usize,
}

impl Poller {
    pub async fn tick(&mut self, client: &GitHubClient, now: SystemTime)
        -> Result<TickOutcome, LsbxError>;
}

// Pure, testable decision — no I/O, no clock:
pub fn zero_eligible_should_warn(streak: u32, jobs_seen: usize, eligible: usize) -> bool;
```

## Boundaries
- **Touches:** `crates/lsbx-broker/src/poll.rs`, `crates/lsbx-broker/src/reconcile.rs` (call sites +
  the banner's caller if needed).
- **Does NOT touch:** `labels.rs` (rule unchanged), `lib.rs` re-exports beyond adding `TickOutcome`,
  any workflow, any golden.

## Verification
```bash
cargo test -p lsbx-broker
cargo clippy -p lsbx-broker --all-targets -- -D warnings
# live: after restart, a poll with queued jobs must show an info line with non-zero seen
journalctl -u lsbx-ci-broker -f | grep -E "tick|eligible"
```
