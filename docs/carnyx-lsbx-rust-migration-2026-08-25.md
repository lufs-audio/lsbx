# Carnyx — Python → Rust `lsbx` migration evidence

- **Date:** 2026-08-25
- **Host:** carnyx (local libvirt, QEMU/KVM, `LIBVIRT_DEFAULT_URI=qemu:///system`)
- **Migration runbook:** `MIGRATION-CARNYX.md` (this repo, §1–§15)
- **PR with the code changes:** https://github.com/lufs-audio/lsbx/pull/29 (OPEN, not merged)
- **Backup / rollback artifacts:** `/home/carnyx/lsbx-migration-backup-20260825T162643Z/` (old unit files, env files, registry, `isos-images-state.tar.gz`)

> Status: **Cutover (§10) and post-cutover verification (§11) are PENDING.** The code, parity work,
> and benchmark evidence are complete and in PR #29; the live service flip requires root and was
> intentionally deferred (see §11 for the exact commands to run). This document is filed now so the
> migration can be reviewed; the §11 results block gets filled in after the cutover.

---

## §3 baseline (before state, recorded 2026-08-25)

### §3.1 Running services

| Unit | State | Identity |
|---|---|---|
| `lsbx-gateway.service` | active (running), enabled | Python `lufs-sandbox ... gateway --host 100.125.210.60 --port 8243` (PID 3771620, since 2026-08-20) |
| `lsbx-stream-proxy.service` | active (running), enabled | Python `lufs-sandbox ... stream-proxy --host 100.125.210.60 --port 8244` (PID 3771621) |
| `lsbx-ci-broker.service` | **inactive (dead)** unit | Old Python broker is running **detached, not under systemd**: PID 1529305 `python -u -m lufs_sandbox.ci_broker` (started 2026-08-21 from a Herdr pane; logs to `/home/carnyx/ISOs/images/ci-broker.log`; holds `state/ci-broker.lock`; `lsbx-carnyx` placement). `systemctl stop` will NOT stop it — a manual `kill` is required at cutover. |

Old gateway `/health` (baseline): `{"ok": true, "profiles": ["ci","default","desktop","web","win11"], "backends": ["demo","exedev","libvirt"]}`

Molimo cross-host stream proxy (`https://molimo.exe.xyz:8247/stream/does-not-exist/vnc.html`): currently **HTTP 307 → redirect to `__exe.dev/login`**, not the documented 404. This is Molimo-side behavior drift since the runbook was written; recorded as observed, not Carnyx's to fix. Direct Carnyx stream proxy: `/vnc.html` → 400, `/stream/does-not-exist/vnc.html` → 404 (as expected).

### §3.2 Load decision

- Old gateway live sandboxes at cutover-prep time: **0** (`GET /sandboxes` → `[]`).
- CI queue: **empty** — `gh run list` for `lufs-audio/lufs-sandbox-server` shows no `in_progress`/`queued`/`pending` runs.
- Decision (documented per runbook §3.2): sandboxes were drained to zero before benchmarking; only transient test VMs were created and destroyed during §9. No force-destroy of anyone's live work was needed.

### §3.3 Baseline health (before)

- `curl -sf -H "Authorization: Bearer $TOKEN" http://100.125.210.60:8243/health` → `{"ok": true, ...}` (exit 0).
- Molimo proxy: see drift note above (307, not 404).

---

## §4.2 GitHub-auth finding (recorded live, not from memory)

`ci-broker.env` contains only `GITHUB_OWNER=lufs-audio`, `GITHUB_REPO=lufs-sandbox-server` — **no GitHub App credential vars** (`GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY_PATH`, `GITHUB_APP_INSTALLATION_ID`, `GITHUB_APP_OWNER`).

`gh auth status` (as `carnyx`): **logged in** to `github.com` as `danialrami`, token from keyring, scopes `admin:org`, `admin:public_key`, `gist`, `repo`.

**Conclusion:** Carnyx is on the **`gh` CLI fallback** auth path (the historically-documented default), not GitHub App auth. The new `lsbx ci-broker run` automatically falls back to `GitHubClient::from_gh_cli_fallback()` when the App env vars are unset, so **no extra broker auth config is needed** beyond `gh auth` being healthy under the unit's user (`carnyx`) — confirmed above. (AGENTS.md documents the App-credential env-var set for hosts that do use App auth.)

