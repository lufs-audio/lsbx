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
//! `.await`-ing it internally, so a caller (the eventual `lsbx serve` CLI
//! subcommand, or this crate's own tests) decides whether/how long to run
//! it — blocking a test's whole async runtime inside a library function
//! with no cancellation handle would make "does the fail-closed check
//! actually prevent a bind" and "does a passing check actually produce a
//! live listener" impossible to test independently within a bounded test
//! timeout. The unit contract does not specify a concrete return type for
//! `run_server`, and the safer, more composable choice — given every other
//! unit in this system's own stated preference for typed, awaitable
//! results over ambient blocking (SPEC.md §1's "ran vs. proven" framing
//! applies here too: a function that blocks forever internally can't ever
//! report whether serving actually failed) — is to hand back something
//! awaitable rather than block the caller's `main` directly. This is
//! called out explicitly here (and in this crate's own PR description) as
//! the judgment call it is, not a silent deviation.

pub mod auth;
pub mod ratelimit;
pub mod routes;

pub use ratelimit::RateLimitConfig;
pub use routes::{build_router, GatewayConfig};

use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

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
/// live server. Returns `Err(LsbxError::AuthFailed)` when a non-loopback
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
    ops: Arc<LsbxOps>,
    config: GatewayConfig,
    addr: SocketAddr,
) -> Result<BoundServer, LsbxError> {
    enforce_fail_closed_bind(&config, addr.ip())?;

    let router = routes::build_router(ops, config);

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
}
