# Phase Spec — one canonical URL: `lsbx.lufshq.com`

**Timestamp:** `2026-11-07T1440Z`
**Slug:** `canonical-url-lsbx-lufshq`
**Status:** Proposed. Decides naming and the lsbx changes it forces; does not implement.
**Related:** `~/repos/molimo/docs/pathing.md` (the vhost decision), `danialrami/molimo` `group_vars/calimbra_vps/caddy_services.yml`, `docs/specs/2026-11-07T1440Z_config-file-toml/SPEC.md` (D-schema for `[gateway]`/`[stream]`).

---

## 0. Why the fleet is on named vhosts (the question re-asked)

Not Tailscale. Recorded in `~/repos/molimo/docs/pathing.md`, decided 2026-11-01:

1. **Cloudflare's proxy only connects to a fixed set of origin ports** — 80,
   8080, 8880, 2052, 2082, 2086, 2095 (http), 443, 2053, 2083, 2087, 2096, 8443
   (https). Every service we run sits on 3000–8750, so a port-addressed URL
   **cannot sit behind Cloudflare at all** — the thing that worked on exe.dev
   (which exposes and terminates 3000–9999 itself) is structurally impossible
   on calimbra.
2. **The zone certificate covers exactly one label** — `CN=lufshq.com,
   SAN=*.lufshq.com`, and a wildcard matches one label only. So flat names
   (`kb.lufshq.com` ✅) and never `x.y.lufshq.com` (❌ alert 40, no cert).
3. TLS at the origin is Caddy's **internal CA** on purpose: a proxied origin
   cannot answer an ACME HTTP-01 challenge, and trying burned 12 attempts per
   10 minutes plus 32 rate-limit waits.

Tailnet access is unaffected and parallel: `http://100.120.220.48:<port>` keeps
working for every service, and `http://100.125.210.60:8243` keeps working for
the carnyx gateway. The vhost is the *public* name, the port is the *agent*
name. Nothing needs to be switched off to adopt the new one.

## 1. Current state (measured 2026-11-07)

| name | DNS | public TLS | result |
|---|---|---|---|
| `lsbx.lufshq.com` | ✅ **created 2026-11-07** — Cloudflare A, origin 113.20.2.11, proxied (172.67.139.239, 104.21.65.28) | pending Caddy block | 401 once the registry is applied |
| `lsbx-carnyx-gateway.lufshq.com` | ✅ proxied | ✅ | 401 unauthenticated, 200 with token (pre-rename baseline) |
| `lsbx-carnyx-streams.lufshq.com` | does not resolve | — | registry entry removed 2026-11-07 (D2) |
| `http://100.120.220.48:8246` (port listener) | n/a | n/a | 200 with token |
| `*.lufshq.com` wildcard DNS | **not configured** | — | `randomtest12345.lufshq.com` does not resolve |

Note on reading the DNS result: a *local* resolver that already cached the
pre-creation NXDOMAIN answers nothing for up to the zone's negative TTL. Verify
with `dig +short @1.1.1.1 lsbx.lufshq.com`, not with the default resolver, and
prefer the public hostname over `--resolve` for curl tests (the origin cert is
Caddy-internal by design — see §0.3).

## 2. Decisions

