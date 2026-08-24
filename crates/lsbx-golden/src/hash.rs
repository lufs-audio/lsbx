//! Content-hash golden naming (`lufs-<sha256[:8]>`) -- SPEC.md Deviation 3.
//!
//! This is the *first real implementation* of a naming scheme that exists
//! today only as a CLI help string in the Python predecessor, with no
//! populated field on any shipped golden. `content_hash` computes a real
//! SHA-256 digest over the given qcow2 file's bytes and returns the first 8
//! hex characters, formatted as `lufs-<hash>`.

use lsbx_kernel::error::LsbxError;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Computes `lufs-<sha256[:8]>` over the bytes of the qcow2 file at
/// `qcow2_path`.
///
/// Reads the file in fixed-size chunks rather than loading it wholesale --
/// qcow2 images are routinely gigabytes in size, and a golden image's
/// content hash should not require holding the entire image in memory at
/// once.
///
/// I/O failure reading the file (not found, permission denied, truncated
/// read) maps to `LsbxError::NotFound`, per this crate's error-variant
/// convention (see `registry.rs`'s module doc comment) -- the overwhelming
/// majority of failures here are "the disk image this golden build was
/// supposed to produce isn't actually at the path we expected," which is a
/// "doesn't resolve" failure, not an internal contract violation.
pub fn content_hash(qcow2_path: &Path) -> Result<String, LsbxError> {
    let mut file = std::fs::File::open(qcow2_path).map_err(|e| {
        LsbxError::NotFound(format!(
            "could not open golden image at {} for content hashing: {}",
            qcow2_path.display(),
            e
        ))
    })?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            LsbxError::NotFound(format!(
                "failed reading golden image at {} while computing content hash: {}",
                qcow2_path.display(),
                e
            ))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    let hash_hex = hex::encode(digest);
    Ok(format!("lufs-{}", &hash_hex[..8]))
}

// See registry.rs's identically-worded comment above its own test module for
// why this scoped allow exists (Unit 01's crates/lsbx-kernel/tests/test_kernel.rs
// pattern, applied to a #[cfg(test)] mod instead of a separate tests/*.rs file).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic_for_identical_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.qcow2");
        std::fs::write(&path, b"fake qcow2 bytes for testing").expect("write");

        let h1 = content_hash(&path).expect("hash 1");
        let h2 = content_hash(&path).expect("hash 2");
        assert_eq!(h1, h2);
        assert!(h1.starts_with("lufs-"));
        assert_eq!(h1.len(), "lufs-".len() + 8);
    }

    #[test]
    fn content_hash_differs_for_different_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("a.qcow2");
        let path_b = dir.path().join("b.qcow2");
        std::fs::write(&path_a, b"content A").expect("write a");
        std::fs::write(&path_b, b"content B").expect("write b");

        let h_a = content_hash(&path_a).expect("hash a");
        let h_b = content_hash(&path_b).expect("hash b");
        assert_ne!(h_a, h_b);
    }

    #[test]
    fn content_hash_missing_file_maps_to_not_found() {
        let result = content_hash(Path::new("/nonexistent/golden.qcow2"));
        assert!(matches!(result, Err(LsbxError::NotFound(_))));
    }

    #[test]
    fn content_hash_matches_known_sha256_prefix() {
        // Independently verifiable: sha256("hello world") =
        // b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde
        // so lufs-<sha256[:8]> should be "lufs-b94d27b9".
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hello.qcow2");
        std::fs::write(&path, b"hello world").expect("write");
        let h = content_hash(&path).expect("hash");
        assert_eq!(h, "lufs-b94d27b9");
    }
}
