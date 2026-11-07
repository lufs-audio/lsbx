# Phase Spec — guest-network reachability: a Tailscale subnet route for carnyx

**Timestamp:** `2026-11-07T1440Z`
**Slug:** `libvirt-guest-net-tailscale-route`
**Status:** Proposed. Recon + decisions; **implementation is a separate, dedicated turn.**
**Related:** `docs/specs/2026-11-07T1440Z_canonical-url-lsbx-lufshq/SPEC.md`, `~/repos/tailscale-acls`.

---

## 0. The gap

exe.dev gave every sandbox a routable address: `ssh <vm>.exe.xyz` worked from
anywhere on the account. On carnyx the guest network is
`virbr0 192.168.122.0/24` (measured 2026-11-07), which is local to that host.
`tailscale status --json` on carnyx reports `PrimaryRoutes: None` — the subnet
is not advertised — so from kora or calimbra a guest is unreachable. Today the
only paths in are:

- `lsbx exec <id> -- <cmd>` (runs on carnyx, then SSHes to the guest), and
- the gateway's `/sandboxes/<id>/{exec,put,get}` over HTTPS.

Both are fine for scripts. Neither is `ssh`, neither is `scp` with a real key,
and neither lets a browser or an editor talk to a dev server inside a sandbox.

## 1. Decisions

| # | Question | Decision | Why |
|---|---|---|---|
| D1 | Mechanism | **Tailscale subnet router on carnyx advertising `192.168.122.0/24`**, route approved in the tailnet admin console. | One host, one route, no per-sandbox objects. Alternatives (per-sandbox `tailscale up` inside the guest, or an lsbx-managed proxy) cost a tailnet node per sandbox or new code. |
| D2 | Who can reach guests | **Nobody new, by construction — and pin that with an explicit grant.** The tailnet ACL already isolates members to `autogroup:self`, so family devices cannot reach carnyx (owned by `daniel@lufs.audio`) and therefore cannot reach any subnet it advertises. Add one explicit grant + one ACL denial test so a future relaxation cannot silently expose the guest subnet. | Verified in `danialrami/tailscale-acls` → `policy.hujson`: `{"src":["autogroup:member"],"dst":["autogroup:self"],"ip":["*"]}` isolates members; `group:superadmin` (`daniel@lufs.audio`) has `* → *`. So the route is effectively admin-only today. Relying on that *implicitly* is the risk — an isolation grant is a policy knob someone will widen later, and it would silently publish the guest net. |
| D3 | Key material | **Do not copy private keys off carnyx.** Reach guests with (a) an operator's own key added to the golden's `authorized_keys` at build time, or (b) `ssh -J carnyx deploy@<guest>` jumping through the host that holds the key. | lsbx's per-sandbox keys are ephemeral and are cleaned up on `down`; syncing them to kora would leak them into a second filesystem and break reap's key reconciliation. |
| D4 | Address stability | **Rely on the `lsbx list --json` record, not on DHCP luck.** Guests are created per sandbox; an address observed once must not be cached in shell history or docs. | `192.168.122.x` is handed out by libvirt's dnsmasq and changes as sandboxes come and go. A DHCP reservation per sandbox is possible later if a use case needs a stable name. |
| D5 | DNS | **No MagicDNS/custom DNS for the guest subnet in v1.** Use IPs from `lsbx list`. | dnsmasq inside libvirt already answers on `virbr0`, but pointing the tailnet's resolvers at a disposable-guest subnet is a footgun for little gain. |
| D6 | Host prerequisite | `net.ipv4.ip_forward=1` (and `net.ipv6.conf.all.forwarding=1` if IPv6 ever matters) persisted via sysctl, plus `tailscale up --advertise-routes=...` in carnyx's tailscale unit. | Subnet routing is forwarding; without it the route exists and silently drops. `--advertise-routes` alone survives restarts only if it is in the unit's flags — a bare `tailscale up` on the CLI does not. |

## 2. Cost/benefit, asked and answered 2026-11-07

Recorded before implementation, so the decision is made against the real trade
rather than "native networking sounds better".

### What we have today (no route)

