//! Relay close-propagation scenarios (Unit 14's own acceptance criteria):
//! "The relay correctly propagates connection close in both directions —
//! client-closes-first and guest-closes-first are both tested — with no
//! half-open connection leak."
//!
//! ## Rework note: no `ops.set_mock_guest_addr(...)` on the real `LsbxOps`
//! An earlier draft called `ops.set_mock_guest_addr(guest_addr)` — no such
//! method exists on the real, merged `LsbxOps` (nor could it: the same
//! "shared, used by every door" reasoning from
//! `test_destination_lookup_security.rs`'s rework note applies here too).
//! This file constructs a real `SandboxStore`, seeds a real
//! `SandboxRecord` whose `host` field points at a locally-bound
//! `TcpListener` this test controls directly, and drives the actual
//! `stream_route_handler`/`relay` code path against it — the same
//! construction pattern as `test_destination_lookup_security.rs`, applied
//! to the close-propagation scenarios instead of the security scenario.

// See test_destination_lookup_security.rs's identically-worded comment
// above its own #![allow(...)] for why this exists (Unit 01's
// crates/lsbx-kernel/tests/test_kernel.rs pattern).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::routing::get;
use axum::Router;
use lsbx_kernel::types::SandboxRecord;
use lsbx_store::sandbox_store::SandboxStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// `host` is set to the full `ip:port` of `listener_addr` — see
/// `test_destination_lookup_security.rs`'s identically-shaped helper for
/// why (it lets this test's fake "guest" listener bind an OS-assigned
/// ephemeral port rather than needing the real, fixed `GUEST_VNC_PORT`
/// free, while still exercising `resolve_host_to_addr`'s real,
/// documented "host already includes a port" resolution path).
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

fn build_stream_router(store: Arc<SandboxStore>) -> Router {
    Router::new()
        .route(
            "/stream/{sandbox_id}/{guest_path}",
            get(lsbx_stream::proxy::stream_route_handler),
        )
        .with_state(store)
}

/// Starts the proxy router bound to a fresh local port and returns its
/// address. Kept as a helper since both scenarios below need an identical
/// setup, differing only in what the "guest" side of the relay does.
async fn start_proxy(store: Arc<SandboxStore>) -> std::net::SocketAddr {
    let app = build_stream_router(store);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind proxy listener");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

#[tokio::test]
async fn client_closes_first_propagates_to_guest_and_leaves_no_half_open_connection() {
    let guest_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind guest listener");
    let guest_addr = guest_listener.local_addr().expect("local_addr");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SandboxStore::new(dir.path().to_path_buf()));
    let sandbox_id = "sbx-client-closes-first";
    store
        .save(&sample_record(sandbox_id, guest_addr))
        .expect("seed record");

    let proxy_addr = start_proxy(store).await;

    // Accept the relay's connection on the guest side, then prove the
    // guest observes a clean TCP EOF once the client closes — this is the
    // "no half-open connection leak" property: the guest side must not be
    // left waiting on a peer that already went away.
    let guest_handle = tokio::spawn(async move {
        let (mut stream, _) = guest_listener.accept().await.expect("accept");
        let mut buf = [0u8; 16];
        let n = stream
            .read(&mut buf)
            .await
            .expect("read after client close");
        assert_eq!(
            n, 0,
            "guest side should observe EOF once the client closes first"
        );
    });

    let ws_url = format!("ws://{proxy_addr}/stream/{sandbox_id}/vnc");
    let (mut ws_stream, response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("upgrade should succeed for a valid, resolved sandbox id");
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SWITCHING_PROTOCOLS
    );

    // Client closes first.
    ws_stream
        .close(None)
        .await
        .expect("client-initiated close should succeed");

    tokio::time::timeout(Duration::from_secs(5), guest_handle)
        .await
        .expect("guest side should observe EOF within the timeout, not hang")
        .expect("guest task should not panic");
}

