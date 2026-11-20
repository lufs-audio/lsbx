# Phase Spec — Two Kali Goldens: `kali` (open) and `kali-tor` (locked)

**Timestamp:** `2026-11-20T0110Z`
**Slug:** `kali-and-kali-tor-goldens`
**Target:** Carnyx libvirt host (`qemu:///system`, local KVM x86_64)
**Status:** PRESENTED FOR APPROVAL — no code, config, image, or registry change has been made

---

## 1. Problem

The `kali` golden (`lsbx-kali-v1.qcow2`, built 2026-09-17) is a single, headless,
Tor-locked image. That single-image design creates three problems:

1. **Not reproducible.** `lsbx golden build` requires `--from` and `--script`. No
   `scripts/build-kali-golden.sh` exists. `grep -rn "tor-route\|proxychains4"` across the repo
   hits exactly two files — `SPEC.md` and `USAGE.md` — both prose. The transparent Tor
   routing, `proxychains4`, UFW rules, SSH hardening, and `99-lsbx` sudoers exist **only inside
   the v1 qcow2**. Any rebuild from the retained base silently loses Tor.
2. **Not observable.** There is no browser and no GUI (`flavor: agent`, `streaming: none`), so
   nothing in the image can be driven visually by an agent.
3. **Not flexible.** Tor egress cannot be relaxed without editing the golden and rebuilding,
   so ordinary work that Tor exits are blocked from (GitHub/Cloudflare 403s, throttled
   mirrors) has no path.

Additionally, `~/.agents/skills/lsbx/SKILL.md` documents **nothing** about Kali — no `kali`
profile, no Tor model, no headless caveat. A fresh agent reading only the skill cannot know
this golden exists. That gap was discovered by reading the repo instead, which is the failure
mode this phase closes.

## 2. Goal

Two goldens, both desktop-capable and agent-drivable, differing **only** in whether egress is
Tor-locked:

| Golden | Profile | Tor | Torrent client | Role |
|---|---|---|---|---|
| `kali-tor` | `kali-tor` | **Locked (fail-closed)** | No | **Takes over the Tor role currently held by `kali`** |
| `kali` | `kali` | **Absent** | `aria2c` | Open-egress lab desktop |

Both: XFCE + noVNC, Chromium (agent-drivable over CDP), Playwright's bundled Chromium,
optional Firefox ESR, rebuilt from a provisioning script.

## 3. Decisions carried in from intake

- Two goldens, not a runtime toggle alone. (Owner decision.)
- `kali-tor` replaces the current Tor-enabled `kali`. (Owner decision.)
- Both get the desktop + browser additions; only `kali` gets the torrent client. (Owner decision.)
- Torrenting behaviour is **not** wired into the build. `aria2c` is installed as a tool and
  documented as requiring open mode. (Owner decision — deliberately out of scope.)
- No `lsbx` config change is authorized by this spec. Implementation is a separate authorization.

## 4. ⚠️ Migration hazard — read before approving

This phase **inverts the meaning of an existing profile name.** Today `lsbx up kali` yields a
Tor-locked VM. After this phase it yields a VM with **no Tor at all**, and `lsbx up kali-tor`
becomes the locked one.

Every habit, script, or agent that runs `lsbx up kali` believing it is torified will silently
leak its real IP. That is the same fail-open failure this phase exists to prevent, reintroduced
by a rename.

Mitigations that are **required**, not optional, in this spec:

1. `kali` ships `/etc/motd.d/00-kali-no-tor` printing a plain warning on every SSH login:
   *"This golden has NO Tor. Use `kali-tor` for torified egress."*
2. `kali`'s healthcheck asserts Tor is **absent** — `! systemctl is-active --quiet tor` — so a
   build that accidentally inherits Tor fails verification rather than passing quietly.
3. `~/.agents/skills/lsbx/SKILL.md` states the inversion in its first Kali paragraph (Unit 07).
4. `kali-tor` keeps the existing healthcheck verbatim, so its identity is provable.

**Alternative if any of this feels too sharp:** name the open golden `kali-lab` and leave `kali`
as the locked default, so no existing invocation changes meaning. Flagged for the approver; the
spec as written follows the stated decision.

## 5. Environment facts verified (2026-11-20)

