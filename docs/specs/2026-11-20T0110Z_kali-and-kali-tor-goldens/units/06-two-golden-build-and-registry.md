# Unit 06 — Two-golden build, registry, memory bump, base bump, healthchecks

## Objective
Build both goldens from the Unit 01 script, register `kali` and `kali-tor` with their profiles,
4 vCPU / 8 GB, distinct and mode-correct healthchecks, and — as a separate final step — move the
base from Kali 2026.2 to 2026.3.

## Context
- Depends on Units 01–05: the script must already reproduce v1 and carry the desktop, browser,
  torrent, and Tor-mode blocks.
- `lsbx golden build --help` — required `--from`, `--script`, `--flavor`, `--cpu`, `--memory`,
  `<NAME>`; optional `--os`, `--streaming`, `--register`, `--dry-run`.
- Registry: `images.carnyx.json` (`goldens[]` + `profiles{}`), read by
  `crates/lsbx-golden/`. Key regex `^[a-z][a-z0-9._-]{0,63}$`; base regex disallows dots beyond
  the `.qcow2` suffix.
- **Migration hazard** (SPEC §4): `kali` becomes the no-Tor golden and `kali-tor` takes over the
  Tor role. This unit is what actually performs the inversion, so its healthchecks are the
  safety mechanism.
- Retained bases: `kali-linux-2026.2-cloud-genericcloud-amd64.tar.xz` (2026.2) and the upstream
  2026.3 archive. Base bump is the **last** step, and may be deferred (SPEC §7).
- Reference shape for a desktop golden: `agent-web` in `images.carnyx.json`.

## Acceptance criteria
- [ ] `lsbx golden build --dry-run` succeeds for both goldens before any real build.
- [ ] Real build produces two standalone flattened qcow2 files with no dangling backing refs.
- [ ] `lsbx golden list` shows `kali` and `kali-tor`, each `cpu: 4`, `memory: 8GB`,
      `flavor: desktop`, `streaming: novnc`.
- [ ] `profiles` in `images.carnyx.json` contains both `kali → kali` and `kali-tor → kali-tor`.
- [ ] `kali-tor` healthcheck includes the existing Tor assertion verbatim:
      `curl -fsS --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip`.
- [ ] `kali` healthcheck includes the **negative** assertion `! systemctl is-active --quiet tor`
      and does **not** include any Tor egress check.
- [ ] Both include: `systemctl is-active --quiet lsbx-desktop.service`,
      `curl -fsS --retry 10 --retry-delay 1 --retry-connrefused http://127.0.0.1:8000/vnc.html`,
      `chromium --version`.
- [ ] `lsbx golden verify kali` and `lsbx golden verify kali-tor` pass every healthcheck.
- [ ] `lsbx up kali` yields a VM where `tor` is absent; `lsbx up kali-tor` yields one where Tor
      egress is proven.
- [ ] Content hash recorded in `images.carnyx.json` for both (currently `null` for all goldens).
- [ ] Base bump, if performed: both goldens rebuilt on the 2026.3 archive and re-verified; if
      deferred, this criterion is marked deferred in the unit's closing note.

## Interface contract
```json
// images.carnyx.json — goldens[] entries and profiles{}
{
  "key": "kali",
  "flavor": "desktop",
  "os": "linux",
  "base": "lsbx-kali-open-v1",
  "mode": "copy",
  "cpu": 4,
  "memory": "8GB",
  "streaming": "novnc",
  "capabilities": ["security", "desktop", "computer-use", "browser", "torrent"],
  "healthcheck": [
    "chromium --version",
    "systemctl is-active --quiet lsbx-desktop.service",
    "curl -fsS --retry 10 --retry-delay 1 --retry-connrefused http://127.0.0.1:8000/vnc.html >/dev/null",
    "! systemctl is-active --quiet tor",
    "aria2c --version",
    "test -f /etc/motd.d/00-kali-no-tor"
  ]
}
{
  "key": "kali-tor",
  "flavor": "desktop",
  "os": "linux",
  "base": "lsbx-kali-v2",
  "mode": "copy",
  "cpu": 4,
  "memory": "8GB",
  "streaming": "novnc",
  "capabilities": ["security", "tor", "desktop", "computer-use", "browser"],
  "healthcheck": [
    "chromium --version",
    "systemctl is-active --quiet lsbx-desktop.service",
    "curl -fsS --retry 10 --retry-delay 1 --retry-connrefused http://127.0.0.1:8000/vnc.html >/dev/null",
    "systemctl is-active --quiet tor",
    "curl -fsS --retry 6 --retry-delay 3 --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip",
    "/usr/local/bin/tor-mode status"
  ]
}
```

## Boundaries
- **Touches:** `images.carnyx.json` (`goldens[]`, `profiles{}`), and produces
  `lsbx-kali-v2.qcow2` / `lsbx-kali-open-v1.qcow2` under `/home/carnyx/ISOs/images/goldens/`.
- **Does NOT touch:** `crates/**`, `scripts/build-kali-golden.sh` (owned by 01–05),
  `images.json`, `lsbx-kali-v1.qcow2` (must remain until the owner retires it).

## Verification
```bash
lsbx golden build --dry-run --from kali-genericcloud-base --script scripts/build-kali-golden.sh \
  --flavor desktop --cpu 4 --memory 8GB --os linux --streaming novnc --mode locked kali-tor
lsbx golden list --json | python3 -m json.tool | grep -A8 '"kali-tor"'
lsbx golden verify kali
lsbx golden verify kali-tor
lsbx up kali-tor --lease PT30M ; lsbx exec <id> -- cat /var/lib/lsbx/tor-mode
lsbx up kali     --lease PT30M ; lsbx exec <id> -- systemctl is-active tor ; echo "expect: inactive"
```
