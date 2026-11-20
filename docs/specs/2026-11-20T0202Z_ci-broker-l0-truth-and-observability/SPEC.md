# Phase Spec — CI broker L0: truth and observability

**Timestamp:** `2026-11-20T0202Z`
**Slug:** `ci-broker-l0-truth-and-observability`
**Branch:** `feature/ci-broker-l0-observability`
**Status:** Units 01–03 IMPLEMENTED and verified. Unit 04 BLOCKED on a host restart (no passwordless sudo).

**Related:** `docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/SPEC.md` (the P1–P4 push-dispatch
phase this L0 precedes).

---

## 1. Problem

CI jobs stopped being claimed on 2026-11-14 and nobody noticed for five days. Two independent
defects produced that, and only one of them is about labels:

1. **A label contract no two parties agree on.** The broker claims a job only if one of the job's
   labels is in `cfg.queue_labels` (`crates/lsbx-broker/src/labels.rs::is_eligible`). The running
   config supplied `queue_labels = {lsbx-windows}`; no workflow in any repo requests
   `lsbx-windows`. Workflows request `[self-hosted, lufs]` (`kb`) and `[self-hosted, lsbx-default]`
   (`lsbx`). Therefore **zero jobs were ever eligible.** Evidence: `kb` run 44302952308 sat queued
   **1,390 minutes** and ended `cancelled` with `runner=` empty; `lsbx CI` has had no run at all
   since 2026-11-14T20:10Z.
2. **The broker cannot report that it is broken.** `crates/lsbx-broker/src/poll.rs` contains **zero**
   `tracing` calls (verified: 0 in `poll.rs`, 15 in `reconcile.rs`). It polled 14 repos every 45 s,
   claimed nothing, and logged nothing. The label mismatch is the trigger; the **silence** is the
   reason it lasted five days.

Also observed: `queue_labels` *defaults* to `["lsbx-default"]` (`poll.rs:108`) — the env file
overrode a correct default with an unusable value.

## 2. Goal

Make the broker **truthful** and **observable**, so that a label mismatch of this kind is loud
within one poll interval instead of invisible for five days. This layer is deliberately independent
of the push-dispatch decision (P1–P4): it is required whichever transport wins, and it is the
fallback if the transport migration stalls.

## 3. Decisions carried in from intake

- **`lufs` is wanted on Carnyx** (owner decision) - and it is **already served** by the live
  `lsbx-linux` runner scale set, not by this poller. Correction recorded in section 11.
- **The x86-placement discriminator is `lsbx-carnyx`** (owner decision — option C in the intake
  discussion): both Carnyx and the ARM fleet may advertise `lufs`, and jobs that must be x86 say
  `lsbx-carnyx`.
- **`lsbx-windows` is retired** (owner decision): it was never wired to anything and is not needed.
  No runner should advertise it and no workflow should request it. Unit 05 of the push-dispatch
  phase (`windows-scale-set`) is therefore **out of scope, permanently**.
- **P1:** A (runner scale sets) conditional on the P3 spike. **P2:** (a) Go sidecar.
  **P3:** `lsbx-linux` labels `lsbx-default, lsbx-carnyx, lufs, X64`. **P4:** runner group
  `lsbx-carnyx` — but see §4.
- **Single dispatch path (corrected).** Linux CI is push-dispatched by the `lsbx-linux` scale
  set, live since 2026-11-14T20:06Z. This poller must hold **no** label the scale set serves, or
  the two race and both boot a VM for one job. See section 11.
- **No GitHub-side mutation in this phase.** Carnyx's runners get the `lsbx-carnyx` **label** here.
  The `lsbx-carnyx` **runner group** does not exist (`/orgs/lufs-audio/actions/runner-groups` has
  `Default, exe, continuo, lufs, lsbx-default`) and is created by the scale-set work, because
  pointing `RUNNER_GROUP` at a nonexistent group would break runner registration — strictly worse
  than the bug being fixed.

## 4. Why the runner group is not changed here

`RUNNER_GROUP=continuo` stays in this phase. Registering a runner into a group that does not exist
fails; a failed registration means **no dispatch at all**, i.e. the exact outage this phase exists
to end. Groups are needed by the scale-set API at scale-set-creation time, so `lsbx-carnyx` is
created in the push-dispatch phase and `RUNNER_GROUP` flips then. The user-facing contract — what
workflows write — is the *label* `lsbx-carnyx`, and that lands now.

## 5. Design

### 5.1 Observability, not API-scanning

The earlier idea of validating `queue_labels` against every workflow's `runs-on` at startup was
rejected: discovering workflows costs 14 repos × N workflow fetches against the same 5,000/hr REST
budget that motivated push dispatch in the first place (`2026-11-14T1900Z` SPEC §Problem: 87
requests per poll, ~4,300/hr at 45 s). A check that re-creates the budget problem is not a fix.

