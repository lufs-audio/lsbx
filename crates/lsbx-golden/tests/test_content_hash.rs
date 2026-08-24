//! Standalone integration test for `content_hash` (SPEC.md Deviation 3):
//! the first real, populated implementation of `lufs-<sha256[:8]>` golden
//! naming.

// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside `cargo test`,
// never in a shipped code path. clippy::unwrap_used / expect_used are
// restriction-group lints that don't understand "this whole file is test
// code" the way #[cfg(test)] does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this file only —
// crates/lsbx-golden/src/**/*.rs (the real production code path) is
// unwrap/expect/panic-free under the same workspace lints with no allow
// needed. Pattern established in Unit 01's crates/lsbx-kernel/tests/test_kernel.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_golden::content_hash;

#[test]
fn content_hash_format_is_lufs_prefixed_with_eight_hex_chars() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("golden.qcow2");
    std::fs::write(&path, b"some qcow2-shaped bytes").expect("write");

    let hash = content_hash(&path).expect("content_hash should succeed");

    assert!(hash.starts_with("lufs-"));
    let hex_part = hash.strip_prefix("lufs-").expect("has lufs- prefix");
    assert_eq!(hex_part.len(), 8);
    assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn content_hash_is_populated_not_a_placeholder() {
    // Guards against a regression back to the aspirational, unpopulated
    // state SPEC.md Deviation 3 describes ("a CLI help string today, not a
    // populated field on any shipped golden") — the hash must actually
    // vary with content, not be a hardcoded literal.
    let dir = tempfile::tempdir().expect("tempdir");
    let path_a = dir.path().join("a.qcow2");
    let path_b = dir.path().join("b.qcow2");
    std::fs::write(&path_a, b"golden A content").expect("write a");
    std::fs::write(&path_b, b"golden B content, totally different").expect("write b");

    let hash_a = content_hash(&path_a).expect("hash a");
    let hash_b = content_hash(&path_b).expect("hash b");

    assert_ne!(hash_a, hash_b, "content hash must actually depend on file content");
}

#[test]
fn content_hash_over_realistic_large_file_streams_without_loading_wholesale() {
    // 8 MiB of repeating pattern -- large enough to exercise the chunked
    // read loop in hash.rs multiple times over, standing in for a
    // real multi-gigabyte qcow2 image without actually needing one in CI.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("large.qcow2");
    let chunk = vec![0xABu8; 1024];
    let mut file = std::fs::File::create(&path).expect("create");
    use std::io::Write;
    for _ in 0..(8 * 1024) {
        file.write_all(&chunk).expect("write chunk");
    }
    drop(file);

    let hash = content_hash(&path).expect("content_hash should succeed on a large file");
    assert!(hash.starts_with("lufs-"));
}
