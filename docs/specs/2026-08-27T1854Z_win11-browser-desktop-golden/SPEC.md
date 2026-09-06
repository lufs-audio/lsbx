# Phase spec — Win11 browser-desktop golden

**Timestamp:** `2026-08-27T1854Z`
**Slug:** `win11-browser-desktop-golden`
**Branch (repo of record):** `feature/win11-browser-desktop-golden` (off `feature-final-integration-wiring`), in `~/repos/lsbx` — the ground-up Rust rewrite of `lsbx`. This phase is about *using* already-landed lsbx tooling to add a Windows golden; it introduces **no Rust source changes** and only edits the carnyx image manifest during Unit 03.

---

## Problem

Carnyx hosts a manually provisioned Windows 11 guest (`win11`) that predates lsbx development. Today it is a standalone libvirt VM reachable only via its SPICE console from carnyx — there is **no lsbx golden** for it, so it cannot be provisioned as a disposable, browser-served desktop the way the Linux `lsbx-web-v1` golden is (`lsbx up … desktop` → noVNC console via the Molimo `:8247` stream proxy).

We want a registered `win11-desktop` golden built **from the existing Windows 11 install** (not a from-scratch unattended reinstall), that:

- clones per `lsbx up` like any other libvirt `Copy`-mode golden (COW overlay),
- exposes a browser noVNC desktop through the existing carnyx stream path (in-guest `:8000` → Molimo `:8247` → console), and
- passes the golden's SSH-based healthcheck so it is a *proven* golden, not just a bootable disk.

## Goals / constraints

- **Build the golden from the existing `win11.clean-install` install** — no Autounattend.xml unattended-install work in this phase.
- **Do not alter the existing manually-provisioned Win11 guest or volumes.** `win11.qcow2`, `win11.clean-install`, `win11-mem.clean-install`, and the `win11` libvirt domain must remain byte-identical and still defined (`virsh list --all`). All phase work happens on fresh COW clones of `win11.clean-install`.
- **Reuse existing lsbx code unchanged.** Hard constraints from the code:

  - **Streaming is VNC-on-guest-`:8000`.** `lsbx-stream/src/proxy.rs:97` hardcodes `GUEST_VNC_PORT: u16 = 8000`; the libvirt backend renders `<graphics type='vnc' port='-1' autoport='yes'/>` (`dbx-backend-libvirt/src/domain_xml.rs:132`). Therefore the Windows golden must run its **own VNC server + browser bridge inside the guest** on `:8000` ↔ `:5900`. The manual `win11` domain's SPICE graphics are **not** part of a clone. RDP / Guacamole is out of scope (see Boundaries).
  - **Management is SSH.** Golden healthchecks and `Backend::run` drive the guest over SSH (`lsbx-backend-libvirt/src/guest_ssh.rs`, username `lsbx`, key `~/.ssh/lsbx_guest_key`). This phase installs **OpenSSH Server for Windows** in the guest; it is currently **not** installed.
  - **libvirt `Copy` mode flattens per-sandbox** (COW overlay on the golden qcow2). The golden itself must therefore be a **flattened, self-contained qcow2** (no backing file pointing at `win11.qcow2`), produced by the existing `qemu-img convert` path in `lsbx-bootstrap/src/flatten.rs`.

## What "done" means (phase acceptance)

1. A registered `win11-desktop` golden exists in the carnyx manifest (`images.carnyx.json`), `os: windows`, `flavor: desktop`, `streaming: novnc`, backed by a **flattened** `~/ISOs/images/goldens/win11-desktop.qcow2`.
2. `lsbx --backend libvirt --images images.carnyx.json golden verify win11-desktop` runs its SSH healthcheck successfully against a fresh clone.
3. `lsbx --backend libvirt --images images.carnyx.json up win11-desktop --lease PT20M` returns a `console_url`; that URL serves a live interactive Windows desktop in a browser through the Molimo `:8247` stream proxy, with no further manual intervention.
4. The original `win11` volumes + domain are untouched (sha256 + `virsh list --all` proof recorded in Unit 03).

## Design approach

### Golden construction (3 units, mostly ops)

| Unit | What | Why first |
|---|---|---|
| **01-provision-ssh** | Clone `win11.clean-install` → disposable `provision-win11` VM; one-time **SPICE console session** installs **OpenSSH Server for Windows**; inject `lsbx_guest_key`, harden (password auth off, key-only); seal → `goldens/win11-ssh.qcow2`. | SSH does not exist in the guest yet and is the management channel for everything downstream. That first console session is unavoidable; after it, no more console needed. |
| **02-install-streaming** | Over SSH, install **TightVNC Server** (loopback `:5900`) + **Python websockify** as a Windows service binding `:8000 → 127.0.0.1:5900`, reusing the existing noVNC asset dir from `scripts/build-desktop-golden.sh`; prove `http://127.0.0.1:8000` answers in-guest. | Needs SSH from 01; is the in-guest half of the browser desktop. |
| **03-flatten-register** | `qemu-img convert` the provisioned clone → `goldens/win11-desktop.qcow2`; `lsbx golden register` (GoldenConfig + profile); `golden verify`; `up` → confirm live browser console; record preservation proof. | Consumes 01+02; owns every "make it real and prove it" acceptance criterion. |

