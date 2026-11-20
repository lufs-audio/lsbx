# Unit 04 — Browser layer: Chromium + Playwright bundled Chromium + optional Firefox ESR

## Objective
Install an agent-drivable browser stack in both goldens: a visible Chromium whose CDP endpoint
is reachable **only on loopback**, plus Playwright's own bundled Chromium for scripted
automation, with optional Firefox ESR for media-heavy sites.

## Context
- `playwright.dev/docs/browsers` (verified 2026-11-20): Playwright supports Chromium, Firefox and
  WebKit, plus branded `chrome`/`msedge` channels. **Playwright's Firefox is a patched build —
  it does not drive branded Firefox.** Playwright installs to `~/.cache/ms-playwright`
  (~281 MB Chromium, ~187 MB Firefox, ~180 MB WebKit) and `npx playwright install --with-deps chromium`
  brings system dependencies.
- Two separate things are being provisioned, and the distinction is load-bearing:
  1. the **system** Chromium the human/agent sees in the noVNC desktop;
  2. **Playwright's bundled** Chromium, used headless or headed with `DISPLAY=:0`.
- Security constraint: a `--remote-debugging-port` is remote code execution in the browser's
  context. It must bind loopback and be reached only through `lsbx exec`/SSH.
- Kali ships Chromium in its repositories; Firefox ESR is the expected default desktop browser.
  Codec caveat: pure Chromium lacks several codecs some video sites need; that is the argument
  for the optional Firefox ESR, not for Google Chrome.

## Acceptance criteria
- [ ] `chromium --version` succeeds in both goldens.
- [ ] Chromium launches inside the XFCE session (visible over noVNC).
- [ ] CDP is reachable at `127.0.0.1:9222/json/version` **from inside the guest**.
- [ ] CDP is **not** reachable from the host at `192.168.122.x:9222` (connection refused).
- [ ] `npx playwright install --with-deps chromium` completes; `~/.cache/ms-playwright` contains
      a chromium build.
- [ ] A `connectOverCDP` call against `http://127.0.0.1:9222` succeeds and returns a page that
      navigates to `https://example.com`.
- [ ] Chromium runs as user `lsbx`, never as root, with no `--no-sandbox` flag.
- [ ] Firefox ESR is installed in both goldens and reports a version.
- [ ] Neither golden installs Google Chrome or adds a third-party apt repository.

## Interface contract
```bash
# /usr/local/bin/lsbx-chromium
#   launches the system Chromium with:
#     --remote-debugging-address=127.0.0.1 --remote-debugging-port=9222
#     --user-data-dir=/home/lsbx/.config/lsbx-chromium
#   must refuse to run as root (exit 1)
#
# ~/.cache/ms-playwright/chromium-<rev>/   Playwright's bundled build
#
# CDP endpoint (loopback only):  http://127.0.0.1:9222/json/version
```

## Boundaries
- **Touches:** `scripts/build-kali-golden.sh` (browser block).
- **Does NOT touch:** `crates/**`, Tor rules, desktop service unit, `aria2c`, `images.carnyx.json`.

## Verification
```bash
lsbx exec <id> -- chromium --version
lsbx exec <id> -- curl -fsS http://127.0.0.1:9222/json/version
lsbx exec <id> -- ss -lntp | grep 9222          # expect 127.0.0.1 only
curl -fsS --max-time 3 http://<guest-ip>:9222/json/version ; echo "expect failure from host"
lsbx exec <id> -- ls ~/.cache/ms-playwright
```
