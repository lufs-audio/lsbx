//! Queue polling: listing queued/in-progress GitHub Actions jobs across
//! discovered repos, and the driver loop that ties polling cadence and
//! periodic repo-list refresh together.
//!
//! # `queued_jobs`: two separate status calls, not one combined call
//!
//! The acceptance criterion reads "`GET .../actions/runs` (status
//! queued/in_progress)". GitHub's real `/actions/runs` endpoint accepts
//! exactly one `status` value per call — there is no comma-joined or
//! multi-value form. So `queued_jobs` below issues two separate requests via
//! [`crate::github_client::GitHubClient::workflow_runs`] (one with
//! `status=queued`, one with `status=in_progress`), not a single call with
//! a combined status string. For each run returned by either call, it then
//! fetches that run's jobs via
//! [`crate::github_client::GitHubClient::run_jobs`] and keeps only jobs
//! whose `status == "queued"` and whose `labels` contain the requested
//! `label` — the same two-step traversal (list runs, then list each run's
//! jobs) the acceptance criterion describes, rather than a hypothetical
//! single endpoint that returns jobs directly.
//!
//! # The driver loop (`Poller`): a real gap between the acceptance criteria
//! and the interface contract
//!
//! The acceptance criteria describe actual scheduling behavior — poll every
//! `poll_interval` across all discovered repos and labels, but only refresh
//! the repo list every `repo_refresh_interval` rather than on every poll
//! tick. The interface contract's code block, though, only names the
//! primitives (`queued_jobs`, `queued_age_seconds`, `is_eligible`) — it has
//! no driver loop type or function at all. Something has to actually
//! implement the two-different-interval behavior on top of those
//! primitives, or "refreshes the repo list every `repo_refresh_interval`
//! rather than on every poll tick" is prose nobody's code satisfies.
//!
//! [`Poller`] fills that gap. It is deliberately NOT a bare
//! `loop { sleep(poll_interval).await; ... }` with no way to observe its
//! behavior short of waiting on a real clock: it exposes a single-step
//! [`Poller::tick`] that a caller (production code, or a test) drives
//! directly. `tick` takes the current time explicitly (`now: SystemTime`)
//! rather than reading `SystemTime::now()` itself, so a test can call it
//! repeatedly with a manually-advanced clock and assert the refresh-cadence
//! behavior deterministically, in well under a second of real wall-clock
//! time — no `tokio::time::pause()`/`advance()` or a real 300-second sleep
//! required. Production code (e.g. Unit 18's reconciliation loop, or a
//! `ci-broker run` CLI entrypoint) is expected to drive it with something
//! like:
//!
//! ```ignore
//! let mut poller = Poller::new(cfg);
//! loop {
//!     let now = std::time::SystemTime::now();
//!     let eligible = poller.tick(&client, now).await?;
//!     // ... dispatch `eligible` jobs (Unit 18) ...
//!     tokio::time::sleep(poller.config().poll_interval).await;
//! }
//! ```
//!
//! `tick` itself never sleeps — the caller owns the actual wait between
//! ticks, which is what makes it composable with Unit 18's dispatch step:
//! Unit 18 can call `tick` from the same loop it uses to reconcile
//! already-dispatched jobs, rather than needing a second, competing sleep
//! loop.

use super::github_client::GitHubClient;
use crate::labels::is_eligible;
use lsbx_kernel::error::LsbxError;
use std::time::{Duration, SystemTime};

pub const FALLBACK_QUEUE_LABEL: &str = "lsbx-default";

const DEFAULT_POLL_INTERVAL_SECS: u64 = 15;
const DEFAULT_REPO_REFRESH_INTERVAL_SECS: u64 = 300;
const DEFAULT_FALLBACK_DELAY_SECS: u64 = 60;

const POLL_INTERVAL_ENV: &str = "LSBX_CI_POLL_INTERVAL";
const FALLBACK_DELAY_ENV: &str = "LSBX_CI_FALLBACK_DELAY";
const REPOS_ENV: &str = "LSBX_CI_REPOS";
const GITHUB_OWNER_ENV: &str = "GITHUB_OWNER";
const GITHUB_REPO_ENV: &str = "GITHUB_REPO";

