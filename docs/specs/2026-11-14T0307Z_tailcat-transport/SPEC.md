# Phase Spec — tailcat as an lsbx transport (recon + decisions)

**Timestamp:** `2026-11-14T0307Z`
**Slug:** `tailcat-transport`
**Status:** **Recon + decisions only.** Implementation is a separate, dedicated
turn (see §7). This document exists so the decision is made against measured
facts and a written comparison, not against "the new shiny thing".
**Related:** `docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md`
(the deferred subnet-route alternative), `docs/specs/2026-11-07T1440Z_canonical-url-lsbx-lufshq/SPEC.md`
(the `lsbx.lufshq.com` / short-lived-token plan), `docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/SPEC.md`.

---

## 0. The question

Serving a disposable VM's desktop in a browser today relies on an ad-hoc
`socat TCP-LISTEN` on carnyx's tailnet IP, one process per sandbox, no auth
beyond the tailnet ACL. Meanwhile everything else in lsbx goes through
`lsbx serve`'s authenticated WebSocket stream proxy.

Tailcat (open-sourced 2026-08-31, Tailscale) is "netcat over Tailscale's data
plane without the control plane": userspace WireGuard + NAT traversal + DERP
rendezvous, no root, no TUN, no accounts, no route/DNS changes. It looks like it
could replace the socat habit with something encrypted, per-sandbox, and
gateway-free.

**Should lsbx adopt tailcat as a transport, and if so how — subprocess, FFI, or
native Rust?** This document answers that and nothing else.

---

## 1. What tailcat actually is (verified 2026-11-14)

Checked against the repo (`github.com/tailscale/tailcat`, cloned and read), the
v0.7.0 release, and the launch blog post. Facts below are measured, not recalled.

| Fact | Value |
|---|---|
| License | BSD-3-Clause |
| Language | Go (module `github.com/tailscale/tailcat`), CLI at `cmd/tailcat`, C ABI at `cmd/libtailcat` |
| Latest release | **v0.7.0**, published 2026-09-20 |
| Linux binary | `tailcat_0.7.0_linux_amd64.tar.gz`, **18 MB static** (also .deb/.rpm/arm64/armv7) |
| Windows/macOS | `tailcat_0.7.0_windows_amd64.zip`, Homebrew (`brew`), Scoop, Snap, AUR, conda-forge, Nix |
| Root required | **No** — userspace WireGuard + gVisor netstack; never touches routes or DNS |
| Identity | A `tc…` address = base64(CBOR(WireGuard pubkey, disco pubkey, pre-shared key, DERP region)) |
| Address length | ~140–190 chars (PSK included by default; longer when DERP nodes are embedded) |
| Ephemeral by default | Fresh key per process run; saved keys via `tailcat genkey` |
| Client restriction | `serve --allow=nodekey:…` allowlist; empty allowlist = "whoever has the address" |
| Relay | Tailscale's public DERP map by default (**rate-limited, no SLA, revocable**); `--derpmap-url` or `--region=derp.example.com` for your own `cmd/derper` |
| Stability promise | **None.** README "Stability": the Go API, CLI flags, output, *and wire format* may all change |

### The two halves, and the seam that matters

- **Server**: `tailcat serve <ports|services>` prints an address; incoming tunnel
  connections are proxied to the **server's own localhost** on that port.
  Services: `all`, `exit-node`, `ssh`, `no-auth-ssh`, `files` (SFTP), `exec`
  (inetd-style).
- **Client**: `tailcat forward <addr> [local:]remote`, `tailcat browse <addr>`,
  `tailcat socks <addr> <cmd>`, `tailcat ssh <addr>`, `tailcat ping <addr>`.
