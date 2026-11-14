# Phase Spec — Console & resize follow-ups (four independent workstreams)

**Timestamp:** `2026-11-14T0349Z`
**Slug:** `console-and-resize-followups`
**Status:** Specs only. One unit per GitHub issue; each is independently
dispatchable and can land in any order (dependency graph in §4).
**Predecessors:**
- `docs/specs/2026-11-14T0337Z_console-endgame-recon/SPEC.md` — the recon these units come out of
- `docs/specs/2026-11-14T0256Z_dynamic-browser-resolution-resizing/SPEC.md` — the resize phase this revises (its Unit 02 premise is dead)
- `docs/specs/2026-11-14T0307Z_tailcat-transport/SPEC.md` — tailcat doors, deliberately *not* a unit here (see §3)

---

## 0. Why one folder, four units

The endgame recon found four separate defects/decisions that share a single
symptom ("the desktop console is not a product yet"). They have **disjoint file
surfaces** and **no circular dependencies**, so they are four units, not four
phases. Each maps 1:1 to a GitHub issue, and each can be implemented and reviewed
alone.

| Unit | Issue | One-line |
|---|---|---|
| `01-console-assets-own-origin` | #55 | The gateway cannot serve the noVNC page; vendor the assets and go same-origin |
| `02-windows-resize-display-path` | #56 | Decide the Windows display/VNC path — TightVNC 2.8.81 removed `ExtendedDesktopSize` |
| `03-gateway-client-mode` | #57 | `lsbx` has no client mode; make it speak to a remote `lsbx serve` |
| `04-linux-desktop-recipe-import` | #58 | The Linux desktop golden's recipe is not in this repo, so Linux resize has no home |

---

## 1. The three roles (answering "does Calimbra own the assets?")

There are three distinct roles, and confusing them is the main architectural risk
in this area:

| Role | Who it is today | What it does |
|---|---|---|
| **Edge** | Calimbra (Caddy, `lsbx.lufshq.com`, Cloudflare) | terminates TLS, reverse-proxies to one upstream. Holds **no** lsbx state |
| **Service** | carnyx (`lsbx serve` on `:8243`, `lsbx-ci-broker`, libvirt) | the state store, the libvirt connection, the console WS relay |
| **Client** | Klaxon / kora / an agent | wants to *use* the service |

**Assets belong to the Service, not the Edge.** Three reasons:

1. **Version coupling.** The noVNC client and the gateway's `/stream/<id>/…` WS
   route are one contract. Vendoring the bundle inside the `lsbx` binary keeps
   them at the same version by construction. If Caddy served them, a Caddy
   change could ship a client older/newer than the gateway with no commit
   linking the two.
2. **Tailnet-only deployments get nothing.** A Calimbra-owned bundle only exists
   where Calimbra is in the path. carnyx's tailnet door (`:8243`) and any tailcat
   door would still be asset-less.
3. **Caddy is a proxy, not a config surface.** Every operational fact already
   lives in the lsbx manifest/state; adding a second place that knows what a
   console needs is the kind of split knowledge the `canonical-url` spec's D6
   explicitly avoided (that is why `public_base_url` is config, not a Caddy
   template).

**And a hard warning about running a *second* `lsbx serve` on Calimbra:** it is
not a stateless proxy. `LsbxOps` owns a `SandboxStore` (a state directory) and a
direct libvirt connection. A second gateway on Calimbra would have its own,
divergent state directory and would be talking to... nothing, unless it were
pointed at carnyx over `qemu+ssh://` — which §1.6 of the recon shows cannot work
today, because disk operations (`qemu-img create -b`, `fs::remove_file`, flatten)
execute **client-locally**. Two gateways = two state stores = split brain, with
each one reaping the other's sandboxes as "missing". **One service instance; edge
is edge; clients are clients.**

---

## 2. What "resize to whatever window size" actually requires

The user-facing requirement is: *drag the browser window to any size and the guest
desktop becomes that size, sharply, at whatever dimension the window happens to
be.* Four independent preconditions, all of which must hold:

| # | Precondition | Status today |
|---|---|---|
| 1 | A page the browser can actually load, that knows how to speak RFB to the gateway | **missing** (placeholder page; assets live in the guest) → Unit 01 |
| 2 | The client must send viewport size; either as RFB `SetDesktopSize` (needs server support) or as an app-level POST | client half exists (`resize=remote`), server half **impossible** on TightVNC 2.8.81 → Unit 02 |
| 3 | Something must actually change the Windows/Linux display mode | not built → Unit 02 (+ Unit 04 for Linux) |
| 4 | The framebuffer must be big enough for the requested size | **violated**: QXL `vgamem=16 MiB` holds ≤2560×1440; 4K needs 33 MB → Unit 02 |

