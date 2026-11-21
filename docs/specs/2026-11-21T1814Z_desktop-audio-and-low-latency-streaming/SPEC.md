# Phase Spec — Desktop audio and low-latency streaming for Windows goldens

**Timestamp:** `2026-11-21T1814Z`
**Slug:** `desktop-audio-and-low-latency-streaming`
**Status:** Planning only. No code, no config, no golden re-bake in this turn.
**Related issues:** `lufs-audio/lsbx#51` (dynamic resize), `#56` (Windows display
path), `#58` (Linux desktop recipe), `#52` (transport doors — **parked by this
phase**), `#53` (bridge wedge), `#55` (noVNC from gateway — done).
**Supersedes:** nothing wholesale. Revises `#56` step 1 (see D7) and defers `#52`.

---

## 0. Problem

The `win11-desktop` golden is the workstation for audio-adjacent work — the
provisioning script already bakes in REAPER, Wwise, Q-SYS Designer, Unity and
Unreal (`scripts/provision-win11-golden.ps1`, Defender exclusion lists). But the
sandbox has **no audio at all**, and the only interactive door is a browser VNC
console that cannot carry audio by design.

Two independent gaps, discovered together on 2026-11-21:

1. **No audio device exists in the guest.** The domain XML that `lsbx` generates
   for a desktop golden (`crates/lsbx-backend-libvirt/src/domain_xml.rs`,
   the `flavor == desktop` branch) renders `<graphics type='vnc'>` and a QXL
   video device and **no `<sound>` and no `<audio>` element**. Windows therefore
   has no render endpoint: no application can play audio, not even to an
   offline render target that never leaves the VM.
2. **No transport can carry audio to a human.** RFB/VNC has no audio channel and
   noVNC has no audio support. So even with a sound card, the current console
   would remain silent. Audio needs a different transport, and the transport
   choice also decides whether the *video* latency is good enough for work.

The user's constraint that shapes every decision below: **video and audio
latency matter** for the intended work, and the host already streams to
Moonlight (Sunshine on carnyx) at a quality the user is happy with — so the
obvious question is whether that existing Sunshine can serve each sandbox.

**It cannot** (D1). Sunshine captures a DRM/KMS connector on carnyx
(`adapter_name = /dev/dri/renderD129`, `output_name = HDMI-A-3`) and the host's
audio sink. A sandbox's desktop lives in a QXL framebuffer inside
`qemu-system-x86_64` and its audio goes to a QEMU audio backend. Nothing about
the host instance can see either. This phase therefore puts Sunshine **inside
the golden** and builds the host-side plumbing to reach it.

---

## 1. Goals, constraints, non-goals

### Goals

- **G1** A `win11` sandbox has a working Windows audio endpoint, so audio-using
  applications run and can render.
- **G2** A human on a Moonlight-capable client can stream a sandbox's desktop
  **with audio**, at a latency measured and recorded — not asserted.
- **G3** Reaching a sandbox is a lifecycle-bound operation: allocation on `up`
  (or on demand), release on `down`/`reap`, no leaked ports or listeners, and
  no manual `socat` in the hot path.
- **G4** The browser console keeps working for what it is good at (inspect,
  computer-use agents, no client install), and its role is documented rather
  than overclaimed.
- **G5** If — and only if — the virtual display driver makes session-0 mode
  changes work, the resize work in `#56` lands as a gateway endpoint with no
  in-session launcher.

### Constraints

- **C1** carnyx is a daily-driver host and the 4080 SUPER is the user's gaming
  GPU. Any decision that removes it from the host needs explicit approval.
- **C2** `intel_iommu` is **not currently enabled** on carnyx (no
  `intel_iommu=on` in `/proc/cmdline`; `/sys/kernel/iommu_groups` is empty).
  Both passthrough options in D2 therefore require a kernel-cmdline change and a
  host reboot before they can even be tested.
- **C3** The zero-idle CI scale set (`lsbx-scaleset@linux.service`) shares this
  host. A streaming VM must not starve CI jobs, and CI must not silently make a
  streaming session unusable.
- **C4** Sandboxes are disposable and lease-bounded. Nothing may assume a stable
  guest IP, guest UUID, or long-lived process.
- **C5** No credentials, keys, or PINs in the repo. No new public exposure: the
  tailnet remains the outer boundary.
- **C6** Goldens are re-baked by flattening a running VM. Anything added to the
  golden must be idempotent in `scripts/provision-win11-golden.ps1`, not a
  hand-applied mutation (README "Working rules for desktop goldens").

### Non-goals

- Replacing the VNC console or `/stream`.
- Real-time audio monitoring (play *into* a DAW and hear yourself). No remote
  transport in this phase reaches monitoring-grade round-trip; see D8's cost
  note. Offline render, playback, auditioning, and mix-checking are in scope.
- Any tailcat or iroh work (`#52`) — parked by this phase.
- Linux desktop audio. The Linux half of resize stays blocked on `#58`.
- HDR, 10-bit, or multi-monitor.
- Gaming-grade streaming (that is what the host instance is for).

---

## 2. Current state of carnyx (measured 2026-11-21)

Everything in this section was measured on the host, not recalled. Re-measure
before acting on it if more than a few weeks have passed.

### 2.1 Hardware

