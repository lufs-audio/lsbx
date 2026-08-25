//! One shared mapping from `octocrab::Error` onto `lsbx_kernel::error::LsbxError`'s
//! existing seven-variant taxonomy, used by both `auth.rs` and
//! `github_client.rs` so the mapping logic exists in exactly one place.
//!
//! - HTTP 401/403 (GitHub rejected our credentials) -> [`LsbxError::AuthFailed`].
//! - Any other GitHub-side error status, or a transport/connection failure
//!   (GitHub unreachable, timed out, 5xx) -> [`LsbxError::BackendUnavailable`].
//! - A response body that doesn't deserialize into the type we expected ->
//!   [`LsbxError::ContractViolated`].

use lsbx_kernel::error::LsbxError;

pub(crate) fn map_octocrab_error(err: octocrab::Error, route: &str) -> LsbxError {
    match &err {
        octocrab::Error::GitHub { source, .. } => {
            let status = source.status_code;
            if status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN {
                LsbxError::AuthFailed(format!(
                    "GitHub rejected credentials for {route}: {status} {}",
                    source.message
                ))
            } else {
                LsbxError::BackendUnavailable(format!(
                    "GitHub API error for {route}: {status} {}",
                    source.message
                ))
            }
        }
        octocrab::Error::Http { .. } | octocrab::Error::Hyper { .. } | octocrab::Error::Service { .. } => {
            LsbxError::BackendUnavailable(format!("GitHub API unreachable for {route}: {err}"))
        }
        octocrab::Error::Json { .. } | octocrab::Error::Serde { .. } | octocrab::Error::SerdeUrlEncoded { .. } => {
            LsbxError::ContractViolated(format!("unexpected response shape from {route}: {err}"))
        }
        _ => LsbxError::BackendUnavailable(format!("GitHub API call to {route} failed: {err}")),
    }
}
