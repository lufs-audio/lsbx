use ed25519_dalek::SigningKey;
use lsbx_kernel::error::LsbxError;
use rand::rngs::OsRng;
use ssh_key::private::{Ed25519Keypair, KeypairData};
use ssh_key::{LineEnding, PrivateKey, PublicKey};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::Builder;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// An ephemeral Ed25519 keypair generated for a single sandbox lease.
///
/// The private key lives on disk only inside `private_key_path`'s parent
/// directory (0700, `lsbx-key-*` prefix); nothing here is ever handed to
/// `lsbx-store` — the state store only persists `key_path`/`key_dir` as
/// string references (see Unit 01's `SandboxRecord`), never key bytes.
pub struct EphemeralKeypair {
    /// 0600, inside a 0700 temp directory prefixed `lsbx-key-`.
    pub private_key_path: PathBuf,
    /// `"ssh-ed25519 <base64> lsbx:<label>"` — a complete OpenSSH
    /// `authorized_keys` line, byte-compatible with what OpenSSH/cloud-init
    /// expect.
    pub public_key_line: String,
    pub label: String,
}

/// Generates a real Ed25519 keypair natively via `ed25519-dalek` (no
/// `ssh-keygen` subprocess), writes the private half into a fresh
/// `lsbx-key-*` temp directory at 0600 (dir at 0700), and returns the public
/// half as a complete OpenSSH line tagged `lsbx:<label>` — the same
/// key-comment convention the `exedev` backend's reaper pattern-matches on.
pub fn generate_ephemeral_keypair(label: &str) -> Result<EphemeralKeypair, LsbxError> {
    let signing_key = SigningKey::generate(&mut OsRng);

    let temp_dir = Builder::new().prefix("lsbx-key-").tempdir().map_err(|e| {
        LsbxError::ContractViolated(format!(
            "failed to create temp dir for ephemeral key: {}",
            e
        ))
    })?;

    #[cfg(unix)]
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).map_err(|e| {
        LsbxError::ContractViolated(format!("failed to set temp dir permissions: {}", e))
    })?;

    // `into_path()` intentionally leaks the `TempDir` guard: the directory
    // and its contents must outlive this function call (the key is used for
    // the lifetime of the sandbox lease) and are only removed later, and
    // explicitly, by `cleanup_keypair` on sandbox destroy/reap.
    #[allow(deprecated)]
    let dir_path = temp_dir.into_path();
    let private_key_path = dir_path.join("id_ed25519");

    let keypair_data = KeypairData::from(Ed25519Keypair::from(&signing_key));
    let private_key = PrivateKey::new(keypair_data, format!("lsbx:{}", label)).map_err(|e| {
        LsbxError::ContractViolated(format!(
            "failed to construct ephemeral ssh private key: {}",
            e
        ))
    })?;

    let private_key_openssh = private_key.to_openssh(LineEnding::LF).map_err(|e| {
        LsbxError::ContractViolated(format!("failed to serialize ephemeral private key: {}", e))
    })?;

    let mut open_options = fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    #[cfg(unix)]
    open_options.mode(0o600);

    let mut file = open_options.open(&private_key_path).map_err(|e| {
        LsbxError::ContractViolated(format!("failed to create private key file: {}", e))
    })?;
    file.write_all(private_key_openssh.as_bytes())
        .map_err(|e| {
            LsbxError::ContractViolated(format!("failed to write private key file: {}", e))
        })?;

    let public_key: PublicKey = PublicKey::from(&private_key);
    let public_key_line = public_key.to_openssh().map_err(|e| {
        LsbxError::ContractViolated(format!("failed to serialize ephemeral public key: {}", e))
    })?;

    Ok(EphemeralKeypair {
        private_key_path,
        public_key_line,
        label: label.to_string(),
    })
}

/// Removes the temp directory and its contents. Called on sandbox
/// destroy/reap. A no-op (not an error) if the directory is already gone.
pub fn cleanup_keypair(keypair: &EphemeralKeypair) -> Result<(), LsbxError> {
    let Some(parent) = keypair.private_key_path.parent() else {
        return Ok(());
    };

    let is_lsbx_key_dir = parent
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("lsbx-key-"));
    if !is_lsbx_key_dir {
        return Ok(());
    }

    match fs::remove_dir_all(parent) {
        Ok(()) => Ok(()),
        // Already gone (e.g. a concurrent cleanup, or the reaper racing a
        // manual `destroy`) — not an error, per house convention I/O-"not
        // found" maps to `LsbxError::NotFound`, but here it's the expected
        // steady state we want to swallow, not surface.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LsbxError::ContractViolated(format!(
            "failed to remove ephemeral key directory {}: {}",
            parent.display(),
            e
        ))),
    }
}
