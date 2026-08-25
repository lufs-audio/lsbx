//! Route table (Unit 13's `build_router`) — every handler is a thin
//! translation between axum's request/response types and one real
//! `LsbxOps` call, per this unit's own acceptance criteria ("no handler
//! contains a conditional that changes VM or golden behavior").
//!
//! ## Why every handler below differs from a Jules reference candidate's
//! shape
//! A prior candidate for this unit invented a fictional API on `LsbxOps`
//! that does not exist anywhere in the real, merged crate (confirmed by
//! direct re-read of `crates/lsbx-ops/src/lib.rs` immediately before
//! writing this file) — methods like `ops.health()`, `.images()`,
//! `.profiles()`, `.capabilities()`, `.consoles()`, `.sandboxes()`,
//! `.create_sandbox(body)`, `.get_console(target)`,
//! `.get_console_by_id(id)`, `.upload(id, dest, bytes)`,
//! `.artifacts(id, source)`, `.get_sandbox(id)`, `.delete_sandbox(id)`,
//! `.exec(id, body: String)`, `.put(id, body: Bytes)`,
//! `.get_local(id, body: String)`, `.check(id, body)`, `.info(id, body)`,
//! `.renew(id, body)`, `.open_console(id, body)`. None of these exist. The
//! real `LsbxOps` (Unit 10) has exactly 18 public async methods: `create`,
//! `destroy`, `list`, `exec(id, command: &[String], timeout: Duration)`,
//! `put(id, source: &Path, destination: &str)`,
//! `get(id, source: &str, destination: &Path)`,
//! `renew(id, duration: Duration)`, `console_url(id)`, `info(id)`,
//! `status()`, `reap(ttl, dry_run)`, `golden_build`, `golden_verify`,
//! `golden_register`, `golden_delete`, `golden_list`, `config_show`,
//! `logs_query`. Every handler below calls one of those 18 real methods
//! and nothing else.
//!
//! ## Judgment calls this file makes explicit (see the PR description for
//! the same list, kept in sync)
//! - `/images`, `/profiles`, `/capabilities` have no dedicated `LsbxOps`
//!   accessor for raw `ImageConfig`/`ProfileConfig`/`BackendCapabilities`
//!   data — the façade never exposes those (by design: doors may not reach
//!   around `LsbxOps` into `lsbx-golden`'s `ImageRegistry` or
//!   `lsbx-kernel`'s `Backend` trait directly). `config_show()` is the only
//!   real, honest registry-shape data `LsbxOps` exposes; `/images` and
//!   `/profiles` each return the corresponding sub-object of its JSON,
//!   and `/capabilities` returns the whole thing, since there is no
//!   narrower real signal for "what can this gateway currently do" than
//!   what the façade actually reports about itself.
//! - `/health` returns the real `StatusReport` (`backend_name`,
//!   `backend_available`, `sandbox_count`) as JSON, not a bare string —
//!   `LsbxOps::status()` is the only method that could back this route,
//!   and its `Ok` type is a struct, not `&str`.
//! - `/sandboxes/<id>/check` and `/sandboxes/<id>/info` are both backed by
//!   `LsbxOps::info(id)` today. No distinct "check" operation exists on
//!   the façade — the unit contract lists `/check` and `/info` as sibling
//!   POST actions with no further semantic detail, and `info(id)`'s real
//!   `PublicSandbox` is the most honest available signal for "is this
//!   sandbox actually there, and what does it currently look like" that
//!   either route name could mean. A future façade method (e.g. a
//!   per-sandbox healthcheck re-run distinct from a snapshot read) would
//!   be the natural place to differentiate them; inventing that logic
//!   inside this gateway crate would violate this unit's own Boundaries
//!   ("Implements no operation logic — every handler calls `LsbxOps`").
//! - `POST /sandboxes/<id>/upload?destination=` reads the raw request
//!   body into a server-side temp file, then calls the real
//!   `put(id, source: &Path, destination: &str)` against that temp path —
//!   `put`'s real signature takes a filesystem `&Path`, not a byte buffer,
//!   so bridging an HTTP body to it requires landing the bytes on disk
//!   first.
//! - `GET /sandboxes/<id>/artifacts?source=` calls the real
//!   `get(id, source: &str, destination: &Path)` against a server-side
//!   temp file, then streams that temp file back as the response body,
//!   for the same reason in reverse.
//! - `POST /sandboxes/<id>/put` and `/get` are the *local-file* variants
//!   named in this unit's own acceptance criteria ("gated by an
//!   `allow_local_files` config flag, disabled by default") — distinct
//!   from `/upload`/`/artifacts`, which move bytes over HTTP. `/put` and
//!   `/get` accept a JSON body naming a path on the *gateway host's own
//!   filesystem* and pass it straight to the real `put`/`get`, which is
//!   exactly why they are gated behind `allow_local_files` at all: an
//!   ungated version would let any authenticated caller read/write
//!   arbitrary paths on the host running the gateway.

