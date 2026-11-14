# Phase Spec — Console endgame recon: HTTPS door, browser console, and remote resize

**Timestamp:** `2026-11-14T0337Z`
**Slug:** `console-endgame-recon`
**Status:** **Recon + revised plan only.** No code changed in this turn. Three
related strands are assessed together because they share one bottleneck; splitting
them produced two specs that each assumed the other had solved it.
**Related:**
- `docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/` (the resize phase this revises — Unit 02's premise is wrong, see §2)
- `docs/specs/2026-11-14T0307Z_tailcat-transport/SPEC.md` (tailcat doors, #52)
- `docs/specs/2026-11-07T1440Z_canonical-url-lsbx-lufshq/SPEC.md` (the `lsbx.lufshq.com` plan this measures)
- Issues: **#51** (resize), **#52** (transport doors), **#53** (bridge wedge)

---

## 0. What was asked

Three questions, all "what's actually left before this is a solid product":

1. Work through the **dynamic resize** bug/spec.
2. Test the browser path through **`lsbx.lufshq.com`** (Caddy on calimbra, on the
   tailnet) so we get HTTPS — and find out what else that unlocks.
3. Test **running `lsbx` from a client** against carnyx as the backend, so the
   whole topology (client → service → hypervisor) is proven, not assumed.

This document reports what is actually true today and proposes the order.

---

## 1. Measured findings (2026-11-14, live)

### 1.1 HTTPS through `lsbx.lufshq.com` already works — better than the spec claimed

The `canonical-url` spec said its Caddy patch was "**PATCHED 2026-11-07, not yet
applied**". It is applied, or at least live:

| Probe | Result |
|---|---|
| `dig +short lsbx.lufshq.com @1.1.1.1` | `172.67.139.239`, `104.21.65.28` (Cloudflare proxied) |
| `GET https://lsbx.lufshq.com/health` (no auth) | **401** — reached carnyx's gateway and was refused, not a 52x |
| `GET https://lsbx.lufshq.com/health` + `Authorization: Bearer …` | **200** `{"backend_available":true,"backend_name":"libvirt","sandbox_count":1}` |
| `GET http://100.120.220.48:8246/health` + token (calimbra tailnet port listener) | **200** |
| `GET http://100.125.210.60:8243/health` + token (carnyx direct) | **200** |
| `GET /stream/<id>/vnc` with `Upgrade: websocket` **over HTTPS, `--http1.1`** | **HTTP/1.1 101 Switching Protocols** (`Server: cloudflare`, `via: 1.1 Caddy`) |

Latency for the same `/health`:

| Path | Time |
|---|---|
| carnyx `:8243` direct (tailnet) | **0.0006 s** |
| calimbra `:8246` (tailnet port listener) | **0.133 s** |
| `https://lsbx.lufshq.com` (Cloudflare → calimbra → carnyx) | **0.213 s** |

**Consequences, good and bad.**

- Good: TLS, a real hostname, and a WebSocket relay that upgrades end-to-end are
  already in place. Nothing needs building for "get HTTPS".
- Bad: that path adds ~200 ms of head latency before a single VNC byte moves, and
  Cloudflare's proxy sits in the interactive stream. For a desktop console that is
  the wrong shape — the tailnet path (or tailcat) should carry the pixels, and the
  public vhost should carry the *control* API.
- Gotcha worth writing down: **WebSocket over HTTP/2 returns 400** ("Connection
  header did not include 'upgrade'"). Browsers use HTTP/1.1 for WS so users are
  fine, but every CLI/probe must pass `--http1.1` or it will look broken.

### 1.2 The gateway's `/console` page is *still* the placeholder — because the service is stale

`GET https://lsbx.lufshq.com/console?target=<id>` returns the "noVNC console —
placeholder" page that `crates/lsbx-stream/assets/novnc-console.html` ships.

The code that replaced it (an authenticated-resolving page that embeds the
sandbox's console URL in a full-window iframe) landed in **`1b24c34`, 18:33 CST**.
`lsbx-serve.service` started at **17:26 CST** and the on-disk binary is from
**21:21 CST**. So the running gateway predates the fix by two commits:
**`sudo systemctl restart lsbx-serve.service` is required** before anything in
this document can be judged. (Not done in this recon turn — it needs sudo.)

### 1.3 …and restarting it still won't make the page work off-host

Even with the new code, `/console` renders an `<iframe>` of `console_url`, which
for the libvirt backend is `http://192.168.122.229:8000/vnc.html` — an address
that exists only on carnyx's `virbr0`. From Klaxon or any Cloudflare visitor that
iframe is a dead frame.

So the `/console` design has a structural hole, independent of staleness: **the
lsbx gateway does not own the noVNC assets.** `crates/lsbx-stream/assets/` contains
exactly one file, `novnc-console.html`, and its own doc comment admits the real
client was never vendored. The gateway can proxy *bytes* (`/stream/<id>/vnc` →
guest `:8000`) but it cannot serve the page that knows how to speak them.

The fix is small and bounded: the shipped bundle is **2.3 MB in 208 files**
(measured in the guest at `C:\lsbx-novnc\noVNC-1.5.0`) — vendorable into
`lsbx-stream/assets/novnc/` and served by the gateway, with the WS URL pointed at
`/stream/<id>/vnc` on the *same origin*. That removes the guest from the browser's
view entirely and makes the console work over HTTPS-with-token, tailnet, or
tailcat, unchanged.

### 1.4 `resize=remote` is available client-side, and impossible server-side (Windows)

- The shipped noVNC is **1.5.0** and does support it: `UI.rfb.resizeSession =
  UI.getSetting('resize') === 'remote'`, plus full `pseudoEncodingExtendedDesktopSize`
  handling in `core/rfb.js`. Unit 01 is genuinely just a config flip + debounce.
- The shipped VNC server is **TightVNC 2.8.81** (`tvnserver.exe` file version
  `2, 8, 81, 0`), and TightVNC's own 2.8.81 release note says:

  > "In this release, we had to **remove support for the ExtendedDesktopSize
  > pseudo-encoding** due to numerous incompatibilities… we've replaced
  > ExtendedDesktopSize with our own version… the new **DesktopConfiguration**
  > pseudo-encoding."

  noVNC speaks `ExtendedDesktopSize`; it does not speak TightVNC's proprietary
  replacement. **`resize=remote` therefore cannot work on the Windows golden with
  the current server, no matter what we configure.** Unit 02 as written is
  unimplementable and must be re-scoped (§2).

### 1.5 The QXL framebuffer is too small for large displays anyway

Live domain XML: `<model type='qxl' ram='65536' vram='65536' vgamem='16384' …>` →
**16 MiB of QXL video memory**.

| Resolution | Framebuffer (32bpp) | Fits in 16 MiB? |
|---|---|---|
| 1920×1080 | 8.3 MB | yes |
| 2560×1440 | 14.7 MB | barely |
| 3840×2160 | **33.2 MB** | **no** |

Any resize story that targets 4K needs `vgamem` raised (64 MiB is the usual
choice) regardless of which protocol carries the request.

### 1.6 There is no remote-client mode in the CLI (this is the real "client" gap)

`crates/lsbx-cli/src/lib.rs::connect_libvirt` **always** builds
`LibvirtTransport::Local { uri }`. `LibvirtTransport::RemoteSsh { … }` exists in
`lsbx-backend-libvirt` — with tests, `qemu+ssh://` URI construction, jump-host
support — and **nothing in the CLI ever constructs it**.

Worse, `qemu+ssh://` alone would not be enough even if wired, because the
backend's disk plumbing is *client-local*: `golden_disk::resolve`,
`image_ops::qemu_img_create_cow`, `std::fs::remove_file(&vm_disk_path)` and the
flatten path all run `qemu-img`/`std::fs` against paths like
`/home/carnyx/ISOs/images/...` **on whatever machine lsbx is running on**. Point a
laptop at carnyx with `qemu+ssh://` and every disk operation fails locally while
the domain operations succeed remotely.

So "run lsbx from a client against carnyx" has exactly one honest shape today:
`ssh carnyx lsbx …`. A real client mode is a design decision, not a fix:

| Option | What it means | Cost |
|---|---|---|
| **A. Gateway client mode** (`lsbx --gateway https://lsbx.lufshq.com --token …`) | CLI subcommands map onto the REST API that already exists and already answers over HTTPS | Medium; needs a REST client in the CLI + auth plumbing + output parity |
| **B. Remote libvirt + remote disk ops** | wire `RemoteSsh`, move every `qemu-img`/fs call behind a trait that can execute on the host | Large; touches the backend's core invariants |
| **C. Keep `ssh carnyx lsbx …`** | document it, delete the pretence of a client mode | Zero |

Option **A** is the one that compounds: it also gives agents and browsers the same
door, and it is the only option that makes the HTTPS work of §1.1 pay off.

### 1.7 The Linux desktop recipe has no home in this repo

Unit 03 (Linux XRANDR) says to edit `scripts/build-desktop-golden.sh`. That file
**does not exist in lsbx** — it lives in the read-only legacy reference at
`/home/carnyx/repos/lufs-sandbox-server/scripts/build-desktop-golden.sh` (which
uses `Xvfb :0 -screen 0 1280x800x24` + `x11vnc`, i.e. the static-resolution stack
Unit 03 wants to replace). The `agent-web` golden (`lsbx-web-v1`) exists, but its
recipe is not in this repo. Porting the recipe is a prerequisite for Unit 03.

---

## 2. Revised decisions for the resize phase

The resize spec's *goal* stands. Two of its units do not survive §1.4 and §1.5.

### R1 — Windows resize is **host-driven**, not RFB-negotiated (supersedes Unit 02)
Since TightVNC 2.8.81 removed `ExtendedDesktopSize`, do not try to make the RFB
server resize itself. Instead:

```
browser viewport change
  → console page POSTs {width,height} to the gateway (authenticated)
  → gateway → lsbx ops → guest `cmd /c set-resolution.ps1 -Width -Height`
  → Windows display driver applies the mode (ChangeDisplaySettingsExW)
  → TightVNC notices the framebuffer changed and re-sends it
  → noVNC reflows (client-side DesktopSize handling, no server negotiation needed)
```

- The Windows-side primitive already exists as a design in the resize spec's Unit
  02 (`set-resolution.ps1`); what changes is *who calls it*. It must be reachable
  from the gateway, so it rides the existing `exec` path (SSH into the guest) —
  no new in-guest daemon, no new protocol.
- Debounce lives in the page (200 ms trailing), not in the guest: never send more
  than one mode change per drag.
- Opportunistic upgrade: if a future VNC server implements the standard
  `ExtendedDesktopSize`, `resize=remote` starts working and we can drop the POST
  path per-golden. Do not build for that now.
- **Open risk to measure first:** does TightVNC 2.8.81 re-send the framebuffer when
  the guest resolution changes underneath it? It should (the framebuffer literally
  changed), but §1.4 proves vendor behaviour is not predictable here. This is a
  20-minute experiment: change resolution while a noVNC session is connected and
  watch the canvas. **Do this before writing any code.**

### R2 — Raise QXL `vgamem` for desktop goldens
`vgamem='16384'` (16 MiB) cannot hold 4K. Desktop goldens need ≥64 MiB
(`65536`) so 2560×1440 and 3840×2160 are reachable. Non-desktop goldens keep the
smaller value. (Windows driver honours QXL VRAM size at boot; changing it requires
a power cycle, i.e. it belongs in the same change as R3.)

### R3 — Serve noVNC from the gateway (new unit; this is the load-bearing one)
Vendor the 2.3 MB noVNC bundle into `crates/lsbx-stream/assets/novnc/`, serve it
from the gateway, and have the page point its WS at `/stream/<id>/vnc` on the same
origin. This is what makes the console work at all off-host — over tailnet, over
`lsbx.lufshq.com`, or over a tailcat address — and it closes the "browser cannot
set a WS header" auth gap at the same time (the page can carry a short-TTL signed
query token, `canonical-url` D7).

Ranking consequence: **R3 outranks the tailcat doors (#52) for the browser path.**
Tailcat's value is real but different — it is for *clients that are not on the
tailnet and cannot reach any inbound port* (cloud agents), not for browsers we can
already reach.

### R4 — Linux resize needs the server swapped, and the recipe imported
`Xvfb` cannot change modes at runtime; `x11vnc` has client-resize support that
requires an XRANDR-capable server (`Xvnc`/TigerVNC, or Xorg + dummy driver). So
Unit 03 becomes: (a) import the Linux desktop recipe into `scripts/` (§1.7), (b)
swap `Xvfb`→`Xvnc` (or take the same host-driven `xrandr` route as R1 for
symmetry), (c) verify XFCE reflows.

### R5 — Do nothing about "remote client" in the resize phase
It is a separate workstream (§1.6 option A). Do not let it block #51.

---

## 3. Recommended order (and why)

```
0.  restart lsbx-serve.service                        [sudo, 5 s]      §1.2
    └─ makes /console current; everything else is judged against this
1.  Measure: does TVN re-send the framebuffer after an in-guest mode change?
                                                      [~20 min]        §R1
2.  R3: vendor noVNC into the gateway, same-origin WS [1 unit]         §R3
    └─ unblocks browser behaviour on ALL transports, incl. HTTPS + tailcat
3.  R1+R2: set-resolution + vgamem + console resize POST + debounce    [#51]
4.  R4: Linux recipe import + XRANDR                                   [#51]
5.  #53: bridge hardening (supervisor, heartbeat, external healthcheck)
6.  #52: socat + tailcat doors  ← only after 2–3, and mainly for agents
7.  HTTPS end-to-end test through lsbx.lufshq.com     [recon script]   §4
8.  Client-mode decision (§1.6 option A) — its own spec
```

Rationale for pulling R3 ahead of the resize work: every resize design needs a
page that the browser can actually load, and today there isn't one. Building the
resize plumbing behind a placeholder page would mean verifying it through an
iframe that can't reach the guest — i.e. debugging two unknowns at once.

Rationale for demoting tailcat (#52) below the console work: tailcat cannot carry
a browser (no WebRTC yet; `tailcat#4`). Its wins are for non-tailnet agents.
Shipping the doors before the page exists would be building a bridge to nowhere.

---

## 4. Test plan for the HTTPS/tailnet paths (no code needed)

Run after step 0's restart:

```bash
TOK=$(grep -o 'LSBX_GATEWAY_TOKEN=.*' /home/carnyx/lsbx-state/serve.env | cut -d= -f2)
ID=$(lsbx up win11 --lease 1h --json | jq -r .id)

# 1. Control API over HTTPS (already proven; keep as a regression)
curl -s -o /dev/null -w '%{http_code}\n' https://lsbx.lufshq.com/health                      # 401
curl -s -H "Authorization: Bearer $TOK" https://lsbx.lufshq.com/health | jq .status          # "success"

# 2. WS relay over HTTPS — MUST use --http1.1 (h2 gives 400, see §1.1)
curl -s -i -N --http1.1 --max-time 5 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' -H 'Sec-WebSocket-Version: 13' \
  -H "Authorization: Bearer $TOK" \
  "https://lsbx.lufshq.com/stream/$ID/vnc" | head -1                                            # 101

# 3. Tailnet port listener (agents)
curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOK" http://100.120.220.48:8246/health

# 4. Browser page (after R3): assets from the gateway, WS same-origin
curl -s "https://lsbx.lufshq.com/console?target=$ID" | grep -c 'novnc'                          # ≥1
```

Browser-level (the part a human must judge):

- Load `https://lsbx.lufshq.com/console?target=$ID` in Chrome and in Safari.
- Confirm latency feels usable over Cloudflare; if not, that is the argument for
  carrying pixels over the tailnet/tailcat and leaving the vhost to the API.

---

## 5. Open questions

- **O1** Does TightVNC 2.8.81 re-send the framebuffer after an in-guest resolution
  change (R1's load-bearing assumption)? *Test before building.*
- **O2** Should the console page's resize POST be one request per drag-end (simple)
  or a small coalescing endpoint that also handles multi-monitor/`heads`? Start
  simple; QXL has `heads='1'`.
- **O3** Vendored noVNC: pin the version in-repo (2.3 MB, 208 files) and vendor the
  *upstream* bundle, or generate it? Vendoring with a recorded upstream version and
  checksum matches how `lsbx-stream/assets/` already works.
- **O4** Does the public vhost stay in the pixel path at all? §1.1's 213 ms says no
  for interactive use; keep `lsbx.lufshq.com` for control + console *page*, and let
  the WS ride the tailnet where possible.
- **O5** Client mode (§1.6 A) — is the CLI-as-gateway-client wanted, or is
  `ssh carnyx lsbx …` genuinely enough? This decides whether the HTTPS work has a
  second consumer.

---

## 6. Out of scope

- GPU passthrough / virtio-gpu venus (the other, larger reason a Windows desktop
  feels slow in a VM — see #54's notes).
- Replacing TightVNC with another Windows VNC server. If R1's measurement fails,
  that becomes the fallback plan (TigerVNC's WinVNC server does implement
  `ExtendedDesktopSize`), and it needs its own trade-off note.
- The tailnet subnet route (`docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md`).

---

## 7. Evidence log

```
# 1.1 HTTPS path
$ dig +short lsbx.lufshq.com @1.1.1.1
172.67.139.239
104.21.65.28
$ curl -s -o /dev/null -w '%{http_code} %{time_total}\n' https://lsbx.lufshq.com/health
401 0.399221
$ curl -s -H "Authorization: Bearer $TOK" https://lsbx.lufshq.com/health
{"data":{"backend_available":true,"backend_name":"libvirt","sandbox_count":1},"status":"success"}   # 200, 0.213 s
$ curl ... http://100.120.220.48:8246/health      # 200, 0.133 s   (calimbra tailnet listener)
$ curl ... http://100.125.210.60:8243/health      # 200, 0.0006 s  (carnyx direct)
$ curl -i -N --http1.1 ... https://lsbx.lufshq.com/stream/<id>/vnc
HTTP/1.1 101 Switching Protocols    Server: cloudflare    via: 1.1 Caddy
$ curl -i -N ... (without --http1.1, i.e. h2)
HTTP/2 400      content-length: 43   (WS needs HTTP/1.1 Upgrade)

# 1.2 stale service
$ systemctl show lsbx-serve.service -p ExecMainStartTimestamp
ExecMainStartTimestamp=Fri 2026-11-13 17:26:38 CST
$ git log -1 --format='%h %ad %s' --date=format:'%Y-%m-%dT%H:%M' -- crates/lsbx-stream/src/console.rs
1b24c34 2026-11-13T18:33 feat(stream): surface console_url … (fixes #37)
$ ls -l --time-style=+%H:%M target/release/lsbx
21:21                       # binary newer than the running process

# 1.3 assets
$ ls crates/lsbx-stream/assets/
novnc-console.html          # the only file
# measured in guest:
noVNC bundle: 2.3 MB in 208 files

# 1.4 client support vs server support
$ findstr ExtendedDesktopSize C:\lsbx-novnc\noVNC-1.5.0\core\rfb.js   # client: present
$ (Get-Item tvnserver.exe).VersionInfo.FileVersion                    # 2, 8, 81, 0
# vendor note (tightvnc.com/release-2.8.81.php, 2023-05-04):
#   "we had to remove support for the ExtendedDesktopSize pseudo-encoding …
#    replaced … with … the new DesktopConfiguration pseudo-encoding"

# 1.5 QXL
$ virsh dumpxml … | grep qxl
<model type='qxl' ram='65536' vram='65536' vgamem='16384' heads='1' primary='yes'/>

# 1.6 CLI has no remote mode
$ grep -n 'RemoteSsh{' crates/lsbx-cli/src/lib.rs          # no matches
$ sed -n '/async fn connect_libvirt/,/^}/p' crates/lsbx-cli/src/lib.rs | grep transport
    let transport = lsbx_backend_libvirt::transport::LibvirtTransport::Local { uri };

# 1.7 Linux recipe not in repo
$ ls scripts/
lsbx-commit-golden.sh  provision-ci-runner.ps1  provision-ci-runner.sh  provision-win11-golden.ps1
$ ls /home/carnyx/repos/lufs-sandbox-server/scripts/ | grep desktop
build-desktop-golden.sh     # read-only legacy reference
```
