//! Bidirectional WebSocket <-> TCP relay (Unit 14).
//!
//! ## Rework note: this file was substantially reworked from an earlier
//! Jules-generated draft
//!
//! The earlier draft's bidirectional relay logic (the `tokio::select!` over
//! both read directions, EOF/close handling in `relay`) was fundamentally
//! sound and is kept close to as-is below. Three real problems in that
//! draft are fixed here:
//!
//! 1. **`ops.lookup_sandbox_destination(&sandbox_id)` does not exist on the
//!    real `LsbxOps`.** Confirmed by direct re-read of the real, merged
//!    `crates/lsbx-ops/src/lib.rs` immediately before writing this file:
//!    `LsbxOps` has no method that returns a raw host/port for a sandbox —
//!    its `sandbox_store: SandboxStore` field is private, and none of
//!    `console_url`/`info`/`status` exposes a destination address. This
//!    unit's own Boundaries state the destination "resolves ... EXCLUSIVELY
//!    by looking up `<sandbox-id>` in the state store," which is a
//!    `SandboxStore` operation, and `LsbxOps`'s own Boundaries list a fixed
//!    eighteen-operation set (`create, destroy, list, exec, put, get,
//!    renew, console_url, info, status, reap, golden_build, golden_verify,
//!    golden_register, golden_delete, golden_list, config_show,
//!    logs_query`) that adding a nineteenth narrow lookup method to is not
//!    this unit's call to make. Given that, this crate takes a
//!    `SandboxStore` directly (constructed by whoever also constructs
//!    `LsbxOps`, from the same `state_dir` — see `LsbxOps::new`'s own
//!    constructor, which likewise takes an owned `SandboxStore` rather than
//!    an `Arc`-wrapped one, and Unit 10's own
//!    `tests/test_all_operations.rs::reap_sweeps_expired_sandbox_and_leaves_live_one`
//!    test, which independently constructs a second `SandboxStore` pointed
//!    at the same directory for exactly this reason: `SandboxStore` has no
//!    `Clone`, so two owners of the same on-disk state each get their own
//!    plain, cheap, synchronous-`fs`-backed instance rather than sharing one
//!    object). `Arc<SandboxStore>` is this crate's own Axum state type
//!    (mirroring the `Arc<LsbxOps>` the interface contract already uses for
//!    the same "shared, `&self`-taking handle usable from multiple Axum
//!    handlers" purpose), passed alongside `Arc<LsbxOps>` for the one
//!    operation this crate needs from `LsbxOps` (`console_detail`, in
//!    `console.rs`). See this crate's PR description for why this is the
//!    stated design decision rather than an ad hoc workaround.
//! 2. **`LsbxError::InternalError` does not exist.** The real, merged
//!    `LsbxError` (`lsbx-kernel/src/error.rs`) is a closed 7-variant enum:
//!    `Usage`, `BackendUnavailable`, `NotFound`, `ContractViolated`,
//!    `LockContention`, `AuthFailed`, `Interrupted` — no `InternalError`.
//!    Two distinct failure shapes need two distinct, real variants here:
//!    - The sandbox id itself does not resolve in the store ->
//!      `LsbxError::NotFound` (this is what `SandboxStore::load` already
//!      returns for a missing record — no new mapping needed, see
//!      `resolve_destination` below).
//!    - The id resolves to a real `SandboxRecord`, but the TCP connection
//!      to the guest's resolved `host:8000` fails (connection refused,
//!      timed out, host unreachable) -> `LsbxError::BackendUnavailable`.
//!      This is "the guest is unreachable," the same shape
//!      `lsbx-backend-demo::DemoBackend` already uses `BackendUnavailable`
//!      for (its `FaultMode::Unavailable` failures), and matches the real
//!      `ExitCode::BackendUnavailable` = "the selected backend's control
//!      plane is unreachable" (SPEC.md §6) more precisely than `NotFound`
//!      would: the destination is known and well-formed, it simply isn't
//!      answering — a materially different claim from "this identifier
//!      doesn't resolve to anything."
//! 3. **Security-critical ordering**: resolving `sandbox_id -> destination`
//!    and failing with `NotFound` must happen *before* any TCP connection
//!    is attempted — never connect first and handle failure after. The
//!    original draft's `stream_handler` already got this right structurally
//!    (the lookup happens before `ws.on_upgrade`, and only the resolved
//!    `guest_addr` — never the raw request — crosses into the upgrade
//!    closure), and that shape is preserved exactly here: `resolve_destination`
//!    is still called, and still allowed to short-circuit with `?`, before
//!    `ws.on_upgrade` is ever reached. The TCP `connect()` call itself
//!    still lives inside `relay`, which still only runs inside the upgrade
//!    closure — i.e. strictly after resolution has already succeeded — so
//!    fixing point 1 (moving the actual store lookup from a nonexistent
//!    `LsbxOps` method to a real `SandboxStore::load` call) does not move
//!    *where* in the control flow that lookup happens.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use lsbx_kernel::error::LsbxError;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The fixed guest port every stream target connects to — the
/// noVNC/websockify convention this unit's contract and SPEC.md §4.8 both
/// name explicitly ("preserving the guest-port-8000 convention"). Not
/// configurable per request: accepting a port from the request would
/// reopen exactly the "arbitrary host:port reachable through the proxy"
/// hole this unit's own acceptance criteria requires closing.
const GUEST_VNC_PORT: u16 = 8000;

