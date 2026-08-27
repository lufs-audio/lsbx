# Unit 01 — Provision Windows OpenSSH Server (interactive SPICE, then seal golden)

## Objective
Turn the existing Windows 11 install into an lsbx-manageable golden by installing and hardening **OpenSSH Server for Windows** (the built-in Optional Feature) inside a disposable clone, so every subsequent step (Unit 02, all healthchecks, all `lsbx` management) drives the guest over SSH and never needs a console again. Seal the resulting disk as `~/ISOs/images/goldens/win11-ssh.qcow2`.

## Context
The manual `win11` guest predates lsbx and was provisioned with SPICE-only graphics (`<graphics type='spice'>`), and has **no SSH server**. The libvirt backend's golden healthcheck and `Backend::run` (`crates/lsbx-backend-libvirt/src/guest_ssh.rs`) reach guests only over SSH (username `lsbx`, port 22, key `~/.ssh/lsbx_guest_key`). `lsbx-stream` requires the guest to serve its browser console on in-guest port **8000** (`crates/lsbx-stream/src/proxy.rs:97`), but that browser bridge is Unit 02 — this unit's only output is *SSH presence*.

This is the **only unit in this phase that opens a graphical console at all**, and only because Windows ships no unattended SSH bootstrap. That one console session is a human-in-the-loop step: it is SPICE from the carnyx host via `virt-viewer`, no physical monitor involved. Everything in Units 02/03 is driven over SSH.

## What this unit does NOT do
- Does **not** touch `/var/lib/libvirt/images/win11.qcow2`, `win11.clean-install`, `win11-mem.clean-install`, or the `win11` domain.
- Does **not** install TightVNC / websockify / any VNC stack. That is Unit 02.
- Does **not** register or verify a golden. That is Unit 03.
- Does **not** modify any Rust source.

## Acceptance criteria
- [ ] `virsh -c qemu:///system` shows the original `win11` domain still defined and shut off; `sha256sum /var/lib/libvirt/images/win11.qcow2 /var/lib/libvirt/images/win11.clean-install /var/lib/libvirt/images/win11-mem.clean-install` matches the pre-unit checksums (recorded at start, unchanged at end).
- [ ] A new domain `provision-win11` (COW overlay `provision-win11.clean-install` on the pristine `win11.clean-install` backing file) boots to a desktop, visible via `virt-viewer --connect qemu:///system --domain-name provision-win11` **once**.
- [ ] Inside that guest: "OpenSSH Server" Windows Optional Feature enabled, `sshd` service set **Auto** and running, world-writable `C:\ProgramData\ssh\administrators_authorized_keys` contains the lsbx LF-line-ending public key (`~/.ssh/lsbx_guest_key.pub`); password auth disabled in `sshd_config`; Windows Firewall rule `OpenSSH-Server-In-TCP` enabled on TCP 22.
- [ ] From carnyx (not in the console): `ssh -i ~/.ssh/lsbx_guest_key lsbx@<provision-win11 guest IP>` logs in **key-only, no password, from a fresh SSH client**.
- [ ] `ssh … lsbx@<IP> 'powershell -c "Get-Service sshd | ft Status -a"'` → `Running`.
- [ ] Power off `provision-win11`, delete the COW overlay volume, `qemu-img convert -O qcow2 provision-win11.clean-install ~/ISOs/images/goldens/win11-ssh.qcow2` (flattened, no backing file), and confirm `qemu-img info` shows `backing file: (none)`.
- [ ] The original three `win11*` volumes remain byte-identical after the convert (sha256 repeat of criterion 1).

## Interface contract (files / commands this unit produces, and how Unit 02 consumes it)

```
produces:
  ~/ISOs/images/goldens/win11-ssh.qcow2   # flattened 27GB-resized, backing=none, WIN11-works-over-SSH
  ~/.ssh/lsbx_guest_key                        # existing key, now accepted by the guest
records:
  ssh://lsbx@<guest>                          # username `lsbx`, port 22, key-only, authorized_keys via administrators_authorized_keys
  sha256  of the three preserved win11* volumes   # start-of-unit snapshot committed in this unit
consumed_by_unit_02:  # Unit 02 does `qemu-img create -b win11-ssh.qcow2` and ssh's in
```

## Verification (how this unit is proven done)
```bash
# carnyx, on an overlay clone of win11-ssh.qcow2:
ssh -i ~/.ssh/lsbx_guest_key lsbx@<IP> 'sshd -T | grep -i "passwordauthentication no"'
ssh … 'powershell -c "Get-Service sshd | ft Status -a"'
qemu-img info ~/ISOs/images/goldens/win11-ssh.qcow2          # backing file: (none)
# preservation:
sha256sum /var/lib/libvirt/images/win11.qcow2 /var/lib/libvirt/images/win11.clean-install /var/lib/libvirt/images/win11-mem.clean-install
virsh -c qemu:///system list --all | grep -w win11             # defined, off
```
