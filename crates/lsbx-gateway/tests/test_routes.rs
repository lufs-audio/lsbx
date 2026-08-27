//! Exercises the route table end-to-end against a real `LsbxOps` backed by
//! a real `DemoBackend` + tempfile-backed `SandboxStore`/`CiJobStore`,
//! matching Unit 10's own `tests/test_all_operations.rs` construction
//! pattern exactly — never a bare `Arc::new(LsbxOps)` unit value or a
//! zero-arg `LsbxOps::new()` (neither of which the real 6-parameter
//! constructor supports anyway).
//!
//! Uses `tower::ServiceExt::oneshot` to drive the router directly (no real
//! socket bind needed for these tests — `test_auth_fail_closed.rs` is
//! where an actual bind is exercised).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lsbx_backend_demo::DemoBackend;
use lsbx_gateway::routes::{build_router, GatewayConfig};
use lsbx_golden::registry::ImageRegistry;
use lsbx_kernel::clock::SystemClock;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

const TEST_TOKEN: &str = "test-token-abc123";

/// Builds a real `LsbxOps` exactly per Unit 10's own construction pattern:
/// a fresh `DemoBackend`, an isolated temp-dir-backed
/// `SandboxStore`/`CiJobStore`, an empty `ImageRegistry`, and (here) a real
/// `SystemClock` rather than a `FakeClock` — this test file doesn't need
/// to force lease expiry deterministically the way `lsbx-ops`'s own reap
/// tests do, so the real clock is the more honest default.
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

fn test_config() -> GatewayConfig {
    GatewayConfig {
        token: Some(TEST_TOKEN.to_string()),
        allow_local_files: false,
        insecure: false,
        max_sandboxes: 8,
        rate_limit: lsbx_gateway::RateLimitConfig {
            requests_per_minute: 6000,
            burst: 1000,
        },
    }
}

fn auth_header() -> (&'static str, String) {
    ("authorization", format!("Bearer {TEST_TOKEN}"))
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body should be valid JSON")
}

#[tokio::test]
async fn health_returns_real_status_report_shape() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());

    let (header_name, header_value) = auth_header();
    let request = Request::builder()
        .uri("/health")
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["backend_name"], "demo");
    assert_eq!(body["data"]["backend_available"], true);
    assert_eq!(body["data"]["sandbox_count"], 0);
}

#[tokio::test]
async fn health_without_credentials_is_unauthorized() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_and_list_sandboxes_round_trip_through_real_lsbxops() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());

    let (header_name, header_value) = auth_header();
    let create_body = serde_json::json!({
        "profile": "lsbx-default-v1",
        "name": "route-test-sandbox",
        "verify": false
    });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();

    let create_response = router.clone().oneshot(create_request).await.unwrap();
    assert_eq!(create_response.status(), StatusCode::OK);
    let created = body_json(create_response).await;
    assert_eq!(created["status"], "success");
    assert_eq!(created["data"]["name"], "route-test-sandbox");
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let list_request = Request::builder()
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();
    let list_response = router.clone().oneshot(list_request).await.unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = body_json(list_response).await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);

    let get_request = Request::builder()
        .uri(format!("/sandboxes/{sandbox_id}"))
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();
    let get_response = router.clone().oneshot(get_request).await.unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let fetched = body_json(get_response).await;
    assert_eq!(fetched["data"]["id"], sandbox_id);

    let delete_request = Request::builder()
        .method("DELETE")
        .uri(format!("/sandboxes/{sandbox_id}"))
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();
    let delete_response = router.clone().oneshot(delete_request).await.unwrap();
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

    let get_after_delete_request = Request::builder()
        .uri(format!("/sandboxes/{sandbox_id}"))
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();
    let get_after_delete_response = router.oneshot(get_after_delete_request).await.unwrap();
    assert_eq!(get_after_delete_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_unknown_sandbox_maps_not_found_to_404() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());

    let (header_name, header_value) = auth_header();
    let request = Request::builder()
        .uri("/sandboxes/sbx-does-not-exist")
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["status"], "error");
    // NotFound's real exit code (SPEC.md §6) is 4 — the envelope's `code`
    // must be that real exit code, not an invented HTTP-only taxonomy.
    assert_eq!(body["code"], 4);
}

#[tokio::test]
async fn exec_against_live_sandbox_returns_real_command_output_shape() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());
    let (header_name, header_value) = auth_header();

    let create_body =
        serde_json::json!({ "profile": "lsbx-default-v1", "name": "exec-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let exec_body = serde_json::json!({ "command": ["echo", "hi"] });
    let exec_request = Request::builder()
        .method("POST")
        .uri(format!("/sandboxes/{sandbox_id}/exec"))
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(exec_body.to_string()))
        .unwrap();
    let exec_response = router.oneshot(exec_request).await.unwrap();
    assert_eq!(exec_response.status(), StatusCode::OK);
    let exec_result = body_json(exec_response).await;
    assert_eq!(exec_result["data"]["exit_code"], 0);
}

