#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lsbx_gateway::moonlight::{
    MoonlightConfig, MoonlightManager, MoonlightStatus, RelayBinding, RelayControl,
};
use lsbx_gateway::routes::{build_router, build_router_with_moonlight, GatewayConfig};
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
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-moonlight-token";
const GOLDEN_BASE: &str = "win11-desktop-audio-canary";

#[derive(Clone, Default)]
struct MockRelay {
    activated: Arc<Mutex<Vec<RelayBinding>>>,
    deactivated: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl RelayControl for MockRelay {
    async fn activate(&self, binding: RelayBinding) -> Result<(), LsbxError> {
        self.activated.lock().unwrap().push(binding);
        Ok(())
    }

    async fn deactivate(&self, sandbox_id: &str) {
        self.deactivated
            .lock()
            .unwrap()
            .push(sandbox_id.to_string());
    }
}

struct MockBackend {
    live_vms: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Backend for MockBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn create_from_golden(
        &self,
        _req: CreateFromGoldenRequest<'_>,
    ) -> Result<CreatedVm, LsbxError> {
        unreachable!("pre-populated store in tests")
    }

    async fn run(
        &self,
        _vm_tag: &str,
        _command: &[String],
        _timeout: Duration,
        _identity_file: Option<&Path>,
    ) -> Result<CommandOutput, LsbxError> {
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

    async fn destroy(&self, vm_tag: &str) -> Result<(), LsbxError> {
        self.live_vms.lock().unwrap().retain(|tag| tag != vm_tag);
        Ok(())
    }

    async fn list_vms(&self) -> Result<Vec<String>, LsbxError> {
        Ok(self.live_vms.lock().unwrap().clone())
    }

    async fn rename_vm(&self, _old_tag: &str, _new_tag: &str) -> Result<(), LsbxError> {
        unreachable!()
    }
}

fn test_golden() -> GoldenConfig {
    GoldenConfig {
        key: "win11-audio".to_string(),
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
        description: "windows audio canary golden".to_string(),
    }
}

fn make_record(id: &str, vm_tag: &str, host: &str, profile: &str) -> SandboxRecord {
    let lease = chrono::Utc::now() + chrono::Duration::hours(2);
    SandboxRecord {
        id: id.to_string(),
        name: format!("vm-{id}"),
        host: host.to_string(),
        profile: profile.to_string(),
        flavor: "desktop".to_string(),
        streaming: "novnc".to_string(),
        username: Some("lsbx".to_string()),
        key_name: None,
        key_path: None,
        key_dir: None,
        pubkey: None,
        task_id: None,
        created_at: None,
        lease_expires_at: Some(lease.to_rfc3339()),
        vm_tag: Some(vm_tag.to_string()),
        https_url: None,
        cleanup_failed: false,
        repository_key: None,
        repository: None,
        extra: serde_json::Map::new(),
    }
}

fn gateway_config() -> GatewayConfig {
    GatewayConfig {
        token: Some(TOKEN.to_string()),
        allow_local_files: false,
        insecure: false,
        max_sandboxes: 8,
        windows_display_resize_golden: None,
        rate_limit: lsbx_gateway::RateLimitConfig {
            requests_per_minute: 6000,
            burst: 1000,
        },
    }
}

fn moonlight_config(slots: u8) -> MoonlightConfig {
    MoonlightConfig {
        bind_ip: Ipv4Addr::new(100, 125, 210, 60),
        allowed_client_ip: Ipv4Addr::new(100, 102, 200, 73),
        exact_windows_novnc_golden: GOLDEN_BASE.to_string(),
        slots,
        host_anchor: 50_005,
        watch_interval: Duration::from_secs(1),
    }
}

struct TestHarness {
    router: axum::Router,
    relay: MockRelay,
    live_vms: Arc<Mutex<Vec<String>>>,
    manager: Option<Arc<MoonlightManager>>,
    _dir: tempfile::TempDir,
}

fn build_harness(slots: u8, enable_moonlight: bool) -> TestHarness {
    let dir = tempfile::tempdir().expect("tempdir");
    let sandbox_store = SandboxStore::new(dir.path().to_path_buf());
    let ci_job_store = CiJobStore::new(dir.path().to_path_buf());

    let mut profiles = HashMap::new();
    profiles.insert(
        "win11-audio".to_string(),
        ProfileConfig::Golden {
            golden: "win11-audio".to_string(),
        },
    );
    profiles.insert(
        "linux-web".to_string(),
        ProfileConfig::Golden {
            golden: "linux-web".to_string(),
        },
    );

    let linux_golden = GoldenConfig {
        key: "linux-web".to_string(),
        flavor: GoldenFlavor::Desktop,
        os: "linux".to_string(),
        base: "lsbx-web-v1".to_string(),
        mode: GoldenMode::Copy,
        cpu: 2,
        memory: "4GB".to_string(),
        disk: None,
        streaming: StreamingMode::Novnc,
        capabilities: vec!["desktop".to_string()],
        healthcheck: vec![],
        repo: None,
        content_hash: None,
        description: "linux web golden".to_string(),
    };

    let registry = ImageRegistry {
        images: vec![],
        goldens: vec![test_golden(), linux_golden],
        profiles,
    };

    let live_vms = Arc::new(Mutex::new(vec![
        "vm-tag-1".to_string(),
        "vm-tag-2".to_string(),
        "vm-tag-3".to_string(),
        "vm-tag-linux".to_string(),
    ]));

    let backend = Box::new(MockBackend {
        live_vms: Arc::clone(&live_vms),
    });

    let ops = Arc::new(LsbxOps::new(
        backend,
        "libvirt".to_string(),
        SandboxStore::new(dir.path().to_path_buf()),
        ci_job_store,
        registry,
        Box::new(SystemClock),
    ));

    let relay = MockRelay::default();
    let (router, manager) = if enable_moonlight {
        let manager = MoonlightManager::new(
            Arc::clone(&ops),
            sandbox_store,
            moonlight_config(slots),
            Arc::new(relay.clone()),
        );
        (
            build_router_with_moonlight(ops, gateway_config(), Arc::clone(&manager)),
            Some(manager),
        )
    } else {
        (build_router(ops, gateway_config()), None)
    };

    TestHarness {
        router,
        relay,
        live_vms,
        manager,
        _dir: dir,
    }
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body should be valid JSON")
}

#[tokio::test]
async fn moonlight_disabled_gateway_returns_usage_error() {
    let harness = build_harness(1, false);

    let req = Request::builder()
        .uri("/moonlight")
        .method("GET")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = body_json(res).await;
    assert_eq!(json["status"], "error");
    assert_eq!(json["code"], 2);
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Moonlight media slots are disabled"));

    let req = Request::builder()
        .uri("/sandboxes/sbx-any/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn moonlight_endpoints_require_authentication() {
    let harness = build_harness(1, true);

    let req = Request::builder()
        .uri("/moonlight")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .uri("/sandboxes/sbx-1/moonlight")
        .method("POST")
        .header("authorization", "Bearer wrong-token")
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn moonlight_rejects_ineligible_sandboxes() {
    let harness = build_harness(2, true);

    // 1. Unknown sandbox id -> 404
    let req = Request::builder()
        .uri("/sandboxes/sbx-unknown/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 2. Linux sandbox -> 400 Usage ("not on the exact configured Windows/noVNC golden")
    let store = SandboxStore::new(harness._dir.path().to_path_buf());
    let linux_rec = make_record("sbx-linux", "vm-tag-linux", "192.168.122.99", "linux-web");
    store.save(&linux_rec).unwrap();

    let req = Request::builder()
        .uri("/sandboxes/sbx-linux/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = body_json(res).await;
    assert_eq!(json["code"], 2);

    // 3. Offline VM (vm_tag not in live_vms) -> 404
    let offline_rec = make_record("sbx-offline", "vm-tag-dead", "192.168.122.101", "win11-audio");
    store.save(&offline_rec).unwrap();

    let req = Request::builder()
        .uri("/sandboxes/sbx-offline/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let json = body_json(res).await;
    assert!(json["message"].as_str().unwrap().contains("not live"));

    // 4. Expired lease -> 404
    let mut expired_rec =
        make_record("sbx-expired", "vm-tag-1", "192.168.122.102", "win11-audio");
    expired_rec.lease_expires_at =
        Some((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
    store.save(&expired_rec).unwrap();

    let req = Request::builder()
        .uri("/sandboxes/sbx-expired/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let json = body_json(res).await;
    assert!(json["message"].as_str().unwrap().contains("expired"));
}

#[tokio::test]
async fn moonlight_acquire_status_list_and_release_lifecycle() {
    let harness = build_harness(2, true);
    let store = SandboxStore::new(harness._dir.path().to_path_buf());

    let rec1 = make_record("sbx-win-1", "vm-tag-1", "192.168.122.51", "win11-audio");
    store.save(&rec1).unwrap();

    // Initially status is null (no slot)
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("GET")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    assert_eq!(json["data"], serde_json::Value::Null);

    // Acquire slot for sbx-win-1
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    let data = &json["data"];
    assert_eq!(data["sandbox_id"], "sbx-win-1");
    assert_eq!(data["slot"], 0);
    assert_eq!(data["address"], "100.125.210.60:50005");
    assert_eq!(data["host_anchor"], 50005);
    assert_eq!(data["guest_ip"], "192.168.122.51");
    assert_eq!(data["active"], true);

    // Relay activation verified
    let activated = harness.relay.activated.lock().unwrap().clone();
    assert_eq!(activated.len(), 1);
    assert_eq!(activated[0].sandbox_id, "sbx-win-1");
    assert_eq!(activated[0].host_anchor, 50005);
    assert_eq!(activated[0].guest_anchor, 47989);
    assert_eq!(activated[0].guest_ip, Ipv4Addr::new(192, 168, 122, 51));

    // GET /sandboxes/{id}/moonlight returns the allocated status
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("GET")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    assert_eq!(json["data"]["slot"], 0);
    assert_eq!(json["data"]["active"], true);

    // GET /moonlight lists the active slot
    let req = Request::builder()
        .uri("/moonlight")
        .method("GET")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res).await;
    let list: Vec<MoonlightStatus> = serde_json::from_value(json["data"].clone()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].sandbox_id, "sbx-win-1");
    assert_eq!(list[0].slot, 0);

    // DELETE /sandboxes/{id}/moonlight releases the slot
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("DELETE")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Relay deactivated
    let deactivated = harness.relay.deactivated.lock().unwrap().clone();
    assert_eq!(deactivated, vec!["sbx-win-1"]);

    // Subsequent GET returns null
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("GET")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    let json = body_json(res).await;
    assert_eq!(json["data"], serde_json::Value::Null);
}

#[tokio::test]
async fn moonlight_idempotent_acquire_and_slot_exhaustion() {
    let harness = build_harness(2, true);
    let store = SandboxStore::new(harness._dir.path().to_path_buf());

    let rec1 = make_record("sbx-win-1", "vm-tag-1", "192.168.122.51", "win11-audio");
    let rec2 = make_record("sbx-win-2", "vm-tag-2", "192.168.122.52", "win11-audio");
    let rec3 = make_record("sbx-win-3", "vm-tag-3", "192.168.122.53", "win11-audio");
    store.save(&rec1).unwrap();
    store.save(&rec2).unwrap();
    store.save(&rec3).unwrap();

    // Acquire slot for sbx-win-1 -> gets slot 0
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(body_json(res).await["data"]["slot"], 0);

    // Calling acquire again on sbx-win-1 is idempotent: returns existing slot 0
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(body_json(res).await["data"]["slot"], 0);

    // Acquire slot for sbx-win-2 -> gets slot 1 (anchor = 50105)
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-2/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    let json = body_json(res).await;
    assert_eq!(json["data"]["slot"], 1);
    assert_eq!(json["data"]["host_anchor"], 50105);
    assert_eq!(json["data"]["address"], "100.125.210.60:50105");

    // Acquire slot for sbx-win-3 -> fails with 409 LockContention (exhaustion)
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-3/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let json = body_json(res).await;
    assert_eq!(json["code"], 6);
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("no Moonlight media slot is free"));

    // Release sbx-win-1
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-1/moonlight")
        .method("DELETE")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Now sbx-win-3 can acquire freed slot 0!
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-3/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["data"]["slot"], 0);
}

#[tokio::test]
async fn moonlight_slot_released_on_sandbox_delete() {
    let harness = build_harness(1, true);
    let store = SandboxStore::new(harness._dir.path().to_path_buf());

    let rec = make_record("sbx-win-del", "vm-tag-1", "192.168.122.51", "win11-audio");
    store.save(&rec).unwrap();

    // Acquire slot
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-del/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Delete sandbox via DELETE /sandboxes/{id}
    let req = Request::builder()
        .uri("/sandboxes/sbx-win-del")
        .method("DELETE")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Verify relay deactivation was called on delete
    let deactivated = harness.relay.deactivated.lock().unwrap().clone();
    assert!(deactivated.contains(&"sbx-win-del".to_string()));
}

#[tokio::test]
async fn moonlight_reaper_sweeps_terminated_vm() {
    let harness = build_harness(1, true);
    let store = SandboxStore::new(harness._dir.path().to_path_buf());

    let rec = make_record("sbx-win-reap", "vm-tag-1", "192.168.122.51", "win11-audio");
    store.save(&rec).unwrap();

    let req = Request::builder()
        .uri("/sandboxes/sbx-win-reap/moonlight")
        .method("POST")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = harness.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // VM is terminated in backend
    harness.live_vms.lock().unwrap().retain(|tag| tag != "vm-tag-1");

    // Run reaper sweep
    harness.manager.as_ref().unwrap().reap_once().await;

    // Verify relay was deactivated
    let deactivated = harness.relay.deactivated.lock().unwrap().clone();
    assert!(deactivated.contains(&"sbx-win-reap".to_string()));

    // Verify record extra is cleaned up in store
    let record = store.load("sbx-win-reap").unwrap();
    assert!(!record.extra.contains_key("moonlight"));
}
