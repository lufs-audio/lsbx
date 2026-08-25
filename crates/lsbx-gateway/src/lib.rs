//! `lsbx-gateway` — Axum HTTP Gateway (Unit 13, Door 2 from SPEC.md §4.8).
//!
//! Every route handler is a thin translation into one real `LsbxOps` call
//! (`routes.rs`); the auth extractor (`auth.rs`) and rate-limit middleware
//! (`ratelimit.rs`) are the two pieces of real cross-cutting logic this
//! crate owns, per its own Boundaries ("Implements no operation logic —
//! every handler calls `LsbxOps`").
//!
//! ## `run_server`'s fail-closed bind behavior
//! The unit contract's acceptance criteria says: "Fail-closed bind:
//! refuses to bind a non-loopback host without both a configured token and
//! an explicit `--insecure` opt-in — never silently listens on `0.0.0.0`
//! unauthenticated." [`run_server`] enforces that check *before* binding
//! anything, and — once the check passes — actually binds a real
//! `tokio::net::TcpListener` and serves the router on it via
//! `axum::serve`, rather than returning `Ok(())` immediately after the
//! check as a stand-in for "would have bound." A gateway crate whose
//! "server" never actually listens on a socket is not implementing Door 2
//! of SPEC.md §4.8 — it is implementing the *check* for Door 2 and calling
//! that the whole unit.
//!
//! `run_server` returns the bound `axum::serve` future rather than
//! `.await`-ing it internally, so a caller (the `lsbx serve` CLI
//! subcommand — see Gap 1/Gap 3 note below — or this crate's own tests)
//! decides whether/how long to run it — blocking a test's whole async
//! runtime inside a library function with no cancellation handle would make
//! "does the fail-closed check actually prevent a bind" and "does a passing
//! check actually produce a live listener" impossible to test independently
//! within a bounded test timeout. The unit contract does not specify a
//! concrete return type for `run_server`, and the safer, more composable
//! choice — given every other unit in this system's own stated preference
//! for typed, awaitable results over ambient blocking (SPEC.md §1's "ran
//! vs. proven" framing applies here too: a function that blocks forever
//! internally can't ever report whether serving actually failed) — is to
//! hand back something awaitable rather than block the caller's `main`
//! directly. This is called out explicitly here (and in this crate's own
//! PR description) as the judgment call it is, not a silent deviation.
//!
//! ## Gap 1 (final integration wiring pass): mounting `lsbx-stream`'s router
//!
//! As merged, this crate's `build_router` produced a REST-only `Router`
//! with no `/stream/*`, `/console`, or `/consoles/*` routes — those live in
//! `lsbx-stream` (Unit 14), which was built and merged as an independent
//! sub-router with no caller wiring it in yet (confirmed by direct re-read
//! of `crates/lsbx-stream/src/lib.rs`'s own module doc comment at the time
//! of this pass: "Unit 13 has not yet landed on `main` — see this crate's
//! PR description for what that means for verifying that composition point
//! today"). [`GatewayDeps::build_router`] below closes that gap: it
//! constructs `lsbx_stream::StreamState { ops, store }` — a *second*,
//! independent `Arc<lsbx_store::sandbox_store::SandboxStore>` pointed at
//! the same `state_dir` the caller's own `LsbxOps` was built from, per
//! `lsbx-stream`'s own documented design (`SandboxStore` has no `Clone` —
//! confirmed against `crates/lsbx-store/src/sandbox_store.rs`, which
//! derives no `Clone` at all — so two owners of the same on-disk state each
//! get their own plain, cheap, synchronous-`fs`-backed instance rather than
//! sharing one object; this is the exact same pattern
//! `lsbx-ops`'s own `tests/test_all_operations.rs` uses for its reap test,
//! and the exact same pattern `lsbx-stream`'s own module doc comment
//! documents as intentional) — builds `lsbx_stream::router(stream_state)`,
//! and `.merge()`s it with this crate's own REST router, so a single bound
//! gateway server (one `TcpListener`, one `axum::serve` call) serves both
//! route families together.
//!
//! `routes::build_router` (this crate's own standalone, full REST router)
//! is left unchanged for callers that want this crate's REST surface in
//! isolation (its own test suite, or a caller with no `lsbx-stream`
//! mounted). The merge in [`GatewayDeps::build_router`] instead calls
//! `routes::build_router_for_merge`, a merge-specific variant — see that
//! function's own doc comment for why: this crate (Unit 13) and
//! `lsbx-stream` (Unit 14) were each built independently and both
//! registered their own, differently-shaped `GET /console` and
//! `GET /consoles/{id}` handlers, which panics axum's router builder with
//! an "Overlapping method route" error if both are merged verbatim (a
//! real conflict this pass's own smoke test caught, not a hypothetical
//! one). `build_router_for_merge` omits this crate's own colliding
//! `/console`/`/consoles/{id}` registrations so `lsbx-stream`'s versions —
//! the ones this pass's own task description names explicitly as part of
//! the merged surface — are what the merged router actually serves for
//! those two paths; every other route this crate owns (including the
//! non-colliding `GET /consoles` list endpoint) is unaffected.

