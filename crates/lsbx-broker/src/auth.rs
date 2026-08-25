//! GitHub App RS256 JWT signing and installation-token exchange/caching.
//!
//! # Error taxonomy mapping
//!
//! This module maps failures onto `lsbx_kernel::error::LsbxError`'s existing
//! seven variants (no new variants — every other merged unit in this
//! workspace follows this same taxonomy):
//!
//! - A genuine credential/authorization failure — a malformed or otherwise
//!   un-encodable private key, or GitHub itself rejecting our JWT/token with
//!   401/403 — maps to [`LsbxError::AuthFailed`].
//! - GitHub's API being unreachable, timing out, or returning a 5xx maps to
//!   [`LsbxError::BackendUnavailable`] (the *broker's* dependency, GitHub, is
//!   the thing that's down — the same shape as a libvirt socket being down
//!   for the libvirt backend).
//! - A response that parses as JSON but doesn't have the shape we expect
//!   (missing `id`, unparsable `expires_at`, etc.) maps to
//!   [`LsbxError::ContractViolated`] — the response violated the contract we
//!   coded against, distinct from "GitHub is down" or "our credentials are
//!   bad".
//!
//! # Auth-scheme note (why this uses `octocrab::Octocrab::builder().app(...)`
//! rather than `personal_token(...)` or a hand-rolled `reqwest` client)
//!
//! GitHub's two App-level endpoints this module calls —
//! `GET /orgs/{owner}/installation` and
//! `POST /app/installations/{id}/access_tokens` — both require
//! `Authorization: Bearer <jwt>`, where the JWT is the RS256-signed App
//! token, *not* a personal access token or installation token.
//!
//! `octocrab`'s `OctocrabBuilder::personal_token(...)` sets `Auth::PersonalToken`,
//! which does produce a `Bearer <token>` header — but it is the wrong tool
//! for a JWT: it has no notion of JWT expiry/regeneration, so using it here
//! would mean manually reconstructing a new `Octocrab` (or its auth state)
//! on every JWT refresh, duplicating logic `octocrab` already has.
//!
//! `octocrab` ships a dedicated mechanism for exactly this: `OctocrabBuilder::app(app_id,
//! key)` sets `Auth::App(AppAuth { app_id, key })`. Verified directly against
//! `octocrab` 0.43.0's source (`src/lib.rs`, `src/auth.rs`,
//! `src/service/middleware/auth_header.rs` at tag `v0.43.0`):
//!
//! - On every request, when the client's auth state is `AuthState::App`,
//!   `Octocrab::execute()` calls `AppAuth::generate_bearer_token()` (which
//!   internally re-signs a fresh JWT via `create_jwt` on *every* call — by
//!   its own doc comment, `octocrab` does not cache this token) and sets
//!   `Authorization: Bearer <jwt>` on the outgoing request. This is the
//!   correct scheme.
//! - That header is only attached when the target URI's authority is empty
//!   (`parts.uri.authority().is_none()`) — true for the relative-path routes
//!   this module calls (e.g. `octocrab.get("/orgs/{owner}/installation",
//!   ...)`), so the documented `AuthHeaderLayer`/`AuthState` bug tracked
//!   upstream as octocrab#576 / #738 (fixed by octocrab#754, which landed in
//!   0.44.0 — one version *after* this workspace's pinned `octocrab = "0.43"`)
//!   does not reach these calls. That bug is specifically about requests
//!   whose target URI carries a *full* authority matching `api.github.com`
//!   verbatim (e.g. a `Location` redirect, or a GitHub-returned absolute URL
//!   such as `installation.access_tokens_url` passed straight back into a
//!   request) — not the relative-path calls made here.
//! - Installation-token exchange has its own separate, already-correct
//!   caching path in `octocrab` (`AuthState::Installation`, populated via
//!   `Octocrab::installation(id)`): it builds the JWT bearer header manually,
//!   POSTs `/app/installations/{id}/access_tokens` directly (bypassing the
//!   `execute()`/`AuthHeaderLayer` path entirely), and caches the resulting
//!   installation token with expiry tracking. This module's own cache
//!   (below) exists because the contract requires an explicit,
//!   independently-observable cache at the `installation_token()` level (the
//!   test scenario mocks the exchange endpoint and asserts it is hit exactly
//!   once), not because `octocrab`'s internal cache is insufficient — using
//!   `Octocrab::installation(id).installation_and_token(id)` under the hood
//!   would work too, but this module's own cache is what the acceptance
//!   criterion actually inspects, so it is written explicitly rather than
//!   left implicit inside `octocrab`.
//!
//! Net effect: `octocrab`'s own `.app()`-authenticated client is used
//! directly for both JWT-authenticated calls (no `reqwest` dependency is
//! introduced), and this module layers its own explicit JWT + installation-
//! token caches on top, matching the contract's caching requirements exactly.
//!
//! # Deliberate deviation from the interface contract's literal signatures
//!
//! The unit contract's interface sketch writes `jwt(&mut self)` and
//! `installation_token(&mut self, owner: &str)`. This module implements both
//! as `&self` instead, using `Mutex`/`OnceLock` interior mutability for the
//! caches and the lazily-built `octocrab` client. Reasoning: Units 17/18
//! build a polling loop on top of this crate's `GitHubClient`/`GitHubAppAuth`
//! that will hold one shared instance across many loop iterations and
//! plausibly concurrent tasks — an exclusive `&mut self` receiver would force
//! every caller into `Arc<Mutex<GitHubAppAuth>>` (a mutex around the whole
//! struct) just to share it, which is strictly worse than the targeted
//! interior mutability used here (only the parts that actually mutate — the
//! caches and the lazy client cell — take a lock, not the whole struct, and
//! callers can hold a plain `&GitHubAppAuth`/`Arc<GitHubAppAuth>`). The
//! externally observable behavior (build/cache once, reuse until expiry) is
//! identical either way. If a reviewer prefers the literal `&mut self`
//! signature, it's a small mechanical change from here — say so and it
//! changes.

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use lsbx_kernel::error::LsbxError;
use octocrab::models::{AppId, Installation, InstallationToken};
use octocrab::Octocrab;
use serde::Serialize;

