# Phase Spec — Dynamic Guest Display Resolution Resizing in Browser Console

**Timestamp:** `2026-11-14T0256Z`  
**Slug:** `dynamic-browser-resolution-resizing`  
**Tracking Issue:** `lufs-audio/lsbx#51`  
**Branch:** `feature/dynamic-browser-resolution-resizing`  

---

## 0. Problem

Disposable desktop sandboxes in `lsbx` (both Windows 11 `win11-desktop` and Linux XFCE `agent-web`) currently operate with a static, hardcoded guest display resolution:
- Windows boots into a fixed display mode (typically 1024x768 or 1280x800).
- Linux boots with an `Xvfb` virtual screen constrained to 1280x800.

When an operator connects via the browser console (`http://<host>:8008/vnc.html` or through `lsbx serve`'s `/console?target=<id>`), noVNC is configured with `resize=scale`. This CSS-scales the static remote framebuffer to fit the browser viewport, causing:
1. **Blurry text and UI scaling distortion** on higher-resolution monitors (e.g. 1920x1080, 2560x1440, or Retina displays on macOS).
2. **Aggressive letterboxing / black borders** whenever the browser window aspect ratio differs from the guest's fixed ratio.
3. **Wasted screen real estate**: the guest OS window manager remains unaware of the browser viewport dimensions and cannot lay out application windows (such as Q-SYS Designer, Wwise, or REAPER) to take advantage of available screen space.

We need **true dynamic resolution resizing**: when the browser window is resized, the guest OS display adapter must dynamically adjust its native virtual resolution to match the client's viewport in real time.

---

## 1. Goals / Constraints

- **Bidirectional RFB negotiation & live OS adjustment**:
  - Browser noVNC client uses `resize=remote` (or active resize event dispatching) with debouncing.
  - The guest receives the requested resolution and applies it directly to the virtual graphics adapter without requiring a VM reboot.
- **Cross-platform guest support**:
  - **Windows 11 guests**: Use the virtual QXL / VirtIO-GPU display adapter via native Win32 `ChangeDisplaySettingsEx()` / `SetDisplayConfig` API or TigerVNC WinVNC `ExtendedDesktopSize` handling.
  - **Linux XFCE guests**: Use X11 RANDR-capable virtual server (e.g. TigerVNC `Xvnc` or `x11vnc` with RANDR hooks) so XFCE panel and desktop lay out seamlessly across arbitrary geometries.
- **No broken fallback**:
  - If a client or display adapter fails to switch resolution, graceful fallback to `resize=scale` must occur without disconnecting the WebSocket stream.
- **Performance & Stability**:
  - Viewport resize events must be debounced (minimum 150ms trailing window) to avoid flooding the guest display subsystem during active window dragging.
- **No regression on existing lsbx doors**:
  - CLI `lsbx up`, `lsbx console <id>`, and `lsbx serve` gateway routing remain compatible.

---

## 2. Ecosystem References

- **noVNC RFB implementation**: `novnc/core/rfb.js` supports `resizeSession: true` (parameter `resize=remote`), sending RFB `SetDesktopSize` client-to-server messages using the `ExtendedDesktopSize` (-308) pseudo-encoding.
- **QEMU / KVM display models**:
  - `domain_xml.rs` renders `<video><model type='qxl'/></video>` for Windows UEFI guests, and `<video><model type='cirrus'/></video>` or VirtIO-GPU for Linux.
  - The QXL / VirtIO-GPU Windows driver supports dynamic resolution mode switching via standard Windows Display Driver Model (WDDM).
- **Windows Display API**: `user32.dll` `ChangeDisplaySettingsExW` and `EnumDisplaySettingsW` with `DEVMODEW` structures.
- **Linux X11 display**: X11 Resize and Rotate extension (XRANDR 1.2+).

---

## 3. Design Approach

The work decomposes into four disjoint units:

```
[Unit 01: Client-Side noVNC Remote Resize]
         │ (sends SetDesktopSize / viewport dimensions)
         ▼
[Unit 02: Windows Display Engine]  ◄───►  [Unit 03: Linux RANDR Engine]
  (Win32 ChangeDisplaySettingsEx)          (X11 XRANDR / Xvnc dynamic sizing)
         │                                       │
         └───────────────────┬───────────────────┘
                             ▼
              [Unit 04: End-to-End Verification]
                (Live browser testing on Klaxon)
```

| Unit | Focus | Key Deliverable |
|---|---|---|
| **01-novnc-remote-resize-client** | Web Client / Gateway | Update noVNC configuration in `lsbx-stream` and golden assets to enable `resize=remote` with debounced viewport synchronization. |
| **02-windows-display-engine** | Windows Guest Stack | Implement dynamic resolution switching for Windows guests via Win32 display API daemon and/or TigerVNC `ExtendedDesktopSize`. |
| **03-linux-randr-engine** | Linux Guest Stack | Enable live dynamic RANDR resizing on `agent-web` (Linux desktop) so XFCE panels and desktop adapt to arbitrary viewports. |
| **04-e2e-console-verification** | Integration & Tests | Verify end-to-end dynamic resizing on live sandboxes from macOS (Klaxon) and automated integration tests. |

---

## 4. Boundaries

- **Touches:**
  - `crates/lsbx-stream/assets/` and `crates/lsbx-stream/src/` (noVNC client configuration and console templates)
  - `scripts/build-desktop-golden.sh` (Linux desktop golden streaming stack)
  - `scripts/provision-win11-golden.ps1` (Windows desktop golden streaming stack)
  - `crates/lsbx-backend-libvirt/src/domain_xml.rs` (video adapter parameters if VRAM/heads adjustments needed)
- **Does NOT touch:**
  - `crates/lsbx-broker` (CI job broker is unaffected)
  - `crates/lsbx-store` / `crates/lsbx-kernel` schemas (state schemas remain unchanged)
  - exe.dev backend (out-of-scope / deprecated)

---

## 5. Phase Acceptance Criteria

1. Opening a `win11` desktop sandbox in a browser and resizing the browser window causes the Windows desktop to change its native resolution to match the window dimensions (e.g. 1920x1080, 1440x900, custom shapes) within 500ms of drag completion.
2. Text and UI elements inside Windows remain sharp and crisp at 1:1 pixel scaling without blurriness, stretching, or letterbox black bars.
3. Linux desktop sandboxes (`agent-web`) dynamically adapt their XFCE desktop to match the browser window size via XRANDR.
4. If a client browser does not support remote resizing, the console cleanly falls back to client scaling without dropping the connection.
5. All 344 workspace tests continue to pass cleanly.
