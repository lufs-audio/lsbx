# Unit 04 — scaleset-listener-service

## Objective
carnyx runs a supervised scale-set listener for `lsbx-linux` that turns GitHub job assignments into disposable VMs, with restart recovery and no orphaned VMs or runners.

## Context
- Phase SPEC → Design; P2 (Go sidecar recommended) and P3/P4 (labels, group `continuo`).
- Unit 02 results (label semantics, latency, API usage) and unit 03 (JIT guest).
- Existing reconciliation and reaping: `crates/lsbx-broker/src/reconcile.rs`, job records in `lsbx-store` (`ci_job_store`), broker lock (`CiJobStore::broker_lock()` → `LockSentinel`). The listener must take the same lock so it never runs alongside the polling broker.
- carnyx memory bounds `max_capacity` (CI golden = 8 GB per VM).

## Decision this unit must record
P2 (language), P3 (final label set), P4 (group), and `max_capacity` with the RAM arithmetic.

## Acceptance criteria
- [ ] Scale set `lsbx-linux` registered in `continuo` with the P3 labels; existing workflows targeting `lsbx-default` / `lsbx-carnyx` run on it unchanged (or the PR lists every workflow that had to change, per unit 02's label answer).
- [ ] Listener runs as systemd `lsbx-scaleset@linux.service` (user `carnyx`, `Restart=on-failure`), takes the broker lock, scales to `min(TotalAssignedJobs, max_capacity)`, acknowledges every message.
- [ ] Killing the listener mid-job and restarting it: the running job finishes, no duplicate VM is created, no orphaned VM/runner after the job (evidence in PR).
- [ ] Cancelling a queued job before a VM is ready: VM is reaped and the JIT runner is removed from the org.
- [ ] A job from a repo absent from all lsbx config runs (phase done criterion 1).
- [ ] 60 minutes idle: < 100 REST requests (phase done criterion 3).

## Interface contract
```ini
# /etc/lsbx/scaleset-linux.toml
org = "lufs-audio"
runner_group = "continuo"
name = "lsbx-linux"
labels = ["lsbx-default", "lsbx-carnyx", "lufs", "X64"]
max_capacity = 2
image = "ci-runner"
lease_seconds = 3600
```

## Boundaries
- **Touches:** listener source (`tools/lsbx-scaleset/**` if Go, or `crates/lsbx-broker/src/scaleset/**` if Rust), `deploy/lsbx-scaleset@.service` (new), `docs/ci-scaleset.md` (new runbook).
- **Does NOT touch:** `poll.rs` (unit 06 retires it), Windows (unit 05).

## Verification
```bash
gh api /orgs/lufs-audio/actions/runner-groups/<continuo_id>/runners --jq '.runners[].name'
ssh carnyx 'systemctl status lsbx-scaleset@linux --no-pager | head -5'
gh run list -R lufs-audio/lsbx --limit 5 --json conclusion,url
```