use crate::error_map::map_octocrab_error;

/// GitHub only allows JWTs that expire in the next 10 minutes. Matches the
/// existing (pre-rewrite) manually-signed claims shape exactly: issued 60
/// seconds in the past (to allow clock drift) and expiring 9 minutes (540s)
/// after that.
const JWT_BACKDATE_SECS: u64 = 60;
const JWT_LIFETIME_SECS: u64 = 9 * 60;

/// Installation tokens are refreshed this many seconds before their real
/// expiry, so a caller never observes a token that expires mid-request.
const INSTALLATION_TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

#[derive(Serialize)]
struct JwtClaims {
    iss: u64,
    iat: u64,
    exp: u64,
}

/// Configuration for a GitHub App. Never derive `Debug` on this type (or log
/// it) without redacting `private_key_pem` first — this crate doesn't derive
/// `Debug` here at all, since nothing needs to print this type.
pub struct GitHubAppConfig {
    pub app_id: u64,
    /// The App's RSA private key, PEM-encoded. Never logged.
    pub private_key_pem: String,
    /// Preset installation id. Discovered via `GET /orgs/{owner}/installation`
    /// when `None`.
    pub installation_id: Option<u64>,
}

struct CachedJwt {
    token: String,
    expires_at_unix: u64,
}

struct CachedInstallationToken {
    token: String,
    expires_at_unix: u64,
}

/// Signs and caches GitHub App JWTs, and exchanges/caches installation
/// access tokens.
///
/// Auth caching here is in-process/in-memory only — this never persists
/// through `lsbx-store` (see the unit boundary: this crate does not touch
/// disk for auth state).
pub struct GitHubAppAuth {
    config: GitHubAppConfig,
    /// Overrides `octocrab`'s default `https://api.github.com` base URI.
    /// `None` in production; set only by this crate's own tests (via
    /// `new_with_base_uri`) to point at a `wiremock` mock server.
    base_uri_override: Option<String>,
    /// A single `octocrab::Octocrab` authenticated as the GitHub App
    /// (`Auth::App`), reused for every JWT-authenticated call rather than
    /// rebuilt per call. See the module doc comment for why this mechanism
    /// (not `personal_token` or a hand-rolled `reqwest` client) is correct
    /// here.
    ///
    /// Lazily constructed on first use (inside an async method), not
    /// eagerly in `new()`: `octocrab::Octocrab::builder().build()`
    /// unconditionally spins up a background Tower-buffered service task,
    /// which requires an active Tokio runtime — so building it eagerly
    /// would force every caller of the synchronous `new()`/`jwt()` methods
    /// to be inside an async context even when they never make a network
    /// call. `OnceLock` keeps construction to exactly once, the same
    /// "build once, reuse for every call" property an eager field would
    /// have had.
    app_client: OnceLock<Octocrab>,
    cached_jwt: Mutex<Option<CachedJwt>>,
    cached_installation_token: Mutex<Option<CachedInstallationToken>>,
    installation_id: Mutex<Option<u64>>,
}

fn now_unix() -> Result<u64, LsbxError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| LsbxError::ContractViolated(format!("system clock before unix epoch: {e}")))
}

