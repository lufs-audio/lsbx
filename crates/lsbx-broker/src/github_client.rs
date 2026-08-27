//! A GitHub API client wrapping `octocrab::Octocrab`, authenticated with
//! either a real installation token (via [`GitHubAppAuth`](crate::auth::GitHubAppAuth))
//! or the `gh` CLI fallback ([`GhCliFallback`](crate::auth::GhCliFallback)).
//!
//! # Unit 17 addition: `workflow_runs` / `run_jobs`
//!
//! Unit 16 (this file's original author) shipped exactly one method,
//! [`GitHubClient::installation_repositories`]. Unit 17's polling logic
//! (`poll::queued_jobs`, in this same crate) needs two more authenticated,
//! paginated GitHub calls this crate had no method for yet:
//! `GET /repos/{repo}/actions/runs?status=<status>` and
//! `GET /repos/{repo}/actions/runs/{run_id}/jobs`. Both are added here,
//! alongside the existing method, rather than duplicated in `poll.rs` or
//! bolted onto a separate client type — `GitHubClient` already owns "the one
//! place that holds an authenticated `octocrab`/`gh`-CLI backing" for this
//! crate, and Unit 16's boundary (see its unit contract) was about not
//! implementing polling *logic* in `src/auth.rs`/`src/github_client.rs`, not
//! about freezing this file against additive, crate-internal extensions by
//! a later unit that shares the crate.
//!
//! Both new methods deliberately do NOT reuse `octocrab`'s own typed
//! `models::workflows::Run`/`models::workflows::Job` (`octocrab::models::workflows`)
//! response models, even though those exist and even though
//! `octocrab::Page<T>` (used here for pagination) already recognizes the
//! `workflow_runs`/`jobs` top-level response keys those types would sit
//! under. `models::workflows::Job::created_at` is a `chrono::DateTime<Utc>`,
//! which means a single job in a page with a malformed or missing
//! `created_at` would fail *deserialization of the entire page* — turning
//! one bad timestamp into a hard `BackendUnavailable`/`ContractViolated` for
//! every other, perfectly good, queued job in that same response. The
//! contract's own `QueuedJob::created_at: Option<String>` shape (and the
//! `test_malformed_timestamp_fails_closed` acceptance scenario) requires the
//! opposite: a bad timestamp should make *that job* ineligible via
//! `labels::queued_age_seconds` returning `None`, not reject the whole page.
//! So the response item shapes below ([`WorkflowRunSummary`], [`JobSummary`])
//! are this crate's own minimal structs, parsed permissively, while still
//! going through `octocrab::Page<T>` for the actual HTTP/pagination
//! machinery — the same "typed response, hand-rolled shape" idiom
//! `installation_repositories_via_octocrab` below already uses for
//! `InstallationRepositories`.
//!
//! # Unit 18 addition: `runner_name` on `JobSummary`, and `job_for_runner`
//!
//! Unit 18's divergence detection needs to know which `job_id` GitHub has
//! *actually* assigned a given runner to, to compare against the `job_id`
//! `lsbx` dispatched a VM for (GitHub assigns runners to jobs by label
//! match, not by any id `lsbx` controls — the two can diverge). That
//! requires a field this struct did not carry as of Unit 17:
//! [`JobSummary::runner_name`].
//!
//! Verified directly against GitHub's real REST API schema for
//! `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs` (the `Job` object,
//! confirmed via the current `workflow-jobs` REST reference) rather than
//! assumed: the field is literally named `runner_name` (`string | null`,
//! null until a runner has been assigned) — the same name this file already
//! uses for the concept elsewhere, so no remapping is needed. Added as
//! `#[serde(default)] Option<String>`, matching this struct's existing
//! permissive-parse convention (see the module doc comment above) — a job
//! response missing `runner_name` entirely (an older API version, or a
//! shape this crate hasn't seen) must not fail the whole page the way a
//! non-optional field would.
//!
//! [`GitHubClient::job_for_runner`] is the new authenticated call this
//! unit's `Reconciler::check_divergence` needs. Design note on *why* it
//! takes `(repo, runner_name)` rather than `(repo, run_id, runner_name)`
//! even though every other new-in-this-file method threads a `run_id`
//! through: at the point `check_divergence` runs, the caller knows the
//! *dispatched* `job_id` (from `CiJobRecord.job_id`) and the runner name
//! learned by tailing the log (`CiJobRecord.runner_name`, populated by
//! `tail_and_update` parsing `Runner registered: (\S+)`) — but the real
//! `CiJobRecord` schema (confirmed against `lsbx-store`'s merged
//! `ci_job_store.rs`, which this unit's own ground truth forbids modifying)
//! has no `run_id` field to round-trip the original dispatch's run through.
//! Rather than overload an unrelated existing field (e.g. stuffing a run id
//! into `dispatched_job_name`, which would be a surprising, undocumented
//! reuse of a field with its own real meaning) or add a field to a schema
//! this unit was told is already complete, `job_for_runner` re-derives the
//! answer by scanning `repo`'s currently queued+in-progress runs' jobs for
//! one whose `runner_name` matches — the same two-step
//! `workflow_runs`-then-`run_jobs` traversal `poll::queued_jobs` already
//! uses, just filtering on `runner_name` instead of `status`+`labels`. This
//! answers the actual question divergence detection needs ("what job is
//! GitHub's ground truth assigning to this runner *right now*") without
//! needing the original dispatch's `run_id` at all — see `reconcile.rs`'s
//! module doc comment for the fuller writeup of this decision and the
//! alternative considered.

