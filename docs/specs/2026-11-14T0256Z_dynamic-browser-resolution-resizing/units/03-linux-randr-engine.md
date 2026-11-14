# Unit 03 — Linux X11 XRANDR Dynamic Resizing Engine

## Objective
Upgrade the Linux desktop golden (`agent-web`) streaming server from a static `Xvfb` screen to an XRANDR-capable X11 display server, enabling dynamic resolution negotiation and automatic XFCE window manager layout adaptation.

## Context
In `scripts/build-desktop-golden.sh`, the Linux desktop golden runs `Xvfb :0 -screen 0 1280x800x24` coupled with `x11vnc`. `Xvfb` establishes a fixed virtual framebuffer at launch that cannot be resized at runtime via X11 XRANDR extensions. When a client requests an alternate resolution, Xvfb ignores it and forces client-side scaling.

TigerVNC's `Xvnc` (or an Xorg server with the `xf86-video-dummy` driver) provides full native XRANDR 1.2+ support. When a client sends a `SetDesktopSize` RFB message, `Xvnc` invokes the XRANDR interface to alter screen dimensions on the fly. The XFCE panel and desktop manager (`xfdesktop` / `xfwm4`) automatically listen for `RRScreenChangeNotify` events and reposition panels and window layouts seamlessly.

## Acceptance criteria
- [ ] `scripts/build-desktop-golden.sh` configures an XRANDR-capable virtual X11 server supporting dynamic resolution adjustments.
- [ ] Executing `xrandr -s <width>x<height>` inside the running guest immediately changes the X11 screen dimensions without restarting the X server or XFCE session.
- [ ] Incoming RFB `SetDesktopSize` messages from connected noVNC clients automatically resize the X11 screen to the client's dimensions.
- [ ] XFCE taskbars and desktop wallpapers dynamically adapt to the new aspect ratio without letterbox clipping.
- [ ] Golden verification healthchecks (`systemctl is-active lsbx-desktop.service` and `curl -fsS http://127.0.0.1:8000/vnc.html`) pass cleanly.

## Interface contract
```bash
# In-guest XRANDR query and resize contract:
DISPLAY=:0 xrandr --output default --mode 1920x1080
# Or dynamically added modeline:
# DISPLAY=:0 xrandr --newmode "1920x1080_60.00" ... && xrandr --addmode default "1920x1080_60.00"
```

## Boundaries
- **Touches:**
  - `scripts/build-desktop-golden.sh`
- **Does NOT touch:**
  - Windows golden configuration (Unit 02 owns Windows)
  - `crates/lsbx-backend-libvirt`

## Verification
```bash
# In an agent-web sandbox:
SBX=$(lsbx up desktop --lease 15m --json | jq -r .id)

# Query XRANDR capabilities inside guest:
lsbx exec "$SBX" -- sh -c "DISPLAY=:0 xrandr"

# Set a new resolution and confirm screen size changed:
lsbx exec "$SBX" -- sh -c "DISPLAY=:0 xrandr -s 1600x900"
lsbx exec "$SBX" -- sh -c "DISPLAY=:0 xwininfo -root | grep 'geometry'"
# Expect: "1600x900"

lsbx down "$SBX"
```
