# Carnyx desktop canary runbook

Read the [rollout spec](specs/2026-11-21T2250Z_carnyx-desktop-rollout/SPEC.md) first.
The tests below change or start **only operator-approved canaries**. Merging a PR is not deployment. Keep the current gateway binary/config, production manifest, active golden disks, host Sunshine and CI configuration unchanged unless there is a separately approved promotion window.

## Baseline and evidence

1. Record UTC, the candidate Git SHA, installed `lsbx --version`, libvirt/QEMU versions, the manifest backing keys, guest OS/driver versions, and disk free space. Compare CPU, RAM, display devices and service state with the November 21 desktop-audio spec. Run a comparable CI job before the first stream; record wall time and host CPU and disk activity.
2. For each candidate use a new versioned backing disk and separate profile in your operator-owned manifest, with the same `os`, `streaming` and health-check semantics from `images.carnyx.json` unless listed below. Never rewrite the active qcow2. A libvirt backing key resolves to `<images-dir>/<backing-key>.qcow2`. Use the supported provision-and-flatten route documented in the repository if `golden build` has no configured flattener.
3. Put the VM under normal store/lease management (short TTL, note the sandbox ID); end the test with `lsbx down <id>`. Collect version/latency/recovery results, not tokens, pairing PINs, client keys, ticket URLs, or other credentials.

## Browser console

- For a new `win11-desktop` clone, confirm the host sees an `RFB` greeting on the private guest port `8000`, not HTML. Then open the gateway console from a separate machine. Enter the guest VNC credential when prompted, not the gateway bearer token. Check framebuffer and input, deliberately close the tab, and verify a new tab can reconnect. Repeat both directly over the tailnet and through the normal HTTPS edge. Check an idle session and post-lease behavior. Sample socket counts before and after a batch of reconnects; don't treat a successful WebSocket upgrade as acceptance.
- Repeat with the versioned Linux raw-RFB canary. While testing resize, use an in-guest `xrandr` query to distinguish a real framebuffer change from viewport scaling. Confirm input coordinates at two viewport sizes. Leave existing profiles untouched until both paths pass.

## Linux: a versioned resize canary

1. Use a disposable clone of an approved Linux guest. The legacy recipe is not attached to this development environment; the compatibility mode is reconstructed from the excerpts recorded in the dated spec and preserves the static Xvfb/websockify path. It is not the candidate for gateway use.
2. Before provisioning TigerVNC, make sure the assigned guest interface is on the private bridge, and restrict ingress to the Carnyx gateway from the guest or bridge firewall. `RFB_PRIVATE_CIDR` chooses the **local listen address**, not a remote ACL. No route advertisement, guest LAN/public listener, or browser access to raw port 8000. Stage an operator-picked eight-character VNC password in a root-only file on the guest's `/run` (tmpfs), and pass its path as `VNC_PASSWORD_FILE`. Delete the plaintext input after use. The resulting VNC password file is intentionally retained for the guest service; VNC's legacy encoding is reversible, so keep the image private and rotate credentials for promoted copies.
3. Run `scripts/provision-linux-desktop-golden.sh --mode tigervnc-raw-rfb-v1` inside the clone using the environment in the README. Re-run in the same canary to verify repeatability. Confirm `lsbx-desktop.service` remains up, the installed `lsbx-linux-desktop-check` passes, and the host sees RFB. For a versioned candidate profile replace the obsolete `curl .../vnc.html` health check with the installed check; do not change the current `agent-web` entry. Use a different backing key for the candidate and retain the old base.
4. Run `scripts/rfb-resize-canary.py` from Carnyx against this disposable guest only, then separately exercise `/console?target=<id>&resize=remote` and the default scale/recovery path from a browser. Record dimensions, pointer correctness, and reconnect results before any profile promotion.

## Windows: one audio canary, no production media relay