| Item | Value | Consequence |
|---|---|---|
| CPU | Intel i7-13700K, 16 cores / 24 threads (8P+8E), 1 socket | 8-vCPU sandboxes leave room, but software video encode competes with CI |
| RAM | 188 GiB total, ~175 GiB available | 32 GiB desktop sandboxes are not RAM-bound |
| GPU 0 | Intel UHD 770 (Raptor Lake-S GT1) | Gen12.2 — **GVT-g is dead on Gen12+ and it has no SR-IOV**, so it is whole-device passthrough or nothing |
| GPU 1 | NVIDIA RTX 4080 SUPER (AD103) | NVENC available **to the host**; consumer Ada has no SR-IOV, so a VM can only have it via whole-device passthrough |
| IOMMU | **not enabled** (`/sys/kernel/iommu_groups` empty) | blocks both passthrough options until `intel_iommu=on iommu=pt` + reboot |
| DRM nodes | `card1`, `card2`, `renderD128`, `renderD129` | host Sunshine is pinned to `renderD129` |

### 2.2 Virtualisation stack

| Item | Value |
|---|---|
| libvirt | 12.9.0 (supports `<audio>`; allows one graphics device **of each type**, so VNC + SPICE can coexist) |
| QEMU | 11.1.3 |
| QEMU audio drivers | `none, alsa, dbus, jack, oss, pa, pipewire, sdl, spice, wav` |
| QEMU sound devices | `ich9-intel-hda`, `intel-hda` + `hda-output`/`hda-duplex`/`hda-micro`, `AC97`, `ES1370`, `sb16`, `usb-audio`, `virtio-sound-pci` |
| Host audio | PipeWire/Pulse reachable (`pactl info` → `/run/user/1000/pulse/native`) |
| Networks | `virbr0` `192.168.122.1/24` (guests), tailnet `100.125.210.60/32`, LAN `192.168.1.195/24` |

### 2.3 The existing host Sunshine (cannot be reused — see D1)

| Item | Value |
|---|---|
| Unit | `sunshine-after-login.service` — a **systemd --user** unit, deliberately started inside the graphical session |
| PID | 7491 (parent: `systemd --user` 1076) |
| Capture | `adapter_name = /dev/dri/renderD129`, `output_name = HDMI-A-3` → **KMS capture of one physical connector** |
| Listening | `0.0.0.0:47984` (HTTPS), `:47989` (HTTP), `:47990` (web UI), `:48010` (RTSP) |
| Config | `~/.config/sunshine/sunshine.conf` — `upnp = enabled`, `lan_encryption_mode = 0`, `stream_audio = enabled`, `min_threads = 4`, `ping_timeout = 30000` |
| Note | the *system* unit `sunshine.service` exists but is `disabled`/`inactive`; the live instance is the user unit. Port range 47984–48010 is therefore **already occupied on all interfaces** — per-sandbox slots must not use it. |

### 2.4 lsbx services on carnyx

| Unit | Scope | State | Role |
|---|---|---|---|
| `lsbx-serve.service` | system | active (since 2026-11-19 17:08, 1 restart) | HTTP gateway, `/console`, `/assets/novnc`, `/consoles/<id>/token`, `/stream/<id>/vnc` |
| `lsbx-scaleset@linux.service` | **user** (`Linger=yes`) | active (PID 294780, since 2026-11-19 20:40) | push-dispatch CI: scale set `lsbx-linux`, group `continuo`, labels `lsbx-linux,lsbx-default,lsbx-carnyx,lufs,self-hosted,Linux,X64`, min 0 / max 4, profile `ci`, lease 3 h |
| `lsbx-ci-broker.service` | system | installed, **inactive** | polling broker; per README only `lsbx-windows` would use it |

CI is confirmed live: last job `44662558321` (2026-11-20T02:41Z) ran as runner
`lsbx-linux-5d8e220b` and succeeded in 2m12s. Idle now (~3 REST + 74
actions-service calls/hour, zero idle runners by design).

### 2.5 Goldens and domains

| Item | Value |
|---|---|
| `win11-desktop` golden | `/home/carnyx/ISOs/images/goldens/win11-desktop.qcow2`, 97 GB, flattened 2026-11-13 |
| Manifest entry | `images.carnyx.json` → 8 vCPU / 32 GB / 256 GB, `streaming: novnc`, healthchecks `rfb-check.ps1` + `session-check.ps1` |
| Generated domain (desktop branch) | VNC graphics, QXL `ram/vram/vgamem = 65536`, **no `<sound>`, no `<audio>`** |
| In-guest bridge | TightVNC raw RFB on `:8000` (loopback-only VNC on `:5900`), password `lsbx`, autologon gives a logged-in session |
| Operator's hand-built `win11` domain | SPICE graphics + `<sound model='ich9'/>` + `<audio id='1' type='spice'/>` + QXL `vgamem=16384` — **the working audio path that was never carried into the golden template** |

### 2.6 What is already known about the display path

From `docs/specs/2026-11-14T0425Z_win11-resize-measurement/FINDINGS.md`:

- TightVNC 2.8.81 removed `ExtendedDesktopSize` but still reports framebuffer
  changes via the legacy `DesktopSize` (-223) rect; noVNC 1.5.0 handles it.
