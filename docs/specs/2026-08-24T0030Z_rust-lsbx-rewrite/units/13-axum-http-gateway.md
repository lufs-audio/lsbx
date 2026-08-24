# Unit 13 — Axum HTTP Gateway

## Objective
Implement the HTTP gateway with the existing route table, auth, and fail-closed bind protection, plus a new rate limiter.

## Context
Layer 6, depends on Unit 10. Preserves every route found in the existing `gateway.py` exactly — this is Door 2 from SPEC.md §4.8.

## Acceptance criteria
- [ ] Every existing route is preserved with identical method/path/semantics: `GET /health`, `/images`, `/profiles`, `/capabilities`, `/consoles`, `/sandboxes`; `POST /sandboxes`; `GET /console?target=`; `GET /consoles/<id>`; `POST /sandboxes/<id>/upload?destination=`; `GET /sandboxes/<id>/artifacts?source=`; `GET /sandboxes/<id>`; `DELETE /sandboxes/<id>`; `POST /sandboxes/<id>/exec`; `POST /sandboxes/<id>/put`, `/get` (gated by an `allow_local_files` config flag, disabled by default); `POST /sandboxes/<id>/check`; `POST /sandboxes/<id>/info`; `POST /sandboxes/<id>/renew`; `POST /sandboxes/<id>/console`.
- [ ] Auth: `Authorization: Bearer <token>` or `X-Api-Key` header, compared against the configured token; `/console` (the browser-facing HTML page) GET is the sole unauthenticated exception, matching existing behavior exactly.
- [ ] Fail-closed bind: refuses to bind a non-loopback host without both a configured token and an explicit `--insecure` opt-in — never silently listens on `0.0.0.0` unauthenticated.
- [ ] New: a token-bucket rate limiter keyed by bearer token (falling back to source IP for the unauthenticated `/console` route), configurable rate/burst, returning `429` with `Retry-After` on exhaustion. Genuinely new functionality (SPEC.md Deviation 13) — the existing gateway has no rate limiter.
- [ ] The JSONL audit log records every mutating request with a SHA-256 hash of the command/body text, never the raw text — matches the existing `_audit_command` privacy property.
- [ ] Every route handler is a thin translation into one `LsbxOps` call — no handler contains a conditional that changes VM or golden behavior.

## Interface contract
```rust
// src/routes.rs
use axum::Router;

pub fn build_router(ops: std::sync::Arc<lsbx_ops::LsbxOps>, config: GatewayConfig) -> Router;

pub struct GatewayConfig {
    pub token: Option<String>,
    pub allow_local_files: bool,
    pub insecure: bool,
    pub rate_limit: RateLimitConfig,
}

pub struct RateLimitConfig { pub requests_per_minute: u32, pub burst: u32 }

// src/auth.rs
/// A `FromRequestParts` extractor validating `Authorization: Bearer` or `X-Api-Key`.
pub struct AuthedRequest;

// src/ratelimit.rs
pub struct TokenBucket {
    // capacity, refill_rate, per-key state
}
impl TokenBucket {
    pub fn check(&self, key: &str) -> RateLimitDecision;
}
pub enum RateLimitDecision {
    Allow,
    Deny { retry_after: std::time::Duration },
}
```

## Boundaries — do NOT touch
Does not implement the WebSocket stream/console proxy itself — Unit 14 owns `/stream/<sandbox-id>/<guest-path>`; this crate mounts Unit 14's router as a sub-router rather than reimplementing it. Implements no operation logic — every handler calls `LsbxOps`.

## Output
- `crates/lsbx-gateway/Cargo.toml`
- `crates/lsbx-gateway/src/lib.rs`
- `crates/lsbx-gateway/src/routes.rs`
- `crates/lsbx-gateway/src/auth.rs`
- `crates/lsbx-gateway/src/ratelimit.rs`
- `crates/lsbx-gateway/tests/test_routes.rs`
- `crates/lsbx-gateway/tests/test_auth_fail_closed.rs`
- `crates/lsbx-gateway/tests/test_rate_limit.rs`

## Verification
```bash
cargo check -p lsbx-gateway --message-format=json
cargo clippy -p lsbx-gateway --all-targets --all-features -- -D warnings
cargo test -p lsbx-gateway --test test_routes
cargo test -p lsbx-gateway --test test_auth_fail_closed
cargo test -p lsbx-gateway --test test_rate_limit
```
Scenario: `test_auth_fail_closed` attempts to bind `0.0.0.0` with no token and no `--insecure`, and asserts the server refuses to start rather than binding unauthenticated.