1. Copy/flatten the approved Windows backing disk to a **new** key such as `win11-desktop-audio-canary`. Set `LSBX_LIBVIRT_AUDIO_GOLDEN=win11-desktop-audio-canary` for the process that creates the test VM. It matches the *backing golden key*, not its profile display name. Use a local CLI on Carnyx with its normal store, or set this in the existing gateway's environment during an approved rollout. Never start a second gateway against the production backend. Unset it to stop adding the device to new VMs. An existing VM's XML is not mutated.
2. Confirm `virsh dumpxml <domain>` shows the ICH9 sound device bound to audio ID 1 (`type='none'`). In the test guest, run `scripts/check-win11-render-endpoint.ps1` (copy it into the clone first); select the tested output device in Windows and use a test tone. An active endpoint alone is not proof that Sunshine captures it.
3. Install a reviewed, pinned Sunshine installer **inside the running clone only**. Verify signature and recorded SHA256 before installation. Do not flatten installed credentials or pairing state into an image. Start with QXL, software H.264, SDR, 1080p60, 15–20 Mbps, UPnP disabled and an explicit render endpoint if the default is wrong. Keep the admin UI accessible only through an approved management channel on the private network. For a first test use a client with approved private reachability to just the canary; if none exists, stop at render verification and arrange a narrow canary path. Do not expose guest ports or publish the whole guest subnet just to test.
4. Pair from the actual Moonlight client. Moonlight displays the PIN, which is entered in Sunshine. Try an actual DAW project with the intended Windows output mode; record audible dropouts and sync. Confirm browser recovery access after streaming and a cold boot. If QXL capture fails, trial a reviewed, signed VDD on the clone only and verify that TightVNC still sees the intended display.
5. Complete the capacity gate in the spec before promising multiple streams. Record direct vs relayed network path, p50/p95 input-to-display, client dropped frames, encode time, audible dropouts, CPU/I/O, and CI duration. Leave relay implementation and Windows resize endpoint gated on the evidence. For a resize probe, first observe the browser framebuffer change after an in-guest action; Moonlight changing a different virtual display is not evidence.

## Rollback / disposition

Destroy just the test sandboxes, remove canary profile selection and the audio opt-in, and restore prior config/binary if the test changed them. Retain the old disk and any safe, redacted evidence. Do not toggle IOMMU or move a GPU during this rollout. Park alternate off-tailnet transports (#52) unless an actual client cannot use existing HTTPS or Tailscale. Do not close issues on the basis of code checks alone.

## Windows preset resize (opt-in)

Copy `scripts/set-win11-display.ps1` to `C:\lsbx\set-win11-display.ps1` in an
approved clone. Confirm that SSH is using the same account as the logged-in
console desktop. The helper uses a temporary interactive scheduled task rather
than trying to change the display from Windows SSH session 0.

For a canary gateway only, set `LSBX_WINDOWS_DISPLAY_RESIZE_GOLDEN` to the
exact Windows backing key used by that clone. Leave it unset to disable the
feature. After connecting to noVNC, pick a preset and press **apply**. This is
explicit, not a reaction to every browser viewport change. `GET
/sandboxes/{id}/display` returns supported presets; `POST` with an authenticated
JSON body such as `{"preset":"1600x900"}` applies one. A failed change leaves
noVNC scaling available. Ultrawide 3440×1440 is not supported by the tested QXL
configuration.

### Carnyx evidence — 2026-11-21

- A fresh Windows clone booted with ICH9 and `audio type='none'`, saw one
  active Windows render endpoint, and retained the QXL recovery console. Live
  Chromium found a missing WebSocket `binary` subprotocol response; after the
  fix, it rendered 2560×1600, delivered keyboard input and reconnected in under
  one second over the isolated SSH-forwarded gateway. The authenticated browser
  resize control changed the running framebuffer from 1920×1080 to 1600×900
  without reconnecting. A separate raw RFB test saw the reverse change via
  `DesktopSize (-223)`.
- Pinned Sunshine `v2026.914.233613` (upstream SHA-256 and Windows Authenticode
  verified), QXL Desktop Duplication, and software H.264 worked with Moonlight
  Qt `v6.2.0`. Use `scripts/provision-win11-sunshine-canary.ps1` on a fresh clone
  to reproduce the guest install. Keep the admin UI on guest loopback; pair one
  client and allow media only from Carnyx's private bridge address.
- The Carnyx-local Moonlight client paired and recorded 1280×720/30 with
  non-silent 48 kHz stereo while CI run `44961935114` succeeded on Carnyx. A
  separate ~90-second 1920×1080/60 test negotiated H.264/60, captured non-silent
  stereo (the recorded tone was independently measured at 997 Hz in both
  runs), and did not require a VDD or GPU passthrough. The recording container
  includes preroll. This establishes capture/playback, not p95 input latency,
  frame delivery under motion, DAW behavior, or long-run capacity. The 1080p
  run did not overlap that CI job.
- The live Linux TigerVNC canary applied 1600×900 and 1024×768 by raw RFB;
  `xrandr` independently confirmed both modes. Provisioning exposed a service
  startup race: the revised health check waits for the bind address and RFB
  greeting rather than assuming the socket is ready at `systemctl restart`.
- Media was direct over the private libvirt bridge, **not** over the tailnet or
  an lsbx slot relay. Do not advertise the guest subnet as a shortcut. A
  tailnet-only relay, two distinct concurrent guests and slot reuse, off-host
  measurements, and public-edge acceptance still require their own tests.
