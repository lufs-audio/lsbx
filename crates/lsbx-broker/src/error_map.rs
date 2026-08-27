//! One shared mapping from `octocrab::Error` onto `lsbx_kernel::error::LsbxError`'s
//! existing seven-variant taxonomy, used by both `auth.rs` and
//! `github_client.rs` so the mapping logic exists in exactly one place.
//!
//! - HTTP 401/403 (GitHub rejected our credentials) -> [`LsbxError::AuthFailed`].
//! - A GitHub 403 specifically caused by API rate limiting is retryable and
//!   maps to [`LsbxError::BackendUnavailable`].
//! - Any other GitHub-side error status, or a transport/connection failure
//!   (GitHub unreachable, timed out, 5xx) -> [`LsbxError::BackendUnavailable`].
//! - A response body that doesn't deserialize into the type we expected ->
//!   [`LsbxError::ContractViolated`].

use lsbx_kernel::error::LsbxError;

fn map_github_http_error(status: http::StatusCode, message: &str, route: &str) -> LsbxError {
    if status == http::StatusCode::FORBIDDEN && message.to_ascii_lowercase().contains("rate limit")
    {
        return LsbxError::BackendUnavailable(format!(
            "GitHub API rate limited for {route}: {status} {message}"
        ));
    }

    if status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN {
        LsbxError::AuthFailed(format!(
            "GitHub rejected credentials for {route}: {status} {message}"
        ))
    } else {
        LsbxError::BackendUnavailable(format!("GitHub API error for {route}: {status} {message}"))
    }
}

pub(crate) fn map_octocrab_error(err: octocrab::Error, route: &str) -> LsbxError {
    match &err {
        octocrab::Error::GitHub { source, .. } => {
            map_github_http_error(source.status_code, &source.message, route)
        }
        octocrab::Error::Http { .. }
        | octocrab::Error::Hyper { .. }
        | octocrab::Error::Service { .. } => {
            LsbxError::BackendUnavailable(format!("GitHub API unreachable for {route}: {err}"))
        }
        octocrab::Error::Json { .. }
        | octocrab::Error::Serde { .. }
        | octocrab::Error::SerdeUrlEncoded { .. } => {
            LsbxError::ContractViolated(format!("unexpected response shape from {route}: {err}"))
        }
        _ => LsbxError::BackendUnavailable(format!("GitHub API call to {route} failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_rate_limit_is_retryable_not_auth_failure() {
        let error = map_github_http_error(
            http::StatusCode::FORBIDDEN,
            "API rate limit exceeded for installation ID",
            "/repos/example/project/actions/runs",
        );
        assert!(matches!(error, LsbxError::BackendUnavailable(_)));
    }

    #[test]
    fn ordinary_forbidden_remains_auth_failure() {
        let error = map_github_http_error(
            http::StatusCode::FORBIDDEN,
            "Resource not accessible by integration",
            "/repos/example/project/actions/runs",
        );
        assert!(matches!(error, LsbxError::AuthFailed(_)));
    }
}
