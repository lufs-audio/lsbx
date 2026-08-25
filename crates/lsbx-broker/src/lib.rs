//! `lsbx-broker` — the zero-idle CI runner broker.
//!
//! This crate is built up across three units that land in order (16 -> 17 ->
//! 18), sharing this one crate but owning disjoint files within it:
//!
//! - Unit 16: `auth` + `github_client` — GitHub App RS256 JWT signing,
//!   installation-token exchange/caching, and org-wide repo discovery.
//! - Unit 17: `poll` + `labels` — queue polling and placement-label
//!   matching, built on top of Unit 16's `GitHubClient`. Also extends
//!   `github_client` with the two new authenticated, paginated calls this
//!   unit's polling needs (`GitHubClient::workflow_runs`/
//!   `GitHubClient::run_jobs`) — additions to an existing crate-internal
//!   file, not a re-implementation of Unit 16's auth/discovery logic. See
//!   `poll.rs`'s module doc comment for the `Poller` driver loop this unit
//!   adds to close the gap between the acceptance criteria's polling-cadence
//!   prose and the interface contract's primitive-only code block.
//! - Unit 18 (this unit): `reconcile` + `job_record` — job<->VM
//!   reconciliation, built on top of Unit 17's poll loop. Also extends
//!   `github_client` with `JobSummary::runner_name` and
//!   `GitHubClient::job_for_runner` (divergence detection needs a GitHub
//!   call Unit 16/17 had no reason to add yet) — see `github_client.rs`'s
//!   "Unit 18 addition" doc comment and `reconcile.rs`'s own module doc
//!   comment ("Gap 1", "Gap 5") for the full design writeups, including the
//!   new `run_broker` entry point this unit adds since none existed
//!   anywhere in this crate before it.
//!
//! Module wiring only lives here; no operational logic.

pub mod auth;
mod error_map;
pub mod github_client;
pub mod job_record;
pub mod labels;
pub mod poll;
pub mod reconcile;
