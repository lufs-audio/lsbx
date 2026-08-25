# Molimo `lsbx` migration evidence — August 25, 2026

## Scope

This records the preflight, backup, and side-by-side Rust verification pass. No
live Molimo service was cut over; the Python gateway, Python CI broker, Caddy,
and `lufs-runner@*.service` remain untouched.

## Preflight

- Host: `molimo`; recorded at `2026-08-25T16:43:53Z`.
- Disk: 50 GB filesystem, 28 GB free (41% used); Rust `target` was 1.4 GB.
- `lsbx-gateway-exedev.service`: active and enabled.
- `lsbx-ci-broker-exe.service`: active and enabled.
- `lufs-runner@1.service` and `lufs-runner@2.service`: active and enabled; out of scope.
- Caddy: active and enabled.
- Old sandbox inventory: empty (`[]`).
- Old CI broker state directory: empty.
- Old local gateway health: HTTP 200, `ok: true`.
- Public `https://molimo.exe.xyz:8243/health`: TLS failed with `wrong version number`; the listener is currently plain HTTP according to the host's Caddy configuration. Unauthenticated HTTP returned 401. This remains a deployment/exposure issue to resolve separately; Caddy was not changed here.
- GitHub App id: `4377007`; live PEM path retained at `/home/exedev/.lufs-sandbox/lufs-audio-ci-app.pem`, owner/mode `exedev:exedev`, `0600`. PEM contents were not copied or logged.
- `gh auth status` as `exedev`: invalid token; App auth is therefore required for the future Rust broker cutover.
- exe.dev control plane via the existing `exe.dev` SSH alias: reachable. Live registered golden VMs included `lsbx-default-v1`, `lsbx-web-v1`, and `lsbx-ci-v1`; no legacy `agent-*` golden was observed.

Full redacted preflight output is preserved outside Git at:
`/home/exedev/lsbx-migration-backup-20260825T164403Z/preflight.txt`.

## Backup

Backup created before code changes:

`/home/exedev/lsbx-migration-backup-20260825T164403Z`

It contains the old state/config archive, `images.json`, both old systemd unit
files, the redacted preflight report, and SHA-256 checksums. The GitHub App PEM
was deliberately excluded.

## Implemented migration fixes

- Added a fail-closed-compatible `lsbx serve --insecure` flag.
- Wired gateway token precedence: CLI token, `LSBX_GATEWAY_TOKEN`, then legacy `LUFSS_GATEWAY_TOKEN`.
- Added legacy path compatibility for `LUFSS_*` state/images/backend settings.
- Resolved user profiles to their registered golden base and inherited flavor, streaming, resource, and healthcheck metadata.
- Reconciled exe.dev provisioning with the Python protocol: `cp`, metadata-derived host, `tag`, ephemeral `ssh-key add/remove`, OpenSSH alias fallback, bounded SSH/SCP, and persisted per-sandbox key association.
- Added file transfer support, including directory uploads used by CI provisioning.
- Made generated broker units include user, working directory, state/images paths, environment file, hardening, and explicit backend flags.
- Fixed the workspace test-gate blockers in gateway integration wiring and the non-portable stdin negative-control test.
- Added the Rust `images.json` asset copied from the reference repository.

## Side-by-side verification

Using a fresh state directory and the existing `exe.dev` SSH alias:

- Rust `golden verify agent-base --json`: passed all four checks (`git`, Python, curl, and web-search); no verification VM remained afterward.
- Fresh Rust `up default` against exe.dev: passed registry healthchecks.
- Cross-process Rust `exec`, `info`, and `down`: passed.
- Cross-process Rust file `put`, `exec`, and `get`: passed; downloaded content matched.
- Final temporary VM inventory: empty.


Additional side-by-side work completed after the initial evidence capture:

- Built the complete release workspace and installed `/usr/local/bin/lsbx` (`lsbx 0.1.0`).
- Bootstrapped fresh `/home/exedev/lsbx-state` with 0700 state subdirectories; no old state was reused.
- Created mode-0600, `exedev`-owned `serve.env` and `broker.env`; the broker file references the existing PEM path and contains no PEM material.
- Authored and daemon-reloaded `/etc/systemd/system/lsbx-serve.service`, left disabled and stopped because the old gateway still owns port 8244.
- Verified the generated broker unit in an isolated unit directory: it contains `User=exedev`, the Rust working directory, explicit images/state paths, broker environment file, hardening, and `--backend=exedev`.
- Ran the Rust gateway side-by-side on `127.0.0.1:18244` as `exedev`; authenticated health, REST create, REST exec, REST delete, and final remote inventory all passed.

## Current status

This branch is **not a cutover approval**. The Rust gateway and broker still need
an installed side-by-side service rehearsal, real GitHub App broker chaos-test,
benchmark evidence, and the controlled cutover/post-cutover checks in
`MIGRATION-MOLIMO.md`. The old services remain the active production path.
