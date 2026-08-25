//! Verifies no GitHub App private key or token value is ever logged,
//! including at `--verbose` (`TRACE`-level tracing).
//!
//! Per the acceptance criterion's literal wording ("a test scans log output
//! for a known-fixture key/token substring and asserts it is absent"), this
//! sets up a real `tracing_subscriber` writer that captures actual emitted
//! log output across a full `jwt()` + `installation_token()` call (not just
//! a manual `format!("{:?}", ...)` of the config struct), then scans that
//! captured output.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's
//! established house convention for test code — see
//! `test_jwt_claims.rs`'s doc comment for the full rationale.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use lsbx_broker::auth::{GitHubAppAuth, GitHubAppConfig};
use tracing_subscriber::fmt::MakeWriter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE_PRIVATE_KEY_PEM: &str = include_str!("fixture_key.pem");
const FIXTURE_TOKEN: &str = "ghs_fixture_installation_token_do_not_log_me";

/// A `MakeWriter` that appends every write into a shared in-memory buffer, so
/// the test can inspect exactly what `tracing` emitted.
#[derive(Clone)]
struct CapturingWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.lock().expect("capture buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn future_rfc3339(seconds_from_now: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds_from_now)).to_rfc3339()
}

/// A fully-populated `octocrab::models::Installation` JSON body — see
/// `test_token_caching.rs`'s identical helper for why this needs every
/// `Author`/`Installation` field `octocrab` 0.43.0 requires, not just
/// "enough for serde to not error".
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
async fn no_secret_leakage_across_jwt_and_installation_token_calls() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = CapturingWriter { buffer: buffer.clone() };

    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();

    // `set_default` (as opposed to `with_default`, which takes a sync
    // closure) returns a guard that can be held across `.await` points —
    // this test is already running inside the `#[tokio::test]` runtime, so
    // there is no need to spin up a second, nested one just to drive the
    // async calls under test while the subscriber is active. The guard is
    // dropped (restoring whatever subscriber was active before) at the end
    // of this fn, scoping the capture to this test only.
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let result = run_calls_under_test().await;
    result.expect("jwt() and installation_token() should both succeed against the mock server");

    let captured = buffer.lock().expect("capture buffer lock");
    let log_text = String::from_utf8_lossy(&captured);

    assert!(
        !log_text.contains(FIXTURE_PRIVATE_KEY_PEM.trim()),
        "log output must never contain the private key PEM body"
    );
    assert!(
        !log_text.contains("BEGIN RSA PRIVATE KEY"),
        "log output must never contain a PEM header, which would indicate key material nearby"
    );
    assert!(
        !log_text.contains(FIXTURE_TOKEN),
        "log output must never contain the installation token value"
    );
}

async fn run_calls_under_test() -> Result<(), Box<dyn std::error::Error>> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/orgs/lufs-audio/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installation_response_body(777)))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/app/installations/777/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": FIXTURE_TOKEN,
            "expires_at": future_rfc3339(3600),
            "permissions": {}
        })))
        .mount(&mock_server)
        .await;

    let auth = GitHubAppAuth::new_with_base_uri(
        GitHubAppConfig {
            app_id: 99,
            private_key_pem: FIXTURE_PRIVATE_KEY_PEM.to_string(),
            installation_id: None,
        },
        &mock_server.uri(),
    )?;

    tracing::trace!(app_id = 99, "about to sign a GitHub App JWT (fixture key must never appear in output)");
    let _jwt = auth.jwt()?;
    tracing::debug!("signed GitHub App JWT");

    tracing::trace!("about to exchange JWT for an installation access token");
    let _token = auth.installation_token("lufs-audio").await?;
    tracing::debug!("exchanged installation access token");

    Ok(())
}
