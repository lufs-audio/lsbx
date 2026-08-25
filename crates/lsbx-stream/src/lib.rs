//! `lsbx-stream` — WebSocket Stream Proxy & noVNC Console (Unit 14).
//!
//! Door 3 from SPEC.md §4.8: a WebSocket relay (`proxy.rs`) bridging a
//! client WebSocket connection to a raw `TcpStream` toward a sandbox
//! guest's fixed VNC port, plus a bundled noVNC console page and console
//! detail lookup (`console.rs`).
//!
//! ## Boundaries (per this unit's own contract)
//! Does not implement the rest of the gateway's routes — Unit 13
//! (`lsbx-gateway`) owns those and mounts this crate's router as a
//! sub-router (as of this PR, Unit 13 has not yet landed on `main` — see
//! this crate's PR description for what that means for verifying that
//! composition point today). Does not decide how a sandbox's host/port is
//! determined at creation time (Units 06/07/09 own that) — this crate only
//! reads what a `SandboxRecord` already has recorded, via `SandboxStore`.
//!
//! ## Why this crate depends on `lsbx-store` directly, not only `lsbx-ops`
//! See `proxy.rs`'s module doc comment (point 1) for the full design
//! rationale: the real, merged `LsbxOps` has no method that resolves a
//! sandbox id to a raw destination address, and adding one is explicitly
//! not this unit's call per `LsbxOps`'s own closed-operation-set
//! Boundaries. This crate's router therefore holds two pieces of shared
//! state side by side — `Arc<lsbx_store::sandbox_store::SandboxStore>` for
//! the destination lookup `proxy.rs` needs, and `Arc<lsbx_ops::LsbxOps>`
//! for everything else (`console.rs`'s `console_detail`) — rather than
//! reaching around `LsbxOps` in a way that would violate SPEC.md's
//! Deviation 12 (every door depends on `lsbx-ops` and *only* `lsbx-ops` for
//! operational logic) for the operations `LsbxOps` actually does own.

pub mod console;
pub mod proxy;

pub use console::{console_detail, console_page_handler, ConsoleDetail, ConsoleParams};
pub use proxy::stream_handler;

use console::consoles_route_handler;
use proxy::stream_route_handler;

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

/// Shared state this crate's router needs: the `LsbxOps` façade for
/// everything not related to raw destination resolution, and the
/// `SandboxStore` this crate's own destination lookup depends on directly.
/// See this module's doc comment for why both are held side by side.
#[derive(Clone)]
pub struct StreamState {
    pub ops: Arc<lsbx_ops::LsbxOps>,
    pub store: Arc<lsbx_store::sandbox_store::SandboxStore>,
}

/// Builds this crate's router as a mountable sub-router, per this unit's
/// own Boundaries ("Unit 13 owns [the gateway's other routes] and mounts
/// this crate's router as a sub-router"). Covers every route this unit's
/// acceptance criteria names: `/stream/{sandbox_id}/{guest_path}` (the WS
/// relay), `/console` (the bundled noVNC page), and `/consoles/{id}`
/// (console detail JSON). The stream and consoles routes are mounted via
/// thin axum-mountable wrappers (`proxy::stream_route_handler`,
/// `console::consoles_route_handler`) around their typed cores, needed
/// because `LsbxError` can't implement axum's foreign `IntoResponse`
/// directly here — see `proxy.rs`'s doc comments for the fuller
/// rationale. Built as `Router<Arc<SandboxStore>>` for the stream route
/// and `Router<Arc<LsbxOps>>` for the two console routes, each resolved to
/// a state-free `Router` via its own `with_state` call, then merged into
/// one router — this is the shape Unit 13 mounts directly.
pub fn router(state: StreamState) -> Router {
    let stream_routes = Router::new()
        .route("/stream/{sandbox_id}/{guest_path}", get(stream_route_handler))
        .with_state(state.store);

    let console_routes = Router::new()
        .route("/console", get(proxy_console_page))
        .route("/consoles/{id}", get(consoles_route_handler))
        .with_state(state.ops);

    stream_routes.merge(console_routes)
}

/// Thin wrapper so `console_page_handler` (which takes no `State`) can sit
/// in the same router as handlers that do, without forcing `console.rs`'s
/// signature to carry a state parameter it never uses. Kept here rather
/// than in `console.rs` since it's routing-composition glue, not console
/// logic.
async fn proxy_console_page(
    params: axum::extract::Query<ConsoleParams>,
) -> axum::response::Html<&'static str> {
    console_page_handler(params).await
}
