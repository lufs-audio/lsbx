//! `Authorization: Bearer <token>` / `X-Api-Key: <token>` auth extractor
//! (Unit 13's `AuthedRequest`).
//!
//! Compares the presented token against a single configured token
//! (`GatewayConfig.token`), matching the existing gateway's behavior
//! exactly: one shared secret, no per-caller token registry. `GET
//! /console` is the sole route this extractor is never applied to (per the
//! unit contract's acceptance criteria); every other route — including
//! `GET /health` — requires a valid token.
//!
//! This module owns *authentication* only (is the presented token valid).
//! Rate limiting (Unit 13's other new piece, `ratelimit.rs`) is a separate
//! `axum` middleware layer applied around the whole router, not folded
//! into this extractor, so the two concerns stay independently testable —
//! the rate-limit test suite needs to exercise `429` behavior for both an
//! authenticated caller (keyed by token) and the one unauthenticated route
//! (keyed by source IP), and folding rate-limit state into an auth
//! extractor would make the unauthenticated case impossible to key by
//! token at all.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Marker extractor: presence in a handler's argument list means "this
/// route requires a valid bearer token/API key," enforced by
/// `FromRequestParts::from_request_parts` below. Carries the validated
/// token forward (needed by the rate limiter, which keys on it) rather
/// than discarding it once validated.
#[derive(Debug, Clone)]
pub struct AuthedRequest {
    /// The exact token string that was presented and validated. The
    /// rate-limit middleware (`ratelimit.rs`) re-derives this same value
    /// independently from the raw headers (middleware runs outside any
    /// individual handler's extractor set), but this field lets a handler
    /// itself inspect which token was used without re-parsing headers.
    pub token: String,
}

/// Why a request failed authentication — surfaced only as a generic 401 in
/// the HTTP response (never echoing back *why*, to avoid handing a caller
/// a token-guessing oracle), but kept distinct internally for tests and
/// tracing.
#[derive(Debug)]
pub enum AuthError {
    MissingCredentials,
    InvalidToken,
    /// The gateway has no token configured at all. A gateway with
    /// `GatewayConfig.token: None` cannot authenticate anyone — every
    /// authenticated route fails closed rather than silently accepting
    /// any presented credential (or, worse, none at all). This is the
    /// same fail-closed posture `run_server`'s bind check enforces at
    /// startup, applied per-request as a second, independent layer.
    NoTokenConfigured,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        // Deliberately identical body/status for every variant — the
        // distinction above exists for this crate's own tests/tracing,
        // not for the wire. A caller who guesses wrong learns nothing
        // about *why* beyond "unauthorized".
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

/// Extracts and validates a bearer token/API key from either
/// `Authorization: Bearer <token>` or `X-Api-Key: <token>`, per this unit's
/// acceptance criteria ("Auth: `Authorization: Bearer <token>` or
/// `X-Api-Key` header, compared against the configured token").
/// `Authorization` is checked first; `X-Api-Key` is only consulted if
/// `Authorization` is absent or not a `Bearer` scheme — a request is never
/// required to present both.
///
/// Takes a bare `&HeaderMap` rather than a full `&Parts` (axum's `Parts`
/// has private fields and cannot be constructed outside a real request,
/// which would make this fn untestable/unusable from the rate-limit
/// middleware in `routes.rs` — that middleware runs before any handler's
/// own extractors and only has a `HeaderMap`, not a `Parts`, readily in
/// hand at the point it needs this same header-parsing logic).
pub fn extract_presented_token(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    headers
        .get("X-Api-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// axum 0.8's `FromRequestParts` uses a native `async fn` in the trait
// (not `#[async_trait]`) with a `Send`-bounded return future — implementing
// it via `#[async_trait]` here mismatches the trait's real desugared
// signature at the lifetime level (confirmed by the compiler: "lifetimes
// do not match associated function in trait"). Implemented directly
// against the real trait shape instead.
impl<S> FromRequestParts<S> for AuthedRequest
where
    S: crate::routes::HasGatewayConfig + Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let configured_token = state
            .gateway_config()
            .token
            .as_deref()
            .ok_or(AuthError::NoTokenConfigured)?;

        let presented =
            extract_presented_token(&parts.headers).ok_or(AuthError::MissingCredentials)?;

        // Constant-time-ish comparison isn't attempted here deliberately:
        // this mirrors the existing Python gateway's plain string
        // comparison exactly (no new timing-side-channel hardening was
        // named in the unit contract's acceptance criteria, and adding one
        // silently would be exactly the kind of behavior-widening change
        // this rewrite's own SPEC.md warns against making without saying
        // so explicitly).
        if presented == configured_token {
            Ok(AuthedRequest { token: presented })
        } else {
            Err(AuthError::InvalidToken)
        }
    }
}