/// A WebSocket upgrade must not turn into an indefinitely connecting relay when
/// a sandbox record points at a guest that is powered off or filtered. The
/// browser has its own bounded reconnect UI, so fail this attempt promptly and
/// let the user request a fresh ticket/connection instead.
const GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// WebSocket ping cadence and the maximum time the relay will tolerate without
/// receiving any client frame (including the browser's automatic Pong). This
/// catches silent network loss where neither EOF nor a WebSocket Close frame is
/// delivered; relying on kernel TCP keepalive alone can leave the guest socket
/// half-open for hours.
const CLIENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const CLIENT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

/// A peer that stops reading must not block the relay loop forever and prevent
/// its heartbeat/cleanup path from running.
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maps an `LsbxError` onto an HTTP response for this crate's handlers.
///
/// `lsbx-kernel` only maps `LsbxError` -> `ExitCode` (a process exit status,
/// not an HTTP status), and `LsbxError` is a foreign type from this crate's
/// perspective — Rust's orphan rules forbid implementing axum's (also
/// foreign) `IntoResponse` trait directly on it here. A free function
/// filling the same role, called from the route-mounted handler below
/// rather than relied on implicitly via `?`, sidesteps that without
/// needing a wrapper newtype to leak into this module's public API. Scoped
/// to what this crate's own handlers actually return: `NotFound` -> 404
/// (the acceptance criteria's own naming: "a malformed or unresolvable
/// `sandbox-id` returns `404`/`LsbxError::NotFound`"), `BackendUnavailable`
/// -> 503, and everything else -> an appropriate 4xx/5xx, all carrying the
/// error's `Display` message as the body so a caller can see why.
fn error_response(err: &LsbxError) -> Response {
    let status = match err {
        LsbxError::Usage(_) => StatusCode::BAD_REQUEST,
        LsbxError::NotFound(_) => StatusCode::NOT_FOUND,
        LsbxError::BackendUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        LsbxError::AuthFailed(_) => StatusCode::UNAUTHORIZED,
        LsbxError::LockContention(_) => StatusCode::CONFLICT,
        LsbxError::ContractViolated(_) | LsbxError::Interrupted(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, err.to_string()).into_response()
}

/// Resolves `sandbox_id` to a `SocketAddr` by looking it up **exclusively**
/// in the state store, per this unit's acceptance criteria and Boundaries.
///
/// Never accepts a host/port from the request itself — the caller only
/// ever supplies `sandbox_id`, and the returned address is always
/// `<the store's recorded host>:8000` (`GUEST_VNC_PORT`), never a
/// caller-controlled port. `SandboxStore::load`'s own real behavior already
/// maps a missing record onto `LsbxError::NotFound` (`map_io_err` treats a
/// `NotFound`-kind `io::Error` from opening the record file as
/// `LsbxError::NotFound`) — this function does not need to invent that
/// mapping, only add the second failure mode a store lookup does not cover
/// on its own: a `host` string that doesn't parse or resolve to a usable
/// socket address is reported as `LsbxError::NotFound` too, since "the
/// stored destination is not a usable address" is the same class of "this
/// identifier does not resolve to anything real" as "the id itself is
/// absent from the store" — neither is "the guest is unreachable" (that's
/// `BackendUnavailable`, reserved for a resolved, well-formed address that
/// a TCP connection attempt itself fails against, in `relay` below).
fn resolve_destination(
    store: &lsbx_store::sandbox_store::SandboxStore,
    sandbox_id: &str,
) -> Result<SocketAddr, LsbxError> {
    let record = store.load(sandbox_id)?;
    resolve_host_to_addr(&record.host, GUEST_VNC_PORT).ok_or_else(|| {
        LsbxError::NotFound(format!(
            "sandbox {sandbox_id} resolved to host '{}', which is not a usable address",
            record.host
        ))
    })
}

/// Turns a stored `host` string (which may already be a bare IP, or an
/// `ip:port`/`host:port` pair depending on what the record actually
/// contains) plus a fixed `default_port` into a `SocketAddr`.
///
/// Tries, in order: `host` parsed directly as a `SocketAddr` (covers a
/// record whose `host` field already includes a port); `host` parsed as a
/// bare `IpAddr` combined with `default_port` (the common case — see
/// Unit 05's `DemoBackend`, whose `host` field is a bare
/// `<hash>.demo.local`-shaped hostname, and Unit 09's real `create`, which
/// persists `created_vm.host` verbatim from whatever a `Backend`
/// implementation returned). A hostname that is neither is intentionally
/// *not* DNS-resolved here — this function only ever parses, it never
/// performs a network lookup, since resolving an attacker-influenced
/// hostname string at request time is its own can of problems and no
/// backend implementation merged so far (`demo`) actually needs it: an
/// address a caller cannot parse without a DNS round-trip is not "resolved
/// exclusively by looking up the sandbox id in the state store" in the
/// sense this unit's acceptance criteria means.
fn resolve_host_to_addr(host: &str, default_port: u16) -> Option<SocketAddr> {
    if let Ok(addr) = host.parse::<SocketAddr>() {
        return Some(addr);
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(ip, default_port));
    }
    None
}

