# Unit 03 — Flatten, register, verify, and land the `win11-desktop` golden

## Objective
Flatten the fully-provisioned Windows guest (from Units 01 + 02) into a self-contained qcow2, register it as the `win11-desktop` golden in the carnyx manifest (os `windows`, flavor `desktop`, streaming `novnc`, SSH-key healthcheck), run `lsbx golden verify` against a fresh clone, then do a live `lsbx up` → browser console end-to-end. This unit **owns every phase-acceptance criterion** — it is what makes the work a real lsbx golden, not just a disk.

## Context
`lsbx golden` (CLI: `crates/lsbx-cli`, ops: `crates/lsbx-ops`, core: `crates/lsbx-golden/src/{registry,build,verify}.rs`) reads/writes the carnyx manifest `~/repos/lsbx/images.carnyx.json`. The libvirt backend's `DiskMode::Copy` creates each sandbox as a COW overlay on the golden qcow2 (`crates/lsbx-backend-libvirt/src/lib.rs`), so the **registered golden must have no backing file** — hence `qemu-img convert` (the same flattening the bootstrap already does in `crates/lsbx-bootstrap/src/flatten.rs`). The libvirt backend renders `<graphics type='vnc'>` and relies on the guest's own `:8000` (Unit 02). Because a clone boots over SSH and self-serves the browser bridge (Units 01+02), `golden verify` and every `up` afterward need **no interactive console**. Finally this unit records the preservation proof that the original `win11*` volumes/domain are untouched.

## Acceptance criteria
- [ ] `~/ISOs/images/goldens/win11-desktop.qcow2` exists, flattened (`qemu-img info` → `backing file: (none)`), produced by `qemu-img convert -O qcow2 <Unit-02-clone-root> ~/ISOs/images/goldens/win11-desktop.qcow2`.
- [ ] `images.carnyx.json` has a registered golden entry `win11-desktop` with `os: "windows"`, `flavor: "desktop"`, `streaming: "novnc"`, a profile referencing the flattened qcow2, and an SSH healthcheck spec (username `lsbx`, key `~/.ssh/lsbx_guest_key`) — produced via `lsbx --backend libvirt --images images.carnyx.json golden register`, not a hand-edit.
- [ ] `lsbx --backend libvirt --images images.carnyx.json golden list` shows `win11-desktop`.
- [ ] `lsbx --backend libvirt --images images.carnyx.json golden verify win11-desktop` → clean: a fresh clone boots (libvirt `Copy` overlay), SSH key-login succeeds, and the guest's `:8000` websockify answers (criterion from Unit 02's verification, run over the verify channel).
- [ ] End-to-end: `lsbx … up win11-desktop --lease PT20M --json` returns a record whose `.console_url` is a `https://molimo.exe.xyz:8247/…` noVNC URL; opening it in a browser shows a live Windows desktop login/desktop screen. [ ] `lsbx … down <id>` releases it and the reaper path leaves no residue.
- [ ] **Preservation proof** recorded in this unit's `docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/PRESERVATION.md`: sha256 of the three original volumes before/after + `virsh list --all` confirming `win11` still defined-and-off — identical to Unit 01's snapshot.
- [ ] No other changes to `~/repos/lsbx`: this unit touches only `images.carnyx.json` (or brings it via `lsbx golden register`) and the new golden file.

## Interface contract (the registered manifest entry and the browser contract this phase ends with)

```jsonc
// images.carnyx.json, added by `lsbx golden register`:
{
  "golden": "win11-desktop",
  "os": "windows",
  "flavor": "desktop",
  "streaming": "novnc",
  "disk": "goldens/win11-desktop.qcow2",
  "healthcheck": { "ssh": { "username": "lsbx", "key_path": "~/.ssh/lsbx_guest_key" } }
}
// sandbox record after `up` has:
//   streaming: "novnc", https_url: Some("https://molimo.exe.xyz:8247/stream/<id>")  (via SandboxRecord::public())
//   console_url points at a self-contained Windows desktop (VNC on :5900, websockify :8000 in-guest).
```

## Verification (how this unit — and the phase — is proven done)
```bash
lsbx --backend libvirt --images images.carnyx.json golden list
lsbx --backend libvirt --images images.carnyx.json golden verify win11-desktop
SBX=$(lsbx --backend libvirt --images images.carnyx.json up win11-desktop --lease PT20M --json | jq -r .id)
lsbx --backend libvirt --images images.carnyx.json console "$SBX"     # open → live Windows in browser
lsbx --backend libvirt --images images.carnyx.json down "$SBX"
sha256sum /var/lib/libvirt/images/win11.qcow2 /var/lib/libvirt/images/win11.clean-install /var/lib/libvirt/images/win11-mem.clean-install
virsh -c qemu:///system list --all | grep -w win11
```
