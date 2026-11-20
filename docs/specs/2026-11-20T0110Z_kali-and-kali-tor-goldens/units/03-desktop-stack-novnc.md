# Unit 03 — Desktop stack: XFCE + `x11vnc` + `websockify` + UFW 8000

## Objective
Give both goldens a working noVNC desktop that the existing `lsbx` gateway relay can reach on
guest port 8000, matching the `agent-web` convention.

## Context
- `docs/specs/2026-09-17T2114Z_kali-tor-golden/ROADMAP.md` §1.1 — the planned `kali-desktop`:
  `xfce4` + `xfce4-terminal`, `x11vnc` bound to `127.0.0.1:5900`, Python `websockify` on
  `0.0.0.0:8000`, UFW allowing `8000/tcp` from `192.168.122.0/24`.
- `images.carnyx.json` → `agent-web` golden — the working pattern to copy: `flavor: desktop`,
  `streaming: novnc`, healthcheck `sudo systemctl start lsbx-desktop.service` then
  `curl -fsS --retry 10 --retry-delay 1 --retry-connrefused http://127.0.0.1:8000/vnc.html`.
- `crates/lsbx-backend-libvirt/src/domain_xml.rs` — renders a virtual QXL/VirtIO-GPU display;
  no GPU passthrough exists or is added here.
- Guest username is `lsbx` (v1 migrated it from `exedev`); the desktop session must run as that
  user, not root.

## Acceptance criteria
- [ ] `lsbx-desktop.service` starts and stays `active` on boot in both goldens.
- [ ] `curl -fsS http://127.0.0.1:8000/vnc.html` returns a 200 with the noVNC page.
- [ ] `lsbx console <id>` yields a working noVNC URL through the gateway relay.
- [ ] `x11vnc` listens on `127.0.0.1:5900` only — not on `192.168.122.x`.
- [ ] UFW allows `8000/tcp` from `192.168.122.0/24` only; default incoming policy stays denied.
- [ ] XFCE session runs as `lsbx`, and `xfce4-terminal` opens via the console.
- [ ] Sets `selenium`-style fixed resolution 1920x1080; dynamic resizing is explicitly out of scope.
- [ ] A golden with 8 GB reports no OOM in `dmesg` after 10 minutes of desktop use.

## Interface contract
```ini
# /etc/systemd/system/lsbx-desktop.service
[Unit]
Description=LSBX desktop (XFCE + x11vnc + websockify)
After=network.target

[Service]
Type=simple
User=lsbx
ExecStart=/usr/local/bin/lsbx-desktop-start
Restart=always

[Install]
WantedBy=multi-user.target
```
```bash
# /usr/local/bin/lsbx-desktop-start
#   starts Xvfb-less real XFCE session on :0 (Xorg dummy or Xvfb),
#   x11vnc -localhost -rfbport 5900 -forever -shared -display :0,
#   websockify --web=/usr/share/novnc 0.0.0.0:8000 127.0.0.1:5900
```

## Boundaries
- **Touches:** `scripts/build-kali-golden.sh` (desktop block).
- **Does NOT touch:** `crates/**`, browser packages (Unit 04), Tor rules (Unit 02), `images.carnyx.json`
  (Unit 06 registers).

## Verification
```bash
lsbx exec <id> -- systemctl is-active lsbx-desktop.service
lsbx exec <id> -- curl -fsS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8000/vnc.html
lsbx exec <id> -- ss -lntp | grep -E '5900|8000'
lsbx exec <id> -- sudo ufw status verbose
```
