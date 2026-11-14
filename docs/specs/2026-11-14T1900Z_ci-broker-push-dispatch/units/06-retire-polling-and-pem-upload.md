# Unit 06 — retire-polling-and-pem-upload

## Objective
The polling broker and the PEM-upload path are removed from production; scale sets are the only dispatch path, and the static repo list is gone.

## Context
- Units 04/05 must have run green for ≥ 7 days (job URLs in `docs/ci-scaleset.md`).
- Files: `lsbx-ci-broker.service` on carnyx, `/home/carnyx/ISOs/images/ci-broker.env` (`LSBX_CI_REPOS`, `LSBX_QUEUE_LABEL`, `LSBX_CI_POLL_INTERVAL`, `GH_TOKEN`), `crates/lsbx-broker/src/poll.rs`, `reconcile.rs` PEM path, `scripts/provision-ci-runner.sh`.

## Acceptance criteria
- [ ] `lsbx-ci-broker.service` disabled on carnyx (Daniel); `LSBX_CI_REPOS` and the user OAuth `GH_TOKEN` removed from carnyx config.
- [ ] `poll.rs` either deleted or reduced to an opt-in `ci-broker reconcile --once` sweep; no timer polls the REST API by default.
- [ ] PEM-upload code path and `provision-ci-runner.sh` deleted (or moved under `legacy/` with a removal date).
- [ ] Org runner group `exe` and label `lsbx-molimo` have no consumers (grep of `lufs-audio` workflows in the PR).
- [ ] Phase done criteria 1–5 re-checked and pasted into the PR.

## Interface contract
```text
lsbx ci-broker reconcile --once   # optional manual sweep; not a daemon
```

## Boundaries
- **Touches:** `crates/lsbx-broker/src/poll.rs`, `crates/lsbx-broker/src/reconcile.rs`, `scripts/provision-ci-runner.sh`, `deploy/*.service`, `MIGRATION-CARNYX.md`, `README.md`.
- **Does NOT touch:** goldens, gateway/stream crates.

## Verification
```bash
grep -rn 'LSBX_CI_REPOS\|lsbx-runner-app.pem' crates scripts deploy; echo "rc=$? (1 = gone)"
ssh carnyx 'systemctl is-enabled lsbx-ci-broker lsbx-scaleset@linux'
```
