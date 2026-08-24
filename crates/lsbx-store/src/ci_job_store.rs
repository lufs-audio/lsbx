use crate::lock::{LockGuard, LockSentinel};
use lsbx_kernel::error::LsbxError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::NamedTempFile;

const SCHEMA_VERSION: u32 = 1;
const KIND: &str = "ci-job";

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiJobRecord {
    pub job_id: String,
    pub queue_label: String,
    pub runner_group: Option<String>,
    pub host_prefix: Option<String>,
    pub phase: String, // "dispatched" | "running" | "completed" | "failed"
    pub sandbox_id: Option<String>,
    pub runner_name: Option<String>,
    pub dispatched_job_name: Option<String>,
    pub actual_job_id: Option<String>,
    pub actual_job_name: Option<String>,
    pub diverged: bool,
    pub repository: String,
    pub created_at: String,
    pub updated_at: String,
    pub lease_expires_at: Option<String>,
    pub last_error: Option<String>,
}

/// On-disk envelope: `{"schema_version":1,"kind":"ci-job","job":{...}}`,
/// exactly per this unit's own interface contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CiJobEnvelope {
    schema_version: u32,
    kind: String,
    job: CiJobRecord,
}

/// One JSON file per CI job at `<state_dir>/ci-broker/<job_id>.json`, and
/// the single `broker_lock()` that replaces the existing system's separately
/// hand-rolled `BrokerLock` with the one `LockSentinel` primitive every
/// process-level lock in `lsbx` is built on.
pub struct CiJobStore {
    state_dir: PathBuf,
}

impl CiJobStore {
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    fn store_dir(&self) -> PathBuf {
        self.state_dir.join("ci-broker")
    }

    fn record_path(&self, job_id: &str) -> PathBuf {
        self.store_dir().join(format!("{}.json", job_id))
    }

    pub fn save(&self, record: &CiJobRecord) -> Result<(), LsbxError> {
        let store_dir = self.store_dir();
        fs::create_dir_all(&store_dir).map_err(|e| map_io_err("failed to create ci-broker dir", e))?;
        let mut perms = fs::metadata(&store_dir)
            .map_err(|e| map_io_err("failed to stat ci-broker dir", e))?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&store_dir, perms).map_err(|e| map_io_err("failed to chmod ci-broker dir", e))?;

        let dest_path = store_dir.join(format!("{}.json", record.job_id));
        let mut temp_file = NamedTempFile::new_in(&store_dir)
            .map_err(|e| map_io_err("failed to create temp file", e))?;

        let envelope = CiJobEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            job: record.clone(),
        };
        serde_json::to_writer(&mut temp_file, &envelope)
            .map_err(|e| map_json_err("failed to serialize ci job record", e))?;

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

    pub fn load(&self, job_id: &str) -> Result<CiJobRecord, LsbxError> {
        let dest_path = self.record_path(job_id);
        let file = fs::File::open(&dest_path)
            .map_err(|e| map_io_err(&format!("failed to open ci job record {}", job_id), e))?;

        let envelope: CiJobEnvelope = serde_json::from_reader(file)
            .map_err(|e| map_json_err("failed to parse ci job envelope", e))?;
        Ok(envelope.job)
    }

    /// Returns every job whose `phase` is not `"completed"` or `"failed"`.
    /// This unit only guarantees the record round-trips atomically — it
    /// does not own what `phase` transitions mean (Unit 18's job).
    pub fn list_in_flight(&self) -> Result<Vec<CiJobRecord>, LsbxError> {
        let store_dir = self.store_dir();
        if !store_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for entry in fs::read_dir(&store_dir).map_err(|e| map_io_err("failed to read ci-broker dir", e))? {
            let entry = entry.map_err(|e| map_io_err("failed to read ci-broker dir entry", e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let record = self.load(stem)?;
                    if record.phase != "completed" && record.phase != "failed" {
                        records.push(record);
                    }
                }
            }
        }
        Ok(records)
    }

    /// `<state_dir>/ci-broker.lock`, built from `LockSentinel::try_acquire`
    /// — not a second hand-rolled mechanism. This is the point of the unit.
    pub fn broker_lock(&self) -> Result<LockGuard, LsbxError> {
        fs::create_dir_all(&self.state_dir).map_err(|e| map_io_err("failed to create state dir", e))?;
        LockSentinel::try_acquire(&self.state_dir.join("ci-broker.lock"))
    }
}
