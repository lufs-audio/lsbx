//! Verifies `GitHubAppAuth::jwt()` builds an RS256 JWT with
//! `{iss: app_id, iat: now-60, exp: now+540}` claims, matching the existing
//! manually-signed claims shape exactly, and that a cached, still-valid JWT
//! is not regenerated on a second call within the same process.
//!
//! `unwrap()`/`expect()` are used freely below per this workspace's
//! established house convention for test code (see e.g.
//! `crates/lsbx-store/tests/test_lock.rs`'s identically-worded rationale):
//! the restriction-group clippy lints `unwrap_used`/`expect_used` fire on
//! any code text, including `tests/*.rs`, even though every fn in this file
//! only compiles under `cargo test` — so every merged crate in this
//! workspace scopes an explicit allow to its test files rather than either
//! disabling the lints workspace-wide or writing test assertions in an
//! unidiomatic, unwrap-free style.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use lsbx_broker::auth::{GitHubAppAuth, GitHubAppConfig};
use serde::Deserialize;

#[derive(Deserialize)]
struct Claims {
    iss: u64,
    iat: u64,
    exp: u64,
}

const FIXTURE_PRIVATE_KEY_PEM: &str = include_str!("fixture_key.pem");
const FIXTURE_PUBLIC_KEY_PEM: &str = include_str!("fixture_pub.pem");

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

fn test_auth() -> GitHubAppAuth {
    GitHubAppAuth::new(GitHubAppConfig {
        app_id: 123_456,
        private_key_pem: FIXTURE_PRIVATE_KEY_PEM.to_string(),
        installation_id: Some(999),
    })
    .expect("fixture key is a valid RSA PEM, GitHubAppAuth::new should succeed")
}

fn decode_claims(token: &str) -> Claims {
    let decoding_key = DecodingKey::from_rsa_pem(FIXTURE_PUBLIC_KEY_PEM.as_bytes())
        .expect("fixture public key is a valid RSA PEM");
    let mut validation = Validation::new(Algorithm::RS256);
    // The JWT's `exp` is a bare unix timestamp (not RFC 7519's default
    // leeway-aware validation target), and we want to inspect `iss`/`iat`/`exp`
    // ourselves rather than have the decode call reject on our own test's
    // clock skew.
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    decode::<Claims>(token, &decoding_key, &validation)
        .expect("jwt() should produce a JWT the matching public key can verify")
        .claims
}

#[test]
fn jwt_claims_match_expected_shape() {
    let auth = test_auth();
    let before = now_unix();
    let token = auth.jwt().expect("jwt() should succeed with a valid fixture key");
    let after = now_unix();

    let claims = decode_claims(&token);

    assert_eq!(claims.iss, 123_456, "iss must be the configured app_id");

    // iat = now - 60, allowing for the few seconds test execution itself takes.
    assert!(
        claims.iat <= before.saturating_sub(59) && claims.iat >= before.saturating_sub(63),
        "iat should be ~60s in the past (before={before}, iat={})",
        claims.iat
    );

    // exp = now + 540 (9 minutes), allowing the same small window.
    let expected_exp_min = before + 540 - 3;
    let expected_exp_max = after + 540 + 3;
    assert!(
        claims.exp >= expected_exp_min && claims.exp <= expected_exp_max,
        "exp should be ~540s (9 min) in the future (before={before}, exp={})",
        claims.exp
    );

    // exp - iat should be exactly the existing manually-signed lifetime:
    // 60s backdate + 540s lifetime = 600s.
    assert_eq!(claims.exp - claims.iat, 600, "exp - iat must equal 600s (60s backdate + 540s lifetime)");
}

#[test]
fn cached_jwt_is_not_regenerated_on_second_call() {
    let auth = test_auth();

    let first = auth.jwt().expect("first jwt() call should succeed");
    let second = auth.jwt().expect("second jwt() call should succeed");

    assert_eq!(
        first, second,
        "a cached, still-valid JWT must not be regenerated on a second call within the same process"
    );
}
