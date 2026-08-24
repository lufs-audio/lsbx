# Unit 17 — CI Broker: Queue Polling & Label Matching

## Objective
Implement the poll loop, label matching, and fallback delay — preserving every documented timing constant and fail-closed behavior from the existing broker exactly.

## Context
Layer 7, depends on Unit 16 (uses its `GitHubClient`). Second of three units sharing the `lsbx-broker` crate; owns `src/poll.rs` and `src/labels.rs`.

## Acceptance criteria
- [ ] Polls every `poll_interval` (default 15s, `LSBX_CI_POLL_INTERVAL` env override) across all discovered repos and a comma-split `queue_label` list.
- [ ] Refreshes the repo list every `repo_refresh_interval` (default 300s) rather than on every poll tick.
- [ ] `FALLBACK_QUEUE_LABEL = "lsbx-default"` is a named constant, not a magic string repeated at call sites.
- [ ] Dedicated placement labels (anything other than the fallback label) are claimed immediately on first sight; the shared `lsbx-default` label requires `queued_age_seconds(job) >= fallback_delay` (default 60s, `LSBX_CI_FALLBACK_DELAY` env override) before it becomes eligible.
- [ ] Fails closed on a malformed `created_at`: `queued_age_seconds` returning `None` blocks eligibility rather than allowing it — a dedicated test forces a malformed timestamp and asserts the job is never claimed.
- [ ] `queued_jobs(label, repo)` lists `GET .../actions/runs` (status queued/in_progress), then per-run `GET .../jobs`, filtering `job.status == "queued" && label in job.labels` — the same two-step traversal as the existing system, not a hypothetical single combined endpoint that doesn't exist.

## Interface contract
```rust
// src/poll.rs
use lsbx_kernel::error::LsbxError;
use super::github_client::GitHubClient;

pub const FALLBACK_QUEUE_LABEL: &str = "lsbx-default";

pub struct PollConfig {
    pub poll_interval: std::time::Duration,         // default 15s
    pub repo_refresh_interval: std::time::Duration, // default 300s
    pub fallback_delay: std::time::Duration,        // default 60s
    pub queue_labels: Vec<String>,
}

pub struct QueuedJob {
    pub job_id: u64,
    pub run_id: u64,
    pub repository: String,
    pub labels: Vec<String>,
    pub created_at: Option<String>, // None if unparseable — fail-closed downstream
}

pub async fn queued_jobs(client: &GitHubClient, label: &str, repo: &str) -> Result<Vec<QueuedJob>, LsbxError>;

// src/labels.rs
/// Returns None if `created_at` is missing or unparseable — callers must treat
/// None as "not eligible," never as "eligible by default."
pub fn queued_age_seconds(job: &QueuedJob, now: std::time::SystemTime) -> Option<u64>;

pub fn is_eligible(job: &QueuedJob, cfg: &PollConfig, now: std::time::SystemTime) -> bool;
```

## Boundaries — do NOT touch
Does not implement GitHub App auth or the HTTP client (Unit 16 owns `src/auth.rs`/`src/github_client.rs`; this unit only calls `GitHubClient`). Does not dispatch a VM or reconcile a job's outcome (Unit 18) — this unit only decides which queued jobs are eligible right now.

## Output
- `crates/lsbx-broker/src/poll.rs`
- `crates/lsbx-broker/src/labels.rs`
- `crates/lsbx-broker/tests/test_fallback_delay.rs`
- `crates/lsbx-broker/tests/test_malformed_timestamp_fails_closed.rs`
- `crates/lsbx-broker/tests/test_label_eligibility.rs`

## Verification
```bash
cargo check -p lsbx-broker --message-format=json
cargo clippy -p lsbx-broker --all-targets --all-features -- -D warnings
cargo test -p lsbx-broker --test test_fallback_delay
cargo test -p lsbx-broker --test test_malformed_timestamp_fails_closed
cargo test -p lsbx-broker --test test_label_eligibility
```
Scenario: `test_malformed_timestamp_fails_closed` constructs a `QueuedJob` with `created_at: Some("not-a-timestamp".into())`, asserts `queued_age_seconds` returns `None`, and asserts `is_eligible` returns `false` for the fallback label — never `true` by default on a parse failure.
