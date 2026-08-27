# Molimo Rust cutover evidence — August 27, 2026

## Decision

Molimo is cut over to the Rust implementation. Production now runs:

- `lsbx-serve.service` — enabled/active, Rust gateway on `100.122.170.73:8244`.
- `lsbx-ci-broker-exe.service` — enabled/active, Rust GitHub App broker.

The Python units remain installed and disabled for rollback. `lufs-runner@1`
and `lufs-runner@2` remained active throughout and were not modified.

## Pre-cutover

- Host: `molimo`.
- Old gateway and broker: active and enabled before maintenance.
- Old sandbox inventory: empty.
- Rust sandbox inventory: empty before cutover.
- Local old gateway health: HTTP 200.
- Caddy-fronted Molimo health: HTTP 401 without credentials; this listener is
  intentionally plain HTTP, not HTTPS.
- Carnyx proxy health: HTTP 401 without credentials before cutover.
- GitHub App token exchange: passed using App `4377007`, installation
  `148555887`; the PEM remained at its existing `exedev`-owned mode-0600 path.
- exe.dev golden verification: `agent-base` passed git, Python, curl, and
  web-search checks.
- Backup: `/home/exedev/lsbx-migration-backup-20260827T054226Z`.

## Verification before cutover

- Release workspace build succeeded and `/usr/local/bin/lsbx` installed.
- `cargo test -p lsbx-backend-exedev --test test_conformance -- --ignored`
  passed against the live `exe.dev` SSH alias.
- Side-by-side Rust gateway passed authenticated health, REST create, exec,
  info, and delete; final inventory was empty.
- Merged stream routes now require gateway auth except the intentional public
  `/console` page.
- Gateway create limit is enforced at eight sandboxes, matching Python.
- Broker transient GitHub `BackendUnavailable` poll errors now retry instead of
  terminating the process; authentication and response-shape errors remain
  fatal.

## Cutover and live checks

- New serve unit started cleanly with `--max-sandboxes 8 --reap-ttl 3h`.
- New broker started cleanly with App credentials and installation discovery.
- Local authenticated health: HTTP 200, `backend_name=exedev`,
  `backend_available=true`, `sandbox_count=0`.
- Public authenticated Caddy health: HTTP 200 with the same Rust health body.
- Live gateway create/exec/delete passed; the command returned
  `molimo-cutover-ok` and final inventory was empty.
- Unauthenticated `/health`: HTTP 401.
- Unauthenticated `/consoles/missing`: HTTP 401.
- Unauthenticated `/console?target=missing`: HTTP 200.
- Carnyx proxy through Molimo Caddy remained reachable (HTTP 401 without its
  bearer credential); no Caddy or Carnyx service was changed.
- Both Rust services: active, zero restarts, exit status 0.

## Broker maintenance-window rehearsal

An existing queued `ci-broker-failure-test` workflow run was reconciled by the
new Molimo broker (run `33038641471`):

| Job | Result | Runner | VM cleanup |
|---|---|---|---|
| `long-molimo` / `98406988860` | success | `lsbx-molimo-1787809555-05b5059a` | passed; VM and record removed |
| `fail-molimo` / `98406989001` | failure as intended | `lsbx-molimo-1787810280-2e6ffcdc` | passed; VM and record removed |

The broker registered both ephemeral runners, handled the ten-minute job and
intentional failure, scrubbed/destroyed both VMs, and left no CI records, key
files, or VMs in the Rust state directory. The initial rehearsal exposed and
fixed the shell-command argv bug in runner provisioning/cleanup before this
successful pass.

## Lightweight benchmark

These are host-local single samples; exe.dev network latency dominates control
plane operations.

| Metric | Python | Rust |
|---|---:|---:|
| `--help` cold command | 0.07 s | <0.01 s |
| `list --json` | 0.08 s | 0.98 s |
| idle gateway memory | 15.4 MiB | 4.4 MiB |
| idle broker memory | 75.6 MiB | 18.9 MiB |
| direct `ssh exe.dev true` control | 0.87 s | same control |

## Credential provenance

Molimo's existing `lufs-runner` configuration confirms the intended auth path:

- `/etc/lufs-runner/runner.env`: App `4377007`, org scope, `lufs-audio`, group
  `exe`, labels `exe,lufs`, two slots.
- `/etc/lufs-runner/app-private-key.pem`: root-owned, mode `0600`.
- The PEM is byte-identical to the exedev-owned mode-0600 copy already used by
  the Rust broker at `/home/exedev/.lufs-sandbox/lufs-audio-ci-app.pem`.
- Installation ID is unset and auto-discovered.
- `lufs-runner.sh check` successfully minted a registration token without
  modifying runner services.

The root-only `/etc` PEM should not be made readable by the Rust service. The
existing exedev-owned identical copy is the correct least-privilege input.

## Current CI policy note

The `exe` runner group disallows public repositories, while `lufs-audio/lsbx`
is public. This explains the queued self-hosted CI check despite an online
`lsbx-molimo` runner; changing that organization policy requires an explicit
security decision.
