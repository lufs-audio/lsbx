use async_trait::async_trait;
use lsbx_kernel::error::LsbxError;
use lsbx_ops::LsbxOps;
use lsbx_store::sandbox_store::SandboxStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_HOST_ANCHOR: u16 = 50_005;
pub const SLOT_STRIDE: u16 = 100;
pub const DEFAULT_SLOT_COUNT: u8 = 1;
pub const MAX_SLOT_COUNT: u8 = 2;
pub const SUNSHINE_ANCHOR: u16 = 47_989;
pub const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct MoonlightConfig {
    pub bind_ip: Ipv4Addr,
    pub allowed_client_ip: Ipv4Addr,
    pub exact_windows_novnc_golden: String,
    pub slots: u8,
    pub host_anchor: u16,
    pub watch_interval: Duration,
}

impl MoonlightConfig {
    pub fn from_env() -> Result<Option<Self>, LsbxError> {
        let Some(bind) = nonempty_env("LSBX_MOONLIGHT_BIND_IP") else {
            return Ok(None);
        };
        let client = required_env("LSBX_MOONLIGHT_CLIENT_IP")?;
        let golden = required_env("LSBX_MOONLIGHT_GOLDEN")?;
        let bind_ip = parse_tailnet_ipv4("LSBX_MOONLIGHT_BIND_IP", &bind)?;
        let allowed_client_ip = parse_tailnet_ipv4("LSBX_MOONLIGHT_CLIENT_IP", &client)?;
        if bind_ip.is_unspecified() || allowed_client_ip.is_unspecified() {
            return Err(LsbxError::Usage(
                "Moonlight addresses may not be wildcard addresses".into(),
            ));
        }
        let slots = parse_env("LSBX_MOONLIGHT_SLOTS", DEFAULT_SLOT_COUNT)?;
        if !(1..=MAX_SLOT_COUNT).contains(&slots) {
            return Err(LsbxError::Usage(format!(
                "LSBX_MOONLIGHT_SLOTS must be between 1 and {MAX_SLOT_COUNT}"
            )));
        }
        let host_anchor = parse_env("LSBX_MOONLIGHT_HOST_ANCHOR", DEFAULT_HOST_ANCHOR)?;
        let highest = u32::from(host_anchor) + u32::from(slots - 1) * u32::from(SLOT_STRIDE) + 21;
        if highest > u32::from(u16::MAX) {
            return Err(LsbxError::Usage(
                "Moonlight host port range exceeds 65535".into(),
            ));
        }
        let watch_ms: u64 = parse_env(
            "LSBX_MOONLIGHT_WATCH_MS",
            DEFAULT_WATCH_INTERVAL.as_millis() as u64,
        )?;
        if !(250..=10_000).contains(&watch_ms) {
            return Err(LsbxError::Usage(
                "LSBX_MOONLIGHT_WATCH_MS must be between 250 and 10000".into(),
            ));
        }
        Ok(Some(Self {
            bind_ip,
            allowed_client_ip,
            exact_windows_novnc_golden: golden,
            slots,
            host_anchor,
            watch_interval: Duration::from_millis(watch_ms),
        }))
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}
fn required_env(name: &str) -> Result<String, LsbxError> {
    nonempty_env(name)
        .ok_or_else(|| LsbxError::Usage(format!("{name} is required when Moonlight is enabled")))
}
fn parse_env<T: std::str::FromStr + Copy>(name: &str, default: T) -> Result<T, LsbxError> {
    match nonempty_env(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| LsbxError::Usage(format!("invalid {name}"))),
    }
}
fn parse_tailnet_ipv4(name: &str, value: &str) -> Result<Ipv4Addr, LsbxError> {
    let ip: Ipv4Addr = value
        .parse()
        .map_err(|_| LsbxError::Usage(format!("{name} must be an IPv4 address")))?;
    let n = u32::from(ip);
    let first = u32::from(Ipv4Addr::new(100, 64, 0, 0));
    let last = u32::from(Ipv4Addr::new(100, 127, 255, 255));
    if !(first..=last).contains(&n) {
        return Err(LsbxError::Usage(format!(
            "{name} must be within 100.64.0.0/10"
        )));
    }
    Ok(ip)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoonlightStatus {
    pub sandbox_id: String,
    pub vm_tag: String,
    pub slot: u8,
    pub address: String,
    pub bind_ip: Ipv4Addr,
    pub allowed_client_ip: Ipv4Addr,
    pub host_anchor: u16,
    pub guest_ip: Ipv4Addr,
    pub lease_expires_at: String,
    pub acquired_at: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct RelayBinding {
    pub sandbox_id: String,
    pub bind_ip: Ipv4Addr,
    pub allowed_client_ip: Ipv4Addr,
    pub host_anchor: u16,
    pub guest_ip: Ipv4Addr,
    pub guest_anchor: u16,
}

#[async_trait]
pub trait RelayControl: Send + Sync {
    async fn activate(&self, binding: RelayBinding) -> Result<(), LsbxError>;
    async fn deactivate(&self, sandbox_id: &str);
}

/// Production adapter around the bounded data-plane relay. One owned relay per
/// sandbox ensures release awaits cancellation of every listener/flow.
pub struct RelayPool {
    relays: tokio::sync::Mutex<HashMap<String, crate::moonlight_relay::MoonlightRelay>>,
}

impl RelayPool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            relays: tokio::sync::Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl RelayControl for RelayPool {
    async fn activate(&self, binding: RelayBinding) -> Result<(), LsbxError> {
        let mut relays = self.relays.lock().await;
        if relays.contains_key(&binding.sandbox_id) {
            return Ok(());
        }
        let config = crate::moonlight_relay::MoonlightRelayConfig {
            bind_addr: std::net::SocketAddr::new(
                std::net::IpAddr::V4(binding.bind_ip),
                binding.host_anchor,
            ),
            guest_ipv4: binding.guest_ip,
            guest_anchor: binding.guest_anchor,
            allowed_client_ip: std::net::IpAddr::V4(binding.allowed_client_ip),
            allowed_anchor_ports: binding.host_anchor..=binding.host_anchor,
            max_tcp_connections: 16,
            max_udp_peers_per_port: 8,
            udp_idle_timeout: Duration::from_secs(30),
            tcp_connect_timeout: Duration::from_secs(3),
        };
        let relay = crate::moonlight_relay::start(config).await.map_err(|e| {
            LsbxError::BackendUnavailable(format!(
                "failed to bind Moonlight media slot for {}: {e}",
                binding.sandbox_id
            ))
        })?;
        relays.insert(binding.sandbox_id, relay);
        Ok(())
    }

    async fn deactivate(&self, sandbox_id: &str) {
        let relay = self.relays.lock().await.remove(sandbox_id);
        if let Some(relay) = relay {
            let _ = relay.stop().await;
        }
    }
}

pub struct MoonlightManager {
    ops: Arc<LsbxOps>,
    store: SandboxStore,
    config: MoonlightConfig,
    relay: Arc<dyn RelayControl>,
    gate: tokio::sync::Mutex<()>,
    active: tokio::sync::RwLock<BTreeSet<String>>,
}

impl MoonlightManager {
    pub fn new(
        ops: Arc<LsbxOps>,
        store: SandboxStore,
        config: MoonlightConfig,
        relay: Arc<dyn RelayControl>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ops,
            store,
            config,
            relay,
            gate: tokio::sync::Mutex::new(()),
            active: tokio::sync::RwLock::new(BTreeSet::new()),
        })
    }

    pub async fn acquire(&self, id: &str) -> Result<MoonlightStatus, LsbxError> {
        let _guard = self.gate.lock().await;
        let resolved = self
            .ops
            .resolve_moonlight_sandbox(id, &self.config.exact_windows_novnc_golden)
            .await?;
        let guest_ip: Ipv4Addr = resolved.guest_ip.parse().map_err(|_| {
            LsbxError::ContractViolated(format!(
                "sandbox {id} did not resolve to an IPv4 guest address"
            ))
        })?;
        let now = chrono::Utc::now();
        let mut record = self.store.load(id)?;
        let existing: Option<MoonlightStatus> = record
            .extra
            .get("moonlight")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                LsbxError::ContractViolated(format!(
                    "invalid Moonlight record for sandbox {id}: {e}"
                ))
            })?;
        let slot = if let Some(existing) = existing.as_ref() {
            existing.slot
        } else {
            let used: BTreeSet<u8> = self
                .store
                .list()?
                .into_iter()
                .filter_map(|r| {
                    r.extra
                        .get("moonlight")
                        .and_then(|v| v.get("slot"))
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u8::try_from(v).ok())
                })
                .collect();
            (0..self.config.slots)
                .find(|s| !used.contains(s))
                .ok_or_else(|| {
                    LsbxError::LockContention(format!(
                        "no Moonlight media slot is free; occupied slots: {used:?}"
                    ))
                })?
        };
        if slot >= self.config.slots {
            return Err(LsbxError::ContractViolated(format!(
                "sandbox {id} has out-of-range Moonlight slot {slot}"
            )));
        }
        let host_anchor = self.config.host_anchor + u16::from(slot) * SLOT_STRIDE;
        let binding = RelayBinding {
            sandbox_id: id.to_string(),
            bind_ip: self.config.bind_ip,
            allowed_client_ip: self.config.allowed_client_ip,
            host_anchor,
            guest_ip,
            guest_anchor: SUNSHINE_ANCHOR,
        };
        self.relay.activate(binding).await?;
        let status = MoonlightStatus {
            sandbox_id: id.to_string(),
            vm_tag: resolved.vm_tag,
            slot,
            address: format!("{}:{host_anchor}", self.config.bind_ip),
            bind_ip: self.config.bind_ip,
            allowed_client_ip: self.config.allowed_client_ip,
            host_anchor,
            guest_ip,
            lease_expires_at: resolved.lease_expires_at,
            acquired_at: existing
                .map(|v| v.acquired_at)
                .unwrap_or_else(|| now.to_rfc3339()),
            active: true,
        };
        record.extra.insert(
            "moonlight".into(),
            serde_json::to_value(&status)
                .map_err(|e| LsbxError::ContractViolated(e.to_string()))?,
        );
        if let Err(error) = self.store.save(&record) {
            self.relay.deactivate(id).await;
            return Err(error);
        }
        self.active.write().await.insert(id.to_string());
        Ok(status)
    }

    pub async fn release(&self, id: &str) -> Result<(), LsbxError> {
        let _guard = self.gate.lock().await;
        self.relay.deactivate(id).await;
        self.active.write().await.remove(id);
        let mut record = self.store.load(id)?;
        record.extra.remove("moonlight");
        self.store.save(&record)
    }

    pub async fn status(&self, id: &str) -> Result<Option<MoonlightStatus>, LsbxError> {
        let record = self.store.load(id)?;
        let Some(value) = record.extra.get("moonlight") else {
            return Ok(None);
        };
        let mut status: MoonlightStatus = serde_json::from_value(value.clone()).map_err(|e| {
            LsbxError::ContractViolated(format!("invalid Moonlight record for sandbox {id}: {e}"))
        })?;
        status.active = self.active.read().await.contains(id);
        Ok(Some(status))
    }

    pub async fn list(&self) -> Result<Vec<MoonlightStatus>, LsbxError> {
        let active = self.active.read().await;
        let mut out = Vec::new();
        for record in self.store.list()? {
            if let Some(value) = record.extra.get("moonlight") {
                let mut status: MoonlightStatus =
                    serde_json::from_value(value.clone()).map_err(|e| {
                        LsbxError::ContractViolated(format!(
                            "invalid Moonlight record for sandbox {}: {e}",
                            record.id
                        ))
                    })?;
                status.active = active.contains(&record.id);
                out.push(status);
            }
        }
        out.sort_by_key(|s| s.slot);
        Ok(out)
    }

    pub async fn reap_once(&self) {
        let mut ids: BTreeSet<String> = self.active.read().await.clone();
        if let Ok(records) = self.store.list() {
            ids.extend(
                records
                    .into_iter()
                    .filter(|record| record.extra.contains_key("moonlight"))
                    .map(|record| record.id),
            );
        }
        for id in ids {
            let valid = match (
                self.ops
                    .resolve_moonlight_sandbox(&id, &self.config.exact_windows_novnc_golden)
                    .await,
                self.status(&id).await,
            ) {
                (Ok(resolved), Ok(Some(stored))) => {
                    resolved.vm_tag == stored.vm_tag
                        && resolved.guest_ip == stored.guest_ip.to_string()
                }
                _ => false,
            };
            if !valid {
                self.relay.deactivate(&id).await;
                self.active.write().await.remove(&id);
                if let Ok(mut record) = self.store.load(&id) {
                    record.extra.remove("moonlight");
                    let _ = self.store.save(&record);
                }
            }
        }
    }

    pub fn spawn_reaper(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(this.config.watch_interval);
            loop {
                tick.tick().await;
                this.reap_once().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tailnet_validation() {
        assert!(parse_tailnet_ipv4("x", "100.64.0.1").is_ok());
        assert!(parse_tailnet_ipv4("x", "100.128.0.1").is_err());
        assert!(parse_tailnet_ipv4("x", "0.0.0.0").is_err());
    }
}