Item 4 is the one that silently caps "whatever window size": a 27" 4K desktop
window will ask for 3840×2160, and 16 MiB of QXL video memory cannot represent
that framebuffer at all.

---

## 3. Deliberately not a unit: tailcat doors (#52)

Tailcat (#52) stays open and unscheduled by this phase. It cannot carry a browser
(no WebRTC; `tailcat#4`), it cannot serve HTML, and its value is for **clients that
are off-tailnet** (cloud agents). Building it before Unit 01 exists would be
building a bridge to a page that does not exist. Revisit after Unit 01 + Unit 03.

---

## 4. Dependency graph

```
Unit 01 (console assets) ──► Unit 02 (resize: needs a page to drive) ──► #51 closes
                        └──► Unit 03 (client mode: benefits from a working console URL)
Unit 04 (Linux recipe) ──► Linux half of Unit 02
```

- **Unit 01 is the keystone.** Nothing user-facing can be verified without it.
- **Unit 02 depends on 01** only for *verification*; its decision work (which
  server/display mechanism) can start immediately.
- **Unit 03 is independent** and can run in parallel with 01.
- **Unit 04 is independent** and small.

---

## 5. Options on the genuinely open questions

The user asked for options + a recommendation wherever the answer is not settled.
Three questions qualify; each is answered inside its unit, but collected here so
the trade-offs are visible in one place.

### Q1 — Which VNC server / display mechanism for Windows? (Unit 02)

| Option | Mechanism | Client-driven? | Cost | Risk |
|---|---|---|---|---|
| **A. TightVNC + host-driven mode set** (recommended) | browser POSTs viewport → gateway → guest `set-resolution.ps1` → Windows `ChangeDisplaySettingsExW`; TVN reports the size change via the legacy `DesktopSize` (-223) rect | no (host drives) | small: one script + one endpoint | depends on TVN emitting -223 on an in-guest change; **verifiable in 20 min** |
| **B. TigerVNC WinVNC server** | server implements `ExtendedDesktopSize` → true `resize=remote` | **yes** | medium: new golden bake, new password/registry config | TigerVNC **dropped official Windows server support in 2020**; community says 1.14 "works reliably". Depending on an unsupported component for a product default is the risk |
| **C. virtio-gpu + viogpudo + QEMU EDID** | hypervisor advertises a preferred mode (`QMP display-update`/EDID); guest driver follows | n/a (host drives) | large: display device change, driver install, QMP plumbing, Windows quirks | virtio-win's own issue tracker asks for this behaviour — the driver does **not** reliably auto-follow host changes today |
| **D. SPICE + spice-vdagent** | vdagent follows the client window natively | yes | large: no maintained browser SPICE client | fails the browser requirement outright |

**Recommendation: A**, with the 20-minute empirical check as its gate, and **B**
documented as the fallback if the check fails. Rationale: A keeps the golden's
current, actively-maintained server, needs no protocol implementation on our side,
and the *only* unknown is a vendor behaviour we can measure before writing code.
B buys a cleaner protocol story at the price of an officially-abandoned Windows
server. C is the "correct" long-term answer if we ever want a GPU-capable display
path, but it is a different project.

### Q2 — Who serves the console assets? (Unit 01)
Options: (a) vendor into `lsbx` and serve from whichever `lsbx serve` terminates
HTTP; (b) Caddy on Calimbra serves a directory; (c) keep serving from the guest
(today).

**Recommendation: (a).** See §1. (b) splinters the client/server contract and
helps only one of the three deployment shapes; (c) is the defect.

### Q3 — How does a client talk to the service? (Unit 03)

| Option | Shape | Cost | Verdict |
|---|---|---|---|
| **A. Gateway client mode** (recommended) | `lsbx --gateway https://lsbx.lufshq.com --token …` → REST | medium (REST client, auth, output parity) | the HTTPS door already answers 200/101; it is the only option that also serves agents and browsers |
| **B. Remote libvirt** | wire `LibvirtTransport::RemoteSsh` + move every `qemu-img`/fs call behind a remote-exec trait | large | correct but invasive; revisit only if A proves insufficient |
| **C. Document `ssh carnyx lsbx …`** | nothing to build | zero | honest, but leaves the CLI unable to use the product's own service |

**Recommendation: A**, specced as Unit 03 and *not* as part of the resize phase.

---

## 6. Out of scope for the whole phase

- GPU passthrough / virtio-gpu venus video acceleration (separate, larger).
- Tailcat doors (#52) — see §3.
- Bridge wedge hardening (#53) — independent bug, own issue.
- The tailnet subnet route spec.