Instead the invariant is **derived from data the broker already fetches**:

- Every tick reports, at `info`: repos scanned, labels scanned, jobs seen, jobs eligible.
- A `debug` line per (repo, label) reports the job count for that cell, so a mismatch is visible at
  the exact cell that returned nothing.
- If two or more consecutive ticks see `jobs_seen > 0` but `jobs_eligible == 0`, emit **one**
  `warn!` naming the configured labels and the labels actually seen on those jobs. That warning
  would have fired on the very first poll of 2026-11-14 and named the fix
  (`configured={lsbx-windows} seen={self-hosted,lufs}`).
- A startup banner logs the effective configuration (labels, repo mode, interval, fallback delay,
  group, binary version and git commit), so "what is this process actually configured to do" never
  requires reading `/proc/<pid>/environ` again.
- `lsbx --version` reports the git commit, so a running binary can never silently drift from
  `main` again (the running binary was built 2026-11-13T22:46, six days before `main`).

### 5.2 What is deliberately NOT in this phase

- No `tor`-style fail-closed behaviour changes, no dispatch-logic changes, no scale set, no
  webhooks, no App-key removal (that is L1 — a separate, security-motivated unit), no changes to
  which runner claims which job beyond the label set.
- No workflow changes in any of the 14 repos. This phase changes only the broker's own view.

## 6. Deliverables

| # | Unit | Depends on |
|---|---|---|
| 01 | Versioned env template + live label migration | — |
| 02 | `Poller::tick` observability: counters, per-cell debug, zero-eligible warning | — |
| 03 | Startup banner + git commit in `--version` | — |
| 04 | Rebuild, restart, end-to-end verification | 01, 02, 03 |

## 7. Done criteria

- [ ] `ci-broker.env` labels are `lsbx-default,lufs` (queue) and `lsbx-default,lsbx-carnyx,lufs`
      (runner); `lsbx-windows` appears nowhere in configuration.
- [ ] A versioned, secret-free template of the env file exists in the repo so the host-local file is
      not the only copy of the contract.
- [ ] Every poll emits one `info` summary with non-zero counts when jobs are queued, and per-cell
      `debug` lines.
- [ ] A tick that sees jobs but claims none produces exactly one `warn` naming configured vs seen
      labels — proven by a unit test, not only by observation.
- [ ] Broker startup logs version, commit, labels, repo mode, interval and group.
- [ ] `lsbx --version` prints the git commit.
- [ ] A real `kb` or `lsbx` job created after the restart is **claimed** (not merely eligible) and
      a VM boots for it.
- [ ] `cargo test -p lsbx-broker` and `cargo clippy --workspace --all-targets -- -D warnings` pass.

## 8. Boundaries

- **Touches:** `crates/lsbx-broker/src/poll.rs`, `crates/lsbx-broker/src/reconcile.rs` (call sites
  only), `crates/lsbx-cli/src/lib.rs` (version string), `deploy/ci-broker.env.example` (new),
  host-local `/home/carnyx/ISOs/images/ci-broker.env` (label lines only).
- **Does NOT touch:** `crates/lsbx-broker/src/labels.rs` (the eligibility **rule** is correct and
  stays; only visibility into it is added), any workflow in any repo, any golden image, the
  `lsbx-carnyx` runner group, `images.json`.

## 9. Ecosystem references

- **`lsbx-carnyx` group vs label:** GitHub runner *groups* scope which repositories may use a runner
  and are set at registration/scale-set creation. They are **not** selectable via `runs-on`. Workflows
  select runners by *label*. This is why a group named `lsbx-default` exists while
  `runs-on: [self-hosted, lsbx-default]` currently matches nothing.
- **Current org runner labels in use:** `self-hosted, Linux, ARM64, continuo, lufs`.
- **REST budget:** 5,000/hr, shared across the account (currently 1,400 used).
- **Host-local env:** `/home/carnyx/ISOs/images/ci-broker.env`, mode 0600, not version-controlled.

---

## 10. Implementation status (2026-11-20)

### Landed and verified

| Unit | Result |
|---|---|
| 01 | `deploy/ci-broker.env.example` added. Live env now `LSBX_QUEUE_LABEL=lsbx-default,lufs`, `RUNNER_LABELS=lsbx-default,lsbx-carnyx,lufs`, `RUNNER_GROUP=continuo`; `lsbx-windows` count 0; mode 600; `GH_TOKEN` intact; backup at `ci-broker.env.bak-20261120-l0` |
| 02 | `TickOutcome` + per-cell `debug` + per-tick `info` + once-per-streak `warn` + eligibility dedupe. `cargo test -p lsbx-broker`: 15 pass incl. the new streak test; `cargo clippy --workspace --all-targets -D warnings`: clean |
| 03 | `crates/lsbx-cli/build.rs` injects `LSBX_GIT_COMMIT`; `lsbx --version` → `lsbx 0.1.0 (07fff50)`; startup banner emitted before `run_broker` |