pub mod auth;
pub mod ratelimit;
pub mod routes;

pub use ratelimit::RateLimitConfig;
pub use routes::{build_router, GatewayConfig};

use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

/// Everything needed to build the *merged* router (this crate's own REST
/// routes plus `lsbx-stream`'s mounted sub-router) — the Gap 1 composition
/// root.
///
/// `state_dir` is the same directory the caller already built its
/// `Arc<LsbxOps>` from (via a `SandboxStore` internal to that `LsbxOps`,
/// which this crate has no accessor for — `LsbxOps`'s `sandbox_store`
/// field is private by design, confirmed against
/// `crates/lsbx-ops/src/lib.rs`). This struct exists specifically so a
/// caller (the CLI's `serve` subcommand, or this crate's own tests) can
/// hand both the already-built `LsbxOps` and the raw `state_dir` needed to
/// construct `lsbx-stream`'s own independent `SandboxStore`, without this
/// crate needing to reach into `LsbxOps`'s private internals (which it
/// cannot) or `lsbx-ops` needing to grow a new accessor for a door-level
/// composition concern that isn't `lsbx-ops`'s job either.
pub struct GatewayDeps {
    pub ops: Arc<LsbxOps>,
    pub state_dir: PathBuf,
}

impl GatewayDeps {
    /// Builds the merged router: this crate's own REST routes
    /// (`routes::build_router`) merged with `lsbx-stream`'s mounted
    /// sub-router (`/stream/*`, `/console`, `/consoles/*`) — see this
    /// module's "Gap 1" doc comment for the full design rationale.
    ///
    /// Constructs a second, independent `lsbx_store::sandbox_store::SandboxStore`
    /// pointed at the same `state_dir` `self.ops` was itself built from —
    /// this is the one new piece of state this function adds beyond what
    /// `routes::build_router` already needed, and it exists solely to
    /// satisfy `lsbx-stream`'s own documented `StreamState` shape.
    pub fn build_router(self, config: GatewayConfig) -> axum::Router {
        let rest_router = routes::build_router_for_merge(Arc::clone(&self.ops), config);

        let stream_store = Arc::new(lsbx_store::sandbox_store::SandboxStore::new(
            self.state_dir.clone(),
        ));
        let stream_state = lsbx_stream::StreamState {
            ops: self.ops,
            store: stream_store,
        };
        let stream_router = lsbx_stream::router(stream_state);

        rest_router.merge(stream_router)
    }
}

/// The result of [`run_server`]'s fail-closed bind check, and (on success)
/// the live server future ready to be awaited.
pub struct BoundServer {
    pub local_addr: SocketAddr,
    /// The bound listener, handed back rather than consumed internally so
    /// [`BoundServer::serve`] can construct the `axum::serve` future at
    /// call time (its concrete type depends on the connect-info generic
    /// parameter, which is awkward to name as a stored field type) while
    /// still letting a caller inspect `local_addr` before deciding to
    /// serve at all.
    listener: tokio::net::TcpListener,
    router: axum::Router,
}