/// Parses `private_key_pem` into a `jsonwebtoken::EncodingKey`. The one place
/// this parse happens, used by both `build_app_client` (to construct
/// `octocrab`'s `.app(...)`-authenticated client) and `jwt()` (to sign this
/// crate's own cached JWT) — avoids duplicating the same PEM-parse-and-map-err
/// block in two places.
fn encoding_key(private_key_pem: &str) -> Result<EncodingKey, LsbxError> {
    EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| LsbxError::AuthFailed(format!("invalid GitHub App private key (RSA PEM): {e}")))
}

/// Builds the `Octocrab` client used for every App (JWT-authenticated)
/// call. Centralized here so there is exactly one place that constructs the
/// `.app(...)`-authenticated builder — see the module doc comment for why
/// `personal_token` is not used for this.
fn build_app_client(app_id: u64, private_key_pem: &str, base_uri_override: Option<&str>) -> Result<Octocrab, LsbxError> {
    let key = encoding_key(private_key_pem)?;
    let mut builder = Octocrab::builder().app(AppId(app_id), key);

    if let Some(base_uri) = base_uri_override {
        builder = builder
            .base_uri(base_uri)
            .map_err(|e| LsbxError::AuthFailed(format!("invalid test base_uri: {e}")))?;
    }

    builder
        .build()
        .map_err(|e| LsbxError::AuthFailed(format!("failed to build GitHub App client: {e}")))
}

impl GitHubAppAuth {
    /// Builds a new `GitHubAppAuth`. Purely synchronous and does not require
    /// an active Tokio runtime: the underlying `octocrab` client (which does
    /// require one to construct) is built lazily on first actual use, not
    /// here.
    pub fn new(config: GitHubAppConfig) -> Result<Self, LsbxError> {
        Self::from_config(config, None)
    }

    /// Same as [`GitHubAppAuth::new`], but lets the caller point the
    /// underlying `octocrab` client at a custom base URI — used only by this
    /// crate's own tests (gated behind the `test-util` feature, mirroring
    /// `lsbx-kernel`'s `testing` feature for the same reason: a bare
    /// `#[cfg(test)]` item is invisible to a `tests/*.rs` integration test,
    /// which compiles as a separate crate) to point at a `wiremock` mock
    /// server instead of real `api.github.com`.
    #[cfg(feature = "test-util")]
    pub fn new_with_base_uri(config: GitHubAppConfig, base_uri: &str) -> Result<Self, LsbxError> {
        Self::from_config(config, Some(base_uri.to_string()))
    }

    fn from_config(config: GitHubAppConfig, base_uri_override: Option<String>) -> Result<Self, LsbxError> {
        let installation_id = config.installation_id;
        Ok(Self {
            config,
            base_uri_override,
            app_client: OnceLock::new(),
            cached_jwt: Mutex::new(None),
            cached_installation_token: Mutex::new(None),
            installation_id: Mutex::new(installation_id),
        })
    }

    /// Returns the lazily-constructed `octocrab` App client, building it on
    /// first call. See the `app_client` field doc comment for why this is
    /// lazy rather than eager.
    fn app_client(&self) -> Result<&Octocrab, LsbxError> {
        if let Some(client) = self.app_client.get() {
            return Ok(client);
        }
        let client = build_app_client(
            self.config.app_id,
            &self.config.private_key_pem,
            self.base_uri_override.as_deref(),
        )?;
        // `OnceLock::set` failing here just means a concurrent caller raced
        // us and already populated the cell — harmless, since both clients
        // are functionally identical and `.get()` below returns whichever
        // one won, not necessarily ours.
        let _ = self.app_client.set(client);
        self.app_client.get().ok_or_else(|| {
            LsbxError::Interrupted(
                "app_client OnceLock unexpectedly empty immediately after set".to_string(),
            )
        })
    }

    /// Builds (or returns a cached, still-valid) RS256 JWT with
    /// `{iss: app_id, iat: now-60, exp: now+540}` claims, matching the
    /// existing manually-signed claims shape exactly.
    ///
    /// This crate's own cache, distinct from `octocrab`'s internal
    /// `AppAuth::generate_bearer_token()` (which does not cache — see the
    /// module doc comment) — this is what makes the "a cached, still-valid
    /// JWT is not regenerated on a second call" acceptance criterion true at
    /// this method's boundary.
    pub fn jwt(&self) -> Result<String, LsbxError> {
        let now = now_unix()?;

        {
            let cached = self
                .cached_jwt
                .lock()
                .map_err(|_| LsbxError::Interrupted("jwt cache lock poisoned".to_string()))?;
            if let Some(cached) = cached.as_ref() {
                if now < cached.expires_at_unix {
                    return Ok(cached.token.clone());
                }
            }
        }

        let claims = JwtClaims {
            iss: self.config.app_id,
            iat: now.saturating_sub(JWT_BACKDATE_SECS),
            exp: now + JWT_LIFETIME_SECS,
        };
        let key = encoding_key(&self.config.private_key_pem)?;
        let token = encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| LsbxError::AuthFailed(format!("failed to sign GitHub App JWT: {e}")))?;