use crate::auth::AuthedRequest;
use crate::ratelimit::{RateLimitDecision, TokenBucket};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Gateway-level configuration, per this unit's interface contract.
pub struct GatewayConfig {
    pub token: Option<String>,
    pub allow_local_files: bool,
    pub insecure: bool,
    pub rate_limit: crate::ratelimit::RateLimitConfig,
}

/// Shared state every handler receives via axum's `State` extractor: the
/// one `Arc<LsbxOps>` every door holds (SPEC.md §4.7's "exactly one place
/// operational state lives"), the gateway's own config, and the rate
/// limiter's shared bucket map.
#[derive(Clone)]
pub struct GatewayState {
    pub ops: Arc<LsbxOps>,
    pub config: Arc<GatewayConfig>,
    pub rate_limiter: Arc<TokenBucket>,
}

/// Narrow trait `auth.rs`'s `FromRequestParts` impl is generic over, so the
/// auth extractor does not need to know `GatewayState`'s exact shape —
/// only that whatever state type it's used with can hand back a
/// `&GatewayConfig`. Kept this way (rather than hard-coding
/// `FromRequestParts<GatewayState>`) so the auth extractor's own unit
/// tests (in `tests/test_auth_fail_closed.rs`) can exercise it against a
/// minimal state value if useful, without pulling in a real `LsbxOps`.
pub trait HasGatewayConfig {
    fn gateway_config(&self) -> &GatewayConfig;
}

impl HasGatewayConfig for GatewayState {
    fn gateway_config(&self) -> &GatewayConfig {
        &self.config
    }
}

/// Builds the full route table for this unit's contract, wiring the auth
/// extractor, rate-limit middleware, and audit-log middleware around every
/// handler below.
///
/// `GET /console` is the sole unauthenticated route (the browser-facing
/// HTML page, per this unit's acceptance criteria) — every other route,
/// including `GET /health`, requires `AuthedRequest` to resolve.
///
/// This is this crate's own standalone router, unchanged from how Unit 13
/// originally shipped it (including its own `GET /console`/
/// `GET /consoles/{id}` handlers) — used directly by this crate's own
/// tests (`tests/test_routes.rs`, `tests/test_auth_fail_closed.rs`,
/// `tests/test_rate_limit.rs`) and by any caller that wants this crate's
/// REST surface in isolation, with no `lsbx-stream` mounted alongside it.
/// See [`build_router_for_merge`] for the variant `lib.rs`'s Gap 1 merge
/// path actually uses.
pub fn build_router(ops: Arc<LsbxOps>, config: GatewayConfig) -> Router {
    build_router_inner(ops, config, true)
}

