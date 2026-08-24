// See crates/lsbx-kernel/tests/test_kernel.rs for why this allow is scoped
// to test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::SandboxRecord;
use lsbx_store::sandbox_store::SandboxStore;
use std::os::unix::fs::PermissionsExt;

fn sample_record(id: &str) -> SandboxRecord {
    SandboxRecord {
        id: id.to_string(),
        name: "test-sandbox".to_string(),
        host: "localhost".to_string(),
        profile: "default".to_string(),
        flavor: "default".to_string(),
        streaming: "none".to_string(),
        username: Some("test".to_string()),
        key_name: Some("test-key".to_string()),
        key_path: Some("/tmp/test-key".to_string()),
        key_dir: Some("/tmp".to_string()),
        pubkey: Some("ssh-ed25519 AAAA...".to_string()),
        task_id: Some("task-123".to_string()),
        created_at: Some("2026-08-24T00:00:00Z".to_string()),
        lease_expires_at: Some("2026-08-24T01:00:00Z".to_string()),
        vm_tag: Some("lsbx-test".to_string()),
        https_url: None,
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let record = sample_record("sbx-round-trip");

    store.save(&record).unwrap();
    let loaded = store.load(&record.id).unwrap();

    assert_eq!(loaded.id, record.id);
    assert_eq!(loaded.name, record.name);
    assert_eq!(loaded.streaming, record.streaming);
    assert_eq!(loaded.username, record.username);
    assert_eq!(loaded.extra, record.extra);
}

#[test]
fn save_writes_enveloped_current_schema_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let record = sample_record("sbx-envelope-shape");

    store.save(&record).unwrap();

    let raw = std::fs::read_to_string(dir.path().join("state").join("sbx-envelope-shape.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value.get("schema_version").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(value.get("kind").and_then(|v| v.as_str()), Some("sandbox"));
    assert!(value.get("sandbox").is_some(), "expected a nested `sandbox` object per the envelope schema");
    assert_eq!(
        value.get("sandbox").and_then(|s| s.get("id")).and_then(|v| v.as_str()),
        Some("sbx-envelope-shape")
    );
}

#[test]
fn save_sets_directory_and_file_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let record = sample_record("sbx-perms");

    store.save(&record).unwrap();

    let state_dir = dir.path().join("state");
    let dir_mode = std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);

    let file_mode = std::fs::metadata(state_dir.join("sbx-perms.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
}

#[test]
fn load_migrates_legacy_flat_record_transparently() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let legacy_json = r#"{
        "id": "sbx-legacy",
        "name": "legacy-sandbox",
        "host": "localhost",
        "profile": "default",
        "flavor": "default",
        "streaming": "none",
        "cleanup_failed": false,
        "extra": {}
    }"#;
    std::fs::write(state_dir.join("sbx-legacy.json"), legacy_json).unwrap();

    // The caller never needs to know this record was legacy-flat on disk.
    let loaded = store.load("sbx-legacy").unwrap();
    assert_eq!(loaded.id, "sbx-legacy");
    assert_eq!(loaded.streaming, "none");
    assert_eq!(loaded.username, None);
}

#[test]
fn load_missing_record_returns_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());

    match store.load("does-not-exist") {
        Err(LsbxError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {:?}", other.map(|r| r.id)),
    }
}

#[test]
fn delete_is_idempotent_on_missing_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());

    // Deleting a record that was never saved must not be an error — the
    // interface contract gives `delete` no `NotFound`-on-absence comment,
    // unlike `load`, which explicitly documents that behavior.
    store.delete("never-existed").unwrap();
}

#[test]
fn delete_removes_existing_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    let record = sample_record("sbx-to-delete");
    store.save(&record).unwrap();

    store.delete(&record.id).unwrap();

    match store.load(&record.id) {
        Err(LsbxError::NotFound(_)) => {}
        other => panic!("expected NotFound after delete, got {:?}", other.map(|r| r.id)),
    }
}

#[test]
fn list_returns_empty_vec_when_state_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());

    // `SandboxRecord` (Unit 01's type) does not derive `PartialEq`, so
    // compare via `is_empty()` rather than `assert_eq!` against `Vec::new()`.
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn list_returns_all_saved_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());

    store.save(&sample_record("sbx-a")).unwrap();
    store.save(&sample_record("sbx-b")).unwrap();
    store.save(&sample_record("sbx-c")).unwrap();

    let mut ids: Vec<String> = store.list().unwrap().into_iter().map(|r| r.id).collect();
    ids.sort();
    assert_eq!(ids, vec!["sbx-a".to_string(), "sbx-b".to_string(), "sbx-c".to_string()]);
}

#[test]
fn save_is_atomic_via_temp_file_rename() {
    // Not directly observable from the public API in a single-threaded
    // test, but we can at least assert no stray temp files are left behind
    // in the store directory after a successful save — `tempfile::persist`
    // renaming into place, rather than a partial write, is what guarantees
    // this.
    let dir = tempfile::tempdir().unwrap();
    let store = SandboxStore::new(dir.path().to_path_buf());
    store.save(&sample_record("sbx-atomic")).unwrap();

    let entries: Vec<_> = std::fs::read_dir(dir.path().join("state"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["sbx-atomic.json".to_string()]);
}
