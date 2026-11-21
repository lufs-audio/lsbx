# Unit 01 — guest-audio-device

## Objective
Desktop-flavour libvirt domains gain a Windows-visible audio endpoint (`ich9`
Intel HDA) whose QEMU backend is `none`, so a guest can render audio without
coupling the VM to carnyx's user audio session.

## Context
- `docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/SPEC.md`
  §2.1 (QEMU 11.1.3 sound devices), §3.5 (why `virtio-sound` is rejected), D3.
- `crates/lsbx-backend-libvirt/src/domain_xml.rs` — two render functions: a
  generic one (non-desktop) and the Windows/desktop one that already emits the
  QXL `<video>` block. The desktop branch is the only one that gains audio.
- The operator's hand-built `win11` domain is the working reference:
  `<sound model='ich9'/>` + `<audio id='1' type='spice'/>`. This unit uses
  `type='none'` instead of `spice` for the reason in D3.

## Acceptance criteria
- [ ] The desktop/Windows render function emits `<sound model='ich9'/>` and an
      `<audio id='1' type='none'/>` element inside `<devices>`.
- [ ] The generic (non-desktop) render function is byte-for-byte unchanged;
      a test asserts its output contains no `<sound` and no `<audio`.
- [ ] A test asserts the desktop output contains `<sound model='ich9'` and
      `<audio id='1' type='none'`.
- [ ] `virsh define` + `virsh start` on a domain rendered by the changed function
      succeeds (no schema error from libvirt 12.9).
- [ ] Inside a booted `win11` sandbox, `Get-PnpDevice -Class AudioEndpoint`
      (or `Get-CimInstance Win32_SoundDevice`) lists at least one HDA device.
- [ ] `cargo test -p lsbx-backend-libvirt` passes.

## Interface contract
```rust
// crates/lsbx-backend-libvirt/src/domain_xml.rs — no public API change.
// Rendered XML fragment added to the desktop branch's <devices>:
//
//   <sound model='ich9'/>
//   <audio id='1' type='none'/>
//
// Placement: after <graphics>, alongside <video>. libvirt requires the
// <audio> element to reference an id that a <sound> device consumes; a bare
// <sound> without <audio> inherits QEMU's default backend, which is why the
// element is explicit rather than omitted.
```

## Boundaries
- **Touches:** `crates/lsbx-backend-libvirt/src/domain_xml.rs` (and its inline
  `#[cfg(test)]` module only).
- **Does NOT touch:** `crates/lsbx-kernel` (no schema change), the golden
  provisioning script (unit 02), `images.carnyx.json`, any other crate.

## Verification
```bash
cargo test -p lsbx-backend-libvirt
cargo clippy -p lsbx-backend-libvirt --all-targets -- -D warnings

# end-to-end, after unit 02's golden exists (or against the current golden):
lsbx up win11
lsbx exec <id> -- powershell -NoProfile -Command "Get-CimInstance Win32_SoundDevice | Select-Object Name,Status"
# expect at least one device with Status=OK
virsh dumpxml <domain> | grep -A1 '<sound'
```
