# Unit 06 — vdd-session0-resize-and-qxl-coexistence

## Objective
Answer two questions with measurements, because both change the plan: (1) can a
virtual-display-backed mode change be issued from **session 0**, which would
dissolve the `#56` step-1 blocker; and (2) does a virtual display driver coexist
with QXL without blacking out the VNC console.

## Context
- Phase SPEC §3.2 (VDD + the Sunshine pre-session-script pattern, and why a
  session-0 mode change is the whole point), §5 **D7** (resize path decision) and
  **D6** (console role risk), §2.6 (the measured `ChangeDisplaySettingsEx`
  results that this unit is trying to route around).
- `docs/specs/2026-11-14T0425Z_win11-resize-measurement/FINDINGS.md` — the prior
  measurements: SSH/session 0 → `DISP_CHANGE_FAILED`; scheduled task session 1 →
  `DISP_CHANGE_BADPARAM`; Run-key agent never executed.
- `docs/specs/2026-11-14T0349Z_console-and-resize-followups/units/02-windows-resize-display-path.md`
  — the unit this one may supersede (its step 1 is the blocked one).
- Unit `02-golden-sunshine-and-vdd.md` in this folder — installs the VDD this unit
  exercises.
- `scripts/rfb-desktopsize-probe.py` — the instrument: it prints every pseudo-rect
  the RFB server sends, including `DesktopSize` (-223), which is how a framebuffer
  change is observed without a browser.
- README "Verifying VNC-level behaviour without a browser" — the `ssh -L 15900`
  tunnel recipe the probe needs.

## Acceptance criteria
- [ ] **Q1 (session-0 mode change):** with a logged-in console session and Sunshine
      running, a mode change issued **from an SSH session (session 0)** through the
      VDD's own path (Sunshine pre-session script or the driver's control tool)
      either changes the guest display or does not — recorded with the exact
      command, the return value, and the resulting geometry.
- [ ] The geometry change is confirmed by a second `DesktopSize` (-223) rect from
      `scripts/rfb-desktopsize-probe.py` at the new size, not merely by the
      command returning success.
- [ ] **Q2 (QXL coexistence):** with the VDD present and primary, the RFB probe's
      `ServerInit` geometry and a console screenshot are recorded; the finding
      states whether the browser console shows a usable desktop, a black frame, or
      a second head.
- [ ] A third check is recorded: whether the guest still has an **active console
      session** (`qwinsta`) and an interactive desktop after the VDD becomes
      primary.
- [ ] `FINDINGS-06-vdd-resize.md` states a verdict for D7 (VDD path viable /
      not viable → `CreateProcessAsUser` fallback) and for D6 (console usable /
      inspect-only / must be dropped for `win11-desktop`), each with the evidence.
- [ ] The phase SPEC's D6 and D7 are updated in place with the verdict and a
      pointer to the findings file; the "Unverified" table is updated.
- [ ] `#56` is updated on GitHub with the result (a comment, not a close, unless
      the verdict is "fully solved").

## Interface contract
```bash
# This unit adds no product interface. Its deliverables are two documents and
# one instrumented run. The commands that must be recorded verbatim in the
# findings file:
#
#   # from carnyx, session 0 (SSH):
#   ssh -i ~/.ssh/lsbx_guest_key lsbx@<guest-ip> \
#     powershell -NoProfile -File 'C:\Program Files\Sunshine\scripts\set-display.ps1' \
#     -Width 1600 -Height 900
#
#   # observe the framebuffer change without a browser:
#   ssh -f -N -L 15900:127.0.0.1:5900 -i ~/.ssh/lsbx_guest_key lsbx@<guest-ip>
#   scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 12
#
#   # session + display state:
#   qwinsta
#   Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorBasicDisplayParams
```

## Boundaries
- **Touches:** `docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/FINDINGS-06-vdd-resize.md`
  (new), the D6/D7 text and "Unverified" table in the phase `SPEC.md`, and a
  GitHub comment on `#56`.
- **Does NOT touch:** `crates/lsbx-gateway/` (unit 07 implements the endpoint),
  `scripts/provision-win11-golden.ps1` (any golden change needed is a follow-up
  to unit 02), `scripts/rfb-desktopsize-probe.py` (it is the instrument; if it
  needs a flag, that is a separate small change with its own test).

## Verification
```bash
# the unit is done when this produces a verdict, not a guess:
scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 12
#   expect: a second "DesktopSize(-223): new size WxH" line after the mode change
#   if absent: Q1 is negative, and D7 falls back to CreateProcessAsUser

grep -c 'ServerInit' <(scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 2)
#   the reported geometry answers Q2 for the console
```