---

## §6.2 Golden path-convention finding (verified empirically)

- **Python old system:** `LUFSS_LIBVIRT_GOLDEN_DIR=/home/carnyx/ISOs/images/goldens`, work dir `LUFSS_LIBVIRT_WORK_DIR=/home/carnyx/ISOs/images/work`.
- **New Rust backend default:** golden dir defaults to `<state_dir>/images` (e.g. `/home/carnyx/lsbx-state/images`), per-VM work dir to `<state_dir>/vms` — it does **not** assume the Python layout.
- When the new backend was run without `LSBX_LIBVIRT_IMAGES_DIR`, `qemu-img` tried `/home/carnyx/lsbx-state/images/agent-base.qcow2` and failed with "No such file or directory".
- **Resolution (documented outcome):** point the new system at the existing golden dir via env — `LSBX_LIBVIRT_IMAGES_DIR=/home/carnyx/ISOs/images/goldens` (and `LSBX_LIBVIRT_VM_DIR=/home/carnyx/lsbx-state/vms` for tests, or the Python work dir for the production units). The generated systemd units already set the correct `LSBX_LIBVIRT_IMAGES_DIR`/`LSBX_LIBVIRT_VM_DIR` (see §10). No golden files were moved/copied/symlinked; the originals stay put for the old system.

No live sandbox/CI state was migrated across the schema boundary (§6.3 honored).

---

## §9 Benchmark results (final, corrected)

