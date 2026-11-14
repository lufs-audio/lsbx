# Unit 04 — Import the Linux desktop golden recipe (prerequisite for XRANDR resize)

## Objective
Bring the recipe that builds the `agent-web` (Linux desktop) golden into this repo so
the Linux half of the resize work has a file to change.

## Context
The resize phase
(`docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/units/03-linux-randr-engine.md`)
says to edit `scripts/build-desktop-golden.sh`. **That file does not exist in
lsbx.** `ls scripts/` contains only `lsbx-commit-golden.sh`,
`provision-ci-runner.{ps1,sh}` and `provision-win11-golden.ps1`.

The only copy is the read-only legacy reference at
`/home/carnyx/repos/lufs-sandbox-server/scripts/build-desktop-golden.sh`, which
installs `xfce4 xfce4-terminal x11vnc novnc websockify python3-venv dbus-x11 xvfb
x11-xserver-utils openssh-server` and runs:

```
Xvfb :0 -screen 0 1280x800x24 -nolisten tcp &
dbus-run-session -- startxfce4 &
x11vnc -display :0 -rfbauth /etc/lsbx-vnc.pass -localhost -listen 127.0.0.1 \
       -shared -forever -o /var/log/lsbx-x11vnc.log &
exec websockify --web $NOVNC_DIR 8000 localhost:5900
```

That is a **static-resolution** stack: `Xvfb` creates a fixed framebuffer and does
not implement XRANDR mode changes, so no client-driven or host-driven resize can
work on it. The registered `agent-web` golden (`base: lsbx-web-v1`) exists, but its
build recipe is not reproducible from this repo — the same class of gap #36 closed
for Windows.

## Acceptance criteria
- [ ] `scripts/provision-linux-desktop-golden.sh` exists in this repo, derived from
      the legacy recipe, with its provenance recorded (source path + commit/date of
      the reference it was derived from).
- [ ] The script is idempotent and parameterized (`DISTRO`, `NOVNC_DIR`,
      `STREAM_PORT`, `VNC_PASSFILE`) with the same defaults the legacy script used.
- [ ] Running it inside a fresh clone of the current base image reproduces a golden
      whose healthchecks pass: `systemctl is-active --quiet lsbx-desktop.service`
      and `curl -fsS http://127.0.0.1:8000/vnc.html`.
- [ ] It refuses to run without root, and it never writes a plaintext VNC password
      (uses `x11vnc -storepasswd`).
- [ ] Documented in `README.md`'s goldens section next to the Windows recipe, noting
      that it supersedes the legacy copy and that the legacy repo stays read-only.
- [ ] No behaviour change to the currently registered `agent-web` golden (this unit
      adds the recipe; swapping `Xvfb` → an XRANDR-capable server is the resize
      unit's job, not this one).

## Interface contract
```
scripts/provision-linux-desktop-golden.sh      # run as root inside the guest being baked
  env: DISTRO=ubuntu|debian|arch
       NOVNC_DIR=/usr/share/novnc
       STREAM_PORT=8000
       VNC_PASSFILE=/etc/lsbx-vnc.pass
  produces: systemd unit lsbx-desktop.service (enabled), listeners
            127.0.0.1:5900 (x11vnc) and 0.0.0.0:8000 (websockify)
```

## Boundaries
- **Touches:** `scripts/provision-linux-desktop-golden.sh` (new), `README.md`.
- **Does NOT touch:** `scripts/build-desktop-golden.sh` in
  `lufs-sandbox-server` (read-only reference; never edit it),
  `crates/lsbx-backend-libvirt/**`, `crates/lsbx-stream/**`, the Windows recipe.

## Verification
```bash
bash -n scripts/provision-linux-desktop-golden.sh          # syntax
shellcheck scripts/provision-linux-desktop-golden.sh || true

# build a golden from the script, then verify it
lsbx golden build agent-web --from <base> \
  --script scripts/provision-linux-desktop-golden.sh \
  --flavor desktop --cpu 2 --memory 4GB --streaming novnc --os linux
lsbx golden verify agent-web
```
