# Unit 16 — CI Broker: GitHub App Auth & Repo Discovery

## Objective
Implement RS256 JWT signing, installation-token exchange and caching, and org-wide repo discovery — the first third of the zero-idle CI runner broker, replacing the existing `openssl` subprocess signing with native `jsonwebtoken`.

## Context
Layer 7. First of three units sharing the `lsbx-broker` crate (Units 16/17/18); this one owns `src/auth.rs` and `src/github_client.rs` only. Land it before Unit 17, which imports its `GitHubClient`.

## Acceptance criteria
- [ ] `GitHubAppAuth::jwt()` builds an RS256 JWT with `{iss: app_id, iat: now-60, exp: now+540}` claims via `jsonwebtoken`, matching the existing manually-signed claims shape exactly.
- [ ] `installation_token()` exchanges the JWT for an installation token via `POST /app/installations/{id}/access_tokens`, discovering the installation id via `GET /orgs/{owner}/installation` when not preset, caching with a 300-second refresh margin — exact existing behavior.
- [ ] Falls back to shelling out to the `gh` CLI when no GitHub App credentials are configured, matching the existing fallback path (keeps local dev/testing possible without full App credentials).
- [ ] `installation_repositories()` calls `GET /installation/repositories?per_page=100`, paginating via `total_count`, through `octocrab`'s typed API rather than hand-rolled JSON parsing.
- [ ] A test proves a cached, still-valid JWT is not regenerated on a second call within the same process.
- [ ] No GitHub App private key or token value is ever logged, including at `--verbose` — a test scans log output for a known-fixture key/token substring and asserts it is absent.

## Interface contract
```rust
// src/auth.rs
use lsbx_kernel::error::LsbxError;

pub struct GitHubAppConfig {
    pub app_id: u64,
    pub private_key_pem: String, // never logged
    pub installation_id: Option<u64>, // discovered via API if None
}

pub struct GitHubAppAuth {
    config: GitHubAppConfig,
    // internal: cached (jwt, expires_at), cached (installation_token, expires_at)
}

impl GitHubAppAuth {
    pub fn new(config: GitHubAppConfig) -> Self;
    pub fn jwt(&mut self) -> Result<String, LsbxError>;
    pub async fn installation_token(&mut self, owner: &str) -> Result<String, LsbxError>;
}

/// Used when no GitHubAppConfig is available — shells to `gh api`, matching the existing fallback.
pub struct GhCliFallback;
impl GhCliFallback {
    pub async fn api(&self, path: &str) -> Result<serde_json::Value, LsbxError>;
}

// src/github_client.rs
pub struct GitHubClient {
    // wraps octocrab::Octocrab, constructed from either an installation token or the gh CLI fallback
}

impl GitHubClient {
    pub async fn installation_repositories(&self, owner: &str) -> Result<Vec<String>, LsbxError>; // full "owner/repo" strings, paginated
}
```

## Boundaries — do NOT touch
Does not implement queue polling or label matching (Unit 17 owns `src/poll.rs`/`src/labels.rs` in the same crate). Does not persist anything through `lsbx-store` — auth caching is in-process/in-memory only, never disk-persisted.

## Output
- `crates/lsbx-broker/Cargo.toml`
- `crates/lsbx-broker/src/lib.rs` (module wiring only)
- `crates/lsbx-broker/src/auth.rs`
- `crates/lsbx-broker/src/github_client.rs`
- `crates/lsbx-broker/tests/test_jwt_claims.rs`
- `crates/lsbx-broker/tests/test_token_caching.rs`
- `crates/lsbx-broker/tests/test_no_secret_leakage.rs`

## Verification
```bash
cargo check -p lsbx-broker --message-format=json
cargo clippy -p lsbx-broker --all-targets --all-features -- -D warnings
cargo test -p lsbx-broker --test test_jwt_claims
cargo test -p lsbx-broker --test test_token_caching
cargo test -p lsbx-broker --test test_no_secret_leakage
```
Scenario: `test_token_caching` calls `installation_token()` twice within the refresh window against a mocked GitHub API and asserts the mock's token-exchange endpoint was hit exactly once.
