# Measurement — Windows resize prerequisites (issue #56)

**Timestamp:** `2026-11-14T0425Z`
**Slug:** `win11-resize-measurement`
**Status:** Measurement record + two hard constraints found. Feeds
`docs/specs/2026-11-14T0349Z_console-and-resize-followups/units/02-windows-resize-display-path.md`
and issue #56.
**Guest:** fresh `lsbx up win11` from the golden flattened at 04:05Z
(`sbx-18e7cd6f6b3f3125-e8f06cf2`, 8 vCPU / 32 GiB / 256 GiB, guest IP
192.168.122.251).

---

## 1. What this measurement was for

Issue #56's recommended option ("A: keep TightVNC, drive the mode change from the
host") rests on one assumption:

> TightVNC 2.8.81 removed `ExtendedDesktopSize`, but it still reports framebuffer
> size changes through the older standardized `DesktopSize` (-223) rect — so a
> size change **initiated inside the guest** reaches a connected noVNC client.

If true, we need no new VNC server. If false, the server must be swapped.

---

## 2. Result 1 — TightVNC **does** announce the framebuffer size (-223). Confirmed.

Tool: `scripts/rfb-desktopsize-probe.py` (added to the repo by this measurement; it
speaks RFB 3.8 with VNC auth and prints every pseudo-rect it sees).

```
$ ssh -f -N -L 15900:127.0.0.1:5900 -i ~/.ssh/lsbx_guest_key lsbx@192.168.122.251
$ ./scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 12
[  0.00s] server version: b'RFB 003.008\n'
[  0.04s] security types: [2, 16]
[  0.09s] security result: 0 (0 = ok)
[  0.22s] ServerInit: 2560x1600 name='desktop-7u8n2gf'
[  0.22s] SetEncodings sent: [0, 1, -223, -308, -224, -239]
[  0.23s] *** DesktopSize(-223): new size 2560x1600
rects=4 size_changes=[(0.23, 2560, 1600)]
RESULT: size change reported by server (-223/-308)
```

Two facts:

- The server announces the current geometry with a `DesktopSize` pseudo-rect at
  connect — it speaks the *legacy* pseudo-encoding even though 2.8.81 dropped the
  *extended* one. That is the mechanism the client-side (noVNC 1.5.0) already
  handles via `pseudoEncodingDesktopSize`.
- The current framebuffer is **2560×1600**, i.e. 16.4 MB at 32bpp — which is
  essentially the **entire** 16 MiB QXL `vgamem`. There is no headroom at all;
  4K (33 MB) is impossible without raising `vgamem`.

**So: Option A's protocol premise is sound.** The server will report a size change
if something changes the guest's mode.

---

## 3. Result 2 — **A display-mode change cannot be issued from an SSH session.** Hard constraint.

Attempting the mode change over SSH (which is how `lsbx exec` and therefore the
whole "host-driven" design would reach the guest):

```
$ ssh … lsbx@192.168.122.251 powershell -File set-res.ps1 -Width 1920 -Height 1080
EnumDisplaySettings(CURRENT)  rc=0  -> 0x0        # rc=0 means FAILED
EnumDisplaySettings(REGISTRY) rc=0  -> 0x0
CDS_TEST             rc=-1                        # DISP_CHANGE_FAILED
CDS_UPDATEREGISTRY   rc=-1                        # DISP_CHANGE_FAILED
now: 0x0
```

SSH on Windows runs in **session 0** (`qwinsta` shows `services` as the current
session for the SSH login), and session 0 has no access to the interactive
display. `ChangeDisplaySettingsEx` therefore fails unconditionally — not because
the requested mode is invalid, but because the caller is not in the session that
owns the display.

**Consequence for #56:** "gateway → `lsbx exec` → `set-resolution.ps1`" does **not**
work as written. The mode change must execute inside the *interactive console
session* (as the logged-in user), or a small agent must live there and be poked by
the host.

## 4. Result 3 — there is **no logged-in session** on this golden. Second hard constraint.

```
$ qwinsta
 SESSIONNAME   USERNAME   ID     STATE   TYPE   DEVICE
>services                 0      Disc                 <- where SSH lands
 console                  1      Conn                 <- nobody logged in
```

