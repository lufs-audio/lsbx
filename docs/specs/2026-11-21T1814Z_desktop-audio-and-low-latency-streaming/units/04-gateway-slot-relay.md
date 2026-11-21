# Unit 04 — gateway-slot-relay

## Objective
`lsbx serve` binds the host ports of every allocated slot on the configured
address and relays their TCP and UDP traffic to the sandbox's guest IP, so a
Moonlight client on the tailnet reaches in-guest Sunshine with no root, no
`iptables`, and no leaked state.

## Context
- Phase SPEC §5 **D4a** (the mechanism decision and its rationale — read this
  first; it is why the relay is userspace and lives in the gateway), §6.2 (slot
  model), §6.4 (failure modes the relay must satisfy).
- Unit `03-moonlight-slot-allocation.md` in this folder — the store API this unit
  consumes (`moonlight_slots::list`), the record layout, and the port base.
- `crates/lsbx-gateway/src/lib.rs` — how the axum server, state and shutdown are
  wired today; the relay must join that lifecycle rather than invent one.
- `crates/lsbx-stream/src/proxy.rs` — the existing WebSocket relay, for style and
  error handling. It is **not** reused: it speaks WS, this speaks raw TCP/UDP.
- `lsbx-serve.service` runs as `carnyx` with no capabilities, and `sudo` needs a
  password on carnyx — so ports >1024 and no `CAP_NET_ADMIN` are hard limits.

## Acceptance criteria
- [ ] On start, the gateway enumerates allocated slots from the store and binds
      listeners **only** on `LSBX_MOONLIGHT_HOST` (never `0.0.0.0`), for the ports
      in `SLOT_PORTS` relative to each slot's base port.
- [ ] A TCP connection to a slot port is relayed byte-faithfully to
      `<guest_ip>:<base+offset>` and the reply is relayed back; a test against a
      local echo server proves it in both directions.
- [ ] A UDP datagram sent to a slot port reaches `<guest_ip>:<base+offset>` and the
      reply returns to the *same* client source address, even when two clients
      use the same slot port concurrently (per-source session table).
- [ ] A UDP session with no traffic for `LSBX_MOONLIGHT_UDP_IDLE` (default 30 s)
      is dropped and its upstream socket closed; the table does not grow without
      bound.
- [ ] A slot allocated *after* the gateway started is picked up without a
      restart (the gateway watches for slot changes — polling the store at a
      bounded interval is acceptable and must be documented).
- [ ] Releasing a slot closes its listeners within one poll interval; a subsequent
      connect is refused rather than hanging.
- [ ] Gateway restart with allocated slots re-binds them and the sandbox streams
      again after the client reconnects; the slot is **not** reported as leaked.
- [ ] Guest unreachable → the listener stays bound, the relay attempt fails fast,
      and the error is logged once per attempt at `warn`, naming slot + sandbox
      id. No panic, no busy loop.
- [ ] Nothing in the relay requires root; it runs as `carnyx`.
- [ ] `cargo test -p lsbx-gateway` passes and clippy is clean with
      `--all-targets`.

## Interface contract
```rust
// crates/lsbx-gateway/src/slot_relay.rs (new)

/// Ports relayed per slot, as offsets from the slot's base port.
/// Sunshine's GameStream set; the exact upstream offsets are Sunshine's to
/// define, so this table is the single place to extend if unit 05 finds a
/// missing flow. TCP and UDP are listed separately because they overlap.
pub const SLOT_PORTS: &[(Proto, u16)] = &[ /* (Tcp, 0), (Udp, 9), ... */ ];

pub enum Proto { Tcp, Udp }

pub struct SlotRelayConfig {
    pub host: String,      // LSBX_MOONLIGHT_HOST; bind address, never 0.0.0.0
    pub udp_idle: Duration,// LSBX_MOONLIGHT_UDP_IDLE, default 30s
    pub poll: Duration,    // LSBX_MOONLIGHT_POLL, default 5s
}

pub struct SlotRelay { /* … */ }

impl SlotRelay {
    /// Binds the ports of every currently allocated slot and starts the watcher.
    pub async fn start(cfg: SlotRelayConfig, store: SandboxStore) -> Result<Self, LsbxError>;
    /// Applies one reconcile pass (allocate/release listeners to match the store).
    pub async fn reconcile(&self) -> Result<(), LsbxError>;
    /// Closes every listener. Called from the gateway's existing shutdown path.
    pub async fn shutdown(self);
}

// Wiring: constructed in crates/lsbx-gateway/src/lib.rs alongside the axum
// server, driven by the same cancellation token.
```

## Boundaries
- **Touches:** `crates/lsbx-gateway/src/slot_relay.rs` (new),
  `crates/lsbx-gateway/src/lib.rs` (construction + shutdown only),
  `crates/lsbx-gateway/Cargo.toml` (deps if needed),
  `crates/lsbx-gateway/tests/` (new tests).
- **Does NOT touch:** `crates/lsbx-store/` (unit 03 owns the API; this unit only
  calls `list`), `crates/lsbx-stream/`, `crates/lsbx-cli/`, `crates/lsbx-ops/`,
  `crates/lsbx-kernel/`, `sidecars/`, any PowerShell, the host's
  `lsbx-serve.service` unit file (the env vars are added by whoever deploys, and
  unit 09 documents them).

## Verification
```bash
cargo test -p lsbx-gateway
cargo clippy -p lsbx-gateway --all-targets -- -D warnings

# behavioural (gateway running with LSBX_MOONLIGHT_HOST set to the tailnet IP)
lsbx up win11 && lsbx moonlight <id>          # slot 0 → base 50000
ss -tulpn | grep 500                          # listeners on the tailnet IP only
# from the client, Moonlight "Add PC" → carnyx:50000 → pair → stream
lsbx moonlight <id> --release
ss -tulpn | grep 500                          # gone within one poll interval
```
