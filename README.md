# lsbx

A pluggable, dual-backend disposable-VM engine and zero-idle GitHub Actions CI
runner broker, for LUFS Audio.

`lsbx` provisions short-lived, ephemeral virtual machines on demand — for CI
runners, sandboxed agent execution, and interactive dev/demo environments —
runs work inside them over four doors (CLI, HTTP, WebSocket console, MCP),
and tears them down. It is the ground-up Rust rewrite of
[`lufs-audio/lufs-sandbox-server`](https://github.com/lufs-audio/lufs-sandbox-server),
which remains the read-only reference for existing behavior and on-disk
schemas this rewrite must not silently break.

**Status:** Implemented and in production. The 17-crate workspace is complete
(spec: [`SPEC.md`](SPEC.md), 20 unit contracts under
`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/`); the Rust gateway and
CI broker cut over on Molimo on 2026-08-27 (see
`docs/molimo-lsbx-rust-migration-2026-08-27.md`) and on Carnyx the same week.

## The exe.dev backend

The `lsbx-backend-exedev` crate runs VMs on [exe.dev](https://exe.dev) over
two co-equal transports, selected by auth mode:

| Auth mode | Config | Transport |
|---|---|---|
| `AccountToken` | `EXE_TOKEN` (or `LSBX_EXEDEV_TOKEN_ENV` naming the var) | HTTPS `POST https://exe.dev/exec` — control verbs + guest exec |
| `VmScopedToken` | a `v0@VM.exe.xyz` token | Same HTTPS path; the lobby scopes `ssh <vm>` |
| `Ssh` | `LSBX_EXEDEV_SSH_KEY` (private key path) | SSH via `russh` |
| `SshAlias` | `LSBX_EXEDEV_SSH_ALIAS` (default `exe.dev`) | SSH via `russh` |

Token auth (HTTPS) is fully self-sufficient: every account-level verb
(`ls --json`, `cp`, `tag`, `ssh-key add/remove`) **and** guest command
execution work over one bearer token — guest commands ride
`ssh <vm> <cmd>` with an in-band `__LSBX_EXIT:$?` exit sentinel (the
`X-Exe-Exit` trailer is not exposed through proxy chains). The HTTPS path
has a ~30 s server-side cap and merges stderr into the response body, so
SSH remains the door for interactive tooling, file transfer, and long jobs.
This shape (verified live 2026-09-04, fixing #30/#31) is what lets a
cloud agent with no SSH key operate the same backend a local agent uses
over SSH.

Use `lsbx` for sandbox lifecycle; raw ssh-over-exec is the fallback surface
for hosts without `lsbx` installed.

## Start here

- [`AGENTS.md`](AGENTS.md) → operating contract for agents (CI authoring,
  broker ops, safety rails)
- [`SPEC.md`](SPEC.md) → the authoritative spec (in
  `docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/`)
- [`CHANGELOG.md`](CHANGELOG.md) → what changed and when

## License

GPL-3.0, matching the other LUFS Primitive CLIs (`snuze`, `apho`, `lrex`).