- A display-mode change **cannot** be issued from SSH (session 0):
  `ChangeDisplaySettingsEx` returns `DISP_CHANGE_FAILED`. A scheduled task with
  `LogonType Interactive` reaches session 1 but gets `DISP_CHANGE_BADPARAM`
  (right session, wrong window station). Startup-folder / `Run`-key agents never
  executed at all.
- Autologon (step 0) is now working, so a logged-in desktop exists.

---

## 3. Ecosystem references

These are outside this repo. They are the reason several units are shaped the way
they are, so they are recorded once here rather than repeated in unit contracts.

### 3.1 Sunshine / Moonlight

- **Sunshine** (`LizardByte/Sunshine`, the host) speaks the GameStream protocol;
  **Moonlight** (`moonlight-stream/moonlight-qt` and mobile clients) is the
  client. Windows host binaries are official; Sunshine has its own web UI on
  `:47990` for pairing.
- **Capture on Windows** is one of `nvfbc` / `ddx` (Desktop Duplication) /
  `wgc` (Windows Graphics Capture) / `kms`. DXGI/WGC work against any WDDM
  adapter, so QXL is a valid capture source. Hardware encoders are NVENC / AMF /
  QSV; **software encoding is supported** and is the fallback when the guest has
  no GPU.
- **Audio** is captured as WASAPI loopback from the guest's default render
  endpoint and sent as Opus. The endpoint must therefore *exist* — which is
  exactly what the golden lacks (G1).
- **Client discovery**: Moonlight auto-discovers via mDNS `_nvstream._tcp.local.`
  (link-local only) or accepts a manual `Add PC` of the form `host:port`.
  Internally Moonlight tracks hosts by the **UUID the host reports**, not by
  address, and remembers several addresses per host.
- **Consequence that shapes D5:** Sunshine generates its UUID on first run and
  persists it. A golden flattened with Sunshine installed carries that UUID into
  every clone, so every sandbox from one golden looks like *the same Moonlight
  host*.

### 3.2 Virtual Display Driver (VDD)

- `Virtual-Display-Driver` (IddSampleDriver-based) creates a virtual monitor with
  arbitrary resolution and refresh, which is the standard way to give Sunshine a
  headless display with the client's exact geometry.
- `RichardDorian/SunshineVirtualDisplay` is the reference pattern: Sunshine runs
  scripts **before a session starts** (from its service context) to set the
  virtual display's mode/refresh/HDR to match the client, and reverts on
  disconnect.
- **Why this matters here:** Sunshine's service runs in **session 0**, and that
  pattern demonstrably changes a display mode from session 0. That is precisely
  what `ChangeDisplaySettingsEx` refuses on QXL from session 0. If it holds for a
  VDD-backed monitor, the `#56` step-1 blocker (D7) dissolves. **Unverified for
  this stack — that is what unit 05 exists to measure.**

### 3.3 Browser clients for Sunshine-class latency

There is no official Moonlight browser client. Community WebRTC bridges that pair
with Sunshine and play in a tab:

| Project | Shape |
|---|---|
| `MrCreativ3001/moonlight-web-stream` | web server relays Sunshine traffic to the browser over WebRTC + WebCodecs |
| `moonlightweb.top` (MoonlightWeb) | embeds `moonlight-common-c`, speaks GameStream, relays video/audio to a browser |
| `linckosz/moonlight-web` | own capture/encode engine, pairs with Sunshine |
| LuminalShine | built-in `/webrtc` endpoint |

All are early, third-party, and unsupported. They require HTTPS and WebCodecs
(`lsbx.lufshq.com` already provides HTTPS). This is a spike, not a commitment
(D8).

### 3.4 The mature browser alternative

Apache Guacamole (`guacd` + HTML5 client) over **RDP** carries audio (PCM
redirection) and gives dynamic resize for free — both of this phase's goals in
one move — at roughly 100–300 ms. RDP on Windows 11 requires Pro/Enterprise and
the RDP service enabled. Recorded here as the boring fallback; it is *not* the
primary path because its latency is not competitive for the user's work.

### 3.5 Windows sound devices under QEMU

- `virtio-sound` has **no Windows driver** (virtio-win issue #929 open; Incus
  disabled `virtio-snd` for Windows in Nov 2025). Rejected.
- `ich9-intel-hda` + an `hda-*` codec is served by Windows' in-box HDAudio
  driver. This is the only realistic Windows sound device and is what the
  operator's hand-built `win11` domain already uses.

---

## 4. Design approach

```
                        carnyx (host)                          guest (win11 golden)
  ┌──────────────────────────────────────────────┐   ┌──────────────────────────────────┐
  │ Moonlight client (tailnet)                   │   │ Sunshine (service, session 0)    │
  │   Add PC: carnyx:PORT_slot ──┐               │   │   capture: DXGI/WGC on VDD|QXL   │
  │                              │               │   │   encode:  sw x264 (default)     │
  │ tailscale0 100.125.210.60    ▼               │   │            or QSV (D2 escalation)│
  │   slot relay PORT_slot ─────► 192.168.122.N  │◄──┤   audio: WASAPI loopback         │
  │   (per-slot block, lifecycle-bound, D4)      │   │            → Opus                │
  │                                              │   │                                  │
  │ lsbx-serve :8243                             │   │ Virtual Display Driver (D7)      │
  │   /console  ── VNC console (inspect only) ───┼──►│   TightVNC raw RFB :8000         │
  └──────────────────────────────────────────────┘   │                                  │
                                                     │ ich9 HDA + <audio type='none'>   │
                                                     │ autologon → interactive session  │
                                                     └──────────────────────────────────┘
```

