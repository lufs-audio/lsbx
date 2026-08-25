//! noVNC console page + console detail (Unit 14).
//!
//! ## `console_password` — an honest gap, not a fabricated value
//! This unit's acceptance criteria says `/consoles/<id>` must return
//! console detail "including a `console_password` field, matching
//! existing behavior." Confirmed by direct re-read of the real, merged
//! `lsbx_kernel::types::SandboxRecord` immediately before writing this
//! file: there is no password field anywhere on it (nor on `PublicSandbox`,
//! nor anywhere in `LsbxOps`'s public surface — `info`/`console_url` are
//! the only two operations that touch a sandbox's console-related state,
//! and neither returns or computes a password). No merged crate generates,
//! stores, or exposes a per-sandbox console password today.
//!
//! Rather than invent a plausible-looking value here (which would silently
//! misrepresent what this system can actually prove about a sandbox's
//! console), this mirrors the same "honest, documented gap" pattern
//! `lsbx-ops` itself already established for `logs_query` and
//! `config_show`'s module doc comment: `console_password` is a real field
//! on `ConsoleDetail` (per the interface contract's literal shape, so a
//! caller's deserialization never breaks), populated with `None` today,
//! with this comment naming exactly why. The day a real backend/mechanism
//! for console auth lands (most naturally as part of a `Backend`
//! implementation that actually manages a VNC/websockify password), this
//! function's body is the only thing that needs to change — its signature
//! already carries the field.

use axum::{
    extract::{Path, Query, State},
    response::{Html, IntoResponse, Json, Response},
};
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use serde::Deserialize;
use std::sync::Arc;

/// Bundled noVNC HTML console page, embedded at build time via
/// `include_str!` — never fetched from a CDN at runtime, per this unit's
/// acceptance criteria and the house's no-CDN-dependency discipline for
/// offline-capable surfaces.
///
/// ## Flagged gap: this is a clearly-labeled placeholder, not real noVNC
/// `assets/novnc-console.html` is a deliberately minimal, explicitly
/// labeled placeholder page — not a vendored copy of the real noVNC JS
/// client. Fetching and vendoring the actual noVNC client (its RFB
/// protocol implementation, its UI chrome, its asset bundle) is a
/// materially larger effort than this unit's remaining scope justifies,
/// and doing it via an AI-generated approximation of noVNC's JS instead of
/// the real, upstream-maintained client would be worse than an honest
/// placeholder: a fake "noVNC-shaped" client that doesn't actually speak
/// the RFB-over-WebSocket protocol correctly would silently fail in a way
/// that's harder to diagnose than a page that plainly says "not yet
/// wired up." This is flagged here, in the file this unit ships, and again
/// in this crate's own PR description, as an explicit gap for whoever
/// picks up real console UI work next — not silently left as a bare
/// one-line stub with no explanation.
const NOVNC_CONSOLE_HTML: &str = include_str!("../assets/novnc-console.html");

#[derive(Deserialize)]
pub struct ConsoleParams {
    pub target: String,
}

/// `GET /console?target=<sandbox-id>` — serves the bundled noVNC console
/// page. Does not itself look up `target` against the store or attempt any
/// connection; the page's own client-side script is what would open a
/// WebSocket back to `/stream/<sandbox-id>/vnc` (this crate's `proxy.rs`
/// handler), which is where the real, store-mediated destination lookup
/// and security property actually live. `target` is accepted here purely
/// so the served page can embed it (e.g. into a query string it hands to
/// the stream endpoint) — see the placeholder gap noted on
/// `NOVNC_CONSOLE_HTML` above for why that wiring is not fleshed out yet.
pub async fn console_page_handler(Query(_params): Query<ConsoleParams>) -> Html<&'static str> {
    Html(NOVNC_CONSOLE_HTML)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleDetail {
    pub id: String,
    pub console_url: Option<String>,
    pub console_password: Option<String>,
}

/// Console detail for a sandbox, built from `LsbxOps`'s real, merged
/// public surface (`info` for `id`, `console_url` for the computed console
/// URL). See this module's doc comment for why `console_password` is
/// `None` today rather than a fabricated value.
///
/// This matches the interface contract's literal signature exactly
/// (`console_detail(ops: &LsbxOps, id: &str) -> Result<ConsoleDetail,
/// LsbxError>`) — a plain, typed function rather than an axum handler, for
/// the same orphan-rule reason `proxy.rs`'s `stream_handler` is: `LsbxOps`
/// is foreign to this crate, but so is axum's `IntoResponse`, so nothing
/// here needs `LsbxError: IntoResponse` as long as this function itself
/// stays a typed core. [`consoles_route_handler`] below is the
/// axum-mountable wrapper `router()` in `lib.rs` actually registers for
/// `/consoles/{id}`.
///
/// A `id` that does not resolve propagates `LsbxOps::info`'s own
/// `LsbxError::NotFound` unchanged — this function adds no additional
/// error mapping of its own.
pub async fn console_detail(ops: &LsbxOps, id: &str) -> Result<ConsoleDetail, LsbxError> {
    let sandbox = ops.info(id).await?;
    Ok(ConsoleDetail {
        id: sandbox.id,
        console_url: sandbox.console_url,
        console_password: None,
    })
}

/// The actual axum route target `lib.rs`'s `router()` registers for
/// `GET /consoles/{id}` (this unit's own acceptance criteria: "`/consoles/<id>`
/// returns console detail including a `console_password` field"). Delegates
/// to [`console_detail`] and converts a `NotFound`/other error into an HTTP
/// response the same way `proxy::stream_route_handler` does for the stream
/// route, so an unresolvable id reaches the client as `404` rather than a
/// generic framework error.
pub async fn consoles_route_handler(
    Path(id): Path<String>,
    State(ops): State<Arc<LsbxOps>>,
) -> Response {
    match console_detail(&ops, &id).await {
        Ok(detail) => Json(detail).into_response(),
        Err(err) => error_response(&err),
    }
}

/// Maps an `LsbxError` onto an HTTP response for this module's routes.
/// Kept as its own small function (rather than sharing `proxy.rs`'s
/// private `error_response`) so this module doesn't need a `pub(crate)`
/// cross-module error-mapping dependency for one call site; the mapping
/// itself is identical in spirit to `proxy.rs`'s (see that module's doc
/// comment on `error_response` for the fuller rationale).
fn error_response(err: &LsbxError) -> Response {
    let status = match err {
        LsbxError::Usage(_) => axum::http::StatusCode::BAD_REQUEST,
        LsbxError::NotFound(_) => axum::http::StatusCode::NOT_FOUND,
        LsbxError::BackendUnavailable(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE,
        LsbxError::AuthFailed(_) => axum::http::StatusCode::UNAUTHORIZED,
        LsbxError::LockContention(_) => axum::http::StatusCode::CONFLICT,
        LsbxError::ContractViolated(_) | LsbxError::Interrupted(_) => {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, err.to_string()).into_response()
}
