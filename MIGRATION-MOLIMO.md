# Migrating Molimo from `lufs-sandbox-server` (Python) to `lsbx` (Rust)

**Audience: an autonomous coding agent operating directly on the `molimo` host (the exe.dev-hosted service VM), as the `exedev` OS user.** This document is written to be executed, not just read. If any step below requires a privilege, credential, or piece of information you do not have, **stop and report the specific blocker — do not work around access controls, do not guess at a credential, do not skip verification to make progress.**

**Do not run `git reset --hard`, `git clean -fd`, or delete any untracked file on this host as part of this migration.** Operators leave live evidence outside version control on hosts like this one, per this project's own standing operating rule.

**A structurally important fact before you start**: Molimo also runs a separate, older, unrelated system — `lufs-runner@1.service`/`lufs-runner@2.service`, standing Docker-container-based GitHub Actions runners, predating the zero-idle broker this migration touches. **This migration does not touch `lufs-runner` at all.** Its source repo (`lufs-runner`) is explicitly out of scope per `lufs-sandbox-server`'s own `SPEC.md` ("Do not modify `exe-sandbox-provider` or `lufs-runner`; port concepts only"). Do not stop, disable, or otherwise interact with `lufs-runner@*.service` as part of this work, even though it shares the `exe` runner group with the system you *are* migrating. If you're unsure whether a given running process belongs to `lufs-runner` or to `lufs-sandbox-server`/`lsbx`, check before acting: `systemctl list-units 'lufs-runner*' 'lsbx*'`.

---

## 1. What this migration is

Molimo currently runs two long-running services, part of `lufs-audio/lufs-sandbox-server` (Python), invoking the **old** `lufs-sandbox` CLI/module:

| Service (systemd unit) | What it runs today | Purpose |
|---|---|---|
| `lsbx-gateway-exedev.service` | `.venv/bin/lufs-sandbox --images images.json --backend exedev gateway ...` | REST API on `100.122.170.73:8244`, also reverse-proxied publicly via exe.dev's own edge and via Molimo's own Caddy |
| `lsbx-ci-broker-exe.service` | `python -m lufs_sandbox.ci_broker` | Zero-idle GitHub Actions runner broker, GitHub App auth |

Unlike Carnyx, **Molimo has no stream-proxy service** — exe.dev's own edge proxy forwards ports 3000–9999 directly to each guest VM's public hostname, so there's no NAT-traversal problem for the exedev backend to solve locally the way the libvirt backend needs to.

You are replacing both with services from `lufs-audio/lsbx` (Rust):

| New service (you will create) | What it runs | Replaces |
|---|---|---|
| `lsbx-serve.service` | `/usr/local/bin/lsbx serve --host 100.122.170.73 --port 8244 ...` | `lsbx-gateway-exedev.service` |
| `lsbx-ci-broker-exe.service` | `/usr/local/bin/lsbx ci-broker run --backend=exedev` | `lsbx-ci-broker-exe.service` (same unit name — generated for you by `lsbx bootstrap`) |

**A real, load-bearing fact about this host you must not break:** Molimo runs **its own** general-purpose Caddy instance (`~/repos/molimo-proxy`) that does double duty — it fronts Molimo's own gateway for public HTTPS, and it *also* reverse-proxies **Carnyx's** gateway/stream-proxy (`molimo.exe.xyz:8246` → Carnyx `100.125.210.60:8243`, `:8247` → Carnyx `:8244`). This migration does not touch Carnyx or Molimo's Caddy config — but because your gateway's port (`8244`) and bind address (`100.122.170.73`) are not changing, the existing Caddy routes should keep working unmodified. Verify this in §9 rather than assuming it, and if you ever do need to touch `~/repos/molimo-proxy`'s config for an unrelated reason while this migration is in flight, know that a mistake there can take down Carnyx's public reachability too, not just Molimo's.

---

## 2. Architecture comparison