/// Detects a WebSocket upgrade request and, once the destination has been
/// resolved (and only then), relays bytes bidirectionally between the
/// client's WebSocket connection and a raw `TcpStream` toward the guest's
/// fixed VNC port.
///
/// This matches the interface contract's literal signature exactly
/// (`Result<Response, LsbxError>`), which is why it is a free function
/// returning a typed error rather than an axum-route-mountable handler
/// directly: axum's `Handler` blanket impl requires the return type to
/// implement `IntoResponse`, and `Result<Response, LsbxError>` only
/// satisfies that if `LsbxError: IntoResponse` — which this crate cannot
/// implement directly (orphan rules; see `error_response` above). This
/// function is the typed, composable core; [`stream_route_handler`] below
/// is the thin axum-mountable wrapper `router()` actually registers.
///
/// `guest_path` is accepted (matching the interface contract's
/// `Path<(String, String)>`) but not used to influence the destination —
/// see this module's doc comment and `resolve_destination`'s own doc
/// comment for why the destination is derived exclusively from the state
/// store, never from any part of the request.
pub async fn stream_handler(
    Path((sandbox_id, _guest_path)): Path<(String, String)>,
    ws: WebSocketUpgrade,
    State(store): State<Arc<lsbx_store::sandbox_store::SandboxStore>>,
) -> Result<Response, LsbxError> {
    // Resolve the destination *before* ws.on_upgrade is ever called — this
    // ordering is itself part of the security property (see this module's
    // doc comment, point 3), not an implementation detail free to move.
    // The `?` here means a resolution failure returns straight to the
    // caller (as a typed `LsbxError`, converted to an HTTP response by
    // `stream_route_handler`'s caller, or usable as-is by any other typed
    // composition) and never reaches the upgrade closure, so no TCP
    // connection to any guest is ever attempted for an id that doesn't
    // resolve.
    let guest_addr = resolve_destination(&store, &sandbox_id)?;

    // noVNC offers `binary`; browsers reject an upgrade that omits the
    // selected protocol when one was requested. Client requests without
    // a subprotocol remain supported.
    Ok(ws
        .protocols(["binary"])
        .on_upgrade(move |socket| async move {
            let _ = relay(socket, guest_addr).await;
        }))
}

/// The actual axum route target `router()` registers for
/// `/stream/{sandbox_id}/{guest_path}` — delegates to [`stream_handler`]
/// and converts its typed `Err(LsbxError)` into an HTTP response via
/// `error_response`, so a resolution failure still reaches the client as
/// `404`/etc. rather than a generic framework error.
pub async fn stream_route_handler(
    path: Path<(String, String)>,
    ws: WebSocketUpgrade,
    state: State<Arc<lsbx_store::sandbox_store::SandboxStore>>,
) -> Response {
    match stream_handler(path, ws, state).await {
        Ok(response) => response,
        Err(err) => error_response(&err),
    }
}