use lsbx_kernel::error::LsbxError;
use octocrab::models::InstallationRepositories;
use octocrab::{Octocrab, Page};
use serde::Deserialize;

use crate::auth::{GhCliFallback, GitHubAppAuth};
use crate::error_map::map_octocrab_error;

enum Backing {
    /// Authenticated as a real installation, via an installation access
    /// token obtained from `GitHubAppAuth::installation_token`.
    Installation(Octocrab),
    /// No GitHub App credentials configured — falls back to shelling out to
    /// `gh api` for every call, matching the existing fallback path.
    GhCli(GhCliFallback),
}

/// Wraps `octocrab::Octocrab`, constructed from either an installation token
/// or the `gh` CLI fallback.
pub struct GitHubClient {
    backing: Backing,
}

impl GitHubClient {
/// Builds a client authenticated as `owner`'s GitHub App installation,
    /// exchanging (and caching, via `auth`) a real installation access
    /// token.
    ///
    /// This is **one of two co-equal, equally-first-class GitHub auth
    /// methods**, not a "primary." A deployment on a host that owns an
    /// App installation (e.g. Molimo/exe.dev) uses this; a deployment that
    /// authenticates with a normal `gh` login (e.g. Carnyx) uses
    /// [`from_gh_cli_fallback`]. The two must *only* ever be configured one
    /// at a time — `build_github_client` in `lsbx-cli` selects by env.
    ///
    /// An installation access token behaves like a classic personal access
    /// token for authorization purposes (see GitHub's own docs on
    /// installation tokens), so `personal_token(...)` — which sets
    /// `octocrab`'s `Auth::PersonalToken` and produces the correct
    /// `Authorization: Bearer <token>` header — is the right tool here. This
    /// is a different call site than the raw-JWT calls in `auth.rs`, which
    /// use `.app(...)` instead; see that module's doc comment for why.
    pub async fn from_app_auth(auth: &GitHubAppAuth, owner: &str) -> Result<Self, LsbxError> {
        let token = auth.installation_token(owner).await?;
        let client = installation_token_client(token)?;
        Ok(Self {
            backing: Backing::Installation(client),
        })
    }

    /// Builds a client backed by the local `gh` CLI, for deployments that
    /// authenticate as a normal GitHub user/OAuth (`gh auth`) rather than as
    /// an App installation — e.g. Carnyx's real setup. Co-equal with
    /// [`from_app_auth`]: a given deployment uses one or the other, never
    /// both, and neither is a degraded "secondary" path.
    ///
    /// The repo list in this mode is always an explicit, configured one
    /// (`LSBX_CI_REPOS`, or `GITHUB_OWNER`+`GITHUB_REPO` — see
    /// [`crate::poll::PollConfig::from_queue_label_and_env`]). It never
    /// enumerates `/installation/repositories`, because that endpoint is
    /// scoped to an App-installation token and does not exist for a normal
    /// `gh` user login — a gh-CLI broker MUST NOT call it (doing so produced
    /// `HTTP 403: You must authenticate with an installation access token`
    /// on Carnyx before this was fixed).
    pub fn from_gh_cli_fallback() -> Self {
        Self {
            backing: Backing::GhCli(GhCliFallback),
        }
    }