| Fact | Value |
|---|---|
| Host | i7-13700K, 24 threads, **188 GiB RAM**, 173 GiB available, 15 GiB used, 188 GiB swap |
| Running VMs | none (`win11`, `provision-win11` both shut off) |
| `lsbx` | 0.1.0 at `~/.local/bin/lsbx`; profiles: `default, web, desktop, win11, kali, ci` |
| Current golden | `lsbx-kali-v1.qcow2` 3.6 GB, 2026-09-17; 2 vCPU / 4 GB |
| Retained base | `kali-linux-2026.2-cloud-genericcloud-amd64.tar.xz` (224 MB), `kali-genericcloud-base.qcow2` (359 MB) |
| Upstream latest | **Kali 2026.3** (released 2026-10-26) |
| Comparison golden | `win11-desktop` runs 8 vCPU / 32 GB — 8 GB is modest by local convention |
| Disk | 895 GB free on `/home` |

## 6. Resource bump: justification

**RAM 4 GB → 8 GB, per golden. The host cost is negligible — confirmed.**

Component budget for a desktop golden with the agent stack:

| Component | Steady | Notes |
|---|---|---|
| Kali base + systemd | ~0.4 GB | |
| XFCE + `x11vnc` + `websockify` | ~0.5 GB | |
| Chromium (visible, a few tabs) | ~0.9 GB | peaks ~1.5 GB; per-process model |
| Playwright's bundled Chromium (concurrent) | ~0.6 GB | separate binary, separate process tree |
| `tor` daemon (`kali-tor` only) | ~0.15 GB | |
| `aria2c` (`kali` only) | ~0.05 GB | disk-cache bound, not RAM bound |
| **Total** | **~2.4–3.4 GB** | **4–5 GB peak** |