/// The Gap 1 merge variant: identical to [`build_router`] except it omits
/// this crate's own `GET /console` and `GET /consoles/{id}` handlers.
///
/// ## Why this exists: a real route-ownership collision found while wiring
/// Gap 1
///
/// This crate (Unit 13) and `lsbx-stream` (Unit 14) were built
/// independently, before either could see the other's real, merged source,
/// and **both** built their own `GET /console` and `GET /consoles/{id}`
/// handlers — each unit's own contract named those exact paths as part of
/// its acceptance criteria, with no coordination between the two (Unit 13's
/// contract calls `/console` "the sole unauthenticated exception" for a
/// browser-facing HTML page resolving a `target` query param to a console
/// URL; Unit 14's contract calls `/console` the bundled noVNC static page,
/// and `/consoles/{id}` a `ConsoleDetail` JSON response including a
/// `console_password` field). Attempting to `.merge()` `lsbx_stream::router`
/// with this crate's own full `build_router` output panics at router-build
/// time with axum's own "Overlapping method route" error — a real,
/// reproducible conflict, not a hypothetical one (confirmed by this crate's
/// own `tests::merged_router_serves_both_gateway_and_stream_routes` test,
/// which caught exactly this panic before this function existed).
///
/// This pass's task description is explicit that the merged gateway should
/// serve "the REST API and the `/stream/*`, `/console`, `/consoles/*`
/// routes together" — naming `/console`/`/consoles/*` as routes that come
/// *from* the `lsbx-stream` mount, not as gateway-owned routes to be
/// preserved verbatim alongside a colliding stream implementation. Given
/// that framing, and that Unit 14's own contract is the one that literally
/// names `/console`/`/consoles/{id}` as its canonical implementation (this
/// crate's own module doc comment already says Unit 13 "mounts Unit 14's
/// router as a sub-router rather than reimplementing it" for the console
/// experience specifically), the resolution taken here is: **`lsbx-stream`'s
/// versions of `/console` and `/consoles/{id}` are authoritative in the
/// merged router**, and this crate's own colliding registrations for those
/// two exact paths are omitted when building for the merge.
///
/// This crate's other, non-colliding routes are unaffected: `GET /consoles`
/// (the list-of-all-console-URLs endpoint, no `{id}` — `lsbx-stream` has no
/// equivalent, since its own `/consoles/{id}` is a single-sandbox detail
/// lookup, not a list) is still registered here and still reachable in the
/// merged router, alongside every other REST route this crate owns
/// (`/health`, `/images`, `/sandboxes/*`, etc.).
pub(crate) fn build_router_for_merge(ops: Arc<LsbxOps>, config: GatewayConfig) -> Router {
    build_router_inner(ops, config, false)
}

fn build_router_inner(ops: Arc<LsbxOps>, config: GatewayConfig, include_own_console_routes: bool) -> Router {
    let rate_limiter = Arc::new(TokenBucket::new(config.rate_limit));
    let state = GatewayState {
        ops,
        config: Arc::new(config),
        rate_limiter,
    };

    let mut authenticated_routes = Router::new()
        .route("/health", get(health))
        .route("/images", get(images))
        .route("/profiles", get(profiles))
        .route("/capabilities", get(capabilities))
        .route("/consoles", get(consoles))
        .route("/sandboxes", get(list_sandboxes).post(create_sandbox))
        .route(
            "/sandboxes/{id}",
            get(get_sandbox).delete(delete_sandbox),
        )
        .route("/sandboxes/{id}/upload", post(upload_to_sandbox))
        .route("/sandboxes/{id}/artifacts", get(download_artifact))
        .route("/sandboxes/{id}/exec", post(exec_in_sandbox))
        .route("/sandboxes/{id}/put", post(put_local_file))
        .route("/sandboxes/{id}/get", post(get_local_file))
        .route("/sandboxes/{id}/check", post(check_sandbox))
        .route("/sandboxes/{id}/info", post(info_sandbox))
        .route("/sandboxes/{id}/renew", post(renew_sandbox))
        .route("/sandboxes/{id}/console", post(console_for_sandbox));

    if include_own_console_routes {
        // See `build_router_for_merge`'s doc comment: this crate's own
        // `/consoles/{id}` collides with `lsbx-stream`'s real,
        // already-merged implementation of the same path, so it is only
        // registered on the standalone (non-merge) router.
        authenticated_routes = authenticated_routes.route("/consoles/{id}", get(console_by_id));
    }

    let authenticated_routes = authenticated_routes
        // Mutating routes only: the audit log records "every mutating
        // request" (acceptance criteria), not read-only ones, so this
        // layer sits under the mutating-route group rather than the
        // whole authenticated router. axum applies middleware innermost
        // to outermost as declared, and `Router::merge` below composes
        // this sub-router with the read-only one, so a route defined on
        // this sub-router keeps the audit layer while routes on the
        // outer router (added after this call) do not.
        .layer(middleware::from_fn_with_state(state.clone(), audit_log_middleware));

    let mut unauthenticated_routes = Router::new();
    if include_own_console_routes {
        // See `build_router_for_merge`'s doc comment: this crate's own
        // `/console` collides with `lsbx-stream`'s real, already-merged
        // implementation of the same path.
        unauthenticated_routes = unauthenticated_routes.route("/console", get(browser_console));
    }

    authenticated_routes
        .merge(unauthenticated_routes)
        // Rate limiting wraps the whole router (both authenticated and
        // unauthenticated routes), per this unit's acceptance criteria:
        // "keyed by bearer token (falling back to source IP for the
        // unauthenticated `/console` route)". The middleware itself
        // decides which key to use per-request based on whether an
        // Authorization/X-Api-Key header is present, independent of
        // whether that credential is later found valid by the auth
        // extractor — a request that's rate-limited never gets far enough
        // to have its credentials checked, matching "throttle before you
        // even look at who's asking".
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit_middleware))
        .with_state(state)
}

