//! Scenario (per the unit contract): `installation_token()` is called twice
//! within the refresh window against a mocked GitHub API, and the mock's
//! token-exchange endpoint (`POST /app/installations/{id}/access_tokens`) is
//! asserted to have been hit exactly once.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's
//! established house convention for test code — see
//! `test_jwt_claims.rs`'s doc comment for the full rationale.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_broker::auth::{GitHubAppAuth, GitHubAppConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE_PRIVATE_KEY_PEM: &str = include_str!("fixture_key.pem");

fn future_rfc3339(seconds_from_now: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds_from_now)).to_rfc3339()
}

/// A fully-populated `octocrab::models::Installation` JSON body (the real
/// response shape `GET /orgs/{owner}/installation` returns), matching every
/// field `octocrab` 0.43.0's `Author`/`Installation` structs require —
/// deliberately more complete than "just enough for serde to not error",
/// since this is the exact typed-deserialization path the contract calls
/// for.
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

#[tokio::test]
async fn installation_token_is_cached_and_exchange_endpoint_hit_exactly_once() {
    let mock_server = MockServer::start().await;

    // GET /orgs/{owner}/installation -> discovers installation id 555.
    Mock::given(method("GET"))
        .and(path("/orgs/lufs-audio/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installation_response_body(555)))
        .expect(1)
        .named("discover installation id")
        .mount(&mock_server)
        .await;

    // POST /app/installations/555/access_tokens -> issues a token valid well
    // beyond the 300s refresh margin, so a second call within the window
    // should be served from cache.
    Mock::given(method("POST"))
        .and(path("/app/installations/555/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": "ghs_fixture_installation_token",
            "expires_at": future_rfc3339(3600),
            "permissions": {}
        })))
        .expect(1)
        .named("exchange jwt for installation token")
        .mount(&mock_server)
        .await;

    let auth = GitHubAppAuth::new_with_base_uri(
        GitHubAppConfig {
            app_id: 42,
            private_key_pem: FIXTURE_PRIVATE_KEY_PEM.to_string(),
            installation_id: None, // force discovery through the mocked endpoint
        },
        &mock_server.uri(),
    )
    .expect("GitHubAppAuth::new_with_base_uri should succeed with a valid fixture key and mock base_uri");

    let first = auth
        .installation_token("lufs-audio")
        .await
        .expect("first installation_token() call should succeed against the mock server");
    let second = auth
        .installation_token("lufs-audio")
        .await
        .expect("second installation_token() call should succeed (from cache)");

    assert_eq!(first, "ghs_fixture_installation_token");
    assert_eq!(
        first, second,
        "a cached, still-valid installation token must not be re-exchanged on a second call"
    );

    // `Mock::expect(1)` on both mounted mocks is itself verified when
    // `mock_server` drops at the end of the test, but assert explicitly too
    // so a failure here points directly at "exchange endpoint hit more than
    // once" rather than an opaque drop-time panic.
    let received = mock_server.received_requests().await.expect("mock server should report received requests");
    let exchange_hits = received
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/app/installations/555/access_tokens")
        .count();
    assert_eq!(exchange_hits, 1, "token-exchange endpoint must be hit exactly once");

    let discovery_hits = received
        .iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.path() == "/orgs/lufs-audio/installation")
        .count();
    assert_eq!(discovery_hits, 1, "installation discovery must also be cached after the first call");
}
