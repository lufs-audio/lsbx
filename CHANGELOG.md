# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-unreleased]

Ground-up Rust rewrite of `lufs-audio/lufs-sandbox-server`'s disposable-VM
engine and zero-idle CI runner broker, as a 17-crate Cargo workspace. See
`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/SPEC.md` for the full spec and
its 15 documented deviations from the original kickoff brief.

### Fixed

- **exe.dev backend HTTPS control path now matches the live API** (fixes
  #30). exe.dev's "Run commands on VM" launch changed the `/exec` endpoint:
  the request body is parsed **verbatim** as a command string (the previous
  JSON-envelope client never worked — the lobby answered
  `{"error":"unknown command"}`), responses are combined plain text (no
  structured JSON), guest execution rides `ssh <vm> <cmd>` as a first-class
  command (the old "422 means fall back to SSH" premise is obsolete), and
  the `X-Exe-Exit` trailer is not reliably exposed through proxy chains, so
  guest exit codes now arrive via an in-band `__LSBX_EXIT:$?` sentinel
  expanded at the VM. One shared endpoint serves control verbs (errors are
  HTTP statuses; `--json` keeps stdout machine-readable) and guest
  execution alike — the VM-scoped `https://<vm>.exe.xyz/exec` URL was
  removed (it fronts the VM's own HTTP services, not an exec API).
- **TLS trust store**: the exec client now uses
  `rustls-tls-native-roots` instead of webpki-compiled roots, so the
  control plane is reachable through TLS-inspecting proxies (corporate
  egress, CI middleboxes, sandboxed agent runtimes) that re-sign upstream
  TLS with a host-store CA. Found live: webpki-root clients failed where
  every system-store tool succeeded.

### Added

- Live HTTP smoke test (`tests/test_http_live.rs`, `--ignored`) proving the
  fixed wire format end-to-end under a token scoped to only `ls` + `ssh`:
  account-level `ls --json` parses over the verbatim format, and guest
  `run` returns the true remote exit code (0 for `echo`, 1 for `false`)
  with the sentinel stripped from output.

### Added

- **Kernel & domain types** (`lsbx-kernel`): `SandboxRecord` /
  `SandboxRecordEnvelope`, `GoldenKey`/`BaseKey`, the `Backend` and `Clock`
  traits, the `LsbxError` taxonomy, the numeric `ExitCode` scheme (0/2–8, with
  1 reserved), and the JSON envelope convention shared by every door.
- **Atomic state store & lock sentinels** (`lsbx-store`): one JSON file per
  sandbox and per CI job, atomic temp-file-plus-rename writes, and a real
  `flock`-based lock sentinel shared by every caller (including the CI
  broker) instead of separate, ad hoc locks.
- **Ephemeral Ed25519 key management** (`lsbx-keys`): native keypair
  generation via `ed25519-dalek`, replacing the `ssh-keygen` subprocess
  shell-out, while preserving the external contract (0600 perms, ephemeral
  temp-directory storage, the `lsbx:<label>` key-comment convention).
- **Backend conformance test kit** (`lsbx-backend-testkit`) and three
  `Backend` implementations: `lsbx-backend-demo` (in-memory mock),
  `lsbx-backend-libvirt` (local KVM or remote-via-SSH, one implementation
  parameterized by transport), and `lsbx-backend-exedev` (SSH control plane
  to exe.dev, with an HTTPS `/exec` fallback).
- **Golden image registry & build lifecycle** (`lsbx-golden`): parses
  `images.json` / `images.carnyx.json` against the real existing schema and
  key/base regexes, composes `golden build`/`golden verify` against the
  `Backend` trait, and implements real, populated `lufs-<sha256[:8]>`
  content-hash naming for the first time (previously a CLI help string with
  no populated field in the existing system).
- **VM lifecycle orchestration & reaper** (`lsbx-lifecycle`): create /
  destroy / renew / reap, TTL-based sweeps, orphaned-key reconciliation, and
  `allowed_goldens()`-style protection so a golden a live sandbox depends on
  is never reaped out from under it.
- **Shared operations façade** (`lsbx-ops`): one typed async function per
  logical operation, making CLI/HTTP/MCP structural parity a compile-time
  property of the crate graph rather than a promise kept by convention.
- **Four doors**: CLI + `ratatui` TUI (`lsbx-cli`, `lsbx-tui`), Axum HTTP
  gateway (`lsbx-gateway`), WebSocket stream proxy and noVNC console
  (`lsbx-stream`), and a stdio MCP server (`lsbx-mcp`) whose tool list is
  generated from — and tested against — `lsbx-ops`'s real method set.
- **Zero-idle CI runner broker** (`lsbx-broker`): GitHub App JWT auth,
  repo discovery and queue polling, and job↔VM reconciliation (including
  divergence detection between the job a runner was dispatched for and the
  job GitHub actually assigned it), calling `lsbx-ops::create`/`exec` like
  any other caller.
- **Golden flattening & host bootstrap** (`lsbx-bootstrap`): host capability
  verification, systemd unit generation for the broker services
  (`lsbx-ci-broker`, `lsbx-ci-broker-exe`), and qcow2 backing-file-chain
  flattening.
- **Workspace CI workflow** (`.github/workflows/ci.yml`): `cargo check` /
  `cargo clippy --all-targets --all-features -- -D warnings` / `cargo test
  --workspace` on the existing always-on `[self-hosted, lufs]` fleet, with
  placement read from the `vars.LSBX_CI_PLACEMENT` repository variable
  (defaulting to `lufs`) so the future cutover to `lsbx`'s own broker
  (`lsbx-default`) is a one-line config change, not a code change, once that
  broker is deployed and verified live.
- **Compatibility fixtures** (`tests/fixtures/`): real, byte-for-byte copies
  of `lufs-audio/lufs-sandbox-server`'s `images.json` and
  `images.carnyx.json` (including the real, live `agent-base` golden
  base-name mismatch between the two files — preserved deliberately, not
  harmonized), plus a current-schema `SandboxRecordEnvelope` sample and a
  legacy-flat (pre-envelope) `SandboxRecord` sample, so schema compatibility
  with the existing on-disk formats is a fact enforced by parsing in tests
  rather than only a claim in a commit message. The legacy-flat/current
  schema-level compatibility is a target property this rewrite is built to
  hold and is checked at the schema level by these fixtures; it is not yet a
  claim of verified bit-for-bit output parity for every code path that
  produces `SandboxRecord` JSON, which remains an ongoing verification
  target as more units land.

### Notes

- This entry documents Unit 20's scope only: the workspace-level CI workflow,
  repo meta files, and the four `tests/fixtures/*.json` files it owns. Wiring
  every crate into the workspace root `Cargo.toml`'s `members` list, and
  updating each crate's own tests to read from `tests/fixtures/` instead of
  inline fixture literals, is a deliberate, separate integration pass —
  see this unit's PR description for the exact boundary.
