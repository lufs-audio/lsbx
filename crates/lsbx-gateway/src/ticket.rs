//! Short-lived, sandbox-scoped console tickets (issue #55).
//!
//! ## Why a query parameter, of all things
//! A browser cannot set an `Authorization` header on a WebSocket handshake —
//! `new WebSocket(url)` takes a URL and nothing else. So the console page mints
//! a ticket with an ordinary `fetch` (which *can* carry the master token as a
//! header), then presents that ticket as `?t=` on the WS upgrade.
//!
//! `docs/specs/2026-11-07T1440Z_canonical-url-lsbx-lufshq/SPEC.md` D7 chose
//! exactly this shape and rejected the alternatives for good reasons: session
//! cookies (server state + CSRF surface) and `?token=<master>` (leaks the one
//! shared secret into logs, history, and referrers). This module implements D7.
//!
//! ## What a ticket is
//! `<expiry>.<hex hmac-sha256>` — deliberately not a JWT: no algorithm
//! negotiation to get wrong, no parser to keep in sync, and the whole claim set
//! is one sandbox id and one integer.
//!
//! ```text
//! sig = HMAC-SHA256(key = SHA256(b"lsbx-console-ticket/v1" || master_token),
//!                   msg = "v1|stream|<sandbox-id>|<unix-expiry>")
//! ```
//!
//! The HMAC key is *derived* from the gateway's master token rather than being
//! the token itself, so a leaked ticket says nothing about the master secret,
//! and there is no second secret to configure or rotate separately.
//!
//! ## Why HMAC is written out here instead of using a crate
//! `hmac` is one call, but `Mac::new_from_slice` returns a `Result` that cannot
//! fail for HMAC (it accepts any key length), and this workspace's lints forbid
//! `unwrap`/`expect` in production paths — so using the crate would mean either
//! an unreachable error branch or a panic. HMAC is a ten-line construction over
//! the already-vendored `sha2`, and it is pinned by RFC 4231 test vectors below,
//! which is a stronger guarantee than "the wrapper type compiled".
//!
//! ## Properties this buys
//! - **Bounded replay:** TTL is [`TICKET_TTL_SECS`], and a ticket is valid for
//!   exactly one sandbox id (baked into the MAC input).
//! - **No state:** verification recomputes the MAC; nothing to store or sweep.
//! - **Fail-closed:** no configured gateway token ⇒ no ticket can be minted or
//!   verified at all.

use sha2::{Digest, Sha256};

/// Domain separator, so this HMAC key can never collide with any other future
/// use of the master token.
const KEY_DOMAIN: &str = "lsbx-console-ticket/v1";

/// Ticket lifetime. Short on purpose: the page mints one immediately before it
/// opens the socket, and a reconnect simply mints another. It only has to
/// outlive the WebSocket *handshake*, not the session.
pub const TICKET_TTL_SECS: u64 = 60;

/// HMAC-SHA256, spelled out over `sha2`.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block_key[..digest.len()].copy_from_slice(&digest);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        inner_pad[i] ^= block_key[i];
        outer_pad[i] ^= block_key[i];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// Derives the ticket HMAC key from the gateway's master token.
fn derive_key(master_token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(KEY_DOMAIN.as_bytes());
    hasher.update(master_token.as_bytes());
    hasher.finalize().into()
}

/// The exact bytes that get MAC'd. Any change here invalidates every outstanding
/// ticket, which is the correct behaviour for a format change.
fn signing_input(sandbox_id: &str, expires_at: u64) -> String {
    format!("v1|stream|{sandbox_id}|{expires_at}")
}