| Aspect | Old (Python, currently running) | New (Rust, `lsbx`) |
|---|---|---|
| Binary | `.venv/bin/lufs-sandbox`, `.venv/bin/python -m lufs_sandbox.ci_broker` | single `lsbx` binary |
| Repo | `lufs-audio/lufs-sandbox-server`, checked out at `/home/exedev/repos/lufs-sandbox-server` | `lufs-audio/lsbx`, recommend `/home/exedev/repos/lsbx` as a sibling checkout |
| Config | Environment variables via systemd `EnvironmentFile=` | CLI flags, a subset also settable via `LSBX_*` env vars — verify exact support with `lsbx <subcommand> --help`, don't assume name parity with the old system |
| State directory | `/home/exedev/.lufs-sandbox/state` | your choice via `--state-dir`; do not point it at the old directory (schemas are not proven byte-compatible — see the Carnyx doc §5 for the full reasoning, identical here) |
| Registry file | `/home/exedev/repos/lufs-sandbox-server/images.json` (the un-suffixed default — Molimo does **not** use `images.carnyx.json`) | same schema, same file, copy it forward |
| Golden images | No local files — exe.dev "goldens" are named VMs in exe.dev's own control plane (`lsbx-default-v1`, `lsbx-web-v1`, `lsbx-ci-v1`, plus legacy `agent-default-v1`/`agent-web-v1` still present pending lease drain) | same naming/reference scheme — nothing to copy on disk, but see §6.2 for what to actually verify |
| Backend control plane | SSH to exe.dev, or `EXE_TOKEN`-authenticated HTTPS to `exe.dev/exec` | same two paths, same credential story |
| Gateway auth | Bearer token + `--insecure` | same model |
| GitHub auth (CI broker) | GitHub App JWT auth, **unconditionally live** (`GITHUB_APP_ID=4377007` uncommitted/active in the real env file) — Molimo has no working `gh` CLI fallback (its `gh auth` session was reported invalid at last audit) | same GitHub App (`LSBX_GITHUB_APP_ID`/`LSBX_GITHUB_APP_PRIVATE_KEY_PATH`/`LSBX_GITHUB_APP_OWNER`) — **this path is not optional for Molimo the way it is for Carnyx; confirm `gh auth status` before assuming a fallback exists as a safety net** |
| Process supervision | systemd, `Restart=on-failure` | same. `--daemon` on the new `lsbx serve` does not fork/background — systemd remains solely responsible, identical to today |
| Reaper | separate `--reap-interval`/`--reap-ttl` flags | internal background task, interval derived from `reap_ttl`, no separate flag |
| `EXE_TOKEN` handling | Old gateway unit explicitly `UnsetEnvironment=EXE_TOKEN` to force the SSH control-plane path rather than risk an expired token blocking cloud provisioning | **verify whether the new `lsbx-backend-exedev` crate has an equivalent fallback-preference concept; if it always prefers `EXE_TOKEN` when present with no override, and your token is stale, this is a real functional regression risk specific to this host — check `crates/lsbx-backend-exedev`'s real source for a `fallback_ssh_key_path`/equivalent option before cutover, don't assume the old unit's workaround has a new-system equivalent by default** |

---

## 3. Pre-flight inventory (run this first, change nothing yet)

### 3.1 Confirm what's actually running right now

```bash
systemctl status lsbx-gateway-exedev.service lsbx-ci-broker-exe.service --no-pager
systemctl is-enabled lsbx-gateway-exedev.service lsbx-ci-broker-exe.service
systemctl status 'lufs-runner@*.service' --no-pager   # confirm these are running too, and that you understand they are OUT OF SCOPE — see §1
```

### 3.2 Confirm current sandbox/job load

```bash
cd /home/exedev/repos/lufs-sandbox-server
.venv/bin/lufs-sandbox --backend exedev --images images.json --state-dir /home/exedev/.lufs-sandbox/state list --json
ls -la /home/exedev/.lufs-sandbox/state/ci-broker/ 2>/dev/null
```

Same guidance as the Carnyx document: decide and document whether to wait for natural lease/queue drain or force-clear before cutover; prefer migrating the CI broker last, once the queue is confirmed empty.

### 3.3 Baseline verification-suite health check

```bash
TOKEN=$(grep -oP '(?<=LUFSS_GATEWAY_TOKEN=).*' /home/exedev/repos/lufs-sandbox-server/.env 2>/dev/null || echo "")
curl -sf -H "Authorization: Bearer $TOKEN" http://100.122.170.73:8244/health
curl -sf https://molimo.exe.xyz:8243/health   # the public Caddy-proxied path — separate from the Carnyx-proxying ports 8246/8247, this one fronts Molimo's OWN gateway
```

