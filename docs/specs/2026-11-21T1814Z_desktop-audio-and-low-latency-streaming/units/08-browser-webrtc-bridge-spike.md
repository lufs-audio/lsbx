# Unit 08 — browser-webrtc-bridge-spike

## Objective
Determine, within one working day, whether a browser tab can carry a sandbox
stream with audio at a latency close enough to Moonlight to be worth maintaining —
and if not, say so plainly and stop.

## Context
- Phase SPEC §3.3 (the candidate bridges and what each is), §5 **D8** (the
  decision this unit resolves, including the honest note that none of these
  reaches monitoring-grade latency), §5 D9 (no new public exposure).
- Units `04-gateway-slot-relay.md` and `05-latency-and-encoder-measurement.md` in
  this folder — the slot and the measurement method this unit reuses. Do not
  invent a second latency methodology; reuse `measure-moonlight-latency.sh`.
- Host facts: `lsbx.lufshq.com` terminates HTTPS (Cloudflare → calimbra → carnyx
  gateway), so a secure context is available; the gateway already serves
  `/console` and `/assets/*`.

## Acceptance criteria
- [ ] A runnable deployment exists under `deploy/moonlight-web/` (compose file or
      equivalent) that starts **one** candidate bridge, with a README stating
      pinned image/commit and required env.
- [ ] The bridge is reachable only over the tailnet (or behind the existing
      authenticated gateway); no new public listener, no port opened on the WAN
      interface.
- [ ] A browser tab connects to a sandbox slot and produces **video**; recorded
      with the browser name/version and the bridge's own logs.
- [ ] **Audio** is confirmed present in the tab, or the finding states exactly
      which part of the chain dropped it.
- [ ] Latency is measured with the unit-05 method and compared to the Moonlight
      number in `MEASUREMENTS-05.json`; the delta is recorded.
- [ ] Keyboard capture is tested, including at least one combination the browser
      normally intercepts (e.g. `Ctrl+W` or `Alt+Tab`) and the result recorded.
- [ ] `FINDINGS-08-browser-bridge.md` ends with a single recommendation:
      **adopt** (with the maintenance cost stated), **park** (with the specific
      condition that would revive it), or **reject** (with the reason) — and names
      Guacamole/RDP as the fallback if the answer is not "adopt".
- [ ] Timebox honoured: if the day ends without video in a tab, the finding is
      "park/reject" and no further work is done in this unit.

## Interface contract
```
deploy/moonlight-web/
  compose.yml          # one bridge service, pinned digest, tailnet-only bind
  .env.example         # bridge URL, gateway token env NAME (never a value)
  README.md            # how to run, what it exposes, how to stop it
```

```
Browser URL: https://<gateway-host>/moonlight-web/   (or the bridge's own
             tailnet address, if it is not proxied through the gateway)
Auth:        the gateway's existing bearer/ticket model; the bridge must not
             introduce a second, weaker auth surface (SPEC D9)
```

## Boundaries
- **Touches:** `deploy/moonlight-web/` (new), and
  `docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/FINDINGS-08-browser-bridge.md`
  (new).
- **Does NOT touch:** any `crates/` source, `crates/lsbx-stream/assets/`,
  `scripts/provision-win11-golden.ps1`, `images.carnyx.json`, the host's
  `lsbx-serve.service`, any firewall or DNS configuration.

## Verification
```bash
docker compose -f deploy/moonlight-web/compose.yml config   # parses, no secrets
docker compose -f deploy/moonlight-web/compose.yml up -d
# then, in a browser: connect, confirm video + audio, run the latency method,
# then
docker compose -f deploy/moonlight-web/compose.yml down
ss -tulpn | grep -v 127.0.0.1   # no leftover listener from the spike
```