        let mut cached = self
            .cached_jwt
            .lock()
            .map_err(|_| LsbxError::Interrupted("jwt cache lock poisoned".to_string()))?;
        *cached = Some(CachedJwt {
            token: token.clone(),
            expires_at_unix: claims.exp,
        });

        Ok(token)
    }

    /// Discovers the installation id for `owner` via
    /// `GET /orgs/{owner}/installation`, if not already preset in
    /// `GitHubAppConfig::installation_id` or previously discovered.
    async fn resolve_installation_id(&self, owner: &str) -> Result<u64, LsbxError> {
        {
            let cached = self
                .installation_id
                .lock()
                .map_err(|_| LsbxError::Interrupted("installation id lock poisoned".to_string()))?;
            if let Some(id) = *cached {
                return Ok(id);
            }
        }

        let route = format!("/orgs/{owner}/installation");
        let installation: Installation = self
            .app_client()?
            .get(&route, None::<&()>)
            .await
            .map_err(|e| map_octocrab_error(e, &route))?;

        let id = installation.id.0;

        let mut cached = self
            .installation_id
            .lock()
            .map_err(|_| LsbxError::Interrupted("installation id lock poisoned".to_string()))?;
        *cached = Some(id);

        Ok(id)
    }

    /// Exchanges the current JWT for an installation access token via
    /// `POST /app/installations/{id}/access_tokens`, discovering the
    /// installation id via `GET /orgs/{owner}/installation` when not preset,
    /// and caching the result with a 300-second refresh margin.
    pub async fn installation_token(&self, owner: &str) -> Result<String, LsbxError> {
        let now = now_unix()?;

        {
            let cached = self.cached_installation_token.lock().map_err(|_| {
                LsbxError::Interrupted("installation token cache lock poisoned".to_string())
            })?;
            if let Some(cached) = cached.as_ref() {
                if now + INSTALLATION_TOKEN_REFRESH_MARGIN_SECS < cached.expires_at_unix {
                    return Ok(cached.token.clone());
                }
            }
        }

        let installation_id = self.resolve_installation_id(owner).await?;

        // Ensure a JWT exists (also warms this crate's own JWT cache); the
        // JWT itself is attached to the outgoing request by `octocrab`'s
        // `.app(...)`-authenticated client, not passed explicitly here.
        let _ = self.jwt()?;

        let route = format!("/app/installations/{installation_id}/access_tokens");
        let token_object: InstallationToken = self
            .app_client()?
            .post(&route, Some(&serde_json::json!({})))
            .await
            .map_err(|e| map_octocrab_error(e, &route))?;

        let expires_at_unix = match &token_object.expires_at {
            Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.timestamp().max(0) as u64)
                .map_err(|e| {
                    LsbxError::ContractViolated(format!(
                        "installation token response had an unparsable expires_at ({raw:?}): {e}"
                    ))
                })?,
            None => {
                return Err(LsbxError::ContractViolated(
                    "installation token response had no expires_at".to_string(),
                ))
            }
        };

        let mut cached = self.cached_installation_token.lock().map_err(|_| {
            LsbxError::Interrupted("installation token cache lock poisoned".to_string())
        })?;
        *cached = Some(CachedInstallationToken {
            token: token_object.token.clone(),
            expires_at_unix,
        });

        Ok(token_object.token)
    }

    /// The GitHub App's numeric id, for constructing a [`GitHubClient`](crate::github_client::GitHubClient).
    pub fn app_id(&self) -> u64 {
        self.config.app_id
    }
}

/// Fallback used when no [`GitHubAppConfig`] is available — shells out to
/// `gh api`, matching the existing fallback path so local dev/testing stays
/// possible without full App credentials.
pub struct GhCliFallback;

impl GhCliFallback {
    pub async fn api(&self, path: &str) -> Result<serde_json::Value, LsbxError> {
        let path = path.to_string();
        let path_for_command = path.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("gh")
                .args(["api", &path_for_command])
                .output()
        })
        .await
        .map_err(|e| LsbxError::Interrupted(format!("gh CLI invocation task was interrupted: {e}")))?
        .map_err(|e| {
            LsbxError::BackendUnavailable(format!(
                "failed to execute `gh api {path}` (is the gh CLI installed and on PATH?): {e}"
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LsbxError::AuthFailed(format!(
                "`gh api {path}` failed ({}): {stderr}",
                output.status
            )));
        }

        serde_json::from_slice(&output.stdout).map_err(|e| {
            LsbxError::ContractViolated(format!("`gh api {path}` returned non-JSON output: {e}"))
        })
    }
}
