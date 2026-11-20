# Unit 04 — Rebuild, restart, end-to-end verification

## Objective
Deploy L0 to the running host and prove a real job is claimed — not merely eligible — by booting a
VM for it.

## Context
- Service: `/etc/systemd/system/lsbx-ci-broker.service`, `ExecStart=/home/carnyx/repos/lsbx/target/release/lsbx ci-broker run --backend=libvirt`,
  `EnvironmentFile=/home/carnyx/ISOs/images/ci-broker.env`.
- Live env edit is Unit 01; counters are Unit 02; banner/version are Unit 03.
- The dispatch path (broker boots a VM from the `ci-runner` golden and registers a runner) has
  **never been exercised** in the current configuration, because no job was ever eligible. Treat
  first dispatch as unproven until observed.
- History to beat: `kb` run 44302952308 queued 1,390 minutes with `runner=` empty.

## Acceptance criteria
- [ ] `cargo build --release` completes; the deployed binary's mtime postdates the last commit.
- [ ] `lsbx --version` reports the commit of the checked-out `HEAD`.
- [ ] `systemctl restart lsbx-ci-broker` succeeds; service is `active (running)` after 60 s.
- [ ] Startup banner appears in `journalctl -u lsbx-ci-broker` with the corrected labels.
- [ ] No `lsbx-windows` anywhere in the effective process environment
      (`tr '\0' '\n' < /proc/<pid>/environ`).
- [ ] At least one `info` tick line is emitted, showing repos/labels/seen/eligible.
- [ ] A dispatched job appears: a CI VM is created from `ci-runner`, the job runs, and the workflow
      reaches a terminal state that is **not** `cancelled` with `runner=` empty.
- [ ] The zero-eligible `warn` path is demonstrated at least once (a deliberately wrong label in a
      scratch run or a unit test) so it is known-live, not merely known-present.
- [ ] No orphaned domains or overlays remain after teardown (`lsbx list`, `virsh list --all`).

## Interface contract
```bash
# deploy sequence
cargo build --release
./target/release/lsbx --version            # commit must match HEAD
sudo systemctl restart lsbx-ci-broker
journalctl -u lsbx-ci-broker -f            # banner + tick lines

# proof of claim (not just eligibility)
gh api "/repos/lufs-audio/<repo>/actions/runs?per_page=5" \
  --jq '.workflow_runs[] | "\(.id) \(.status)/\(.conclusion)"'
gh api "/repos/lufs-audio/<repo>/actions/runs/<id>/jobs" \
  --jq '.jobs[] | "runner=\(.runner_name // "NONE") concl=\(.conclusion)"'
```

## Boundaries
- **Touches:** `target/release/lsbx` (rebuilt), `lsbx-ci-broker.service` (restart only — the unit
  file is not edited), runtime state under `/home/carnyx/ISOs/images/state`.
- **Does NOT touch:** the unit file's contents, env values beyond Unit 01, goldens, workflows.

## Verification
```bash
systemctl is-active lsbx-ci-broker
journalctl -u lsbx-ci-broker --since "-10min" | grep -cE "queue_labels"
lsbx list --json | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["data"]))'
```