/// Bidirectionally relays bytes between `ws` and a fresh `TcpStream`
/// connected to `guest_addr`.
///
/// One event loop owns both sockets so it can also send WebSocket Ping frames
/// and observe Pong/other client activity. EOF and explicit Close frames still
/// tear down immediately; a silently vanished browser is reaped after
/// [`CLIENT_HEARTBEAT_TIMEOUT`] instead of waiting for the host's much longer
/// TCP keepalive defaults. Every write is bounded too, because a send blocked on
/// a peer that stopped reading would otherwise prevent the heartbeat branch
/// from running.
async fn relay(mut ws: WebSocket, guest_addr: SocketAddr) -> Result<(), LsbxError> {
    let mut tcp_stream =
        tokio::time::timeout(GUEST_CONNECT_TIMEOUT, TcpStream::connect(guest_addr))
            .await
            .map_err(|_| {
                LsbxError::BackendUnavailable(format!(
                    "timed out connecting to guest at {guest_addr}"
                ))
            })?
            .map_err(|e| {
                LsbxError::BackendUnavailable(format!(
                    "failed to connect to guest at {guest_addr}: {e}"
                ))
            })?;

    let mut buf = vec![0u8; 8192];
    let mut last_client_activity = tokio::time::Instant::now();
    let first_heartbeat = last_client_activity + CLIENT_HEARTBEAT_INTERVAL;
    let mut heartbeat = tokio::time::interval_at(first_heartbeat, CLIENT_HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            message = ws.next() => {
                let Some(message) = message else {
                    break;
                };
                let Ok(message) = message else {
                    break;
                };
                last_client_activity = tokio::time::Instant::now();

                match message {
                    Message::Binary(data) => {
                        if timed_tcp_write(&mut tcp_stream, &data).await.is_err() {
                            break;
                        }
                    }
                    Message::Text(data) => {
                        if timed_tcp_write(&mut tcp_stream, data.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Message::Ping(payload) => {
                        if !timed_ws_send(&mut ws, Message::Pong(payload)).await {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => {
                        let _ = timed_ws_send(&mut ws, Message::Close(None)).await;
                        break;
                    }
                }
            }
            read_result = tcp_stream.read(&mut buf) => {
                match read_result {
                    Ok(0) => {
                        let _ = timed_ws_send(&mut ws, Message::Close(None)).await;
                        break;
                    }
                    Ok(n) => {
                        if !timed_ws_send(
                            &mut ws,
                            Message::Binary(buf[..n].to_vec().into()),
                        ).await {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = timed_ws_send(&mut ws, Message::Close(None)).await;
                        break;
                    }
                }
            }
            _ = heartbeat.tick() => {
                if client_heartbeat_expired(last_client_activity, tokio::time::Instant::now()) {
                    let _ = timed_ws_send(&mut ws, Message::Close(None)).await;
                    break;
                }
                if !timed_ws_send(&mut ws, Message::Ping(b"lsbx".to_vec().into())).await {
                    break;
                }
            }
        }
    }

    // Make the guest observe EOF promptly in every exit path, including a
    // heartbeat timeout where the browser-side TCP connection never closed.
    let _ = tcp_stream.shutdown().await;
    Ok(())
}

async fn timed_tcp_write(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    match tokio::time::timeout(RELAY_WRITE_TIMEOUT, stream.write_all(data)).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "guest write timed out",
        )),
    }
}

async fn timed_ws_send(ws: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(RELAY_WRITE_TIMEOUT, ws.send(message)).await,
        Ok(Ok(()))
    )
}

fn client_heartbeat_expired(
    last_activity: tokio::time::Instant,
    now: tokio::time::Instant,
) -> bool {
    now.saturating_duration_since(last_activity) >= CLIENT_HEARTBEAT_TIMEOUT
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    #[test]
    fn heartbeat_only_expires_after_the_full_silence_window() {
        let start = tokio::time::Instant::now();
        assert!(!client_heartbeat_expired(
            start,
            start + CLIENT_HEARTBEAT_TIMEOUT - Duration::from_millis(1),
        ));
        assert!(client_heartbeat_expired(
            start,
            start + CLIENT_HEARTBEAT_TIMEOUT,
        ));
    }
}