fn mac_hex(master_token: &str, sandbox_id: &str, expires_at: u64) -> String {
    let mac = hmac_sha256(
        &derive_key(master_token),
        signing_input(sandbox_id, expires_at).as_bytes(),
    );
    let mut out = String::with_capacity(64);
    for byte in mac {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Constant-shape string comparison: walks both strings fully instead of
/// returning at the first difference, so a caller cannot learn the correct MAC
/// one prefix byte at a time. (This is not a claim of perfect constant-time
/// behaviour in the presence of an optimising compiler — it removes the obvious
/// early-exit leak, matching the posture the gateway's token comparison already
/// documents.)
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Mints a ticket for `sandbox_id`, valid until `expires_at` (unix seconds).
///
/// The expiry is a parameter rather than read from the clock here, so the caller
/// (the HTTP handler) owns time and tests can pin it — the same discipline the
/// rest of the workspace follows for clocks.
pub fn mint(master_token: &str, sandbox_id: &str, expires_at: u64) -> String {
    format!(
        "{expires_at}.{}",
        mac_hex(master_token, sandbox_id, expires_at)
    )
}

/// Why a ticket was rejected. Never surfaced verbatim to a client (a 401 is a
/// 401), but kept distinct so the middleware can log honestly and tests can
/// assert *which* rule fired.
#[derive(Debug, PartialEq, Eq)]
pub enum TicketError {
    /// Not `<digits>.<64 hex chars>`.
    Malformed,
    /// The expiry has passed.
    Expired,
    /// Well-formed and unexpired, but the MAC is wrong — or was minted for a
    /// different sandbox, or with a different gateway token. Deliberately
    /// indistinguishable.
    SignatureInvalid,
}

/// Verifies `ticket` for `sandbox_id` at time `now` (unix seconds).
pub fn verify(
    master_token: &str,
    sandbox_id: &str,
    ticket: &str,
    now: u64,
) -> Result<(), TicketError> {
    let (expiry_part, signature) = ticket.split_once('.').ok_or(TicketError::Malformed)?;
    if expiry_part.is_empty() || signature.len() != 64 {
        return Err(TicketError::Malformed);
    }
    if !signature.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(TicketError::Malformed);
    }
    let expires_at: u64 = expiry_part.parse().map_err(|_| TicketError::Malformed)?;

    if now > expires_at {
        return Err(TicketError::Expired);
    }

    let expected = mac_hex(master_token, sandbox_id, expires_at);
    if constant_time_eq(signature.to_ascii_lowercase().as_str(), &expected) {
        Ok(())
    } else {
        Err(TicketError::SignatureInvalid)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const MASTER: &str = "master-token-for-tests";
    const SANDBOX: &str = "sbx-18e7cb8ac0ca9b71-ab2474e9";

    /// RFC 4231 test case 2: a known-answer vector, so the hand-written
    /// construction is proven correct rather than merely self-consistent.
    #[test]
    fn hmac_matches_rfc_4231_test_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn mint_then_verify_round_trips() {
        let ticket = mint(MASTER, SANDBOX, 1_000);
        assert!(verify(MASTER, SANDBOX, &ticket, 999).is_ok());
        // Boundary: still valid at exactly the expiry instant.
        assert!(verify(MASTER, SANDBOX, &ticket, 1_000).is_ok());
    }

    #[test]
    fn expiry_is_enforced() {
        let ticket = mint(MASTER, SANDBOX, 1_000);
        assert_eq!(
            verify(MASTER, SANDBOX, &ticket, 1_001),
            Err(TicketError::Expired)
        );
    }

    #[test]
    fn ticket_is_scoped_to_one_sandbox() {
        let ticket = mint(MASTER, SANDBOX, 1_000);
        assert_eq!(
            verify(MASTER, "sbx-somebody-else", &ticket, 999),
            Err(TicketError::SignatureInvalid)
        );
    }

    #[test]
    fn a_ticket_from_a_different_master_token_is_rejected() {
        let ticket = mint("some-other-gateway", SANDBOX, 1_000);
        assert_eq!(
            verify(MASTER, SANDBOX, &ticket, 999),
            Err(TicketError::SignatureInvalid)
        );
    }

    #[test]
    fn malformed_tickets_are_rejected_without_panicking() {
        for bad in [
            "",
            "nope",
            "123",
            ".abc",
            "abc.def",
            "1000.",
            "1000.zz",
            "1000.5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec38",
            "1000.5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec38430",
        ] {
            assert_eq!(
                verify(MASTER, SANDBOX, bad, 999),
                Err(TicketError::Malformed),
                "{bad:?} should be Malformed"
            );
        }
    }

    #[test]
    fn tampering_with_the_expiry_invalidates_the_signature() {
        let ticket = mint(MASTER, SANDBOX, 1_000);
        let (_, signature) = ticket.split_once('.').unwrap();
        let extended = format!("9999999999.{signature}");
        assert_eq!(
            verify(MASTER, SANDBOX, &extended, 999),
            Err(TicketError::SignatureInvalid)
        );
    }

    #[test]
    fn the_master_token_does_not_appear_in_a_ticket() {
        // A ticket travels in a URL, so it must never carry the shared secret.
        let ticket = mint(MASTER, SANDBOX, 1_000);
        assert!(!ticket.contains(MASTER));
        assert!(!ticket.to_lowercase().contains("master"));
    }

    #[test]
    fn uppercase_hex_is_accepted_because_urls_get_normalised() {
        let ticket = mint(MASTER, SANDBOX, 1_000);
        let (expiry, signature) = ticket.split_once('.').unwrap();
        let shouted = format!("{expiry}.{}", signature.to_uppercase());
        assert!(verify(MASTER, SANDBOX, &shouted, 999).is_ok());
    }
}