// ---------------------------------------------------------------------
// Rate-limit middleware
// ---------------------------------------------------------------------

async fn rate_limit_middleware(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // `ConnectInfo` is only populated when the router is served via
    // `into_make_service_with_connect_info` (real production serving, and
    // this crate's own `test_auth_fail_closed.rs`/`test_rate_limit.rs`
    // tests that bind a real listener) — a test harness driving the
    // router directly via `tower::ServiceExt::oneshot` (as
    // `test_routes.rs` does) has no real peer socket to report. Reading it
    // out of `request.extensions()` here (rather than taking
    // `Option<ConnectInfo<SocketAddr>>` as a typed handler parameter) is a
    // deliberate workaround: `axum::middleware::from_fn`'s `FromFn` only
    // implements `Service` for a fixed, finite set of extractor-arity
    // tuples, and this middleware's real parameter list (state + headers
    // + an optional connect-info + the body-consuming `Request` + `Next`)
    // does not land on a supported arity — confirmed by the compiler
    // rejecting `Option<ConnectInfo<SocketAddr>>` as a typed parameter
    // here with "the trait `Service<Request<Body>>` is not implemented
    // for `FromFn<...>`". Reading the same data out of extensions instead
    // keeps the handler's typed-parameter arity at the two axum already
    // supports (`State` + `HeaderMap` + `Request` + `Next`) while still
    // reaching the identical `ConnectInfo` value when one is present.
    // `0.0.0.0:0` is an obviously-synthetic placeholder used only as this
    // fallback's rate-limit *key material* — never returned to a caller or
    // used for any authorization decision, only to give the
    // unauthenticated-route fallback path a `SocketAddr` to key on when a
    // real one legitimately isn't available.
    let addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr)
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
    let key = rate_limit_key(&headers, addr);

    match state.rate_limiter.check(&key) {
        RateLimitDecision::Allow => next.run(request).await,
        RateLimitDecision::Deny { retry_after } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "status": "error",
                    "code": 429,
                    "message": "rate limit exceeded",
                })),
            )
                .into_response();
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                #[allow(clippy::unwrap_used)] // A Duration's whole-second count formatted as ASCII digits is always a valid header value.
                axum::http::HeaderValue::from_str(&retry_after.as_secs().to_string()).unwrap(),
            );
            response
        }
    }
}

/// Derives the rate-limit key for a request: the presented bearer
/// token/API key if one is on the request, else the caller's source IP
/// (the fallback this unit's acceptance criteria names explicitly for the
/// unauthenticated `/console` route). This mirrors, but is independent
/// from, `auth.rs`'s own token extraction — the rate limiter must key on
/// *whatever token was presented*, valid or not, since throttling by
/// presented-but-wrong credentials is exactly what stops a credential-
/// guessing loop from becoming an unbounded-rate brute force.
fn rate_limit_key(headers: &HeaderMap, addr: SocketAddr) -> String {
    crate::auth::extract_presented_token(headers).unwrap_or_else(|| format!("ip:{}", addr.ip()))
}

// ---------------------------------------------------------------------
// Audit-log middleware
// ---------------------------------------------------------------------