Two doors, deliberately different jobs:

| Door | Carries | Latency | Client install | Use for |
|---|---|---|---|---|
| **Moonlight** (new) | video + **audio** | measured in unit 05 | Moonlight app | actual work |
| **VNC console** (existing) | video only | low | none (browser) | inspect, computer-use agents, "is it alive" |

Reachability is **host-side port-slot forwarding**, not a new transport. This is
the same primitive tailcat (`#52`) would have provided, minus the crypto, and it
is deliberately the smallest thing that works: the tailnet remains the outer
boundary, Moonlight's PIN pairing is the inner one.

The forwarding *mechanism* is its own decision (D4a) because of a constraint
discovered while writing this spec: `lsbx-serve.service` runs as the unprivileged
`carnyx` user and `sudo` on carnyx requires a password, so kernel NAT is not
available to any process this repo ships today.

---

## 5. Decisions (open questions resolved into cost/benefit)

Each decision records the recommendation, the cost of taking it, the cost of not
taking it, and the condition under which it should be revisited. Where a decision
is genuinely still open, the unit that resolves it is named.

### D1 — Sunshine runs **inside the golden**; the host instance is not reusable

**Decision:** install Sunshine in the `win11-desktop` golden and run it as an
in-guest service. Do not attempt to make the host's Sunshine instance serve
sandboxes.

**Why the host instance cannot:** it captures a KMS connector
(`/dev/dri/renderD129`, `HDMI-A-3`) and the host's PipeWire sink. A sandbox has
neither. The only reuse path is "keep a `virt-viewer` window per sandbox on
carnyx's screen, capture that window, and reroute the guest's audio into the
host sink" — three extra hops (SPICE decode → host compositor → capture →
encode), one host window and one SPICE decode per sandbox, host audio mixed into
the stream, and still no arbitrary resolution.

| | Cost | Benefit |
|---|---|---|
| **In-guest Sunshine** | one more thing to bake into the golden; guest CPU for encode; per-sandbox port plumbing | correct capture and audio source; independent of the host session; dies with the VM; standard pattern |
| **Host Sunshine + virt-viewer window** | ~0 new install; but per-sandbox host window, mixed audio, +2 latency hops, no clean teardown | nothing that survives contact with a second sandbox |

**Revisit if:** a supported host-side API appears for capturing a QEMU
framebuffer without passthrough (there isn't one today; Looking Glass needs
passthrough).

### D2 — Encoder: software x264 first, **measured**; iGPU passthrough as escalation; 4080 passthrough rejected by default

**Decision:** ship software encoding first and measure it (unit 05). Escalate to
whole-iGPU passthrough only if the measurement says software is unusable.

| Tier | Encode latency | Quality at sane bitrate | Host cost | Guest cost |
|---|---|---|---|---|
| **A. Software x264/x265** (default) | ~10–20 ms encode; ~40–80 ms glass-to-glass | mediocre at 1080p60, 15–25 Mbit | ~0 | 2–4 of 8 vCPU |
| **B. Whole-iGPU passthrough** (UHD 770 → one VM) | ~5–10 ms encode; ~20–40 ms glass-to-glass | good (QSV) | host loses the iGPU (acceptable if the monitor is on the 4080) | none |
| **C. 4080 passthrough** | ~5–10 ms encode; ~15–30 ms glass-to-glass | host-equivalent | **host loses gaming and any other GPU VM** | none |

Costs that are easy to miss:

- **A** costs CPU that the zero-idle CI scale set also wants (C3). A 1080p60
  software stream is roughly 2–4 cores continuously; with `max_runners = 4` CI
  can already saturate the box. Needs a concurrency rule (D10).
- **B and C both require IOMMU**, which is **off today** (C2): a kernel-cmdline
  change plus a host reboot, and then a vfio binding that must be re-applied
  after every host kernel update. GVT-g is not an option on Gen12.2, so **B is
  all-or-nothing: one streaming VM at a time gets the iGPU.**
- **C** is the only tier that makes a sandbox feel like the host stream, and the
  only one that costs the user their gaming GPU.

**Revisit when:** unit 05's numbers land. Flip condition for B: software encode
either can't hold 30 fps at 1080p or produces visible artifacts at a bitrate the
tailnet can carry. Flip condition for C: the user explicitly accepts losing the
4080 on the host.

### D3 — Guest audio device: `ich9` HDA + `<audio type='none'>`

**Decision:** desktop-flavour domains get `<sound model='ich9'/>` and an
`<audio>` element. Default QEMU backend is `none`.

**Rationale:** Sunshine captures WASAPI **loopback** from the guest's render
endpoint, so the endpoint must exist but its host-side sink does not matter.
`type='none'` gives the guest a working endpoint and lets QEMU discard its own
copy — no host audio leak, no dependency on carnyx's PipeWire session (which is
a *user* session and would break a system-service VM anyway).

