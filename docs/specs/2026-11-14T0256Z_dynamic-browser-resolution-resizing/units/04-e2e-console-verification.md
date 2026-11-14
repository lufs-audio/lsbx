# Unit 04 — End-to-End Dynamic Console Resize Verification

## Objective
Verify the complete end-to-end dynamic resolution resizing lifecycle across both Windows 11 and Linux desktop sandboxes, driving the browser console from remote client hosts (such as macOS Klaxon) through `lsbx serve` and direct stream endpoints, closing Issue #51.

## Context
Units 01, 02, and 03 deliver client-side debounced remote resize signaling, Windows 11 dynamic display driver adaptation, and Linux XRANDR integration. This unit verifies the full integration live: opening desktop sandboxes in modern browsers (Safari, Chrome, Brave) on a remote client, actively resizing the browser viewport across standard (1920x1080), laptop (1440x900), split-screen (960x1080), and ultrawide viewports, and observing native resolution updates without distortion.

## Acceptance criteria
- [ ] Launching `lsbx up win11` and opening `http://100.125.210.60:8008/vnc.html` from a remote browser (Klaxon) displays the Windows 11 desktop.
- [ ] Resizing the browser window causes Windows to dynamically change its resolution within 500ms of drag completion to match the viewport dimensions exactly.
- [ ] Text in applications (e.g. Q-SYS Designer, REAPER, Notepad, Edge) remains sharp with 1:1 pixel rendering and zero blurred scaling artifacts.
- [ ] Launching `lsbx up desktop` and resizing the browser viewport dynamically resizes the XFCE desktop without letterboxing or clipped desktop panels.
- [ ] Rapid continuous window dragging does not crash or stall the in-guest VNC server or WebSocket stream proxy.
- [ ] Clean sandbox teardown (`lsbx down <id>`) leaves no residual processes or orphaned displays.

## Interface contract
```bash
# Verification suite across both desktop profiles:
lsbx golden verify win11-desktop
lsbx golden verify agent-web

# Live sandbox tests:
WIN_ID=$(lsbx up win11 --lease 20m --json | jq -r .id)
LINUX_ID=$(lsbx up desktop --lease 20m --json | jq -r .id)

# Browser console URLs:
lsbx console "$WIN_ID"
lsbx console "$LINUX_ID"
```

## Boundaries
- **Touches:**
  - `docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/`
  - Integration verification logs and evidence
- **Does NOT touch:**
  - Core storage or broker schemas

## Verification
```bash
# 1. Full workspace verification suite:
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

# 2. Golden verification gates:
lsbx golden verify win11-desktop
lsbx golden verify agent-web

# 3. Live smoke test:
SBX=$(lsbx up win11 --lease 10m --json | jq -r .id)
lsbx console "$SBX"
lsbx down "$SBX"
```