### Streaming design (the "how the browser actually sees Windows")

Mirror the Linux desktop golden's browser bridge (`scripts/build-desktop-golden.sh`: `websockify --web $NOVNC_DIR 8000 localhost:5900`), as Windows guests:

- **VNC in-guest:** TightVNC **Server** (not the Java viewer) bound **127.0.0.1:5900**, password-locked — mirrors the Linux `-localhost` + passfile posture so it is not exposed to the VM's external interface.
- **Bridge in-guest:** **Python websockify** (`pip install websockify`, runs as an `sc`/scheduled-task Windows service) on `0.0.0.0:8000 → 127.0.0.1:5900`, served with the same noVNC web assets the Linux golden ships.

`lsbx-stream` then relays `wss://molimo.exe.xyz:8247/… → <guest ip>:8000` identically to the Linux desktop — **zero Rust changes**.

### The one-time interactive SPICE step (answer to "how would the spice session work")

SPICE is the existing `win11` domain's native remote-viewer protocol, already how the manual guest is administered today:

- From carnyx: `virt-viewer --connect qemu:///system --domain-name provision-win11` (or `remote-viewer spice://<host>:<port>` after `virsh started` prints the SPICE port). It attaches **no physical monitor** — SPICE is a virtual display you see in a window on carnyx, same as the noVNC browser experience but native.

- A human drives that one window **once**: enable the built-in "OpenSSH Server (sshd)" Windows Optional Feature, start `sshd`, drop the lsbx public key into `C:\ProgramData\ssh\administrators_authorized_keys`, and confirm a key-only SSH login works from carnyx. After that window closes, this phase never opens it again — Units 02/03 and sandbox healthchecks are all SSH.

## Boundaries

- **No Rust source changes** anywhere in `~/repos/lsbx/crates/`. A genuine lsbx bug found is reported, not fixed here.
- **Does NOT** alter/delete/rename `/var/lib/libvirt/images/win11.qcow2`, `win11.clean-install`, `win11-mem.clean-install`, or redefine/un-define the `win11` domain.
- **Does NOT** implement RDP, Guacamole, a Windows-native agent, unattended Win11 install (Autounattend), or change `GUEST_VNC_PORT` / the domain-XML `<graphics>` device.
- **Does NOT** touch the exe.dev backend, Molimo Caddy, or the CI broker.

## Ecosystem references (interfaces this phase relies on)

- `crates/lsbx-stream/src/proxy.rs` — `GUEST_VNC_PORT: u16 = 8000` (hardcoded); WS relay → `<guest>:8000`.
- `crates/lsbx-backend-libvirt/src/lib.rs`, `domain_xml.rs`, `guest_ssh.rs` — `DiskMode::Copy`; `<graphics type='vnc' port='-1' autoport='yes'/>`; SSH `run`/healthcheck target (username `lsbx`, key `~/.ssh/lsbx_guest_key`).
- `crates/lsbx-golden/src/registry.rs`, `build.rs`, `verify.rs` — GoldenConfig schema (os/flavor/streaming), build, SSH healthcheck.
- `crates/lsbx-bootstrap/src/flatten.rs` — `qemu-img convert` used to flatten the provisioned clone into the registrable golden.
- Ops reality (from `MIGRATION-CARNYX.md` + `lufs-sandbox-server/docs/live-tests/2026-08-20-cloud-desktop-final-validation/03-win11-feasibility.md`): carnyx golden dir `~/ISOs/images/goldens/`; `LUFSS_CONSOLE_BASE=https://molimo.exe.xyz:8246`, `LUFSS_STREAM_BASE=https://molimo.exe.xyz:8247`; `LIBVIRT_DEFAULT_URI=qemu:///system`; the manual `win11` guest is a preserved asset. External Windows tooling (OpenSSH Server, TightVNC, Python+websockify) appears only as install steps in Units 01/02.

## Phase verification

```bash
# carnyx, from this repo branch, after Unit 03:
lsbx --backend libvirt --images images.carnyx.json golden list            # win11-desktop present
lsbx --backend libvirt --images images.carnyx.json golden verify win11-desktop
SBX=$(lsbx --backend libvirt --images images.carnyx.json up win11-desktop --lease PT20M --json | jq -r .id)
lsbx --backend libvirt --images images.carnyx.json console "$SBX"       # molimo.exe.xyz:8247 URL
# open console URL in a browser → live interactive Windows desktop
lsbx --backend libvirt --images images.carnyx.json down "$SBX"

# Preservation proof:
sha256sum /var/lib/libvirt/images/win11.qcow2 /var/lib/libvirt/images/win11.clean-install /var/lib/libvirt/images/win11-mem.clean-install
virsh -c qemu:///system list --all | grep -w win11                # still defined, shut off
```