Record both as your **before** baseline.

---

## 4. Backup — do this before touching anything

### 4.1 Snapshot all state, config, and registry files

```bash
BACKUP_DIR="/home/exedev/lsbx-migration-backup-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$BACKUP_DIR"
tar czf "$BACKUP_DIR/lufs-sandbox-state.tar.gz" -C /home/exedev/.lufs-sandbox state ci-broker-exe.env ci-runner-exe.env 2>/dev/null
cp /home/exedev/repos/lufs-sandbox-server/images.json "$BACKUP_DIR/"
sudo cp /etc/systemd/system/lsbx-gateway-exedev.service "$BACKUP_DIR/"
sudo cp /etc/systemd/system/lsbx-ci-broker-exe.service "$BACKUP_DIR/"
echo "Backup written to $BACKUP_DIR"
ls -la "$BACKUP_DIR"
```

**Do not** put `/home/exedev/.lufs-sandbox/lufs-audio-ci-app.pem` (the live GitHub App private key) into any tarball that leaves this host or gets committed anywhere. Confirm its permissions are still `0600` and its owner is `exedev`, and leave it in place — the new system will reference the same file by path, not a copy (§7.4).

### 4.2 Record the GitHub auth state precisely

```bash
grep -E '^(GITHUB_APP_ID|GITHUB_APP_KEY|GITHUB_INSTALLATION_ID|GITHUB_OWNER|GITHUB_REPO)=' /home/exedev/.lufs-sandbox/ci-broker-exe.env
sudo -u exedev gh auth status   # run as the exedev user specifically, not your own login — the last known audit found this session invalid
```

Unlike Carnyx, this host is expected to have live App credentials — confirm the actual values (redact the key path's contents from any report you write, the path itself is fine to record) rather than assuming the example file's shape reflects the real, live file.

### 4.3 Confirm which exe.dev goldens are actually live

```bash
cd /home/exedev/repos/lufs-sandbox-server
cat images.json | python3 -m json.tool
```

Cross-reference the `goldens[]` list against what's actually registered in exe.dev's own control plane (however this host's tooling queries that — check for an `exe` CLI or equivalent, or use whatever mechanism the old `backends/exedev.py` itself uses to list VMs, e.g. `.venv/bin/lufs-sandbox --backend exedev images` should at minimum confirm the registry parses; a live inventory check against exe.dev itself is a stronger confirmation if a tool for it exists on this host). Note explicitly whether the legacy `agent-default-v1`/`agent-web-v1` goldens (flagged in this project's own history as "slated for retirement after all leases drain") are still present, and do not assume they're safe to ignore — a migration that starts using the new registry file unchanged will still reference them if they're still in `images.json`.

---

## 5. Build and install the new `lsbx` binary

### 5.1 Toolchain

```bash
command -v cargo || curl https://sh.rustup.rs -sSf | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version   # verified against 1.98.0; `rustup update` if materially older, don't work around a real compile error by downgrading a dependency
```

Molimo's exedev backend does **not** need libvirt headers — you can skip the `pkg-config --exists libvirt` check that matters on Carnyx, but the build still compiles the `lsbx-backend-libvirt` crate as part of the full workspace (it's one Cargo workspace, not per-backend builds). Confirm the same headers requirement anyway if the build fails on that crate specifically:

```bash
pkg-config --exists libvirt && echo "found" || echo "MISSING — even though this host doesn't need libvirt at runtime, cargo build --workspace still compiles that crate; install the dev headers or narrow the build to skip it (cargo build --release -p lsbx-cli and its actual dependency closure) if you'd rather not install libvirt-dev on a host that will never use it"
```

If you deliberately choose to skip libvirt-dev on this host, build only what Molimo actually runs rather than the full workspace:

```bash
cargo build --release -p lsbx-cli -p lsbx-broker
```

(`lsbx-cli` pulls in `lsbx-backend-libvirt` as a normal dependency for its `--backend auto`/`--backend libvirt` support regardless — confirm whether a narrower build is actually possible before assuming it is; if `lsbx-cli` unconditionally depends on the libvirt backend crate, you'll need the dev headers here too, and that's fine, just document why.)

### 5.2 Clone and build

