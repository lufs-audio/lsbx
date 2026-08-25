//! Security scenario (Unit 14's own named acceptance criterion):
//! `/stream/<forged-id>/vnc` for a sandbox id absent from the store must
//! return `404`/`LsbxError::NotFound`, and must never attempt a TCP
//! connection to any guest.
//!
//! ## Rework note: no test-double methods on the real `LsbxOps`/`SandboxStore`
//! An earlier draft of this test called `LsbxOps::new()` with no arguments,
//! and asserted against `ops.connection_attempts` — neither exists on the
//! real, merged `LsbxOps` (its real constructor takes six arguments; it
//! has no `connection_attempts` field, and cannot: it's a shared struct
//! used by every other door and the CI broker, so it can't carry ad hoc
//! test-spy fields for one crate's tests). This file uses only real,
//! already-merged construction:
//!
//! - A real `SandboxStore` backed by a `tempfile::tempdir()`, matching the
//!   exact pattern `lsbx-ops`'s own `tests/test_all_operations.rs::build_ops`
//!   establishes.
//! - The "forged id" case needs no mocking at all: an id that was simply
//!   never `save()`d into the store already produces `LsbxError::NotFound`
//!   from the real `SandboxStore::load` — that *is* the forged-id case,
//!   not a scenario requiring a fake.
//! - The "zero connection attempts" assertion is proven against a real
//!   `tokio::net::TcpListener` this test spins up and controls directly
//!   (an `AtomicUsize` incremented from a background `accept()` loop this
//!   test owns), rather than asking the production `LsbxOps`/`SandboxStore`
//!   type to self-report a counter neither one has. A *valid*, seeded
//!   sandbox record points at this listener's real address, so the test
//!   can assert the listener receives a connection for the valid id (the
//!   happy path this crate's relay logic exists to serve) while asserting
//!   it receives *zero* connections when the forged id is requested — the
//!   comparison is what makes "no TCP connection is ever attempted for the
//!   forged id" a meaningfully tested claim rather than "a listener that
//!   was never going to be reachable anyway wasn't reached."