The console session exists but has **no user**: the guest sits at the logon
screen, and that is what the VNC server is mirroring. Which means:

- there is no interactive session for a mode change to run in, and
- the "desktop" a browser sees today is the *logon screen*, not a desktop.

Attempts to make an interactive session appear unattended, for the record:

| Attempt | Result |
|---|---|
| `Winlogon\AutoAdminLogon=1` + `DefaultUserName`/`DefaultPassword`/`DefaultDomainName` | no logon after reboot |
| plus `PasswordLess\Device\DevicePasswordLessBuildVersion=0` and `AutoLogonCount=999` | still no logon after reboot |
| RFB keystrokes to the logon screen (`rfb_type.py`, typed `lsbx` + Enter) | keys accepted by the server, no logon — the logon desktop requires a **secure attention sequence** first |

This matches Windows 11 25H2 behaviour: classic `AutoAdminLogon` is commonly
defeated by the passwordless/Hello defaults, and the SAS wall means you cannot
type a password into the logon screen over VNC without first sending
Ctrl+Alt+Del.

---

## 5. What this means for the plan

The resize design gets **simpler in one axis and one requirement bigger**:

1. **Protocol:** no server swap needed. TightVNC's `-223` reporting + noVNC's
   `DesktopSize` handling are sufficient. Option B (TigerVNC WinVNC) stays on the
   shelf; Option C (virtio-gpu) stays a separate project.
2. **Session:** the guest must run a **logged-in desktop session** for any resize
   to be possible, and for the console to be a desktop at all. So #56 gains a
   prerequisite that is valuable independently:
   - **autologon that actually works on Win11 25H2** (most reliable path:
     Sysinternals `Autologon.exe`, which stores the credential as an LSA secret
     rather than `DefaultPassword`; or `netplwiz`'s "users must enter a password"
     toggle), **plus** an idle-lock defeat (`NoLockScreen=1`,
     `InactivityTimeoutSecs=0`, `DelayLockAfterResume` off) so the session stays
     logged in and unlocked for the life of the sandbox.
3. **Trigger:** the mode change runs *in that session*. Two workable shapes:
   - **`schtasks /IT` as the logged-in user** (interactive token), fired by
     `lsbx exec`; simple, no new daemon, but it is a scheduled-task dance.
   - **A tiny in-session display agent** (autostart in the user's Run key) that
     watches a file/pipe for `width,height` and applies the mode. More moving
     parts, but it makes the host→guest call a single file write and it can also
     do things scheduled tasks cannot (multi-monitor, retries, mode enumeration).
4. **Video memory:** `vgamem` must be raised (64 MiB) before 4K is even
   representable; at 2560×1600 the current 16 MiB is already 98% consumed.

## 6. Recommended sequence for #56 (revised)

```
1. Autologon + unlocked idle session in the golden  (prerequisite, independently
   valuable: the browser console shows a desktop instead of a logon screen)
2. vgamem 16 MiB -> 64 MiB for flavor: desktop
3. Display agent (or schtasks /IT) that applies a requested mode in-session
4. Gateway endpoint + console-page debounce (unchanged from the unit contract)
5. Verify with scripts/rfb-desktopsize-probe.py: expect a second
   `DesktopSize(-223)` rect at the new geometry after each requested mode
```

## 7. Artefacts

- `scripts/rfb-desktopsize-probe.py` — the probe (new, in-repo, documented).
- `rfb_type.py` (throwaway) — RFB keystroke injector used for the logon attempt;
  not committed, since it did not achieve anything and is a remote-input tool.

## 8. Open questions

- **O1** Does `schtasks /IT` work when the logged-in user is the same account the
  task is registered under, on a session created by autologon? (Needs the working
  autologon first.)
- **O2** Does an in-session mode change actually produce a *second* `-223` rect
  within a second or two? Everything says yes; it is unverified because we could
  not create a session. **This is the remaining unknown**, and it is the gate for
  keeping TightVNC.
- **O3** Does TightVNC keep mirroring the console session across a resolution
  change without dropping the client? (Related to #53's wedge behaviour.)
