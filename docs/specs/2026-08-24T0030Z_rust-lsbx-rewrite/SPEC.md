# SPEC — `lsbx`: A Pluggable Dual-Backend Disposable VM Engine & Zero-Idle CI Runner Broker in Rust

**Status:** Draft — Phase 1 (speccing) complete, awaiting Phase 2 (bplate scaffold + build)
**Owner:** Daniel Ramirez
**Companion repos:** `lufs-audio/lufs-sandbox-server` (predecessor; read-only reference for schemas and behavior, not modified by this work), `lufs-audio/lufs-runner` (the always-on self-hosted CI fleet; complementary, not superseded — see §0.9), `lufs-audio/bplate` (scaffolding + cross-cutting CLI conventions), `lufs-audio/kb` (House of Process Registries)

---

## 0. Deviations From the Kickoff Brief — Read This First

Fifteen places where this spec deviates from the literal kickoff brief, each because the real codebase or real house convention said something different than the brief assumed. Every one is reversible — say the word and it changes.

| # | Brief said | Spec does instead | Why |
|---|---|---|---|
| 1 | Golden manifest is `images.yaml` | Preserves the real files: `images.json` **and** `images.carnyx.json` (two files, JSON not YAML) | That's what's actually on disk in `lufs-sandbox-server`. Renaming/reformatting to match the brief's assumption would be the actual compatibility break this rewrite is supposed to avoid. |
| 2 | Backward-compatible with existing goldens | Preserves a real, live inconsistency: the `agent-base` profile resolves to golden `lsbx-default-v1` in `images.json` but `lsbx-agent-v1` in `images.carnyx.json` | Silently unifying these is a functional change, not a port. Harmonizing them is a legitimate follow-up — just not a silent one inside a "compatible" rewrite. |
| 3 | Backward-compatible with `lufs-<sha256[:8]>` content-hash naming | Treats content-hash naming as **new**, implemented for the first time | It's aspirational in the current system — a CLI help string (`--content-hash`) with no populated field on any shipped golden. There is no real behavior to be compatible with; the honest move is to actually build it. |
| 4 | Ed25519 ephemeral key generation | Generates natively via `ed25519-dalek`, replacing the current `ssh-keygen` subprocess shell-out | Directly requested by the brief; flagged here only because it changes the mechanism while preserving the external contract (0600 perms, ephemeral temp storage, the `lsbx:<label>` key-comment convention `exedev`'s reaper already pattern-matches on). |
| 5 | GitHub App RSA JWT auth | Signs natively via `jsonwebtoken`, replacing the current `openssl dgst -sign` subprocess | Same claims shape (RS256, `iss`/`iat`/`exp`), same caching behavior — internal upgrade, no external contract change. |
| 6 | "Local Libvirt QEMU/KVM" and "Remote Libvirt SSH transport" as items in a list of four backends | Implemented as **one** `LibvirtBackend` parameterized by a `LibvirtTransport` enum (`Local` \| `RemoteSsh`), not two separate `Backend` trait impls | Mirrors the existing (proven) Python `libvirt.py`, which already handles both through one implementation. This still honors "dual-backend" at the concept level — libvirt-family vs. exedev-family — while keeping local/remote independently selectable and independently testable. Reversible if remote ends up needing materially different retry/timeout semantics. |
| 7 | (implicit) everything native Rust | `qemu-img`/`virsh` are still invoked via subprocess (explicit argv arrays, never shell interpolation) for image flatten/convert; only VM lifecycle moves to the native `virt` crate | No mature Rust `qemu-img` binding exists (confirmed against the current crate ecosystem). Matches what the existing system already does for this one operation. |
| 8 | Adhere to "the Six Core Nouns" | Adheres, but narrows the **process** noun to the closed set of Backend implementations, not an open contributor registry | `apho` (the confirmed precedent for this exact phrase) has an open, growing `processes/` folder. `lsbx` doesn't — its actual growing registry is the **golden image catalog**, not the backend set. Said explicitly in §2 rather than papered over. |
| 9 | (unstated) relationship to the existing self-hosted fleet | Treated as complementary, not a replacement | No document anywhere states `lsbx`'s CI broker should replace `lufs-audio/lufs-runner`'s always-on exe.dev + 3×Pi fleet, and the two optimize for opposite things: the fleet exists to guarantee a runner is *already waiting*; a zero-idle broker trades that guarantee for zero idle spend. Recommend they coexist — fleet for steady-state/latency-sensitive jobs, `lsbx` broker for burst/overflow/specialized jobs that can tolerate a cold-start — until Daniel says otherwise. |
| 10 | (unstated) scaffold profile | Phase 2 hand-scaffolds the Cargo workspace root per §8's exact layout | `bplate`'s current profiles (`rust-cli`, `python-cli`, `ts-node`, `polyglot`) don't include a multi-crate Rust workspace profile. Not blocking this spec; flagged as a real gap worth a small `bplate` follow-up (a `rust-workspace` profile) sometime after this ships. |
| 11 | (unstated) where SPEC.md lives | Root `SPEC.md` is a one-paragraph pointer; the real spec is here, under `docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/` | This is the **current** house convention, confirmed against the live state of `snuze` and `lrex` (both nest their real spec this way today). `apho` still has a flat root `SPEC.md`, which is the earlier, now-superseded pattern — not repeated here. |
| 12 | "100% parity across all CLI tools" for the MCP door | Interpreted as structural/behavioral parity — every CLI operation has a corresponding MCP tool, enforced by both being thin adapters over one shared `lsbx-ops` façade (Unit 10) | Not interpreted as wire-format compatibility with the old Python `mcp.py`'s tool schema, which the brief doesn't ask for and which this research pass didn't fully reverse-engineer. Internal-to-this-rewrite consistency is the real ask, and the façade makes it structurally true rather than aspirationally true. |
| 13 | Axum Gateway "quota rate limiter" | Designed fresh (token-bucket, keyed by bearer token / API key / source IP) | The existing Python gateway has no rate limiter. New functionality, not a port. |
| 14 | `ratatui` "wizard" | Designed fresh (guided flows for `up` and `golden build`) | Not present in the existing CLI's flag surface. New functionality, not a port. |
| 15 | `.github/workflows/ci.yml` default placement is `lsbx-default` | Ships pointed at the existing always-on `lufs` fleet label first; the cutover to self-hosted-via-its-own-broker (`lsbx-default` / `vars.LSBX_CI_PLACEMENT`) is a deliberate, later dogfooding step (Unit 20) | Chicken-and-egg: a not-yet-built, not-yet-deployed broker cannot be the thing that builds itself. `vars.LSBX_CI_PLACEMENT` exists from day one specifically so this cutover is a config change, not a code change, when the broker is verified live. |

---

## 1. Problem Statement

`lufs-sandbox-server` (CLI name `lsbx`) is LUFS Audio's disposable-VM engine: it provisions short-lived, ephemeral virtual machines on demand, runs work inside them, and tears them down — used today for CI runners, sandboxed agent execution, and interactive dev/demo environments. The existing implementation (`lufs-audio/lufs-sandbox-server`, Python ≥3.11, two dependencies beyond stdlib: `pyyaml`, `rich`) works and is in service. Its golden-image manifests (`images.json`, `images.carnyx.json`), its CI-facing runner labels (`lsbx-molimo`, `lsbx-carnyx`, `lsbx-default`), its HTTP/CLI contracts, and its GitHub App CI broker are real, load-bearing artifacts — not paper designs this rewrite gets to ignore.

This rewrite exists for three reasons, roughly in order of what they cost today:

1. **Nothing is actually verified.** The current implementation shells out to `ssh`, `scp`, `virsh`, `qemu-img`, `ssh-keygen`, and `openssl` for almost everything load-bearing, and recovers meaning from string-parsed subprocess output and regex-matched log tails (the CI broker's runner-lifecycle detection is a set of regexes over a tailed log file). That is exactly the "ran, but was it *proven*" gap this house has spent the last several builds (snuze, apho) closing everywhere else. A disposable-VM engine that can't prove a VM is actually healthy before handing it to a CI job is the worst place to still have that gap.
2. **The state store has no lock sentinel.** `SandboxStore` uses a thread `RLock` only — a single-process assumption. The CI broker already had to build its *own*, separate `flock` (`BrokerLock`) to work around exactly this, which is a tell: the primitive underneath both should have had one lock sentinel from the start, not two ad hoc ones.
3. **Content-hash golden naming doesn't exist yet.** `lufs-<sha256[:8]>` is a CLI help string today, not a populated field on any shipped golden. This rewrite is where it actually gets built.

**What this rewrite is not:** a decision to retire `lufs-audio/lufs-runner`. See Deviation 9 and §0.9 — that relationship is explicitly open, not resolved here.

### 1.1 Compliance With the Seven LUFS Tests

| Test | How `lsbx` satisfies it |
|---|---|
| **Local-first, self-owned** | Runs entirely on hardware Daniel controls or pays for directly (local libvirt/KVM, or an exe.dev VM pool he holds the SSH keys and bearer tokens to). No backend is mandatory except the in-process demo mock — local libvirt and demo need zero third-party services. |
| **Verifiable** | Two-phase, everywhere it matters: "ran" (a backend call returned, a process exited 0) is never conflated with "proven" (the VM reached the golden's declared healthchecks; the golden's bytes match its content hash; the CI job's dispatched runner actually registered *and* picked up the job `lsbx` meant it to, not a diverged one). |
| **Agent-ergonomic** | `--json` on every command, an MCP door with structural 1:1 CLI parity (§0 Deviation 12), typed exit codes (§6), `--dry-run` on every mutating path that supports it, NDJSON progress on `serve`. |
| **Composable** | One static binary, one process tree; every door is a thin adapter over the same `lsbx-ops` calls a script or another agent could make directly. |
| **Provenance-bearing** | `SandboxRecord` and `CiJobRecord` are typed, persisted handoff artifacts (§2, Handoff). Golden images carry a real, populated content hash — see Deviation 3. |
| **Craft-grounded** | Vocabulary stays the operator's own: lease, golden, flavor, reap, console — not generic cloud-provider nouns bolted on from outside. |
| **Automates toil, not taste** | Automates provisioning, teardown, and CI-runner bookkeeping. Never decides what a golden image should contain (a human/agent authors the provisioning script) or what a CI job does (a human/agent authors the workflow YAML). |

---

## 2. Architectural Frame: The Six Core Nouns

`lsbx` instantiates the Six Core Nouns of the House of Process Registries, in the sense `apho`'s `SPEC.md` established for this phrase — each noun bound to a concrete module in this codebase, not asserted in the abstract:

1. **Kernel (`crates/lsbx-kernel/`)**: the shared substrate every other crate depends on — domain types (`SandboxRecord`, `LeaseInfo`, `GoldenKey`, `Profile`), the `Backend` trait, the `Clock` trait, the JSON envelope, and the rigid exit-code taxonomy (`ExitCode`).
2. **Process (`crates/lsbx-backend-{demo,libvirt,exedev}/`)**: the self-contained atomic capabilities — each a `Backend` trait implementation. **Narrower than most instantiations of this noun**: this is a closed, curated set of three implementations, not an open contributor registry the way `apho`'s `processes/` folder or Workchain's `components/` are. Said plainly rather than stretched to fit — see Deviation 8.
3. **Registry (`crates/lsbx-golden/` + `images.json` / `images.carnyx.json`)**: the filesystem-driven golden-image catalog. This is `lsbx`'s actual growing registry — new entries are added by `golden build`/`golden register`, validated against the same schema every time.
4. **Recipe (a `golden build` invocation, and a resolved `Profile`)**: a declarative composition — base image (or base golden) + provisioning script + resulting golden, or a named profile resolving to a golden + a flavor. Both are compositions of registry entries into something launchable.
5. **Handoff (`SandboxRecord`, `CiJobRecord`)**: the typed, persisted, reproducible contracts passed between stages — CLI/HTTP/MCP hand a `SandboxRecord` to the reaper; GitHub Actions' job queue hands a `CiJobRecord` to the broker's reconciliation loop.
6. **Verification (`crates/lsbx-kernel/src/verify.rs` + the `lsbx-backend-testkit` conformance suite)**: golden healthchecks, VM readiness gates, and CI-job divergence detection — "proven, not exited 0," applied to VMs instead of files.

---

## 3. Architectural Overview

```
                    ┌─────────────────────────────────────────────┐
                    │            lsbx (single binary)               │
                    │                                                │
 human/CI/agent ───▶│  Door 1: CLI (clap v4)  +  TUI (ratatui)      │
                    │  Door 2: HTTP Gateway (axum)                   │
                    │  Door 3: WS Stream Proxy + noVNC console       │
                    │  Door 4: MCP Server (rmcp, stdio)              │
                    └─────────────────────┬──────────────────────────┘
                                           │  all four doors are thin
                                           │  (de)serializing adapters
                                           ▼
                    ┌─────────────────────────────────────────────┐
                    │      lsbx-ops — shared operations façade       │
                    │  (this is HOW CLI/HTTP/MCP parity is real,     │
                    │   not just asserted — see Unit 10)             │
                    └───────────┬─────────────────────┬─────────────┘
                                ▼                     ▼
                ┌───────────────────────┐   ┌─────────────────────────┐
                │  lsbx-lifecycle         │   │  lsbx-golden              │
                │  create / destroy /     │   │  registry parse, build,   │
                │  renew / reap / list    │   │  flatten, content-hash,   │
                └──────┬──────────┬───────┘   │  verify                  │
                       ▼          ▼           └─────────────┬─────────────┘
              ┌────────────┐ ┌──────────┐                    │
              │ lsbx-store  │ │ lsbx-keys│                    │
              │ atomic JSON │ │ ed25519  │                    │
              │ + lock      │ │ ephemeral│                    │
              │ sentinels   │ │ keys     │                    │
              └────────────┘ └──────────┘                    │
                                           ┌────────────────────┘
                                           ▼
                    ┌─────────────────────────────────────────────┐
                    │        Backend trait (in lsbx-kernel)          │
                    └───┬────────────────┬────────────────┬─────────┘
                        ▼                ▼                ▼
              lsbx-backend-demo  lsbx-backend-libvirt  lsbx-backend-exedev
              (mock, tests,      (local KVM, or remote  (SSH control plane,
               CI-free dev)      via SSH transport)      or HTTPS /exec)

                    ┌─────────────────────────────────────────────┐
                    │   lsbx-broker — zero-idle CI runner broker     │
                    │   auth (Unit 16) · poll+labels (Unit 17) ·     │
                    │   reconcile (Unit 18)                          │
                    │   → calls lsbx-ops::create/exec, same as any   │
                    │     other caller                               │
                    └─────────────────────────────────────────────┘

                    ┌─────────────────────────────────────────────┐
                    │   lsbx-bootstrap — host prep, golden flatten,  │
                    │   systemd unit generation for the broker       │
                    │   services (lsbx-ci-broker, lsbx-ci-broker-exe)│
                    └─────────────────────────────────────────────┘
```

The load-bearing decision in this diagram is the `lsbx-ops` waist: every door calls the same typed functions, so CLI/HTTP/MCP parity is a compile-time property of the crate graph, not a promise kept by convention across three separately-maintained implementations.

---

## 4. Component Design

### 4.1 Kernel & Domain Types
`SandboxRecord` preserves the existing schema exactly (`schema_version`, `kind: "sandbox"`, and the full existing field set — `id`, `name`, `host`, `profile`, `flavor`, `streaming`, `username`, `key_name`, `key_path`, `key_dir`, `pubkey`, `task_id`, `created_at`, `lease_expires_at`, `vm_tag`, `https_url`, `cleanup_failed`, `repository_key`, `repository`, `extra`), including auto-migration of legacy flat (unversioned) records. `Backend` and `Clock` are traits so every downstream crate is testable without real infrastructure or real time. Exit codes and the JSON envelope are defined once, here, and used everywhere (§6, §7).

### 4.2 Atomic State Store & Lock Sentinels
One JSON file per sandbox (`SandboxStore`) and one per CI job (`CiJobStore`), atomic writes via temp-file-plus-rename, exactly as today — but with a real lock sentinel this time: a fixed, never-unlinked lock file per protected resource, acquired via `flock(LOCK_EX)`, with an open-lock-fstat-stat-compare-retry cycle so a lock can never be silently orphaned by a concurrent unlink-and-recreate race. The CI broker's own lock (today's `BrokerLock`) becomes one more consumer of this one primitive instead of a second, separately-invented one.

### 4.3 Ephemeral Key Management
Ed25519 keypairs generated natively (`ed25519-dalek`), not shelled out to `ssh-keygen`. The external contract is preserved exactly: 0600 private key permissions, ephemeral temp-directory storage, and the `lsbx:<label>` key-comment convention the `exedev` backend's reaper already pattern-matches on to sweep orphaned keys.

### 4.4 Backend Abstraction & the Four Backends
One `Backend` trait (`capabilities`, `create_from_golden`, `run`, `put_file`, `get_file`, `destroy`, `list_vms`, `rename_vm`), three implementations: `demo` (in-memory mock, zero real infrastructure, exists so every other unit can be built and tested before `libvirt`/`exedev` exist), `libvirt` (local KVM or remote-via-SSH, one implementation parameterized by transport — Deviation 6), `exedev` (SSH control plane to exe.dev, or its HTTPS `/exec` API as a fallback, matching the existing dual-mode). A shared conformance test kit (Unit 04) defines the invariants every implementation must satisfy, so "implements the trait" and "behaves correctly" aren't two different claims.

### 4.5 Golden Image Registry & Build Lifecycle
Parses `images.json` and `images.carnyx.json` with the exact existing schema (`images[]`, `goldens[]`, `profiles{}`; key regex `^[a-z][a-z0-9._-]{0,63}$`; base regex `^[a-z][a-z0-9-]{0,63}$` with `.qcow2` suffix stripped before matching) — including the real `agent-base` base-name mismatch between the two files (Deviation 2), preserved rather than silently fixed. `golden build` composes a base image or base golden with a provisioning script, executed inside a VM launched through the `Backend` trait; `golden verify` runs the golden's declared healthchecks; content-hash naming (`lufs-<sha256[:8]>`) is implemented for real this time (Deviation 3).

### 4.6 VM Lifecycle Orchestration & Reaper
Owns the actual state machine: `create` (generate a keypair, pick a backend, provision from a golden/profile, persist a `SandboxRecord`, wait for readiness with a timeout), `destroy`, `renew` (lease extension), and the reap loop (TTL-based sweep, orphaned-key reconciliation via the `lsbx:<label>` tag, and `allowed_goldens()`-style protection so a golden a live sandbox depends on is never reaped out from under it). `Sandbox::public()` strips key material before anything crosses a door, exactly as today.

### 4.7 The Shared Operations Façade (`lsbx-ops`)
One async function per logical operation — `create`, `destroy`, `list`, `exec`, `put`, `get`, `renew`, `console_url`, `info`, `status`, `reap`, `golden_build`, `golden_verify`, `golden_register`, `golden_delete`, `golden_list`, `config_show`, `logs_query` — each a typed request in, typed `Result<Response, LsbxError>` out. This is the mechanism, not just the intent, behind Deviation 12: CLI, HTTP, and MCP cannot drift from each other because none of them contain any operational logic of their own.

### 4.8 The Four Doors
CLI (`clap` v4) plus a `ratatui` TUI dashboard (bare `lsbx` when interactive, matching today) and a new guided wizard for `up`/`golden build` (Deviation 14). Axum HTTP gateway preserving the exact existing route table, Bearer/`X-Api-Key` auth, and fail-closed non-loopback bind protection, plus a new token-bucket rate limiter (Deviation 13). A WebSocket stream proxy (`tokio-tungstenite`) replacing the current raw-socket relay, preserving the guest-port-8000/noVNC convention and the state-store-mediated destination lookup that prevents an arbitrary host:port from ever being reachable through the proxy. An `rmcp`-based stdio MCP server whose tool list is generated from `lsbx-ops`'s operation set.

### 4.9 Zero-Idle CI Runner Broker
GitHub App auth via `jsonwebtoken` (RS256, same claims, same caching window) with an `octocrab`-based repo-discovery and polling loop, preserving every real behavioral detail found in the existing `ci_broker.py`: `FALLBACK_QUEUE_LABEL = "lsbx-default"`, the 60-second fallback delay (`LSBX_CI_FALLBACK_DELAY`) that dedicated placement labels skip, fail-closed handling of a malformed `created_at` (blocks eligibility rather than allowing it), the 15-second poll interval and 300-second repo-list refresh, and divergence detection between the job `lsbx` dispatched a runner for and the job GitHub actually assigned it (logged, not fatal). Calls `lsbx-ops::create`/`exec` like any other caller — the broker has no special access to VM lifecycle.

### 4.10 Host Bootstrap
Verifies a target host is actually capable (libvirt reachable, `qemu-img` present, state directories exist with correct permissions) before `lsbx` trusts it, generates/installs the systemd units for the broker services (`lsbx-ci-broker`, `lsbx-ci-broker-exe`, names preserved from `AGENTS.md`), and owns golden flattening (collapsing a qcow2 backing-file chain into a single self-contained image before a golden is marked ready).

---

## 5. Unit Decomposition & Dependency Order

| Layer | Unit | Crate | Owns | Depends on |
|---|---|---|---|---|
| 1 | 01 — Kernel Domain Types & Exit Codes | `lsbx-kernel` | `src/types.rs`, `src/backend.rs` (trait only), `src/clock.rs`, `src/error.rs`, `src/exit_code.rs`, `src/envelope.rs` | — |
| 2 | 02 — Atomic State Store & Lock Sentinels | `lsbx-store` | `src/sandbox_store.rs`, `src/ci_job_store.rs`, `src/lock.rs` | 01 |
| 2 | 03 — Ephemeral Ed25519 Key Management | `lsbx-keys` | `src/keygen.rs`, `src/reconcile.rs` | 01 |
| 2 | 04 — Backend Conformance Test Kit | `lsbx-backend-testkit` | `src/lib.rs` (the shared `backend_conformance_suite!` macro/fn set) | 01 |
| 3 | 05 — Demo/Mock Backend | `lsbx-backend-demo` | `src/lib.rs` | 01, 04 |
| 3 | 06 — Local + Remote Libvirt Backend | `lsbx-backend-libvirt` | `src/lib.rs`, `src/transport.rs`, `src/image_ops.rs` | 01, 04 |
| 3 | 07 — Exedev SSH Backend | `lsbx-backend-exedev` | `src/lib.rs`, `src/ssh.rs`, `src/http_fallback.rs` | 01, 04, 03 (key-comment convention only) |
| 4 | 08 — Golden Image Registry & Build Lifecycle | `lsbx-golden` | `src/registry.rs`, `src/build.rs`, `src/hash.rs`, `src/verify.rs` | 01 (backend trait, exercised via 05 in tests) |
| 4 | 09 — VM Lifecycle Orchestration & Reaper | `lsbx-lifecycle` | `src/create.rs`, `src/reap.rs`, `src/lease.rs` | 01, 02, 03 (backend trait, exercised via 05 in tests) |
| 5 | 10 — Shared Operations Façade | `lsbx-ops` | `src/lib.rs` (one module per operation) | 08, 09 |
| 6 | 11 — CLI Surface & Output Formatting | `lsbx-cli` | `src/cli.rs`, `src/format.rs`, `src/main.rs` | 10 |
| 6 | 12 — Ratatui TUI Dashboard & Wizard | `lsbx-tui` | `src/dashboard.rs`, `src/wizard.rs` | 10 |
| 6 | 13 — Axum HTTP Gateway | `lsbx-gateway` | `src/routes.rs`, `src/auth.rs`, `src/ratelimit.rs` | 10 |
| 6 | 14 — WebSocket Stream Proxy & noVNC Console | `lsbx-stream` | `src/proxy.rs`, `src/console.rs` | 10, 02 (destination lookup) |
| 6 | 15 — Stdio MCP Server | `lsbx-mcp` | `src/lib.rs`, `src/tools.rs` | 10 |
| 7 | 16 — CI Broker: GitHub App Auth & Repo Discovery | `lsbx-broker` | `src/auth.rs`, `src/github_client.rs` | 01, 02 |
| 7 | 17 — CI Broker: Queue Polling & Label Matching | `lsbx-broker` | `src/poll.rs`, `src/labels.rs` | 16 |
| 7 | 18 — CI Broker: Job↔VM Reconciliation | `lsbx-broker` | `src/reconcile.rs`, `src/job_record.rs` | 17, 10 |
| 8 | 19 — Golden Flattening & Host Bootstrap | `lsbx-bootstrap` | `src/verify_host.rs`, `src/systemd.rs`, `src/flatten.rs` | 06, 08 |
| 8 | 20 — Workspace Manifest, CI Workflow & Compat Fixtures | (workspace root) | `Cargo.toml`, `.github/workflows/ci.yml`, `tests/fixtures/` | all |

Layers 2, 3, and 6 are each internally parallel — every unit in a layer can be dispatched to an isolated builder (Herdr pane / Jules session) simultaneously once the layer above it has landed. Units 16–18 share one crate (`lsbx-broker`) but own disjoint files within it; land them in order (16 → 17 → 18) even though they're not different crates, since 17 imports 16's client and 18 imports 17's poll loop.

---

## 6. Exit Code Taxonomy

Per `bplate`'s cross-tool standard (`docs/units/10-exit-code-and-json-envelope-standard.md`): `0`, `2`, and `5` are a fixed floor; `1` is reserved and never assigned; everything else is domain-specific to `lsbx` but fully documented here.

| Code | Name | Meaning |
|---|---|---|
| 0 | `SUCCESS` | |
| 1 | *(reserved)* | never assigned |
| 2 | `USAGE` | bad CLI arguments or a malformed request |
| 3 | `BACKEND_UNAVAILABLE` | the selected backend's control plane is unreachable (libvirt socket down, exe.dev endpoint unreachable) |
| 4 | `NOT_FOUND` | a referenced sandbox id, golden key, profile, or image key does not resolve |
| 5 | `CONTRACT_VIOLATED` | verification failed — a healthcheck, a readiness timeout, or an output contract |
| 6 | `LOCK_CONTENTION` | a required lock (broker lock, sandbox operation lock) is held by another process |
| 7 | `AUTH_FAILED` | GitHub App JWT/installation-token failure, or a gateway bearer-auth rejection |
| 8 | `INTERRUPTED` | a long-running operation was signal-interrupted mid-flight |

`NOT_FOUND` (4) deliberately covers both "no such sandbox" and "no such golden/profile" under one code — both are "the identifier you gave me doesn't resolve," and proliferating near-duplicate codes for the same failure shape doesn't help an agent branch on it any better. Same logic for `AUTH_FAILED` (7) covering both CI-broker and gateway auth failures.

---

## 7. JSON Envelope & CLI Contract Conventions

Exactly `bplate`'s standard, unchanged: `{"status":"success","data":{...}}` on success, `{"status":"error","code":<numeric>,"message":"..."}` on failure, where `code` is always the process's actual exit status — never a value that disagrees with what the shell sees. Every subcommand supports `--json`; every mutating subcommand that can be previewed supports `--dry-run`.

---

## 8. Crate Workspace Architecture & Topological Build/Landing Order

```
lsbx/                              (Cargo workspace root)
├── Cargo.toml                     (workspace members, shared profile/lints)
├── crates/
│   ├── lsbx-kernel/                Layer 1
│   ├── lsbx-store/                 Layer 2
│   ├── lsbx-keys/                  Layer 2
│   ├── lsbx-backend-testkit/       Layer 2
│   ├── lsbx-backend-demo/          Layer 3
│   ├── lsbx-backend-libvirt/       Layer 3
│   ├── lsbx-backend-exedev/        Layer 3
│   ├── lsbx-golden/                Layer 4
│   ├── lsbx-lifecycle/             Layer 4
│   ├── lsbx-ops/                   Layer 5
│   ├── lsbx-cli/                   Layer 6  (produces the `lsbx` binary)
│   ├── lsbx-tui/                   Layer 6
│   ├── lsbx-gateway/               Layer 6
│   ├── lsbx-stream/                Layer 6
│   ├── lsbx-mcp/                   Layer 6
│   ├── lsbx-broker/                Layer 7
│   └── lsbx-bootstrap/             Layer 8
├── docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/
├── tests/fixtures/                 (real images.json, images.carnyx.json, legacy SandboxRecord samples, copied verbatim from lufs-sandbox-server)
└── .github/workflows/ci.yml
```

`crates/lsbx-cli` is the sole binary crate (`[[bin]] name = "lsbx"`) and the composition root — it depends on every door crate and wires them in as subcommands (`lsbx serve` → gateway + stream, `lsbx mcp` → MCP server, `lsbx ci-broker run` → broker), matching the existing system's one-binary, many-subcommands shape rather than splitting into several installable binaries.

**Landing order** (what `land-plane` should merge, in sequence, respecting the layer table in §5): 01 → {02, 03, 04} → {05, 06, 07} → {08, 09} → 10 → {11, 12, 13, 14, 15} → 16 → 17 → 18 → 19 → 20. Everything inside a `{}` group is safe to land in any order relative to its siblings, since none of them touch each other's files.

**Scaffold note (Deviation 10):** `bplate` doesn't yet have a profile for a multi-crate Rust workspace. Phase 2 should hand-scaffold the workspace root and per-crate `Cargo.toml`s directly against the layout above rather than wait on a new `bplate` profile — but a `rust-workspace` profile generalizing this shape is worth proposing to `bplate` afterward, since this will not be the last multi-crate LUFS Primitive.

---

## 9. Technology Stack

Confirmed against the current crate ecosystem (not assumed from training-era defaults) at spec time:

| Concern | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` | full-featured, everything in this system is I/O-bound |
| Error handling | `thiserror` | zero-panic monadic errors per house `rust-dev` standard |
| Local libvirt bindings | `virt` (0.4.x) | still the standard, maintained inside the libvirt project itself; needs `libvirt-dev` headers |
| Remote SSH transport | `russh` (0.6x) | pure-Rust, Tokio-native, protocol-level exec + SFTP without shelling to `ssh`; chosen over `openssh` (which wraps the system binary) because the remote-libvirt and exedev backends need programmatic batch-mode control (stdin isolation, precise timeouts), not `~/.ssh/config` convenience |
| MCP server SDK | `rmcp` (3.x) | the now-official SDK from the `modelcontextprotocol` org; stdio transport via the `transport-io` feature |
| TUI | `ratatui` (0.30.x) | immediate-mode `Frame`-based rendering |
| HTTP framework | `axum` (0.8.x) | `WebSocketUpgrade` extractor for the stream door, `FromRequestParts`-based bearer-auth extractor composed alongside it |
| WebSocket framing | `tokio-tungstenite` (0.30.x) | no mature websockify/noVNC-proxy crate exists — the bidirectional relay is hand-rolled on top of this, bridging to a raw `TcpStream` toward the guest VNC port, same as the existing system's raw-socket relay, just async and native |
| Ed25519 keys | `ed25519-dalek` (3.x) | breaking RNG-plumbing change from the 2.x line (`rand_core` 0.10's fallible/infallible trait split) — pin the exact `ed25519-dalek`/`rand_core` pair together and verify the generation call compiles before trusting any AI-generated snippet of it |
| GitHub App JWT | `jsonwebtoken` (11.x) | RS256 signing, replaces the `openssl` subprocess shell-out |
| GitHub API client | `octocrab` (0.5x) | has a dedicated `apps` module for GitHub App auth flows; used for repo discovery and Actions job/run queries |
| Content hashing | `sha2` (0.11.x) | unchallenged standard, hardware-accelerated backends selected automatically |
| Advisory file locking | `fs4` (1.x) | pure-Rust (`rustix`-based, no libc), async-runtime-aware — the primitive Unit 02's lock sentinel is built on |
| CLI parsing | `clap` (v4, derive API) | |

---

## 10. Verification Strategy

Every unit passes the standard three-tier gate before a PR is opened: `cargo check --message-format=json` → `cargo clippy --all-targets --all-features -- -D warnings` → `cargo test`. Beyond that floor, three things make this system's *correctness* claims earn their keep rather than just its *compile* claims:

1. **Backend conformance, not backend trust.** Every `Backend` implementation is run against the same conformance suite (Unit 04): create→run→destroy idempotence within tolerance, `capabilities()` truthfulness (a backend never claims a capability it doesn't have), and `list_vms()` reflecting reality after create/destroy. A backend that passes its own unit tests but fails the shared conformance suite is not done.
2. **Byte-identical compatibility fixtures.** The real `images.json` and `images.carnyx.json` from `lufs-audio/lufs-sandbox-server`, and real (including legacy flat) `SandboxRecord` samples, are copied verbatim into `tests/fixtures/` (Unit 20) and parsed in CI. A schema change that breaks these fixtures fails the build — this is what makes "100% schema and functional compatibility" a checked fact instead of a claim in a commit message.
3. **Divergence and reconciliation are tested paths, not just logged ones.** The CI broker's divergence detection (Unit 18) and the reaper's `allowed_goldens()`-style protection (Unit 09) get dedicated tests that force the divergent/protected case, not just the happy path — mirroring the exact class of bug the `snuze` audits found twice: defects invisible to green tests because nobody wrote the test that exercises the cross-unit interaction.

---

## 11. Done Criteria

- [ ] All 20 units land, in dependency order, each with a green three-tier gate.
- [ ] `tests/fixtures/` contains the real `images.json`, `images.carnyx.json`, and at least one legacy-flat and one current-schema `SandboxRecord`, all parsed successfully by `lsbx-golden`/`lsbx-store`.
- [ ] The `agent-base` golden base-name mismatch (Deviation 2) is preserved exactly, not harmonized, and is asserted by a test that would fail if someone "fixed" it accidentally.
- [ ] Every door (CLI, HTTP, WS stream, MCP) can create, list, exec into, and destroy a `demo`-backend sandbox end-to-end with no real infrastructure.
- [ ] The CI broker runs a full auth → discover → poll → dispatch → reconcile cycle against a mocked GitHub API in tests, including a forced-divergence case.
- [ ] `lsbx golden build` produces a golden with a real, populated, verified `lufs-<sha256[:8]>` content hash.
- [ ] `.github/workflows/ci.yml` exists, runs on `[self-hosted, lufs]` at ship time, and has `vars.LSBX_CI_PLACEMENT` wired so the cutover to `lsbx-default` is a one-line config change later, not a code change.
- [ ] A KB product suite exists at `lufs-audio/kb` → `docs/product/lsbx/`, per `bplate`'s documentation-and-workflow standard.
- [ ] Every deviation in §0 has either been ratified by Daniel or explicitly reversed.
