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

**Status:** Phase 1 (speccing) complete. Not yet implemented — see
[`SPEC.md`](SPEC.md) before writing any code against this repo.

## Start here

- [`SPEC.md`](SPEC.md) → the authoritative spec (in
  `docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/`)
- [`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/`](docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/)
  → the 20 disjoint unit-of-work contracts, dependency-ordered, each independently
  buildable and verifiable

## License

GPL-3.0, matching the other LUFS Primitive CLIs (`snuze`, `apho`, `lrex`).
