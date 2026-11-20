# Unit 01 — Recover v1 provisioning into `scripts/build-kali-golden.sh`

## Objective
Produce a `shellcheck`-clean provisioning script that recreates everything baked by hand into
`lsbx-kali-v1.qcow2` on a fresh base image, so a rebuild no longer silently loses Tor or the
hardening.

## Context
- `docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md` — the authoritative prose description of
  the v1 delta: Tor daemon ports, `/usr/local/bin/tor-route`, `proxychains4`, UFW, `sshd_config`
  hardening (`MaxAuthTries 4`, `X11Forwarding no`, key-only), `/etc/sudoers.d/99-lsbx` retaining
  both `lsbx` and `exedev`.
- `docs/specs/2026-09-17T2114Z_kali-tor-golden/ROADMAP.md` §2.1 — names this script as planned
  work that was never delivered.
- `lsbx golden build --help` — `--from`, `--script`, `--flavor`, `--cpu`, `--memory`, `--os`,
  `--streaming`, `--register`, `--dry-run`, `--no-cleanup`, `--interactive`, `--shell`.
- Retained base: `/home/carnyx/ISOs/images/build/kali-linux-2026.2-cloud-genericcloud-amd64.tar.xz`.

The v1 script is a **recovery** exercise, not a design exercise: the goal is behavioural parity
with the prose, not improvement. Improvements belong to Units 02–05.

## Acceptance criteria
- [ ] `scripts/build-kali-golden.sh` exists and is executable.
- [ ] `bash -n` and `shellcheck` pass with no warnings.
- [ ] Script accepts `--mode <locked|open>`; `locked` installs the Tor stack, `open` does not.
- [ ] Running the script inside a throwaway `lsbx up default` sandbox on stock Kali produces:
      `tor-route` present and executable, `proxychains4` configured, UFW rules present,
      `sshd_config` matching the v1 hardening, `/etc/sudoers.d/99-lsbx` containing both users.
- [ ] Script is idempotent — a second run exits 0 and changes nothing.
- [ ] `lsbx golden build --dry-run --from kali-genericcloud-base --script scripts/build-kali-golden.sh ...` exits 0.

## Interface contract
```bash
# scripts/build-kali-golden.sh
#   --mode <locked|open>   required; locked = Tor stack, open = none
#   --base <path>          optional; default kali-genericcloud-base.qcow2
#   --dry-run              print actions, change nothing
#   exit 0 on success, non-zero on any failed step
#
# Provisioned file paths (parity with v1):
#   /usr/local/bin/tor-route
#   /etc/proxychains4.conf
#   /etc/sudoers.d/99-lsbx
#   /etc/ssh/sshd_config.d/99-lsbx-hardening.conf
#   /var/lib/lsbx/tor-mode        (written in Unit 02)
```

## Boundaries
- **Touches:** `scripts/build-kali-golden.sh` (new only).
- **Does NOT touch:** `crates/**`, `images.carnyx.json`, `images.json`, any `.qcow2`, any existing
  spec folder.

## Verification
```bash
bash -n scripts/build-kali-golden.sh
shellcheck scripts/build-kali-golden.sh
bash scripts/build-kali-golden.sh --mode locked --dry-run
```