| | Cost | Benefit |
|---|---|---|
| `ich9` + `type='none'` | none; one XML element | endpoint exists; Sunshine audio works; no host sink coupling |
| `ich9` + `type='pa'/'pipewire'` | couples every VM to carnyx's user session; host hears every sandbox | useful only for "play the sandbox on the host speakers", which nobody asked for |
| `virtio-sound` | no Windows driver (3.5) | — |

**Revisit if:** a second consumer needs host-side audio (e.g. a recording CI
job), at which point `<audio>` becomes a per-golden field rather than a constant.

### D4 — Reachability: host port-slot forwarding ("Door 6"), not subnet route, not tailcat

**Decision:** allocate a contiguous block of host ports per sandbox slot and
forward them from `tailscale0` to the guest IP. Lifecycle-bound to
`up`/`down`/`reap`.

| Option | Cost | Benefit |
|---|---|---|
| **Port-slot forwarding** | ~1 unit of Rust + a lifecycle to get right | no new binary, no new protocol; tailnet ACL is the boundary; single-digit-ms overhead at worst |
| Tailnet subnet route (`2026-11-07T1440Z_libvirt-guest-net-tailscale-route`) | publishes all of `192.168.122.0/24` incl. operator VMs; ACL + route approval | every guest reachable natively |
| tailcat (`#52`) | new 18 MB Go binary, no stability promise, address-is-password | works for clients **not** on the tailnet |

**Revisit when:** a client outside the tailnet needs a sandbox (that is `#52`'s
actual use case — cloud agents), or when more than a handful of slots makes
per-slot forwarding awkward enough to want a real proxy.

### D4a — Forwarding mechanism: **in-gateway userspace relay** (recommended), not kernel NAT

This sub-decision exists because of a hard constraint: **nothing this repo ships
runs with `CAP_NET_ADMIN`.** `lsbx-serve.service` runs as `carnyx`, and `sudo` on
carnyx requires a password (`sudo -n true` → *"a sudo password is required"*).
Kernel DNAT therefore needs either a privileged daemon or a sudoers entry — both
of which are host configuration, not code in the repo.

| Mechanism | Cost | Benefit | Risk |
|---|---|---|---|
| **A. In-gateway userspace relay** (recommended) | a tokio TCP+UDP forwarder in `lsbx-serve`; one process owns every slot; UDP needs a per-source-address session table | **no root**; lifecycle is the store, not a rules table; a host reboot cannot leave stale rules; the same code path a future gateway client mode (`#57`) needs | one extra copy per packet; a gateway restart drops live streams; UDP relay is ~200 lines of state machine |
| **B. Root helper daemon** (`lsbx-netd`) doing iptables DNAT | a privileged daemon + IPC + rule persistence + reconcile | kernel fast path, zero copy, ~0 ms | new root service to secure and audit; stale rules after reboot/crash; more moving parts than the problem deserves |
| **C. sudoers NOPASSWD for a narrow iptables wrapper** | a one-line host config change outside the repo | kernel fast path, minimal code | grants a network-reconfiguration capability to the `carnyx` user; invisible to anyone reading the repo; the exact kind of host mutation AGENTS.md warns against |

**Recommendation:** A. Moonlight's media is UDP; a userspace relay adds one copy
and a NAT-like table, which on a 20–40 Mbit stream on this hardware is noise
compared to software x264 encoding (D2). It also keeps every privileged operation
out of the repo and makes "release the slot" a store write instead of an
`iptables -D` that can be forgotten.

**Flip condition:** unit 05 measures the relay's added latency and CPU; if the
relay costs more than ~2 ms glass-to-glass or shows packet-loss artefacts under
load, revisit B (not C).

**Consequence for the spec:** because forwarding lives in the gateway, a
`lsbx moonlight <id>` invocation is a *request to the gateway*, not a direct host
mutation. That makes the command work identically for a remote client later
(`#57`) and keeps one owner for "who is listening on the tailnet".

### D5 — Client identity: **per-slot pre-added Moonlight entries**, not a stable per-golden UUID

**Decision:** assign each concurrent streaming sandbox a **slot number**, map each
slot to a fixed host port block, and document pre-adding `carnyx:PORT_slot` once
per slot in Moonlight. Do **not** rely on the baked-in Sunshine UUID as a stable
identity.

| Approach | UX | Breaks when | Cost |
|---|---|---|---|
| **Port slots (chosen)** | add N entries once; slot N always works; no client edits afterwards | you exceed N concurrent desktop VMs | a slot allocator + teardown (unit 03) |
| Stable per-golden UUID | exactly one "win11" entry that follows the current sandbox | two sandboxes from the same golden run at once — they collide on one UUID | zero plumbing, but concurrency is silently broken |
| Per-VM entry (fresh UUID each boot) | a new entry per sandbox | you edit Moonlight's host list every single time | a UUID-rotation step in provisioning + client churn |

**Note for unit 02:** whether Sunshine's UUID is stable across clones must be
*verified*, not assumed (read the persisted state file in a booted golden). If it
is stable, the collision risk above is real and slots are the answer. If it is
randomised per boot, then every sandbox is a new Moonlight entry and slots become
a *convenience* rather than a necessity — the decision stands either way, but the
documentation differs.

