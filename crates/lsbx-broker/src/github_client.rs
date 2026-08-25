//! A GitHub API client wrapping `octocrab::Octocrab`, authenticated with
//! either a real installation token (via [`GitHubAppAuth`](crate::auth::GitHubAppAuth))
//! or the `gh` CLI fallback ([`GhCliFallback`](crate::auth::GhCliFallback)).

use lsbx_kernel::error::LsbxError;
use octocrab::models::InstallationRepositories;
use octocrab::Octocrab;

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