| need | how it works now | cost |
|---|---|---|
| run a command in a sandbox | `lsbx exec <id> -- …` on carnyx; or `POST /sandboxes/<id>/exec` via the gateway | zero setup |
| copy files in/out | `lsbx put` / `lsbx get`; or gateway `/put` `/get` | zero setup |
| shell into a sandbox by hand | `ssh -J carnyx lsbx@<guest-ip>` — ProxyJump needs nothing but SSH to carnyx | zero setup |
| reach a dev server in the guest | `ssh -L 8000:localhost:8000 -J carnyx lsbx@<guest-ip>`, browse localhost:8000 | one flag |
| browser desktop (win11 / kali) | gateway `/console` → `/stream/<id>/websockify` (once the console work in the URL spec lands) | already in progress |

Every capability already exists. ProxyJump covers ssh/scp/rsync and port
forwarding; the gateway covers everything scripted.

### What the route adds

- **Native addressing**: `ssh lsbx@192.168.122.213`, `curl 192.168.122.213:8000`,
  an IDE's remote-SSH with no jump-host stanza, a browser pointed straight at a
  guest's noVNC. No jump flags, no forward flags, no gateway call.
- **Independence from the gateway**: a `lsbx serve` deploy no longer interrupts
  reachability.
- It is the one exe.dev property (`ssh <vm>` from anywhere) the current stack
  does not reproduce natively.

### What it costs

- **Four steps, one of them out-of-band**: `ip_forward` + `--advertise-routes`
  on carnyx (2), route approval in the tailnet console (1, manual), ACL grant +
  test in `danialrami/tailscale-acls` (1, reviewed PR).
- **It exposes more than sandboxes.** `192.168.122.0/24` is libvirt's *default*
  network: every domain on it becomes reachable, and today that includes
  operator-owned `win11` and `provision-win11`, plus anything defined later.
  Access control shifts from "the gateway's token" to "each guest's own sshd
  and whatever it happens to listen on" — a VM with a dev server bound to
  `0.0.0.0` becomes tailnet-reachable.
- **One more invariant to keep true**: a reinstall without the persisted flag,
  a re-approval after a node re-registers, or a bare `tailscale up` from the
  CLI all change behaviour silently. `tailscale status` on carnyx is the only
  place that shows the route.
- **DHCP churn**: guest addresses change as sandboxes come and go, so any
  workflow that hardcodes one breaks quietly. `lsbx list --json` stays the
  source of truth — the route makes bad habits *possible* in a way the gateway
  does not.

### Verdict: not yet, and cheap to add later

The deciding question is not "can we" but "what does it unlock that ProxyJump
doesn't", and today the answer is convenience, not capability. Two conditions
would flip it:

1. A workflow that genuinely needs native L3 and cannot use a jump host —
   e.g. tools driving Windows desktop guests that refuse ProxyJump, or an
   editor that will not accept a jump stanza.
2. Sandboxes on **their own bridge** (a dedicated prefix, e.g.
   `192.168.123.0/24`), so the advertised route covers sandboxes only and does
   not also publish operator VMs. That is the same split-network question as
   MTU option C in §7, and doing both together is cheaper than doing either
   alone.

Until then: `ssh -J carnyx` is the documented human path (D3), the gateway is
the automation path, and the stack grows no new trust boundary.

### The stack, before and after (the component inventory)

Asked alongside the decision, so it is recorded rather than re-derived:

| layer | component | before | after | changes? |
|---|---|---|---|---|
| hypervisor | **libvirt/KVM on carnyx** (`virbr0 192.168.122.0/24`) | guest net local to the host | same | no |
| engine | **lsbx** (`serve` gateway + `/stream` on carnyx `:8243`, `ci-broker`, CLI) | the only door to a guest | still the automation door | no |
| mesh | **Tailscale** | L3 to carnyx/kora/calimbra only | + `192.168.122.0/24` routed via carnyx | **yes** |
| edge | **Caddy on calimbra** (vhosts, TLS-internal, Cloudflare in front) | publishes `lsbx.lufshq.com` | unchanged | no |
| host OS | **Ansible (danialrami/molimo)** | converges base/tailscale/caddy/incus | + a persisted `--advertise-routes` flag and a sysctl | **yes** |
| access control | **tailnet ACLs** (`danialrami/tailscale-acls`) | member isolation confines family to own devices | + an explicit superadmin→subnet grant and a denial test | **yes** |
| runner | **GitHub Actions + the zero-idle broker** | jobs run in guests created on carnyx | unchanged (broker stays off the public name) | no |
| storage | golden qcow2s + per-VM disks under `~/ISOs/images/{goldens,work}` | host-local | unchanged | no |
| console | **noVNC inside goldens** (`:8000`) + lsbx-stream WS relay | gateway-mediated | + reachable directly at `<guest>:8000` | reachable, not changed |

