#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lsbx_gateway::routes::{build_router, GatewayConfig};
use lsbx_golden::registry::{
    GoldenConfig, GoldenFlavor, GoldenMode, ImageRegistry, ProfileConfig, StreamingMode,
};
use lsbx_kernel::backend::{
    Backend, BackendCapabilities, CommandOutput, CreateFromGoldenRequest, CreatedVm,
};
use lsbx_kernel::clock::SystemClock;
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::SandboxRecord;
use lsbx_ops::LsbxOps;
use lsbx_store::ci_job_store::CiJobStore;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceExt;

const TOKEN: &str = "display-test-token";
const SANDBOX_ID: &str = "sbx-display-test";
const GOLDEN_BASE: &str = "win11-display-v2";

#[derive(Clone)]
struct RecordingControl {
    commands: Arc<Mutex<Vec<Vec<String>>>>,
    block_next: Arc<AtomicBool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl RecordingControl {
    fn new() -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            block_next: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

struct RecordingBackend {
    control: RecordingControl,
}

#[async_trait]
impl Backend for RecordingBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn create_from_golden(
        &self,
        _req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, LsbxError> {
        unreachable!("display route tests use a pre-populated store")
    }

    async fn run(
        &self,
        _vm_tag: &str,
        command: &[String],
        _timeout: Duration,
        _identity_file: Option<&Path>,
    ) -> Result<CommandOutput, LsbxError> {
        self.control.commands.lock().unwrap().push(command.to_vec());
        if self.control.block_next.swap(false, Ordering::SeqCst) {
            self.control.entered.add_permits(1);
            self.control.release.acquire().await.unwrap().forget();
        }
        Ok(CommandOutput {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    async fn put_file(
        &self,
        _vm_tag: &str,
        _source: &Path,
        _destination: &str,
        _identity_file: Option<&Path>,
    ) -> Result<(), LsbxError> {
        unreachable!()
    }

    async fn get_file(
        &self,
        _vm_tag: &str,
        _source: &str,
        _destination: &Path,
        _identity_file: Option<&Path>,
    ) -> Result<(), LsbxError> {
        unreachable!()
    }

    async fn destroy(&self, _vm_tag: &str) -> Result<(), LsbxError> {
        unreachable!()
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        Ok(vec!["display-vm".to_string()])
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        unreachable!()
    }
}

fn golden() -> GoldenConfig {
    GoldenConfig {
        key: "win11-display".to_string(),
        flavor: GoldenFlavor::Desktop,
        os: "windows".to_string(),
        base: GOLDEN_BASE.to_string(),
        mode: GoldenMode::Copy,
        cpu: 8,
        memory: "32GB".to_string(),
        disk: None,
        streaming: StreamingMode::Novnc,
        capabilities: vec!["desktop".to_string()],
        healthcheck: vec![],
        repo: None,
        content_hash: None,
        description: "display route test golden".to_string(),
    }
}

fn record() -> SandboxRecord {
    SandboxRecord {
        id: SANDBOX_ID.to_string(),
        name: "display-test".to_string(),
        host: "192.0.2.10".to_string(),
        profile: "win11".to_string(),
        flavor: "desktop".to_string(),
        streaming: "novnc".to_string(),
        username: Some("lsbx".to_string()),
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at: None,
        vm_tag: Some("display-vm".to_string()),
        https_url: Some("https://display.invalid".to_string()),
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

fn router(configured_golden: Option<&str>) -> (axum::Router, RecordingControl, tempfile::TempDir) {
    router_with(configured_golden, golden(), record())
}

fn router_with(
    configured_golden: Option<&str>,
    golden: GoldenConfig,
    record: SandboxRecord,
) -> (axum::Router, RecordingControl, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(dir.path().to_path_buf());
    sandbox_store.save(&record).expect("save sandbox");

    let mut profiles = HashMap::new();
    profiles.insert(
        "win11".to_string(),
        ProfileConfig::Golden {
            golden: "win11-display".to_string(),
        },
    );
    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![golden],
        profiles,
    };
    let control = RecordingControl::new();
    let ops = LsbxOps::new(
        Box::new(RecordingBackend {
            control: control.clone(),
        }),
        "recording".to_string(),
        sandbox_store,
        CiJobStore::new(dir.path().to_path_buf()),
        registry,
        Box::new(SystemClock),
    );
    let config = GatewayConfig {
        token: Some(TOKEN.to_string()),
        allow_local_files: false,
        insecure: false,
        max_sandboxes: 8,
        windows_display_resize_golden: configured_golden.map(str::to_string),
        rate_limit: lsbx_gateway::RateLimitConfig {
            requests_per_minute: 6000,
            burst: 1000,
        },
    };
    (build_router(Arc::new(ops), config), control, dir)
}

fn request(method: &str, body: Option<serde_json::Value>, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/sandboxes/{SANDBOX_ID}/display"));
    if authenticated {
        builder = builder.header("authorization", format!("Bearer {TOKEN}"));
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

async fn json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn both_display_verbs_require_gateway_authentication() {
    let (router, _control, _dir) = router(Some(GOLDEN_BASE));
    for method in ["GET", "POST"] {
        let body = (method == "POST").then(|| serde_json::json!({"preset":"1920x1080"}));
        let response = router
            .clone()
            .oneshot(request(method, body, false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}

#[tokio::test]
async fn get_exposes_only_the_fixed_tested_presets_for_an_exact_eligible_golden() {
    let (router, _control, _dir) = router(Some(GOLDEN_BASE));
    let response = router.oneshot(request("GET", None, true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["supported"], true);
    let presets = body["data"]["presets"].as_array().unwrap();
    assert!(presets.iter().any(|value| value == "1920x1080"));
    assert!(!presets.iter().any(|value| value == "3440x1440"));
}

#[tokio::test]
async fn post_rejects_unlisted_presets_and_ineligible_or_disabled_goldens() {
    let (eligible, control, _dir) = router(Some(GOLDEN_BASE));
    let invalid = eligible
        .oneshot(request(
            "POST",
            Some(serde_json::json!({"preset":"3440x1440"})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(control.commands.lock().unwrap().is_empty());

    for configured in [None, Some("some-other-backing")] {
        let (router, control, _dir) = router(configured);
        let response = router
            .oneshot(request(
                "POST",
                Some(serde_json::json!({"preset":"1920x1080"})),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(control.commands.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn exact_backing_still_requires_windows_and_novnc_in_manifest_and_record() {
    let mut linux = golden();
    linux.os = "linux".to_string();
    let mut manifest_without_novnc = golden();
    manifest_without_novnc.streaming = StreamingMode::None;
    let mut record_without_novnc = record();
    record_without_novnc.streaming = "none".to_string();

    for (candidate_golden, candidate_record) in [
        (linux, record()),
        (manifest_without_novnc, record()),
        (golden(), record_without_novnc),
    ] {
        let (router, control, _dir) =
            router_with(Some(GOLDEN_BASE), candidate_golden, candidate_record);
        let response = router
            .oneshot(request(
                "POST",
                Some(serde_json::json!({"preset":"1920x1080"})),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(control.commands.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn post_runs_only_the_fixed_helper_path_with_the_validated_preset() {
    let (router, control, _dir) = router(Some(GOLDEN_BASE));
    let response = router
        .oneshot(request(
            "POST",
            Some(serde_json::json!({"preset":"1600x900"})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["preset"], "1600x900");
    assert_eq!(body["data"]["applied"], true);

    let commands = control.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0][0], "cmd");
    assert_eq!(commands[0][1], "/c");
    assert_eq!(
        commands[0][2],
        r"powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\lsbx\set-win11-display.ps1 -Preset 1600x900"
    );
}

#[tokio::test]
async fn a_second_display_post_is_rejected_instead_of_queued() {
    let (router, control, _dir) = router(Some(GOLDEN_BASE));
    control.block_next.store(true, Ordering::SeqCst);

    let first_router = router.clone();
    let first = tokio::spawn(async move {
        first_router
            .oneshot(request(
                "POST",
                Some(serde_json::json!({"preset":"1600x900"})),
                true,
            ))
            .await
            .unwrap()
    });
    control.entered.acquire().await.unwrap().forget();

    let second = router
        .oneshot(request(
            "POST",
            Some(serde_json::json!({"preset":"1920x1080"})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);

    control.release.add_permits(1);
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    assert_eq!(control.commands.lock().unwrap().len(), 1);
}