At 4 GB this is marginal — and `agent-web` proves exactly why: it runs XFCE + one browser at
4 GB, but **one** browser. This phase's design runs **two** Chromium generations concurrently
(the visible one and Playwright's). That is the specific gap that justifies 8 GB.

Host impact: 8 GB is **4.2%** of 188 GiB; both goldens running simultaneously is **8.5%**. CPU
4 vCPU is 17% of 24 threads. **Verdict: carnyx handles this trivially — no capacity concern.**

*Honest caveat:* 6 GB would likely suffice if the two browsers never run at once. 8 GB is
recommended for headroom, not because 6 GB would fail.

## 7. Base bump: 2026.2 → 2026.3 — is it needed?

**No, not strictly.** The mandatory work is the provisioning script (Unit 01); the base version
is an independent variable. Arguments each way:

- *For:* the golden is one release plus ~2 months of package updates behind; 2026.3 shipped
  2026-10-26; the rebuild happens anyway, so the bump is nearly free.
- *Against:* a base bump changes kernel, cloud-init, and package set simultaneously with a
  large feature addition — two sources of failure in one verification run.

**Recommendation:** bump, but as the **last** thing (Unit 06), after the script reproduces v1's
behaviour on the 2026.2 base. That keeps the diff attributable. If verification gets murky,
ship v2 pinned to 2026.2 and bump in a follow-up.

## 8. Tor mode design (chosen)

Two goldens provide the guarantee; a fail-closed helper provides convenience **on top**, never
a bare off switch.

- `locked` (baked into `kali-tor`): current `REDIRECT` model, hardened — IPv6 closed, non-DNS
  UDP dropped, DNS via `DNSPort`. Inherently fail-closed: Tor down ⇒ redirected traffic hits a
  dead port and fails rather than escaping.
- `open` (baked into `kali`): **no** transparent redirect, **and no Tor daemon at all.**
- The `tor-mode` helper is provisioned in `kali-tor` only, as `locked|open` where `open` means
  *redirect removed, `OUTPUT` policy `DROP`, explicit allowlist installed* — never
  "unrestricted." Mode is logged to `/var/lib/lsbx/tor-mode`.

Rationale: make the unsafe state require a deliberate, logged, allowlisted act, rather than
making the safe state a default that can be switched away.

## 9. Boundaries — what this phase does NOT do

- No GPU passthrough, no VFIO, no `<hostdev>`. (Separately tracked in
  `docs/specs/2026-11-14T0337Z_console-endgame-recon/SPEC.md`.)
- No torrenting behaviour, no torrent configuration, no tracker/DHT settings. `aria2c` only.
- No CDP exposure beyond loopback. No port forwarded from the host.
- No change to `images.json` (the exedev/portable manifest) beyond what `golden register` writes
  to `images.carnyx.json`.
- No WSL/Windows work, no `agent-web` or `win11-desktop` changes.
- No modification of the retained 2026.2 base archive.

## 10. Done criteria

- [ ] `scripts/build-kali-golden.sh` exists, is `shellcheck`-clean, and reproduces v1's Tor +
      hardening delta on a fresh base without manual steps.
- [ ] `lsbx golden list` shows both `kali` and `kali-tor` with the memory/CPU in §6.
- [ ] `lsbx golden verify kali` and `lsbx golden verify kali-tor` both pass all healthchecks.
- [ ] `kali-tor` proves Tor egress via `--socks5-hostname 127.0.0.1:9050`; `kali` proves Tor
      **absent**.
- [ ] Both serve `vnc.html` on `:8000`, reachable through `lsbx console`.
- [ ] `~/.agents/skills/lsbx/SKILL.md` documents both profiles, the inversion, and the
      headless caveat.
- [ ] Nothing in `crates/**` is modified.

## 11. Ecosystem references

- **Repo of record:** `~/repos/lsbx` — branch `main`. Golden build: `crates/lsbx-golden/src/build.rs`;
  registry: `images.carnyx.json`, `images.json`; domain XML: `crates/lsbx-backend-libvirt/src/domain_xml.rs`.
- **Prior phase:** `docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md` (the v1 build — the
  authoritative description of the Tor delta being recovered) and `ROADMAP.md` §1.1 (desktop
  streaming), §2.1 (automated rebuild).
- **Upstream Kali:** `kali.download/cloud-images/current/` — genericcloud amd64 images.
- **Tor:** Tor Project documentation on `TransPort`/`DNSPort`/`SocksPort`; the project
  discourages routing arbitrary browsers and BitTorrent through Tor.
- **Playwright:** `playwright.dev/docs/browsers` — Chromium/Firefox/WebKit support; Playwright's
  Firefox is a patched build, **not** branded Firefox; browsers install to `~/.cache/ms-playwright`.
- **Skill file:** `~/.agents/skills/lsbx/SKILL.md` — outside this repo.

## 12. Implementation status (2026-11-20, branch `feature/kali-and-kali-tor-goldens`)

### Landed and verified

- **Unit 01** — `scripts/build-kali-golden.sh` exists and passes: `bash -n`, `--check`, `--emit
  locked`, `--emit open`, and `--dry-run` in both modes. Emitted files re-verify cleanly and report
  `mode already baked as '<mode>'`.

### Deferred in Unit 06 — deliberately, with reason

`images.carnyx.json` was **not** edited. Registering `kali` → `lsbx-kali-open-v1` before those qcow2
files exist would break the currently-working `lsbx up kali` in the live working tree. The registry
change and the image build must land **atomically**; splitting them regresses a working command.
Unit 07 (skill) is deferred for the same reason — it would document profiles that do not exist yet.

### Precondition discovered — Unit 06 cannot run as written

`golden build --from <name>` resolves base disks from `<images_dir>/goldens/<name>.qcow2`
(`crates/lsbx-backend-libvirt/src/golden_disk.rs:197`). The stock base lives in
`<images_dir>/build/kali-genericcloud-base.qcow2`, so **`--from kali-genericcloud-base` will not
resolve in a real build**. `lsbx golden reconcile` confirms only the five manifest goldens are
`present`.

Fix (one of, to decide):
1. Register the stock base as a golden (key `kali-genericcloud-base` → `lsbx-kali-v0.qcow2`), or
2. symlink `goldens/kali-genericcloud-base.qcow2` → `build/kali-genericcloud-base.qcow2`, or
3. use `--from kali` (v1) as the base — **not recommended**: the open variant would inherit the Tor
   install, and only Unit 06's negative healthcheck (`! systemctl is-active tor`) would catch it.

### Correction to a claim made during implementation

`lsbx golden build --dry-run` **does not validate anything**: `build.rs:169` returns a canned config
(`content_hash: lufs-dryrun`) before touching the backend
(`dry_run_returns_canned_config_without_touching_backend`). A passing dry-run is therefore **not**
evidence that the base resolves or that the script would succeed.

### Tooling gap

`shellcheck` is not installed on this host, so the `shellcheck` acceptance criterion in Unit 01 is
**unverified**; `bash -n` plus manual review were used instead.

## 13. Units

| # | Unit | Depends on |
|---|---|---|
| 01 | Recover v1 provisioning into `scripts/build-kali-golden.sh` | — |
| 02 | Tor mode architecture: fail-closed `locked`, IPv6/QUIC closure, mode logging | 01 |
| 03 | Desktop stack: XFCE + `x11vnc` + `websockify` + UFW 8000 | 01 |
| 04 | Browser layer: Chromium + Playwright bundled Chromium + optional Firefox ESR | 03 |
| 05 | Torrent client: `aria2c`, gated to open mode, no wiring | 01 |
| 06 | Two-golden build, registry, memory bump, base bump, healthchecks | 02, 03, 04, 05 |
| 07 | `lsbx` skill: Kali visibility, profile inversion, headless caveat | 06 |
