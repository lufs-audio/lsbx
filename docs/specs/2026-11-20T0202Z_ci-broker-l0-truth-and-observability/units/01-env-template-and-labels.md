# Unit 01 — Versioned env template + live label migration

## Objective
Make the broker's label contract correct on the running host, and give it a version-controlled,
secret-free source of truth so the host-local file is no longer the only copy.

## Context
- Live file: `/home/carnyx/ISOs/images/ci-broker.env` (mode 0600, **not** in git). Current relevant
  lines: `LSBX_QUEUE_LABEL=lsbx-windows`, `RUNNER_LABELS=lsbx-windows`,
  `RUNNER_GROUP=continuo`, `LSBX_CI_REPOS=<14 repos>`.
- Why it broke: `crates/lsbx-broker/src/labels.rs::is_eligible` requires a job label to be a member
  of `cfg.queue_labels`. Workflows request `self-hosted,lufs` and `self-hosted,lsbx-default`.
  `lsbx-windows` is requested by nothing.
- `poll.rs:108`: `queue_labels` defaults to `["lsbx-default"]`, so an absent variable would have
  worked. The explicit override is what broke it.
- See `../../2026-11-20T0202Z_ci-broker-l0-truth-and-observability/SPEC.md` §3 and §4 for the
  label and runner-group decisions.

## Acceptance criteria
- [ ] `deploy/ci-broker.env.example` exists in the repo, contains **no secrets** (no `GH_TOKEN`, no
      App key), and carries the corrected label values with a comment explaining each.
- [ ] Live env has `LSBX_QUEUE_LABEL=lsbx-default,lufs`.
- [ ] Live env has `RUNNER_LABELS=lsbx-default,lsbx-carnyx,lufs`.
- [ ] Live env has **no** occurrence of `lsbx-windows`.
- [ ] Live env keeps `RUNNER_GROUP=continuo` (group `lsbx-carnyx` is created in the scale-set phase;
      pointing at a nonexistent group would break registration).
- [ ] Live env retains mode `0600` and its `GH_TOKEN` line unchanged.
- [ ] A backup of the pre-change file exists alongside it.

## Interface contract
```bash
# ci-broker.env — label contract (canonical)
LSBX_QUEUE_LABEL=lsbx-default,lufs          # comma-split; membership decides eligibility
RUNNER_LABELS=lsbx-default,lsbx-carnyx,lufs # labels Carnyx-provisioned runners advertise
RUNNER_GROUP=continuo                       # unchanged in L0; -> lsbx-carnyx with the scale set
# lsbx-windows RETIRED — nothing requests it, nothing offers it.
```

## Boundaries
- **Touches:** `deploy/ci-broker.env.example` (new), `/home/carnyx/ISOs/images/ci-broker.env`.
- **Does NOT touch:** any repo workflow, `crates/**`, runner groups, goldens.

## Verification
```bash
grep -c 'lsbx-windows' /home/carnyx/ISOs/images/ci-broker.env          # expect 0
grep -E '^(LSBX_QUEUE_LABEL|RUNNER_LABELS|RUNNER_GROUP)=' /home/carnyx/ISOs/images/ci-broker.env
stat -c '%a' /home/carnyx/ISOs/images/ci-broker.env                    # expect 600
test -f deploy/ci-broker.env.example && dotenv-linter deploy/ci-broker.env.example || true
```
