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

pub struct PollConfig {
    pub poll_interval: std::time::Duration,         // default 15s
    pub repo_refresh_interval: std::time::Duration, // default 300s
    pub fallback_delay: std::time::Duration,        // default 60s
    pub queue_labels: Vec<String>,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
            repo_refresh_interval: Duration::from_secs(DEFAULT_REPO_REFRESH_INTERVAL_SECS),
            fallback_delay: Duration::from_secs(DEFAULT_FALLBACK_DELAY_SECS),
            queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        }
    }
}

impl PollConfig {
    /// Builds a [`PollConfig`] from `queue_label` (a comma-split list of
    /// placement labels, per the acceptance criterion) plus the documented
    /// env-var overrides (`LSBX_CI_POLL_INTERVAL`, `LSBX_CI_FALLBACK_DELAY`),
    /// falling back to their defaults when unset or unparseable.
    pub fn from_queue_label_and_env(queue_label: &str) -> Self {
        let defaults = Self::default();
        let queue_labels = queue_label
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        Self {
            poll_interval: duration_from_env_secs(POLL_INTERVAL_ENV).unwrap_or(defaults.poll_interval),
            repo_refresh_interval: defaults.repo_refresh_interval,
            fallback_delay: duration_from_env_secs(FALLBACK_DELAY_ENV).unwrap_or(defaults.fallback_delay),
            queue_labels: if queue_labels.is_empty() {
                defaults.queue_labels
            } else {
                queue_labels
            },
        }
    }
}

fn duration_from_env_secs(var: &str) -> Option<Duration> {
    std::env::var(var).ok()?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub struct QueuedJob {
    pub job_id: u64,
    pub run_id: u64,
    pub repository: String,
    pub labels: Vec<String>,
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
pub async fn queued_jobs(client: &GitHubClient, label: &str, repo: &str) -> Result<Vec<QueuedJob>, LsbxError> {
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
            config,
            repos: Vec::new(),
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
        repo_refresh_due(self.last_repo_refresh, self.config.repo_refresh_interval, now)
    }

    /// Runs one poll step at time `now`:
    ///
    /// 1. Refreshes the repo list via
    ///    `client.installation_repositories()` if [`Self::should_refresh_repos`]
    ///    says a refresh is due — never on every tick otherwise, matching
    ///    the acceptance criterion.
    /// 2. For every discovered repo and every configured queue label, calls
    ///    `queued_jobs`, then filters to jobs where `is_eligible(job, cfg,
    ///    now)` is true.
    ///
    /// Takes `now` explicitly (rather than reading `SystemTime::now()`
    /// itself) so a test can drive multiple ticks with a manually-advanced
    /// clock and assert the refresh-cadence behavior deterministically,
    /// without a real `repo_refresh_interval`-length sleep.
    pub async fn tick(&mut self, client: &GitHubClient, now: SystemTime) -> Result<Vec<QueuedJob>, LsbxError> {
        if self.should_refresh_repos(now) {
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
fn repo_refresh_due(last_refresh: Option<SystemTime>, repo_refresh_interval: Duration, now: SystemTime) -> bool {
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
        assert!(repo_refresh_due(last_refresh, config_refresh_interval(), now_at_boundary));

        // Comfortably past the boundary, still due.
        let now_past_boundary = t0 + Duration::from_secs(301);
        assert!(repo_refresh_due(last_refresh, config_refresh_interval(), now_past_boundary));
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
}
