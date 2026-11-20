# Unit 07 — `lsbx` skill: Kali visibility, profile inversion, headless caveat

## Objective
Make `~/.agents/skills/lsbx/SKILL.md` describe the Kali goldens accurately, so an agent that reads
only the skill knows they exist, knows which one is torified, and cannot repeat the mistakes this
phase was chartered to fix.

## Context
- `~/.agents/skills/lsbx/SKILL.md` currently documents `up/down/exec/put/get/renew/console/...`,
  the config precedence chain, and profile mechanics — but contains **zero** mentions of
  `kali`, `tor`, `proxychains`, or `flavor: agent`/`streaming: none`. Verified by:
  `grep -niwE "kali|tor|onion" ~/.agents/skills/lsbx/SKILL.md` → no matches.
- The authoritative detail lives in `docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md` and, after
  this phase, in `docs/specs/2026-11-20T0110Z_kali-and-kali-tor-goldens/SPEC.md` §4.
- The skill is a *pointer document*: it should stay short and link to the specs rather than
  restating them. Do not copy the Tor rule set into the skill.
- This file lives outside the `lsbx` repo, at `~/.agents/skills/lsbx/SKILL.md`. It is **not**
  under version control with the repo.

## Acceptance criteria
- [ ] `grep -niwE "kali" ~/.agents/skills/lsbx/SKILL.md` returns matches.
- [ ] A section lists both profiles in one table: `kali` (no Tor, `aria2c` present) and `kali-tor`
      (Tor-locked, no `aria2c`), with `flavor: desktop` and `streaming: novnc`.
- [ ] The inversion is stated in the first paragraph of that section, in bold, before any other
      Kali detail: running `lsbx up kali` gives **no** Tor.
- [ ] The previous headless caveat is corrected rather than deleted: the skill notes that `kali`
      was *previously* `flavor: agent` / `streaming: none`, so older references are stale.
- [ ] The Tor capability is described as "guest-internal transparent routing + `proxychains4`",
      linking to the 2026-09-17 spec rather than restating rules.
- [ ] The skill states the desktop is reachable via `lsbx console <id>` on guest port 8000.
- [ ] `description:` frontmatter still matches what the skill covers; if Kali materially widens
      its scope, the description is updated too.
- [ ] No rule text, iptables snippet, or package list is duplicated from the specs.

## Interface contract
```markdown
## Kali goldens

**`lsbx up kali` is NOT torified.** It is the open-egress desktop (no Tor daemon, `aria2c`
installed). Use **`lsbx up kali-tor`** for Tor-locked egress.

| Profile | Tor | Extra tools | Spec |
|---|---|---|---|
| `kali` | none | `aria2c` | docs/specs/2026-11-20T0110Z_kali-and-kali-tor-goldens/SPEC.md |
| `kali-tor` | locked (fail-closed) | — | docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md |

Both are `flavor: desktop`, `streaming: novnc`, reachable via `lsbx console <id>` on guest :8000.
Both include Chromium with CDP on loopback `127.0.0.1:9222` only.
```

## Boundaries
- **Touches:** `~/.agents/skills/lsbx/SKILL.md` **only**.
- **Does NOT touch:** anything in `~/repos/lsbx`, any golden, any profile, `images.carnyx.json`,
  any other skill.

## Verification
```bash
grep -niwE "kali" ~/.agents/skills/lsbx/SKILL.md
grep -niE "kali-tor" ~/.agents/skills/lsbx/SKILL.md
grep -niE "lsbx up kali" ~/.agents/skills/lsbx/SKILL.md   # must show the NOT-torified warning
```