**Revisit if:** Moonlight gains a dynamic host-list API (it has none today; the
host list is local client state).

### D6 — The VNC console stays, with a narrowed role

**Decision:** keep `/console` + `/stream/<id>/vnc` as the zero-install door for
inspection and computer-use agents. Document that it carries **no audio** and is
not the work door.

**Known risk (must be measured in unit 06):** VNC mirrors the **QXL** framebuffer.
If a Virtual Display Driver becomes the primary display, the QXL head may go
unused and the browser console may show black. That would force an explicit
choice per golden: VNC-as-inspect-only (QXL kept as a second head) vs
Moonlight-only for `win11-desktop`.

| | Cost | Benefit |
|---|---|---|
| Keep both | one coexistence quirk to measure and document | agents and "is it alive" keep a browser door; humans get audio via Moonlight |
| Drop VNC for win11 | loses the zero-install door and `#53`-adjacent tooling | simpler display stack |

### D7 — Resize: **VDD-first**, `CreateProcessAsUser` as fallback (revises `#56` step 1)

**Decision:** unit 06 measures whether a VDD-backed mode change works from
session 0 (the Sunshine pre-session-script pattern, 3.2). If it does, `#56` step 1
is solved without any in-session launcher, and only the gateway endpoint (unit 07)
remains. If it does not, fall back to the documented
`WTSQueryUserToken` + `DuplicateTokenEx` + `CreateProcessAsUser`
(`lpDesktop="winsta0\\default"`) launcher.

| Path | Cost | Benefit | Risk |
|---|---|---|---|
| **VDD mode change from session 0** | already installing the VDD for Sunshine (unit 02) | no launcher, no scheduled tasks, arbitrary resolution + refresh, and Sunshine's ecosystem has done this before | unverified on this stack; may need the VDD's own control channel |
| `CreateProcessAsUser` launcher | a small native helper + a file/pipe protocol + supervision | known-good Windows API path | more moving parts in the golden; still limited to modes QXL can represent |
| Keep `resize=scale` only | zero | nothing | the original `#51` complaint stands |

**This does not unblock the Linux half** (`#58` still has to import the recipe).

### D8 — Browser + low latency is a **timeboxed spike**, not a commitment

**Decision:** unit 08 evaluates one WebRTC bridge behind the gateway. If it
doesn't reach "usable in a tab within one day", stop and document Guacamole/RDP
as the browser fallback.

| Path | Latency | Cost | Maturity |
|---|---|---|---|
| Moonlight native | best | already being built | mature |
| WebRTC bridge (bridge + browser) | ~+20–60 ms over native | a sidecar service, HTTPS/WSS, ICE, WebCodecs; third-party code in the trust path | early, unsupported |
| Guacamole + RDP | 100–300 ms | a sidecar + RDP enabled in the golden; also gives resize for free | mature |
| VNC console | low video, no audio | zero | in-repo |

**Honest cost note on "low latency" in general:** none of these reaches
monitoring-grade round-trip (~10 ms). If the intended work is playing into a DAW
and hearing it, the answer is a local session, not any remote transport. If it is
playback, auditioning, mix-checking, and offline render, all four rows above are
adequate and the ranking is purely about feel.

### D9 — Security posture: tailnet ACL outside, Moonlight PIN inside

**Decision:** slots are forwarded from the tailnet interface only, never from the
LAN or WAN. Pairing is Sunshine's PIN flow. No PINs, credentials, or addresses of
private services in the repo. The host Sunshine's `lan_encryption_mode = 0` is a
*host* setting and must not be copied into the golden.

| | Cost | Benefit |
|---|---|---|
| Tailnet-only forwarding + PIN | one client-side pairing per slot | no new public surface; no secret in a URL; consistent with D4's reasoning in the tailcat spec |
| Public forwarding / no PIN | none | unacceptable: a stream with audio of a logged-in Windows desktop |

**Revisit if:** a non-tailnet client needs access — then the tailcat spec's
address-as-capability model becomes relevant again (and it, too, treats the
address as a secret).

### D10 — Capacity and CI coexistence

**Decision:** cap concurrent streaming sandboxes and make the cap explicit in the
slot allocator. Working assumption: **2 concurrent software-encoded 1080p60
streams** on carnyx alongside CI, to be validated in unit 05.

| Policy | Cost | Benefit |
|---|---|---|
| Cap at 2, refuse a third with a clear error | a third sandbox can't stream | predictable latency for the ones that do; CI keeps its cores |
| No cap | CI jobs slow down or the stream stutters; whoever wins is whoever started first | none |

**Revisit when:** unit 05 gives real per-stream CPU numbers, or D2 escalates to
iGPU passthrough (which moves encode off the CPU entirely and changes the math).

### D11 — Slots are allocated at `up` time or on demand, and are always released

**Decision:** allocation is **on demand** (`lsbx moonlight <id>`), not automatic
at `up`, so CI and agent sandboxes never pay for a slot. Release is bound to
`down`, to the TTL reaper, and to a reconcile pass that drops rules whose guest no
longer exists.

| | Cost | Benefit |
|---|---|---|
| On demand | one extra command in the happy path | no slot consumed by non-desktop sandboxes; no port leak from VMs nobody streams |
| At `up` | zero commands | every sandbox holds a port block whether or not anyone streams it; caps out fast |

