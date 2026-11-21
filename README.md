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

**CI dispatch (2026-11-14):** Linux CI jobs on Carnyx are dispatched by
**push**, not polling: [`sidecars/lsbx-scaleset`](sidecars/lsbx-scaleset/)
registers the GitHub runner scale set `lsbx-linux` in runner group `continuo`,
long-polls GitHub's job queue (no repo list, ~0 REST calls) and boots one lsbx
VM per assigned job with a single-use JIT runner config — the GitHub App key
never enters a guest. The polling broker (`lsbx ci-broker run`) now serves only
`lsbx-windows`. See [CI on Carnyx](#ci-on-carnyx).

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
- [Carnyx desktop rollout](docs/carnyx-desktop-runbook.md) → browser recovery, Linux resize canary, and Windows audio/streaming gates
- [`CHANGELOG.md`](CHANGELOG.md) → what changed and when
- Or jump straight to the command reference below.

## Remote CLI over the gateway

Point the client at the same gateway you use for the browser console. Set
`LSBX_GATEWAY_TOKEN` in the environment from your secret manager (prefer that
instead of passing credentials on the command line).

```bash
export LSBX_GATEWAY_URL=https://lsbx.lufshq.com
lsbx status
lsbx list --json
lsbx up win11 --lease 30m
lsbx console <id> --open
lsbx down <id>
```

`--gateway <url>` is the flag form; don't combine it with `--backend`. Normal
HTTPS certificate validation stays on. `--insecure` explicitly allows a
plaintext gateway URL for local development only. Discovery and lifecycle run
on the gateway, not through remote libvirt. `put`/`get` transfer bytes (up to
2 MiB per upload and 32 MiB per download); larger artifacts need another approved
transfer path. Remote clients don't need server-side local-file access. Golden
mutations, service management, and the TUI remain local-only. Configure
`LSBX_PUBLIC_BASE_URL` on Carnyx so published console links use the gateway.

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
| `lsbx dashboard` (aliases: `tui`, `dash`) | Interactive Ratatui TUI dashboard (launched by default on interactive TTYs) |

### Goldens

| Command | What it does |
| --- | --- |
| `lsbx golden list` | Show registry goldens and profiles |
| `lsbx golden reconcile` | Cross-reference manifest goldens against what the backend actually has (`present`/`missing` per golden, plus unregistered artifacts): the libvirt image directory, or exedev's golden-shaped VMs |
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
  `powershell -NoProfile -File C:\lsbx\rfb-check.ps1` work verbatim.

The verified Win11 desktop golden on Carnyx (`win11-desktop`, `os: windows`,
`streaming: novnc`, aliased to profile `win11`) runs with 8 vCPUs, 32 GB RAM,
and 256 GB virtual disk, exposing its console as noVNC:

```bash
LSBX_LIBVIRT_USER=lsbx lsbx golden verify win11-desktop
LSBX_LIBVIRT_USER=lsbx lsbx up win11
# With LSBX_PUBLIC_BASE_URL configured, the console URL uses the gateway.
lsbx console <id>   # prints the active console URL
```

Interactive customization sessions can be committed directly back into the golden:

```bash
./scripts/lsbx-commit-golden.sh <sandbox-id> win11-desktop
```

And reproducible builds from scripts support `--os windows`:

```bash
lsbx golden build win11-desktop --from win11 --script scripts/provision-win11-golden.ps1 --flavor desktop --cpu 8 --memory 32GB --os windows --streaming novnc
```

### Linux desktop golden recipe and resize canary

`scripts/provision-linux-desktop-golden.sh` is the in-repo recipe for the XFCE
`agent-web` stack. The legacy repository named by #58 was unavailable through the
attached read-only integration when this recipe was added, so its provenance is
stated precisely in the script: `compat-xvfb-websockify-v1` is reconstructed from
the command/package excerpts archived in this repository, not claimed as an
exact copy. The legacy repository remains read-only and was not modified.

The default compatibility mode preserves the old fixed-resolution
Xvfb → x11vnc → websockify shape. It exists for reproducibility, not as a new
production recommendation: the current gateway relay expects **raw RFB** on guest
`:8000`, while this compatibility stack exposes an HTTP/WebSocket bridge there.

The #51 experiment is an explicit, versioned canary. It uses TigerVNC `Xvnc` as
both the RANDR-capable X server and raw-RFB server; no manifest or profile opts
into it by default:

```bash
# Inside an approved disposable Linux guest, stage an operator-chosen, exactly
# eight-character password in a root-owned mode-0600 tmpfs file. Canary mode has
# no password default.
sudo env RFB_PRIVATE_CIDR=192.168.122.0/24 \
  VNC_PASSWORD_FILE=/run/lsbx-vnc-password \
  ./scripts/provision-linux-desktop-golden.sh --mode tigervnc-raw-rfb-v1
sudo rm -f /run/lsbx-vnc-password

# On Carnyx, using the guest's private address. The probe has no default and
# prompts without echo; automation may use LSBX_VNC_PASSWORD instead.
scripts/rfb-resize-canary.py --host <guest-private-ip> --port 8000 \
  --width 1600 --height 900

# Enable native resize for this browser session only:
# /console?target=<id>&resize=remote
```

The canary resolves a boot-time address inside `RFB_PRIVATE_CIDR` and passes it
to TigerVNC's `-interface`; it never intentionally listens on a wildcard. This
selects the local interface but is **not a source ACL**: the operator must ensure
the private bridge/firewall allows only the authorized gateway path.

Do not replace the `agent-web` manifest entry until a fresh canary clone passes
the raw-RFB banner check, `xrandr`, the RFB resize canary, XFCE reflow inspection,
and fallback/reconnect testing. Provision/flatten into a new backing key. The
encoded VNC credential is retained in that image, is reversible legacy VNC
material, and is consequently shared by every clone; use a canary-only password,
keep the image private, and rotate/re-bake before promotion. See the
[runbook](docs/carnyx-desktop-runbook.md) for password and network handling. The
script supports Ubuntu/Debian package names and an Arch path; it checks actual
package/binary availability instead of silently downloading an unversioned VNC
or websockify build.

### Working rules for desktop goldens (read before customizing one)

These come from real sessions on carnyx; each one cost debugging time.

**Committing a session (`scripts/lsbx-commit-golden.sh`)**

- The script **destroys the sandbox**. Finish in the guest first; anything not
  installed is lost.
- It waits up to `LSBX_COMMIT_SHUTDOWN_WAIT` (default 180 s) for an ACPI shutdown
  and then **refuses to flatten a live disk** rather than producing a torn image.
  Windows can take minutes to honour ACPI when an installer is mid-flight; `--force`
  hard-powers-off and flattens anyway.
- Output is deliberately **uncompressed** — a golden is a live COW backing file, so
  a compressed backing cluster costs an inflate on every guest read (see #54).
- Growing the qcow2 (`qemu-img resize`) does **not** grow the guest partition.
  Extend it inside the guest: `reagentc /disable` → delete the inert Recovery
  partition that sits between `C:` and the free space (`diskpart delete partition
  override`) → `Resize-Partition` → `reagentc /enable`.
- Always follow with `lsbx golden verify <key>` and a real `lsbx up`.

**Reaching the guest**

- Console: `lsbx console <id>` prints the console URL. Use the gateway-hosted
  page from a client; the Windows guest's private port `8000` speaks raw RFB,
  not browser HTTP. A plain TCP forward to it doesn't serve noVNC.
- Management is SSH as user `lsbx` with `~/.ssh/lsbx_guest_key`, which **lands in
  session 0**. Anything that needs the interactive desktop — display mode changes
  above all — cannot run from there; see
  `docs/specs/2026-11-14T0425Z_win11-resize-measurement/FINDINGS.md`.
- Windows Search indexing is disabled and Defender exclusions are baked in for
  install/toolchain paths, so installer-heavy sessions are not double-taxed.

**Verifying VNC-level behaviour without a browser**

`scripts/rfb-desktopsize-probe.py` speaks RFB 3.8 to a guest's VNC port and prints
whatever pseudo-rects the server sends — including `DesktopSize`, which is how a
framebuffer change is detected. Run the probe on Carnyx against the private
raw-RFB listener for the test guest; do not expose that port publicly.

### Services

| Command | What it does |
| --- | --- |
| `lsbx serve` | Axum HTTP gateway + WebSocket/noVNC stream proxy + broker sidecar |

### Opening a desktop console in a browser

The gateway serves the console **and** the noVNC client, so a browser never needs
to reach a guest's private IP:

```
GET  /console?target=<sandbox-id>     HTML console (public page, no secrets)
GET  /assets/novnc/<path>             vendored noVNC 1.5.0 bundle (public)
POST /consoles/<id>/token             mint a short-lived ticket (bearer auth)
GET  /stream/<id>/vnc                 WebSocket relay (bearer OR ?t=<ticket>)
```

Usage:

1. `lsbx console <id>` prints the console URL (or open `/console` with no target
   for an index of running sandboxes).
2. The page asks for your **gateway token**, keeps it only in that tab's memory,
   and exchanges it for a 60-second, sandbox-scoped ticket.
3. The WebSocket then carries `?t=<ticket>`, because a browser cannot set an
   `Authorization` header on a WS handshake. Tickets are HMAC-SHA256 over a key
   derived from the gateway token, scoped to one sandbox id, and never cached
   (`Cache-Control: no-store`). The master token never appears in a URL, a log,
   or the served page.
4. If the RFB server requires a guest VNC password, the page prompts for it
   separately. The ticket only authorizes the relay.

Why the assets live here rather than in Caddy: the noVNC client and the
`/stream/*` protocol are one version contract, so vendoring keeps them pinned to
the same release by construction. See
`crates/lsbx-stream/assets/novnc/PROVENANCE.md` for the pinned upstream release,
checksum and the re-vendor procedure.
| `lsbx ci-broker run --backend <libvirt\|exedev>` | Zero-idle **polling** CI broker — today only `lsbx-windows` on Carnyx |
| `lsbx-scaleset run` ([`sidecars/lsbx-scaleset`](sidecars/lsbx-scaleset/)) | Go sidecar: runner scale set listener, push dispatch for Linux CI (`lsbx-linux`) |
| `lsbx mcp` | stdio MCP server for tool agents |
| `lsbx reap` | TTL sweep with orphaned-key reconciliation |

## CI on Carnyx

| `runs-on` | served by | runner group | how |
| --- | --- | --- | --- |
| `[self-hosted, lsbx-carnyx]` | scale set `lsbx-linux` | `continuo` | `lsbx-scaleset@linux` (systemd user unit), push |
| `[self-hosted, lsbx-default]` | scale set `lsbx-linux` | `continuo` | same — Molimo's broker is retired |
| `[self-hosted, lsbx-windows]` | polling broker | `continuo` | `lsbx-ci-broker.service` (`LSBX_QUEUE_LABEL=lsbx-windows`) |
| `[self-hosted, lufs, ARM64]` | Raspberry Pi runners | `continuo` | not lsbx |

Any repository the `continuo` group allows can use these labels — there is no
repo list to maintain. Keep the rule from [`AGENTS.md`](AGENTS.md): `self-hosted`
plus exactly one placement label.

Operating the listener:

```bash
systemctl --user status lsbx-scaleset@linux          # on carnyx, as user carnyx
journalctl --user -u lsbx-scaleset@linux -f          # scaling up / job started / runner retired
set -a; . ~/ISOs/images/scaleset-linux.env; set +a; lsbx-scaleset status   # persisted runner state
```

Concurrency is `LSBX_SS_MAX_RUNNERS` (4 VMs × 4 vCPU / 8 GB). Each job gets a
fresh `ci` VM (~14 s boot; queued → running ~25 s, measured over 5 jobs) that is destroyed
when the job completes; cancelled jobs, listener restarts and leases are
handled as described in the sidecar's
[README](sidecars/lsbx-scaleset/README.md).

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
  golden reconcile` sees past a missing manifest by asking the backend what
  golden artifacts actually exist — its image directory on libvirt, its
  golden-shaped VMs on exedev.

Carnyx's host config is the working reference: goldens in
`/home/carnyx/ISOs/images/goldens/`, VM disks in `.../work/`, manifest
`LSBX_IMAGES=/home/carnyx/repos/lsbx/images.carnyx.json`.

### Guest CPU topology and disk cache (why they are pinned)

Every domain renders an explicit `<topology sockets='1' dies='1' cores='N'
threads='1'/>` and `<driver ... cache='writeback'/>`. Both are load-bearing,
not defaults left unset:

- **One socket, N cores.** With no `<topology>`, libvirt defaults to *one
  socket per vCPU*, and Windows **client** SKUs are socket-limited (Home 1,
  Pro 2, Pro for Workstations 4). A `cpu: 8` Windows 11 Pro guest therefore
  reported `%NUMBER_OF_PROCESSORS% = 2` and ran on a quarter of its vCPUs —
  measured 2026-11-14 on the live `win11-desktop` sandbox (lsbx#54).
- **`cache='writeback'`.** Disposable VMs have no durability requirement worth
  O_DIRECT for; the default `cache=none` made installer-heavy work (Unity /
  Unreal / Wwise) thrash the disk. Host crash = lost unsynced writes, which is
  acceptable for a sandbox that exists to be destroyed.

Goldens must also be **uncompressed** (`qemu-img convert -O qcow2`, no `-c`):
with `mode: copy`, every sandbox is a COW overlay on the golden, so a
compressed backing cluster costs a zlib inflate on every read inside QEMU's
main loop. `scripts/lsbx-commit-golden.sh` is the supported way to flatten an
interactive session back into a golden and is written this way on purpose.

## Networking

Guest NICs are pinned to **MTU 1280** (domain XML, the libvirt bridge, and
cloud-init all set it) so guests stay correct on any path that may cross a
tailnet. Read [`docs/networking.md`](docs/networking.md) before changing any of
those three values — it records the measured facts, the failure mode a
"harmless" bump to 1500 causes, and the benchmark sequence to run first.

## Specs

- [`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/`](docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/)
  → the root 20-unit rewrite spec
- [`docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/`](docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/)
  → the Win11 browser-desktop golden phase (bake-in, streaming, register
  via `lsbx golden`)

- [`docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/`](docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/)
  → push dispatch via runner scale sets (`lsbx-scaleset`), decisions P1–P4 and status

## License

GPL-3.0, matching the other LUFS Primitive CLIs (`snuze`, `apho`, `lrex`).