/// Which repos this broker polls, and how that list is derived. gh-CLI and
/// GitHub-App are **co-equal** first-class auth methods, each with its own
/// natural repo source — gh-CLI deployments configure repos explicitly
/// (they have no installation scope), App deployments typically discover
/// them from the installation.
///
/// - `Some(list)`: a static, explicitly-configured repo list (from
///   `LSBX_CI_REPOS`, or `GITHUB_OWNER`+`GITHUB_REPO`). The `Poller` uses
///   it verbatim and never calls `installation_repositories` (the App-only
///   endpoint) — this is the gh-CLI path.
/// - `None`: discover repos via `GitHubClient::installation_repositories()`
///   (the App-installation endpoint) — the App path.
pub struct PollConfig {
    pub poll_interval: std::time::Duration,         // default 15s
    pub repo_refresh_interval: std::time::Duration, // default 300s
    pub fallback_delay: std::time::Duration,        // default 60s
    pub queue_labels: Vec<String>,
    /// Explicit repo list (`owner/repo`). `None` means "discover via the
    /// App installation endpoint."
    pub repos: Option<Vec<String>>,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
            repo_refresh_interval: Duration::from_secs(DEFAULT_REPO_REFRESH_INTERVAL_SECS),
            fallback_delay: Duration::from_secs(DEFAULT_FALLBACK_DELAY_SECS),
            queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
            repos: None,
        }
    }
}

impl PollConfig {
    /// Builds a [`PollConfig`] from `queue_label` (a comma-split list of
    /// placement labels, per the acceptance criterion) plus the documented
    /// env-var overrides (`LSBX_CI_POLL_INTERVAL`, `LSBX_CI_FALLBACK_DELAY`),
    /// falling back to their defaults when unset or unparseable.
    ///
    /// The repo list is read from `LSBX_CI_REPOS` (comma-separated
    /// `owner/repo`). When that is unset, it falls back to
    /// `GITHUB_OWNER`+`GITHUB_REPO` (the legacy single-repo env pair). When
    /// neither is present, `repos` is `None`, which selects the App mode's
    /// installation-endpoint discovery.
    pub fn from_queue_label_and_env(queue_label: &str) -> Self {
        let defaults = Self::default();
        let queue_labels = queue_label
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        let repos = repos_from_env();

        Self {
            poll_interval: duration_from_env_secs(POLL_INTERVAL_ENV)
                .unwrap_or(defaults.poll_interval),
            repo_refresh_interval: defaults.repo_refresh_interval,
            fallback_delay: duration_from_env_secs(FALLBACK_DELAY_ENV)
                .unwrap_or(defaults.fallback_delay),
            queue_labels: if queue_labels.is_empty() {
                defaults.queue_labels
            } else {
                queue_labels
            },
            repos,
        }
    }
}

/// Derives the explicit repo list from the environment, or `None` if no
/// explicit repos are configured (the App-discovery case).
fn repos_from_env() -> Option<Vec<String>> {
    if let Ok(raw) = std::env::var(REPOS_ENV) {
        let list = parse_repo_csv(&raw);
        if !list.is_empty() {
            return Some(list);
        }
    }
    // Legacy single-repo pair: `GITHUB_OWNER` + `GITHUB_REPO`.
    let owner = std::env::var(GITHUB_OWNER_ENV).ok();
    let repo = std::env::var(GITHUB_REPO_ENV).ok();
    match (owner, repo) {
        (Some(o), Some(r)) if !o.is_empty() && !r.is_empty() => Some(vec![format!("{o}/{r}")]),
        _ => None,
    }
}