// Integration test binary -- every fn here is a #[tokio::test], so a failed
// unwrap()/expect() only ever panics inside `cargo test`, never in a
// shipped code path. Pattern established in Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs and every prior merged unit's own
// tests/*.rs (e.g. lsbx-ops's tests/test_all_operations.rs).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::routing::get;
use axum::Router;
use lsbx_kernel::types::SandboxRecord;
use lsbx_store::sandbox_store::SandboxStore;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Builds a `SandboxRecord` with every field the real, merged
/// `lsbx_kernel::types::SandboxRecord` requires, `host` set to the full
/// `ip:port` of `listener_addr` — a full, real record (not a partial
/// fixture), matching the shape `lsbx-lifecycle`'s own `lease.rs` test
/// helper (`base_record`) and `lsbx-ops`'s own `create` flow both
/// populate. Encoding the port in `host` (rather than a bare IP) is what
/// lets this test's fake "guest" listener bind an OS-assigned ephemeral
/// port instead of needing the real, fixed `GUEST_VNC_PORT` (8000) to be
/// free — `resolve_host_to_addr` in `src/proxy.rs` explicitly supports a
/// `host` that already includes a port for exactly this reason (a record
/// whose `host` field is a full `SocketAddr` string), so this is exercising
/// real, documented resolution behavior, not working around it.
fn sample_record(id: &str, listener_addr: std::net::SocketAddr) -> SandboxRecord {
    SandboxRecord {
        id: id.to_string(),
        name: id.to_string(),
        host: listener_addr.to_string(),
        profile: "lsbx-default-v1".to_string(),
        flavor: "default".to_string(),
        streaming: "novnc".to_string(),
        username: None,
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at: None,
        vm_tag: Some(format!("demo-{id}")),
        https_url: Some(format!("https://{}/novnc", listener_addr.ip())),
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

/// Spawns a background task that counts every accepted connection into
/// `counter`, and returns the listener's real, bound address. This is the
/// "real `TcpListener` whose accept-count is tracked in an `AtomicUsize`
/// this test controls" double the task's own guidance calls for, standing
/// in for a guest VNC endpoint without needing any real VM.
async fn spawn_counting_listener() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind counting listener");
    let addr = listener.local_addr().expect("local_addr");
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    tokio::spawn(async move {
        while let Ok((_stream, _peer)) = listener.accept().await {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        }
    });

    (addr, counter)
}

/// Builds the router under test with a real `SandboxStore` (backed by a
/// fresh tempdir) as its only piece of state — exactly what
/// `proxy::stream_route_handler` actually needs, matching
/// `lsbx_stream::router`'s own composition (this test builds the stream
/// half directly rather than going through the full `StreamState`, since
/// this scenario has nothing to do with `LsbxOps`/console routes).
fn build_stream_router(store: Arc<SandboxStore>) -> Router {
    Router::new()
        .route(
            "/stream/{sandbox_id}/{guest_path}",
            get(lsbx_stream::proxy::stream_route_handler),
        )
        .with_state(store)
}

#[tokio::test]
async fn forged_sandbox_id_returns_404_with_zero_connection_attempts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SandboxStore::new(dir.path().to_path_buf()));

    // A real listener standing in for a guest VNC endpoint, so this test
    // can prove the forged-id request never reaches it — deliberately
    // *not* seeded into the store under the forged id below.
    let (guest_addr, connection_count) = spawn_counting_listener().await;

    // Seed a *different*, valid sandbox pointing at the same listener, so
    // the comparison in this test is meaningful: the listener is provably
    // reachable through this proxy for a real id, which is what makes "zero
    // connections for the forged id" a real assertion rather than a
    // tautology about an unreachable listener.
    let valid_id = "sbx-valid-0001";
    store
        .save(&sample_record(valid_id, guest_addr))
        .expect("seed valid record");

    let app = build_stream_router(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy listener");
    let proxy_addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // The forged id: never saved into the store at all. This is the
    // "forged/nonexistent sandbox id" scenario named by this unit's own
    // Verification section — no mocking needed, an absent record already
    // produces LsbxError::NotFound from the real SandboxStore::load.
    let forged_id = "sbx-forged-does-not-exist";
    let ws_url = format!("ws://{proxy_addr}/stream/{forged_id}/vnc");

    let result = tokio_tungstenite::connect_async(&ws_url).await;

    // Must fail — never a successful upgrade — and specifically with an
    // HTTP 404, matching this unit's own acceptance criteria wording
    // ("returns 404/LsbxError::NotFound").
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        }
        other => panic!("expected an HTTP 404 upgrade rejection, got: {other:?}"),
    }

    // Give any (incorrect) connection attempt a brief window to land
    // before asserting it never did — this is a real wait against a real
    // listener, not a synchronous check that could race a background task.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        0,
        "no TCP connection should ever be attempted for a sandbox id absent from the store"
    );
}

#[tokio::test]
async fn valid_sandbox_id_does_reach_the_resolved_guest_listener() {
    // The converse of the scenario above, proving the comparison is
    // meaningful: a *valid*, seeded id really does cause this proxy to
    // connect to the destination resolved from the store.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SandboxStore::new(dir.path().to_path_buf()));

    let (guest_addr, connection_count) = spawn_counting_listener().await;

    let valid_id = "sbx-valid-0002";
    store
        .save(&sample_record(valid_id, guest_addr))
        .expect("seed valid record");

    let app = build_stream_router(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy listener");
    let proxy_addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let ws_url = format!("ws://{proxy_addr}/stream/{valid_id}/vnc");
    let (_ws_stream, response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("valid id should upgrade successfully");
    assert_eq!(response.status(), axum::http::StatusCode::SWITCHING_PROTOCOLS);

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        1,
        "a valid, resolved sandbox id should result in exactly one TCP connection to its guest"
    );
}