impl BoundServer {
    /// Awaiting this runs the server until the process is torn down —
    /// there is no built-in shutdown signal wired in here; a caller that
    /// needs graceful shutdown should race this against its own signal
    /// future, the same pattern `axum::serve`'s own docs recommend.
    pub async fn serve(self) -> std::io::Result<()> {
        axum::serve(
            self.listener,
            self.router
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }
}

/// Checks the fail-closed bind precondition (this unit's acceptance
/// criteria) and, if it passes, actually binds `addr` and constructs the
/// live server — now serving the merged router (this crate's own REST
/// routes plus `lsbx-stream`'s mounted sub-router; see this module's "Gap
/// 1" doc comment) rather than the REST-only router `build_router` alone
/// would produce. Returns `Err(LsbxError::AuthFailed)` when a non-loopback
/// bind is attempted without both a configured token and `insecure: true`
/// — `AuthFailed` (exit code 7 per SPEC.md §6) is the correct mapping
/// since this is exactly "a gateway bearer-auth rejection" at the
/// soonest possible point: before any bearer auth could ever be checked
/// at all, because there would be no real auth to check against.
///
/// A bind failure at the OS level (port in use, permission denied) is
/// `LsbxError::ContractViolated` — the fail-closed *check* passed (this
/// bind was supposed to be allowed), but the actual `TcpListener::bind`
/// call itself failed, which is a different claim than "this bind was
/// never authorized to happen."
pub async fn run_server(
    deps: GatewayDeps,
    config: GatewayConfig,
    addr: SocketAddr,
) -> Result<BoundServer, LsbxError> {
    enforce_fail_closed_bind(&config, addr.ip())?;

    let router = deps.build_router(config);

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        LsbxError::ContractViolated(format!("failed to bind gateway listener on {addr}: {e}"))
    })?;
    let local_addr = listener.local_addr().map_err(|e| {
        LsbxError::ContractViolated(format!("failed to read bound listener's local address: {e}"))
    })?;

    Ok(BoundServer {
        local_addr,
        listener,
        router,
    })
}