#[tokio::test]
async fn exec_with_empty_command_array_is_rejected_as_usage() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());
    let (header_name, header_value) = auth_header();

    let create_body = serde_json::json!({ "profile": "lsbx-default-v1", "name": "exec-empty-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let exec_body = serde_json::json!({ "command": [] });
    let exec_request = Request::builder()
        .method("POST")
        .uri(format!("/sandboxes/{sandbox_id}/exec"))
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(exec_body.to_string()))
        .unwrap();
    let exec_response = router.oneshot(exec_request).await.unwrap();
    assert_eq!(exec_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn local_file_put_is_rejected_when_allow_local_files_is_disabled() {
    let (ops, _dir) = build_test_ops();
    let config = test_config(); // allow_local_files: false by default
    let router = build_router(ops.clone(), config);
    let (header_name, header_value) = auth_header();

    let create_body = serde_json::json!({ "profile": "lsbx-default-v1", "name": "put-gate-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let put_body = serde_json::json!({ "source": "/etc/hostname", "destination": "/tmp/x" });
    let put_request = Request::builder()
        .method("POST")
        .uri(format!("/sandboxes/{sandbox_id}/put"))
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(put_body.to_string()))
        .unwrap();
    let put_response = router.oneshot(put_request).await.unwrap();
    assert_eq!(put_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn local_file_put_succeeds_when_allow_local_files_is_enabled() {
    let (ops, dir) = build_test_ops();
    let mut config = test_config();
    config.allow_local_files = true;
    let router = build_router(ops.clone(), config);
    let (header_name, header_value) = auth_header();

    let create_body = serde_json::json!({ "profile": "lsbx-default-v1", "name": "put-enabled-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let source_path = dir.path().join("local-put-source.txt");
    std::fs::write(&source_path, b"hello from local file put test").unwrap();

    let put_body = serde_json::json!({
        "source": source_path.to_string_lossy(),
        "destination": "/tmp/uploaded-via-local-put.txt"
    });
    let put_request = Request::builder()
        .method("POST")
        .uri(format!("/sandboxes/{sandbox_id}/put"))
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(put_body.to_string()))
        .unwrap();
    let put_response = router.oneshot(put_request).await.unwrap();
    assert_eq!(put_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn upload_stages_http_body_and_calls_real_put() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops.clone(), test_config());
    let (header_name, header_value) = auth_header();

    let create_body = serde_json::json!({ "profile": "lsbx-default-v1", "name": "upload-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let upload_request = Request::builder()
        .method("POST")
        .uri(format!(
            "/sandboxes/{sandbox_id}/upload?destination=/tmp/via-http-upload.txt"
        ))
        .header(header_name, &header_value)
        .body(Body::from("uploaded bytes over http"))
        .unwrap();
    let upload_response = router.oneshot(upload_request).await.unwrap();
    assert_eq!(upload_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn console_by_id_returns_real_console_url_for_novnc_sandbox() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops.clone(), test_config());
    let (header_name, header_value) = auth_header();

    let create_body = serde_json::json!({ "profile": "lsbx-default-v1", "name": "console-target", "verify": false });
    let create_request = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();
    let created = body_json(router.clone().oneshot(create_request).await.unwrap()).await;
    let sandbox_id = created["data"]["id"].as_str().unwrap().to_string();

    let console_request = Request::builder()
        .uri(format!("/consoles/{sandbox_id}"))
        .header(header_name, &header_value)
        .body(Body::empty())
        .unwrap();
    let console_response = router.oneshot(console_request).await.unwrap();
    assert_eq!(console_response.status(), StatusCode::OK);
    let console_body = body_json(console_response).await;
    // DemoBackend always returns an https_url, so the resulting record's
    // streaming is "novnc" and console_url must be present.
    assert!(!console_body["data"]["console_url"].is_null());
}

#[tokio::test]
async fn browser_console_route_requires_no_authentication() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops.clone(), test_config());

    // No Authorization/X-Api-Key header at all — GET /console is the sole
    // unauthenticated route per this unit's acceptance criteria.
    let request = Request::builder()
        .uri("/console?target=sbx-does-not-exist")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    // Unauthenticated access must not be rejected with 401 — the sandbox
    // itself doesn't exist so this is a 404, not an auth failure, and the
    // important assertion is that it is NOT StatusCode::UNAUTHORIZED.
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_backed_routes_return_real_registry_shape() {
    let (ops, _dir) = build_test_ops();
    let router = build_router(ops, test_config());
    let (header_name, header_value) = auth_header();

    for path in ["/images", "/profiles", "/capabilities"] {
        let request = Request::builder()
            .uri(path)
            .header(header_name, &header_value)
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "route {path} should return 200"
        );
    }
}

#[tokio::test]
async fn create_respects_configured_sandbox_limit() {
    let (ops, _dir) = build_test_ops();
    let mut config = test_config();
    config.max_sandboxes = 1;
    let router = build_router(ops, config);
    let (header_name, header_value) = auth_header();

    let body = serde_json::json!({ "profile": "lsbx-default-v1", "verify": false });
    let first = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let first_response = router.clone().oneshot(first).await.unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second = Request::builder()
        .method("POST")
        .uri("/sandboxes")
        .header(header_name, &header_value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let second_response = router.oneshot(second).await.unwrap();
    assert_eq!(second_response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
