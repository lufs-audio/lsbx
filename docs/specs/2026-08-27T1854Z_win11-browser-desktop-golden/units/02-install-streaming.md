# Unit 02 — Windows browser-streaming stack in the guest

## Objective
Inside the SSH-provisioned Windows golden (`win11-ssh.qcow2` from Unit 01), install and auto-start the two in-guest components `lsbx-stream` expects for a noVNC console: a **VNC server on `127.0.0.1:5900`** and a **websockify bridge on `0.0.0.0:8000` → `127.0.0.1:5900`** serving the noVNC web assets. Prove the guest answers a browser-connectable HTTP handshake on `:8000` **over SSH, with no console**.

## Context
`lsbx-stream/src/proxy.rs:97` hardcodes the browser front of every golden to guest port **8000** (`GUEST_VNC_PORT`), and the libvirt backend renders guest graphics as `<graphics type='vnc' port='-1' autoport='yes'/>` (`crates/lsbx-backend-libvirt/src/domain_xml.rs:132`). Windows has no distro-managed x11vnc/websockify pair like the Linux desktop golden's `scripts/build-desktop-golden.sh`; this unit supplies the Windows-native equivalent. Both components run as Windows services so they come up on boot without a user logging in, which is what makes disposable `lsbx up` clones browser-served with zero manual steps.

VNC binds **loopback only** (`127.0.0.1:5900`) to mirror the Linux golden's `-localhost` posture; only the websockify bridge exposes `:8000` (and even that is only reachable from carnyx's stream relay, since clones aren't on the public net on their own). TightVNC is chosen over UltraVNC/RealVNC for its unattended service install on Windows.

## Acceptance criteria
- [ ] Guest (a fresh COW clone of `win11-ssh.qcow2`) boots with **no console session**, over SSH only.
- [ ] From carnyx, over SSH: `powershell -c "Get-NetTCPConnection -LocalPort 8000,5900 -State Listen"` lists **both** listeners; `127.0.0.1:5900` and `0.0.0.0:8000`.
- [ ] `powershell -c "Invoke-WebRequest http://127.0.0.1:8000/vnc.html -UseBasicParsing"` → HTTP 200 with the noVNC HTML body; the same request from carnyx's reachable interface for this guest also returns 200 (websockify bound to `0.0.0.0`).
- [ ] A websocket handshake to `ws://<guest-ip>:8000/websockify` upgrades successfully from carnyx (e.g. `websocat` or python `websockets`), proving the WS relay `wss://molimo.exe.xyz:8247/…` → `:8000` will have a live peer.
- [ ] Both components are Windows services / scheduled tasks with **Startup = Automatic**, running as the `lsbx`-reachable user, with no interactive desktop requirement. Restart the guest once more and re-run criterion 2 without touching the console — still listening.
- [ ] No Rust or manifest files in `~/repos/lsbx` changed by this unit.

## Interface contract (what this unit's guest contains / serves, consumed by Unit 03 & sandboxes)

```
in-guest listeners:
  127.0.0.1:5900   TightVNC Server   (password-protected, loopback only)
  0.0.0.0:8000     Python websockify  (noVNC assets from scripts/build-desktop-golden.sh,
                                        proxying 8000 ↔ 5900)
autostart:            sc / schtasks, both Automatic
served noVNC page:   http://<guest>:8000/vnc.html  (the browser URL the stream proxy fronts)
consumed_by_unit_03:  Unit 03 leaves this guest up and `lsbx golden verify` proves the bridge
                       answers from a fresh clone's :8000 before flattening.
```

## Verification (how this unit is proven done)
```bash
# carnyx, over SSH, on a fresh clone of win11-ssh.qcow2:
ssh -i ~/.ssh/lsbx_guest_key lsbx@<IP> 'powershell -c "Get-NetTCPConnection -LocalPort 8000,5900 -State Listen | ft LocalAddress,LocalPort -a"'
ssh … 'powershell -c "(Invoke-WebRequest http://127.0.0.1:8000/vnc.html -UseBasicParsing).StatusCode"'
websocat -1 ws://<IP>:8000/websockify < /dev/null   # expect an upgrade response, not refusal
```
