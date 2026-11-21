# Unit 02 — golden-sunshine-and-vdd

## Objective
`win11-desktop` is re-baked with Sunshine (host, Windows) and a virtual display
driver installed and configured, started automatically in the logged-in session,
with a healthcheck that proves both are alive — and with the Sunshine UUID
behaviour measured and recorded.

## Context
- Phase SPEC §2.5 (current golden: 8 vCPU / 32 GB / 97 GB qcow2, TightVNC raw RFB
  on `:8000`, autologon working), §3.1 (Sunshine capture/encode/audio/discovery
  facts), §3.2 (VDD and the Sunshine pre-session-script pattern), D1, D5, D9.
- `scripts/provision-win11-golden.ps1` — numbered steps 1–7; this unit adds step
  8 (Sunshine) and step 9 (VDD) and extends the existing healthcheck writers.
- `images.carnyx.json` → `goldens[win11-desktop].healthcheck`.
- README "Working rules for desktop goldens" (C6: idempotent script, never a
  hand-applied mutation; `lsbx golden verify` then a real `lsbx up`).
- The host Sunshine config is **not** a template: `lan_encryption_mode = 0` and
  `upnp = enabled` must not be copied (D9).

## Acceptance criteria
- [ ] `scripts/provision-win11-golden.ps1` installs Sunshine from the pinned
      upstream release, writing a config that: enables audio, binds only the
      guest's interfaces, and leaves UPnP disabled and LAN encryption at its
      default.
- [ ] Sunshine is registered as a Windows service set to start automatically, and
      `Get-Service` reports it `Running` in a fresh sandbox without any manual
      step.
- [ ] The virtual display driver is installed and a virtual monitor is present
      (`Get-PnpDevice` / display enumeration shows it) in a fresh sandbox.
- [ ] A new `C:\lsbx\sunshine-check.ps1` is written by the script and asserts
      both: the service is running, and the GameStream HTTP port answers locally.
- [ ] `images.carnyx.json`'s `win11-desktop` healthcheck includes
      `C:\lsbx\sunshine-check.ps1`.
- [ ] Running the provisioning script twice on the same guest is idempotent (no
      duplicate services, no duplicate firewall rules, no failure).
- [ ] The golden is re-baked and `lsbx golden verify win11-desktop` passes.
- [ ] **Recorded finding:** whether Sunshine's reported UUID is identical across
      two sandboxes cloned from the same golden (read the persisted state file in
      each). Written into the phase SPEC's "Unverified" table as resolved, with
      the answer.
- [ ] **Recorded finding:** whether Sunshine captures the VDD successfully and
      encodes with software x264 in-guest (a single frame arriving in a Moonlight
      client is enough to prove capture; latency is unit 04).

## Interface contract
```powershell
# New script steps (append to scripts/provision-win11-golden.ps1; keep the
# existing numbering style):
#
#   # 8. Sunshine (GameStream host) — audio-bearing stream door
#   # 9. Virtual Display Driver — arbitrary resolution, headless display
#
# New in-guest healthcheck, written by the script like session-check.ps1:
#   C:\lsbx\sunshine-check.ps1
#     exit 0 iff  (Get-Service Sunshine).Status -eq 'Running'
#             and  Invoke-WebRequest http://127.0.0.1:<http-port>/serverinfo succeeds
#
# Sunshine config written to: C:\Program Files\Sunshine\config\sunshine.conf
#   min_threads   = 4
#   stream_audio  = enabled
#   upnp          = disabled
#   address_family= ipv4
#   (no lan_encryption_mode override — inherit the upstream default)
#
# Ports are Sunshine's own defaults inside the guest. The host-side mapping is
# unit 03's business; the guest always listens on the standard block.
```

## Boundaries
- **Touches:** `scripts/provision-win11-golden.ps1`, `images.carnyx.json`
  (`win11-desktop` healthcheck array only), and a new findings file
  `docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/FINDINGS-02-sunshine-vdd.md`.
- **Does NOT touch:** `crates/` at all, `scripts/provision-ci-runner.ps1`,
  the CI golden, `sidecars/`, the host's Sunshine config.

## Verification
```bash
# script-level
pwsh -NoProfile -File scripts/provision-win11-golden.ps1 -WhatIf   # if supported, else dry read

# golden-level
lsbx golden build win11-desktop --from win11 \
  --script scripts/provision-win11-golden.ps1 --flavor desktop \
  --cpu 8 --memory 32GB --os windows --streaming novnc
lsbx golden verify win11-desktop

# fresh-sandbox proof (the only proof that counts)
lsbx up win11
lsbx exec <id> -- powershell -NoProfile -ExecutionPolicy Bypass -File C:\\lsbx\\sunshine-check.ps1
lsbx exec <id> -- powershell -NoProfile -Command "Get-PnpDevice -Class Monitor | Select-Object FriendlyName,Status"
lsbx exec <id> -- powershell -NoProfile -Command "Get-ChildItem 'C:\\Program Files\\Sunshine\\config' -Recurse | Select-String -Pattern 'uuid|uniqueid'"
```