/// The fail-closed check itself, factored out so it can be unit-tested
/// (`tests/test_auth_fail_closed.rs`) without needing an actual free port
/// to bind for the negative case.
///
/// A loopback address (`127.0.0.1`, `::1`) is always allowed regardless of
/// token/insecure configuration — the risk this check exists to prevent is
/// an *unauthenticated* listener reachable from *other hosts*, and a
/// loopback bind is by definition reachable only from the same host,
/// which is the same trust boundary a config file or Unix socket would
/// already cross.
fn enforce_fail_closed_bind(config: &GatewayConfig, ip: IpAddr) -> Result<(), LsbxError> {
    if ip.is_loopback() {
        return Ok(());
    }

    let has_token = config.token.is_some();

    if has_token && config.insecure {
        // Both conditions are satisfied simultaneously in this branch
        // only because the acceptance criteria's "without both a
        // configured token and an explicit --insecure opt-in" is read as
        // "both must be present to bind" — i.e. `insecure` alone does not
        // bypass requiring a token, and a token alone does not bypass
        // requiring the explicit `--insecure` opt-in for a non-loopback
        // host. This is the strictest reading available of an acceptance
        // criterion phrased as a double negative, and is called out
        // explicitly in this crate's PR description as the interpretation
        // taken.
        return Ok(());
    }

    Err(LsbxError::AuthFailed(format!(
        "refusing to bind non-loopback address {ip}: a non-loopback bind requires both a \
         configured token (token: {token_state}) and the explicit --insecure opt-in \
         (insecure: {insecure}) — binding unauthenticated to a non-loopback interface is \
         never allowed",
        token_state = if has_token { "present" } else { "absent" },
        insecure = config.insecure
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn config(token: Option<&str>, insecure: bool) -> GatewayConfig {
        GatewayConfig {
            token: token.map(str::to_string),
            allow_local_files: false,
            insecure,
            rate_limit: crate::ratelimit::RateLimitConfig::default(),
        }
    }

    #[test]
    fn loopback_bind_is_always_allowed() {
        let cfg = config(None, false);
        assert!(enforce_fail_closed_bind(&cfg, "127.0.0.1".parse().unwrap()).is_ok());
        assert!(enforce_fail_closed_bind(&cfg, "::1".parse().unwrap()).is_ok());
    }

    #[test]
    fn non_loopback_without_token_or_insecure_is_refused() {
        let cfg = config(None, false);
        let result = enforce_fail_closed_bind(&cfg, "0.0.0.0".parse().unwrap());
        assert!(matches!(result, Err(LsbxError::AuthFailed(_))));
    }

    #[test]
    fn non_loopback_with_token_but_not_insecure_is_refused() {
        let cfg = config(Some("secret"), false);
        let result = enforce_fail_closed_bind(&cfg, "0.0.0.0".parse().unwrap());
        assert!(matches!(result, Err(LsbxError::AuthFailed(_))));
    }

    #[test]
    fn non_loopback_with_insecure_but_no_token_is_refused() {
        let cfg = config(None, true);
        let result = enforce_fail_closed_bind(&cfg, "0.0.0.0".parse().unwrap());
        assert!(matches!(result, Err(LsbxError::AuthFailed(_))));
    }

    #[test]
    fn non_loopback_with_both_token_and_insecure_is_allowed() {
        let cfg = config(Some("secret"), true);
        assert!(enforce_fail_closed_bind(&cfg, "0.0.0.0".parse().unwrap()).is_ok());
    }

    /// Gap 1 smoke test: builds the merged router via `GatewayDeps::build_router`
    /// against a real `DemoBackend`-backed `LsbxOps`, and confirms — through
    /// one HTTP request per route family, both served through the SAME
    /// `Router` value — that both this crate's own REST routes and
    /// `lsbx-stream`'s mounted routes are actually reachable together. Per
    /// the task's own instructions this is a smoke test, not a rebuild of
    /// `lsbx-stream`'s own test suite (which already covers that crate's
    /// routes in depth, in its own crate).
    #[tokio::test]
    async fn merged_router_serves_both_gateway_and_stream_routes() {
        use tower::ServiceExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let sandbox_store = lsbx_store::sandbox_store::SandboxStore::new(dir.path().to_path_buf());
        let ci_job_store = lsbx_store::ci_job_store::CiJobStore::new(dir.path().to_path_buf());
        let registry = lsbx_golden::registry::ImageRegistry {
            images: vec![],
            goldens: vec![],
            profiles: std::collections::HashMap::new(),
        };
        let backend = lsbx_backend_demo::DemoBackend::new();
        let clock = Box::new(lsbx_kernel::clock::SystemClock);
        let ops = Arc::new(LsbxOps::new(
            Box::new(backend),
            "demo".to_string(),
            sandbox_store,
            ci_job_store,
            registry,
            clock,
        ));

        let deps = GatewayDeps {
            ops,
            state_dir: dir.path().to_path_buf(),
        };
        let gw_config = config(Some("test-token"), false);
        let router = deps.build_router(gw_config);

        // One gateway route (an authenticated REST route from routes.rs):
        // /health with the configured bearer token must succeed.
        let health_req = axum::http::Request::builder()
            .uri("/health")
            .header("Authorization", "Bearer test-token")
            .body(axum::body::Body::empty())
            .expect("build health request");
        let health_response = router.clone().oneshot(health_req).await.expect("oneshot health");
        assert_eq!(health_response.status(), axum::http::StatusCode::OK);

        // One stream-crate route (the unauthenticated /console page from
        // lsbx-stream, mounted via this merge): must be served by the SAME
        // router value, proving the merge actually happened rather than
        // two separately-bound servers.
        let console_req = axum::http::Request::builder()
            .uri("/console?target=sbx-does-not-matter")
            .body(axum::body::Body::empty())
            .expect("build console request");
        let console_response = router.oneshot(console_req).await.expect("oneshot console");
        assert_eq!(console_response.status(), axum::http::StatusCode::OK);
    }
}
