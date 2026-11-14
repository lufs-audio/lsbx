# Unit 01 — noVNC Remote Resize Client Configuration & Viewport Synchronization

## Objective
Configure the client-side noVNC integration across the `lsbx-stream` console assets and golden bootstrap templates to enable dynamic `resize=remote` negotiation with client-side debouncing, replacing static `resize=scale` letterboxing.

## Context
In `lsbx`, the browser console is served either via `lsbx serve`'s `/console?target=<id>` route or directly by the in-guest websockify daemon (`http://<ip>:8000/vnc.html`). Currently, `C:\lsbx-novnc\noVNC-1.5.0\index.html` and `scripts/build-desktop-golden.sh` hardcode an immediate redirect to `/vnc.html?resize=scale`. This instructs noVNC to stretch/letterbox the canvas with CSS rather than negotiating resolution changes with the server.

noVNC 1.5.0 supports `resize=remote` (via `UI.recomputeScreenSize()` and `rfb.resizeSession = true`), which sends RFB `SetDesktopSize` messages using the `ExtendedDesktopSize` pseudo-encoding. This unit updates the client-side redirect and UI parameters to request remote desktop resizing with a 200ms debounce on window resize events.

## Acceptance criteria
- [ ] `index.html` entrypoints in `scripts/build-desktop-golden.sh` and `scripts/provision-win11-golden.ps1` redirect to `/vnc.html?resize=remote` instead of `resize=scale`.
- [ ] When opening `vnc.html?resize=remote`, noVNC initializes with `resizeSession = true` and transmits `SetDesktopSize` packets upon viewport dimensions change.
- [ ] Viewport resize events in the browser are debounced by at least 200ms to avoid overwhelming the VNC server or display driver during active window dragging.
- [ ] If the remote server signals lack of `ExtendedDesktopSize` support in its handshake, noVNC gracefully degrades to client-side scaling without disconnecting.
- [ ] `crates/lsbx-stream/src/console.rs` and `crates/lsbx-gateway/src/routes.rs` console embed templates pass through `resize=remote` parameters.

## Interface contract
```javascript
// URL parameter contract for noVNC client:
// /vnc.html?resize=remote&autoconnect=1

// Client-side debounce & resize configuration in noVNC:
rfb.resizeSession = true;
window.addEventListener('resize', debounce(() => {
    rfb.requestDesktopSize(window.innerWidth, window.innerHeight);
}, 200));
```

## Boundaries
- **Touches:**
  - `scripts/build-desktop-golden.sh`
  - `scripts/provision-win11-golden.ps1`
  - `crates/lsbx-stream/src/console.rs`
  - `crates/lsbx-gateway/src/routes.rs`
- **Does NOT touch:**
  - In-guest display drivers or OS kernels (Units 02 and 03 handle guest OS adapters)
  - `crates/lsbx-broker`

## Verification
```bash
# Verify noVNC asset templates contain resize=remote parameter
grep -n "resize=remote" scripts/build-desktop-golden.sh scripts/provision-win11-golden.ps1
cargo test -p lsbx-stream
cargo test -p lsbx-gateway
```
