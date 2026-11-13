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

pub(crate) fn map_gh_cli_error(
    status: std::process::ExitStatus,
    stderr: &str,
    path: &str,
) -> LsbxError {
    let lower = stderr.to_ascii_lowercase();

    // API rate limits (HTTP 403 with "rate limit") are transient and retryable.
    if lower.contains("rate limit") {
        return LsbxError::BackendUnavailable(format!(
            "`gh api {path}` failed ({status}): {stderr}"
        ));
    }

    // Explicit 401 or 403 indicates authentication or authorization rejection.
    if lower.contains("401")
        || lower.contains("bad credentials")
        || lower.contains("403")
        || lower.contains("forbidden")
        || lower.contains("resource not accessible")
    {
        return LsbxError::AuthFailed(format!("`gh api {path}` failed ({status}): {stderr}"));
    }

    if lower.contains("404") || lower.contains("not found") {
        return LsbxError::NotFound(format!("`gh api {path}` failed ({status}): {stderr}"));
    }

    // All other errors: 5xx, timeouts, connection drops, DNS issues -> BackendUnavailable (retryable).
    LsbxError::BackendUnavailable(format!("`gh api {path}` failed ({status}): {stderr}"))
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

    #[test]
    fn gh_cli_transient_network_errors_map_to_backend_unavailable() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(256); // exit code 1
        #[cfg(not(unix))]
        let status = std::process::Command::new("false").status().unwrap();

        // 502 server error
        let err502 = map_gh_cli_error(status, "gh: Server Error (HTTP 502)", "/test");
        assert!(matches!(err502, LsbxError::BackendUnavailable(_)));

        // TLS handshake timeout
        let err_tls = map_gh_cli_error(
            status,
            r#"Get "https://api.github.com/repos/...": net/http: TLS handshake timeout"#,
            "/test",
        );
        assert!(matches!(err_tls, LsbxError::BackendUnavailable(_)));

        // Connection error
        let err_conn = map_gh_cli_error(
            status,
            "error connecting to api.github.com\ncheck your internet connection",
            "/test",
        );
        assert!(matches!(err_conn, LsbxError::BackendUnavailable(_)));

        // Rate limit 403
        let err_rate = map_gh_cli_error(
            status,
            "gh: API rate limit exceeded for user (HTTP 403)",
            "/test",
        );
        assert!(matches!(err_rate, LsbxError::BackendUnavailable(_)));

        // Bad credentials 401
        let err_auth = map_gh_cli_error(status, "gh: Bad credentials (HTTP 401)", "/test");
        assert!(matches!(err_auth, LsbxError::AuthFailed(_)));

        // Forbidden 403
        let err_forbid = map_gh_cli_error(
            status,
            "gh: Resource not accessible by integration (HTTP 403)",
            "/test",
        );
        assert!(matches!(err_forbid, LsbxError::AuthFailed(_)));
    }
}
