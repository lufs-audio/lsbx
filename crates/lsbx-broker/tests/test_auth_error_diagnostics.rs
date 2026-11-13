//! Tests for #44: an auth failure must be diagnosable from **one log line**.
//!
//! #44 reported `403 Forbidden — Resource not accessible by integration` on
//! two specific repos (`podcast-pipeline`, `kb`) while a freshly-minted token
//! worked fine on all eleven. The old error string carried only the route, so
//! the failing repo was visible only by reading the route text, and *nothing*
//! said which token had failed or what that token could see — the two facts
//! that separate the competing explanations.
//!
//! These tests pin the two halves of the fix:
//!
//! 1. an `AuthFailed` is enriched with the failing repo, the token's age, and
//!    the number of repos that token can see;
//! 2. **nothing else is touched** — a rate-limit is a retryable
//!    `BackendUnavailable`, not an auth problem, and rewriting it would erase
//!    a distinction `error_map` draws deliberately.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's
//! established house convention for test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use lsbx_broker::auth::{GitHubAppAuth, GitHubAppConfig};
use lsbx_broker::github_client::GitHubClient;
use lsbx_kernel::error::LsbxError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE_PRIVATE_KEY_PEM: &str = include_str!("fixture_key.pem");
const OWNER: &str = "lufs-audio";

fn future_rfc3339(seconds_from_now: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds_from_now)).to_rfc3339()
}

fn installation_response_body(installation_id: u64) -> serde_json::Value {
    serde_json::json!({
        "id": installation_id,
        "account": {
            "login": "lufs-audio",
            "id": 1,
            "node_id": "O_kgDOAAAAAQ",
            "avatar_url": "https://avatars.githubusercontent.com/u/1?v=4",
            "gravatar_id": "",
            "url": "https://api.github.com/users/lufs-audio",
            "html_url": "https://github.com/lufs-audio",
            "followers_url": "https://api.github.com/users/lufs-audio/followers",
            "following_url": "https://api.github.com/users/lufs-audio/following{/other_user}",
            "gists_url": "https://api.github.com/users/lufs-audio/gists{/gist_id}",
            "starred_url": "https://api.github.com/users/lufs-audio/starred{/owner}{/repo}",
            "subscriptions_url": "https://api.github.com/users/lufs-audio/subscriptions",
            "organizations_url": "https://api.github.com/users/lufs-audio/orgs",
            "repos_url": "https://api.github.com/users/lufs-audio/repos",
            "events_url": "https://api.github.com/users/lufs-audio/events{/privacy}",
            "received_events_url": "https://api.github.com/users/lufs-audio/received_events",
            "type": "Organization",
            "site_admin": false
        },
        "permissions": {},
        "events": []
    })
}

/// One `models::Repository` entry, with only the fields `octocrab` requires
/// non-optional. Everything else is `Option`, so the entry stays small and
/// readable rather than being a 60-key blob nobody will maintain.
fn repository_entry(id: u64, name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "full_name": format!("lufs-audio/{name}"),
        "url": format!("https://api.github.com/repos/lufs-audio/{name}")
    })
}

/// Mounts everything a working App-auth broker needs, plus a token the mock
/// reports as able to see exactly two repos.
async fn mount_common(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/orgs/lufs-audio/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installation_response_body(555)))
        .named("discover installation id")
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/app/installations/555/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": "ghs_scoped",
            "expires_at": future_rfc3339(3600),
            "permissions": {}
        })))
        .named("exchange jwt for installation token")
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_count": 2,
            "repositories": [repository_entry(1, "kb"), repository_entry(2, "albumdujour")]
        })))
        .named("list installation repositories")
        .mount(server)
        .await;
}

async fn build_client(server: &MockServer) -> GitHubClient {
    let auth = GitHubAppAuth::new_with_base_uri(
        GitHubAppConfig {
            app_id: 42,
            private_key_pem: FIXTURE_PRIVATE_KEY_PEM.to_string(),
            installation_id: None,
        },
        &server.uri(),
    )
    .expect(
        "GitHubAppAuth::new_with_base_uri should succeed with the fixture key and mock base_uri",
    );

    GitHubClient::from_app_auth(Arc::new(auth), OWNER)
        .await
        .expect("from_app_auth should mint successfully against the mock server")
}

#[tokio::test]
async fn auth_failure_names_the_repo_the_token_age_and_the_tokens_scope() {
    let server = MockServer::start().await;
    mount_common(&server).await;

    // The #44 symptom, reproduced: the token is valid, but GitHub says this
    // repo is not accessible to it.
    Mock::given(method("GET"))
        .and(path("/repos/lufs-audio/kb/actions/runs"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "message": "Resource not accessible by integration"
        })))
        .named("403 on kb")
        .mount(&server)
        .await;

    let client = build_client(&server).await;

    // Discovery first — this is what teaches the client what the token can
    // see, and it is also what the App-mode poll loop does when repo
    // discovery is enabled.
    let repos = client
        .installation_repositories()
        .await
        .expect("repo discovery should succeed against the mock server");
    assert_eq!(repos.len(), 2, "fixture should expose exactly two repos");

    let err = match client.workflow_runs("lufs-audio/kb", "queued").await {
        Ok(runs) => panic!(
            "a 403 must surface as an error, not a silently empty result (got {} runs)",
            runs.len()
        ),
        Err(err) => err,
    };

    let LsbxError::AuthFailed(message) = err else {
        panic!(
            "a 403 'Resource not accessible by integration' must map to AuthFailed, got {err:?}"
        );
    };

    assert!(
        message.contains("repo=lufs-audio/kb"),
        "the failing repo must be named explicitly, not only implied by the route: {message}"
    );
    assert!(
        message.contains("token_sees=2 repos"),
        "the failing token's scope must be reported, so a scope problem is distinguishable \
         from an expiry problem without another API call: {message}"
    );
    assert!(
        message.contains("token_age="),
        "token age must be reported — it is what separates a stale-token 401 from a \
         genuine insufficient-scope 403: {message}"
    );
}

#[tokio::test]
async fn rate_limit_stays_retryable_and_is_not_rewritten_as_an_auth_error() {
    let server = MockServer::start().await;
    mount_common(&server).await;

    // GitHub returns 403 for rate limiting too. `error_map` deliberately maps
    // that to the retryable `BackendUnavailable`, and the #44 diagnostics must
    // not flatten the two cases together.
    Mock::given(method("GET"))
        .and(path("/repos/lufs-audio/albumdujour/actions/runs"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "message": "API rate limit exceeded for installation ID 148555887"
        })))
        .named("403 rate limit")
        .mount(&server)
        .await;

    let client = build_client(&server).await;

    let err = match client
        .workflow_runs("lufs-audio/albumdujour", "queued")
        .await
    {
        Ok(runs) => panic!(
            "a rate limit must surface as an error, not a silently empty result (got {} runs)",
            runs.len()
        ),
        Err(err) => err,
    };

    let LsbxError::BackendUnavailable(message) = err else {
        panic!("a rate-limited 403 must stay BackendUnavailable (retryable), got {err:?}");
    };
    assert!(
        !message.contains("repo="),
        "a rate limit is not an auth problem and must not be enriched as one: {message}"
    );
}