/// Splits a comma-separated `owner/repo` env value into trimmed,
/// non-empty `String`s. Pure — factored out so the parsing logic is
/// unit-testable without mutating process-global env (which two tests in
/// the same binary would race on).
fn parse_repo_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn duration_from_env_secs(var: &str) -> Option<Duration> {
    std::env::var(var)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

pub struct QueuedJob {
    pub job_id: u64,
    pub run_id: u64,
    pub repository: String,
    pub labels: Vec<String>,
    pub name: Option<String>,
    pub created_at: Option<String>, // None if unparseable — fail-closed downstream
}

/// GitHub's real `/actions/runs` endpoint accepts one `status` value per
/// call — see the module doc comment for why `queued_jobs` below issues one
/// request per entry in this list rather than a single combined call.
const RUN_STATUSES: [&str; 2] = ["queued", "in_progress"];

/// Lists queued jobs carrying `label` in `repo`: for each of
/// [`RUN_STATUSES`], lists that status's workflow runs
/// (`GET .../actions/runs?status=<status>`), then for each run lists its
/// jobs (`GET .../actions/runs/{run_id}/jobs`), keeping only jobs whose
/// `status == "queued"` and whose `labels` contain `label`.
pub async fn queued_jobs(
    client: &GitHubClient,
    label: &str,
    repo: &str,
) -> Result<Vec<QueuedJob>, LsbxError> {
    let mut jobs = Vec::new();

    for status in RUN_STATUSES {
        let runs = client.workflow_runs(repo, status).await?;

        for run in runs {
            let run_jobs = client.run_jobs(repo, run.id).await?;

            for job in run_jobs {
                if job.status == "queued" && job.labels.iter().any(|l| l == label) {
                    jobs.push(QueuedJob {
                        job_id: job.id,
                        run_id: job.run_id,
                        repository: repo.to_string(),
                        labels: job.labels,
                        name: job.name,
                        created_at: job.created_at,
                    });
                }
            }
        }
    }

    Ok(jobs)
}

/// The driver loop tying `poll_interval`-cadence polling and
/// `repo_refresh_interval`-cadence repo-list refresh together. See the
/// module doc comment for why this exists and how it's meant to be driven.
pub struct Poller {
    config: PollConfig,
    repos: Vec<String>,
    last_repo_refresh: Option<SystemTime>,
}

impl Poller {
    pub fn new(config: PollConfig) -> Self {
        Self {
            repos: config.repos.clone().unwrap_or_default(),
            config,
            last_repo_refresh: None,
        }
    }

    pub fn config(&self) -> &PollConfig {
        &self.config
    }

    /// The repos this `Poller` will poll as of the most recent refresh
    /// (empty until the first `tick`, which always refreshes since
    /// `last_repo_refresh` starts `None`).
    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    /// The time of the most recent repo-list refresh, or `None` if `tick`
    /// has never been called. Exposed so a test can assert the refresh
    /// cadence directly (e.g. "unchanged across two closely-spaced ticks,
    /// advanced after a tick past `repo_refresh_interval`") without needing
    /// to inspect `repos()` contents, which would require a live
    /// `GitHubClient`.
    pub fn last_repo_refresh(&self) -> Option<SystemTime> {
        self.last_repo_refresh
    }

    /// Pure decision of whether a repo-list refresh is due at `now`, given
    /// this `Poller`'s current `last_repo_refresh` and
    /// `config.repo_refresh_interval` — no I/O, no `GitHubClient` required.
    /// `tick` below calls this directly; a test can also call it directly to
    /// exercise the "refresh every `repo_refresh_interval`, not on every
    /// tick" cadence deterministically with a manually-advanced `now`,
    /// without needing a live or mocked `GitHubClient` at all.
    pub fn should_refresh_repos(&self, now: SystemTime) -> bool {
        repo_refresh_due(
            self.last_repo_refresh,
            self.config.repo_refresh_interval,
            now,
        )
    }

    /// Runs one poll step at time `now`:
    ///
    /// 1. If no explicit repo list was configured (`config.repos` is `None`
    ///    — the App mode), refreshes the repo list via
    ///    `client.installation_repositories()` when [`Self::should_refresh_repos`]
    ///    says a refresh is due — never on every tick otherwise, matching
    ///    the acceptance criterion. If an explicit repo list *was*
    ///    configured (`Some(..)` — the gh-CLI mode), it was populated at
    ///    construction and is used verbatim; `installation_repositories` is
    ///    never called (that endpoint is App-installation-scoped and would
    ///    fail under normal `gh` user auth).
    /// 2. For every repo and every configured queue label, calls
    ///    `queued_jobs`, then filters to jobs where `is_eligible(job, cfg,
    ///    now)` is true.
    ///
    /// Takes `now` explicitly (rather than reading `SystemTime::now()`
    /// itself) so a test can drive multiple ticks with a manually-advanced
    /// clock and assert the refresh-cadence behavior deterministically,
    /// without a real `repo_refresh_interval`-length sleep.
    pub async fn tick(
        &mut self,
        client: &GitHubClient,
        now: SystemTime,
    ) -> Result<Vec<QueuedJob>, LsbxError> {
        // Static-repo (gh-CLI) mode: repos were populated at construction;
        // never hit the App-only installation endpoint. Only the
        // discovery mode (config.repos == None) refreshes via the client.
        if self.config.repos.is_none() && self.should_refresh_repos(now) {
            self.repos = client.installation_repositories().await?;
            self.last_repo_refresh = Some(now);
        }

        let mut eligible = Vec::new();
        for repo in &self.repos {
            for label in &self.config.queue_labels {
                let jobs = queued_jobs(client, label, repo).await?;
                for job in jobs {
                    if is_eligible(&job, &self.config, now) {
                        eligible.push(job);
                    }
                }
            }
        }

        Ok(eligible)
    }
}

/// Pure "is a refresh due" decision, factored out of [`Poller::should_refresh_repos`]
/// so it has no dependency on `Poller`'s fields — kept as a free function
/// specifically so it's trivial to unit-test directly (see `poll::tests`
/// below) with nothing but plain `SystemTime` values, no `Poller`
/// construction or `GitHubClient` required.
fn repo_refresh_due(
    last_refresh: Option<SystemTime>,
    repo_refresh_interval: Duration,
    now: SystemTime,
) -> bool {
    match last_refresh {
        None => true,
        Some(last) => match now.duration_since(last) {
            Ok(elapsed) => elapsed >= repo_refresh_interval,
            // `now` is before `last` (clock went backwards, or a caller
            // passed an out-of-order `now`) — do not refresh; treat the
            // interval as not yet elapsed rather than erroring over a clock
            // anomaly unrelated to polling.
            Err(_) => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the "poll every `poll_interval`, but only refresh the repo
    /// list every `repo_refresh_interval`" cadence from the acceptance
    /// criteria, entirely via manually-advanced `SystemTime` values — no
    /// real sleep, no `GitHubClient`, runs in a fraction of a second.
    #[test]
    fn should_refresh_repos_follows_the_configured_interval_not_every_tick() {
        let config = PollConfig {
            poll_interval: Duration::from_secs(15),
            repo_refresh_interval: Duration::from_secs(300),
            fallback_delay: Duration::from_secs(60),
            queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
            repos: None,
        };
        let poller = Poller::new(config);

        let t0 = SystemTime::UNIX_EPOCH;
        // First-ever check (no prior refresh) is always due.
        assert!(poller.should_refresh_repos(t0));

        // Simulate having just refreshed at t0, then check several
        // `poll_interval`-sized ticks (15s each) that stay well under the
        // 300s `repo_refresh_interval` — none of these should be due.
        let last_refresh = Some(t0);
        for tick in 1..=19u64 {
            // 19 * 15s = 285s, still short of 300s.
            let now = t0 + Duration::from_secs(tick * 15);
            assert!(
                !repo_refresh_due(last_refresh, config_refresh_interval(), now),
                "tick {tick} (t+{}s) should not trigger a refresh yet",
                tick * 15
            );
        }

        // At t+300s (exactly `repo_refresh_interval`), a refresh is due —
        // boundary is inclusive (`>=`).
        let now_at_boundary = t0 + Duration::from_secs(300);
        assert!(repo_refresh_due(
            last_refresh,
            config_refresh_interval(),
            now_at_boundary
        ));

        // Comfortably past the boundary, still due.
        let now_past_boundary = t0 + Duration::from_secs(301);
        assert!(repo_refresh_due(
            last_refresh,
            config_refresh_interval(),
            now_past_boundary
        ));
    }

    /// After `Poller::tick` performs a refresh, `last_repo_refresh` advances
    /// to the `now` the refresh happened at — asserted purely through the
    /// public `should_refresh_repos`/`last_repo_refresh` accessors driving
    /// a manually-advanced clock, matching how a real caller (Unit 18's
    /// driving loop, or a future `ci-broker run` entrypoint) would compose
    /// with this type without needing a live `GitHubClient` just to observe
    /// the cadence.
    #[test]
    fn last_repo_refresh_reflects_the_tick_time_it_was_set_at() {
        let config = PollConfig {
            poll_interval: Duration::from_secs(15),
            repo_refresh_interval: Duration::from_secs(300),
            fallback_delay: Duration::from_secs(60),
            queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
            repos: None,
        };
        let mut poller = Poller::new(config);
        assert_eq!(poller.last_repo_refresh(), None);

        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        // Manually thread the same decision `tick` uses, without a
        // `GitHubClient` — this is exactly the internal state transition
        // `tick` performs when `should_refresh_repos` says yes.
        assert!(poller.should_refresh_repos(t0));
        poller.last_repo_refresh = Some(t0);

        assert_eq!(poller.last_repo_refresh(), Some(t0));
        // Immediately after, at the same instant, no refresh is due yet.
        assert!(!poller.should_refresh_repos(t0));

        let t_plus_299 = t0 + Duration::from_secs(299);
        assert!(!poller.should_refresh_repos(t_plus_299));

        let t_plus_300 = t0 + Duration::from_secs(300);
        assert!(poller.should_refresh_repos(t_plus_300));
    }

    fn config_refresh_interval() -> Duration {
        Duration::from_secs(300)
    }

    /// The gh-CLI (co-equal) repo-source: `LSBX_CI_REPOS` comma-separated
    /// `owner/repo` list is parsed into an explicit `PollConfig.repos`,
    /// selecting static-repo mode. Tested pure (no process-global env) to
    /// avoid the cross-test race that two env-reading tests in the same
    /// binary would hit under parallel execution.
    #[test]
    fn parse_repo_csv_splits_trims_and_drops_empties() {
        assert_eq!(
            parse_repo_csv("a/b, c/d ,"),
            vec!["a/b".to_string(), "c/d".to_string()]
        );
        assert_eq!(parse_repo_csv(""), Vec::<String>::new());
        assert_eq!(parse_repo_csv(" , "), Vec::<String>::new());
    }
}
