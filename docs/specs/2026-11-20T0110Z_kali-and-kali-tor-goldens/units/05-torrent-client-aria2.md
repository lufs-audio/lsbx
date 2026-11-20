# Unit 05 — Torrent client: `aria2c`, gated to open mode, no wiring

## Objective
Install `aria2c` in the `kali` golden only, as an available tool, with no torrent configuration,
no tracker/DHT tuning, and no build-time behaviour that uses it.

## Context
- Intake decision: the torrent client is installed but **torrenting is deliberately out of scope**
  for this phase. `aria2c` is chosen over `transmission`/`qbittorrent` because it is a single
  binary with no daemon and no persistent service — nothing to leave running.
- Technical constraint that motivates the mode gate: BitTorrent over Tor is broken and
  discouraged. DHT discovery, UDP trackers, and peer connections do not route through
  `proxychains`, so a torrent client in the `locked` golden would leak. `aria2c` therefore ships
  **only** in `kali` (no Tor daemon present at all), never in `kali-tor`.
- Do not add any repository or package beyond the Kali default set for this unit.

## Acceptance criteria
- [ ] `aria2c --version` succeeds in `kali`.
- [ ] `aria2c` is **absent** from `kali-tor` (`command -v aria2c` returns non-zero).
- [ ] No torrent config file is created; `~/.aria2/` does not exist in the built golden.
- [ ] No systemd unit, timer, or service for `aria2c` exists.
- [ ] `/etc/motd.d/00-kali-no-tor` exists in `kali` and states plainly that Tor is absent and
      that `kali-tor` is the torified golden.
- [ ] No tracker URLs, DHT bootstrap nodes, or peer lists are written anywhere in the image.

## Interface contract
```bash
# Package: aria2  (Kali repo)
# Binary:  /usr/bin/aria2c
#
# Presented as a tool only. Documented usage requires the golden with no Tor:
#   lsbx up kali
#   lsbx exec <id> -- aria2c --seed-time=0 <url>
#
# Explicitly NOT provisioned: ~/.aria2/aria2.conf, any tracker list, DHT config,
# any service unit, any scheduled job.
```

## Boundaries
- **Touches:** `scripts/build-kali-golden.sh` (`open` mode only — the `aria2c` block and the
  `00-kali-no-tor` motd file).
- **Does NOT touch:** `crates/**`, Tor rules, browser packages, desktop unit, `images.carnyx.json`.

## Verification
```bash
lsbx exec <kali-id>      -- aria2c --version
lsbx exec <kali-tor-id>  -- command -v aria2c ; echo "expect non-zero: $?"
lsbx exec <kali-id>      -- ls -d ~/.aria2 2>/dev/null ; echo "expect absent"
lsbx exec <kali-id>      -- cat /etc/motd.d/00-kali-no-tor
lsbx exec <kali-id>      -- systemctl list-units --all | grep -i aria2 ; echo "expect no match"
```
