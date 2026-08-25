//! `lsbx-broker` — the zero-idle CI runner broker.
//!
//! This crate is built up across three units that land in order (16 -> 17 ->
//! 18), sharing this one crate but owning disjoint files within it:
//!
//! - Unit 16 (this unit): `auth` + `github_client` — GitHub App RS256 JWT
//!   signing, installation-token exchange/caching, and org-wide repo
//!   discovery.
//! - Unit 17: `poll` + `labels` — queue polling and placement-label matching,
//!   built on top of this unit's `GitHubClient`.
//! - Unit 18: `reconcile` + `job_record` — job<->VM reconciliation, built on
//!   top of Unit 17's poll loop.
//!
//! Module wiring only lives here; no operational logic.

pub mod auth;
mod error_map;
pub mod github_client;