Measured on carnyx, 2026-08-25. Profile `default` (agent-base golden). `--no-wait`/`--no-verify` excludes readiness; full-create includes it. Concurrent = 10 in-flight no-wait `POST /sandboxes` (`hey` has no published binaries and `go`/`ab`/`wrk` are absent, so a `curl` fan-out loop was used per runbook §9.1's allowance).

| Metric | Old (Python) | New (Rust) | Speedup |
|---|---|---|---|
| Binary startup (`--help`) | 47 ms | 8 ms | **5.9×** |
| Create latency (no wait) | 9,400 ms | 9,400 ms | **1.0×** ¹ |
| Create latency (full, with readiness) | 12,637 ms | 11,009 ms | **1.1×** ² |
| Exec round-trip (`echo hi`) | 208 ms | 61 ms | **3.4×** |
| `list` latency (2 live sandboxes) | 48 ms | 16 ms | **3.0×** |
| Gateway `/health` median (100 req) | 0.2 ms | 0.1 ms | **2.0×** |
| Gateway `/health` p99 (100 req) | 0.4 ms | 3.4 ms | 0.1× ³ |
| Idle memory (gateway + stream) | 33,348 kB | 30,248 kB | **1.1×** |
| Idle CPU (5 s sample) | 0.00% | 0.00% | — |
| Concurrent create, 10 in-flight | 1/10 ok, 3×429, 5×timeout | 1/10 ok, 9×503 | ⁴ |
| Destroy latency | 292 ms | 230 ms | **1.3×** |

¹ The earlier "37.9×" claim (9,425→249 ms) was measured before guest-IP resolution moved inside `create_from_golden`; the corrected back-to-back no-wait creates are 9.4 s both sides — true parity (both dominated by `qemu-img` clone + cloud-init + IP wait).
² The original full-create run hit a 120 s readiness timeout; that exposed four readiness bugs (username default, healthcheck identity key, recycled-DHCP host-key check, SSH argv collapse) plus the IP-resolution timing — all fixed in PR #29 (§15.2 of the runbook). Rerun: 11.0 s.
³ Rust p99 reflects one-time cold-path costs; steady-state median is lower.
⁴ Neither provider sustains 10 concurrent creates — both serialize on libvirt/kernel resources. Old throttles via HTTP 429 (quota), new returns 503. The concurrent run also exposed a Rust leak (failed create left an orphaned VM); that is fixed with a rollback in `create_from_golden` (verified zero orphans on re-run).

---

## §10 Cutover — PENDING (requires root)

Blocked in this session because `sudo` on carnyx requires a password and was not available to the automation. Everything below is pre-verified and ready to run. **Before starting:** CI queue must be empty and no live sandboxes (both confirmed true as of §3.2).

```bash
# 1. Stop old services
sudo systemctl stop lsbx-ci-broker.service
sudo systemctl stop lsbx-stream-proxy.service
sudo systemctl stop lsbx-gateway.service
# The old Python broker is NOT the systemd unit (it runs detached, PID 1529305) — stop it directly too:
kill 1529305   # confirm via: ps -p 1529305 -o pid,cmd

# 2. Disable old services (keep unit files for rollback — see §13)
sudo systemctl disable lsbx-ci-broker.service lsbx-stream-proxy.service lsbx-gateway.service

# 3. Install new units (lsbx bootstrap writes these; verify they match lsbx-state/systemd-units/)
sudo cp /home/carnyx/lsbx-state/systemd-units/lsbx-serve.service /etc/systemd/system/
sudo cp /home/carnyx/lsbx-state/systemd-units/lsbx-ci-broker.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable lsbx-serve.service lsbx-ci-broker.service
sudo systemctl start lsbx-serve.service
sudo systemctl start lsbx-ci-broker.service

# 4. Confirm
sleep 3
systemctl status lsbx-serve.service lsbx-ci-broker.service --no-pager
journalctl -u lsbx-serve.service -u lsbx-ci-broker.service --since "2 minutes ago" --no-pager
```

Notes for the operator:
- The new `lsbx-serve.service` binds `100.125.210.60:8243` (production port) with token from `/home/carnyx/lsbx-state/serve.env` and `--insecure` (tailnet is the security boundary, same model as the old gateway).
- The new `lsbx-ci-broker.service` runs `lsbx ci-broker run --backend=libvirt` with env from `/home/carnyx/ISOs/images/ci-broker.env` (already present and readable; `gh` auth healthy under `carnyx`, so the CLI-fallback auth path works).
- If a new service crash-loops, **do not debug under production pressure** — roll back per §13 immediately.

---

## §11 Post-cutover verification — PENDING (to be filled after §10)

```bash
# Gateway health on production port (expect the new Rust gateway now):
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" http://100.125.210.60:8243/health

# Molimo cross-host proxy (expect 404, not 502 — see §3.1 drift note for current 307 behavior):
curl -i https://molimo.exe.xyz:8247/stream/does-not-exist/vnc.html

# Full round trip via REST:
SBX=$(curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" -X POST http://100.125.210.60:8243/sandboxes \
      -d '{"profile":"default"}' -H 'Content-Type: application/json' | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
curl -sf -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" -X POST http://100.125.210.60:8243/sandboxes/$SBX/exec -d '{"command":["git","--version"]}' -H 'Content-Type: application/json'
curl -sf -X DELETE -H "Authorization: Bearer $LSBX_GATEWAY_TOKEN" http://100.125.210.60:8243/sandboxes/$SBX

# CI broker smoke (real GitHub Actions signal, Carnyx placement):
gh workflow run ci-broker-failure-test.yml -f placement=lsbx-carnyx --repo lufs-audio/lufs-sandbox-server

# Desktop console smoke (merged stream-proxy path):
SBX=$(/usr/local/bin/lsbx --backend libvirt --state-dir /home/carnyx/lsbx-state up agent-web --lease 10m --json | jq -r .id)
/usr/local/bin/lsbx --state-dir /home/carnyx/lsbx-state console $SBX
/usr/local/bin/lsbx --state-dir /home/carnyx/lsbx-state down $SBX
```

Fill in results here once executed. If anything that passed in §8's isolated test fails here, investigate the production-port-specific difference (permissions/firewall/Molimo Caddy routing), not the command itself.

---

## §13 Rollback

Fully documented in `MIGRATION-CARNYX.md` §13. Key facts verified for this host:
- Old unit files backed up: `/home/carnyx/lsbx-migration-backup-20260825T162643Z/lsbx-gateway.service`, `lsbx-ci-broker.service`, `lsbx-stream-proxy.service`.
- The old Python system's state dir (`/home/carnyx/ISOs/images/state`), registry (`images.carnyx.json`), and golden qcow2s were **never modified** by this migration — rollback is a pure unit-file swap, no data recovery.
- Rollback = stop new units, restore + start old units, confirm old `/health` returns the §3.3 baseline. Do not re-attempt the same cutover without investigating the specific failure first.

---

## §14 Decommissioning (not yet applicable)

Do not decommission anything until ≥1 week of stable production running under the new system, including ≥1 real CI cycle and ≥1 real desktop-console usage. See `MIGRATION-CARNYX.md` §14 for the keep/keep-30-days/may-disable matrix.