- **The seam**: `serve` proxies to `localhost` only. Reaching a *specific guest
  IP* (carnyx's `192.168.122.x`) needs either `serve exit-node` +
  `forward … local:remote-ip:remote-port` (README-documented), or a tailcat
  server running **inside the guest**.
- **`--json` exists** (`tailcat [--json] serve …`): server mode writes
  `{"listenAddr":"tc…"}` to stdout. This is the machine-readable seam a wrapper
  needs — the human line on stderr is `# 🐈 Server listening with new address: …`
  (an emoji and prose, i.e. exactly the kind of thing to never parse).

### Measured in this recon (on carnyx, 2026-11-14)

| Test | Result |
|---|---|
| `tailcat serve --json 18081` | `{"listenAddr":"tcpGFwWCC…"}` (153 chars) in ~10 s |
| `tailcat serve --json exit-node` | address + `# Selected bootstrap relay region 301, New York City` |
| `tailcat forward <addr> 18082:18081` + `curl` | **HTTP 200, 0.196 s on the first connection** |
| `tailcat ping <addr>` | `pong in 270 µs via 172.17.0.1:37951` — **direct UDP, no DERP relay** |
| 50 MB through the tunnel | 0.064 s ≈ **820 MB/s** (direct loopback same-host: 4.4 GB/s) — *same-host only, not a WAN number* |
| `tailcat serve 18000:192.168.122.36:8000` | **Rejected** by v0.7.0: `serve` takes ports/services only; the README's `port:host:port` serve-mapping is on `main`, not in the release |
| `forward <exit-node-addr> 18080:192.168.122.36:8000` | **Inconclusive** — the test guest's websockify had stopped listening at that moment (direct `curl` from carnyx also failed), so the timeout is not attributable to tailcat |

**Read that last row carefully.** The one test that would have proved
"tailcat reaches an arbitrary guest IP" did not produce a verdict, because the
target was down. Anyone implementing §7 must re-run it on a healthy guest before
choosing exit-node mode.

### What tailcat is not

- **Not browser-usable on its own.** No WebRTC yet (`tailcat#4`); the js/wasm
  demo relays over DERP only. A browser still needs a local `forward`/`browse`
  process on *some* machine it can reach at `127.0.0.1`.
- **Not a policy or identity system.** No ACLs, no users, no audit log. The
  address *is* the credential. Its own `SECURITY.md` says the threat model
  "hasn't historically included malicious adversaries" and that hardening for
  mutually untrusting parties is future work.
- **Not stable.** Wire-format changes are explicitly permitted between releases.

---

## 2. What lsbx has today (inventory, with the actual gaps)

Five live paths exist for getting a desktop to a human. They are not
alternatives to each other; they are layers.

| # | Path | Where the code is | What it does | Gap |
|---|---|---|---|---|
| 1 | In-guest bridge | `scripts/provision-win11-golden.ps1`, `scripts/build-desktop-golden.sh` | websockify on guest `:8000` ← TightVNC `:5900` (Windows) / x11vnc+Xvfb (Linux); serves noVNC assets itself | Binds `0.0.0.0:8000` inside a VM nobody off-host can route to |
| 2 | Gateway WS relay | `crates/lsbx-stream/src/proxy.rs` (`GUEST_VNC_PORT = 8000`), mounted by `crates/lsbx-gateway/src/lib.rs` | `GET /stream/{id}/{path}` upgrades WS and relays to `<record.host>:8000`; destination comes **only** from the state store | `/stream/*` requires a bearer header — **browsers cannot set headers on a WS handshake** (the gap `canonical-url` D7 plans to close with a short-TTL signed query token; not implemented) |
| 3 | `/console` page | `crates/lsbx-gateway/src/routes.rs`, `crates/lsbx-stream/src/console.rs` | Unauthenticated HTML page that resolves a sandbox and embeds its `console_url` in an iframe | Iframes the *guest IP* URL, which off-host browsers cannot load — so on carnyx this page is only useful from carnyx |
| 4 | `socat` one-shot | nothing in the repo — operator habit | `socat TCP-LISTEN:8008,bind=<carnyx tailnet IP> TCP:<guest>:8000` | No auth beyond tailnet ACL; one process per sandbox; no lifecycle tie-in to `down`/reap; silently forgotten |
| 5 | Tailnet subnet route | `docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md` | Advertise `192.168.122.0/24` via carnyx → every guest natively reachable | **Deferred on purpose**: it publishes *every* domain on libvirt's default network, including operator VMs (`win11`, `provision-win11`), and needs `ip_forward` + route approval + ACL changes |

Two structural facts behind all five:

- `SandboxRecord.host` is the **guest's private IP**; `console_url` is derived
  from it. Nothing in the record describes *how a remote client reaches that
  IP*, which is why every off-host path is a manual bolt-on.
- The only authenticated door (`lsbx serve`) binds carnyx's tailnet IP
  (`/etc/systemd/system/lsbx-serve.service` → `100.125.210.60:8243`). Clients
  outside the tailnet have no path at all today.

---

## 3. Comparison

Judged for one job: **a human, on a machine we may or may not control, opening a
disposable VM's desktop in a browser.**

| | socat (today) | `lsbx serve` `/stream` | tailnet subnet route | **tailcat** |
|---|---|---|---|---|
| Inbound port on carnyx | **Yes**, one per sandbox | Yes, one for all | No (routed) | **No — egress only** |
| Firewall/NAT traversal | Needs reachable host | Needs reachable host | Needs route approval | **Hole-punching; DERP fallback** |
| Works from a machine *not* on the tailnet | No | Only via public Caddy vhost + token | No (tailnet-only) | **Yes** |
| Auth model | tailnet ACL | bearer token (header — browser gap) | tailnet ACL | **address = bearer capability**; optional client-key allowlist |
| Per-sandbox isolation | one process, shared IP | store-mediated, port fixed | **none — whole /24** | **own key + own address per sandbox** |
| Blast radius if address leaks | n/a (ACL) | token | entire guest subnet | that one sandbox, until the process exits |
| Ops cost per sandbox | manual command | zero (systemd) | zero after setup | one supervisor child (if embedded) |
| Browser-compatible directly | Yes (plain TCP) | **No** (WS header auth) | Yes | **No** — needs a local `forward` |
| Maturity | boring | in-repo, tested | specced, deferred | **v0.7.0, no stability promise** |
| Extra dependency | none | none | none | Go binary (18 MB) or a Go shared lib |

### The honest summary

- tailcat's **real** win is not "serving". It is that it removes the
  requirement for the *other* end to be on our network or to reach an inbound
  port — which is exactly the property `exe.dev` used to give us and which
  `docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md` names
  as the one thing ProxyJump does not reproduce.
- It **loses** on the browser hop: browsers cannot speak tailcat, so a human
  still runs something locally. It gains on *who can reach what*, not on *fewer
  steps for a human who already has the tailnet*.
- Therefore: **tailcat complements the gateway; it does not replace it.** Using
  it as a replacement for `lsbx serve` would trade an authenticated,
  store-mediated, browser-native WS relay for "a secret string is the password".

---

## 4. Decisions

### D1 — tailcat is a **transport**, not a serving stack. Keep `/stream` and `/console`.
The gateway's WS relay resolves destinations from the state store and enforces a
token; that is the part we would regret losing. tailcat answers "how do bytes get
between two machines", not "what do we serve". It becomes **Door 5** — a new
plumbing option beside `socat`/tailnet/Caddy, feeding the same in-guest
websockify.

### D2 — Phase 1 integration is a **pinned subprocess wrapper**, not FFI.
`lsbx tailcat serve|connect` shells out to a version-pinned `tailcat` binary and
captures the address from `--json` (`{"listenAddr": …}`), never from the
`# 🐈 …` stderr line. Rationale: the CLI is the supported surface today; a
subprocess keeps the embedded Go runtime (and its "don't use after `fork`
without `exec`" rule and `tc_free` ownership rules) out of the Rust process; and
if tailcat's wire format churns, a version bump is a one-line change instead of
an ABI investigation. `LSBX_TAILCAT_BIN` overrides the path.

### D3 — FFI via `libtailcat` (C ABI v1) is **deferred**, behind a feature flag, as a timeboxed spike.
`cmd/libtailcat` exposes `tc_server_new/start/listen/accept`, `tc_conn_read/write`,
`tc_client_new/dial`, tokens for cancellation, and `tc_info` JSON. That is
genuinely enough to hand a tailcat listener to the existing stream proxy instead
of a TCP port. It is still deferred because: cgo + Go toolchain in CI; a Go
runtime embedded in every `lsbx` process; synchronous blocking calls that need
`spawn_blocking` bridging; and ABI v1 with no stability promise. **Flip
condition:** upstream ships stable ABI v2 *and* the subprocess supervisor
becomes the flaky part (supervision restarts, orphan cleanup) in practice.

### D4 — Preferred placement: **tailcat runs *inside* the golden** for desktop consoles; on the **host** only for lifecycle ops.
The guest already runs websockify on `:8000`, has outbound internet, and is
disposable — so `tailcat serve 8000` inside the guest yields one address per
sandbox, no host port, no exit-node complexity, and the address dies with the VM.
Windows and Linux static binaries both exist. The host-hosted route (carnyx
serving a mapped guest port) is kept for `exec`/`put`/`get`-style access, where
carnyx already holds the SSH keys. **Caveat:** the hero test for the host route
(exit-node + remote-IP forward) is the inconclusive one — re-run it before
choosing (see §1).

### D5 — Keys are **ephemeral per sandbox** by default.
`--key=new` (explicitly, so a saved `default` key on a host never silently
becomes a VM's identity). The address is recorded on the record and dies with
the process/VM. Saved keys are reserved for long-lived services, never for
disposable sandboxes.

### D6 — The address is treated as a **secret**, and an allowlist is mandatory when it leaves a private channel.
Because the address contains the PSK, whoever holds it connects. So: print it to
the operator's terminal, store it mode-0600 under the state dir, never log it
(tailcat's own docs say the same), never put it in a public DNS TXT record for a
sandbox, and if it must cross an untrusted channel, generate a client key with
`tailcat genkey --client` and pin it with `serve --allow=nodekey:…`. Never
`serve all`; never `no-auth-ssh`.

### D7 — DERP policy is a first-class config value.
A desktop console is a bandwidth-heavy, latency-sensitive stream. Tailscale's
public tailcat relays are explicitly rate-limited with no SLA and may be revoked.
So: `LSBX_TAILCAT_DERPMAP_URL` (and `--region` for a self-hosted `cmd/derper`)
must be settable by the operator, and the honest default narrative is "direct UDP
when hole-punching wins, your own DERP when it doesn't". Interactive use of
public relays is a fallback, not a plan.

### D8 — `socat` stays, documented as the zero-dependency escape hatch.
On a host that can already reach the guest (carnyx itself, or anything with the
subnet route later), one `socat` line beats installing a second binary. Keep it
in the docs as the "I am already on the host" path; do not delete it in favour
of tailcat.

---

## 5. What this buys, concretely

| Need | Today | With tailcat |
|---|---|---|
| Browser desktop from Klaxon (on tailnet) | socat on `:8008` | `lsbx tailcat connect <id> --open` → local `127.0.0.1:PORT`, one command, dies with the CLI |
| Browser desktop from a machine **not** on the tailnet (a friend's laptop, a colleague) | impossible without the public vhost | works — hand over one address + one command |
| Agent in a cloud sandbox (no tailnet, no root) reaching carnyx | public `lsbx.lufshq.com` + token | tailcat address; userspace, no root, no inbound port |
| Per-sandbox revocation | `lsbx down` (but the socat may linger) | address is process-scoped; `down` kills the serve child |
| Audit / policy | gateway token + logs | **nothing** — this is the regression to be honest about |

---

## 6. Interface sketch (contract only — **no code in this turn**)

```
lsbx tailcat serve <sandbox-id> [--port N] [--json]
    # Starts (or reuses) a tailcat server for one sandbox and prints
    #   {"sandbox_id": …, "tailcat_addr": "tc…", "port": N, "pid": …, "expires_at": …}
    # Address is persisted on the SandboxRecord (extra), never on stdout elsewhere.
    # Child is supervised and reaped by `lsbx down` and by the TTL reaper.

lsbx tailcat connect <sandbox-id|tc-addr> [--open] [--local-port N]
    # Equivalent to: tailcat forward --open-browser <addr> <local>:<remote>
    # Blocks until interrupted. `--open` opens the browser at the guest's
    # websockify root (/vnc.html?resize=remote).

lsbx tailcat list          # sandbox_id, addr prefix, port, pid, age
lsbx tailcat stop <id>     # kills the supervised child, drops the addr from the record
```

Env: `LSBX_TAILCAT_BIN` (path), `LSBX_TAILCAT_DERPMAP_URL`, `LSBX_TAILCAT_REGION`,
`LSBX_TAILCAT_ALLOW` (comma-separated `nodekey:…`), `LSBX_TAILCAT_KEY` (default `new`).

**Hard rules for the interface:** never proxy `all`; `/vnc.html` only; the address
is redacted in every log line and every `--json` output that a non-operator door
can see (gateway, MCP).

---

## 7. Unit decomposition sketch (for the implementation turn, not this one)

| Unit | Scope |
|---|---|
| **01-tailcat-transport-supervisor** | `lsbx-tailcat` crate or module: locate/pin binary, spawn, capture `--json` address, supervise, `down` integration, address redaction + 0600 persistence. No CLI surface yet. |
| **02-tailcat-cli-surface** | `lsbx tailcat serve/connect/list/stop`, env plumbing, `--open` UX, `SandboxRecord.extra` persistence, `reap` integration. |
| **03-golden-packaging** | Ship the pinned `tailcat` binary into the Windows and Linux desktop goldens, autostart `tailcat serve 8000` under the existing websockify supervision, healthcheck that the address is printed. |
| **04-libtailcat-spike** (optional, timeboxed) | Feature-flagged FFI proof: `tc_server_listen` + tokio `spawn_blocking` bridging into the stream proxy; measured against the supervisor for restart behaviour. Decide D3's flip. |
| **05-e2e-verification** | Klaxon (and one non-tailnet machine) → address → `forward` → browser → live Windows desktop; plus `lsbx down` leaves no tailcat process. Re-runs the inconclusive exit-node test on a healthy guest. |

Dependency order: 01 → 02 → 05; 03 can proceed in parallel with 02; 04 last.

---

## 8. Open questions

- **O1** Does `serve exit-node` + `forward … :remote-ip:port` work in v0.7.0 at
  all? (Inconclusive here — target was down.) If not, D4's host mode shrinks to
  "serve from inside the guest" only.
- **O2** For a VM guest, who owns the tailcat key lifecycle — the golden's boot
  script, or `lsbx` writing it into the guest via SSH? The former keeps the guest
  self-sufficient; the latter keeps the address known to lsbx without parsing
  guest logs.
- **O3** Do we want tailcat to also carry `lsbx exec`/`put`/`get` for agents
  outside the tailnet (replacing "public gateway + token")? That is a bigger
  trust decision than the console path and probably deserves its own spec.
- **O4** Rate-limit reality of the public DERP relays for a live VNC stream: worth
  one measurement on a genuinely remote client before we call it "fallback only".
- **O5** Should the gateway ever *hand out* a tailcat address (i.e. become an
  authenticator that mints a transport credential)? Attractive, but it makes the
  gateway a secret-distribution point; not in scope here.

---

## 9. Out of scope

- Reimplementing magicsock/DERP/disco/netstack in Rust (rejected: months of work
  duplicating an upstream that is itself unstable).
- Replacing `lsbx serve`'s WS relay, `/console`, or the gateway token model.
- The tailnet subnet route (`docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md`)
  — still deferred; tailcat is the cheaper experiment and does not require
  publishing `192.168.122.0/24`.
- tailcat's `ssh`/`no-auth-ssh`/`files`/`exec` services. lsbx already has SSH via
  `lsbx exec`; adding a second shell door is a liability.

---

## 10. Evidence log

All commands run on carnyx, 2026-11-14, against `tailcat v0.7.0`
(`tailcat_0.7.0_linux_amd64.tar.gz`, 18 MB, extracted to `/tmp`, since removed).
No VM was touched by these tests.

```
$ ./tailcat version
v0.7.0

$ ./tailcat serve --json 18081        # stdout
{"listenAddr":"tcpGFwWCCMYSRvuQSIWoQPZxJuNLnWQ7M_qaTAiaxCSDJmv0WgbWFrWCCjPJyHmxKnr2HdOMqw2FjS_-wJL5ZBErYncxpT-VPvfWFxWCDbY4mjMFjyp_wE7QcT79VyLNepFUL6jkSIQWJcGetRQ2FpGQEt"}
# stderr: # Selected bootstrap relay region 301, New York City

$ ./tailcat forward <addr> 18082:18081
# forwarding 127.0.0.1:18082 -> remote localhost:18081

$ curl -s -o /dev/null -w "%{http_code} %{time_total}\n" http://127.0.0.1:18082/index.html
200 0.196161

$ ./tailcat ping <addr>
pong in 270µs via 172.17.0.1:37951        # direct UDP, not DERP

$ curl 50 MiB through the tunnel:  52428800 bytes in 0.063836s  (~820 MB/s)
  same host, direct loopback:     52428800 bytes in 0.011879s  (~4.4 GB/s)

$ ./tailcat serve 18000:192.168.122.36:8000
invalid port or service to serve: … (want one of: all, ssh, no-auth-ssh, files, exec, exit-node)
# i.e. v0.7.0 serve has no port:host:port mapping; the README documents main-branch behaviour
```

### Startup-timing caveat worth carrying into implementation

Tailcat's first run took ~10 s before it printed an address (DERP region
selection + registration). Anything that boots a server and immediately expects
an address must wait for the `--json` line with a bounded timeout (call it ≥30 s)
rather than polling for readiness on a fixed sleep.
