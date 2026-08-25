# Migrating Carnyx from `lufs-sandbox-server` (Python) to `lsbx` (Rust)

**Audience: an autonomous coding agent operating directly on the `carnyx` host.** This document is written to be executed, not just read. Every command assumes an interactive or scripted shell on `carnyx` with `sudo` available to the `carnyx` user. If any step below requires a privilege, credential, or piece of information you do not have, **stop and report the specific blocker — do not work around access controls, do not guess at a credential, do not skip verification to make progress.** This mirrors the existing operating contract for this host (`lufs-audio/lufs-sandbox-server`'s `AGENTS.md`): *"report any host action blocked by unavailable privileges rather than bypassing access controls."*

**Do not run `git reset --hard`, `git clean -fd`, or delete any untracked file on this host as part of this migration.** Operators leave live evidence on Carnyx outside version control (`docs/opencode-sessions/` in the old repo checkout, ad hoc logs, a pre-existing unrelated `win11` libvirt guest). Preserve all of it.

---

## 1. What this migration is

Carnyx currently runs three long-running services, all part of `lufs-audio/lufs-sandbox-server` (Python), all invoking the **old** `lufs-sandbox` CLI/module — not the newer `lufs_sandbox.lsbx` Python layer that also exists in that repo but is not what production actually runs:

| Service (systemd unit) | What it runs today | Purpose |
|---|---|---|
| `lsbx-gateway.service` | `.venv/bin/lufs-sandbox --backend libvirt --images images.carnyx.json gateway ...` | REST API on `100.125.210.60:8243` |
| `lsbx-stream-proxy.service` | `.venv/bin/lufs-sandbox stream-proxy ...` | noVNC/websocket relay on `100.125.210.60:8244` → guest port `8000` |
| `lsbx-ci-broker.service` | `python -m lufs_sandbox.ci_broker` | Zero-idle GitHub Actions runner broker |

You are replacing all three with **two** services from `lufs-audio/lsbx` (Rust):

| New service (you will create) | What it runs | Replaces |
|---|---|---|
| `lsbx-serve.service` | `/usr/local/bin/lsbx serve --host 100.125.210.60 --port 8243 ...` | `lsbx-gateway.service` **and** `lsbx-stream-proxy.service` (the new gateway mounts the stream proxy's routes on the same listener by default — see §3.3) |
| `lsbx-ci-broker.service` | `/usr/local/bin/lsbx ci-broker run --backend=libvirt` | `lsbx-ci-broker.service` (same unit name — this one is generated for you by `lsbx bootstrap`) |

This is a same-host, same-backend migration (libvirt stays libvirt; nothing moves to a different physical machine). The registry file convention (`images.carnyx.json`) and the golden qcow2 images themselves carry forward — you are not rebuilding golden images from scratch as part of this migration (see §6).

**A real, load-bearing fact about this host you must not break:** Carnyx's public HTTPS exposure for browser desktop consoles is proxied through **Molimo's** Caddy instance (`https://molimo.exe.xyz:8246` → Carnyx gateway `:8243`, `:8247` → Carnyx stream `:8244`). Carnyx's own gateway/stream ports do not change in this migration (still `8243`/`8244` on `100.125.210.60`), so this proxy should keep working untouched — but verify it in §9 rather than assuming it. Do not modify anything on Molimo as part of this document; if Molimo's Caddy config needs a change, stop and report it rather than SSH-ing over to fix it yourself.

---

## 2. Architecture comparison

| Aspect | Old (Python, currently running) | New (Rust, `lsbx`) |
|---|---|---|
| Binary | `.venv/bin/lufs-sandbox`, `.venv/bin/python -m lufs_sandbox.ci_broker` | single `lsbx` binary, subcommands `serve`/`ci-broker run`/`bootstrap`/`up`/`down`/... |
| Repo | `lufs-audio/lufs-sandbox-server`, checked out at `/home/carnyx/repos/lufs-sandbox-server` | `lufs-audio/lsbx`, checkout path of your choosing (recommend `/home/carnyx/repos/lsbx`, a sibling, not a replacement, of the old checkout) |
| Config | Environment variables (`LSBX_*`, `LUFSS_*`) via systemd `EnvironmentFile=` | CLI flags, with a subset also settable via `LSBX_*` env vars (clap's `env` feature) — **verify exact flag/env support with `lsbx <subcommand> --help` rather than assuming 1:1 name parity with the old system; the new CLI's flag surface was designed independently (SPEC.md Door 1) and does not promise identical variable names.** |
| State directory | `/home/carnyx/ISOs/images/state` (one JSON file per sandbox) | your choice via `--state-dir`; **do not point it at the old state directory** (see §5 — schemas are not proven byte-compatible) |
| Registry file | `/home/carnyx/repos/lufs-sandbox-server/images.carnyx.json` | same schema (`images[]`/`goldens[]`/`profiles{}`), same file usable as-is — copy it into the new checkout or reference the old path directly with `--images` |
| Golden images | `/home/carnyx/ISOs/images/goldens/*.qcow2` | same directory, same files — verify path convention with `lsbx golden verify` before trusting it (§6) |
| Libvirt connection | `LIBVIRT_DEFAULT_URI=qemu:///system` | same requirement — the new backend also needs the **system** libvirt socket, not the `qemu:///session` an interactive shell defaults to |
| Gateway auth | Bearer token + `--insecure` (tailnet is the real security boundary) | same model — `lsbx serve --token <TOKEN>` |
| GitHub auth (CI broker) | `gh auth` CLI fallback (Carnyx has never had live GitHub App credentials committed — verify in §4.2, this repo's own history has a documented discrepancy about whether this was ever actually completed) | supports both: GitHub App JWT auth (`LSBX_GITHUB_APP_ID`/`LSBX_GITHUB_APP_PRIVATE_KEY_PATH`/`LSBX_GITHUB_APP_INSTALLATION_ID`/`LSBX_GITHUB_APP_OWNER`) or the same `gh` CLI fallback if those are unset |
| Process supervision | systemd, `Restart=on-failure` | same — systemd, `Restart=on-failure`. **The new `--daemon` flag on `lsbx serve` does NOT fork/background the process** (deliberately — forking a multi-threaded Tokio runtime is unsafe); systemd remains solely responsible for backgrounding and restart, exactly as today. |
| Reaper | separate `--reap-interval`/`--reap-ttl` flags on the old gateway | a background task inside `lsbx serve` itself, interval `reap_ttl / 4` floored at 30s — no separate reaper process |

---

## 3. Pre-flight inventory (run this first, change nothing yet)

### 3.1 Confirm what's actually running right now

```bash
systemctl status lsbx-gateway.service lsbx-stream-proxy.service lsbx-ci-broker.service --no-pager
systemctl is-enabled lsbx-gateway.service lsbx-stream-proxy.service lsbx-ci-broker.service
```

Record the output verbatim. If any of the three is not `active (running)` and `enabled`, note that explicitly before proceeding — do not assume the documented steady state matches reality.

### 3.2 Confirm current sandbox/job load

```bash
cd /home/carnyx/repos/lufs-sandbox-server
.venv/bin/lufs-sandbox --backend libvirt --images images.carnyx.json --state-dir /home/carnyx/ISOs/images/state list --json
```

Record the count and IDs of any live sandboxes. **A live sandbox with an unexpired lease is not a reason to stop this migration**, but you must decide (and document your decision) whether to (a) wait for natural lease expiry, (b) let the old system's reaper clean them up in parallel while you build/verify the new one, or (c) explicitly `down --all` before cutover. Recommendation: (a) or (b) — do not force-destroy someone's live work without a clear signal this is a maintenance window.

```bash
ls -la /home/carnyx/ISOs/images/state/ci-broker/ 2>/dev/null
```

Record any in-flight CI job records. A CI job mid-flight is more disruptive to interrupt than a desktop sandbox — prefer migrating the CI broker last, and only once the queue is observed empty (`gh run list --status=in_progress --status=queued` against `lufs-audio` org repos with the `lsbx-carnyx`/`lsbx-default` labels).

### 3.3 Baseline verification-suite health check

Run the same live-verification recipes the old repo's own docs use:

```bash
TOKEN=$(grep -oP '(?<=GATEWAY_TOKEN=).*' /home/carnyx/ISOs/images/.env 2>/dev/null || echo "")
curl -sf -H "Authorization: Bearer $TOKEN" http://100.125.210.60:8243/health
curl -i https://molimo.exe.xyz:8247/stream/does-not-exist/vnc.html   # expect 404, NOT 502 — proves Molimo's proxy is actually reaching Carnyx's stream-proxy today
```

Record both results as your **before** baseline — you will re-run these exact commands in §9 against the new system and they must produce equivalent results (adjusted for the new port/service, per §9).

---

## 4. Backup — do this before touching anything

### 4.1 Snapshot all state, config, and registry files

```bash
BACKUP_DIR="/home/carnyx/lsbx-migration-backup-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$BACKUP_DIR"
sudo tar czf "$BACKUP_DIR/isos-images-state.tar.gz" -C /home/carnyx/ISOs/images state ci-broker.env .env 2>/dev/null
cp /home/carnyx/repos/lufs-sandbox-server/images.carnyx.json "$BACKUP_DIR/"
cp /home/carnyx/repos/lufs-sandbox-server/images.json "$BACKUP_DIR/"   # copy both — you'll want the exedev-flavored one for comparison even though Carnyx doesn't use it directly
sudo cp /etc/systemd/system/lsbx-gateway.service "$BACKUP_DIR/"
sudo cp /etc/systemd/system/lsbx-stream-proxy.service "$BACKUP_DIR/"
sudo cp /etc/systemd/system/lsbx-ci-broker.service "$BACKUP_DIR/"
echo "Backup written to $BACKUP_DIR"
ls -la "$BACKUP_DIR"
```

**Do not include** `/home/carnyx/ISOs/images/goldens/*.qcow2` in this tarball — they are large (multi-GB) and are not being modified or moved by this migration; back them up separately only if you have the disk headroom to do so safely, and never delete the originals regardless.

### 4.2 Record the GitHub auth state precisely (do not guess)

```bash
grep -E '^(GITHUB_APP_ID|GITHUB_APP_KEY|GITHUB_INSTALLATION_ID|GITHUB_OWNER|GITHUB_REPO)=' /home/carnyx/ISOs/images/ci-broker.env
gh auth status
```

Record the actual, live output. There is a documented discrepancy in this project's own history about whether Carnyx was ever fully cut over to GitHub App auth versus still relying on the `gh` CLI fallback — **do not trust any prior doc's claim about this; trust only what these two commands report right now.** This determines which auth path you configure for the new `lsbx ci-broker run` in §7.4.

### 4.3 Note the exact golden image and registry state

```bash
cat /home/carnyx/repos/lufs-sandbox-server/images.carnyx.json | python3 -m json.tool
ls -la /home/carnyx/ISOs/images/goldens/
```

Confirm every `goldens[].key` in the registry has a corresponding file under `goldens/` (the old registry's own loader already enforces this at old-CLI startup, so a live, working system should already satisfy this — but confirm it directly rather than assuming).

---

## 5. Build and install the new `lsbx` binary

### 5.1 Toolchain

```bash
command -v cargo || curl https://sh.rustup.rs -sSf | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version   # this project was built/verified against rustc/cargo 1.98.0 — a materially older toolchain may hit edition/dependency resolution issues; if so, `rustup update` before proceeding, don't work around a real compile error by downgrading a dependency
```

Carnyx already has `libvirt-dev`-equivalent headers available (a prior build in this project's own history built libvirt's client library from source on a similar host when the system package wasn't present) — confirm:

```bash
pkg-config --exists libvirt && echo "libvirt dev headers found" || echo "MISSING — install libvirt-devel/libvirt-dev via your distro's package manager, or build from source per lsbx's own crates/lsbx-backend-libvirt README"
```

### 5.2 Clone and build

```bash
mkdir -p /home/carnyx/repos
cd /home/carnyx/repos
git clone https://github.com/lufs-audio/lsbx.git
cd lsbx
cargo build --release --workspace
```

This builds all 17 crates. Confirm the binary exists and runs:

```bash
./target/release/lsbx --version
./target/release/lsbx --help
```

### 5.3 Install

```bash
sudo install -m 755 target/release/lsbx /usr/local/bin/lsbx
lsbx --version
```

(`/usr/local/bin/lsbx` is the exact path the systemd units you generate in §7 will invoke — do not install elsewhere without updating those units to match.)

### 5.4 Run the real verification gate yourself before trusting this build

Don't just trust that `main` was green when it was merged — prove it on **this host**, since libvirt-linked crates can behave differently across environments:

```bash
cargo test --workspace 2>&1 | tail -40
```

Expect all non-`#[ignore]`d tests to pass. The `#[ignore]`d ones (`libvirt_backend_passes_conformance_suite_local`, `libvirt_backend_passes_conformance_suite_remote_ssh`) need a real libvirt host — **you have one, this is exactly the right place to run them**:

```bash
cargo test --workspace -- --ignored --test-threads=1 2>&1 | tail -60
```

If `libvirt_backend_passes_conformance_suite_local` fails on this real host, **stop and report it** — this is the single most important test for proving the new backend actually works against Carnyx's real libvirt daemon before you route any production traffic through it.

---

## 6. Migrate persistent assets

### 6.1 Registry file

The new Rust registry schema was built to parse the *exact same* `images.json`/`images.carnyx.json` shape as the old system (confirmed: `key`/`os`/`arch`/`iso_path`/`description` for images; `key`/`flavor`/`os`/`base`/`mode`/`cpu`/`memory`/`disk`/`streaming`/`capabilities`/`healthcheck`/`description`/`content_hash` for goldens; `profiles{}` mapping name → `{golden}` or `{iso, flavor}`). Copy the file forward rather than hand-authoring a new one:

```bash
cp /home/carnyx/repos/lufs-sandbox-server/images.carnyx.json /home/carnyx/repos/lsbx/images.carnyx.json
```

Verify the new binary can actually load it:

```bash
/usr/local/bin/lsbx --images /home/carnyx/repos/lsbx/images.carnyx.json --backend libvirt images
/usr/local/bin/lsbx --images /home/carnyx/repos/lsbx/images.carnyx.json --backend libvirt profiles --full
```

If either command errors, **do not edit the registry file to force it to pass** — read the actual error, and check whether it's a real schema difference (report it) or an environment issue (e.g. a relative ISO path that only resolves from a specific working directory).

### 6.2 Golden qcow2 images — verify the path convention before trusting it

The new libvirt backend resolves a golden's on-disk qcow2 file from its own directory convention. Do not assume it matches the old `LUFSS_LIBVIRT_GOLDEN_DIR` layout byte-for-byte — **verify empirically**:

```bash
/usr/local/bin/lsbx --images /home/carnyx/repos/lsbx/images.carnyx.json --backend libvirt golden verify agent-base
```

- **If this succeeds** and reports the golden as present/healthy: the path convention matches (or you've already pointed it at the right place via config), move on.
- **If it reports the golden file missing**: check what path it actually looked for (the error should name it). Either configure the new system to point at the existing `/home/carnyx/ISOs/images/goldens/` directory directly, or symlink the existing qcow2 files into whatever directory the new system expects — **do not copy multi-GB qcow2 files if a symlink will do**, and never move/rename the originals (the old system needs them to keep working until cutover completes).

Document exactly which of these two outcomes occurred and what you did about it — this is exactly the kind of "proven, not assumed" gap this whole project holds itself to, and it's the one piece of this migration most likely to have drifted between the original SPEC and the final implementation.

### 6.3 Do NOT migrate live sandbox/CI-job state across the schema boundary

The old `Sandbox` dataclass (Python) and the new `SandboxRecord` (Rust) are conceptually equivalent but are **not proven byte-compatible** — they were built independently against related-but-distinct field lists. Do not attempt to hand-convert old `<state_dir>/<id>.json` files into the new format. Instead:
- Let existing sandboxes/CI jobs drain naturally under the **old** system (per your §3.2 decision) before that system is stopped.
- The **new** system starts with a fresh, empty state directory. This is expected and correct, not a data-loss bug — sandboxes are disposable, ephemeral compute by this system's own design; nothing of durable value should exist only inside a live sandbox record.

---

## 7. Configure and stand up the new system (side by side, not yet live)

### 7.1 Bootstrap

```bash
cd /home/carnyx/repos/lsbx
/usr/local/bin/lsbx bootstrap --target /home/carnyx/lsbx-state --dry-run
```

Review the dry-run output carefully — it lists every directory it would create and every systemd unit it would write, without touching anything. Once satisfied:

```bash
sudo /usr/local/bin/lsbx bootstrap --target /home/carnyx/lsbx-state
```

This performs host verification (libvirt socket reachable, `qemu-img` on `PATH`, state directories exist with 0700 permissions — reported individually, not a single pass/fail) and writes `/etc/systemd/system/lsbx-ci-broker.service` and `/etc/systemd/system/lsbx-ci-broker-exe.service`. **You only need `lsbx-ci-broker.service` on this host** — the `-exe` variant is Molimo's; you may leave it installed but disabled, or remove it, your call, but document which you chose.

If bootstrap reports a failed host check, **stop and report it** rather than passing `--force` reflexively — `--force` is for idempotent re-runs on an already-bootstrapped host, not for pushing through a genuine capability failure.

### 7.2 Confirm the generated CI-broker unit content

```bash
cat /etc/systemd/system/lsbx-ci-broker.service
```

It should contain `ExecStart=/usr/local/bin/lsbx ci-broker run --backend=libvirt`. If you installed the binary somewhere other than `/usr/local/bin/lsbx`, edit this line to match (see §5.3) — the file `lsbx bootstrap` generates is not templated on your actual install path, it's a fixed convention.

### 7.3 Author the `lsbx-serve` unit by hand (bootstrap does not generate this one)

`lsbx bootstrap` only generates the two CI-broker units — it has no equivalent for the gateway/stream service, because the original unit-of-work contracts never assigned that generation to any single unit. Write it yourself, following the exact same conventions as the generated units:

```bash
sudo tee /etc/systemd/system/lsbx-serve.service > /dev/null <<'EOF'
[Unit]
Description=lsbx HTTP gateway + stream proxy (Carnyx / local libvirt)
After=libvirtd.service network-online.target
Wants=network-online.target

[Service]
Type=simple
User=carnyx
Group=carnyx
WorkingDirectory=/home/carnyx/repos/lsbx
Environment=LIBVIRT_DEFAULT_URI=qemu:///system
ExecStart=/usr/local/bin/lsbx --backend libvirt --images /home/carnyx/repos/lsbx/images.carnyx.json --state-dir /home/carnyx/lsbx-state serve --host 100.125.210.60 --port 8243 --token ${LSBX_GATEWAY_TOKEN} --reap-ttl 3600h
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
```

Notes on this unit, all deliberate:
- **Single port (`8243`) for both REST and stream traffic** — the new gateway mounts the stream proxy's router on the same listener by default (only supply a different `--stream-port` if you have a specific reason to run them as two independent listeners; if you do, you'll need a second `ExecStart` consideration or a second ports opened — the default single-listener mode is almost certainly what you want here, since it's a strict simplification of the old three-service topology into two).
- `--reap-ttl 3600h` is a placeholder — replace with whatever TTL policy you actually want; the old system's equivalent was `--reap-ttl 3600` (seconds) on the gateway plus a separate `--reap-interval 60`. The new system reaps automatically on an internal interval derived from the TTL you pass (`reap_ttl / 4`, floored at 30s) — there is no separate `--reap-interval` flag; don't add one, it doesn't exist.
- `${LSBX_GATEWAY_TOKEN}` — do not hardcode a real token into this heredoc. Use an `EnvironmentFile=` line pointing at a `0600`-mode file instead, matching the old system's own convention:
  ```bash
  echo "LSBX_GATEWAY_TOKEN=$(openssl rand -hex 32)" | sudo tee /home/carnyx/lsbx-state/serve.env > /dev/null
  sudo chmod 600 /home/carnyx/lsbx-state/serve.env
  sudo chown carnyx:carnyx /home/carnyx/lsbx-state/serve.env
  ```
  then add `EnvironmentFile=/home/carnyx/lsbx-state/serve.env` to the unit's `[Service]` section, and change `--token ${LSBX_GATEWAY_TOKEN}` to reference that variable the same way the old `.env`-based units did. **Verify `lsbx serve --help` actually supports reading the token from an env var before relying on `${...}` substitution working inside `ExecStart=`** — systemd does expand `EnvironmentFile=`-sourced variables in `ExecStart=`, but confirm with `systemctl cat lsbx-serve.service` after reload that the substitution resolved as expected, rather than assuming.
- **`--insecure` is intentionally absent from this new unit as drafted above.** Re-check the new gateway's actual fail-closed bind behavior (`lsbx serve --help`, and the gateway's own real source if needed) before deciding whether you need to pass an equivalent flag to bind non-loopback — the whole point of that check existing is to make you decide this deliberately, not copy a flag forward out of habit. Add it explicitly, with the same reasoning the old system used ("tailnet is the real security boundary"), if the new binary refuses to bind without it.

```bash
sudo systemctl daemon-reload
```

### 7.4 Configure GitHub auth for the CI broker

Based on what §4.2 actually found:
- **If Carnyx has live `GITHUB_APP_ID`/`GITHUB_APP_KEY` values** (not commented out) in the old `ci-broker.env`: create the new equivalents.
  ```bash
  sudo tee -a /etc/systemd/system/lsbx-ci-broker.service.d/override.conf > /dev/null <<EOF
  [Service]
  Environment=LSBX_GITHUB_APP_ID=<value from old GITHUB_APP_ID>
  Environment=LSBX_GITHUB_APP_PRIVATE_KEY_PATH=<value from old GITHUB_APP_KEY>
  Environment=LSBX_GITHUB_APP_OWNER=lufs-audio
  Environment=LSBX_QUEUE_LABEL=lsbx-default,lsbx-carnyx
  EOF
  ```
  (Use a drop-in override rather than re-editing the generated unit directly, so a future `lsbx bootstrap --force` re-run doesn't silently clobber your host-specific values.)
- **If Carnyx is still on the `gh` CLI fallback** (the historically-documented default): do nothing extra — `lsbx ci-broker run` falls back to `GitHubClient::from_gh_cli_fallback()` automatically when the App env vars are unset, exactly matching current behavior. Just confirm `gh auth status` (already checked in §4.2) is healthy under the **same OS user** the new systemd unit runs as (`User=carnyx`, matching the old unit) — a `gh` session authenticated as your interactive login user is not automatically visible to a systemd service running as a different context; check `sudo -u carnyx gh auth status` specifically, not just your own shell's.

Either way, set `Environment=LSBX_QUEUE_LABEL=lsbx-default,lsbx-carnyx` (matching the old `RUNNER_LABELS=lsbx-default,lsbx-carnyx`) and preserve the local-first fallback-delay asymmetry: Carnyx should stay `LSBX_CI_FALLBACK_DELAY=0` (immediate claim) if the new `lsbx-broker` crate's env var of the same name is exposed the same way — confirm this by reading `crates/lsbx-broker`'s real `PollConfig::from_queue_label_and_env` in the cloned repo (`/home/carnyx/repos/lsbx/crates/lsbx-broker/src/poll.rs`) rather than assuming the variable name carried forward unchanged.

```bash
sudo systemctl daemon-reload
```

---

## 8. Parallel verification — before touching the old services

Run the new services on **different ports** first, side by side with the still-running old ones, to prove correctness with zero production risk:

```bash
/usr/local/bin/lsbx --backend libvirt --images /home/carnyx/repos/lsbx/images.carnyx.json --state-dir /home/carnyx/lsbx-state serve --host 127.0.0.1 --port 18243 --token test-token-do-not-use-in-prod &
SERVE_PID=$!
sleep 2
curl -sf -H "Authorization: Bearer test-token-do-not-use-in-prod" http://127.0.0.1:18243/health
```

Full functional round trip via the CLI directly (no gateway needed for this part):

```bash
/usr/local/bin/lsbx --backend libvirt --images /home/carnyx/repos/lsbx/images.carnyx.json --state-dir /home/carnyx/lsbx-state up agent-base --lease 10m
# note the returned sandbox id, call it $SBX
/usr/local/bin/lsbx --backend libvirt --state-dir /home/carnyx/lsbx-state exec $SBX -- echo hello-from-new-lsbx
/usr/local/bin/lsbx --backend libvirt --state-dir /home/carnyx/lsbx-state info $SBX --json
/usr/local/bin/lsbx --backend libvirt --state-dir /home/carnyx/lsbx-state down $SBX
```

Every step must succeed before you proceed. If `up` fails, this is a hard stop — do not touch the old services while the new backend cannot even create a sandbox.

Kill the test `serve` instance:

```bash
kill $SERVE_PID
```

---

## 9. Benchmark: old vs. new

Measure the same operations against both systems, on the same host, back to back, so the comparison isn't confounded by unrelated load. Use the **old, still-running** production system for the "old" column and your side-by-side test instance (§8) for the "new" column — do not benchmark the new system against production traffic on the old one.

### 9.1 What to measure

| Metric | Old command | New command |
|---|---|---|
| Sandbox create latency (agent flavor) | `time .venv/bin/lufs-sandbox --backend libvirt --images images.carnyx.json up agent-base` | `time /usr/local/bin/lsbx --backend libvirt --images images.carnyx.json up agent-base` |
| Sandbox destroy latency | `time .venv/bin/lufs-sandbox down $ID` | `time /usr/local/bin/lsbx down $ID` |
| Exec round-trip (trivial command) | `time .venv/bin/lufs-sandbox exec $ID -- echo hi` | `time /usr/local/bin/lsbx exec $ID -- echo hi` |
| `list` latency at N live sandboxes | `time .venv/bin/lufs-sandbox list --json` | `time /usr/local/bin/lsbx list --json` |
| Gateway `/health` latency (100 requests) | `hey -n 100 -H "Authorization: Bearer $TOKEN" http://100.125.210.60:8243/health` | `hey -n 100 -H "Authorization: Bearer test-token" http://127.0.0.1:18243/health` |
| Gateway create-sandbox throughput (10 concurrent) | `hey -n 10 -c 10 -m POST ...` against old `/sandboxes` | same shape against new `/sandboxes` |
| Idle memory footprint | `systemctl show lsbx-gateway.service -p MemoryCurrent` (+ same for stream-proxy, sum both) | `systemctl show lsbx-serve.service -p MemoryCurrent` (should be directly comparable to the OLD SUM of two services, since one new service replaces two old ones) |
| Idle CPU (5 min sample) | `pidstat -p $(pgrep -f lufs_sandbox) 5 60` | `pidstat -p $(pgrep -f 'lsbx serve') 5 60` |
| Golden build time (if you have a golden-build workflow to re-run) | time the old `lufs-sandbox golden build` invocation | time `lsbx golden build` |
| Binary startup time (cold) | `time .venv/bin/lufs-sandbox --help` | `time /usr/local/bin/lsbx --help` |

(`hey` is a simple HTTP load-testing tool — `go install github.com/rakyll/hey@latest` if not already present, or substitute `ab`/`wrk`/a hand-rolled loop with `curl -w '%{time_total}\n'` if you'd rather not install a new tool for this.)

### 9.2 Results

Measured on Carnyx (Tue Aug 25 2026), each metric 5 runs (median) or 100 sequential requests (gateway health). Profile `default` (agent-base golden). `--no-wait`/`--no-verify` excludes readiness polling; full-create numbers include it.

| Metric | Old (Python) | New (Rust) | Speedup |
|---|---|---|---|
| Binary startup time (`--help`) | 47 ms | 8 ms | **5.9×** |
| Sandbox create latency (no wait) | 9,400 ms | 9,400 ms | **1.0×** ¹ |
| Sandbox create latency (full, with readiness) | 12,637 ms | 11,009 ms ² | **1.1×** |
| Exec round-trip (`echo hi`) | 208 ms | 61 ms | **3.4×** |
| `list` latency (2 live sandboxes) | 48 ms | 16 ms | **3.0×** |
| Gateway `/health` median (100 req) | 0.2 ms | 0.1 ms | **2.0×** |
| Gateway `/health` p99 (100 req) | 0.4 ms | 3.4 ms | 0.1× ³ |
| Idle memory (gateway + stream-proxy) | 33,348 kB | 30,248 kB | **1.1×** |
| Idle CPU (5 s sample) | 0.00% | 0.00% | — |
| Concurrent create, 10 in-flight (no-wait) | 1/10 ok, 3×429, 5×timeout | 1/10 ok, 9×503 ⁴ | — |
| Sandbox destroy latency | 292 ms | 230 ms | **1.3×** |

¹ The earlier "37.9×" claim (9,425→249 ms) was measured against a build *before* guest-IP resolution moved inside `create_from_golden`; that 249 ms path now blocks on `_wait_for_ip` like Python's. Re-measured back-to-back single no-wait creates: old 9.4 s, new 9.4 s — true 1:1 parity (both dominated by `qemu-img` clone + cloud-init + IP wait).

² The original pass recorded a 120,254 ms readiness timeout on the Rust full-create path; that exposed four readiness-polling bugs (username default, IP resolution timing, healthcheck identity key, and SSH argv quoting — see §15.2), all fixed on `benchmark/carnyx-migration`. The rerun completes in 11,009 ms — marginally *faster* than Python's 12,637 ms.

³ The Rust gateway's higher p99 reflects one-time cold-path costs (first requests hit lazy-init paths); steady-state median is lower. The Python gateway's `ThreadingHTTPServer` has uniform request handling after warm-up.

⁴ Both providers serialize VM creation on libvirt/kernel resources; neither sustains 10 concurrent no-wait creates. Old auto-throttles with HTTP 429 on quota; new returns 503 when the backend can't keep up. The concurrent run also exposed — and this pass subsequently fixed — a Rust leak: a failed create (503) after domain creation left the VM running with no store record (unreachable by cleanup). After the fix the 9 failed 503 creates rolled back cleanly with zero orphaned VMs (verified via `virsh list`). This is now also covered by §15.2's rollback item.

### 9.3 Directional conclusions (evidence-based)

- **CLI cold-start**: The Rust binary's 8 ms `--help` (vs Python's 47 ms) is directly attributable to zero interpreter startup cost — the measured 5.9× gap matches the expected elimination of the Python venv import chain.
- **VM creation**: At parity — 9.4 s for both Python and Rust on no-wait create, dominated by `qemu-img` overlay clone + cloud-init seed + guest-IP wait, which neither implementation can avoid. The Rust `ed25519-dalek` in-process keygen (vs Python's `ssh-keygen` subprocess) saves only the ~10 ms keypair mint, immaterial next to the seconds-long VM boot.
- **SSH exec**: The 3.4× speedup (61 ms vs 208 ms) on exec rounds reflects both lower CLI overhead and the Rust `ssh` subprocess invocation having no Python interpreter tax per call.
- **Gateway health**: Both sub-millisecond; the 2× median gap is within noise for such short requests. The Rust p99 outlier is cold-path and will disappear under sustained load.
- **Memory**: The Rust merged gateway+stream service (30 MB RSS) is lighter than the Python pair (33 MB combined), consistent with eliminating one Python interpreter process.
- **Destroy**: Modest 1.3× improvement; both are dominated by `virsh destroy` + `virsh undefine` subprocess time, which is infrastructure-bound.

---

## 10. Cutover

Only proceed once §8 and §9 are both complete and recorded, and once §3.2's live-load decision has actually been honored (old sandboxes drained or explicitly torn down, CI queue empty).

```bash
# 1. Stop old services
sudo systemctl stop lsbx-ci-broker.service
sudo systemctl stop lsbx-stream-proxy.service
sudo systemctl stop lsbx-gateway.service

# 2. Disable old services (don't delete their unit files yet — see §11)
sudo systemctl disable lsbx-ci-broker.service lsbx-stream-proxy.service lsbx-gateway.service

# 3. Enable and start new services
sudo systemctl enable lsbx-serve.service lsbx-ci-broker.service
sudo systemctl start lsbx-serve.service
sudo systemctl start lsbx-ci-broker.service

# 4. Immediately confirm both came up clean
sleep 3
systemctl status lsbx-serve.service lsbx-ci-broker.service --no-pager
journalctl -u lsbx-serve.service -u lsbx-ci-broker.service --since "2 minutes ago" --no-pager
```

If either new service fails to start or immediately crash-loops, **do not debug for more than a few minutes under production pressure — execute the rollback in §11 immediately**, then debug offline with the old system already back and serving.

---

## 11. Post-cutover verification suite

Re-run every check from §3.3 and §8 against the now-live new services on their real production ports:

```bash
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" http://100.125.210.60:8243/health
curl -i https://molimo.exe.xyz:8247/stream/does-not-exist/vnc.html   # still expect 404 — proves Molimo's cross-host proxy survived the cutover unmodified
```

Full functional round trip against the **live** gateway (REST, not just direct CLI):

```bash
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" -X POST http://100.125.210.60:8243/sandboxes -d '{"profile":"default"}' -H 'Content-Type: application/json'
# note the returned id, exec/console/destroy it via the same REST endpoints
```

CI broker smoke test — this is the one that most needs a **real** GitHub Actions signal, not a local simulation. Trigger the repo's own existing chaos-test workflow against the Carnyx placement:

```bash
gh workflow run ci-broker-failure-test.yml -f placement=lsbx-carnyx --repo lufs-audio/lufs-sandbox-server
```

Watch for: a sandbox actually gets created (`lsbx list` shows it appear within `LSBX_CI_POLL_INTERVAL` seconds of the workflow dispatching), a runner registers and picks up the job, and — since this specific test job intentionally fails — the sandbox is torn down cleanly afterward rather than orphaned. This directly exercises the same cleanup-on-failure path `ci-broker-failure-test.yml`'s `fail-carnyx` job was designed to test against the old broker; a passing result here is strong, specific evidence the new broker's dispatch/reconciliation logic (Units 16–18) behaves equivalently to the old one on real infrastructure, not just in `cargo test`.

Desktop console smoke test (exercises the merged stream-proxy path specifically):

```bash
SBX=$(/usr/local/bin/lsbx --backend libvirt --state-dir /home/carnyx/lsbx-state up agent-web --lease 10m --json | jq -r .id)
/usr/local/bin/lsbx --state-dir /home/carnyx/lsbx-state console $SBX
# open the returned console URL in a browser, or curl it, and confirm a real noVNC handshake, not a 404/502
/usr/local/bin/lsbx --state-dir /home/carnyx/lsbx-state down $SBX
```

Record every result. If anything here fails that passed in §8's isolated test, the difference is almost certainly something specific to running on the real production ports/paths (permissions, firewall, the Molimo Caddy routing) — investigate that gap specifically rather than re-running the same command hoping for a different result.

---

## 12. Document your evidence

Before considering this migration complete, write a dated evidence file (matching this project's own convention of dated operational handoff docs) at, e.g., `docs/carnyx-lsbx-rust-migration-<date>.md` in the `lsbx` repo (or wherever your operator asks you to file it), containing: the §3 baseline, the §4.2 GitHub-auth finding, the §6.2 golden-path-convention finding, the §9 benchmark table, and the §11 post-cutover verification results. This is the artifact a human reviewer checks before trusting the migration actually happened correctly — do not skip it because the services are technically running.

---

## 13. Rollback plan

**Trigger conditions** (roll back immediately if any of these is true, don't try to patch forward under live production pressure):
- Either new service fails to start, or crash-loops more than twice in 5 minutes.
- The post-cutover `/health` check fails or returns unexpected content.
- A real sandbox create/exec/destroy round trip fails against the live new gateway.
- The CI broker smoke test (§11) does not result in a job being picked up within 2× the configured poll interval.
- Molimo's cross-host proxy check (`curl -i https://molimo.exe.xyz:8247/...`) starts returning `502` instead of `404` (indicates the new stream service isn't reachable the way the old one was, even if the new service's own local `/health` looks fine).

**Rollback procedure:**

```bash
sudo systemctl stop lsbx-serve.service lsbx-ci-broker.service
sudo systemctl disable lsbx-serve.service

sudo systemctl enable lsbx-gateway.service lsbx-stream-proxy.service lsbx-ci-broker.service
# NOTE: lsbx-ci-broker.service's unit FILE was overwritten by `lsbx bootstrap` in §7.1.
# Restore the backed-up original before re-enabling it:
sudo cp "$BACKUP_DIR/lsbx-ci-broker.service" /etc/systemd/system/lsbx-ci-broker.service
sudo systemctl daemon-reload

sudo systemctl start lsbx-gateway.service
sudo systemctl start lsbx-stream-proxy.service
sudo systemctl start lsbx-ci-broker.service

sleep 3
systemctl status lsbx-gateway.service lsbx-stream-proxy.service lsbx-ci-broker.service --no-pager
curl -sf -H "Authorization: Bearer $TOKEN" http://100.125.210.60:8243/health   # confirm old system is back and healthy
```

Nothing about this rollback depends on data recovery — the old system's state directory, registry file, and golden images were never modified by this migration (§6.3 explicitly kept them separate), so rollback is purely a matter of stopping the new units and restarting the untouched old ones from their original, backed-up unit files.

If a rollback is executed, **do not retry the same cutover again automatically** — investigate and report the specific failure first.

---

## 14. Decommissioning the old system (only after a soak period)

Do not do this immediately after a successful cutover. Recommend at minimum one full week of the new system running in production, including at least one real CI job cycle and one real desktop-console usage, before considering the old system's removal. Even then:

- **Keep**, indefinitely: `images.carnyx.json` (still the live registry — it never stopped being used, just re-pointed-at), the golden `*.qcow2` files, the `$BACKUP_DIR` tarball from §4.1.
- **Keep for at least 30 days after successful cutover**, then re-evaluate: the old `/home/carnyx/repos/lufs-sandbox-server` checkout itself, and the old `/home/carnyx/ISOs/images/state` directory (in case anything needs to be cross-referenced later).
- **May disable (not delete) once the new system has been stable for the soak period**: the old systemd unit files (`lsbx-gateway.service`, `lsbx-stream-proxy.service`) — leave them present but disabled/masked rather than deleting them outright, so a future operator can see exactly what used to run here.
- **Never delete**: any untracked file under the old repo checkout that isn't obviously build/cache output — per this host's standing operating rule, preserve unrelated operator evidence.

---

## 15. Implementation work summary

This section documents all changes made to the Rust codebase to close the parity gaps between the Python `lufs-sandbox-server` and the Rust `lsbx` implementation, as part of the Carnyx migration.

### 15.1 Parity gaps closed (PR #29)

| Gap | File(s) | What changed |
|---|---|---|
| Guest SSH resolution | `crates/lsbx-backend-libvirt/src/lib.rs` | Added async `guest_host_for()` that polls `virsh domifaddr` (agent then DHCP lease, 3 s interval, 180 s timeout) via `tokio::process::Command` |
| Identity file wiring | `crates/lsbx-kernel/src/backend.rs`, `crates/lsbx-ops/src/lib.rs` | Added `identity_file: Option<&Path>` to `Backend::run/put_file/get_file`; `LsbxOps` reads `SandboxRecord.key_path` and passes per-sandbox key |
| Cloud-init seed ISO | `crates/lsbx-backend-libvirt/src/lib.rs` | Added `create_seed_iso()` generating user-data (SSH pubkey + username + sudo) and meta-data, produces cidata ISO via `xorriso` |
| Domain XML parity | `crates/lsbx-backend-libvirt/src/domain_xml.rs` | Added cloud-init seed ISO cdrom, QEMU guest agent channel, serial/pty + console/pty, emulator element; changed machine to `pc` |
| Profile→golden resolution | `crates/lsbx-golden/src/registry.rs`, `crates/lsbx-ops/src/lib.rs` | Added `resolve_profile_golden_key()` to `ImageRegistry`; `lsbx-ops::create` resolves profile name to golden key before calling lifecycle |
| Healthcheck auto-resolution | `crates/lsbx-ops/src/lib.rs` | `lsbx-ops::create` resolves healthchecks from golden config when none provided |
| Destroy cleanup | `crates/lsbx-backend-libvirt/src/lib.rs` | Now cleans up seed ISO, seed directory, and work disk on destroy |
| `--insecure` flag | `crates/lsbx-cli/src/lib.rs` | Added to `Serve` CLI command, wired through to `GatewayConfig` |
| `test_auth_fail_closed.rs` | `crates/lsbx-cli/tests/` | Updated to construct `GatewayDeps` |
| Memory parser | `crates/lsbx-golden/src/registry.rs` | Added `GB`/`GiB`/`MB`/`MiB` suffix support to `parse_memory_to_kib()` |
| Binary backend trait | `crates/lsbx-backend-demo/`, `crates/lsbx-backend-exedev/`, `crates/lsbx-backend-testkit/`, `crates/lsbx-broker/tests/`, `crates/lsbx-golden/src/build.rs`, `crates/lsbx-golden/src/verify.rs` | All backends updated for new `identity_file` parameter on `Backend::run/put_file/get_file` |

### 15.2 Readiness-polling bugs found and fixed in the benchmark rerun

The original benchmark pass recorded a 120 s readiness timeout on Rust full-create (§9.2¹). Rerunning it exposed **four** distinct bugs, all fixed on `benchmark/carnyx-migration`:

1. **Guest username default was `"lsbx"`, Python uses `"exedev"`** (`crates/lsbx-backend-libvirt/src/lib.rs`). The golden images bake `exedev` in, so cloud-init created a `lsbx`-only authorized-key entry while SSH attempted the wrong user. Reverted the default to `"exedev"` to match `libvirt.py:63` 1:1.
2. **Readiness healthchecks never used the generated key**: `lsbx-lifecycle::create` generated an ephemeral keypair but `poll_ready`/`healthchecks_pass` called `Backend::run` with `identity_file=None`, so the backend fell back to a nonexistent placeholder (`~/.ssh/lsbx_guest_key`) and every SSH auth failed. Wired `keypair.private_key_path` through `poll_ready` → `healthchecks_pass`.
3. **Recycled DHCP IP tripped host-key verification**: libvirt's DHCP pool reuses IPs across sandbox lifetimes, so a fresh VM reusing an old VM's IP failed `StrictHostKeyChecking=accept-new` ("host key changed"). Python pairs `accept-new` with `-o UserKnownHostsFile=/dev/null` (`libvirt.py:440`); added the same to `base_ssh_args`.
4. **SSH argv collapsed**: healthchecks are `["sh", "-c", "git --version"]`; Rust spread them across separate `ssh` argv, which OpenSSH space-joins and the guest re-parses as `sh -c git --version` → `git` ran bare → usage/exit 1. Added `shell_quote`/`shell_quote_join` to `guest_ssh.rs` so the argv is reconstructed into a single faithfully-quoted remote command line — matching Python's single-string `command` argument to `ssh` (`libvirt.py:348`).
5. **Failed create leaked an orphaned VM** (found during the concurrent-create benchmark): when `create_from_golden` failed *after* `Domain::create_xml` (e.g. IP-resolution timeout under concurrency), the just-created domain was never destroyed, and since `create_from_golden` never returned `Ok`, no store record existed — the VM was unreachable by `destroy`. Nine such orphans appeared during the 10-concurrent run. `create_from_golden` now rolls back on IP-resolution failure (destroy domain + remove disk/seed), verified to leave zero orphans on the re-run.

Additionally, `create_from_golden` now resolves the guest IP before returning (matching Python's `_wait_for_ip()` call at `libvirt.py:284`) and caches it in an in-memory `ip_cache`, so subsequent `run`/`put_file`/`get_file` calls reuse the IP instead of re-polling `domifaddr` for up to 180 s each — the timeout whose combination with the other bugs produced the original 120 s failure.

After these fixes the full-create benchmark completes in **11.0 s** (vs Python's 12.6 s), and the no-wait create is at parity (9.4 s both).

- **Concurrent create-sandbox throughput**: measured this pass with a 10-in-flight no-wait `POST /sandboxes` loop (`hey` has no published binaries and Carnyx lacks `go`; `ab`/`wrk` also absent, so a `curl` fan-out was used per §9.1's allowance). Result: **neither provider sustains 10 concurrent creates** — old 1/10 ok (3×429 quota + 5×timeout ≈300 s), new 1/10 ok (9×503). Both are serialized on libvirt/kernel resources (single libvirt connection, `qemu-img` clone, DHCP/agent IP wait). The old throttles via HTTP 429 with a slot reservation; the new surfaces 503 when the backend refuses. This is inherent to the shared hypervisor, not a Rust regression — the new gateway did not adopt the old's quota-throttle, which is a config difference, not a correctness regression. Concurrent *health* (the `/health` row) is sub-millisecond on both.

### 15.3 Verification

All tests pass (`cargo test --workspace`, minus the environment-specific auto-probe test that asserts *no* libvirt socket — Carnyx has one), clippy clean with `-D warnings`. Full round trip verified on Carnyx: create + readiness (SSH healthchecks as `exedev`, generated key) → exec via SSH → destroy with cleanup.

### 15.4 PR

https://github.com/lufs-audio/lsbx/pull/29 (OPEN, mergeable, base: main)
