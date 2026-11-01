//! Regression tests for #43: the broker's installation token must be
//! **re-minted** as it approaches expiry, not captured once at process start.
//!
//! # Why this file exists alongside `test_token_caching.rs`
//!
//! `test_token_caching.rs` exercises `GitHubAppAuth::installation_token()`
//! directly — and it passed for the entire life of this bug. The refresh
//! logic was never broken; what was broken is that nothing *above* it ever
//! called it a second time. `GitHubClient` minted one token in
//! `from_app_auth` and held that `Octocrab` for the life of the process, so
//! every call after the token's 60-minute expiry sent a dead token, got a
//! `401`, and — because `AuthFailed` is fatal by design — killed the broker
//! (1116 restarts, zero dispatches, on molimo).
//!
//! So these tests assert the invariant at the `GitHubClient` boundary, which
//! is where the defect actually lived. A test one layer down could not have
//! caught it.
//!
//! # Two assertions per scenario, deliberately
//!
//! The exchange-endpoint hit count proves a token was *minted*. The
//! `Authorization` header on the request that follows proves the client
//! actually *used* it. Only the second one fails if the client is not
//! rebuilt — a re-mint nobody consumes would still leave the broker sending
//! a dead token, which is precisely the shape of the original bug.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's
//! established house convention for test code — see `test_jwt_claims.rs`'s
//! doc comment for the full rationale.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lsbx_broker::auth::{GitHubAppAuth, GitHubAppConfig};
use lsbx_broker::github_client::GitHubClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const FIXTURE_PRIVATE_KEY_PEM: &str = include_str!("fixture_key.pem");
const OWNER: &str = "lufs-audio";
const REPO: &str = "lufs-audio/albumdujour";
const RUNS_PATH: &str = "/repos/lufs-audio/albumdujour/actions/runs";
const TOKEN_EXCHANGE_PATH: &str = "/app/installations/555/access_tokens";

fn future_rfc3339(seconds_from_now: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds_from_now)).to_rfc3339()
}

/// A fully-populated `octocrab::models::Installation` JSON body — the real
/// response shape `GET /orgs/{owner}/installation` returns. Same fixture the
/// sibling `test_token_caching.rs` uses, for the same reason: this is the
/// typed-deserialization path the contract calls for, not a hand-waved stub.
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

/// Issues a *distinct* token per mint, each with a caller-chosen lifetime.
///
/// A fixed response body would make the second scenario untestable: the whole
/// question there is whether the client ends up authenticating with a
/// *different* token, which requires the two mints to differ.
struct MintSequence {
    /// `(token, lifetime_seconds_from_now)`, consumed in order; the last entry
    /// is reused if the endpoint is hit more times than there are entries (so
    /// an unexpected extra mint shows up as a hit-count failure with a clear
    /// message rather than an opaque panic).
    mints: Vec<(&'static str, i64)>,
    served: AtomicUsize,
}

impl Respond for MintSequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let index = self.served.fetch_add(1, Ordering::SeqCst);
        let (token, lifetime) = self.mints.get(index).copied().unwrap_or_else(|| {
            *self
                .mints
                .last()
                .expect("MintSequence needs at least one mint")
        });

        ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": token,
            "expires_at": future_rfc3339(lifetime),
            "permissions": {}
        }))
    }
}

async fn mount_installation_discovery(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/orgs/lufs-audio/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installation_response_body(555)))
        .named("discover installation id")
        .mount(server)
        .await;
}

async fn mount_workflow_runs(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(RUNS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_count": 0,
            "workflow_runs": []
        })))
        .named("list workflow runs")
        .mount(server)
        .await;
}

async fn mount_token_exchange(server: &MockServer, mints: Vec<(&'static str, i64)>) {
    Mock::given(method("POST"))
        .and(path(TOKEN_EXCHANGE_PATH))
        .respond_with(MintSequence {
            mints,
            served: AtomicUsize::new(0),
        })
        .named("exchange jwt for installation token")
        .mount(server)
        .await;
}

/// Builds a real `GitHubClient` over App auth pointed at `server`.
///
/// `installation_id: None` on purpose, so installation discovery is exercised
/// through the mock too.
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

fn hits(requests: &[Request], method_name: &str, path_str: &str) -> usize {
    requests
        .iter()
        .filter(|r| r.method.as_str() == method_name && r.url.path() == path_str)
        .count()
}

/// The `Authorization` header of every request to `path_str`, in the order the
/// mock server received them.
fn auth_headers_for(requests: &[Request], path_str: &str) -> Vec<String> {
    requests
        .iter()
        .filter(|r| r.url.path() == path_str)
        .map(|r| {
            r.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<no authorization header>")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn healthy_token_is_reused_and_client_is_not_rebuilt() {
    let server = MockServer::start().await;
    mount_installation_discovery(&server).await;
    mount_workflow_runs(&server).await;
    // 3600s of life against a 300s refresh margin: healthy throughout.
    mount_token_exchange(&server, vec![("ghs_healthy", 3600)]).await;

    let client = build_client(&server).await;

    for _ in 0..3 {
        client
            .workflow_runs(REPO, "queued")
            .await
            .expect("workflow_runs should succeed against the mock server");
    }

    let requests = server
        .received_requests()
        .await
        .expect("mock server should report received requests");

    assert_eq!(
        hits(&requests, "POST", TOKEN_EXCHANGE_PATH),
        1,
        "a healthy installation token must be exchanged exactly once across many calls"
    );
    assert_eq!(
        hits(&requests, "GET", "/orgs/lufs-audio/installation"),
        1,
        "installation discovery must also stay cached"
    );

    let auths = auth_headers_for(&requests, RUNS_PATH);
    assert_eq!(auths.len(), 3, "all three calls should have reached GitHub");
    assert!(
        auths.iter().all(|a| a == "Bearer ghs_healthy"),
        "every call must authenticate with the cached token: {auths:?}"
    );
}

#[tokio::test]
async fn token_crossing_the_refresh_margin_is_reminted_and_client_rebuilt() {
    let server = MockServer::start().await;
    mount_installation_discovery(&server).await;
    mount_workflow_runs(&server).await;
    // 302 seconds of life against the 300s refresh margin: healthy for the
    // first call, and inside the margin a few seconds later — a real straddle
    // of the expiry boundary rather than an assumed one.
    mount_token_exchange(&server, vec![("ghs_mint_0", 302), ("ghs_mint_1", 3600)]).await;

    let client = build_client(&server).await;

    client
        .workflow_runs(REPO, "queued")
        .await
        .expect("the pre-expiry call should succeed");

    // Let the 2s of headroom lapse so the next call is inside the margin.
    tokio::time::sleep(Duration::from_secs(3)).await;

    client
        .workflow_runs(REPO, "queued")
        .await
        .expect("the post-expiry call should succeed on a re-minted token");

    let requests = server
        .received_requests()
        .await
        .expect("mock server should report received requests");

    assert_eq!(
        hits(&requests, "POST", TOKEN_EXCHANGE_PATH),
        2,
        "crossing the refresh margin must trigger exactly one re-mint"
    );

    let auths = auth_headers_for(&requests, RUNS_PATH);
    assert_eq!(auths.len(), 2, "both calls should have reached GitHub");
    assert_eq!(
        auths[0], "Bearer ghs_mint_0",
        "the pre-expiry call authenticates with the first token"
    );
    assert_eq!(
        auths[1], "Bearer ghs_mint_1",
        "the post-expiry call must authenticate with the RE-MINTED token — this is the \
         assertion that fails if the client is not rebuilt, which is the #43 defect"
    );
}
