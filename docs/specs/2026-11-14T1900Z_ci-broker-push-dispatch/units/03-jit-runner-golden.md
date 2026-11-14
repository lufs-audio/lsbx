# Unit 03 — jit-runner-golden

## Objective
The Linux CI golden boots a runner from a JIT config delivered by the broker, and no GitHub App key is ever uploaded to a guest.

## Context
- Today: `crates/lsbx-broker/src/reconcile.rs::prepare_runner_upload` writes `lsbx-runner-app.pem` + env into the upload dir; the guest runs `scripts/provision-ci-runner.sh` (JWT → installation token → registration token → `config.sh --ephemeral`).
- Target: broker (or the unit-04 listener) calls `GenerateJitRunnerConfig` host-side; the guest receives one opaque string and runs `run.sh --jitconfig "$JIT"` as the runner user. JIT configs are single-use and bound to one runner.
- Goldens: `images.carnyx.json`, `/home/carnyx/ISOs/images/goldens/lsbx-ci-v1.qcow2` (4 vCPU / 8 GB).

## Acceptance criteria
- [ ] New guest script `scripts/run-jit-runner.sh` reads the JIT config from a file (mode 0400, deleted after read) and execs `run.sh --jitconfig`; exits non-zero with a clear message if the runner binary is older than the JIT-capable version.
- [ ] `prepare_runner_upload` gains a JIT mode that writes **only** the JIT file + non-secret env; the PEM-copy path is behind an explicit legacy flag, off by default for scale-set dispatch.
- [ ] Unit test: in JIT mode the upload directory contains no file matching `*.pem` and no `GITHUB_APP_KEY` env assignment.
- [ ] The golden ships a runner version that supports `--jitconfig` (version recorded in `images.carnyx.json` notes).
- [ ] One job completes via the unit-02 spike listener using this script.

## Interface contract
```bash
# guest
/opt/lsbx/run-jit-runner.sh /tmp/lsbx-ci-broker/jitconfig   # file contains the base64 JIT config
```

## Boundaries
- **Touches:** `scripts/run-jit-runner.sh` (new), `crates/lsbx-broker/src/reconcile.rs` (upload set), golden build recipe for the CI image, `images.carnyx.json`.
- **Does NOT touch:** `poll.rs`, Windows golden (unit 05), production service files.

## Verification
```bash
cargo test -p lsbx-broker prepare_runner_upload
grep -rn 'pem' crates/lsbx-broker/src/reconcile.rs
```
