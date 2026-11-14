# Unit 02 — In-Guest Dynamic Display Resizing Engine for Windows 11

## Objective
Implement native on-the-fly display resolution adaptation for Windows 11 sandboxes, enabling the guest QXL / VirtIO-GPU display adapter to switch its virtual resolution dynamically when requested by the browser console.

## Context
Windows 11 virtual machines created via `lsbx` run under KVM with UEFI and a virtual QXL or VirtIO-GPU display adapter (`domain_xml.rs`). TightVNC Server for Windows mirrors the primary desktop framebuffer, but historically operates on a static display resolution configured in the user profile.

To support dynamic resizing, Windows requires a display mode change using the Win32 `ChangeDisplaySettingsEx` API. This unit provisions a lightweight resolution daemon (`lsbx-display-agent`) or hooks TigerVNC's native `ExtendedDesktopSize` handler so that incoming resize requests immediately apply new width/height geometries to the active display adapter.

## Acceptance criteria
- [ ] Windows guest includes a resolution change utility (`C:\lsbx\set-resolution.exe` or PowerShell script calling `user32.dll!ChangeDisplaySettingsExW`).
- [ ] Invoking the resolution change with valid dimensions (e.g. `1920x1080`, `1440x900`, `1600x1200`) instantly updates the active Windows desktop resolution without requiring a user logout or reboot.
- [ ] VNC server detects the desktop resolution change and transmits an updated RFB framebuffer update (`DesktopSize` pseudo-encoding) to all connected clients.
- [ ] `domain_xml.rs` renders adequate video RAM for high-resolution displays (at least 32MB VRAM / 64MB RAM on the video adapter).
- [ ] Healthcheck `curl -fsS http://127.0.0.1:8000/vnc.html` and SSH command execution remain 100% operational after multiple resolution changes.

## Interface contract
```powershell
# In-guest resolution invocation contract:
# C:\lsbx\set-resolution.ps1 -Width <int> -Height <int>

param(
    [int]$Width = 1920,
    [int]$Height = 1080
)

# Win32 ChangeDisplaySettingsEx call against primary display:
# Returns 0 (DISP_CHANGE_SUCCESSFUL) on success
```

## Boundaries
- **Touches:**
  - `scripts/provision-win11-golden.ps1`
  - `crates/lsbx-backend-libvirt/src/domain_xml.rs` (QXL/VirtIO-GPU memory parameters if needed)
- **Does NOT touch:**
  - Linux guest configuration (Unit 03 owns Linux)
  - `crates/lsbx-broker`

## Verification
```bash
# Boot fresh clone of win11-desktop:
SBX=$(lsbx up win11 --lease 15m --json | jq -r .id)
IP=$(lsbx info "$SBX" --json | jq -r .host)

# Change resolution to 1920x1080 over SSH:
ssh -i ~/.ssh/lsbx_guest_key lsbx@$IP 'powershell -c "C:\lsbx\set-resolution.ps1 -Width 1920 -Height 1080"'

# Verify new resolution reported by Windows:
ssh -i ~/.ssh/lsbx_guest_key lsbx@$IP 'powershell -c "(Get-CimInstance Win32_VideoController).VideoModeDescription"'
# Expect: "1920 x 1080 x 4294967296 colors"

lsbx down "$SBX"
```
