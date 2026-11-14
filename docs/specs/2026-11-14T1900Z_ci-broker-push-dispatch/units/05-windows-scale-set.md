# Unit 05 — windows-scale-set

## Objective
Windows CI jobs (`lsbx-windows`) are served by their own scale set from the win11 golden using a JIT config.

## Context
- Unit 04 listener (one instance per scale set); unit 03 JIT pattern.
- Windows golden and its runner upload path: `reconcile.rs` uploads to `C:\lsbx-ci-broker`; win11 golden work in `docs/specs/2026-08-27T1854Z_win11-browser-desktop-golden/`.

## Acceptance criteria
- [ ] Scale set `lsbx-windows` in `continuo` with labels from P3; `max_capacity` justified against carnyx RAM alongside `lsbx-linux`.
- [ ] Guest runs `run.cmd --jitconfig` from a file that is deleted after read; no PEM uploaded.
- [ ] One Windows job completes successfully; VM reaped.

## Interface contract
```ini
# /etc/lsbx/scaleset-windows.toml
name = "lsbx-windows"
labels = ["lsbx-windows", "Windows"]
image = "win11"
max_capacity = 1
```

## Boundaries
- **Touches:** Windows guest script, `/etc/lsbx/scaleset-windows.toml` template in `deploy/`, `images.carnyx.json` (notes).
- **Does NOT touch:** Linux golden, `poll.rs`.

## Verification
```bash
gh run list -R lufs-audio/<windows-test-repo> --limit 3 --json conclusion,url
ssh carnyx 'virsh -c qemu:///system list --all'
```