```bash
mkdir -p /home/exedev/repos
cd /home/exedev/repos
git clone https://github.com/lufs-audio/lsbx.git
cd lsbx
cargo build --release --workspace   # or the narrower build from §5.1 if you went that route
./target/release/lsbx --version
./target/release/lsbx --help
```

### 5.3 Install

```bash
sudo install -m 755 target/release/lsbx /usr/local/bin/lsbx
lsbx --version
```

### 5.4 Run the real verification gate on this host

```bash
cargo test --workspace 2>&1 | tail -40
```

This host is the right place to run the exedev-specific ignored test:

```bash
EXE_TOKEN="$(grep -oP '(?<=EXE_TOKEN=).*' /home/exedev/repos/lufs-sandbox-server/.env 2>/dev/null)" \
  cargo test --workspace -- --ignored --test-threads=1 exedev_backend_passes_conformance_suite 2>&1 | tail -60
```

If this needs an `EXE_TOKEN` env var and none is set (or the one in the old `.env` is the stale one the old gateway unit explicitly avoids relying on — see §2's `EXE_TOKEN` row), check whether the test can run via the SSH control-plane path instead; if it can't run either way without a live credential you don't have, **report that specifically rather than skipping verification silently.**

---

## 6. Migrate persistent assets

### 6.1 Registry file

```bash
cp /home/exedev/repos/lufs-sandbox-server/images.json /home/exedev/repos/lsbx/images.json
/usr/local/bin/lsbx --images /home/exedev/repos/lsbx/images.json --backend exedev images
/usr/local/bin/lsbx --images /home/exedev/repos/lsbx/images.json --backend exedev profiles --full
```

Same guidance as Carnyx: if either command errors, read the actual error before concluding it's a real schema incompatibility versus an environment/path issue.

### 6.2 Golden VMs — verify by name, not by file

Because exe.dev goldens are remote named VMs, not local files, the equivalent of Carnyx's "verify the qcow2 path convention" step here is:

```bash
/usr/local/bin/lsbx --images /home/exedev/repos/lsbx/images.json --backend exedev golden verify agent-base
```

This should reach out over the exedev backend's real control plane (SSH or `EXE_TOKEN`-HTTPS) and confirm the named golden VM (`lsbx-default-v1` per `agent-base.base` in `images.json`) actually exists and responds. If it can't reach exe.dev at all, that's a credential/connectivity problem to solve before proceeding, not a registry problem — don't edit the registry file to work around a connectivity failure.

### 6.3 Do NOT migrate live sandbox/CI-job state across the schema boundary

Identical reasoning to the Carnyx document §6.3: the old and new on-disk sandbox-record schemas are not proven byte-compatible. Let existing state drain under the old system; start the new system with a fresh state directory.

---

## 7. Configure and stand up the new system (side by side, not yet live)

### 7.1 Bootstrap

```bash
cd /home/exedev/repos/lsbx
/usr/local/bin/lsbx bootstrap --target /home/exedev/lsbx-state --dry-run
```

**Expect the host-verification step to report the libvirt-socket check as failed** — that's correct and expected on this host (Molimo has no libvirt at all), not a bug to fix. If `bootstrap` treats a failed libvirt check as fatal even on a host that will only ever run `--backend exedev`, this is worth flagging explicitly: check `verify_host()`'s real behavior (`crates/lsbx-bootstrap/src/verify_host.rs` in your clone) to see whether it's possible to skip that one check, or whether you need `--no-verify` here specifically because of this host/backend mismatch. Document whichever is true rather than silently passing `--no-verify` without understanding why it was necessary.

```bash
sudo /usr/local/bin/lsbx bootstrap --target /home/exedev/lsbx-state [--no-verify if the above investigation confirms it's needed]
```

This writes both `/etc/systemd/system/lsbx-ci-broker.service` and `lsbx-ci-broker-exe.service`. **You only need the `-exe` variant on this host** — leave the plain `lsbx-ci-broker.service` disabled or remove it, your call, document which.

### 7.2 Confirm the generated CI-broker unit content

```bash
cat /etc/systemd/system/lsbx-ci-broker-exe.service
```

Should contain `ExecStart=/usr/local/bin/lsbx ci-broker run --backend=exedev`. Adjust the binary path if you installed it somewhere other than `/usr/local/bin/lsbx`.

### 7.3 Author the `lsbx-serve` unit by hand

```bash
sudo tee /etc/systemd/system/lsbx-serve.service > /dev/null <<'EOF'
[Unit]
Description=lsbx HTTP gateway (Molimo / exe.dev backend)
After=network-online.target caddy.service
Wants=network-online.target

[Service]
Type=simple
User=exedev
Group=exedev
WorkingDirectory=/home/exedev/repos/lsbx
EnvironmentFile=/home/exedev/lsbx-state/serve.env
UnsetEnvironment=EXE_TOKEN
ExecStart=/usr/local/bin/lsbx --backend exedev --images /home/exedev/repos/lsbx/images.json --state-dir /home/exedev/lsbx-state serve --host 100.122.170.73 --port 8244 --reap-ttl 3600h --insecure
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ReadWritePaths=/home/exedev/lsbx-state
EOF
```

Notes:
- Hardening directives (`NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=full`, `ReadWritePaths`) are carried forward from the **old** `lsbx-gateway-exedev.service` — Carnyx's old gateway unit didn't have these, but Molimo's did; preserve that asymmetry deliberately rather than "fixing" it to match Carnyx, unless you have a specific reason to change Molimo's security posture as part of this migration (you almost certainly don't — flag it if you think otherwise, don't just harmonize the two hosts silently).
- `ReadWritePaths` must include wherever `--state-dir` actually points, or the gateway will fail to write sandbox records under the `ProtectSystem=full` sandboxing — this is exactly the kind of thing that passes a naive smoke test run interactively (outside systemd's sandboxing) and then fails only once actually started via `systemctl start`, so **test via `systemctl start`, not by running the binary by hand**, before declaring this unit correct.
- The `EXE_TOKEN`-avoidance workaround from the old unit (`UnsetEnvironment=EXE_TOKEN`) is **not** carried forward automatically here — resolve the §2 open question about whether the new exedev backend has an equivalent preference/override before deciding whether you need something similar in this new unit's `[Service]` section.
- Create `serve.env` with a real bearer token, mode 0600, owner `exedev:exedev`, same pattern as Carnyx's doc §7.3.

```bash
echo "LSBX_GATEWAY_TOKEN=$(openssl rand -hex 32)" > /home/exedev/lsbx-state/serve.env
chmod 600 /home/exedev/lsbx-state/serve.env
sudo systemctl daemon-reload
```

### 7.4 Configure GitHub auth for the CI broker

Molimo's App credentials are expected to already be live (§4.2). Wire them into a drop-in override, referencing the **existing** PEM file by path rather than copying it:

```bash
sudo mkdir -p /etc/systemd/system/lsbx-ci-broker-exe.service.d
sudo tee /etc/systemd/system/lsbx-ci-broker-exe.service.d/override.conf > /dev/null <<EOF
[Service]
Environment=LSBX_GITHUB_APP_ID=<value from old GITHUB_APP_ID, confirmed in §4.2>
Environment=LSBX_GITHUB_APP_PRIVATE_KEY_PATH=/home/exedev/.lufs-sandbox/lufs-audio-ci-app.pem
Environment=LSBX_GITHUB_APP_OWNER=lufs-audio
Environment=LSBX_QUEUE_LABEL=lsbx-default,lsbx-molimo
EOF
sudo systemctl daemon-reload
```

Preserve the local-first/cloud-fallback asymmetry: Molimo keeps the **60-second** fallback delay (not Carnyx's 0) — confirm the new `lsbx-broker` crate's actual env var name for this (`crates/lsbx-broker/src/poll.rs`'s real `PollConfig::from_queue_label_and_env`, in your clone) before assuming it's unchanged from the old `LSBX_CI_FALLBACK_DELAY`, and set it explicitly rather than relying on whatever default the new code ships with matching this host's specific required value by coincidence.

**Since Molimo has no working `gh` CLI fallback (per §4.2 / this project's own last-known audit), a broker that starts without valid App credentials here will not silently degrade to a working state the way Carnyx's might — it will simply fail to authenticate.** Treat a `LSBX_GITHUB_APP_ID` misconfiguration on this host as a hard-stop, not a "try it and see" situation.

---

## 8. Parallel verification — before touching the old services

```bash
/usr/local/bin/lsbx --backend exedev --images /home/exedev/repos/lsbx/images.json --state-dir /home/exedev/lsbx-state serve --host 127.0.0.1 --port 18244 --token test-token-do-not-use-in-prod &
SERVE_PID=$!
sleep 2
curl -sf -H "Authorization: Bearer test-token-do-not-use-in-prod" http://127.0.0.1:18244/health
```

Full functional round trip:

```bash
/usr/local/bin/lsbx --backend exedev --images /home/exedev/repos/lsbx/images.json --state-dir /home/exedev/lsbx-state up agent-base --lease 10m
# note $SBX
/usr/local/bin/lsbx --backend exedev --state-dir /home/exedev/lsbx-state exec $SBX -- echo hello-from-new-lsbx
/usr/local/bin/lsbx --backend exedev --state-dir /home/exedev/lsbx-state info $SBX --json
/usr/local/bin/lsbx --backend exedev --state-dir /home/exedev/lsbx-state down $SBX
kill $SERVE_PID
```

Every step must succeed before proceeding — an exedev `up` failure here most likely means an auth/connectivity problem with exe.dev itself (SSH key, `EXE_TOKEN`), not a Rust-vs-Python behavior difference; diagnose accordingly.

---

## 9. Benchmark: old vs. new

Same methodology as the Carnyx document — measure both systems back to back on this host.

| Metric | Old command | New command |
|---|---|---|
| Sandbox create latency | `time .venv/bin/lufs-sandbox --backend exedev up agent-base` | `time /usr/local/bin/lsbx --backend exedev up agent-base` |
| Sandbox destroy latency | `time .venv/bin/lufs-sandbox down $ID` | `time /usr/local/bin/lsbx down $ID` |
| Exec round-trip | `time .venv/bin/lufs-sandbox exec $ID -- echo hi` | `time /usr/local/bin/lsbx exec $ID -- echo hi` |
| `list` latency | `time .venv/bin/lufs-sandbox list --json` | `time /usr/local/bin/lsbx list --json` |
| Gateway `/health` latency (100 req) | `hey -n 100 -H "Authorization: Bearer $TOKEN" http://100.122.170.73:8244/health` | same shape against the port-18244 test instance |
| Idle memory | `systemctl show lsbx-gateway-exedev.service -p MemoryCurrent` | `systemctl show lsbx-serve.service -p MemoryCurrent` |
| Idle CPU (5 min) | `pidstat -p $(pgrep -f lufs_sandbox) 5 60` | `pidstat -p $(pgrep -f 'lsbx serve') 5 60` |
| Binary cold-start | `time .venv/bin/lufs-sandbox --help` | `time /usr/local/bin/lsbx --help` |
| Exedev control-plane round trip specifically (SSH or HTTPS latency to exe.dev is likely the dominant cost, not the local binary — measure it explicitly so you don't misattribute network latency to the rewrite) | time a bare `ssh`/`curl` call to exe.dev's control plane directly, outside either CLI | same bare call — this number should be nearly identical between old and new, since neither rewrite changes exe.dev's own response time; use it as a control/sanity-check on your other numbers |

Record results in the same `Metric | Old | New | Speedup` table format as the Carnyx doc, and include the same directional-expectations caveat: cold-start/short-command metrics should favor the compiled binary clearly; anything dominated by exe.dev's own network round trip should show little to no difference, and a large apparent "speedup" on an exe.dev-bound operation is more likely a measurement artifact (e.g. a warm SSH connection reused) than a real improvement — sanity-check against the control measurement above before reporting a surprising number as fact.

---

## 10. Cutover

Only proceed once §8 and §9 are complete and recorded, and the §3.2 live-load decision has been honored.

```bash
sudo systemctl stop lsbx-ci-broker-exe.service
sudo systemctl stop lsbx-gateway-exedev.service

sudo systemctl disable lsbx-ci-broker-exe.service lsbx-gateway-exedev.service

sudo systemctl enable lsbx-serve.service lsbx-ci-broker-exe.service
sudo systemctl start lsbx-serve.service
sudo systemctl start lsbx-ci-broker-exe.service

sleep 3
systemctl status lsbx-serve.service lsbx-ci-broker-exe.service --no-pager
journalctl -u lsbx-serve.service -u lsbx-ci-broker-exe.service --since "2 minutes ago" --no-pager
```

Do not touch `lufs-runner@*.service` at any point in this sequence (§1).

If either new service fails to start or crash-loops, execute the rollback in §11 immediately rather than debugging live.

---

## 11. Post-cutover verification suite

```bash
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" http://100.122.170.73:8244/health
curl -sf https://molimo.exe.xyz:8243/health   # Molimo's own public Caddy-fronted path
```

Full functional round trip via the live gateway's REST API (not just direct CLI):

```bash
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" -X POST http://100.122.170.73:8244/sandboxes -d '{"profile":"default"}' -H 'Content-Type: application/json'
```

CI broker smoke test against the real chaos-test workflow, Molimo placement:

```bash
gh workflow run ci-broker-failure-test.yml -f placement=lsbx-molimo --repo lufs-audio/lufs-sandbox-server
```

Confirm a sandbox is created within `LSBX_CI_POLL_INTERVAL` seconds, a runner registers, picks up the intentionally-failing job, and the sandbox is torn down cleanly afterward.

**Also explicitly re-verify Carnyx's proxied reachability is unaffected**, since you didn't touch Carnyx but your host fronts its public exposure:

```bash
curl -i https://molimo.exe.xyz:8246/health   # Carnyx's gateway, proxied through YOUR Caddy — should be completely unaffected by anything you did on Molimo today, confirm it actually is
```

If this last check fails, the regression is almost certainly unrelated to anything in this document (you didn't touch Caddy config) — but confirm that before ruling it out, since a coincidental network blip during your own maintenance window is exactly the kind of thing that gets wrongly blamed on or wrongly cleared of an unrelated change if you don't check explicitly.

---

## 12. Document your evidence

Write a dated evidence file (e.g. `docs/molimo-lsbx-rust-migration-<date>.md`) containing the §3 baseline, the §4.2/§4.3 findings, the §9 benchmark table, and the §11 post-cutover results — including the Carnyx-proxy cross-check.

---

## 13. Rollback plan

**Trigger conditions:**
- Either new service fails to start, or crash-loops more than twice in 5 minutes.
- The post-cutover `/health` checks (either Molimo's own or the Carnyx cross-check) fail.
- A real sandbox create/exec/destroy round trip fails against the live new gateway.
- The CI broker smoke test does not result in a job being picked up within 2× the configured poll interval — treat this as more urgent on Molimo than on Carnyx, since there is no working `gh`-CLI fallback here to soften a GitHub-App-auth misconfiguration.

**Rollback procedure:**

```bash
sudo systemctl stop lsbx-serve.service lsbx-ci-broker-exe.service
sudo systemctl disable lsbx-serve.service

sudo cp "$BACKUP_DIR/lsbx-ci-broker-exe.service" /etc/systemd/system/lsbx-ci-broker-exe.service
sudo systemctl daemon-reload

sudo systemctl enable lsbx-gateway-exedev.service lsbx-ci-broker-exe.service
sudo systemctl start lsbx-gateway-exedev.service
sudo systemctl start lsbx-ci-broker-exe.service

sleep 3
systemctl status lsbx-gateway-exedev.service lsbx-ci-broker-exe.service --no-pager
curl -sf -H "Authorization: Bearer $TOKEN" http://100.122.170.73:8244/health
```

As with Carnyx, rollback here is purely a matter of stopping the new units and restarting the untouched old ones — the old state directory, registry file, and App credential PEM were never modified by this migration.

---

## 14. Decommissioning the old system (only after a soak period)

Same minimum-one-week soak recommendation as the Carnyx document, including a real CI job cycle.

- **Keep**, indefinitely: `images.json`, the App credential PEM (`/home/exedev/.lufs-sandbox/lufs-audio-ci-app.pem` — never delete a live credential as part of a "cleanup"), the `$BACKUP_DIR` tarball.
- **Keep for at least 30 days**, then re-evaluate: the old `/home/exedev/repos/lufs-sandbox-server` checkout, the old `/home/exedev/.lufs-sandbox/state` directory.
- **May disable (not delete) once stable**: the old unit files.
- **Never touch**: `lufs-runner@*.service` or anything under its own separate config — out of scope for this entire migration, start to finish (§1).