| # | Question | Decision | Why |
|---|---|---|---|
| D1 | Name | **`lsbx.lufshq.com`.** One name for the whole gateway: control plane, `/stream/*` WebSocket relay, and `/console`. | The Rust `serve` already merges gateway and stream onto one listener by default (`lsbx-cli/src/lib.rs:1197`), so the two-name split (`-gateway` / `-streams`) describes a topology that no longer exists. One name, one cert, one log. |
| D2 | `lsbx-carnyx-streams` entry | **Delete it.** Also delete the `:8247` port listener if the role rendered one. | Its upstream (`carnyx:8244`) has no listener and never will — carnyx binds only `:8243`, where `/stream/*` is already served. It is a 502 generator with a log slug. |
| D3 | `lsbx-carnyx-gateway` vhost | **Remove it. No alias.** The old name's Cloudflare record is already gone (NXDOMAIN at 1.1.1.1 as of 2026-11-07), so an alias block would serve nothing; a client holding a cached address now gets Cloudflare 530 (origin DNS error) regardless of what Caddy does. | An alias only has value if the name resolves. Keeping a vhost for a name with no DNS record is dead config that someone will later mistake for an active path. One name, one name only — rollback is re-adding the record, which is a two-minute DNS change, not something to pre-stage. |
| D4 | `l-sbx` entry (`:8243` → molimo `:8244`) | **Leave running while molimo's gateway stays up, but never give it a vhost.** | It points at the box being deprecated. `pathing.md` already reached this conclusion; the 401-everything state of molimo's gateway makes it unusable anyway. |
| D5 | Port listeners `:8246` / `:8243` | **Keep `:8246`** (tailnet agents use it). `:8243` dies with the molimo route. | Port listeners are the agent path and are explicitly out of scope for removal. |
| D6 | Where the hostname lives in lsbx | **`[gateway] public_base_url` in the new TOML config**, env `LSBX_PUBLIC_BASE_URL` to override. | The gateway must not hardcode a host; the same binary serves carnyx (tailnet + vhost) and any future host. |
| D7 | How the browser authenticates the WebSocket | **Short-lived signed query token.** `/console?id=<sandbox>` (itself bearer-gated) mints a token scoped to that one sandbox and stream, with a ≤60 s TTL, HMAC'd with the gateway secret; `/stream/<id>/<path>` accepts `?t=` in addition to the bearer header. | Browsers cannot set headers on a WebSocket handshake. A signed token keeps the gateway stateless (no session store, no cookie jar) and bounds replay to the TTL. Rejected: session cookies (state + CSRF surface), `?token=<the gateway token>` in the URL (leaks the master secret into logs, history, and referrers). |

## 3. The lsbx changes the rename forces (the real work)

The rename itself is four lines of YAML. These are the gaps that make it *look*
like it needs more, and one of them is a genuine defect:

1. **`https_url` is always `None` on the libvirt backend**
   (`crates/lsbx-backend-libvirt/src/lib.rs:504`). `SandboxRecord::public()`
   derives `console_url` only from `(streaming == "novnc", https_url)`
   (`crates/lsbx-kernel/src/types.rs:41-45`), so **a noVNC golden on carnyx can
   never report a console URL** — `desktop` and `win11` profiles both stream,
   both come back `console_url: null`. The stream relay and the guest's
   `:8000/vnc.html` both exist; nothing joins them.
2. **`/console` is a stub** that redirects to `console_url`
   (`crates/lsbx-gateway/src/routes.rs:992-1015`). With (1), it redirects to
   nothing.
3. **`console_url` has no notion of the public name.** Even with `https_url`
   populated, the URL would be the guest's `192.168.122.x:8000`, which is
   unreachable from anywhere but carnyx.

Proposed shape, in dependency order:

- **A.** Add `[gateway] public_base_url`. When set, `SandboxRecord::public()`
  (or a thin gateway-side mapper) renders
  `console_url = "<public_base_url>/console?id=<sandbox_id>"`.
- **B.** Make `/console` a real page: load any static noVNC assets, read `id`
  from the query, and point the client at
  `wss://<host>/stream/<id>/websockify` — the route
  `lsbx-stream`'s router already registers (`crates/lsbx-stream/src/lib.rs:67`),
  and the shape the guest's websockify expects.