### Extra drift fixed

`LSBX_BIN` pointed at the retired Python shim (`…/lufs-sandbox-server/.venv/bin/lufs-sandbox`).
Only `crates/lsbx-bootstrap/src/systemd.rs:122` reads it — the running broker does not — so it was
latent, not live. Repointed at `target/release/lsbx` in both the template and the live env.

### Unit 04 — BLOCKED

`sudo -n` on the host requires a password, so the agent cannot restart the service. The running
process still holds the **deleted** old inode (`/proc/1817/exe` → `…/lsbx (deleted)`); the rebuilt
binary sits on disk and will be picked up on restart.

Operator commands (approval + a terminal required):

```bash
sudo systemctl restart lsbx-ci-broker
journalctl -u lsbx-ci-broker -f | grep -E "ci-broker starting|poll tick|none eligible"
./target/release/lsbx --version          # must print 0.1.0 (<sha>)
```

Proof of *claim* (not just eligibility) still outstanding: trigger a `kb` or `lsbx` job and confirm
`runner=` is not empty and the conclusion is not `cancelled`.

### Deliberately not done

Runner group `lsbx-carnyx` was **not** created and `RUNNER_GROUP` stays `continuo` — see §4.

### 10.1 Follow-up found by the live restart: no subscriber existed

The first restart proved the binary swapped but emitted **nothing** — because the workspace had **no
`tracing` subscriber anywhere**. Every `tracing::*` call in the codebase (including the 15
pre-existing ones in `reconcile.rs`) had always been a no-op. L0's log lines were therefore
"known-present, not known-live" — the exact distinction this phase's own acceptance criteria warn
about.

Fixed by initialising a subscriber in `crates/lsbx-cli/src/main.rs` (`tracing-subscriber`'s
`EnvFilter`, default `info`, `RUST_LOG` overriding, `try_init` so it cannot panic). Proven live:

```
$ ./target/release/lsbx serve --port 18123 --stream-port 18124
 INFO lsbx serve: background reap pass completed destroyed=[] keys_reconciled=0
```

Requires one more service restart to take effect. `RUST_LOG=debug` enables the per-cell poll lines.

## 11. Correction: the first diagnosis was incomplete

Implementation initially widened the poller to `LSBX_QUEUE_LABEL=lsbx-default,lufs`. That was
**wrong, and reverted before merge.**

`origin/main` moved to `29d510d` (2026-11-14) while this phase was in progress. It records that
P1–P4 were already decided, the Go sidecar is the chosen implementation (P2a), the scale set is
`lsbx-linux` (P3), the group is `continuo` (P4) — and that **unit 04 landed and has been live since
2026-11-14T20:06Z** as the systemd **user** unit `lsbx-scaleset@linux.service`.

Verified live on this host:

- `lsbx-scaleset@linux.service` — active, PID 7841, long-polling (`Getting next message
  lastMessageID=…`), 74 Actions-service requests/hr vs 3 REST/hr. Healthy.
- Its labels: `lsbx-linux, lsbx-default, lsbx-carnyx, lufs, self-hosted, Linux, X64`
  (`~/ISOs/images/scaleset-linux.env`).
- `ci-broker.env` had been **deliberately narrowed** to `LSBX_QUEUE_LABEL=lsbx-windows` for exactly
  this reason.

So the original framing — "the broker's label is wrong" — was only half right. The poller's label was
correct **for its narrowed role**. The genuinely broken things were:

1. the poller's total silence (real; fixed here), and
2. `lufs` not matching the scale set until it was recreated with that label — the most plausible
   explanation for the `kb` job that sat 23 h on 2026-11-18, since a scale set matches only the labels
   it was **created** with, and a changed `LSBX_SS_LABELS` only takes effect on a recreate.

### The hazard this avoided

Two dispatch paths serving one label is worse than either path being down: both boot a VM for one
job, and placement becomes a race. The poller's own `TickOutcome` counters would eventually have
shown it, but only after the waste.

### Corrected end state

- Scale set `lsbx-linux`: the sole Linux dispatch path.
- Poller: narrowed to `lsbx-windows` (inert — nothing requests it), or stopped outright once the
  Windows decision is final. **One path at a time, chosen explicitly, never overlapping.**
