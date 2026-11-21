# Unit 09 — docs-and-runbook

## Objective
The README, AGENTS.md and CHANGELOG describe the two doors, the slot lifecycle, the
env vars and the measured latency numbers, so an operator (or an agent) can use the
result without reading the spec folder.

## Context
- The whole phase, but especially SPEC §6 (usage model and process management) and
  §5 D4a/D5/D6/D9 — the docs must not contradict those decisions.
- `README.md` — "Working rules for desktop goldens", "Services", "Opening a desktop
  console in a browser", "CI on Carnyx". The new material slots in beside these.
- `AGENTS.md` — the CI placement and environment-variable sections; the slot env
  vars belong near `LSBX_*` conventions.
- `CHANGELOG.md` — "Keep a Changelog" format, `[0.1.0-unreleased]` section.
- Inputs that must already exist: `MEASUREMENTS-05.json`, `FINDINGS-05-latency.md`,
  `FINDINGS-06-vdd-resize.md`, `FINDINGS-08-browser-bridge.md` (or an explicit
  "parked" note if unit 08 did not run).

## Acceptance criteria
- [ ] README has a **"Streaming a desktop with audio (Moonlight)"** section
      containing: the happy-path commands from SPEC §6.1, the port-slot model
      (base 50000 + slot×100, why it avoids 47984–48010), the env vars, and a
      pointer to `lsbx moonlight --list`.
- [ ] README states plainly that the **VNC console carries no audio** and that
      Moonlight is the work door — the door table from SPEC §4 appears in the
      README.
- [ ] README records the **measured** glass-to-glass video latency and audio
      latency, with the measurement date and a link to `MEASUREMENTS-05.json`.
      No adjectives without a number next to them.
- [ ] README's "Working rules for desktop goldens" gains: Sunshine runs in the
      guest; the host's Sunshine instance is unrelated and must not be pointed at
      a sandbox; a re-bake is required for any Sunshine config change.
- [ ] AGENTS.md documents `LSBX_MOONLIGHT_SLOTS`, `LSBX_MOONLIGHT_PORT_BASE`,
      `LSBX_MOONLIGHT_HOST`, `LSBX_MOONLIGHT_UDP_IDLE` next to the other `LSBX_*`
      vars, with their defaults.
- [ ] CHANGELOG has an entry under `[0.1.0-unreleased]` describing: guest audio
      device, in-guest Sunshine + VDD, slot forwarding, `lsbx moonlight`, and the
      measured latency — or, if a piece did not land, only what did.
- [ ] No secrets, tokens, PINs, guest IPs of running sandboxes, or the gateway
      token appear in any doc.
- [ ] Every command block in the new README section has been run at least once in
      the form written.

## Interface contract
```
README.md  sections added/updated:
  ### Streaming a desktop with audio (Moonlight)     [new]
  ### Working rules for desktop goldens             [amended]
  ### Opening a desktop console in a browser        [amended: no audio, role]
  ## Door table                                     [new, from SPEC §4]

AGENTS.md  section amended:
  ## CI broker environment variables  →  add the LSBX_MOONLIGHT_* block
  (or a new sibling section if it reads better; keep the LSBX_* convention)

CHANGELOG.md  entry under [0.1.0-unreleased] → Added
```

## Boundaries
- **Touches:** `README.md`, `AGENTS.md`, `CHANGELOG.md`.
- **Does NOT touch:** anything under `docs/specs/` (those are dated records; unit
  05/06/08 already wrote their own findings files), any `crates/`, any `scripts/`,
  `images.carnyx.json`.

## Verification
```bash
# every fenced command block in the new README section, run in order on carnyx
grep -n "lsbx moonlight" README.md
# no leaked secrets or sandbox IPs in the diff
git diff -- README.md AGENTS.md CHANGELOG.md | grep -nE "145f332a|token|PIN|192\.168\.122\." || echo "clean"
```