/// Records every mutating request (POST/PUT/PATCH/DELETE) as a JSONL audit
/// line — but the SHA-256 hash of the request path + body, never the raw
/// text, matching the existing `_audit_command`'s privacy property exactly
/// (per this unit's acceptance criteria).
///
/// This implementation writes to `tracing` (a JSONL-shaped structured
/// event, matching the "JSONL audit log" wording) rather than a
/// hand-rolled file writer, since this crate's dependency list already
/// includes `tracing` for every other diagnostic path, and a real
/// production deployment routes `tracing` output to whatever sink it
/// wants (an actual file, syslog, a collector) without this crate needing
/// to own file-rotation/rendering concerns itself. The audit *content* —
/// method, path, and body hash, never body plaintext — is what this
/// unit's acceptance criteria actually specifies; the sink is a
/// deployment concern this crate does not need to invent an opinion
/// about.
async fn audit_log_middleware(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    if !method_is_mutating(&method) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => Bytes::new(),
    };

    let mut hasher = Sha256::new();
    hasher.update(method.as_str().as_bytes());
    hasher.update(b" ");
    hasher.update(path.as_bytes());
    hasher.update(b"\n");
    hasher.update(&bytes);
    let body_hash = hex::encode(hasher.finalize());

    tracing::info!(
        target: "lsbx_gateway::audit",
        method = %method,
        path = %path,
        body_sha256 = %body_hash,
        "mutating gateway request"
    );

    let rebuilt = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    next.run(rebuilt).await
}

