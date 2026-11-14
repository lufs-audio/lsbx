# Unit 02 — scaleset-spike

## Objective
Prove, with a throwaway listener and one real job, that a runner scale set in `lufs-audio` group `continuo` can receive a job and hand a disposable carnyx VM a JIT runner config that completes it — and answer the label question that decides P3.

## Context
- Phase SPEC → Options (A), Open decisions P1–P4, Design.
- `github.com/actions/scaleset` README: create scale set → message session → `GetMessage` long-poll → `GenerateJitRunnerConfig` → runner started with the JIT config; `DeleteMessage` acknowledges; scale on `statistics.TotalAssignedJobs`, not message counts; jobs can be reassigned up to 3× (`JobAssigned` then `JobCompleted` with `canceled`). `examples/dockerscaleset` is the closest template.
- Auth: GitHub App (`ClientID`, `InstallationID`, `PrivateKey`) is the recommended mode. The existing App is id `4377007` (org `lufs-audio`); its PEM lives on carnyx in the runner env. The App needs the org **Self-hosted runners: read & write** permission.
- Runner binary in the CI golden: `run.sh --jitconfig <base64>` (actions/runner ≥ 2.297).

## Decision this unit must record
Inputs for P1/P2 written in the PR: does A work end-to-end; pickup latency; any preview-API surprise; and **P3's label answer** — does `runs-on: [self-hosted, <label>]` match a scale set whose labels include `<label>`, or only `runs-on: <label>`?

## Acceptance criteria
- [ ] A scale set `lsbx-spike` exists in group `continuo` with labels `lsbx-spike` and one extra label `lsbx-spike-extra`; it is deleted at the end of the unit.
- [ ] Three dispatches from a test repo **not** in `LSBX_CI_REPOS`: `runs-on: lsbx-spike`, `runs-on: [self-hosted, lsbx-spike]`, `runs-on: lsbx-spike-extra`. Each outcome (picked up / stays queued) recorded with run URLs.
- [ ] At least one run completes **successfully** in a carnyx libvirt VM that received only a JIT config (no `.pem` in the upload set; guest-side `find / -name '*.pem' 2>/dev/null | grep -i app` empty in the job log).
- [ ] Queued → runner-online latency measured for 3 jobs.
- [ ] Idle listener for 30 min: REST `rate_limit.core.used` delta for the App installation recorded.
- [ ] Spike code lives on a branch under `spikes/scaleset/` and is not wired into the production broker.

## Interface contract
```text
lsbx-scaleset-spike --app-id 4377007 --installation-id <id> --key <pem> \
  --org lufs-audio --runner-group continuo --name lsbx-spike --labels lsbx-spike,lsbx-spike-extra \
  --max-capacity 1 --lsbx-gateway https://lsbx.lufshq.com --image ci-runner
```

## Boundaries
- **Touches:** `spikes/scaleset/**` (new), `docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/units/02-scaleset-spike.md` (results section).
- **Does NOT touch:** `crates/**`, production `lsbx-ci-broker.service`, `ci-broker.env`, org runner groups other than creating/deleting `lsbx-spike`.

## Verification
```bash
gh api /orgs/lufs-audio/actions/runner-groups --jq '.runner_groups[]|select(.name=="continuo").id'
gh run list -R lufs-audio/<test-repo> --limit 3 --json conclusion,url,displayTitle
ssh carnyx 'virsh -c qemu:///system list --all | grep lsbx || echo clean'
```
