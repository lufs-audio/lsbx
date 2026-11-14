//! Console door end-to-end: a real merged gateway serves the vendored noVNC
//! bundle on its own origin, mints a short-lived sandbox-scoped ticket, and
//! accepts that ticket on the WebSocket upgrade (issue #55).
//!
//! The point of this file is the two properties that used to be impossible:
//!   1. **No guest address anywhere.** The page a browser loads must not mention
//!      `192.168.x`, because that address is unreachable from anywhere but the
//!      hypervisor host — the bug that made `/console` a dead iframe.
//!   2. **A browser can actually authenticate the socket.** It cannot set an
//!      `Authorization` header on a WebSocket handshake, so the ticket path is
//!      the only way the authenticated relay is reachable from a page at all.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lsbx_backend_demo::DemoBackend;
use lsbx_gateway::{GatewayDeps, GatewayConfig};
use lsbx_golden::registry::ImageRegistry;
use lsbx_kernel::clock::SystemClock;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use tower::ServiceExt;

const TOKEN: &str = "test-gateway-token";

fn build_router(state_dir: &std::path::Path) -> Router {
    let ops = LsbxOps::new(
        Box::new(DemoBackend::new()),
        "demo".to_string(),
        SandboxStore::new(state_dir.to_path_buf()),
        CiJobStore::new(state_dir.to_path_buf()),
        ImageRegistry {
            images: vec![],
            goldens: vec![],
            profiles: std::collections::HashMap::new(),
        },
        Box::new(SystemClock),
    );
    let deps = GatewayDeps {
        ops: std::sync::Arc::new(ops),
        state_dir: state_dir.to_path_buf(),
    };
    deps.build_router(GatewayConfig {
        token: Some(TOKEN.to_string()),
        allow_local_files: false,
        insecure: false,
        max_sandboxes: 8,
        rate_limit: lsbx_gateway::RateLimitConfig {
            requests_per_minute: 6000,
            burst: 1000,
        },
    })
}

#[tokio::test]
async fn console_page_loads_assets_from_its_own_origin_and_names_no_guest_ip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let router = build_router(dir.path());

    // Create a sandbox so there is a real target to render.
    let created = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sandboxes")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"default"}"#))
                .unwrap(),
        )
        .await
        .expect("create");
    assert_eq!(created.status(), StatusCode::OK);
    let body = axum::body::to_bytes(created.into_body(), usize::MAX)
        .await
        .expect("body");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let id = parsed["data"]["id"].as_str().expect("id").to_string();

    // The page itself.
    let page = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/console?target={id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("console page");
    assert_eq!(page.status(), StatusCode::OK);
    let html = String::from_utf8(
        axum::body::to_bytes(page.into_body(), usize::MAX)
            .await
            .expect("body")
            .to_vec(),
    )
    .expect("utf8");

    assert!(
        !html.contains("192.168."),
        "the console page must never mention a guest-private address — that was the \
         dead-iframe bug (#55). Got:\n{html}"
    );
    assert!(!html.contains(TOKEN), "the master token must not reach the page");
    assert!(
        html.contains(&format!("stream/{id}/vnc")) || html.contains("stream/' + encodeURIComponent"),
        "the page must open the gateway's stream route for this sandbox"
    );
    assert!(
        html.contains("/assets/novnc/core/rfb.js"),
        "the page must load the vendored noVNC client from its own origin"
    );
}

#[tokio::test]
async fn vendored_novnc_bundle_is_served_with_a_module_compatible_type() {
    let dir = tempfile::tempdir().expect("tempdir");
    let router = build_router(dir.path());

    // Public on purpose: a browser loading subresources of a page cannot attach
    // an Authorization header to each one.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/novnc/core/rfb.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("asset");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8"),
        "ES-module imports fail silently in browsers without a JS MIME type"
    );

    let missing = router
        .oneshot(
            Request::builder()
                .uri("/assets/novnc/does/not/exist.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("missing asset");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn console_ticket_grants_the_stream_upgrade_without_an_auth_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    let router = build_router(dir.path());

    let created = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sandboxes")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"default"}"#))
                .unwrap(),
        )
        .await
        .expect("create");
    let body = axum::body::to_bytes(created.into_body(), usize::MAX)
        .await
        .expect("body");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let id = parsed["data"]["id"].as_str().expect("id").to_string();

    // 1. Minting requires the master token.
    let unauth = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/consoles/{id}/token"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("unauth mint");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    // 2. With the token, a ticket comes back and is not the master token.
    let minted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/consoles/{id}/token"))
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("mint");
    assert_eq!(minted.status(), StatusCode::OK);
    assert_eq!(
        minted
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "a ticket is a credential and must not be cached"
    );
    let minted_body = axum::body::to_bytes(minted.into_body(), usize::MAX)
        .await
        .expect("body");
    let minted_json: serde_json::Value = serde_json::from_slice(&minted_body).expect("json");
    let ticket = minted_json["data"]["t"].as_str().expect("ticket").to_string();
    assert!(!ticket.is_empty());
    assert!(!ticket.contains(TOKEN), "ticket must not embed the master token");

    // 3. A forged ticket is refused at the upgrade, not relayed.
    let forged = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/stream/{id}/vnc?t=9999999999.deadbeef"))
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .header("Sec-WebSocket-Version", "13")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("forged upgrade");
    assert_eq!(
        forged.status(),
        StatusCode::UNAUTHORIZED,
        "an invalid ticket must not reach the stream handler"
    );

    // 4. A ticket minted for one sandbox must not open another.
    let other = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/stream/sbx-someone-else/vnc?t={ticket}"))
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .header("Sec-WebSocket-Version", "13")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("cross-sandbox upgrade");
    assert_eq!(other.status(), StatusCode::UNAUTHORIZED);

    // 5. The genuine ticket gets past auth on the real sandbox. (The DemoBackend
    //    guest is unreachable, so the *relay* may fail afterwards; the assertion
    //    is only that auth let it through — i.e. not a 401.)
    let genuine = router
        .oneshot(
            Request::builder()
                .uri(format!("/stream/{id}/vnc?t={ticket}"))
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .header("Sec-WebSocket-Version", "13")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("genuine upgrade");
    assert_ne!(
        genuine.status(),
        StatusCode::UNAUTHORIZED,
        "a valid ticket must satisfy the stream route's auth"
    );
}