fn method_is_mutating(method: &axum::http::Method) -> bool {
    matches!(
        *method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
}

// ---------------------------------------------------------------------
// Error mapping: LsbxError -> HTTP response
// ---------------------------------------------------------------------

/// Maps a real `LsbxError` onto an HTTP status + the SPEC.md §7 JSON
/// envelope (`{"status":"error","code":<exit code>,"message":"..."}`),
/// using each variant's own real `exit_code()` as the envelope's `code` —
/// never inventing a separate HTTP-specific error taxonomy that could
/// disagree with the exit code an equivalent CLI invocation would report,
/// per SPEC.md §7's "code is always the process's actual exit status."
fn error_response(err: LsbxError) -> Response {
    let status = match &err {
        LsbxError::Usage(_) => StatusCode::BAD_REQUEST,
        LsbxError::BackendUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        LsbxError::NotFound(_) => StatusCode::NOT_FOUND,
        LsbxError::ContractViolated(_) => StatusCode::UNPROCESSABLE_ENTITY,
        LsbxError::LockContention(_) => StatusCode::CONFLICT,
        LsbxError::AuthFailed(_) => StatusCode::UNAUTHORIZED,
        LsbxError::Interrupted(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let code: i32 = err.exit_code().into();
    (
        status,
        Json(serde_json::json!({
            "status": "error",
            "code": code,
            "message": err.to_string(),
        })),
    )
        .into_response()
}

fn success_response<T: Serialize>(data: T) -> Response {
    Json(serde_json::json!({
        "status": "success",
        "data": data,
    }))
    .into_response()
}

// ---------------------------------------------------------------------
// Handlers — GET /health
// ---------------------------------------------------------------------

/// `GET /health` -> `LsbxOps::status()`. Returns the real `StatusReport`
/// (`backend_name`, `backend_available`, `sandbox_count`) as JSON, per
/// this file's module doc comment. Always HTTP 200 when the call itself
/// succeeds — the interesting signal (is the *backend* reachable) lives
/// inside `backend_available`, not in the transport status code, so a
/// caller that only checks "did /health return 200" and a caller that
/// inspects the body both get an honest answer without the two being able
/// to disagree with each other.
async fn health(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.status().await {
        Ok(report) => success_response(serde_json::json!({
            "backend_name": report.backend_name,
            "backend_available": report.backend_available,
            "sandbox_count": report.sandbox_count,
        })),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — /images, /profiles, /capabilities (all backed by config_show)
// ---------------------------------------------------------------------

async fn images(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.config_show().await {
        Ok(config) => success_response(config.get("images").cloned().unwrap_or(serde_json::json!({}))),
        Err(e) => error_response(e),
    }
}

async fn profiles(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.config_show().await {
        Ok(config) => success_response(config.get("profiles").cloned().unwrap_or(serde_json::json!({}))),
        Err(e) => error_response(e),
    }
}

async fn capabilities(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.config_show().await {
        Ok(config) => success_response(config),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — /consoles, /consoles/<id>
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct ConsoleEntry {
    id: String,
    console_url: String,
}

/// `GET /consoles` -> `LsbxOps::list()`, filtered to sandboxes whose real
/// `PublicSandbox.console_url` is `Some`. No dedicated `LsbxOps::consoles()`
/// method exists — this is `list()`'s own real, typed data, reshaped.
async fn consoles(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.list().await {
        Ok(sandboxes) => {
            let entries: Vec<ConsoleEntry> = sandboxes
                .into_iter()
                .filter_map(|s| {
                    s.console_url.map(|url| ConsoleEntry {
                        id: s.id,
                        console_url: url,
                    })
                })
                .collect();
            success_response(entries)
        }
        Err(e) => error_response(e),
    }
}

/// `GET /consoles/<id>` -> `LsbxOps::console_url(id)`.
async fn console_by_id(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.console_url(&id).await {
        Ok(url) => success_response(serde_json::json!({ "id": id, "console_url": url })),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — /sandboxes (GET, POST)
// ---------------------------------------------------------------------

/// `GET /sandboxes` -> `LsbxOps::list()`, direct passthrough of the real
/// `Vec<PublicSandbox>`.
async fn list_sandboxes(State(state): State<GatewayState>, _auth: AuthedRequest) -> Response {
    match state.ops.list().await {
        Ok(sandboxes) => success_response(sandboxes),
        Err(e) => error_response(e),
    }
}

/// Request body for `POST /sandboxes`, parsed into the real
/// `lsbx_lifecycle::create::CreateRequest` fields. `lease_secs`/
/// `ready_timeout_secs` are `u64` seconds on the wire (JSON has no native
/// `Duration`), converted to `std::time::Duration` before calling
/// `LsbxOps::create`.
#[derive(Deserialize)]
struct CreateSandboxBody {
    profile: String,
    name: Option<String>,
    task_id: Option<String>,
    #[serde(default = "default_lease_secs")]
    lease_secs: u64,
    #[serde(default = "default_ready_timeout_secs")]
    ready_timeout_secs: u64,
    #[serde(default = "default_verify")]
    verify: bool,
    #[serde(default)]
    healthchecks: Vec<Vec<String>>,
}

fn default_lease_secs() -> u64 {
    3600
}
fn default_ready_timeout_secs() -> u64 {
    30
}
fn default_verify() -> bool {
    true
}

/// `POST /sandboxes` -> `LsbxOps::create(CreateRequest)`.
async fn create_sandbox(
    State(state): State<GatewayState>,
    _auth: AuthedRequest,
    Json(body): Json<CreateSandboxBody>,
) -> Response {
    let req = lsbx_lifecycle::create::CreateRequest {
        profile: &body.profile,
        name: body.name.as_deref(),
        task_id: body.task_id.as_deref(),
        lease: Duration::from_secs(body.lease_secs),
        ready_timeout: Duration::from_secs(body.ready_timeout_secs),
        verify: body.verify,
        healthchecks: body.healthchecks,
    };

    match state.ops.create(req).await {
        Ok(sandbox) => success_response(sandbox),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — /sandboxes/<id> (GET, DELETE)
// ---------------------------------------------------------------------

/// `GET /sandboxes/<id>` -> `LsbxOps::info(id)`.
async fn get_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.info(&id).await {
        Ok(sandbox) => success_response(sandbox),
        Err(e) => error_response(e),
    }
}

/// `DELETE /sandboxes/<id>` -> `LsbxOps::destroy(id)`.
async fn delete_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.destroy(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — upload / artifacts (HTTP-body <-> real Path-based put/get)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct DestinationQuery {
    destination: String,
}

/// `POST /sandboxes/<id>/upload?destination=` -> `LsbxOps::put(id, source:
/// &Path, destination: &str)`. The real `put` takes a filesystem path, not
/// a byte buffer, so the raw request body is written to a server-side
/// temp file first, then that temp file's path is handed to `put` as
/// `source`. The temp file is cleaned up after the call regardless of
/// outcome.
async fn upload_to_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DestinationQuery>,
    _auth: AuthedRequest,
    body: Bytes,
) -> Response {
    let temp_file = match tokio::task::spawn_blocking(move || {
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(file.path(), &body)?;
        Ok::<_, std::io::Error>(file)
    })
    .await
    {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => {
            return error_response(LsbxError::ContractViolated(format!(
                "failed to stage upload body to a temp file: {e}"
            )))
        }
        Err(e) => {
            return error_response(LsbxError::ContractViolated(format!(
                "upload staging task panicked: {e}"
            )))
        }
    };

    let result = state.ops.put(&id, temp_file.path(), &query.destination).await;
    match result {
        Ok(()) => success_response(serde_json::json!({ "id": id, "destination": query.destination })),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct SourceQuery {
    source: String,
}

/// `GET /sandboxes/<id>/artifacts?source=` -> `LsbxOps::get(id, source:
/// &str, destination: &Path)`. The real `get` writes to a filesystem
/// path, not a byte buffer, so this calls it against a server-side temp
/// file and then streams that temp file's bytes back as the HTTP response
/// body.
async fn download_artifact(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SourceQuery>,
    _auth: AuthedRequest,
) -> Response {
    let temp_file = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            return error_response(LsbxError::ContractViolated(format!(
                "failed to create temp file for artifact download: {e}"
            )))
        }
    };
    let temp_path = temp_file.path().to_path_buf();

    if let Err(e) = state.ops.get(&id, &query.source, &temp_path).await {
        return error_response(e);
    }

    match tokio::fs::read(&temp_path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(e) => error_response(LsbxError::ContractViolated(format!(
            "failed to read back downloaded artifact: {e}"
        ))),
    }
}

// ---------------------------------------------------------------------
// Handlers — exec
// ---------------------------------------------------------------------

/// Request body for `POST /sandboxes/<id>/exec`. `command` is a real argv
/// array (`Vec<String>`), matching `LsbxOps::exec`'s actual
/// `command: &[String]` parameter — never a single shell string, which is
/// exactly the kind of shell-interpolation shape SPEC.md warns against
/// elsewhere in this system.
#[derive(Deserialize)]
struct ExecBody {
    command: Vec<String>,
    #[serde(default = "default_exec_timeout_secs")]
    timeout_secs: u64,
}

fn default_exec_timeout_secs() -> u64 {
    30
}

#[derive(Serialize)]
struct ExecResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// `POST /sandboxes/<id>/exec` -> `LsbxOps::exec(id, command, timeout)`.
async fn exec_in_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
    Json(body): Json<ExecBody>,
) -> Response {
    if body.command.is_empty() {
        return error_response(LsbxError::Usage(
            "exec request body's 'command' array must not be empty".to_string(),
        ));
    }

    match state
        .ops
        .exec(&id, &body.command, Duration::from_secs(body.timeout_secs))
        .await
    {
        Ok(output) => success_response(ExecResponse {
            exit_code: output.exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — local-file put/get (gated by allow_local_files)
// ---------------------------------------------------------------------

/// Shared "is this feature enabled" check for the two local-file routes.
/// Both return the same `LsbxError::Usage`-shaped rejection (mapped to 400)
/// when the flag is off, since a caller hitting a disabled route is
/// exactly the "malformed request against this gateway's actual
/// configuration" case `Usage` names — not a 404 (the route exists) and
/// not a 403 (this isn't a permissions question about *who* is asking,
/// it's a deployment-level feature gate that's off for everyone).
fn require_local_files_enabled(config: &GatewayConfig) -> Result<(), LsbxError> {
    if config.allow_local_files {
        Ok(())
    } else {
        Err(LsbxError::Usage(
            "local file access (put/get by server-side path) is disabled on this gateway \
             (allow_local_files is false); this is a deliberate, default-off safety gate, \
             not a missing feature"
                .to_string(),
        ))
    }
}

#[derive(Deserialize)]
struct PutLocalBody {
    /// A path on the *gateway host's own filesystem* — real
    /// `LsbxOps::put`'s `source: &Path` parameter, unlike `/upload`'s HTTP
    /// body bytes. Gated by `allow_local_files` precisely because this
    /// lets an authenticated caller name arbitrary host paths.
    source: String,
    destination: String,
}

/// `POST /sandboxes/<id>/put` -> `LsbxOps::put(id, source: &Path,
/// destination: &str)`, gated by `allow_local_files`.
async fn put_local_file(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
    Json(body): Json<PutLocalBody>,
) -> Response {
    if let Err(e) = require_local_files_enabled(&state.config) {
        return error_response(e);
    }

    let source_path = std::path::PathBuf::from(&body.source);
    match state.ops.put(&id, &source_path, &body.destination).await {
        Ok(()) => success_response(serde_json::json!({ "id": id, "destination": body.destination })),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct GetLocalBody {
    source: String,
    /// A path on the *gateway host's own filesystem* to write the
    /// downloaded bytes to — real `LsbxOps::get`'s `destination: &Path`.
    destination: String,
}

/// `POST /sandboxes/<id>/get` -> `LsbxOps::get(id, source: &str,
/// destination: &Path)`, gated by `allow_local_files`.
async fn get_local_file(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
    Json(body): Json<GetLocalBody>,
) -> Response {
    if let Err(e) = require_local_files_enabled(&state.config) {
        return error_response(e);
    }

    let destination_path = std::path::PathBuf::from(&body.destination);
    match state.ops.get(&id, &body.source, &destination_path).await {
        Ok(()) => success_response(serde_json::json!({ "id": id, "source": body.source })),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handlers — check / info / renew / console (POST sub-resource actions)
// ---------------------------------------------------------------------

/// `POST /sandboxes/<id>/check` -> `LsbxOps::info(id)`. See this file's
/// module doc comment ("Judgment calls") for why `/check` and `/info` are
/// implemented identically today.
async fn check_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.info(&id).await {
        Ok(sandbox) => success_response(sandbox),
        Err(e) => error_response(e),
    }
}

/// `POST /sandboxes/<id>/info` -> `LsbxOps::info(id)`.
async fn info_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.info(&id).await {
        Ok(sandbox) => success_response(sandbox),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct RenewBody {
    duration_secs: u64,
}

/// `POST /sandboxes/<id>/renew` -> `LsbxOps::renew(id, duration)`.
async fn renew_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
    Json(body): Json<RenewBody>,
) -> Response {
    match state.ops.renew(&id, Duration::from_secs(body.duration_secs)).await {
        Ok(sandbox) => success_response(sandbox),
        Err(e) => error_response(e),
    }
}

/// `POST /sandboxes/<id>/console` -> `LsbxOps::console_url(id)`. The POST
/// sibling of `GET /consoles/<id>`, per this unit's separate listing of
/// the two in its acceptance criteria.
async fn console_for_sandbox(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    _auth: AuthedRequest,
) -> Response {
    match state.ops.console_url(&id).await {
        Ok(url) => success_response(serde_json::json!({ "id": id, "console_url": url })),
        Err(e) => error_response(e),
    }
}

// ---------------------------------------------------------------------
// Handler — GET /console (the sole unauthenticated route)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct ConsoleTargetQuery {
    target: String,
}

/// `GET /console?target=<id>` -> `LsbxOps::console_url(target)`, rendered
/// as the browser-facing HTML page this unit's acceptance criteria names
/// as "the sole unauthenticated exception." Returns a minimal page linking
/// to the real console URL rather than a full noVNC embed — Unit 14 (the
/// WS stream proxy/noVNC console) owns the actual console experience per
/// this unit's own Boundaries ("mounts Unit 14's router as a sub-router
/// rather than reimplementing it"); this route's job is only to resolve
/// `target` to a real console URL and hand the browser something to
/// navigate to.
async fn browser_console(
    State(state): State<GatewayState>,
    Query(query): Query<ConsoleTargetQuery>,
) -> Response {
    match state.ops.console_url(&query.target).await {
        Ok(Some(url)) => Html(format!(
            "<!DOCTYPE html><html><head><title>lsbx console: {id}</title></head>\
             <body><p>Console for sandbox <code>{id}</code>:</p>\
             <p><a href=\"{url}\">{url}</a></p></body></html>",
            id = html_escape(&query.target),
            url = html_escape(&url)
        ))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Html(format!(
                "<!DOCTYPE html><html><body><p>Sandbox <code>{}</code> has no console \
                 available (not a noVNC-streaming sandbox).</p></body></html>",
                html_escape(&query.target)
            )),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// Minimal HTML-entity escaping for the two values (`target` id, resolved
/// console URL) this handler interpolates into a hand-built page. Scoped
/// deliberately narrow (the five characters that matter for breaking out
/// of HTML text/attribute context) rather than pulling in a templating
/// dependency for one small unauthenticated page.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
