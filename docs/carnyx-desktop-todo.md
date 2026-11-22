# Carnyx Desktop Rollout: Next Steps & Verification Checklist

This to-do document outlines the exact operational checklist to resume and finalize live host validation on Carnyx for the Tailnet Moonlight media relay and allocator (Unit E of `docs/specs/2026-11-21T2250Z_carnyx-desktop-rollout/SPEC.md`).

All code, unit tests, and gateway integration tests are committed and passing. Merging to `main` does not deploy or alter production; live canary execution is performed in an isolated sandbox environment.

---

## 0. Pre-Flight & Safety Rails

- [ ] **Production isolation:** Confirm production `lsbx-serve.service` remains untouched on port 8243 and host Sunshine remains on port 47989.
- [ ] **Dedicated canary storage:** Use `~/lsbx-tailnet-canary-nov21` for all canary state, disks, and isolated gateway runs.
- [ ] **Backing image:** Ensure `win11-desktop-audio-canary.qcow2` is present in `~/lsbx-rollout-nov21/win-images/` (symlinked in canary environment).
- [ ] **Release binary:** Ensure `target/release/lsbx` is compiled (`cargo build --release --bin lsbx`).

---

## 1. Boot Windows Audio Canary VM

- [ ] Navigate to the isolated canary test directory:
  ```bash
  cd ~/lsbx-tailnet-canary-nov21
  ```
- [ ] Spin up the disposable audio canary VM:
  ```bash
  ./cli up win11-audio-canary --lease 60m --name tailnet-win-a-nov21
  ```
- [ ] Note the assigned `<canary-id>` and private guest IP (e.g. `192.168.122.x`).
- [ ] Confirm libvirt domain XML includes the ICH9 audio device (`type='none'`):
  ```bash
  virsh dumpxml <vm-tag> | grep -A 2 "<sound model='ich9'>"
  ```

---

## 2. In-Guest Sunshine Provisioning

- [ ] Stage Sunshine MSI and provisioning scripts into guest `C:\lsbx\`:
  ```bash
  ./cli put <canary-id> ~/lsbx-rollout-nov21/Sunshine-Windows-AMD64-installer.msi C:/lsbx/Sunshine-canary.msi
  ./cli put <canary-id> provision-win11-sunshine-canary.ps1 C:/lsbx/provision-win11-sunshine-canary.ps1
  ./cli put <canary-id> lsbx-sunshine-auth.ps1 C:/lsbx/lsbx-sunshine-auth.ps1
  ./cli put <canary-id> ~/lsbx-rollout-nov21/lsbx-win-tone.ps1 C:/lsbx/canary-tone.ps1
  ./cli put <canary-id> ~/lsbx-rollout-nov21/lsbx-win-tone.wav C:/lsbx/canary-tone.wav
  ./cli put <canary-id> ~/lsbx-rollout-nov21/lsbx-win-audio-task.ps1 C:/lsbx/canary-audio-task.ps1
  ```
- [ ] Execute Sunshine installation in the clone on guest anchor port 47989:
  ```bash
  ./cli --json exec --timeout 120 <canary-id> -- powershell -NoProfile -ExecutionPolicy Bypass \
    -File "C:/lsbx/provision-win11-sunshine-canary.ps1" -Installer "C:/lsbx/Sunshine-canary.msi" -DisposableClone -AnchorPort 47989
  ```
- [ ] Initialize Sunshine local admin credentials:
  ```bash
  ./cli --json exec --timeout 60 <canary-id> -- powershell -NoProfile -ExecutionPolicy Bypass \
    -File "C:/lsbx/lsbx-sunshine-auth.ps1" -Mode SetUp -AnchorPort 47989
  ```
- [ ] Start interactive audio playback tone (997 Hz stereo):
  ```bash
  ./cli --json exec --timeout 60 <canary-id> -- powershell -NoProfile -ExecutionPolicy Bypass \
    -File "C:/lsbx/canary-audio-task.ps1"
  ```

---

## 3. Run Isolated Gateway with Moonlight Relay

- [ ] Launch an isolated test gateway on port 18243 bound to Tailscale:
  ```bash
  export LSBX_MOONLIGHT_BIND_IP=100.125.210.60
  export LSBX_MOONLIGHT_CLIENT_IP=100.125.210.60  # or your client machine tailnet IP
  export LSBX_MOONLIGHT_GOLDEN=win11-desktop-audio-canary
  export LSBX_MOONLIGHT_SLOTS=1
  export LSBX_MOONLIGHT_HOST_ANCHOR=50005

  /home/carnyx/repos/lsbx/target/release/lsbx \
    --backend libvirt \
    --images ~/lsbx-tailnet-canary-nov21/images.json \
    --state-dir ~/lsbx-tailnet-canary-nov21/state \
    serve --host 100.125.210.60 --port 18243 --token canary-token --insecure
  ```
- [ ] In a separate shell, acquire Moonlight Slot 0 via the gateway CLI:
  ```bash
  /home/carnyx/repos/lsbx/target/release/lsbx \
    --gateway http://100.125.210.60:18243 --token canary-token --insecure \
    moonlight <canary-id>
  ```
- [ ] Verify slot assignment:
  * Address: `100.125.210.60:50005`
  * Active: `true`
  * Slot: `0`
- [ ] Confirm relay sockets are open on Carnyx:
  ```bash
  ss -tulpn | grep 50005
  ```

---

## 4. Moonlight Pairing & Stream Verification

- [ ] Pair Moonlight to the relay address `100.125.210.60:50005`:
  * From Carnyx test client:
    ```bash
    xvfb-run ~/lsbx-rollout-nov21/moonlight/bin/moonlight-sandbox pair 100.125.210.60:50005
    ```
  * Note the 4-digit PIN emitted by Moonlight.
- [ ] Confirm pairing inside the guest via `lsbx-sunshine-auth.ps1`:
  ```bash
  ./cli put <canary-id> <(echo -n "$PIN") C:/lsbx/sunshine-test-pin.txt
  ./cli --json exec --timeout 60 <canary-id> -- powershell -NoProfile -ExecutionPolicy Bypass \
    -File "C:/lsbx/lsbx-sunshine-auth.ps1" -Mode Pair -AnchorPort 47989
  ```
- [ ] Record a 30-second streaming session:
  ```bash
  xvfb-run ~/lsbx-rollout-nov21/moonlight/bin/moonlight-sandbox stream \
    100.125.210.60:50005 "Desktop" -fps 60 -width 1920 -height 1080
  ```
- [ ] Inspect recorded media artifact:
  * Verify H.264 video streams without corruption.
  * Verify 48 kHz stereo audio with `ffmpeg -i <recording>.mkv -af volumedetect -f null -` (non-silent).
  * Confirm 997 Hz tone frequency in audio spectrum.

---

## 5. Teardown & Verification

- [ ] Release Moonlight slot:
  ```bash
  /home/carnyx/repos/lsbx/target/release/lsbx \
    --gateway http://100.125.210.60:18243 --token canary-token --insecure \
    moonlight <canary-id> --release
  ```
- [ ] Destroy disposable canary VM:
  ```bash
  ./cli down <canary-id>
  ```
- [ ] Verify socket closure:
  ```bash
  ss -tulpn | grep 50005  # Must return empty
  ```
- [ ] Record telemetry and timing metrics into `docs/carnyx-desktop-runbook.md`.