- **C.** Stop requiring `https_url` for streaming: the gateway knows the guest
  destination; the record only needs `streaming: novnc` and the guest port
  (`[stream] guest_port`, default 8000 — already the unit's value).
- **D.** Auth on `/stream/*` and `/console`: the WS upgrade must require the
  same bearer token (or a short-lived signed query token, since browsers cannot
  set headers on a WebSocket). **This is a security requirement, not a
  nicety** — today the relay path is registered on the same router as the
  authenticated routes, and the current Caddy vhost publishes it.

## 4. Caddy / DNS execution (danialrami/molimo) — PATCHED 2026-11-07, not yet applied

Cloudflare: **`lsbx.lufshq.com` A record created 2026-11-07** (origin
113.20.2.11, proxied) — verified with `dig +short @1.1.1.1 lsbx.lufshq.com`
→ `172.67.139.239, 104.21.65.28`.

`group_vars/calimbra_vps/caddy_services.yml`, edited (uncommitted at time of
writing):

```yaml
  - name: LSBX Carnyx Gateway
    upstream: 100.125.210.60
    port: 8243
    proxy_port: 8246          # kept: tailnet agents
    vhost: lsbx.lufshq.com    # canonical
    enabled: true
    log_slug: lsbx-carnyx-gateway
```

- `LSBX Carnyx Streams` and the pre-rename name are **both gone**: the streams
  entry deleted (D2), and the `lsbx-carnyx-gateway` alias dropped (D3) because
  its DNS record no longer exists. Registry is 36 entries; YAML re-parses clean
  with no duplicate names.
- The Jinja template renders `lsbx.lufshq.com` + the `:8246` port block, with no
  `:8247` and no streams block. Verified by rendering `roles/caddy/templates/Caddyfile.j2`
  locally against the edited registry.
- The **live** Caddyfile on calimbra still contains the old
  `lsbx-carnyx-gateway.lufshq.com` block until the role runs — harmless (the name
  does not resolve), but it means the check "is the old block gone?" is a
  post-apply assertion, not a pre-apply one.
- Still to do: commit + `ansible-playbook` on calimbra (a host write; the role
  refuses to reload Caddy unless the rendered file validates), then the §5 checks
  against `lsbx.lufshq.com`. Keep `tls internal` — proxied origin.
- Verification gotcha: kora's resolver (systemd-resolved → tailnet DNS
  100.100.100.100) cached the pre-creation NXDOMAIN, so `curl` on kora still says
  "could not resolve" while `dig @1.1.1.1` answers. Flush with
  `resolvectl flush-caches` (a pkexec on kora) before calling a name broken.

## 5. Verification

- `dig +short lsbx.lufshq.com` → Cloudflare addresses.
- `curl -s -o /dev/null -w '%{http_code}' https://lsbx.lufshq.com/health` → **401**.
- Same with `Authorization: Bearer $LSBX_GATEWAY_TOKEN` → 200 with
  `backend_name: libvirt`.
- `GET /sandboxes` lists a live sandbox created through the CLI (already proven
  for `lsbx-carnyx-gateway` on 2026-11-07: 200 from both `:8246` and the vhost).
- Console: create a `desktop` profile sandbox, `lsbx info` shows a
  `console_url` under `https://lsbx.lufshq.com`, and the page connects to the
  guest's websockify through `/stream/<id>/websockify`.
- Negative: `/stream/<id>/websockify` **without** a token must be rejected
  (D above).

## 6. Open questions — weighed

### O2 — RESOLVED 2026-11-07: Option A (401 everywhere; `/health` stays authenticated)

Banked as Option A: no landing page, and **`/health` keeps requiring the
bearer token**. Rationale: the gateway's whole contract is "token or nothing",
a human-facing page is a second public surface to keep truthful, and the
useful questions ("is it up", "what is it") are already answerable with
`curl -H "Authorization: Bearer …" /health` from any host that has the token.

The options remain recorded here because Option C (public `/health`) is a
one-line change if uptime monitoring ever needs a credential-free probe, and
Option B (a page) becomes interesting only if the *authenticated homepage* idea
below is taken up.

**Related idea, deliberately not in scope:** an authenticated homepage where a
human provisions themselves a sandbox ("log in, pick a golden, get a URL").
`lsbx` already has a TUI (`crates/lsbx-tui`, `ratatui`) for interactive
provisioning over SSH, and the gateway has every operation the page would need
(`POST /sandboxes`, `GET /sandboxes/{id}/console`). What is missing is per-user
identity — the single shared token cannot express "who provisioned this" — so
that page belongs with the per-caller-token work, not with the URL rename. Not
specced here; noted so the two do not get conflated.

**Option A — 401 everywhere but the API.** `/` returns the same 401 as every
other route.
  Pros: nothing public to fingerprint or index; no HTML surface, no CSP, no
  asset pipeline, no template to keep in sync with reality; the 401 *is* the
  health signal ("the gateway is up and refusing you"); smallest possible code.
  Cons: a human who opens the URL from a phone sees a bare 401 and reasonably
  concludes the service is down; discovering `/console` requires knowing the
  shape; no place for a version/build id, which is exactly what you want when
  debugging "which binary is behind this name".

