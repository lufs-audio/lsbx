# Unit 14 — WebSocket Stream Proxy & noVNC Console

## Objective
Implement the bidirectional WebSocket relay and embedded noVNC console page, replacing the existing raw-socket stdlib relay with `tokio-tungstenite`, while preserving the guest-port-8000 convention and state-store-mediated destination lookup.

## Context
Layer 6, depends on Unit 10 and Unit 02 (for the destination-lookup security property). This is Door 3 from SPEC.md §4.8.

## Acceptance criteria
- [ ] `/stream/<sandbox-id>/<guest-path>` resolves the destination host/port **exclusively** by looking up `<sandbox-id>` in the state store — never accepts a host/port directly from the request. Preserves the existing "prevents arbitrary host/port" security property exactly.
- [ ] Detects a WebSocket upgrade request and relays bytes bidirectionally between the client WS connection and a raw `TcpStream` to the guest's port 8000 (the noVNC/websockify convention, preserved).
- [ ] `/console?target=<sandbox-id>` serves a bundled noVNC HTML console page, embedded at build time (`include_str!` or an equivalent asset bundle) — not fetched from a CDN at runtime, matching the no-CDN-dependency discipline this house already applies to other verifiable, offline-capable surfaces.
- [ ] `/consoles/<id>` returns console detail including a `console_password` field, matching existing behavior.
- [ ] The relay correctly propagates connection close in both directions — client-closes-first and guest-closes-first are both tested — with no half-open connection leak.
- [ ] A malformed or unresolvable `sandbox-id` returns `404`/`LsbxError::NotFound` **before** any TCP connection to a guest is attempted — the lookup-then-connect ordering is itself part of the security property, not an implementation detail to optimize away.

## Interface contract
```rust
// src/proxy.rs
use axum::extract::ws::WebSocketUpgrade;

pub async fn stream_handler(
    axum::extract::Path((sandbox_id, guest_path)): axum::extract::Path<(String, String)>,
    ws: WebSocketUpgrade,
    ops: axum::extract::State<std::sync::Arc<lsbx_ops::LsbxOps>>,
) -> Result<axum::response::Response, lsbx_kernel::error::LsbxError>;

async fn relay(ws: axum::extract::ws::WebSocket, guest_addr: std::net::SocketAddr) -> Result<(), lsbx_kernel::error::LsbxError>;

// src/console.rs
pub struct ConsoleParams { pub target: String }

pub async fn console_page_handler(
    axum::extract::Query(params): axum::extract::Query<ConsoleParams>,
) -> axum::response::Html<&'static str>; // bundled noVNC page via include_str!

pub struct ConsoleDetail {
    pub id: String,
    pub console_url: Option<String>,
    pub console_password: Option<String>,
}

pub async fn console_detail(ops: &lsbx_ops::LsbxOps, id: &str) -> Result<ConsoleDetail, lsbx_kernel::error::LsbxError>;
```

## Boundaries — do NOT touch
Does not implement the rest of the gateway's routes — Unit 13 owns those and mounts this crate's router as a sub-router. Does not decide how a sandbox's host/port is determined at creation time (Units 06/07/09 own that) — only reads what's already recorded in the `SandboxRecord`.

## Output
- `crates/lsbx-stream/Cargo.toml`
- `crates/lsbx-stream/src/lib.rs`
- `crates/lsbx-stream/src/proxy.rs`
- `crates/lsbx-stream/src/console.rs`
- `crates/lsbx-stream/assets/novnc-console.html`
- `crates/lsbx-stream/tests/test_destination_lookup_security.rs`
- `crates/lsbx-stream/tests/test_relay_close_propagation.rs`

## Verification
```bash
cargo check -p lsbx-stream --message-format=json
cargo clippy -p lsbx-stream --all-targets --all-features -- -D warnings
cargo test -p lsbx-stream --test test_destination_lookup_security
cargo test -p lsbx-stream --test test_relay_close_propagation
```
Scenario: `test_destination_lookup_security` requests `/stream/<forged-id>/vnc` for a sandbox id absent from the store, and asserts (via a connection-attempt-counting test double) that no TCP connection is ever attempted — only a `NotFound` response is returned.