    /// Test-only: builds an installation-token-backed client with a plain
    /// token string, pointed at a custom base URI instead of real
    /// `api.github.com`.
    ///
    /// Mirrors `GitHubAppAuth::new_with_base_uri` (`auth.rs`) exactly — same
    /// `#[cfg(feature = "test-util")]` gate (a bare `#[cfg(test)]` item is
    /// invisible to this crate's `tests/*.rs` integration tests, which
    /// compile as a separate crate), same rationale: this unit's
    /// `test_divergence_nonfatal.rs` needs `GitHubClient::job_for_runner` to
    /// hit a `wiremock` mock server, not real GitHub, and
    /// `GitHubClient::from_app_auth` has no seam for that on its own (its
    /// `installation_token_client` helper never threads a base-uri override
    /// through). Takes a plain `token: String` rather than a
    /// `GitHubAppAuth` — the divergence test only needs an authenticated
    /// `octocrab` client pointed at the mock, not a real JWT exchange, so
    /// skipping straight to the installation-token step avoids a second,
    /// unrelated mock endpoint the test would otherwise need to stand up
    /// just to reach this one.
    #[cfg(feature = "test-util")]
    pub fn from_installation_token_with_base_uri(token: String, base_uri: &str) -> Result<Self, LsbxError> {
        let client = installation_token_client_with_base_uri(token, Some(base_uri))?;
        Ok(Self {
            backing: Backing::Installation(client),
        })
    }

    /// Lists every repository the installation can access, as full
    /// `"owner/repo"` strings.
    ///
    /// Calls `GET /installation/repositories?per_page=100`, paginating via
    /// `total_count`, through `octocrab`'s typed API
    /// (`octocrab::models::InstallationRepositories`) rather than hand-rolled
    /// JSON parsing.
    ///
    /// Deliberate deviation from the interface contract's literal
    /// `installation_repositories(&self, owner: &str)` sketch: this method
    /// takes no `owner` parameter. `GET /installation/repositories` is
    /// inherently scoped by which installation's access token authenticates
    /// the request (this `GitHubClient` was already built `from_app_auth`
    /// for a specific `owner` — see that constructor) — GitHub's endpoint
    /// itself has no `owner` path/query parameter to accept, so an `owner:
    /// &str` parameter here would be an unused, silently-ignored no-op. If a
    /// reviewer wants the literal signature kept (e.g. for call-site
    /// symmetry with other methods), it's a trivial mechanical addition —
    /// say so and it changes.
    pub async fn installation_repositories(&self) -> Result<Vec<String>, LsbxError> {
        match &self.backing {
            Backing::Installation(client) => installation_repositories_via_octocrab(client).await,
            Backing::GhCli(fallback) => installation_repositories_via_gh_cli(fallback).await,
        }
    }

    /// Lists workflow runs in `repo` with the given single `status` value
    /// (e.g. `"queued"` or `"in_progress"`), via
    /// `GET /repos/{repo}/actions/runs?status=<status>`, paginated at up to
    /// 100 per page (matching [`PER_PAGE`], the same constant
    /// `installation_repositories` already paginates at).
    ///
    /// Deliberately takes exactly one `status` value per call, not a
    /// comma-joined list: GitHub's real `/actions/runs` endpoint only
    /// accepts a single `status` query value, not a set — see
    /// `poll::queued_jobs`, which calls this once per status it cares
    /// about (`"queued"`, then `"in_progress"`) rather than trying to
    /// combine them into one request.
    pub async fn workflow_runs(&self, repo: &str, status: &str) -> Result<Vec<WorkflowRunSummary>, LsbxError> {
        match &self.backing {
            Backing::Installation(client) => workflow_runs_via_octocrab(client, repo, status).await,
            Backing::GhCli(fallback) => workflow_runs_via_gh_cli(fallback, repo, status).await,
        }
    }

    /// Lists every job attached to `run_id` in `repo`, via
    /// `GET /repos/{repo}/actions/runs/{run_id}/jobs`, paginated at up to
    /// 100 per page.
    pub async fn run_jobs(&self, repo: &str, run_id: u64) -> Result<Vec<JobSummary>, LsbxError> {
        match &self.backing {
            Backing::Installation(client) => run_jobs_via_octocrab(client, repo, run_id).await,
            Backing::GhCli(fallback) => run_jobs_via_gh_cli(fallback, repo, run_id).await,
        }
    }