So the route changes three things (tailscale, the Ansible flag, the ACL) and
touches nothing in lsbx itself — which is also why it is easy to defer: no code
depends on the answer.

## 3. What it takes, concretely

1. **carnyx**: `sudo tailscale set --advertise-routes=192.168.122.0/24`
   (needs `pkexec`-equivalent on that host — Daniel runs it), and the same flag
   persisted in `/etc/default/tailscaled` or the unit's `FLAGS`.
2. **carnyx**: `sysctl -w net.ipv4.ip_forward=1` + a file in `/etc/sysctl.d/`.
3. **Tailnet admin**: approve the route on the carnyx machine page (routes are
   not live until approved), then pin access in `danialrami/tailscale-acls`
   → `policy.hujson`. Nothing *needs* to change — member isolation already
   covers it — but an explicit grant makes the intent enforceable and reviewable:

   ```hujson
   // --- carnyx libvirt guest network (lsbx sandboxes) ---
   // Reachable only by the admin group; member isolation would already
   // exclude family devices, but this makes that a stated rule rather
   // than a side effect of another grant.
   {
       "src": ["group:superadmin"],
       "dst": ["carnyx"],
       "ip":  ["192.168.122.0/24:22"],
   },
   ```

   plus an ACL test so widening the isolation grant fails loudly:

   ```hujson
   {
       "src": "ramirj123@gmail.com",
       "deny": ["192.168.122.0/24:22"],
   },
   ```
   Port 22 only: a guest's dev server on :8000 is reachable by SSH tunnel
   (`ssh -L 8000:localhost:8000`), which keeps the grant one port wide.
4. **Verify from kora**: `tailscale ping <guest ip>`, `nc -vz <guest ip> 22`.
5. **Document** the jump path (D3) in the lsbx operating notes so nobody copies
   a key off carnyx.

## 4. Verification

- `tailscale status --json | jq '.Self.PrimaryRoutes'` on carnyx lists the
  prefix after approval (not before — this is the step people think worked).
- From kora: `ssh -o ConnectTimeout=5 deploy@<guest-ip>` reaches the guest's
  sshd banner (auth may fail — reachability, not trust, is the assertion).
- From a device **not** in the grant: the same address times out.
- `lsbx up` a sandbox, `tailscale ping` it, `lsbx down` it, confirm the route
  does not blackhole anything else (`tailscale ping carnyx` still direct).
- Disk/CPU cost: none measurable; note the MTU interaction — `virbr0` is
  `mtu 1280` on carnyx (observed), so large transfers through the route should
  be tested for PMTU stalls.

## 5. Rollback

`tailscale set --advertise-routes=` (empty) + un-approve in the admin console.
`ip_forward` can stay at 1 harmlessly, or be reverted with the sysctl file. No
guest, golden, or lsbx state is touched by any of this.

## 6. Open questions

- **O1** Do we want guests reachable at all, or is the honest answer "the
  gateway is the interface, and `ssh` is an operator convenience"? *Leaning:*
  operator convenience is worth it — but the ACL (D2) is what makes it
  defensible, so the question is really "who is in the grant".
- **O2** Should the route be limited to a sub-range per sandbox pool later
  (e.g. `192.168.122.128/25` for CI runners, separate from agent sandboxes)?
  *Leaning:* not yet; one prefix, revisit if a CI runner ever needs to be
  reachable from outside.
- **O3 (INVESTIGATED — see §7)** `virbr0`'s `1280` MTU is deliberate in lsbx's
  own code, not host drift. Crossover options weighed in §7; no decision yet.

## 7. The 1280 MTU: what it is, and the crossover options

### What it actually is (measured 2026-11-07)

| where | value | source |
|---|---|---|
| `virbr0` bridge | `mtu 1280` | libvirt network XML `<mtu size='1280'/>` in `virsh net-dumpxml default` |
| guest NIC | `1280` | **lsbx's own domain XML** `<mtu size='1280'/>` (`crates/lsbx-backend-libvirt/src/domain_xml.rs:159`) *and* cloud-init runs `ip link set dev ens3/eth0 mtu 1280` (`crates/lsbx-backend-libvirt/src/lib.rs:327-328`) |
| `tailscale0` | `1280` | WireGuard's default (and Tailscale's practical ceiling without PMTU help) |
| host NICs (`enp2s0`, `enp5s0`, `wlan0`), docker bridges | `1500` | host default |

