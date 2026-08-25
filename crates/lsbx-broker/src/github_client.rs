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

    /// Builds a client that falls back to the `gh` CLI, for local dev/testing
    /// without full GitHub App credentials.
    pub fn from_gh_cli_fallback() -> Self {
        Self {
            backing: Backing::GhCli(GhCliFallback),
        }
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
}

/// Builds the single `Octocrab` client construction path for a real
/// installation access token. Centralized so there is exactly one place that
/// constructs this builder (rather than repeating the same
/// `Octocrab::builder()...build()` block at each call site).
fn installation_token_client(token: String) -> Result<Octocrab, LsbxError> {
    Octocrab::builder()
        .personal_token(token)
        .build()
        .map_err(|e| LsbxError::AuthFailed(format!("failed to build installation-token client: {e}")))
}

const PER_PAGE: u8 = 100;

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
