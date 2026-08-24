# Unit 03 — Ephemeral Ed25519 Key Management

## Objective
Generate ephemeral Ed25519 keypairs natively — no `ssh-keygen` subprocess — while preserving the existing external contract exactly: 0600 private-key permissions, ephemeral temp-directory storage, and the `lsbx:<label>` key-comment convention the `exedev` backend's reaper already pattern-matches on for orphan revocation.

## Context
Layer 2, depends only on Unit 01. The existing system shells to `ssh-keygen -t ed25519 -f <tmpdir>/id_ed25519 -N "" -C "lsbx:<label>"`. `ed25519-dalek` 3.0 changed its RNG plumbing from the 2.x line (the `rand_core` 0.10 fallible/infallible trait split) — pin exact dependency versions together and verify the generation call actually compiles rather than trusting a pre-written snippet of it (flagged in SPEC.md §9; this is real, current version churn, not a hypothetical).

## Acceptance criteria
- [ ] `generate_ephemeral_keypair(label)` produces a real Ed25519 keypair via `ed25519-dalek` — no subprocess.
- [ ] The private key is written into a fresh temp directory (`lsbx-key-XXXXXX` prefix, matching the existing `tempfile.mkdtemp(prefix="lsbx-key-")` convention), 0600 on the private key file, 0700 on its directory.
- [ ] The public key is emitted as a complete OpenSSH `authorized_keys` line (`ssh-ed25519 <base64> lsbx:<label>`) — byte-compatible with what OpenSSH/cloud-init expects, not just raw-key base64.
- [ ] The comment tag is exactly `lsbx:<label>`, matching what `reconcile_orphaned_keys` (and the existing `reconcile_keys()`) pattern-match on.
- [ ] `reconcile_orphaned_keys(tagged_keys, known_labels)` revokes any `lsbx:*`-tagged key whose label isn't in `known_labels`, and returns the count revoked; it takes backend-supplied `(comment, revoke_fn)` pairs so it stays backend-agnostic (Units 06/07 each build their own listing).
- [ ] A round-trip test decodes the emitted `public_key_line` through an independent OpenSSH-format parse path and confirms it matches the generated signing key's public half bit-for-bit.

## Interface contract
```rust
// src/keygen.rs
use lsbx_kernel::error::LsbxError;
use std::path::PathBuf;

pub struct EphemeralKeypair {
    pub private_key_path: PathBuf, // 0600, inside a 0700 temp dir
    pub public_key_line: String,   // "ssh-ed25519 <base64> lsbx:<label>"
    pub label: String,
}

pub fn generate_ephemeral_keypair(label: &str) -> Result<EphemeralKeypair, LsbxError>;

/// Removes the temp directory and its contents. Called on sandbox destroy/reap.
pub fn cleanup_keypair(keypair: &EphemeralKeypair) -> Result<(), LsbxError>;

// src/reconcile.rs
pub struct TaggedKey {
    pub comment: String,
    pub revoke: Box<dyn FnOnce() -> Result<(), LsbxError>>,
}

/// Returns the number of orphaned `lsbx:*`-tagged keys revoked.
pub fn reconcile_orphaned_keys(
    tagged_keys: Vec<TaggedKey>,
    known_labels: &[String],
) -> Result<usize, LsbxError>;

/// Parses a `lsbx:<label>` comment tag if present; ignores non-`lsbx`-tagged keys.
pub fn parse_label_tag(comment: &str) -> Option<String>;
```

## Boundaries — do NOT touch
Does not decide *where* a backend stores authorized keys (guest `authorized_keys` file vs. exe.dev's key-registration API) — Units 06/07 call `reconcile_orphaned_keys` with their own backend-specific `TaggedKey` listing. Does not persist anything through `lsbx-store` — key material's lifetime is tied to its temp directory; the state store only holds `key_path`/`key_dir` as string references (Unit 01's `SandboxRecord` fields), never the key bytes themselves.

## Output
- `crates/lsbx-keys/Cargo.toml`
- `crates/lsbx-keys/src/lib.rs`
- `crates/lsbx-keys/src/keygen.rs`
- `crates/lsbx-keys/src/reconcile.rs`
- `crates/lsbx-keys/tests/test_keygen.rs`
- `crates/lsbx-keys/tests/test_reconcile.rs`

## Verification
```bash
cargo check -p lsbx-keys --message-format=json
cargo clippy -p lsbx-keys --all-targets --all-features -- -D warnings
cargo test -p lsbx-keys --test test_keygen
cargo test -p lsbx-keys --test test_reconcile
```
Scenario: `cargo test -p lsbx-keys pubkey_line_parses_as_valid_openssh` decodes the emitted `public_key_line` independently and asserts it matches the generated `SigningKey`'s public half bit-for-bit.