#[tokio::test]
async fn guest_closes_first_propagates_to_client_and_leaves_no_half_open_connection() {
    let guest_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind guest listener");
    let guest_addr = guest_listener.local_addr().expect("local_addr");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SandboxStore::new(dir.path().to_path_buf()));
    let sandbox_id = "sbx-guest-closes-first";
    store
        .save(&sample_record(sandbox_id, guest_addr))
        .expect("seed record");

    let proxy_addr = start_proxy(store).await;

    // Accept the relay's connection, then close it immediately from the
    // guest side — this is the guest-closes-first scenario.
    let guest_handle = tokio::spawn(async move {
        let (stream, _) = guest_listener.accept().await.expect("accept");
        drop(stream);
    });

    let ws_url = format!("ws://{proxy_addr}/stream/{sandbox_id}/vnc");
    let (mut ws_stream, response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("upgrade should succeed for a valid, resolved sandbox id");
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SWITCHING_PROTOCOLS
    );

    guest_handle.await.expect("guest task should not panic");

    // The client side must observe the relay closing cleanly (either an
    // explicit Close frame or the stream simply ending) within a bounded
    // window, never hanging indefinitely waiting on a guest that already
    // disconnected — that hang is exactly the "half-open connection leak"
    // this acceptance criterion rules out.
    let observed_close = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match futures::StreamExt::next(&mut ws_stream).await {
                Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => continue,
                Some(Err(_)) => return true,
                None => return true, // stream ended
            }
        }
    })
    .await
    .expect("client side should observe the relay closing within the timeout, not hang");

    assert!(
        observed_close,
        "client side should observe a clean close once the guest closes first"
    );
}

#[tokio::test]
async fn abandoned_client_is_reaped_before_a_reconnect_uses_a_fresh_guest_socket() {
    let guest_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind guest listener");
    let guest_addr = guest_listener.local_addr().expect("local_addr");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SandboxStore::new(dir.path().to_path_buf()));
    let sandbox_id = "sbx-abandon-then-reconnect";
    store
        .save(&sample_record(sandbox_id, guest_addr))
        .expect("seed record");

    let proxy_addr = start_proxy(store).await;

    // A browser tab can vanish without sending a WebSocket Close frame. The
    // first guest socket must still observe EOF, and the relay must accept a
    // second independent session rather than leaving the destination wedged.
    let guest_handle = tokio::spawn(async move {
        let (mut first, _) = guest_listener.accept().await.expect("accept first relay");
        let mut byte = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), first.read(&mut byte))
            .await
            .expect("abandoned first relay should be reaped promptly")
            .expect("read first relay");
        assert_eq!(
            n, 0,
            "guest should observe EOF after an abandoned browser client"
        );

        let (mut second, _) = guest_listener
            .accept()
            .await
            .expect("accept reconnect relay");
        second
            .write_all(b"RFB 003.008\n")
            .await
            .expect("write RFB greeting to reconnect");
    });

    let ws_url = format!("ws://{proxy_addr}/stream/{sandbox_id}/vnc");
    let (first_client, first_response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("first upgrade");
    assert_eq!(
        first_response.status(),
        axum::http::StatusCode::SWITCHING_PROTOCOLS
    );

    // No `.close()`: dropping the transport models a tab/process disappearing.
    drop(first_client);

    let (mut second_client, second_response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .expect("reconnect should not hang behind the abandoned client")
    .expect("second upgrade");
    assert_eq!(
        second_response.status(),
        axum::http::StatusCode::SWITCHING_PROTOCOLS
    );

    let greeting = tokio::time::timeout(
        Duration::from_secs(5),
        futures::StreamExt::next(&mut second_client),
    )
    .await
    .expect("reconnect should receive the guest greeting promptly")
    .expect("reconnect stream should remain open")
    .expect("reconnect frame should be valid");
    assert_eq!(greeting.into_data(), b"RFB 003.008\n".as_slice());

    second_client.close(None).await.expect("close reconnect");
    guest_handle.await.expect("guest task should not panic");
}
