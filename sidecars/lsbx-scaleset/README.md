# lsbx-scaleset — push dispatch for the lsbx CI broker

`lsbx-scaleset` registers a GitHub Actions **runner scale set** and long-polls
GitHub's job queue for it. When GitHub assigns a job, it boots one disposable
lsbx VM, hands the guest a **single-use JIT runner config**, and destroys the VM
when the job completes. It replaces the polling broker's Linux path
(`lsbx ci-broker run`), which listed runs/jobs for a hard-coded repo list over
the REST API.

| | polling broker (`lsbx ci-broker`) | `lsbx-scaleset` |
|---|---|---|
| how jobs are found | REST: list runs × jobs per repo × label, every 45 s | GitHub pushes over an outbound long-poll |
| repo list | `LSBX_CI_REPOS` (14 repos) | none — any repo the runner group allows |
| REST budget | ~4,300 req/h | ~0 (measured hourly in the log line `github requests`) |
| secret in the guest | GitHub App private key copied into every VM | JIT config only (one runner, one job) |
| job ↔ runner | first idle runner with matching labels wins | GitHub assigns the job to the scale set |

Design and decisions: `docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/`.
Library: [`github.com/actions/scaleset`](https://github.com/actions/scaleset)
(the same client ARC uses; "public preview").

## How it works

```
GitHub Actions service ◀── long-poll (outbound HTTPS) ── lsbx-scaleset (carnyx, systemd --user)
   JobAvailable / Started / Completed + statistics          │ target = min(MAX, MIN + TotalAssignedJobs)
                                                            ▼
                                lsbx CLI:  up ci  →  put {jitconfig, run-jit-runner.sh, runner tarball}
                                           exec run-jit-runner.sh  →  runner listens, takes its one job
                                           down   (on JobCompleted, or GC when the runner is gone)
```

* **Scale up** when `TotalAssignedJobs` rises: one VM per assigned job, capped
  by `LSBX_SS_MAX_RUNNERS`. Provisioning is asynchronous so the message loop
  never blocks on a VM boot. Failures back off (10 s → 5 min).
* **Scale down** happens on `JobCompleted` (the ephemeral runner deregisters
  itself). A runner left idle because its job was cancelled is removed after
  `LSBX_SS_IDLE_TIMEOUT`; the runner registration is deleted **first**, and if
  GitHub refuses (it was just handed a job) the VM is kept.
* **GC** (every ~30 s, piggy-backed on the empty long-poll) catches what
  messages miss: lease expiry (`LSBX_SS_LEASE`), runners that finished while
  the listener was down, and idle VMs whose runner process died.
* **Restart-safe.** State is persisted to `LSBX_SS_STATE`. On start, runners
  whose VM still exists are kept (a job may be running on them); half-made or
  unknown VMs carrying the scale set's name prefix are destroyed. Busy runners
  are never destroyed on shutdown. After a crash GitHub holds the old message
  session for ~60 s; the listener retries until it can open a new one.
* **Orphan sweep (libvirt).** Killing `lsbx up` mid-boot leaves a running
  domain plus disks that `lsbx list` never recorded (measured). With
  `LSBX_SS_VIRSH_URI` set, start-up and every GC pass destroy domains that
  carry this scale set's name prefix and that no tracked runner owns, and
  delete their `<name>.*` / `<name>-*` files in `LSBX_LIBVIRT_VM_DIR`.
  Nothing outside the prefix is ever touched.
* **The scale set is persistent.** It is not deleted on exit, so restarts never
  drop queued jobs. Labels only take effect when a scale set is **created**
  (GitHub silently ignores label updates — measured), so a label change makes
  the listener delete and recreate the scale set, and only while nothing is
  assigned to it.

## Labels: what `runs-on` matches

Measured on github.com, 2026-11-14 (`lufs-audio/lmid` run `43634252712`):

* A scale set matches only the labels it was **created** with. Scale-set
  runners do **not** implicitly carry `self-hosted`, `Linux` or `X64`.
* So the `lsbx-linux` scale set is created with
  `lsbx-linux, lsbx-default, lsbx-carnyx, self-hosted, Linux, X64` (`lufs`
  joins once the Pi fleet's own workflows say `ARM64`), and
  existing workflows written as `runs-on: [self-hosted, lsbx-carnyx]` or
  `[self-hosted, lsbx-default]` run on it **unchanged**.
* Matching is still conjunctive: every label in `runs-on` must be on the scale
  set. `[self-hosted, lufs, ARM64]` therefore never lands here.

## Configuration

One env file per scale set, `~/ISOs/images/scaleset-<instance>.env` (0600);
see [`packaging/scaleset-linux.env.example`](packaging/scaleset-linux.env.example).

| variable | default | meaning |
|---|---|---|
| `LSBX_SS_URL` | — | org URL, `https://github.com/lufs-audio` |
| `LSBX_SS_NAME` | — | scale set name; also the VM/runner name prefix |
| `LSBX_SS_LABELS` | name | labels, comma separated (see above) |
| `LSBX_SS_RUNNER_GROUP` | `default` | runner group (`continuo`) |
| `LSBX_SS_MIN_RUNNERS` / `LSBX_SS_MAX_RUNNERS` | 0 / 2 | warm pool / concurrency cap |
| `GITHUB_APP_ID`, `GITHUB_APP_KEY` | — | App id and PEM path; the key never leaves the host |
| `GITHUB_APP_INSTALLATION_ID` | lookup | resolved from the org when unset |
| `LSBX_SS_LSBX_BIN`, `LSBX_SS_LSBX_ARGS` | `lsbx` | lsbx CLI and its global flags (backend/images/state-dir) |
| `LSBX_SS_PROFILE` | `ci` | lsbx profile to boot |
| `LSBX_SS_LEASE` | `3h` | hard cap on a VM's life |
| `LSBX_SS_BOOT_TIMEOUT` | `10m` | VM boot + runner start |
| `LSBX_SS_IDLE_TIMEOUT` | `3m` | grace before a surplus idle runner is removed |
| `LSBX_SS_RUNNER_TARBALL` | — | host-cached `actions-runner-linux-x64-*.tar.gz`, used when the golden lacks `/opt/actions-runner` |
| `LSBX_SS_GUEST_SCRIPT` | — | `scripts/run-jit-runner.sh` |
| `LSBX_SS_RUNNER_USER` | `exedev` | guest user that runs the runner |
| `LSBX_SS_STATE` | — | JSON state file (a `.lock` beside it enforces one listener per scale set) |
| `LSBX_SS_VIRSH_URI` | off | enables the libvirt orphan sweep (below) |

## Build, install, operate (carnyx)

```bash
# build (Go >= 1.27.3; static binary, no cgo)
cd sidecars/lsbx-scaleset && go test -race ./... &&
  CGO_ENABLED=0 go build -trimpath -ldflags "-s -w -X main.version=$(git rev-parse --short HEAD)" -o lsbx-scaleset .
install -m 0755 lsbx-scaleset ~/.local/bin/

# runner cache (verify against the release's "BEGIN SHA linux-x64" hash)
curl -fLo ~/ISOs/images/cache/actions-runner-linux-x64-2.338.2.tar.gz \
  https://github.com/actions/runner/releases/download/v2.338.2/actions-runner-linux-x64-2.338.2.tar.gz

# user unit (carnyx has linger enabled: `loginctl show-user carnyx -p Linger`)
install -Dm644 packaging/lsbx-scaleset@.service ~/.config/systemd/user/lsbx-scaleset@.service
systemctl --user daemon-reload && systemctl --user enable --now lsbx-scaleset@linux

journalctl --user -u lsbx-scaleset@linux -f        # scale up / job started / retired
lsbx-scaleset status                               # (with the env file loaded) persisted runner state
lsbx-scaleset delete-scale-set                     # retire a scale set (stop the unit first)
```

A user unit (not a system unit) because the `carnyx` account has no
passwordless sudo; linger keeps it running without a login session.
