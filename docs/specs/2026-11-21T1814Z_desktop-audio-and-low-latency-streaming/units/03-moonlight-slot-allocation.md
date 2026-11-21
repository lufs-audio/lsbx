# Unit 03 — moonlight-slot-allocation

## Objective
`lsbx moonlight <id>` allocates an exclusive, idempotent, store-tracked port slot
for a sandbox and prints the Moonlight address; `--release` and `--list` manage
the same state; the TTL reaper and reconcile free slots whose sandbox is gone.

This unit is **allocation and bookkeeping only**. Nothing listens on the ports
yet — that is unit 04.

## Context
- Phase SPEC §5 D4, D4a (why the gateway owns listeners), D5 (slots as client
  identity), D10 (cap of 2), D11 (allocate on demand, always release);
  §6.2 (port-slot model), §6.3 (state machine), §6.4 (failure modes).
- `crates/lsbx-store/src/sandbox_store.rs` — `SandboxStore::{load, list, save,
  delete}`; `crates/lsbx-store/src/lock.rs` — `LockGuard::{acquire, try_acquire}`
  is the exclusivity primitive for allocation.
- `crates/lsbx-kernel/src/types.rs` — `SandboxRecord.extra: serde_json::Map`.
  Slots live there; **no schema change** (the phase boundary forbids it).
- `crates/lsbx-cli/src/lib.rs` — subcommand wiring; `lsbx console <id>` is the
  closest existing shape to copy for output and error style.
- Host facts: host Sunshine already owns 47984–48010 on all interfaces, which is
  why the base is 50000 (SPEC §2.3, §6.2).

## Acceptance criteria
- [ ] `lsbx moonlight <id>` on a running sandbox prints the slot number and the
      `host:port` address, and writes
      `extra.moonlight = {"slot": N, "base_port": P, "guest_ip": ..., "state":
      "allocated", "allocated_at": ...}` to the record.
- [ ] Running it twice for the same sandbox returns the same slot and does not
      consume a second slot.
- [ ] Two different sandboxes get different slots while a free slot exists.
- [ ] With `LSBX_MOONLIGHT_SLOTS=2` and two slots taken, a third request fails
      with an error naming the slots in use (not a hang, not a silent reuse).
- [ ] `lsbx moonlight --list` reports id, slot, base port, guest IP, state and age
      for every sandbox that has one; `--json` emits the same data as an envelope.
- [ ] `lsbx moonlight <id> --release` clears `extra.moonlight` and frees the slot.
- [ ] `lsbx down <id>` releases the slot as part of teardown.
- [ ] The reaper/reconcile path releases a slot whose sandbox record no longer
      exists or whose lease has expired.
- [ ] Allocation is safe against concurrent invocations: two simultaneous
      `lsbx moonlight` calls for two sandboxes never return the same slot (a test
      with threads or a held `LockGuard` proves the lock is taken).
- [ ] `lsbx moonlight <id> --json` on a backend other than `libvirt` returns a
      clear "unsupported for this backend" error rather than pretending.
- [ ] `cargo test -p lsbx-store -p lsbx-cli` passes; workspace clippy clean.

## Interface contract
```rust
// crates/lsbx-store/src/moonlight_slots.rs (new)

pub const DEFAULT_SLOTS: u8 = 2;
pub const DEFAULT_BASE_PORT: u16 = 50_000;
pub const PORTS_PER_SLOT: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotState { Allocated, Paired, Released }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MoonlightSlot {
    pub sandbox_id: String,
    pub slot: u8,
    pub base_port: u16,
    pub guest_ip: String,
    pub state: SlotState,
    pub allocated_at: String, // RFC3339
}

pub struct SlotConfig {
    pub slots: u8,          // LSBX_MOONLIGHT_SLOTS,        default DEFAULT_SLOTS
    pub base_port: u16,     // LSBX_MOONLIGHT_PORT_BASE,    default DEFAULT_BASE_PORT
    pub host: String,       // LSBX_MOONLIGHT_HOST — the address printed and (unit 04) bound
}
impl SlotConfig { pub fn from_env() -> Result<Self, LsbxError>; }

/// Idempotent. Takes the store lock, reuses an existing allocation for `sandbox_id`,
/// otherwise assigns the lowest free slot. Fails with a message naming occupied
/// slots when none is free.
pub fn allocate(store: &SandboxStore, cfg: &SlotConfig, sandbox_id: &str)
    -> Result<MoonlightSlot, LsbxError>;

pub fn release(store: &SandboxStore, sandbox_id: &str) -> Result<(), LsbxError>;

pub fn list(store: &SandboxStore) -> Result<Vec<MoonlightSlot>, LsbxError>;

/// Reaper/reconcile hook: release slots whose sandbox id is absent from `live`.
pub fn release_stale(store: &SandboxStore, live: &[String]) -> Result<Vec<String>, LsbxError>;
```

```
CLI surface (crates/lsbx-cli):
  lsbx moonlight <sandbox-id> [--release] [--json]
  lsbx moonlight --list [--json]

Env:
  LSBX_MOONLIGHT_SLOTS       default 2      (SPEC D10)
  LSBX_MOONLIGHT_PORT_BASE   default 50000
  LSBX_MOONLIGHT_HOST        default: the address the gateway is configured with

Record layout (SandboxRecord.extra):
  "moonlight": { "slot": 0, "base_port": 50000, "guest_ip": "192.168.122.31",
                 "state": "allocated", "allocated_at": "2026-11-21T18:40:00Z" }
```

## Boundaries
- **Touches:** `crates/lsbx-store/src/moonlight_slots.rs` (new),
  `crates/lsbx-store/src/lib.rs` (module declaration), `crates/lsbx-cli/src/lib.rs`
  (subcommand + dispatch + output), `crates/lsbx-cli/tests/` (new tests), and the
  reaper call site in `crates/lsbx-lifecycle/` **only** to invoke
  `release_stale` (one call, no logic move).
- **Does NOT touch:** `crates/lsbx-gateway/` (unit 04), `crates/lsbx-kernel/`
  (no schema change), the `Backend` trait, `crates/lsbx-ops/` (deliberately — see
  below), `sidecars/`, any PowerShell.

**Deliberate divergence:** this is *not* a `LsbxOps` method and gets no MCP tool.
It is a host-network booking operation, not a backend lifecycle verb, and adding
it to the façade would change the 19-method parity contract that `lsbx-mcp`
asserts. A future gateway client mode (`#57`) reaches it over REST instead.

## Verification
```bash
cargo test -p lsbx-store -p lsbx-cli
cargo clippy -p lsbx-store -p lsbx-cli --all-targets -- -D warnings

# behavioural
lsbx up win11
lsbx moonlight <id>            # → "slot 0: carnyx:50000"
lsbx moonlight <id>            # → same slot, no second allocation
lsbx moonlight --list          # → one row
lsbx moonlight <id> --json     # → envelope with data.moonlight
lsbx down <id>
lsbx moonlight --list          # → empty
```
