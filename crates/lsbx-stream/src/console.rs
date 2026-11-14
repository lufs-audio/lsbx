//! noVNC console page + console detail.
//!
//! ## The console page is served by the gateway, same-origin (issue #55)
//! This module used to render an `<iframe>` of the sandbox's `console_url`, which
//! on the libvirt backend is the guest's private `http://192.168.122.x:8000/…`.
//! No browser outside the hypervisor host can load that, so the page was dead
//! off-host — and before that it was an explicit placeholder, because the real
//! noVNC client was never in the repository.
//!
//! It now renders a real client that loads the vendored bundle from
//! [`crate::assets`] and opens a WebSocket to `/stream/<id>/vnc` **on its own
//! origin**. Consequences worth keeping in mind:
//! - the guest IP never reaches the browser;
//! - the same page works over the tailnet, over the public HTTPS vhost, and over
//!   any future transport door, unchanged;
//! - the browser authenticates with a short-lived, sandbox-scoped ticket minted
//!   by the gateway, because a WebSocket handshake cannot carry an
//!   `Authorization` header (canonical-url spec D7). The master token stays in
//!   the browser's localStorage and is only ever sent as a header to the ticket
//!   endpoint — never in a URL.
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

/// The console page template, embedded at build time. It is a template rather
/// than a finished document because three values are per-request: the sandbox
/// id, the vendored noVNC version (for diagnostics), and the resize mode.
/// Substitution is a plain `str::replace` per token — deliberately not
/// `format!`, whose brace escaping is a poor fit for a file full of CSS.
const CONSOLE_PAGE_TEMPLATE: &str = include_str!("../assets/console.html");

/// Vendored noVNC release, surfaced in the served page's `generator` meta tag so
/// a running deployment can be identified without reading the binary.
const NOVNC_BUNDLE_VERSION: &str = "1.5.0";

#[derive(Deserialize, Default)]
pub struct ConsoleParams {
    pub target: Option<String>,
    pub id: Option<String>,
    /// `scale` (default) or `remote`. `remote` asks the RFB server to resize the
    /// guest to the viewport, which needs a server that supports it — see #56.
    pub resize: Option<String>,
}

/// `GET /console?target=<sandbox-id>` — the browser console page.
///
/// No guest address appears in the response: the page loads the vendored noVNC
/// client from this origin and opens a WebSocket to `/stream/<id>/vnc`. An id
/// that does not resolve is a `404`, matching every other door's treatment of an
/// unknown sandbox (and preventing the page from advertising that a console
/// exists).
pub async fn console_page_handler(
    ops: Option<&lsbx_ops::LsbxOps>,
    Query(params): Query<ConsoleParams>,
) -> Response {
    let Some(ops) = ops else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "console unavailable: this router was built without an LsbxOps",
        )
            .into_response();
    };

    let target = params.target.as_deref().or(params.id.as_deref());
    let Some(target) = target else {
        return html_response(render_index_page(ops).await);
    };

    // Resolve first: an unknown id must 404 rather than render a page whose
    // WebSocket can only ever fail.
    match ops.info(target).await {
        Ok(sandbox) => {
            let resize = match params.resize.as_deref() {
                Some("remote") => "remote",
                _ => "scale",
            };
            html_response(render_console_page(&sandbox.id, resize))
        }
        Err(LsbxError::NotFound(message)) => (
            axum::http::StatusCode::NOT_FOUND,
            format!("no such sandbox: {message}"),
        )
            .into_response(),
        Err(err) => error_response(&err),
    }
}

fn html_response(body: String) -> Response {
    let mut response = Html(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Renders the per-sandbox console page.
///
/// The only substitutions are the sandbox id, the vendored noVNC version and the
/// resize mode. The id is HTML-escaped because it is interpolated into both a
/// title and a JS string literal; today ids are `sbx-<hex>-<hex>`, but escaping
/// costs nothing and keeps the template honest if that ever loosens.
fn render_console_page(sandbox_id: &str, resize_mode: &str) -> String {
    CONSOLE_PAGE_TEMPLATE
        .replace("%SANDBOX_ID%", &html_escape(sandbox_id))
        .replace("%NOVNC_VERSION%", NOVNC_BUNDLE_VERSION)
        .replace("%RESIZE_MODE%", resize_mode)
}

/// Bare index: no `target`, so list what can be opened instead of erroring.
/// Deliberately minimal and link-only — the full dashboard lives in `lsbx-tui`
/// and in the gateway's JSON API.
async fn render_index_page(ops: &LsbxOps) -> String {
    let mut rows = String::new();
    match ops.list().await {
        Ok(sandboxes) if sandboxes.is_empty() => {
            rows.push_str("<li>No sandboxes are running. Start one with <code>lsbx up &lt;profile&gt;</code>.</li>");
        }
        Ok(sandboxes) => {
            for sandbox in sandboxes {
                if sandbox.console_url.is_none() {
                    continue;
                }
                let id = html_escape(&sandbox.id);
                rows.push_str(&format!(
                    "<li><a href=\"/console?target={id}\">{id}</a> <span class=dim>{}</span></li>",
                    html_escape(&sandbox.profile)
                ));
            }
            if rows.is_empty() {
                rows.push_str("<li>No running sandbox has a console.</li>");
            }
        }
        Err(err) => rows.push_str(&format!(
            "<li class=err>Could not list sandboxes: {}</li>",
            html_escape(&err.to_string())
        )),
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>lsbx consoles</title>\
         <meta name=\"generator\" content=\"lsbx console · noVNC {version}\">\
         <style>body{{margin:0;background:#101418;color:#e8eef5;font:14px system-ui,sans-serif;padding:24px}}\
         a{{color:#58a6ff}}code{{background:#0b0f14;padding:1px 5px;border-radius:4px}}\
         .dim{{opacity:.6}}.err{{color:#fca5a5}}li{{margin:6px 0}}</style></head>\
         <body><h1 style=\"font-size:16px\">lsbx consoles</h1><ul>{rows}</ul></body></html>",
        version = NOVNC_BUNDLE_VERSION
    )
}

/// Minimal HTML-entity escaping for the values interpolated into the pages above.
/// Same narrow, deliberate scope as the gateway's own helper: the five characters
/// that can break out of HTML text or an attribute.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
