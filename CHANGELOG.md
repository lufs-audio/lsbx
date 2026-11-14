# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-unreleased]

Ground-up Rust rewrite of `lufs-audio/lufs-sandbox-server`'s disposable-VM
engine and zero-idle CI runner broker, as a 17-crate Cargo workspace. See
`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/SPEC.md` for the full spec and
its 15 documented deviations from the original kickoff brief.

### Added

- **Interactive TUI dashboard wired into CLI** (#47, commit `c134d77`). Running
  bare `lsbx` on an interactive TTY now launches the Ratatui dashboard, while
  pipes and `--json` cleanly fall back to status reports. Added explicit `lsbx
  dashboard` (aliases: `tui`, `dash`) subcommand.
- **Reproducible Windows golden builds via `lsbx golden build`** (#36, commit
  `b203cd6`). Added `--os <OS>` support across CLI, ops façade, MCP, and TUI
  wizard; on Windows, scripts stage to `C:\Windows\Temp\` and run via PowerShell
  with execution policy bypass. Added `scripts/provision-win11-golden.ps1` and
  retired stale fallback image.
- **Interactive Windows browser console & surfaced `console_url`** (#37, commit
  `1b24c34`). Populated `host` with resolved guest IP and `https_url` from
  `LSBX_PUBLIC_BASE_URL` / `LSBX_CONSOLE_BASE` or in-guest websockify (`:8000`).
  Gateway `/console?target=<id>` now serves a responsive full-window noVNC
  iframe with direct link.
- **Zero-idle CI dispatch for Windows runner jobs** (#39, commit `064a365`).
  Automated detection of `lsbx-windows` label in `Reconciler::dispatch` routing
  to `win11-desktop`, native PowerShell Actions runner provisioner
  (`scripts/provision-ci-runner.ps1`), 1280 MTU clamping, and lifecycle
  monitoring.
- **Golden commit/save helper & hardware upgrade** (commit `081284e`). Added
  `scripts/lsbx-commit-golden.sh` to commit interactive sandbox changes into
  golden images; upgraded `win11-desktop` to 8 vCPUs / 32 GB RAM / 256 GB virtual
  disk with aliased `win11` profile.

### Fixed

- **Broker resilience for gh CLI network hiccups** (commit `0df6bb2`). Added
  `map_gh_cli_error` so transient HTTP 502, connection resets, and TLS
  handshake timeouts map to retryable `LsbxError::BackendUnavailable` rather
  than fatal `AuthFailed` crashes.

### Added

- **`lsbx golden reconcile`** — cross-references the manifest's goldens
  against what the backend says it actually has, via the new
  `Backend::list_golden_artifacts`: each manifest golden reports `present`
  or `missing`, and artifacts with no manifest entry are listed under
  `unregistered_vms` as candidates for `golden register`. **Where a golden
  lives is backend-specific, and the trait now says so**: on exedev a golden
  is a long-lived VM (the default implementation filters `list_vms` to the
  `lsbx-*-v*` naming convention), while on libvirt a golden is a qcow2 disk
  image and `list_vms` reports *domains* — reconciling against the VM
  inventory there would have marked every manifest golden missing, since the
  only domains are VMs built from those images. `LibvirtBackend` overrides
  the hook with the image directory's contents, deduplicated by canonical
  target so a `agent-base.qcow2 -> lsbx-agent-v1.qcow2` convenience symlink
  is one artifact, not two. Available through every door: the CLI subcommand
  (`lsbx golden reconcile`, `--json` envelope + human KEY/BASE/STATUS
  table), the MCP tool (`golden_reconcile` — `LsbxOps` now exposes 19
  public methods, parity assertions updated), and
  `LsbxOps::golden_reconcile` itself. A control-plane failure propagates
  verbatim — a dead backend never masquerades as an empty inventory.
  Closes the 2026-09-15 exedev-golden-discovery gap: `golden list` is
  registry-only by design, so goldens that existed only as tagged VMs on a
  live exe.dev account were invisible to every door.

### Fixed

- **A missing default image manifest is no longer silently treated as an
  empty registry.** The CLI still proceeds (correct behavior — a fresh
  host has no manifest yet), but now prints a stderr note naming the
  default path it looked at and the remedy (`--images` / `LSBX_IMAGES` /
  `LSBX_IMAGES_PATH`). An explicitly passed `--images` path that is absent
  stays silent, since the caller chose that path. This cost real debugging
  time on kora (2026-09-15): `-b exedev` was live and authenticated, but
  the empty default view read as "the backend has no goldens" rather than
  "no manifest was ever installed at `<state_dir>/images.json`".
- Also fixed: `lsbx-mcp` keeps `#[allow(deprecated)]` on
  `rmcp::model::ServerInfo`, which is a deprecated *alias* in rmcp 3.4.x and
  the actual name in 3.1.x. The deprecated form is deliberate: the workspace
  pins `rmcp = "3"` and `Cargo.lock` is gitignored (per AGENTS.md), so
  different hosts resolve different 3.x minors — kora gets 3.4.0, carnyx's
  local lock has 3.1.4 — and only the alias compiles on both. Migrating to
  `ServerConfig` (the 3.4 name) builds everywhere a lock resolves 3.4+ and
  fails `E0432` on a host with an older lock, which is what a release rebuild
  on carnyx hit on 2026-11-07. Pinning rmcp to an exact version (or committing
  the lock) is the real fix and is not part of this change.

### Removed

- **Dead 422-fallback machinery** (follow-up to #30/#31): the
  `fallback_ssh_key_path` fields on both token auth variants, the
  `account_token_with_fallback` / `vm_scoped_token_with_fallback`
  constructors, the `ExedevAuth::fallback_ssh_key_path()` accessor, the
  `HttpExecOutcome` enum (the client returns `CommandOutput` directly),
  and the CLI's `LSBX_EXEDEV_SSH_KEY`-as-422-fallback branch (the env var
  still configures standalone `Ssh` auth). All were unreachable after the
  #31 wire-format fix: exe.dev's run-on-vm launch removed the 422 premise.
  The auth-mode table in `README.md` documents the two co-equal transports.
- Also fixed: `test_auth_modes`' URL-mirror helper still encoded the
  pre-#31 VM-scoped `/exec` URL rule; it now pins the account-level-only
  rule the live API actually follows.

### Fixed

- **The CI broker's installation token is now re-minted as it nears expiry,
  instead of captured once at process start** (fixes #43). `GitHubClient`
  held a single `Octocrab` built from a token minted exactly once in
  `from_app_auth`; `GitHubAppAuth`'s own cache and 300 s refresh margin were
  correct but unreachable afterwards. GitHub App installation tokens live 60
  minutes, so a long-lived broker spent most of every cycle sending a dead
  token: every `actions/runs` call returned `401 Bad credentials`,
  `map_octocrab_error` classified that as `AuthFailed`, and `AuthFailed` is
  fatal by design — so the process exited, systemd restarted it, and the loop
  repeated (1116 restarts and zero dispatches on molimo). `Backing::App` now
  holds the auth *handle* (`Arc<GitHubAppAuth>`) and asks it for a token on
  every call, rebuilding the client only when the token string actually
  changes; `Backing::Static` remains for the `test-util` seam. `from_app_auth`
  takes `Arc<GitHubAppAuth>` — its one call site in `lsbx-cli` is updated.
  New tests assert the invariant at the `GitHubClient` boundary (where the
  defect lived, not one layer down in `auth.rs`, whose caching test passed
  throughout) on **both** the exchange-endpoint hit count and the
  `Authorization` header of the request that follows; both fail against the
  pre-fix behaviour.
- **Auth failures now name the failing repo, the token's age, and the token's
  scope** (fixes #44). The old message carried only the route, so a
  `403 Resource not accessible by integration` said nothing about which token
  had failed or what that token could see — the two facts that separate the
  competing explanations. `AuthFailed` messages are now enriched with `repo=`,
  `token_age=` and `token_sees=`, the scope being recorded from the last
  successful `installation_repositories()` call made with the *same* token and
  cleared whenever a new token is minted (a scope observation from an older
  token would be actively misleading). Only `AuthFailed` is rewritten: a
  rate-limited 403 stays the retryable `BackendUnavailable` that `error_map`
  deliberately maps it to.
- **`GitHubClient` now builds its per-call clients against the same host its
  `GitHubAppAuth` is configured for.** `GitHubAppAuth::base_uri_override()` is
  new and threaded into `installation_token_client_with_base_uri`. Previously
  an App-auth client always pointed at `api.github.com` regardless of where
  its auth pointed, which left the token-refresh path with no seam a test
  could intercept — the structural reason the #43 defect outlived a green
  suite.
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

- **Decision spec for calimbra sandbox hosting**
  (`docs/specs/2026-11-01T2354Z_calimbra-incus-backend/SPEC.md`, #45). Records
  decisions D1–D8 with the rejected options and the reason for each rejection:
  calimbra is a *services + broker* host rather than a sandbox host (3.8 GiB
  total against goldens of 4–8 GB, **and** every existing golden's
  `healthcheck` is VM-shaped — `systemctl`, `docker image inspect`, `cmd /c` —
  so Incus system containers cannot satisfy them at any size); Incus VMs are
  rejected as a general sandbox answer at a measured ~697 s boot-to-IP; the
  `Backend` contract keeps its SSH shape via option C, with the `exec`
  primitive (option B) deferred as an additive refactor; and goldens are built
  on carnyx, never on calimbra, because a 4 GB golden cannot be built on a
  3.8 GiB host.
- `crates/lsbx-broker/tests/test_client_token_refresh.rs` — regression tests
  for #43 (healthy token reused without rebuild; token crossing the 300 s
  margin re-minted **and** the rebuilt client authenticating with the new
  token).
- `crates/lsbx-broker/tests/test_auth_error_diagnostics.rs` — tests for #44
  (an `AuthFailed` names repo/age/scope; a rate-limited 403 stays retryable and
  is not enriched).
- `GitHubAppAuth::base_uri_override()` — see the `Fixed` entry above.

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
- **Win11 browser-desktop golden phase** (see
  `docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/`): registered the
  `win11-desktop` golden on Carnyx (Windows 11 25H2, desktop flavor,
  `streaming: novnc`, TightVNC loopback + noVNC `resize=scale` console baked
  into the image), and made Windows goldens first-class across the toolchain:
  - `CreateFromGoldenRequest` / `CreateRequest` / `DomainXmlParams` carry
    `os`; the libvirt backend renders a dedicated Windows domain shape
    (UEFI/SecureBoot q35, per-VM OVMF `_VARS.fd`, `hyperv` + `smm`, swtpm
    2.0, localtime + hypervclock, qxl, VNC) and skips cloud-init seed-ISO
    injection for `os == "windows"`.
  - Windows healthchecks run under the guest's `cmd.exe` session — the
    guest-SSH transport reconstructs `cmd /c …` argv with cmd.exe quoting
    (POSIX single-quote tokens are literal for Windows OpenSSH), and the
    ops/golden healthcheck wrappers pick `cmd /c` over `sh -c` by `os`.
  - Guest-IP resolution filters loopback and transient APIPA (169.254/16)
    addresses Windows reports while booting, so create/verify don't SSH to
    a dead end.
  - `lsbx up`/`verify` for Windows use the golden's baked guest identity
    (no ephemeral keypair, no cloud-init `authorized_keys` injection).
  - `ImageRegistry` is now `Serialize`; `golden register`/`delete` and
    `golden build --register` persist registry mutations straight back to
    the loaded manifest via the CLI's `build_deps`. `golden register`
    gained `--os`/`--cpu`/`--memory`/`--disk`/`--mode`/`--repo`.
  - Verified live on Carnyx: `lsbx golden verify win11-desktop` passes both
    Windows healthchecks and `lsbx up win11-desktop` provisions a ready
    UEFI/TPM SecureBoot VM with its noVNC console reachable in a browser.

### Notes

- `AGENTS.md`'s "CI authoring" section is reconciled with deployment reality
  (#35): `lsbx-default` is documented as the label this repo's own workspace CI
  actually runs on (`.github/workflows/ci.yml`, `ci/ci.yml`), and the legacy
  `lufs` label reservation is retired now that its own stated condition —
  "until this repo's own broker is verified live" — is met. `SPEC.md`
  Deviation 15 is deliberately left as written: it is a dated record of the
  original chicken-and-egg decision, not a statement of current practice.
- `cargo fmt --all --check` is **not** clean on this repo at rustfmt 1.99 and
  is not a CI gate (`ci.yml` runs `check`, `clippy -D warnings`, `test`). The
  files added/changed by this entry are rustfmt-clean; pre-existing formatting
  drift elsewhere is left untouched rather than swept into a focused change.
- This entry documents Unit 20's scope only: the workspace-level CI workflow,
  repo meta files, and the four `tests/fixtures/*.json` files it owns. Wiring
  every crate into the workspace root `Cargo.toml`'s `members` list, and
  updating each crate's own tests to read from `tests/fixtures/` instead of
  inline fixture literals, is a deliberate, separate integration pass —
  see this unit's PR description for the exact boundary.
