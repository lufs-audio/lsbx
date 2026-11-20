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

---

## Status — 2026-11-20 (partial, deliberately staged)

Precondition check: unit 04 has been live since 2026-11-14T20:06Z — **~5.5 days, under the 7-day bar
this unit sets**. Unit 05 (windows-scale-set) is still open, and the owner has since decided Windows CI
is not needed at all.

Given that, the security-critical half was done now and the rest is left gated:

### Done — the App private key no longer reaches a guest

- `reconcile.rs::prepare_runner_upload` no longer reads `config.app_key_path`, no longer writes
  `lsbx-runner-app.pem`, no longer emits `GITHUB_APP_KEY` into `lsbx-runner.env`, and no longer
  `sudo install`s the key into `/etc/lsbx-runner-app.pem`.
- The remainder is **fail-closed**: guest-side registration has no credential and fails, rather than
  the key being silently exposed. A `warn!` at the call site names the replacement (`lsbx-linux`
  scale set + JIT) so a future re-enable cannot fail quietly.
- Removed the now-orphaned `fs::set_permissions(&key_path, …)` chmod.

**Verified:** `cargo build --release` OK, `cargo test -p lsbx-broker` 16 passed,
`cargo clippy --workspace --all-targets -D warnings` clean, and
`grep -rn 'lsbx-runner-app.pem' crates/` returns only comments and a legacy `/etc` cleanup string.

### Not done — and why

- **`provision-ci-runner.sh` not deleted.** It is now unusable (no key), but deleting it is coupled to
  host-side token minting, which is the real replacement work. Deleting it first would leave the
  window path with no registration mechanism at all, before that mechanism exists.
- **`poll.rs` not deleted.** It stays as the documented Windows/fallback path. Deleting it is only
  correct once unit 05 is formally dropped and an opt-in `ci-broker reconcile --once` sweep exists per
  this unit's interface contract.
- **`lsbx-ci-broker.service` still enabled on carnyx.** Disabling it is a host action requiring sudo.
  It is inert meanwhile: its only queue label (`lsbx-windows`) is requested by zero workflows across
  all 14 repos (verified), so it can never reach the runner-provisioning path above.
- `GH_TOKEN` / `LSBX_CI_REPOS` remain in `ci-broker.env`; removing them is part of the same disable.

### Remaining checklist (host, needs sudo)

```bash
sudo systemctl disable --now lsbx-ci-broker     # scale set is the only dispatch path
# then drop GH_TOKEN + LSBX_CI_REPOS from /home/carnyx/ISOs/images/ci-broker.env
```
