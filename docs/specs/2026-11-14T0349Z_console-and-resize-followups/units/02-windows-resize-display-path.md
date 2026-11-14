# Unit 02 — Windows display path for resize (decision + implementation)

## Objective
Make "drag the browser window to any size and the Windows desktop becomes exactly
that size, sharply" true — which first requires choosing the mechanism, because the
current server cannot do the protocol half of it.

## Context
Read `docs/specs/2026-11-14T0337Z_console-endgame-recon/SPEC.md` §1.4–§1.5 and this
phase's `SPEC.md` §2 and §5-Q1 first. The three facts that force this unit:

1. **TightVNC 2.8.81 removed `ExtendedDesktopSize`.** Vendor release note
   (2023-05-04): *"we had to remove support for the ExtendedDesktopSize
   pseudo-encoding … replaced … with … the new DesktopConfiguration
   pseudo-encoding."* noVNC 1.5.0 speaks the standard pseudo-encoding, not
   TightVNC's replacement. So `resize=remote` (client-initiated) **cannot work**
   with the shipped server at any configuration.
2. **TightVNC still reports framebuffer size changes** through the older,
   standardized `DesktopSize` (-223) pseudo-rect (TigerVNC's implementation survey
   lists TightVNC as a server that sends it: non-incremental, separate update),
   and **noVNC 1.5.0 handles `pseudoEncodingDesktopSize`**. So a size change
   *initiated inside the guest* should reach the browser.
3. **QXL has 16 MiB of video memory** (`vgamem='16384'`), which cannot represent a
   4K framebuffer (3840×2160×4 B = 33.2 MB), so "whatever size" is capped before
   any protocol matters.

The prior resize spec (`docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/`)
assumed (1) was available. It is not; this unit supersedes its Unit 02.

## Decision to record (before code)
Choose one of the four options in `SPEC.md` §5-Q1 and record the choice, the date,
and the rejected options **in the resize spec** (edit that phase's `SPEC.md` with a
dated "superseded" note — do not rewrite its history). Gate the choice on the
measurement in criterion 1 below.

## Acceptance criteria
- [ ] **Measurement first:** with a noVNC session connected to a live `win11`
      sandbox, changing the guest resolution from inside the guest (e.g.
      `QRes`-equivalent via `ChangeDisplaySettingsEx`) causes the browser canvas to
      change size without reconnecting. Recorded in the phase's evidence log with
      timestamps. *(This single experiment decides between Option A and Option B.)*
- [ ] The chosen mechanism is recorded with rationale and rejected alternatives in
      the resize phase's SPEC.
- [ ] Inside a fresh `win11` sandbox, the display can be set to **any** of
      1024×768, 1280×800, 1440×900, 1600×900, 1920×1080, 2560×1440, 3440×1440
      (ultrawide), and 3840×2160, each verified by
      `(Get-CimInstance Win32_VideoController).VideoModeDescription` returning that
      exact geometry.
- [ ] The domain's QXL model carries enough video memory for the largest supported
      mode: `vgamem >= 65536` (KiB) for goldens with `flavor: desktop`, verified by
      `virsh dumpxml` and by a successful 3840×2160 switch.
- [ ] Browser-side resize is debounced (≥200 ms trailing) so a window drag produces
      at most one mode change per settle, verified by the guest-side mode-change
      count during one drag.
- [ ] A client that cannot resize (no JS, or a failed mode-set) keeps a usable
      console: it falls back to client-side scaling without dropping the WS.
- [ ] Regression: `lsbx golden verify win11-desktop` passes after the change.

## Interface contract
```
# Option A (recommended) — host-driven mode set
# in-guest:
C:\lsbx\set-resolution.ps1 -Width <int> -Height <int>      # exit 0 on success
#   implemented with user32!EnumDisplaySettingsW + ChangeDisplaySettingsExW;
#   returns non-zero and a human-readable reason when the mode is unsupported.

# host-side (gateway, authenticated):
POST /sandboxes/{id}/display   { "width": 1920, "height": 1080 }
   -> {"id": "...", "width": 1920, "height": 1080, "applied": true}
   executed as: lsbx exec <id> -- cmd /c C:\lsbx\set-resolution.ps1 -Width W -Height H

# domain XML (libvirt, desktop goldens):
<model type='qxl' ram='65536' vram='65536' vgamem='65536' heads='1' primary='yes'/>
```

## Boundaries
- **Touches:** `scripts/provision-win11-golden.ps1` (ship `set-resolution.ps1`),
  `crates/lsbx-backend-libvirt/src/domain_xml.rs` (vgamem for desktop goldens),
  `crates/lsbx-gateway/src/routes.rs` (display endpoint), the vendored console page
  from Unit 01 (debounced POST), `images.carnyx.json` if a per-golden video-memory
  field is needed.
- **Does NOT touch:** `crates/lsbx-broker/**`, `crates/lsbx-store/**` schemas,
  the Linux golden (Unit 04), the tailcat/socat doors (#52).

## Verification
```bash
# 1. modes, end to end
ID=$(lsbx up win11 --lease 20m --json | jq -r .id)
for g in 1280x800 1440x900 1920x1080 2560x1440 3440x1440 3840x2160; do
  W=${g%x*}; H=${g#*x}
  lsbx exec "$ID" -- cmd /c "C:\\lsbx\\set-resolution.ps1 -Width $W -Height $H"
  lsbx exec "$ID" -- powershell -NoProfile -Command \
    "(Get-CimInstance Win32_VideoController).VideoModeDescription" | grep -F "$W x $H"
done

# 2. video memory
virsh -c qemu:///system dumpxml "$ID" | grep qxl     # expect vgamem='65536'

# 3. browser (human): drag the window, confirm the desktop re-lays-out sharply
lsbx console "$ID"
```
