//! Exercises the token-bucket rate limiter's `429`/`Retry-After` behavior
//! against the real route table (built via `build_router` against a real
//! `LsbxOps`, same construction pattern as `test_routes.rs`), not the
//! `TokenBucket` type in isolation (that unit-level coverage lives in
//! `ratelimit.rs`'s own `#[cfg(test)]` module).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use lsbx_backend_demo::DemoBackend;
use lsbx_golden::registry::ImageRegistry;
use lsbx_gateway::routes::{build_router, GatewayConfig};
use lsbx_gateway::RateLimitConfig;
use lsbx_kernel::clock::SystemClock;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

const TEST_TOKEN: &str = "rate-limit-test-token";

fn build_test_ops() -> (Arc<LsbxOps>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(dir.path().to_path_buf());
    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![],
        profiles: HashMap::new(),
    };
    let ops = LsbxOps::new(
        Box::new(DemoBackend::new()),
        "demo".to_string(),
        sandbox_store,
        ci_job_store,
        registry,
        Box::new(SystemClock),
    );
    (Arc::new(ops), dir)
}

fn tightly_limited_config() -> GatewayConfig {
    GatewayConfig {
        token: Some(TEST_TOKEN.to_string()),
        allow_local_files: false,
        insecure: false,
        // A burst of 2 means the 3rd request within the same key in quick
        // succession must be denied — small enough to exhaust
        // deterministically within a single test without any sleeping.
        rate_limit: RateLimitConfig {
            requests_per_minute: 60,
            burst: 2,
        },
    }
}

fn health_request(token: &str) -> Request<Body> {
    Request::builder()
        .uri("/health")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn exhausting_burst_returns_429_with_retry_after_header() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, tightly_limited_config());

    // First two requests (burst: 2) must be allowed through to the real
    // handler (200, since the token is valid and /health always succeeds
    // against a healthy DemoBackend).
    let first = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    // Third request within the same burst window must be denied with 429
    // and a Retry-After header, per this unit's acceptance criteria.
    let third = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = third
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .expect("429 response must carry a Retry-After header");
    let retry_after_secs: u64 = retry_after
        .to_str()
        .expect("Retry-After should be ASCII")
        .parse()
        .expect("Retry-After should be a whole-second integer");
    assert!(retry_after_secs >= 1);
}

#[tokio::test]
async fn rate_limit_is_keyed_per_token_not_shared_globally() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, tightly_limited_config());

    // Exhaust token A's burst of 2.
    let _ = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    let _ = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    let exhausted = router.clone().oneshot(health_request(TEST_TOKEN)).await.unwrap();
    assert_eq!(exhausted.status(), StatusCode::TOO_MANY_REQUESTS);

    // A request presenting a *different* token must not be affected by
    // token A's exhaustion — the limiter is keyed per-token (this unit's
    // acceptance criteria: "keyed by bearer token"), not globally. This
    // second token happens to be invalid against the configured token, so
    // the real observable outcome is 401 (auth rejects it) rather than
    // 429 (rate limiter would have denied it) — the distinction between
    // those two status codes is exactly what proves the two callers have
    // independent rate-limit buckets: if they shared one bucket, this
    // would also come back 429, masking the real auth failure underneath.
    let different_key_request = Request::builder()
        .uri("/health")
        .header("authorization", "Bearer a-completely-different-token")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(different_key_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unauthenticated_console_route_is_rate_limited_by_source_ip() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, tightly_limited_config());

    // GET /console is the sole unauthenticated route; with no
    // Authorization/X-Api-Key header at all the rate limiter must fall
    // back to keying by source IP (this unit's acceptance criteria).
    // `axum::extract::ConnectInfo` needs a real connection to populate a
    // socket address, so this exercises it via `into_make_service_with_connect_info`
    // rather than `Router::oneshot` (which has no peer address to report) —
    // `tower::ServiceExt::oneshot` alone can't supply ConnectInfo, so this
    // test binds an ephemeral real listener instead of using oneshot,
    // matching how `test_auth_fail_closed.rs` already exercises a real
    // bind elsewhere in this crate's own test suite.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let make_service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let server_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, make_service).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let statuses = fetch_console_route_three_times(addr).await;
    server_handle.abort();

    // First two requests (burst: 2) succeed at the transport level (some
    // real status, since the sandbox in `target=` doesn't exist this will
    // be 404, not 429); the third, from the same source IP, must be 429.
    assert_eq!(statuses.len(), 3);
    assert_ne!(statuses[0], 429);
    assert_ne!(statuses[1], 429);
    assert_eq!(statuses[2], 429);
}

async fn fetch_console_route_three_times(addr: std::net::SocketAddr) -> Vec<u16> {
    let mut statuses = Vec::new();
    for _ in 0..3 {
        if let Some(status) = fetch_status(addr, "/console?target=sbx-none").await {
            statuses.push(status);
        }
    }
    statuses
}

/// Same minimal dependency-free HTTP GET helper used in
/// `test_auth_fail_closed.rs`, duplicated here rather than shared via a
/// `tests/common/` module — each integration test file in this crate is
/// its own compilation unit, and neither existing helper module
/// convention nor this unit's own interface contract calls for factoring
/// two ~15-line helpers into shared test infrastructure.
async fn fetch_status(addr: std::net::SocketAddr, path: &str) -> Option<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.ok()?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.ok()?;

    let response_text = String::from_utf8_lossy(&response);
    let status_line = response_text.lines().next()?;
    status_line.split_whitespace().nth(1)?.parse().ok()
}