**Option B — a minimal landing page at `/`.** Unauthenticated HTML naming the
service, its version, and links to `/health` and `/console`.
  Pros: a human gets an answer in one request; matches molimo's edge behaviour
  (`respond "calimbra edge — services are on their own ports" 200`); makes the
  name self-describing for future-you at 2 a.m.
  Cons: adds a public HTML route to a service whose whole point is that it is
  token-gated; if the name ever gets indexed or shared, the page advertises the
  API's existence; version strings on a public page are a free fingerprint;
  one more thing to keep truthful.

**Option C — no page, but make `/health` unauthenticated.** Keep `/` at 401;
let `/health` answer `{"backend_name","sandbox_count"}` without a token.
  Pros: humans and uptime monitors get a cheap, non-HTML answer; no public HTML
  surface; `/health` is already the one route whose body is non-sensitive
  (`backend_available`, `backend_name`, `sandbox_count`) — it is the natural
  exception, and `auth.rs` already documents that only the one console route is
  unauthenticated today, so this is a one-line precedent rather than a new
  concept.
  Cons: `sandbox_count` on the public internet is a (small) operational leak —
  it tells an observer whether the box is busy; would need to drop that field or
  accept it.

**Weighing.** B buys the least: the useful part of a landing page is "is it
alive and what is it", which C answers with no HTML. A is the safest but makes
troubleshooting cost a token. C is the only option that improves observability
without adding a public surface — with the caveat that `/sandbox_count` should
probably be trimmed from the public body. **Recommendation: C, falling back to A
if even a count is too much.** B only becomes right if a human-facing entry
point is genuinely wanted (e.g. a link roster for all lufshq services), and that
belongs on the edge host, not on lsbx.

### O3 — do broker URLs ever touch the public name?

**Option A — broker stays entirely off the public name (current leaning).**
Workers and the broker reach carnyx over the tailnet (`100.125.210.60:8243`) or
locally, never through `lsbx.lufshq.com`.
  Pros: the broker is the one component that creates and destroys machines; its
  requests stay on a network only Daniel's devices can reach. The gateway has a
  **single shared token** with no per-caller identity — publishing the endpoint
  that token guards, for a polling loop that runs 24/7, is the worst
  exposure-to-auth-strength ratio in the system. It also removes Cloudflare and
  calimbra from the broker's failure domain: a CI poll does not need two extra
  hops whose failure looks like "GitHub is broken". And the broker runs *on*
  carnyx today, so the public name would be a round trip through the internet to
  reach a process on the same host.
  Cons: nothing works from a runner that is not on the tailnet; a GitHub-hosted
  runner (or a future cloud broker) could not use it; there is no public "job
  status" view.

**Option B — broker uses the public name.**
  Pros: uniform with every other service; works from anywhere including
  GitHub-hosted runners; one URL to document.
  Cons: exposes the strongest-impact, weakest-auth endpoint to the internet;
  adds latency and two failure domains to a poll loop; and it forces the shared
  token to be mounted in whatever runs the broker, widening where that secret
  lives.

**Weighing.** A is right, and the *reason* is auth strength, not secrecy: the
moment the gateway grows per-caller tokens with scoped commands (the exe.dev
model), B becomes cheap and safe. Until then, the deciding question is "does any
runner live off the tailnet?" — today, no: the CI runners are libvirt guests on
carnyx, which reach the host natively. **Recommendation: A, recorded as
conditional** — revisit if a cloud-hosted runner is ever added, and do the
per-caller-token work *first* if so.

### O1 — RESOLVED (D7)

Signed short-lived query token for the WS handshake. No cookie, no session
store, never the master token in a URL.

## 7. Out of scope

- Load balancing across several lsbx hosts (one carnyx; deferred until there is
  a second host, and it needs a shared state store first — see the calimbra spec).
- The `l-sbx` route's removal (dies with exe.dev).
- Per-sandbox hostnames (`<id>.lsbx.lufshq.com`) — the two-label certificate
  problem in §0 rules this out without a paid Advanced Certificate.
