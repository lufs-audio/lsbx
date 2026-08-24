# Unit 20 — Workspace Manifest, CI Workflow & Compat Fixtures

## Objective
Wire the Cargo workspace root, author `lsbx`'s own CI workflow (pointed at the existing self-hosted fleet first, per SPEC.md Deviation 15), and land the real compatibility fixtures every earlier unit's tests reference.

## Context
Layer 8, the final unit — depends on everything else landing first, since it's the integration point. This is where "100% schema and functional compatibility" (the brief's own words) becomes a checked fact in CI rather than a claim in a commit message (SPEC.md §10).

## Acceptance criteria
- [ ] Workspace root `Cargo.toml` lists all 17 crates as members, with a shared `[workspace.package]` (edition, `license = "GPL-3.0"`, repository) and `[workspace.lints]` enforcing `-D warnings` at the workspace level so no individual crate can quietly opt out.
- [ ] `tests/fixtures/` contains real, byte-for-byte copies of `lufs-audio/lufs-sandbox-server`'s `images.json` and `images.carnyx.json`, plus at least one real legacy-flat `SandboxRecord` sample and one current-schema sample. Every unit that inlined a fixture literal (Units 01, 08) is updated to read from here instead, removing the duplication.
- [ ] `.github/workflows/ci.yml` runs `cargo check` / `clippy --all-targets --all-features -- -D warnings` / `cargo test --workspace` (excluding `--ignored` infrastructure-requiring tests) on `runs-on: [self-hosted, lufs]` — the existing always-on fleet, per Deviation 15 — with placement read from `vars.LSBX_CI_PLACEMENT` defaulting to `lufs` until the broker is verified live. This satisfies the brief's placement requirement as a config seam rather than a premature cutover.
- [ ] A comment in the workflow file plus a note in `README.md` documents the future cutover explicitly: once `lsbx-default` is live and verified, flipping `vars.LSBX_CI_PLACEMENT` to `"lsbx-default"` is the entire migration — no code change required. This unit documents that seam; it does not flip it.
- [ ] `AGENTS.md` exists at repo root, ported from the existing system's `AGENTS.md` (broker service names `lsbx-ci-broker`/`lsbx-ci-broker-exe`, env vars `LSBX_QUEUE_LABEL`/`RUNNER_LABELS`/`LUFSS_VM_PREFIX`), updated for the new crate layout.
- [ ] `CHANGELOG.md` initialized in Keep a Changelog format with a `0.1.0-unreleased` entry summarizing the rewrite.
- [ ] Every unit's own `cargo test -p <crate>` from Units 01–19 still passes unmodified in behavior after the fixture-path updates — this unit changes no production behavior, only wires the final integration.

## Interface contract
```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = [
    "crates/lsbx-kernel", "crates/lsbx-store", "crates/lsbx-keys", "crates/lsbx-backend-testkit",
    "crates/lsbx-backend-demo", "crates/lsbx-backend-libvirt", "crates/lsbx-backend-exedev",
    "crates/lsbx-golden", "crates/lsbx-lifecycle", "crates/lsbx-ops",
    "crates/lsbx-cli", "crates/lsbx-tui", "crates/lsbx-gateway", "crates/lsbx-stream", "crates/lsbx-mcp",
    "crates/lsbx-broker", "crates/lsbx-bootstrap",
]

[workspace.package]
edition = "2024"
license = "GPL-3.0"
repository = "https://github.com/lufs-audio/lsbx"

[workspace.lints.rust]
unsafe_code = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
```
```yaml
# .github/workflows/ci.yml (excerpt — the placement line the brief asked for)
jobs:
  build:
    runs-on: [self-hosted, "${{ vars.LSBX_CI_PLACEMENT || 'lufs' }}"]
    # Cutover note: once lsbx's own broker is deployed and verified serving
    # the `lsbx-default` label, set the repo variable LSBX_CI_PLACEMENT to
    # "lsbx-default". No workflow file change required — see SPEC.md Deviation 15.
```

## Boundaries — do NOT touch
Does not modify any crate's production source — only adds workspace-level config, fixtures, and repo-meta files, and updates test files to read fixtures from `tests/fixtures/` instead of inline literals. Does not flip `vars.LSBX_CI_PLACEMENT` to `"lsbx-default"` — that is a deliberate, later, human-triggered action once the broker is verified live, not something this unit performs.

## Output
- `Cargo.toml` (workspace root)
- `.github/workflows/ci.yml`
- `tests/fixtures/images.json`
- `tests/fixtures/images.carnyx.json`
- `tests/fixtures/sandbox_record_legacy_flat.json`
- `tests/fixtures/sandbox_record_current.json`
- `AGENTS.md`
- `CHANGELOG.md`

## Verification
```bash
cargo check --workspace --message-format=json
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace -- --ignored   # run separately, on a host with real libvirt/exe.dev access
```
Scenario: after this unit lands, `cargo test -p lsbx-golden --test test_registry_schema` (Unit 08) must still pass reading `tests/fixtures/images.json`/`images.carnyx.json` instead of an inlined literal, with the `agent-base` mismatch assertion unchanged and still passing.
