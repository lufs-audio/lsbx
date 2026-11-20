# Phase Spec — CI broker: from polling to push dispatch

**Timestamp:** `2026-11-14T1900Z`
**Slug:** `ci-broker-push-dispatch`
**Status:** specced — decisions P1–P4 open (Daniel). Unit 01 is independent
of them and can start now.
**Related:** `danialrami/molimo` → `docs/ci.md` (measurements) and
`docs/specs/2026-11-14T1625Z_calimbra-parity-and-cutover/units/09-ci-carnyx-end-to-end.md`
(carnyx is the default x86 CI; molimo's exe broker is retired).

---

## Problem

The zero-idle broker (`lsbx ci-broker run`, `crates/lsbx-broker`) finds work by
**polling the GitHub REST API**. Measured on carnyx 2026-11-14:

- Each poll = 14 repos × **3 queue labels** × 2 status calls
  (`actions/runs?status=queued|in_progress`) + `runs/{id}/jobs` per active run
  ≈ **87 requests**, taking ~25 s.
- At `LSBX_CI_POLL_INTERVAL=15` that was ~6,960 requests/hour against a
  5,000/hour budget: the broker was **blind ~17 minutes of every hour**.
  At 45 s it is ~4,300/hour — under budget, ~700/hour of headroom, and that
  budget is shared with everything else on Daniel's GitHub account (the token
  is a user OAuth `gho_` token).
- 2/3 of those calls are redundant: `Poller::tick` (`crates/lsbx-broker/src/poll.rs`)
  loops `for repo { for label { queued_jobs(repo, label) } }` and
  `queued_jobs` re-lists the same runs and jobs for every label.
- The repo list is **static** (`LSBX_CI_REPOS`, 14 repos in
  `/home/carnyx/ISOs/images/ci-broker.env`). A new repo gets no CI until
  someone edits that file and restarts the broker.
- Pickup latency is bounded below by the poll interval (+ ~70 s VM boot).
- **Security:** `reconcile.rs::prepare_runner_upload` copies the GitHub App
  **private key** into every disposable VM
  (`/tmp/lsbx-ci-broker/lsbx-runner-app.pem`) so the guest can mint its own
  registration token (`scripts/provision-ci-runner.sh`). Any job that can
  read that file holds org-wide runner-admin power for the App's lifetime.

Daniel's requirements: **push from GitHub instead of polling**, **no repo
list to maintain**, more efficient, flexible, future-proof.

## Options

| | A. **Runner scale sets** (`actions/scaleset`) | B. **`workflow_job` webhooks** (GitHub App) | C. Smarter polling |
|---|---|---|---|
| How work arrives | Broker holds a **long-poll** on GitHub's Actions message queue (~50 s hold, returns immediately when a job is assigned). Outbound only | GitHub POSTs `workflow_job` (queued/in_progress/completed) to an HTTPS endpoint | Broker asks the REST API on a timer |
| Repo list | **None.** The scale set is registered at org level in a runner group; any repo the group admits (incl. new ones) can target it | **None** if the App is installed on "All repositories" | Discovery via `installation/repositories` (already supported when `config.repos` is unset and App auth is used) |
| REST API budget | Not on the hot path (message session API) | Not on the hot path; reconciliation only | The whole problem |
| Inbound exposure | None | Public HTTPS endpoint (e.g. `hooks.lufshq.com` on calimbra → tailnet → carnyx), HMAC-verified | None |
| Lost events | Messages are redelivered until acknowledged (`DeleteMessage`) | **GitHub does not retry failed deliveries automatically**; needs a redelivery/reconcile sweep | n/a |
| Runner credentials in the VM | **JIT config** (`GenerateJitRunnerConfig`): single-use, one runner, no App key | App key or a pre-minted registration token | same as today |
| Who matches jobs to runners | GitHub (assigns a job to an idle scale-set runner; `statistics.TotalAssignedJobs` is the scaling signal) | Broker (label match, then a race among same-label runners) | Broker |
| Maturity | **Public preview** library ("API is stable, interfaces may change"); GitHub's ARC uses it as its **sole** client since ARC 0.14.0 (2026-03). Go only | Stable, documented for years | — |
| Labels | Scale set name is a label; **multiple labels supported on github.com** | Any labels | Any labels |

**Recommendation: A (scale sets)**, with C's unit 01 fix shipped first as a
stopgap and B documented as the fallback if the preview API moves against us.
A meets every requirement at once — push-style, no repo list, no REST budget,
no inbound port — and it fixes the App-key-in-VM problem as a side effect.

