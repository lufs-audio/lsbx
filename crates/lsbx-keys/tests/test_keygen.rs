// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside `cargo test`,
// never in a shipped code path. clippy::unwrap_used / expect_used are
// restriction-group lints that don't understand "this whole file is test
// code" the way #[cfg(test)] does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this file only —
// crates/lsbx-keys/src/**/*.rs (the real production code path) is unwrap/
// expect/panic-free under the same workspace lints with no allow needed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_keys::keygen::{cleanup_keypair, generate_ephemeral_keypair};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Named scenario from the unit contract: decodes the emitted
/// `public_key_line` through an *independent* OpenSSH-format parse path
/// (`ssh_key::PublicKey::from_openssh`), and separately re-parses the
/// private key file that was written to disk
/// (`ssh_key::PrivateKey::from_openssh`), then asserts the private key's own
/// derived public half is bit-for-bit identical to the parsed public line.
/// This proves the two halves written to disk actually correspond to each
/// other, not just that each half independently looks well-formed.
#[test]
fn pubkey_line_parses_as_valid_openssh() {
    let keypair = generate_ephemeral_keypair("test-label").unwrap();

    // Independent decode path #1: parse the emitted public key line.
    let parsed_pub = ssh_key::PublicKey::from_openssh(&keypair.public_key_line).unwrap();
    assert_eq!(parsed_pub.algorithm(), ssh_key::Algorithm::Ed25519);
    assert_eq!(parsed_pub.comment(), "lsbx:test-label");

    // Independent decode path #2: read the private key file back off disk
    // and parse it, then derive its public half independently of path #1.
    let private_key_bytes = std::fs::read_to_string(&keypair.private_key_path).unwrap();
    let parsed_priv = ssh_key::PrivateKey::from_openssh(&private_key_bytes).unwrap();
    let derived_pub = parsed_priv.public_key();

    // Bit-for-bit: the public line and the private key's derived public half
    // must be the exact same key data, not just "both look like ed25519".
    assert_eq!(derived_pub, &parsed_pub);

    cleanup_keypair(&keypair).unwrap();
}

#[test]
fn private_key_file_permissions_are_0600_and_dir_is_0700() {
    let keypair = generate_ephemeral_keypair("perm-check").unwrap();

    #[cfg(unix)]
    {
        let file_mode = std::fs::metadata(&keypair.private_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);

        let dir_path = keypair.private_key_path.parent().unwrap();
        let dir_mode = std::fs::metadata(dir_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    cleanup_keypair(&keypair).unwrap();
}

#[test]
fn temp_dir_uses_lsbx_key_prefix() {
    let keypair = generate_ephemeral_keypair("prefix-check").unwrap();

    let dir_name = keypair
        .private_key_path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        dir_name.starts_with("lsbx-key-"),
        "expected dir name to start with lsbx-key-, got {}",
        dir_name
    );
    assert_eq!(keypair.private_key_path.file_name().unwrap(), "id_ed25519");

    cleanup_keypair(&keypair).unwrap();
}

#[test]
fn comment_tag_is_exactly_lsbx_colon_label() {
    let keypair = generate_ephemeral_keypair("my-sandbox-42").unwrap();
    assert!(keypair.public_key_line.ends_with(" lsbx:my-sandbox-42"));
    assert_eq!(keypair.label, "my-sandbox-42");
    cleanup_keypair(&keypair).unwrap();
}

/// Adapted from Candidate B's `test_ephemeral_keypair_generation_and_cleanup`:
/// verifies `cleanup_keypair` actually removes the temp directory from disk
/// (not just that it returns `Ok`), which the round-trip test above doesn't
/// itself check — real coverage the round-trip test doesn't provide.
#[test]
fn cleanup_removes_temp_directory_from_disk() {
    let keypair = generate_ephemeral_keypair("cleanup-check").unwrap();
    let dir_path = keypair.private_key_path.parent().unwrap().to_path_buf();

    assert!(dir_path.exists(), "temp dir should exist before cleanup");
    assert!(
        keypair.private_key_path.exists(),
        "private key file should exist before cleanup"
    );

    cleanup_keypair(&keypair).unwrap();

    assert!(
        !dir_path.exists(),
        "temp dir should be removed after cleanup"
    );
}

#[test]
fn cleanup_is_idempotent_when_already_removed() {
    let keypair = generate_ephemeral_keypair("double-cleanup").unwrap();
    cleanup_keypair(&keypair).unwrap();
    // Calling cleanup again on an already-removed directory must not error —
    // the reaper and an explicit `destroy` can legitimately race to call
    // this for the same sandbox.
    cleanup_keypair(&keypair).unwrap();
}