### D12 — `#52` (tailcat/iroh) stays parked

**Decision:** no tailcat or iroh work in this phase. D4 provides the same
reachability for the tailnet case, which is the only case this phase has.

**Why:** tailcat solves "client is not on our network and cannot reach an inbound
port". The user's client is on the tailnet. Building a transport before the
display/audio stack works would be optimising the part that isn't broken. Iroh's
browser story is relay-only (browsers can't send UDP), so it wouldn't help the
browser door either.

**Revisit when:** a cloud agent or an off-tailnet human needs a sandbox — and
note that D4's port forwarding is deliberately the thing tailcat would later
replace without touching the guest.

---

## 6. Usage model and process management

This section is the operator contract. It is what unit 03 and unit 04 implement
and unit 09 documents; it is written down here so the units have something to
agree with.

### 6.1 Happy path (a human wants a desktop with audio)

```
lsbx up win11                      # sandbox boots, autologon gives a desktop
lsbx moonlight <id>                # allocates slot N, binds its listeners, prints
                                   #   → "slot 2: carnyx:50200  (pair once in Moonlight)"
# Moonlight: Add PC → carnyx:50200 → enter PIN shown by Sunshine's web UI
#           (http://carnyx:<slot web-ui port>, tailnet only) → stream
lsbx moonlight <id>                # idempotent: same slot, address re-printed
lsbx moonlight --list              # what is allocated and where
lsbx down <id>                     # slot released, listeners closed
```

### 6.2 Port-slot model

- **Block size:** 100 contiguous ports per slot (Sunshine derives its own offsets
  inside the block; the allocator owns the *block*, never individual ports).
- **Base:** `50000 + slot * 100`, i.e. slot 0 → 50000–50099, slot 1 → 50100–50199,
  … Slots deliberately start above the host Sunshine range (47984–48010) and below
  the ephemeral range, so a per-sandbox block never collides with either the host
  stream or an outgoing connection.
- **Owner:** the gateway process (`lsbx serve`) holds the listeners (D4a). Slots
  are therefore process state plus store state, and a slot cannot outlive the
  process that owns it.
- **Slot count:** `LSBX_MOONLIGHT_SLOTS` (default 2, per D10).
- **Recorded on the record:** `SandboxRecord.extra` gets
  `{"moonlight": {"slot": N, "base_port": P, "guest_port": Q, "paired": false}}`.
  No schema change — `extra` is already a free-form map.
- **Idempotent:** running `lsbx moonlight <id>` twice returns the same slot, not a
  second one.

### 6.3 State machine

| State | Meaning | Who moves it |
|---|---|---|
| (no slot) | sandbox running, no streaming | `lsbx up` leaves it here |
| `allocated` | slot assigned, listeners bound, Sunshine reachable | `lsbx moonlight <id>` |
| `paired` | a Moonlight client completed PIN pairing | Sunshine (read-only observation) |
| `released` | rules removed, slot free | `lsbx down`, `lsbx moonlight --release`, TTL reaper, reconcile |

### 6.4 Failure modes the implementation must handle

| Failure | Required behaviour |
|---|---|
| Guest gone but slot held | reconcile frees the slot and closes its listeners; `lsbx moonlight list` shows it as stale before that |
| Sunshine not yet listening (guest booting) | `lsbx moonlight` waits with a bounded timeout and reports "Sunshine not ready" rather than handing out a dead address |
| Gateway restarts mid-stream | the stream drops; the slot stays allocated in the store and listeners are re-bound on start; the client reconnects. Must not be reported as a slot leak. |
| Host reboot | listeners are process state, so nothing to clean up; `lsbx moonlight list` reports the slot as allocated-but-unbound until the gateway starts and re-binds it |
| Two sandboxes claim one slot | allocation is exclusive; second gets the next free slot or a clear "no free slot" error |
| Slot exhaustion | explicit error naming the slots in use, not a hang |
| `down` fails mid-teardown | record `cleanup_failed` as the existing code does; reconcile retries |

### 6.5 Interaction with existing lifecycle

- Leases are unchanged: `LSBX_SS_LEASE=3h` for CI, and a desktop sandbox's lease
  is whatever `lsbx up` set. A streaming session does **not** extend a lease;
  `lsbx moonlight` on a nearly-expired sandbox should warn.
- The TTL reaper must free slots. A slot freed late is recoverable; a slot leaked
  forever is not.
- CI (`lsbx-scaleset`) is untouched by this phase. No changes to
  `sidecars/lsbx-scaleset`, `LSBX_SS_*`, or the runner labels.

### 6.6 Observability

- `lsbx moonlight list` — sandbox id, slot, base port, guest IP, state, age.
- Log lines must name the slot and sandbox id, and must not print PINs.
- Unit 05's measurement output is the latency record of truth; unit 09 puts the
  headline numbers in the README.

---

## 7. Unit decomposition

| # | Unit | Depends on | Delivers |
|---|---|---|---|
| 01 | `01-guest-audio-device` | — | `ich9` + `<audio type='none'>` on desktop-flavour domains, test-pinned |
| 02 | `02-golden-sunshine-and-vdd` | 01 | Sunshine + VDD baked into `win11-desktop`; healthcheck; UUID behaviour measured |
| 03 | `03-moonlight-slot-allocation` | 02 | slot allocator, `lsbx moonlight` subcommand, store + reap/reconcile integration |
| 04 | `04-gateway-slot-relay` | 03 | the TCP+UDP forwarder in `lsbx serve` that makes a slot reachable (D4a) |
| 05 | `05-latency-and-encoder-measurement` | 04 | measured glass-to-glass + audio latency; the D2/D4a/D10 flip decisions |
| 06 | `06-vdd-session0-resize-and-qxl-coexistence` | 02 | the D7 measurement + the D6 risk measurement |
| 07 | `07-gateway-display-resize-endpoint` | 06 | `POST /sandboxes/{id}/display` + console debounce (gated on 06's result) |
| 08 | `08-browser-webrtc-bridge-spike` | 04, 05 | timeboxed browser evaluation (D8) |
| 09 | `09-docs-and-runbook` | 04, 05 | README + AGENTS runbook, latency numbers, door table |

Order: 01 → 02 → 03 → 04 → 05; 06 can run in parallel with 03–05 (it only needs
02); 07 after 06; 08 after 05; 09 last.

---

## 8. Boundaries

**In scope:** the files named in each unit's `Touches` list; `win11-desktop` only.

**Out of scope:** the Linux desktop golden (`#58`), tailcat/iroh (`#52`), the CI
scale set and broker, `/stream` protocol changes, `SandboxRecord` schema changes,
any golden other than `win11-desktop`, GPU passthrough implementation (D2's tier B
and C are *decisions*, not units — a passthrough change is its own phase if unit
04 justifies it).

---

## 9. Done criteria for the phase

- [ ] A `win11` sandbox from a re-baked golden has a Windows render endpoint that
      an application can play into (unit 01 + 02 verified in-guest).
- [ ] `lsbx moonlight <id>` produces a working Moonlight stream **with audio** to
      a client on the tailnet, with no manual `socat`, no manual `iptables`, and
      no root.
- [ ] `lsbx down <id>` frees the slot and stops its listeners; a leaked slot is
      recoverable via reconcile.
- [ ] Glass-to-glass video latency and audio latency are recorded numbers in the
      repo, not adjectives.
- [ ] The browser console still works and its role (no audio) is documented.
- [ ] No process this repo ships requires root: slot forwarding works from the
      unprivileged `carnyx` user (D4a).
- [ ] D7 is answered with a measurement: either the VDD path works from session 0
      and `#56` reduces to a gateway endpoint, or the `CreateProcessAsUser`
      fallback is written down as the plan.
- [ ] `cargo check --workspace`, `cargo clippy --workspace --all-targets
      --all-features -- -D warnings`, `cargo test --workspace` pass.

---

## 10. Evidence log (recon, 2026-11-21)

Commands and their results, so the next session does not re-derive them.

```
# CI liveness
systemctl --user status lsbx-scaleset@linux   → active, PID 294780
gh api /repos/lufs-audio/lsbx/actions/runs/44662558321/jobs
  → runner lsbx-linux-5d8e220b, group continuo, labels self-hosted,lsbx-default, success

# Host Sunshine
systemctl --user list-units | grep -i sunshine → sunshine-after-login.service active
ss -tulpn | grep -E '4798|4799|4800|4801'
  → 47990, 47989, 47984, 48010 all on 0.0.0.0 (host Sunshine owns this range)
cat ~/.config/sunshine/sunshine.conf
  → adapter_name=/dev/dri/renderD129, output_name=HDMI-A-3, stream_audio=enabled

# Passthrough prerequisites
grep -o 'intel_iommu=on' /proc/cmdline   → (empty)
ls /sys/kernel/iommu_groups | wc -l      → 0

# Virtualisation / audio capability
qemu-system-x86_64 --version             → 11.1.3
qemu-system-x86_64 -audiodev help        → none alsa dbus jack oss pa pipewire sdl spice wav
qemu-system-x86_64 -device help          → ich9-intel-hda, hda-output, virtio-sound-pci, ...
virsh --version                          → 12.9.0

# The gap, in one command
grep -i 'sound\|audio' crates/lsbx-backend-libvirt/src/domain_xml.rs  → no matches
virsh dumpxml win11 | grep -i 'sound\|audio'
  → <sound model='ich9'/>  <audio id='1' type='spice'/>   (operator's hand-built domain)

# Goldens
ls -la /home/carnyx/ISOs/images/goldens/win11-desktop.qcow2 → 97 GB, 2026-11-13
```

### Unverified, and deliberately left as open questions

| Question | Why it matters | Resolved by |
|---|---|---|
| Is Sunshine's UUID stable across golden clones? | decides D5's collision risk and the docs | unit 02 |
| Does a VDD mode change work from session 0 on this stack? | decides D7 (and possibly closes `#56` step 1) | unit 06 |
| Do QXL and a VDD coexist without blacking out the VNC console? | decides D6 | unit 06 |
| What is software-x264 glass-to-glass latency at 1080p60 here? | decides D2 tier and D10's cap | unit 05 |
| Can software encode hold 30 fps with 2 concurrent streams and CI running? | decides D10 | unit 05 |
| Does any WebRTC bridge work behind the gateway within a day? | decides D8 | unit 08 |