So it is **deliberate, in two independent places, and product-owned** — not a
host quirk inherited from `wlan0`, and not something `virsh` decided on its own.
Your memory is right that it is about the networking stack on carnyx; the
specific mechanism is Tailscale/WireGuard's 1280 ceiling. What is missing is a
**written rationale**: a grep of `docs/`, `SPEC.md`, `README.md`, `CHANGELOG.md`
and the migration docs finds no explanation, only the code. That is itself a
finding — the value is load-bearing and undocumented, which is exactly the kind
of thing that gets "tidied" to 1500 by a future change and silently breaks
tailnet paths.

Consequence today: 1280 − 20 (IP) − 20 (TCP) = **1240-byte MSS**, i.e. ~14% less
payload per packet than a 1500 path. Irrelevant for `lsbx exec`, git, apt, CI
logs; visible on multi-GB transfers (`lsbx put/get` of an ISO, qcow2 copies).

### The four options

**A. Keep 1280 everywhere (status quo).**
  Pros: correct for *any* path that touches the tailnet, no PMTUD dependency, no
  ICMP assumptions, uniform with the WireGuard default, zero change and zero new
  failure modes. If the subnet route in this spec lands, this is the only option
  that cannot blackhole.
  Cons: ~14% per-connection throughput left on the table for host↔guest traffic
  that never leaves carnyx; the value has no documented reason.

**B. Move guests and `virbr0` to 1500, rely on PMTUD.**
  Pros: maximum throughput on the local bridge (golden copies, `put/get`,
  in-guest downloads are unaffected either way since they NAT out `enp2s0` at
  1500); one less odd number to explain.
  Cons: the moment traffic crosses `tailscale0` (1280) — which is exactly what
  this spec introduces — you depend on ICMP frag-needed arriving end to end. Any
  filtered ICMP (Cloudflare, a guest firewall, docker) gives the classic
  "connection hangs after the banner" blackhole, which is miserable to debug and
  intermittent by nature. B and the subnet route fight each other.

**C. Split networks: 1500 for host-local job VMs, 1280 for tailnet-facing VMs.**
  Pros: the right MTU for each path; CI runners (which never leave the host) get
  full throughput.
  Cons: two libvirt networks, two DHCP ranges, and every golden/profile has to
  declare which one it wants — a manifest + profile schema change and the exact
  per-host-manifest complexity the calimbra spec's D7 warns about. Real cost for
  a throughput gain nobody has asked for yet.

**D. Keep guests at 1500, clamp MSS at the router for tailnet paths.**
  Pros: keeps local throughput high and avoids tailnet blackholes without
  touching guests.
  Cons: `tcp option maxseg size 1240` in nftables only fixes TCP — UDP/QUIC
  still need PMTUD; adds a second, easily-forgotten place where the MTU story
  lives; and it must survive reboots via a persisted ruleset.

### Weighing

The decision hinges on whether guests are ever reached over L3 through the
tailnet, which is precisely what this spec is for.

- **With the subnet route: A is the honest choice.** C buys throughput only for
  traffic that never leaves the host, at the cost of a schema change; D is a
  second knob to maintain for a subset of protocols.
- **Without the subnet route** (gateway/exec only) the 1280 costs ~14% on
  transfers that are already small, and A remains cheaper than B's PMTUD risk.
- **B only becomes right** if the subnet route is abandoned *and* someone
  measures a throughput problem worth fixing.

**Recommendation: keep A (1280), and fix the actual defect — write down why, in
`SPEC.md`'s deviation list, next to the `<mtu size='1280'/>` in the domain XML,
so the next person does not "correct" it.** Revisit with measurements, not
intuition: two guests on `virbr0` running `iperf3` at 1280 vs 1500, plus
`iperf3` host↔guest and guest↔tailnet-peer, before touching anything.

## 8. Out of scope

- Per-sandbox tailnet nodes (a node per VM — the thing this design avoids).
- IPv6 for guests.
- Replacing `lsbx exec` — it stays the scripted path regardless.
