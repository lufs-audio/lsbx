# Phase Spec — calimbra sandbox hosting: the Incus decision

**Timestamp:** `2026-11-01T2354Z`
**Slug:** `calimbra-incus-backend`
**Issue of record:** `lufs-audio/lsbx#45`
**Status:** Decision spec. This phase decides; it does not implement.
**Target host:** calimbra (`113.20.2.11` / `molimo.lufs.audio`, CloudCone LA)
**Backend trait under discussion:** `crates/lsbx-kernel/src/backend.rs`
**Measurements cited from:** `danialrami/molimo` → `docs/incus-findings.md` (2026-11-01), summarized in issue #45's comment

---

## 0. Decisions, in one table — every one reversible

| # | Question | Decision | Why | Reversible? |
|---|---|---|---|---|
| D1 | Should calimbra take over **lsbx sandbox provisioning**? | **No.** calimbra is a *services + broker* host. Sandbox/CI-runner provisioning stays on carnyx (libvirt, real goldens) or moves to a box sized for it. | calimbra has **3.8 GiB total RAM**; the smallest existing golden (`agent-base`, 2 vCPU/4 GB) already exceeds the whole box, and `ci-runner` (4 vCPU/8 GB) is **2.1×** it. See §3.1. | Yes — trivially, if calimbra is resized. The numbers, not the principle, drive this. |
| D2 | Incus **system containers** for lsbx sandboxes (issue option ①)? | **Rejected.** | Two independent reasons: RAM (§3.1) *and* the goldens' healthchecks are VM-shaped (`systemctl is-active tor`, `docker image inspect`, `cmd /c`) — a container has no systemd, so ① doesn't merely distort the contract, it invalidates the existing goldens. See §3.2. | Yes, but it would require a new container-shaped golden family, not a backend flag. |
| D3 | `incus virtual-machine` as the **general** sandbox backend (option ②)? | **Rejected as a general answer.** | Measured boot-to-IP is **~697s** (~11.6 min) with a real VM image. Unusable for anything job-shaped. See §3.3. | Yes — this is a *performance* judgement; a faster storage/CPU host changes it. |
| D4 | Incus **split** — containers for long-lived services, VMs for sandboxes (option ③)? | **Adopted, first half only.** Incus containers for long-lived services (caddy, forgejo, runner slots, litellm). The second half (Incus VMs for sandboxes) is **not** taken on calimbra, per D1. | Matches how the workloads actually differ: services are long-lived and stateful; sandboxes are short-lived and disposable. It also puts the **broker** on calimbra (a services role) without pretending the box can also host sandboxes. | Yes. |
| D5 | Keep the SSH-shaped `create_from_golden` contract, or add an `exec` primitive? (issue's A/B/C) | **Option C first**: no `lsbx-kernel` change; a golden is responsible for being reachable, expressed through the existing `GoldenConfig.healthcheck`. **Option B** (`exec` as the primary primitive) is the eventual cleaner answer and is explicitly **deferred**. | C requires zero change to the two backends that already work (libvirt/carnyx, exedev/molimo), so it cannot destabilize production. B is an additive refactor later, not a rewrite. See §4. | Yes — B is a deliberate "not yet", not a rejection. |
| D6 | Does `BackendCapabilities` need an `exec_native` flag? | **Not now.** | Only Option B would need it, and B is deferred (§4). Adding a capability flag with no backend that sets it differently is speculative. | Yes, and it belongs with B if B is ever taken. |
| D7 | A `calimbra` golden family? | **Yes if, and only if, calimbra ever hosts sandboxes** — as a **separate manifest** (`images.calimbra.json`), never by mutating `images.carnyx.json`. | Different host, different size envelope, different reachability. Sharing one manifest across hosts is how the live `agent-base` inconsistency (see the rewrite SPEC's deviation #2) happened. | Yes. |
| D8 | Golden *build* location | **carnyx** (or a CI job on a host with the ISO and ≥8 GB free), never calimbra. | You cannot build a 4 GB golden on a 3.8 GB host. This is forced, not chosen. | Yes, if calimbra is resized. |

---

## 1. Problem

calimbra is being stood up as molimo's IaC'd replacement, and #45 records an assumption in the `cloud-vps` design decision that is now false:

> molimo's replacement was recorded as **Incus system containers**, chosen explicitly because the target is a VPS and nested virt was considered unavailable.

Nested virt **is** available on calimbra (verified 2026-11-01: `/proc/cpuinfo` has `vmx ept vpid hypervisor`, `kvm_intel` + `kvm` loaded, `KVM_GET_API_VERSION → 12`, `KVM_CREATE_VM → SUCCESS`, `KVM_CHECK_EXTENSION NESTED_STATE → 1`). That turns a forced decision into a genuine three-way choice, and this spec makes it.

The decision matters beyond calimbra because the `Backend` trait's shape is shared: whatever a fourth backend needs leaks into the contract that libvirt and exedev already satisfy.

---

## 2. What is verified, and what is not

Honesty section. Everything below is either measured (with provenance) or explicitly marked as an open measurement. **Nothing in this spec is a guess presented as a number.**

### 2.1 Measured on calimbra, 2026-11-01

Provenance: `danialrami/molimo` → `docs/incus-findings.md`; summarized in #45's comment.

| | container | VM |
|---|---|---|
| boot to IP | **~6.1 s** | **~697 s** |
| cloud-init | absent from the image | present; works **only via profile** user-data |
| pubkey via cloud-init | impossible | works (profile) |
| sshd in stock image | no | no (`NO_SSHD`) |
| sshd installable | **yes** — `apt-get install openssh-server` rc=0, `sshd -t` `CONFIG_OK`, binds `:22` | not attempted |

Host facts: Ubuntu 24.04.5, kernel 6.8.0-139, **6 vCPU Intel Xeon E5-2697 v2 (Ivy Bridge-EP, 2013)**, **3.8 GiB RAM + 1.0 GiB swap**, 217 GB ext4, Incus 7.6, cgroup2, AppArmor (116 profiles), `overlay` on demand. **CPU flags for compute: `aes` only — no `avx2`, no `sha_ni`, no `avx512`.**

Concurrency measured: **three 1 GiB VMs coexist in 3.8 GiB with 2.3 GiB free and zero swap.**

### 2.2 Two figures from #45's own body are retracted

Recorded here so they don't get re-quoted:

- The **"243 s VM boot"** figure is **void**. That measurement booted `images:ubuntu/24.04`, which is `Type: container` — a squashfs rootfs under `--vm`. Re-measured with a real VM image (`ubuntu/24.04/desktop`), boot-to-IP is **~697 s**. **The VM is ~115× a container, not ~40×.**
- The claim that **RAM is the binding constraint** is **too strong**. Three 1 GiB VMs fit with 2.3 GiB to spare. RAM is a real ceiling at *golden* sizes, but not for a handful of small sandboxes.

### 2.3 NOT measured — open, with the command that closes it

| Open question | Command | Blocks |
|---|---|---|
| Multi-VM concurrency ceiling at **golden** sizes (≥2 GiB) | `for i in 1 2 3; do incus launch --vm images:ubuntu/24.04/desktop v$i -c limits.memory=2GiB & done; incus list` | D1 |
| Does `incus virtual-machine` support **snapshot**? | `incus snapshot create v1 s1` | §7 |
| Does `incus virtual-machine` expose a **console** in the sense `capabilities().console` means? | `incus console v1 --type=vga` | §7 |
| `incus publish` / qcow2 transfer cost over the tailnet | time a 4 GB `incus publish` + pull | §6 |
| Whether a 1 GiB golden is actually *useful* for agent work | run `agent-base`'s healthchecks against one | D7 |

None of these block the decisions above: D1–D4 follow from RAM and the 697 s figure alone, and D5–D8 are contract decisions that no measurement changes.

---

## 3. Decision 1 — hosting shape

### 3.1 Option ① (containers) fails on RAM as well as contract

The live goldens, from `images.carnyx.json` (verified against the file, not from memory):

| golden | flavour | cpu | memory | disk |
|---|---|---|---|---|
| `agent-base` | agent | 2 | **4 GB** | — |
| `ci-runner` | ci-runner | 4 | **8 GB** | — |
| `agent-web` | desktop | 2 | **4 GB** | — |
| `win11-desktop` | desktop | 4 | **8 GB** | 27 GB |
| `kali` | agent | 2 | **4 GB** | — |

calimbra has **3.8 GiB total**. Every golden **meets or exceeds the entire box**; `ci-runner` is 2.1× it. So *no existing golden is usable on calimbra at its declared size*, container or VM.

### 3.2 …and it fails a second, independent test the issue doesn't make

The goldens' `healthcheck` arrays are **VM-shaped**, not just their RAM:

- `kali`: `systemctl is-active tor`, plus a `--socks5-hostname 127.0.0.1:9050` curl to `check.torproject.org`
- `agent-web`: `sudo systemctl start lsbx-desktop.service` then `systemctl is-active --quiet`
- `ci-runner`: `docker image inspect myoung34/github-runner:ubuntu-noble`
- `win11-desktop`: `curl ... -o NUL` (a `cmd.exe` command line)

An Incus **system container** has no systemd, no Docker daemon, and no `cmd.exe`. It therefore cannot satisfy these healthchecks **regardless of RAM**. So option ① doesn't merely distort the `Backend` contract — it invalidates every golden in the registry, which is a much larger cost than the issue's framing ("no cloud-init, `os` meaningless") implies.

This is the strongest single argument in this spec, and it's independent of §3.1: even a resized calimbra would not make ① work with today's goldens.

### 3.3 Option ② (Incus VMs) fails as a *general* answer on boot time

**~697 s boot-to-IP.** For a CI runner that must register and pick up a job, or an agent sandbox created per task, that is unusable. The measured container boot (**~6.1 s**) is ~115× faster and is exactly what services want.

So the honest reading of the numbers is not "containers or VMs" but **"which workload goes where"** — which is option ③.

### 3.4 Decision

Adopt **③'s split**, with the sandbox half explicitly **not** on calimbra:

- **calimbra runs services, in Incus system containers**: caddy, forgejo, runner slots, litellm, and **the `lsbx` CI broker** (a long-lived service — the natural landing spot for the molimo→calimbra migration, which is itself gated on #43).
- **lsbx sandboxes and CI runners stay on carnyx** (libvirt, with the real goldens and the space to run them), or move to a host sized for them.

**The blunt version:** calimbra cannot be self-sufficient for lsbx sandbox work. A 3.8 GiB box with no AVX2 and a 2013 Xeon can host a couple of 1 GiB containers comfortably and a couple of 1 GiB VMs awkwardly. It cannot host a 4 GB `agent-base` or an 8 GB `ci-runner`, and those are the workloads the sandbox system exists to run.

Flagged as a deviation from #45's framing: the issue treats "calimbra takes over lsbx sandbox provisioning" as given and asks only *how*. The measurements say the premise needs revisiting first. **Reversible** — resize calimbra and D1 flips.

---

## 4. Decision 2 — the contract question (A/B/C)

This is the decision #45's comment correctly identifies as the real one, and it is independent of §3: it applies to any container-shaped backend, on any host.

The constraint, precisely (`crates/lsbx-kernel/src/backend.rs`, verified):

```rust
pub struct CreatedVm {
    pub vm_tag: String,
    pub host: String,              // SSH-reachable host
    pub https_url: Option<String>,
}
```

`run()`, `put_file()`, `get_file()` all take `identity_file: Option<&Path>`. The implied model is *create a guest → inject a pubkey → SSH to `host` → shell in*. That is a VM model. A container has no cloud-init to inject the key with, so the interface has nothing to hang on.

But note what the interface actually **promises**: "run a command in the sandbox", "move a file in", "move a file out", "destroy it". It does **not** promise SSH. SSH is an implementation choice that leaked into the type signature.

| option | shape | cost |
|---|---|---|
| **A** | Keep SSH; ship an `sshd` in the golden | `create_from_golden` unchanged; measured to work in a container (§2.1). Golden must carry sshd + a known user — which carnyx's goldens already do. Risk: a per-sandbox sshd is a process and an attack surface for something whose job is "run one job, get destroyed". |
| **B** | Add `exec` as the primary primitive; `run()` reimplemented on top per backend (`ssh` for libvirt/exedev, `incus exec` for incus) | An `lsbx-kernel` change touching every backend; `Capabilities` grows a flag. Containers need no sshd, no cloud-init, no pubkey. Also makes exedev's `run()` faster (no SSH round-trip) and removes `identity_file` from the hot path. |
| **C** | No contract change; a golden is responsible for being reachable, expressed through the existing `healthcheck` list | Least code, no trait churn, matches how carnyx already works. |

**Decision: C first, B deferred.**

1. It requires **no `lsbx-kernel` change**, so it cannot destabilize libvirt/carnyx or exedev/molimo — both of which are live.
2. It is **measured to work** (§2.1: sshd installs in a container, `sshd -t` passes, binds `:22`).
3. It puts the responsibility where the evidence says it belongs: a golden that cannot be reached is a broken golden, and `healthcheck` is the existing mechanism for saying so. `ci-runner`'s healthcheck already asserts specific in-guest state (`test -x /opt/lufs-runner/lufs-runner.sh`), so adding sshd reachability is consistent rather than novel.
4. B stays available as an **additive** refactor, not a rewrite.

Open questions C does **not** settle, to be answered when a container backend is actually specced (not now):

1. Does `healthcheck` gain a **required** SSH-reachability entry, or does each golden declare its own?
2. For a container backend, what does `CreatedVm.host` mean? The container's IP inside `10.172.224.0/24` is only reachable from the Incus host — fine for `lsbx serve` running on the same host, but it should be stated explicitly rather than left implicit.

---

## 5. Golden sizing policy

**If** calimbra ever hosts sandboxes (D1 says it does not today):

- A **separate manifest**, `images.calimbra.json`. Never mutate `images.carnyx.json` to fit another host.
- Target envelope: **1 vCPU / 1 GiB** sandboxes, one golden (`calimbra-micro`, flavour `agent`). At 3.8 GiB total this allows ~2–3 concurrent sandboxes, matching the measured three-1 GiB-VM result.
- **No `ci-runner`** (4 vCPU/8 GB) and **no Windows** golden on calimbra. Both are larger than the host.
- Every golden in this family must declare a `healthcheck` that is satisfiable **on that host** — the §3.2 lesson generalised into a policy.

---

## 6. Where goldens are built, and how they move

Forced by arithmetic, not preference: **you cannot build a 4 GB golden on a 3.8 GB host.**

- **Build on carnyx** (`qemu:///system`, local KVM, has the ISOs and space), or as a CI job on a host with the ISO and ≥8 GB free.
- **Transfer** as qcow2 over the tailnet, or `incus publish` + pull. **Cost is unmeasured** (§2.3) — measure before committing to a mechanism, because a 4 GB transfer per golden revision is a real operational cost.
- The existing flatten step (`qemu-img convert`, backing=none, from `lsbx-golden`) stays where it is; a golden that arrives on another host must already be flattened, or the transfer carries a backing-file dependency that will not resolve.

---

## 7. `capabilities()` semantics

Current shape (verified):

```rust
pub struct BackendCapabilities {
    pub console: bool,
    pub remote_transport: bool,
    pub snapshot: bool,
}
```

- A **container** backend has no `console` and no `snapshot` in the VM sense → those are `false`. `remote_transport` is `false` too: there is no SSH transport, only `incus exec` on the host.
- Do **not** add an `exec_native` flag now (D6). It only has meaning once Option B exists, and a capability flag no backend sets differently is speculative surface.

**Open** (§2.3): whether `incus virtual-machine` supports `snapshot` and what `console` means there. Unmeasured, so unspecified here rather than guessed.

---

## 8. Streaming / NAT traversal

- molimo's **exedev** backend needed **no** stream proxy — exe.dev's edge forwards ports.
- carnyx's **libvirt** backend **does** — `lsbx-carnyx-streams`, currently **DOWN** on `100.125.210.60:8244`.
- An Incus-VM backend on calimbra faces the **libvirt-shaped** problem: guest ports are on a private bridge (`10.172.224.0/24`), reachable only from the Incus host. The answer is the existing `lsbx serve` stream proxy co-located on the host — the same shape as carnyx, not a new mechanism.
- This is another reason D1's "services on calimbra" is the right role: `lsbx serve` alongside the broker is a service, and the console/stream door only becomes a hard problem when you also host sandboxes.

---

## 9. What this spec does NOT do

- **No implementation.** No `lsbx-backend-incus` crate. No `Backend` trait change.
- **No measurement claims beyond §2.1.** Every unmeasured number is labelled.
- **No change to `images.carnyx.json`, `images.json`, or any golden.**
- **No decision on whether the exedev backend survives.** #38 (exe.dev Windows semantics) is a *separate* question, and its value depends on whether exedev outlives molimo — if calimbra replaces molimo and sandboxes stay on carnyx, #38 is likely throwaway work. Flagged here, decided in #38.

---

## 10. Acceptance criteria for this phase

1. A decision is recorded for D1–D8, with the rejected options and the reason each was rejected. ✅ (this document)
2. The measured numbers are cited with provenance, and the two retracted figures are marked void. ✅ (§2.1, §2.2)
3. Every remaining unknown is listed as an open measurement with a command that closes it, rather than estimated. ✅ (§2.3)
4. No code changes. ✅
5. Daniel ratifies or overturns D1 (the surprising one) before any container backend is specced. ⏳

---

## 11. Consequence for the issue queue

- **#43 is on the critical path for this spec's adopted plan**, not adjacent to it: calimbra's first job is the **broker**, and migrating a broker that 401s for ~64% of every hour just relocates the outage. Fix #43, *then* migrate.
- **#38** should be re-evaluated after D1 is ratified, for the reason in §9.
- **#36/#37/#39** (the Windows cluster) are unaffected by this spec — they live on carnyx/libvirt, which this spec keeps as the sandbox host.