## Decisions (recorded 2026-11-14, Daniel)

- **P1 → A, runner scale sets.** Webhooks (B) stay specified as the
  **pinned fallback** (endpoint on calimbra, `danialrami/molimo` registry) if
  the scale-set API leaves preview badly; no work on B now.
- **P2 → (a), Go sidecar** `sidecars/lsbx-scaleset/`, `actions/scaleset`
  `v0.5.0`, driving VMs through the lsbx CLI.
- **P3 → `lsbx-linux`**, labels **`lsbx-linux, lsbx-default, lsbx-carnyx,
  self-hosted, Linux, X64`** (+ `lufs` once `lufs-runner`'s ARM64 pin is
  merged). Spike answer: a scale set matches **only the labels it was created
  with** and does **not** implicitly carry `self-hosted`/`Linux`/`X64`; with
  them added explicitly, existing `runs-on: [self-hosted, lsbx-carnyx]`
  workflows match unchanged. `lsbx-windows` analogous (unit 05).
- **P4 → `continuo`.**

## Status (2026-11-14)

| unit | state | evidence |
|---|---|---|
| 01 poll-dedupe | open (moot once 06 lands) | poller kept for `lsbx-windows` only |
| 02 scaleset-spike | **done** | `lufs-audio/lmid` runs `43634252712` (3 label forms), `43634593785` (kill mid-job), `43635118229` (cancel) — see `sidecars/lsbx-scaleset/README.md` |
| 03 jit-runner-golden | **JIT path done**; golden bake open | `scripts/run-jit-runner.sh`; guest upload = `jitconfig` + script + runner tarball, no key (unit test + guest `find`/`grep`). The `ci` golden still lacks `/opt/actions-runner`, so the sidecar stages a host-cached, checksum-verified runner 2.338.2 |
| 04 scaleset-listener-service | **done, live since 2026-11-14 20:06 UTC** | `lsbx-scaleset@linux` (systemd **user** unit — `carnyx` has no sudo; linger on); poller narrowed to `LSBX_QUEUE_LABEL=lsbx-windows` in `ci-broker.env` (backup `.bak-20261114-prescaleset`), **takes effect when Daniel restarts `lsbx-ci-broker`** |
| 05 windows-scale-set | open | Windows stays on the poller |
| 06 retire-polling-and-pem-upload | open (after 05) | PEM upload now only on the Windows path |
| 07 lsbx-core-orphan-domains | **new, open** | see finding below |

**Finding → unit 07.** Killing `lsbx up` mid-boot (SIGKILL, or a timeout
cancelling the CLI) leaves a running libvirt domain and its
`<name>.qcow2`/`<name>-cidata.iso` that `lsbx list` never recorded, so `lsbx
down` cannot reach them. The polling broker has the same exposure. The sidecar
sweeps domains under its own name prefix (`LSBX_SS_VIRSH_URI`), but the real
fix is in `lsbx-lifecycle::create`: persist a `creating` record before the
domain is defined, and let `reap` clean `creating` records older than the boot
timeout.

**Measured (production `lsbx-linux`, `lufs-audio/lmid` run `43635862928`, 5
jobs, max 4):** queued → job running **25–26 s** for jobs that got a slot
immediately (VM boot 14 s + ~10 s staging/extracting the runner; the job starts
< 2 s after the runner listens); the job that waited for a slot started at
49 s. The polling broker's measured pickup was 72 s (`danialrami/molimo`
`docs/ci.md`). Done criterion 2 (boot + < 5 s) is **not yet met**: baking the
runner into the `ci` golden (unit 03 remainder) removes most of the ~10 s.
`lufs-audio/lsbx` CI run `43635704194` (`[self-hosted, lsbx-default]`,
unchanged workflow) passed on `lsbx-linux`. Observed coexistence: while the
old poller still held `lsbx-default`/`lsbx-carnyx`, job 5 of that run went to
the poller's idle duplicate VM — GitHub hands a job to *any* idle matching
runner, scale set or not (this is why `lufs` waits for the Pi pin). Also:
two jobs in parallel; listener SIGKILL → GitHub holds the old message session ~60 s
before a new one opens (the listener retries), queued jobs wait and then run,
running jobs finish and are reaped after restart.

## Open decisions (as originally posed)

- **P1 — Dispatch model:** A (recommended) vs B. If B, the endpoint lives on
  calimbra (`danialrami/molimo` registry) and that repo gets a unit.
