# Unit 07 — gateway-display-resize-endpoint

## Objective
The browser console resizes the guest desktop to the browser viewport by POSTing
the viewport size to the gateway, which applies it through the mechanism unit 06
proved — so `#56` closes without an in-session launcher.

**Gated:** this unit is only in scope if `FINDINGS-06-vdd-resize.md` says the
session-0 path works. If it does not, stop and re-scope; do not implement a
half-working endpoint.

## Context
- Phase SPEC §5 **D7** (the decision this unit implements), §5 D6 (console role),
  §1 goal G5.
- Unit `06-vdd-session0-resize-and-qxl-coexistence.md` in this folder — **the
  verdict and the exact in-guest command this unit must call**. This unit invents
  no display mechanism; it only carries the request.
- `crates/lsbx-gateway/src/routes.rs` — existing routes `/console`,
  `/consoles/{id}/token`, `/health`; match their auth and error-envelope style.
- `crates/lsbx-stream/assets/console.html` — the page. It already has
  `rfb.resizeSession = RESIZE_MODE === 'remote'`, which is dead on TightVNC
  (SPEC §2.6); this unit adds the POST path instead of trying to revive that.
- `crates/lsbx-ops/` — the exec façade the gateway uses to run something in a
  guest; the display call must reuse it rather than open its own SSH.

## Acceptance criteria
- [ ] `POST /sandboxes/{id}/display` accepts `{"width": W, "height": H}` with the
      same bearer auth as `/consoles/{id}/token`, and returns `200` with the
      applied geometry on success.
- [ ] Unauthenticated → `401`; unknown sandbox → `404`; out-of-range or
      non-integer dimensions → `400` with a message naming the valid range.
- [ ] The endpoint is a **no-op returning 501 Not Implemented** for a sandbox
      whose golden lacks the `resize` capability — it must never silently succeed.
- [ ] The in-guest call is exactly the one unit 06 validated, invoked through the
      existing exec path; no new SSH client, no new transport.
- [ ] `console.html` sends at most one request per 200 ms of settling (trailing
      debounce), never while disconnected, and never for a size the guest already
      has.
- [ ] On a `501` or a transport failure the page falls back to
      `scaleViewport = true` and shows a non-blocking notice; the stream is not
      torn down.
- [ ] `images.carnyx.json` gains `"resize"` in `win11-desktop`'s `capabilities`
      **only if** the golden actually supports it after unit 02/06.
- [ ] A gateway test proves the debounce and the capability gate without a browser
      (unit-level test on the request scheduler + an integration test on the route
      with a mocked exec).
- [ ] `cargo test -p lsbx-gateway -p lsbx-stream` and workspace clippy pass.

## Interface contract
```
POST /sandboxes/{id}/display
Authorization: Bearer <gateway token>
Content-Type: application/json

{ "width": 1600, "height": 900 }

200 → { "data": { "sandbox_id": "...", "width": 1600, "height": 900, "applied": true } }
400 → { "error": { "code": "invalid_dimensions", "message": "width must be 640..=3840" } }
401 → { "error": { "code": "unauthorized", ... } }
404 → { "error": { "code": "not_found", ... } }
501 → { "error": { "code": "resize_unsupported", "message": "golden has no resize capability" } }
```

```js
// console.html — behaviour contract
const DEBOUNCE_MS = 200;      // trailing; never more than one POST per settle
// send only when: connected && capabilities include "resize" && size changed
// on 501 / network error: scaleViewport = true, show notice, keep the stream
```

## Boundaries
- **Touches:** `crates/lsbx-gateway/src/routes.rs`,
  `crates/lsbx-stream/assets/console.html`, `crates/lsbx-gateway/tests/`,
  `images.carnyx.json` (`win11-desktop.capabilities` only).
- **Does NOT touch:** `crates/lsbx-ops/` beyond calling its existing exec method
  (if a new ops method is genuinely required, that is a flagged deviation, not a
  silent addition — it changes the MCP parity count), the `Backend` trait,
  `scripts/provision-win11-golden.ps1` (the in-guest script is unit 02/06's),
  `crates/lsbx-stream/src/proxy.rs`.

## Verification
```bash
cargo test -p lsbx-gateway -p lsbx-stream
cargo clippy --workspace --all-targets --all-features -- -D warnings

# behavioural, with a browser
#   open /console?target=<id>, drag the window to 1600x900
#   expect: exactly one POST per drag; guest geometry follows
scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 12
#   expect a DesktopSize(-223) rect per settled resize
```
