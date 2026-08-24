use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::{SandboxRecord, SandboxRecordEnvelope};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::NamedTempFile;

/// `schema_version` for the current (non-legacy) on-disk `SandboxRecord`
/// envelope. Bumping this is a Unit 01 concern (it owns the record shape);
/// this crate only ever writes `1`.
const SCHEMA_VERSION: u32 = 1;
const KIND: &str = "sandbox";

/// Maps a raw `std::io::Error` onto `LsbxError` per house convention:
/// `NotFound` I/O errors become `LsbxError::NotFound`, everything else
/// becomes `LsbxError::ContractViolated`.
fn map_io_err(context: &str, e: std::io::Error) -> LsbxError {
    if e.kind() == std::io::ErrorKind::NotFound {
        LsbxError::NotFound(format!("{}: {}", context, e))
    } else {
        LsbxError::ContractViolated(format!("{}: {}", context, e))
    }
}

fn map_json_err(context: &str, e: serde_json::Error) -> LsbxError {
    LsbxError::ContractViolated(format!("{}: {}", context, e))
}

/// One JSON file per sandbox at `<state_dir>/state/<id>.json`. The current
/// (non-legacy) on-disk shape is the `SandboxRecordEnvelope`
/// (`{"schema_version":1,"kind":"sandbox","sandbox":{...}}`) — SPEC.md §4.1
/// states `SandboxRecord` "preserves the existing schema exactly
/// (`schema_version`, `kind: "sandbox"`, ...)", and Unit 01's own
/// `SandboxRecord::from_legacy_flat` only makes sense as a *migration path*
/// if the current shape is distinguishable from the legacy flat shape by
/// the presence of that envelope. `load` transparently unwraps the
/// envelope, or falls back to `SandboxRecord::from_legacy_flat` for
/// unversioned records, so callers never need to know which shape was on
/// disk — matching this unit's acceptance criteria.
pub struct SandboxStore {
    state_dir: PathBuf,
}

impl SandboxStore {
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    fn store_dir(&self) -> PathBuf {
        self.state_dir.join("state")
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.store_dir().join(format!("{}.json", id))
    }

    pub fn save(&self, record: &SandboxRecord) -> Result<(), LsbxError> {
        let store_dir = self.store_dir();
        fs::create_dir_all(&store_dir).map_err(|e| map_io_err("failed to create state dir", e))?;
        let mut perms = fs::metadata(&store_dir)
            .map_err(|e| map_io_err("failed to stat state dir", e))?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&store_dir, perms).map_err(|e| map_io_err("failed to chmod state dir", e))?;

        let dest_path = store_dir.join(format!("{}.json", record.id));
        let mut temp_file = NamedTempFile::new_in(&store_dir)
            .map_err(|e| map_io_err("failed to create temp file", e))?;

        let envelope = SandboxRecordEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            sandbox: record.clone(),
        };
        serde_json::to_writer(&mut temp_file, &envelope)
            .map_err(|e| map_json_err("failed to serialize sandbox record", e))?;

        let mut perms = temp_file
            .as_file()
            .metadata()
            .map_err(|e| map_io_err("failed to stat temp file", e))?
            .permissions();
        perms.set_mode(0o600);
        temp_file
            .as_file()
            .set_permissions(perms)
            .map_err(|e| map_io_err("failed to chmod temp file", e))?;

        temp_file
            .persist(&dest_path)
            .map_err(|e| LsbxError::ContractViolated(format!("failed to atomically rename into place: {}", e)))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<SandboxRecord, LsbxError> {
        let dest_path = self.record_path(id);
        let file = fs::File::open(&dest_path).map_err(|e| map_io_err(&format!("failed to open sandbox record {}", id), e))?;

        let value: serde_json::Value = serde_json::from_reader(file)
            .map_err(|e| map_json_err("failed to parse sandbox record json", e))?;

        if value.get("schema_version").is_some() {
            let envelope: SandboxRecordEnvelope = serde_json::from_value(value)
                .map_err(|e| map_json_err("failed to parse sandbox record envelope", e))?;
            Ok(envelope.sandbox)
        } else {
            SandboxRecord::from_legacy_flat(value)
        }
    }

    /// Idempotent: deleting a sandbox record that doesn't exist is not an
    /// error (matches the interface contract, which — unlike `load`, whose
    /// doc comment explicitly calls out `NotFound` on absence — makes no
    /// such statement for `delete`; a caller reaping an already-gone record
    /// should not have to special-case that).
    pub fn delete(&self, id: &str) -> Result<(), LsbxError> {
        let dest_path = self.record_path(id);
        match fs::remove_file(&dest_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(map_io_err(&format!("failed to delete sandbox record {}", id), e)),
        }
    }

    pub fn list(&self) -> Result<Vec<SandboxRecord>, LsbxError> {
        let store_dir = self.store_dir();
        if !store_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for entry in fs::read_dir(&store_dir).map_err(|e| map_io_err("failed to read state dir", e))? {
            let entry = entry.map_err(|e| map_io_err("failed to read state dir entry", e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    records.push(self.load(stem)?);
                }
            }
        }
        Ok(records)
    }
}
