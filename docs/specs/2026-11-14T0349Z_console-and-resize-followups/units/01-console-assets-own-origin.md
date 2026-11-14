# Unit 01 — Console assets on the gateway's own origin

## Objective
Serve the real noVNC client bundle from `lsbx serve` and point its WebSocket at
`/stream/<id>/vnc` on the same origin, so a browser can open a sandbox desktop
without ever being told the guest's private IP.

## Context
`crates/lsbx-stream/assets/` contains exactly one file, `novnc-console.html`,
whose own doc comment says the real client was never vendored. Consequences:

- `GET /console?target=<id>` renders an iframe of `console_url`, which for the
  libvirt backend is `http://192.168.122.x:8000/vnc.html` — an address that exists
  only on carnyx's `virbr0`. Off-host, the iframe is a dead frame.
- The gateway can relay WS bytes (`/stream/{sandbox_id}/{guest_path}` →
  `<record.host>:8000`, port pinned by `GUEST_VNC_PORT`) but cannot serve the page
  that knows how to use them.

The bundle to vendor is upstream noVNC, currently **1.5.0, 2.3 MB, 208 files**
(measured in the running golden at `C:\lsbx-novnc\noVNC-1.5.0`).

Relevant prior art: `docs/specs/2026-11-07T1440Z_canonical-url-lsbx-lufshq/SPEC.md`
§3 "B. Make `/console` a real page … point the client at
`wss://<host>/stream/<id>/websockify`", and its D7 (short-TTL signed query token,
because browsers cannot set an `Authorization` header on a WebSocket handshake).

## Acceptance criteria
- [ ] `crates/lsbx-stream/assets/novnc/` contains the vendored upstream bundle with
      a pinned version and a recorded upstream provenance/checksum file.
- [ ] The gateway serves the bundle itself (static files from the embedded assets,
      no guest involved) under a stable path prefix.
- [ ] `GET /console?target=<id>` returns a page that loads **no asset from the
      guest's IP**; its WS URL is same-origin `/stream/<id>/vnc`.
- [ ] The page works identically when reached via (a) carnyx `:8243` directly,
      (b) `https://lsbx.lufshq.com` through Caddy+Cloudflare, (c) loopback.
- [ ] A browser that cannot present an `Authorization` header can still connect:
      the page obtains a short-lived, sandbox-scoped token from the gateway and
      presents it as a query parameter; the WS route accepts `?t=` in addition to
      the bearer header, verifying signature + expiry + sandbox scope.
- [ ] An unauthenticated or expired token is rejected with 401 and never upgrades.
- [ ] No guest IP, key path, or gateway master token ever appears in the served
      page or its JS.
- [ ] `cargo test --workspace` green; new tests cover (i) asset route serves the
      real bundle not the placeholder, (ii) the console page contains no
      `192.168.` occurrence, (iii) token mint/verify round-trip incl. expiry and
      wrong-sandbox rejection.

## Interface contract
```
GET  /console?target=<sandbox-id>          -> HTML page (unauthenticated, as today)
GET  /assets/novnc/<path...>               -> vendored noVNC static files
GET  /stream/{sandbox_id}/{guest_path}     -> WS upgrade; auth via
                                              Authorization: Bearer <master>
                                              OR ?t=<signed token>
POST /consoles/{id}/token                  -> {"t": "...", "expires_at": "..."}
                                              (bearer-authenticated; mints a token
                                               scoped to that sandbox + the stream path)

// token claim set (HMAC over the gateway secret):
{ "sub": "<sandbox-id>", "scope": "stream", "exp": <unix-ts> }   // TTL <= 60 s
```

## Boundaries
- **Touches:** `crates/lsbx-stream/assets/**`, `crates/lsbx-stream/src/console.rs`,
  `crates/lsbx-stream/src/lib.rs`, `crates/lsbx-gateway/src/{auth.rs,routes.rs,lib.rs}`,
  `crates/lsbx-gateway/Cargo.toml` (static-file serving), `crates/lsbx-ops/src/lib.rs`
  only if a token-minting op is unavoidable (prefer keeping it in the gateway).
- **Does NOT touch:** `crates/lsbx-backend-libvirt/**`, `scripts/**`, the goldens,
  `crates/lsbx-broker/**`.

## Verification
```bash
cargo test --workspace
ID=$(lsbx up win11 --lease 15m --json | jq -r .id)

# no guest IP leaks into the page, and assets are same-origin
curl -s "http://127.0.0.1:8243/console?target=$ID" | grep -c '192.168'          # expect 0
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8243/assets/novnc/vnc.html   # 200

# token path: mint then upgrade without a bearer header
T=$(curl -s -X POST -H "Authorization: Bearer $TOK" \
      "http://127.0.0.1:8243/consoles/$ID/token" | jq -r .t)
curl -s -i -N --http1.1 --max-time 5 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' -H 'Sec-WebSocket-Version: 13' \
  "http://127.0.0.1:8243/stream/$ID/vnc?t=$T" | head -1                          # 101
```
