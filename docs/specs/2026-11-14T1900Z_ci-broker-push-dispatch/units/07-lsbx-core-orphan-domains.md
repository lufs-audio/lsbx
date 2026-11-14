# Unit 07 — lsbx core: no orphan domains when `lsbx up` is interrupted

## Objective
An interrupted `lsbx up` (SIGKILL, timeout, crash) must never leave a libvirt
domain or disk that lsbx does not know about.

## Context
Measured 2026-11-14 while killing `lsbx-scaleset` mid-provision: domain
`lsbx-ss-spike-f1156e5b` kept running, `lsbx list` was empty, and
`~/ISOs/images/work/lsbx-ss-spike-f1156e5b{.qcow2,-cidata.iso}` remained. Both
CI front ends (polling broker and scale set) cancel `up` on timeouts, so this
leaks real capacity over time. `lsbx-scaleset` carries a prefix-scoped virsh
sweep as a stopgap (`sidecars/lsbx-scaleset/sweep.go`).

## Acceptance criteria
- [ ] `lsbx-lifecycle::create` persists a sandbox record in phase `creating`
      **before** the domain/disk is created, and flips it on readiness.
- [ ] `lsbx reap` (and `lsbx down <id>`) destroys `creating` records older than
      the ready timeout, including domain + disk files.
- [ ] Ignored integration test (real libvirt): kill `lsbx up` 5 s in; `lsbx list`
      shows the `creating` record; `lsbx reap` leaves no domain and no files.
- [ ] Once landed, `lsbx-scaleset`'s virsh sweep is removed (or kept off by
      default) and its README updated.

## Boundaries
- In scope: `crates/lsbx-lifecycle`, `crates/lsbx-backend-libvirt`, `crates/lsbx-store`.
- Out of scope: the exe.dev backend (no local domains).