- **P2 — Implementation language for the listener:**
  - **(a) Go sidecar** `lsbx-scaleset` built on `github.com/actions/scaleset`
    + its `listener` package, driving lsbx through the existing gateway HTTP
    API or CLI (create sandbox, put JIT config, destroy). *Recommended:* the
    GitHub protocol stays in GitHub's maintained code — the part most likely
    to change during preview — while lsbx's Rust core stays the source of
    truth for VMs.
  - (b) Port the protocol to Rust inside `lsbx-broker` (one binary; ~6
    endpoints: scale-set CRUD, message session, `GetMessage`,
    `DeleteMessage`, `GenerateJitRunnerConfig`, runner removal). Every
    upstream protocol change becomes our job.
- **P3 — Scale sets and labels.** Proposed: `lsbx-linux` (labels
  `lsbx-default`, `lsbx-carnyx`, `lufs`, `X64`) and `lsbx-windows` (labels
  `lsbx-windows`, `Windows`). **Must be verified by the spike (unit 02):**
  whether jobs written as `runs-on: [self-hosted, lsbx-default]` match a scale
  set labelled `lsbx-default`, or whether workflows must drop `self-hosted`.
- **P4 — Runner group:** `continuo` (consistent with `danialrami/molimo` D5).

## Design (for A)

```
GitHub Actions service ◀── long-poll (outbound HTTPS) ── lsbx-scaleset (carnyx, systemd)
        │  JobAssigned / statistics                         │
        │                                                   │ desired = TotalAssignedJobs (≤ max_capacity)
        │                                                   ▼
        │                                     lsbx gateway / CLI: create sandbox from golden
        │                                     put JIT config → guest runs `run.sh --jitconfig`
        ◀── runner connects, takes one job ─────── disposable VM (ephemeral, one job)
                                              reaper destroys VM on JobCompleted / lease expiry
```

- One listener per scale set; `max_capacity` = how many VMs carnyx will run
  for that set (memory-bound: ci golden is 4 vCPU / 8 GB).
- Existing pieces reused: `lsbx-store` job records + broker lock,
  `reconcile.rs` reaping and restart recovery, the CI goldens.
- The poller (`poll.rs`) is retired from the hot path once A is proven; it
  may remain as an opt-in reconciliation sweep.

## Units

| # | unit | depends on | decision |
|---|---|---|---|
| 01 | poll-dedupe (stopgap) | — | — |
| 02 | scaleset-spike | — | P1, P2 (spike informs them) |
| 03 | jit-runner-golden | 02 | — |
| 04 | scaleset-listener-service | 02, 03 | P2, P3, P4 |
| 05 | windows-scale-set | 04 | P3 |
| 06 | retire-polling-and-pem-upload | 04 (+05) | — |
| 07 | lsbx-core-orphan-domains | — | — |

Shared-file order: `crates/lsbx-broker/src/poll.rs`: 01 → 06;
`crates/lsbx-broker/src/reconcile.rs`: 03 → 06; `images.carnyx.json` / goldens:
03 → 05.

## Done criteria

1. A job from a repo that has **never been listed anywhere** in lsbx config
   runs on a carnyx VM (proves: no repo list).
2. Pickup latency (queued → runner online) is VM boot time + < 5 s, measured
   over 5 jobs.
3. Broker REST API usage over 60 minutes with 0 jobs: < 100 requests.
4. No GitHub App private key is written into any guest (grep of the upload
   set + a guest-side `find / -name '*.pem'` in a test job).
5. Cancelled jobs and listener restarts leave no orphaned VMs or runners.

## Ecosystem References

- `actions/scaleset` (Go, public preview): https://github.com/actions/scaleset — `listener` package, `examples/dockerscaleset`.
- GitHub docs, self-hosted runners → autoscaling (`workflow_job` webhooks): https://docs.github.com/en/actions/reference/runners/self-hosted-runners
- Org runner groups (`lufs-audio`): `Default`, `exe` (retiring), `continuo`, `lufs` (private repos only), `lsbx-default`.
- carnyx runtime: `lsbx-ci-broker.service`, env `/home/carnyx/ISOs/images/ci-broker.env`, runner env `/home/carnyx/ISOs/images/ci-runner.env`. User `carnyx` has no passwordless sudo; system-unit changes need Daniel. Login shell is fish (`sed` is aliased to `sd`) — pipe scripts to `bash -s`.
- `danialrami/molimo` `docs/ci.md` — targeting table and the API-budget measurements this spec cites.
