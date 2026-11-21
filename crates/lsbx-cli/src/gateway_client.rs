//! HTTP client for driving the single authoritative `lsbx serve` process.
//!
//! This module deliberately speaks in bytes for file transfer. A client-side
//! `put` never sends a path from the gateway host, and `get` never asks the
//! gateway to write a host path; those unsafe, server-local routes remain
//! unused.

use lsbx_golden::registry::GoldenConfig;
use lsbx_kernel::error::LsbxError;
use lsbx_kernel::types::PublicSandbox;
use reqwest::header::{HeaderValue, AUTHORIZATION, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

pub(crate) struct GatewayClient {
    base: Url,
    http: reqwest::Client,
    authorization: HeaderValue,
}

#[derive(Debug, Deserialize)]
struct WireEnvelope<T> {
    status: String,
    data: Option<T>,
    code: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireSandbox {
    id: String,
    name: String,
    host: String,
    profile: String,
    flavor: String,
    streaming: String,
    task_id: Option<String>,
    created_at: Option<String>,
    lease_expires_at: Option<String>,
    console_url: Option<String>,
    cleanup_failed: bool,
    repository: Option<String>,
}

impl From<WireSandbox> for PublicSandbox {
    fn from(value: WireSandbox) -> Self {
        Self {
            id: value.id,
            name: value.name,
            host: value.host,
            profile: value.profile,
            flavor: value.flavor,
            streaming: value.streaming,
            task_id: value.task_id,
            created_at: value.created_at,
            lease_expires_at: value.lease_expires_at,
            console_url: value.console_url,
            cleanup_failed: value.cleanup_failed,
            repository: value.repository,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct GatewayStatus {
    pub(crate) backend_name: String,
    pub(crate) backend_available: bool,
    pub(crate) sandbox_count: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GatewayExecOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Debug, Deserialize)]
struct GatewayConsole {
    console_url: Option<String>,
}

#[derive(Serialize)]
struct CreateBody<'a> {
    profile: &'a str,
    name: Option<&'a str>,
    task_id: Option<&'a str>,
    lease_secs: u64,
    ready_timeout_secs: u64,
    verify: bool,
    healthchecks: Vec<Vec<String>>,
}

#[derive(Serialize)]
struct ExecBody<'a> {
    command: &'a [String],
    timeout_secs: u64,
}

#[derive(Serialize)]
struct RenewBody {
    duration_secs: u64,
}

impl GatewayClient {
    pub(crate) fn new(
        gateway: &str,
        token: Option<&str>,
        allow_insecure_http: bool,
    ) -> Result<Self, LsbxError> {
        let mut base = Url::parse(gateway)
            .map_err(|e| LsbxError::Usage(format!("invalid gateway URL: {e}")))?;

        match base.scheme() {
            "https" => {}
            "http" if allow_insecure_http => {}
            "http" => {
                return Err(LsbxError::Usage(
                    "plaintext gateway URLs require the explicit --insecure flag".to_string(),
                ))
            }
            scheme => {
                return Err(LsbxError::Usage(format!(
                    "gateway URL scheme must be https (or http with --insecure), not '{scheme}'"
                )))
            }
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(LsbxError::Usage(
                "gateway URL must not contain credentials; use --token or LSBX_GATEWAY_TOKEN"
                    .to_string(),
            ));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(LsbxError::Usage(
                "gateway URL must not contain a query string or fragment".to_string(),
            ));
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }

        let token = token.filter(|value| !value.is_empty()).ok_or_else(|| {
            LsbxError::AuthFailed(
                "gateway token is missing; set --token or LSBX_GATEWAY_TOKEN".to_string(),
            )
        })?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                LsbxError::AuthFailed(
                    "gateway token is not a valid HTTP bearer credential".to_string(),
                )
            })?;
        authorization.set_sensitive(true);

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15 * 60))
            .build()
            .map_err(|e| {
                LsbxError::BackendUnavailable(format!("failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            base,
            http,
            authorization,
        })
    }

    pub(crate) async fn status(&self) -> Result<GatewayStatus, LsbxError> {
        self.json(Method::GET, self.endpoint(&["health"])?, None)
            .await
    }

    pub(crate) async fn list(&self) -> Result<Vec<PublicSandbox>, LsbxError> {
        let values: Vec<WireSandbox> = self
            .json(Method::GET, self.endpoint(&["sandboxes"])?, None)
            .await?;
        Ok(values.into_iter().map(PublicSandbox::from).collect())
    }

    pub(crate) async fn info(&self, id: &str) -> Result<PublicSandbox, LsbxError> {
        let value: WireSandbox = self
            .json(Method::GET, self.endpoint(&["sandboxes", id])?, None)
            .await?;
        Ok(value.into())
    }

    pub(crate) async fn create(
        &self,
        profile: &str,
        name: Option<&str>,
        task_id: Option<&str>,
        lease: Duration,
        ready_timeout: Duration,
        verify: bool,
    ) -> Result<PublicSandbox, LsbxError> {
        let body = serde_json::to_value(CreateBody {
            profile,
            name,
            task_id,
            lease_secs: lease.as_secs(),
            ready_timeout_secs: ready_timeout.as_secs(),
            verify,
            healthchecks: Vec::new(),
        })
        .map_err(json_encode_error)?;
        let value: WireSandbox = self
            .json(Method::POST, self.endpoint(&["sandboxes"])?, Some(body))
            .await?;
        Ok(value.into())
    }

    pub(crate) async fn destroy(&self, id: &str) -> Result<(), LsbxError> {
        let response = self
            .request(Method::DELETE, self.endpoint(&["sandboxes", id])?)
            .send()
            .await
            .map_err(transport_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(response).await)
        }
    }

    pub(crate) async fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
    ) -> Result<GatewayExecOutput, LsbxError> {
        let body = serde_json::to_value(ExecBody {
            command,
            timeout_secs: timeout.as_secs(),
        })
        .map_err(json_encode_error)?;
        self.json(
            Method::POST,
            self.endpoint(&["sandboxes", id, "exec"])?,
            Some(body),
        )
        .await
    }

    pub(crate) async fn renew(
        &self,
        id: &str,
        duration: Duration,
    ) -> Result<PublicSandbox, LsbxError> {
        let body = serde_json::to_value(RenewBody {
            duration_secs: duration.as_secs(),
        })
        .map_err(json_encode_error)?;
        let value: WireSandbox = self
            .json(
                Method::POST,
                self.endpoint(&["sandboxes", id, "renew"])?,
                Some(body),
            )
            .await?;
        Ok(value.into())
    }

    pub(crate) async fn console_url(&self, id: &str) -> Result<Option<String>, LsbxError> {
        let console: GatewayConsole = self
            .json(
                Method::POST,
                self.endpoint(&["sandboxes", id, "console"])?,
                Some(serde_json::json!({})),
            )
            .await?;
        match console.console_url {
            Some(url) => Ok(Some(self.resolve_reported_url(id, &url)?)),
            None => Ok(None),
        }
    }

    pub(crate) async fn goldens(&self) -> Result<Vec<GoldenConfig>, LsbxError> {
        self.json(Method::GET, self.endpoint(&["goldens"])?, None)
            .await
    }

    pub(crate) async fn profiles(&self) -> Result<serde_json::Value, LsbxError> {
        self.json(Method::GET, self.endpoint(&["profiles"])?, None)
            .await
    }

    pub(crate) async fn images(&self) -> Result<serde_json::Value, LsbxError> {
        self.json(Method::GET, self.endpoint(&["images"])?, None)
            .await
    }

    pub(crate) async fn upload(
        &self,
        id: &str,
        destination: &str,
        bytes: Vec<u8>,
    ) -> Result<(), LsbxError> {
        let mut url = self.endpoint(&["sandboxes", id, "upload"])?;
        url.query_pairs_mut()
            .append_pair("destination", destination);
        let response = self
            .request(Method::POST, url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(transport_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(response).await)
        }
    }

    pub(crate) async fn download(&self, id: &str, source: &str) -> Result<Vec<u8>, LsbxError> {
        let mut url = self.endpoint(&["sandboxes", id, "artifacts"])?;
        url.query_pairs_mut().append_pair("source", source);
        let response = self
            .request(Method::GET, url)
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        read_limited(response, MAX_DOWNLOAD_BYTES).await
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header(AUTHORIZATION, self.authorization.clone())
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<T, LsbxError> {
        let mut request = self.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let bytes = read_limited(response, MAX_JSON_BYTES).await?;
        let envelope: WireEnvelope<T> = serde_json::from_slice(&bytes).map_err(|e| {
            LsbxError::ContractViolated(format!(
                "gateway returned malformed JSON success response: {e}"
            ))
        })?;
        if envelope.status != "success" {
            return Err(error_from_wire(
                StatusCode::OK,
                envelope.code,
                envelope.message,
                None,
            ));
        }
        envelope.data.ok_or_else(|| {
            LsbxError::ContractViolated(
                "gateway success response did not contain a data field".to_string(),
            )
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, LsbxError> {
        let mut url = self.base.clone();
        let mut path = url.path_segments_mut().map_err(|_| {
            LsbxError::Usage("gateway URL cannot be used as an HTTP base URL".to_string())
        })?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    fn resolve_reported_url(&self, id: &str, reported: &str) -> Result<String, LsbxError> {
        let url = self.base.join(reported).map_err(|e| {
            LsbxError::ContractViolated(format!("gateway reported an invalid console URL: {e}"))
        })?;
        if url.origin() == self.base.origin()
            && url.username().is_empty()
            && url.password().is_none()
        {
            return Ok(url.to_string());
        }

        // Older/current gateways expose the backend's guest-private noVNC URL
        // from LsbxOps here. A remote client must not navigate to that address;
        // use the gateway's own public, same-origin console page instead.
        let mut page = self.endpoint(&["console"])?;
        page.query_pairs_mut().append_pair("target", id);
        Ok(page.to_string())
    }
}

fn json_encode_error(error: serde_json::Error) -> LsbxError {
    LsbxError::ContractViolated(format!("failed to encode gateway request: {error}"))
}

fn transport_error(error: reqwest::Error) -> LsbxError {
    LsbxError::BackendUnavailable(format!("gateway request failed: {}", error.without_url()))
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, LsbxError> {
    let too_large = || LsbxError::Usage(format!("gateway response exceeds {limit} bytes"));
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn response_error(response: reqwest::Response) -> LsbxError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let wire = read_limited(response, MAX_ERROR_BYTES)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WireEnvelope<serde_json::Value>>(&bytes).ok());
    let code = wire.as_ref().and_then(|value| value.code);
    let message = wire.and_then(|value| value.message);
    error_from_wire(status, code, message, retry_after)
}

fn error_from_wire(
    status: StatusCode,
    code: Option<i32>,
    message: Option<String>,
    retry_after: Option<String>,
) -> LsbxError {
    let fallback = status
        .canonical_reason()
        .unwrap_or("gateway request failed")
        .to_string();
    let message = message.unwrap_or(fallback);

    if status == StatusCode::UNAUTHORIZED || code == Some(7) {
        return LsbxError::AuthFailed(strip_error_prefix(&message, "auth failed: "));
    }
    if status == StatusCode::NOT_FOUND || code == Some(4) {
        return LsbxError::NotFound(strip_error_prefix(&message, "not found: "));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let suffix = retry_after
            .map(|value| format!(" (retry after {value} seconds)"))
            .unwrap_or_default();
        return LsbxError::BackendUnavailable(format!("{message}{suffix}"));
    }
    if status.is_server_error() {
        return LsbxError::BackendUnavailable(strip_error_prefix(
            &message,
            "backend unavailable: ",
        ));
    }
    if status.is_client_error() {
        return LsbxError::Usage(strip_known_prefix(&message));
    }

    LsbxError::ContractViolated(format!(
        "gateway returned an unexpected HTTP status {status}: {message}"
    ))
}

fn strip_error_prefix(message: &str, prefix: &str) -> String {
    message.strip_prefix(prefix).unwrap_or(message).to_string()
}

fn strip_known_prefix(message: &str) -> String {
    const PREFIXES: [&str; 7] = [
        "usage: ",
        "backend unavailable: ",
        "not found: ",
        "contract violated: ",
        "lock contention: ",
        "auth failed: ",
        "interrupted: ",
    ];
    PREFIXES
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix))
        .unwrap_or(message)
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn plaintext_requires_explicit_opt_in() {
        let result = GatewayClient::new("http://127.0.0.1:1234", Some("secret"), false);
        assert!(matches!(result, Err(LsbxError::Usage(_))));
    }

    #[test]
    fn missing_token_is_auth_failed_without_echoing_a_secret() {
        let result = GatewayClient::new("https://example.invalid", None, false);
        let error = result.err().expect("missing token must fail");
        assert!(matches!(error, LsbxError::AuthFailed(_)));
        assert!(!error.to_string().contains("Bearer"));
    }

    #[test]
    fn url_credentials_are_rejected() {
        let result = GatewayClient::new(
            "https://user:secret@example.invalid",
            Some("another-secret"),
            false,
        );
        let error = result.err().expect("URL credentials must fail");
        assert!(matches!(error, LsbxError::Usage(_)));
        assert!(!error.to_string().contains("another-secret"));
    }

    async fn authenticated_list(headers: HeaderMap) -> axum::response::Response {
        if headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer test-token")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Json(serde_json::json!({
            "status": "success",
            "data": [{
                "id": "sbx-remote",
                "name": "remote",
                "host": "remote.demo.local",
                "profile": "default",
                "flavor": "desktop",
                "streaming": "novnc",
                "task_id": null,
                "created_at": "2026-11-21T00:00:00Z",
                "lease_expires_at": "2026-11-21T01:00:00Z",
                "console_url": "/console?target=sbx-remote",
                "cleanup_failed": false,
                "repository": null
            }]
        }))
        .into_response()
    }

    async fn spawn(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test gateway");
        let address = listener.local_addr().expect("test gateway address");
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test gateway");
        });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn list_preserves_the_local_public_sandbox_json_shape() {
        let (base, task) = spawn(Router::new().route("/sandboxes", get(authenticated_list))).await;
        let client = GatewayClient::new(&base, Some("test-token"), true).expect("client");
        let remote = client.list().await.expect("remote list");

        let local = PublicSandbox {
            id: "sbx-remote".to_string(),
            name: "remote".to_string(),
            host: "remote.demo.local".to_string(),
            profile: "default".to_string(),
            flavor: "desktop".to_string(),
            streaming: "novnc".to_string(),
            task_id: None,
            created_at: Some("2026-11-21T00:00:00Z".to_string()),
            lease_expires_at: Some("2026-11-21T01:00:00Z".to_string()),
            console_url: Some("/console?target=sbx-remote".to_string()),
            cleanup_failed: false,
            repository: None,
        };
        let remote_envelope = lsbx_kernel::envelope::Envelope::Success { data: &remote };
        let local_values = vec![local];
        let local_envelope = lsbx_kernel::envelope::Envelope::Success {
            data: &local_values,
        };
        assert_eq!(
            serde_json::to_value(remote_envelope).expect("serialize remote envelope"),
            serde_json::to_value(local_envelope).expect("serialize local envelope")
        );
        task.abort();
    }

    #[tokio::test]
    async fn wrong_token_maps_to_auth_failed_and_is_not_leaked() {
        let (base, task) = spawn(Router::new().route("/sandboxes", get(authenticated_list))).await;
        let secret = "definitely-wrong-secret";
        let client = GatewayClient::new(&base, Some(secret), true).expect("client");
        let error = client.list().await.expect_err("wrong token must fail");
        assert!(matches!(error, LsbxError::AuthFailed(_)));
        assert!(!error.to_string().contains(secret));
        task.abort();
    }

    #[derive(Clone, Default)]
    struct TransferState(Arc<Mutex<Vec<u8>>>);

    async fn upload_bytes(
        State(state): State<TransferState>,
        Query(query): Query<HashMap<String, String>>,
        body: Bytes,
    ) -> axum::response::Response {
        if query.get("destination").map(String::as_str) != Some("/guest/destination") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        *state.0.lock().expect("transfer state") = body.to_vec();
        Json(serde_json::json!({"status":"success","data":{}})).into_response()
    }

    async fn download_bytes(
        State(state): State<TransferState>,
        Query(query): Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        if query.get("source").map(String::as_str) != Some("/guest/source") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        state
            .0
            .lock()
            .expect("transfer state")
            .clone()
            .into_response()
    }

    #[test]
    fn guest_private_console_url_falls_back_to_gateway_page() {
        let client = GatewayClient::new("https://gateway.example/api", Some("token"), false)
            .expect("client");
        let url = client
            .resolve_reported_url("sbx-one", "https://192.0.2.10/vnc.html")
            .expect("console URL");
        assert_eq!(url, "https://gateway.example/api/console?target=sbx-one");
    }

    #[tokio::test]
    async fn transfers_use_http_bytes_not_gateway_host_paths() {
        let state = TransferState::default();
        let router = Router::new()
            .route("/sandboxes/sbx-transfer/upload", post(upload_bytes))
            .route("/sandboxes/sbx-transfer/artifacts", get(download_bytes))
            .with_state(state.clone());
        let (base, task) = spawn(router).await;
        let client = GatewayClient::new(&base, Some("test-token"), true).expect("client");
        let payload = b"opaque client bytes".to_vec();

        client
            .upload("sbx-transfer", "/guest/destination", payload.clone())
            .await
            .expect("upload bytes");
        assert_eq!(*state.0.lock().expect("state"), payload);

        let downloaded = client
            .download("sbx-transfer", "/guest/source")
            .await
            .expect("download bytes");
        assert_eq!(downloaded, payload);
        task.abort();
    }
}
