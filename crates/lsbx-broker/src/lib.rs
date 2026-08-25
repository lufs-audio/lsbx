//! `lsbx-broker` — the zero-idle CI runner broker.
//!
//! This crate is built up across three units that land in order (16 -> 17 ->
//! 18), sharing this one crate but owning disjoint files within it:
//!
//! - Unit 16: `auth` + `github_client` — GitHub App RS256 JWT signing,
//!   installation-token exchange/caching, and org-wide repo discovery.
//! - Unit 17 (this unit): `poll` + `labels` — queue polling and
//!   placement-label matching, built on top of Unit 16's `GitHubClient`.
//!   Also extends `github_client` with the two new authenticated,
//!   paginated calls this unit's polling needs
//!   (`GitHubClient::workflow_runs`/`GitHubClient::run_jobs`) — additions to
//!   an existing crate-internal file, not a re-implementation of Unit 16's
//!   auth/discovery logic. See `poll.rs`'s module doc comment for the
//!   `Poller` driver loop this unit adds to close the gap between the
//!   acceptance criteria's polling-cadence prose and the interface
//!   contract's primitive-only code block.
//! - Unit 18: `reconcile` + `job_record` — job<->VM reconciliation, built on
//!   top of Unit 17's poll loop.
//!
//! Module wiring only lives here; no operational logic.

pub mod auth;
mod error_map;
pub mod github_client;
pub mod labels;
pub mod poll;
