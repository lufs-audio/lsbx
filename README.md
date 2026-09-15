# lsbx

A pluggable, dual-backend disposable-VM engine and zero-idle GitHub Actions CI
runner broker, for LUFS Audio.

`lsbx` provisions short-lived, ephemeral virtual machines on demand — for CI
runners, sandboxed agent execution, and interactive dev/demo environments —
runs work inside them over four doors (CLI, HTTP, WebSocket console, MCP),
and tears them down. It is the ground-up Rust rewrite of
[`lufs-audio/lufs-sandbox-server`](https://github.com/lufs-audio/lufs-sandbox-server),
which remains the read-only reference for existing behavior and on-disk
schemas this rewrite must not silently break.

**Status:** Implemented and in production. The 17-crate workspace is complete
(spec: [`SPEC.md`](SPEC.md), 20 unit contracts under
`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/`); the Rust gateway and
CI broker cut over on Molimo on 2026-08-27 (see
`docs/molimo-lsbx-rust-migration-2026-08-27.md`) and on Carnyx the same week.
The libvirt backend provisions both Linux and Windows goldens; macOS/
Windows-native provisioning doors are follow-ups tracked in the specs below.

## The exe.dev backend

The `lsbx-backend-exedev` crate runs VMs on [exe.dev](https://exe.dev) over
two co-equal transports, selected by auth mode:

| Auth mode | Config | Transport |
|---|---|---|
| `AccountToken` | `EXE_TOKEN` (or `LSBX_EXEDEV_TOKEN_ENV` naming the var) | HTTPS `POST https://exe.dev/exec` — control verbs + guest exec |
| `VmScopedToken` | a `v0@VM.exe.xyz` token | Same HTTPS path; the lobby scopes `ssh <vm>` |
| `Ssh` | `LSBX_EXEDEV_SSH_KEY` (private key path) | SSH via `russh` |
| `SshAlias` | `LSBX_EXEDEV_SSH_ALIAS` (default `exe.dev`) | SSH via `russh` |

Token auth (HTTPS) is fully self-sufficient: every account-level verb
(`ls --json`, `cp`, `tag`, `ssh-key add/remove`) **and** guest command
execution work over one bearer token — guest commands ride
`ssh <vm> <cmd>` with an in-band `__LSBX_EXIT:$?` exit sentinel (the
`X-Exe-Exit` trailer is not exposed through proxy chains). The HTTPS path
has a ~30 s server-side cap and merges stderr into the response body, so
SSH remains the door for interactive tooling, file transfer, and long jobs.
This shape (verified live 2026-09-04, fixing #30/#31) is what lets a
cloud agent with no SSH key operate the same backend a local agent uses
over SSH.

Use `lsbx` for sandbox lifecycle; raw ssh-over-exec is the fallback surface
for hosts without `lsbx` installed.

## Start here

- [`AGENTS.md`](AGENTS.md) → operating contract for agents (CI authoring,
  broker ops, safety rails)
- [`SPEC.md`](SPEC.md) → the authoritative spec pointer (root rewrite spec +
  the Win11 browser-desktop golden phase spec)
- [`CHANGELOG.md`](CHANGELOG.md) → what changed and when
- Or jump straight to the command reference below.

## Building and verifying

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Infrastructure-requiring tests (real libvirt/KVM, real exe.dev SSH/HTTPS) are
excluded from the default `cargo test` run; run them manually on a host with
that infrastructure present:

```bash
cargo test --workspace -- --ignored
```

## Usage

`lsbx` runs against a real backend selected by `--backend` (`libvirt`,
`exedev`, `demo`) or `--backend auto`, which probes libvirt → exedev → demo.

### Sandbox lifecycle

| Command | What it does |
| --- | --- |
| `lsbx up <profile>` | Provision a sandbox from a profile or golden (flags: `-n/--name`, `-t/--task-id`, `-l/--lease`, `--no-verify`, `--ready-timeout`) |
| `lsbx down <id>…` / `--all` | Tear sandbox(es) down |
| `lsbx list` | Show live sandboxes (`--expired` for the reaper's target set) |
| `lsbx exec <id> -- <cmd>` | Run a command inside a sandbox over guest SSH |
| `lsbx put` / `lsbx get` | Copy files in/out via `scp` |
| `lsbx renew <id> <duration>` | Extend a sandbox's lease |
| `lsbx console <id>` | Open the WebSocket-noVNC console (stream proxy) |
| `lsbx info <id>` / `lsbx status` | Per-sandbox detail / backend summary |

### Goldens

| Command | What it does |
| --- | --- |
| `lsbx golden list` | Show registry goldens and profiles |
| `lsbx golden reconcile` | Cross-reference manifest goldens against the backend's live VM inventory (`present`/`missing` per golden, plus unregistered golden-shaped VMs) |
| `lsbx golden build <name> --from <golden> --script <path> …` | Rebuild a golden from a provisioning script |
| `lsbx golden verify <golden>` | Boot a fresh clone and run its declared healthchecks, then destroy it |
| `lsbx golden register <name> --base <golden> --flavor <flavor> [--os] [--cpu] [--memory] [--disk] [--mode] [--repo] …` | Add a golden to the registry |
| `lsbx golden delete <name>` | Remove a golden from the registry |
| `lsbx images` / `lsbx profiles [--full]` | Inspect the loaded image manifest / profiles |

Registry mutations (`golden register`/`delete`, and `golden build --register`)
persist straight back to the manifest the CLI loaded (see
`LSBX_IMAGES` below), so one-shot invocations survive process exit.

### Windows goldens

`golden register … --os windows` (or a hand-maintained manifest entry with
`"os": "windows"`) makes a golden first-class across `verify`/`up`:

- The libvirt backend renders a UEFI/SecureBoot domain (q35, OVMF
  `_VARS.fd` per VM, `hyperv` enlightenments, `smm`, swtpm 2.0, localtime +
  hypervclock, qxl) and never attaches a cloud-init seed CD-ROM — Windows
  guests don't run cloud-init, so the SSH key the broker needs must be baked
  into the golden's `authorized_keys` (see the spec's golden-baking notes).
- No ephemeral keypair is generated or registered; commands run through the
  backend's baked-guest-identity fallback key.
- Healthchecks execute under the guest's `cmd.exe` session (the transport
  reconstructs `cmd /c …` argv with cmd.exe quoting), not `sh -c`, so
  healthchecks like `echo lsbx-windows-ok` and
  `curl -fsS http://127.0.0.1:8000/vnc.html -o NUL` work verbatim.

The verified Win11 desktop golden on Carnyx (`win11-desktop`, `os: windows`,
`streaming: novnc`) exposes its console as noVNC:

```bash
LSBX_LIBVIRT_USER=lsbx lsbx golden verify win11-desktop
LSBX_LIBVIRT_USER=lsbx lsbx up win11-desktop
# → browser at http://<guest-ip>:8000/vnc.html  (VNC password `lsbx`)
```

### Services

| Command | What it does |
| --- | --- |
| `lsbx serve` | Axum HTTP gateway + WebSocket/noVNC stream proxy + broker sidecar |
| `lsbx ci-broker run --backend <libvirt\|exedev>` | Zero-idle CI broker (GitHub App or `gh`-CLI auth) |
| `lsbx mcp` | stdio MCP server for tool agents |
| `lsbx reap` | TTL sweep with orphaned-key reconciliation |

## Placement configuration (libvirt)

Everything roots under the state directory (`--state-dir` /
`LSBX_STATE_DIR`, default `~/.local/share/lsbx` unless a host convention
overrides it) unless the environment says otherwise:

- `LSBX_LIBVIRT_URI` — connection URI (default `qemu:///system`)
- `LSBX_LIBVIRT_IMAGES_DIR` / `LSBX_LIBVIRT_VM_DIR` — where golden qcow2
  files are read from and per-VM disks are written (defaults
  `<state_dir>/images` and `<state_dir>/vms`)
- `LSBX_LIBVIRT_USER` (fallback `LUFSS_LIBVIRT_USER`) — guest SSH username
  (`exedev` by default; use `lsbx` for the baked Win11 desktop golden)
- `LSBX_IMAGES` / `LSBX_IMAGES_PATH` / `--images` — image-manifest path
  (precedence: `--images` flag, `LSBX_IMAGES_PATH`, then the long-standing
  `LSBX_IMAGES` host convention; falls back to `<state_dir>/images.json`).
  When no manifest exists at the resolved default path, the CLI notes this
  on stderr and proceeds with an empty registry — an explicit `--images`
  path that is absent stays silent, since the caller chose it. `lsbx
  golden reconcile` sees past a missing manifest by asking the backend
  what golden-shaped VMs actually exist.

Carnyx's host config is the working reference: goldens in
`/home/carnyx/ISOs/images/goldens/`, VM disks in `.../work/`, manifest
`LSBX_IMAGES=/home/carnyx/repos/lsbx/images.carnyx.json`.

## Specs

- [`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/`](docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/)
  → the root 20-unit rewrite spec
- [`docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/`](docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/)
  → the Win11 browser-desktop golden phase (bake-in, streaming, register
  via `lsbx golden`)

## License

GPL-3.0, matching the other LUFS Primitive CLIs (`snuze`, `apho`, `lrex`).