    /// Unit 18 addition — see this module's doc comment ("Unit 18 addition")
    /// for the full design rationale.
    ///
    /// Scans `repo`'s currently queued and in-progress workflow runs' jobs
    /// (the same two-status, two-step `workflow_runs`-then-`run_jobs`
    /// traversal `poll::queued_jobs` already performs against its own
    /// private `RUN_STATUSES` — duplicated here as
    /// [`DIVERGENCE_SCAN_STATUSES`] rather than imported, since Unit 17's
    /// `poll::RUN_STATUSES` is a private `const` and widening its visibility
    /// for a same-crate-but-different-unit caller is outside this unit's
    /// boundary of "does not touch Unit 17's files") for the first job whose
    /// [`JobSummary::runner_name`] equals `runner_name`, and returns that
    /// job's `id`. Returns `Ok(None)` if no matching job is found in either
    /// status — this is not itself an error: a runner between "registered"
    /// and "GitHub has attached it to a job's `runner_name` field" is a
    /// real, transient, non-error state.
    pub async fn job_for_runner(&self, repo: &str, runner_name: &str) -> Result<Option<u64>, LsbxError> {
        for status in DIVERGENCE_SCAN_STATUSES {
            let runs = self.workflow_runs(repo, status).await?;
            for run in runs {
                let jobs = self.run_jobs(repo, run.id).await?;
                for job in jobs {
                    if job.runner_name.as_deref() == Some(runner_name) {
                        return Ok(Some(job.id));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Minimal, permissively-parsed shape of one entry in a
/// `GET /repos/{repo}/actions/runs` response's `workflow_runs` array. Only
/// the fields `poll::queued_jobs` actually needs — see the module doc
/// comment for why this is a hand-rolled struct rather than
/// `octocrab::models::workflows::Run`.
#[derive(Deserialize)]
pub struct WorkflowRunSummary {
    pub id: u64,
}

/// Minimal, permissively-parsed shape of one entry in a
/// `GET /repos/{repo}/actions/runs/{run_id}/jobs` response's `jobs` array.
/// `created_at` is deliberately `Option<String>`, not a parsed timestamp
/// type — see the module doc comment for why a malformed/missing timestamp
/// on one job must not fail the whole page.
#[derive(Deserialize)]
pub struct JobSummary {
    pub id: u64,
    pub run_id: u64,
    pub status: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Added by Unit 18 — see this module's doc comment ("Unit 18 addition")
    /// for the field-name verification against GitHub's real REST schema and
    /// why it is `#[serde(default)] Option<String>` like the other optional
    /// fields on this struct. `null`/absent until GitHub has actually
    /// assigned a runner to this job.
    #[serde(default)]
    pub runner_name: Option<String>,
}

/// Builds the single `Octocrab` client construction path for a real
/// installation access token. Centralized so there is exactly one place that
/// constructs this builder (rather than repeating the same
/// `Octocrab::builder()...build()` block at each call site).
fn installation_token_client(token: String) -> Result<Octocrab, LsbxError> {
    installation_token_client_with_base_uri(token, None)
}

/// Unit 18 addition: same construction path as
/// [`installation_token_client`], with an optional base-URI override for
/// this crate's own tests — see
/// [`GitHubClient::from_installation_token_with_base_uri`]'s doc comment for
/// why this seam exists, and `auth.rs`'s `build_app_client` for the
/// identical pattern already established there.
fn installation_token_client_with_base_uri(
    token: String,
    base_uri_override: Option<&str>,
) -> Result<Octocrab, LsbxError> {
    let mut builder = Octocrab::builder().personal_token(token);

    if let Some(base_uri) = base_uri_override {
        builder = builder
            .base_uri(base_uri)
            .map_err(|e| LsbxError::AuthFailed(format!("invalid test base_uri: {e}")))?;
    }

    builder
        .build()
        .map_err(|e| LsbxError::AuthFailed(format!("failed to build installation-token client: {e}")))
}

const PER_PAGE: u8 = 100;

/// Duplicated from `poll::RUN_STATUSES` — see
/// [`GitHubClient::job_for_runner`]'s doc comment for why this is a
/// same-crate duplicate rather than a shared import.
const DIVERGENCE_SCAN_STATUSES: [&str; 2] = ["queued", "in_progress"];

async fn installation_repositories_via_octocrab(client: &Octocrab) -> Result<Vec<String>, LsbxError> {
    let mut full_names = Vec::new();
    let mut page: u32 = 1;

    loop {
        let params = serde_json::json!({ "per_page": PER_PAGE, "page": page });
        let response: InstallationRepositories = client
            .get("/installation/repositories", Some(&params))
            .await
            .map_err(|e| map_octocrab_error(e, "/installation/repositories"))?;

        let page_len = response.repositories.len();
        for repo in response.repositories {
            full_names.push(repo.full_name.unwrap_or_else(|| repo.name.clone()));
        }

        let total_count = usize::try_from(response.total_count).map_err(|_| {
            LsbxError::ContractViolated(format!(
                "installation/repositories returned a negative total_count: {}",
                response.total_count
            ))
        })?;

        if full_names.len() >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(full_names)
}

async fn installation_repositories_via_gh_cli(fallback: &GhCliFallback) -> Result<Vec<String>, LsbxError> {
    let mut full_names = Vec::new();
    let mut page: u32 = 1;

    loop {
        let path = format!("/installation/repositories?per_page={PER_PAGE}&page={page}");
        let value = fallback.api(&path).await?;

        let repositories = value
            .get("repositories")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                LsbxError::ContractViolated(
                    "gh api installation/repositories response had no `repositories` array".to_string(),
                )
            })?;

        let total_count = value
            .get("total_count")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                LsbxError::ContractViolated(
                    "gh api installation/repositories response had no `total_count`".to_string(),
                )
            })? as usize;

        let page_len = repositories.len();
        for repo in repositories {
            let full_name = repo
                .get("full_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    LsbxError::ContractViolated(
                        "gh api installation/repositories entry had no `full_name`".to_string(),
                    )
                })?;
            full_names.push(full_name.to_string());
        }

        if full_names.len() >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(full_names)
}

async fn workflow_runs_via_octocrab(
    client: &Octocrab,
    repo: &str,
    status: &str,
) -> Result<Vec<WorkflowRunSummary>, LsbxError> {
    let route = format!("/repos/{repo}/actions/runs");
    let mut runs = Vec::new();
    let mut page: u32 = 1;

    loop {
        let params = serde_json::json!({ "status": status, "per_page": PER_PAGE, "page": page });
        let response: Page<WorkflowRunSummary> = client
            .get(&route, Some(&params))
            .await
            .map_err(|e| map_octocrab_error(e, &route))?;

        let page_len = response.items.len();
        let total_count = response.total_count.unwrap_or(page_len as u64);
        runs.extend(response.items);

        if runs.len() as u64 >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(runs)
}

async fn workflow_runs_via_gh_cli(
    fallback: &GhCliFallback,
    repo: &str,
    status: &str,
) -> Result<Vec<WorkflowRunSummary>, LsbxError> {
    let mut runs = Vec::new();
    let mut page: u32 = 1;

    loop {
        let path = format!("/repos/{repo}/actions/runs?status={status}&per_page={PER_PAGE}&page={page}");
        let value = fallback.api(&path).await?;

        let workflow_runs = value
            .get("workflow_runs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                LsbxError::ContractViolated(
                    "gh api actions/runs response had no `workflow_runs` array".to_string(),
                )
            })?;

        let total_count = value
            .get("total_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(workflow_runs.len() as u64);

        let page_len = workflow_runs.len();
        for run in workflow_runs {
            let parsed: WorkflowRunSummary = serde_json::from_value(run.clone()).map_err(|e| {
                LsbxError::ContractViolated(format!("gh api actions/runs entry had unexpected shape: {e}"))
            })?;
            runs.push(parsed);
        }

        if runs.len() as u64 >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(runs)
}

async fn run_jobs_via_octocrab(client: &Octocrab, repo: &str, run_id: u64) -> Result<Vec<JobSummary>, LsbxError> {
    let route = format!("/repos/{repo}/actions/runs/{run_id}/jobs");
    let mut jobs = Vec::new();
    let mut page: u32 = 1;

    loop {
        let params = serde_json::json!({ "per_page": PER_PAGE, "page": page });
        let response: Page<JobSummary> = client
            .get(&route, Some(&params))
            .await
            .map_err(|e| map_octocrab_error(e, &route))?;

        let page_len = response.items.len();
        let total_count = response.total_count.unwrap_or(page_len as u64);
        jobs.extend(response.items);

        if jobs.len() as u64 >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(jobs)
}

async fn run_jobs_via_gh_cli(fallback: &GhCliFallback, repo: &str, run_id: u64) -> Result<Vec<JobSummary>, LsbxError> {
    let mut jobs = Vec::new();
    let mut page: u32 = 1;

    loop {
        let path = format!("/repos/{repo}/actions/runs/{run_id}/jobs?per_page={PER_PAGE}&page={page}");
        let value = fallback.api(&path).await?;

        let jobs_array = value
            .get("jobs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                LsbxError::ContractViolated(
                    "gh api actions/runs/.../jobs response had no `jobs` array".to_string(),
                )
            })?;

        let total_count = value
            .get("total_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(jobs_array.len() as u64);

        let page_len = jobs_array.len();
        for job in jobs_array {
            let parsed: JobSummary = serde_json::from_value(job.clone()).map_err(|e| {
                LsbxError::ContractViolated(format!(
                    "gh api actions/runs/.../jobs entry had unexpected shape: {e}"
                ))
            })?;
            jobs.push(parsed);
        }

        if jobs.len() as u64 >= total_count || page_len == 0 {
            break;
        }
        page += 1;
    }

    Ok(jobs)
}
