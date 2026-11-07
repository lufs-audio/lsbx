# Networking: the guest bridge and why its MTU is 1280

Covers the libvirt backend's guest network on a bare-metal host (Carnyx is the
reference: `qemu:///system`, libvirt network `default`, bridge `virbr0`,
`192.168.122.0/24`). Read this before changing any `mtu` value, anywhere.

## The facts, as measured on Carnyx (2026-11-07)

| where | value | set by |
|---|---|---|
| `virbr0` (bridge) | `mtu 1280` | libvirt network XML: `<mtu size='1280'/>` — check with `virsh net-dumpxml default` |
| guest NIC (Linux) | `1280` | **lsbx's domain XML** `<mtu size='1280'/>` on the `<interface>` (`crates/lsbx-backend-libvirt/src/domain_xml.rs`) **and** cloud-init `ip link set dev ens3 mtu 1280` (`crates/lsbx-backend-libvirt/src/lib.rs`) |
| guest NIC (Windows) | `1280` | the same domain XML `<interface><mtu size='1280'/></interface>`; the guest inherits it, no cloud-init involved |
| `tailscale0` | `1280` | WireGuard's default, and Tailscale's practical ceiling without PMTUD cooperation |
| host NICs (`enp2s0`, `enp5s0`, `wlan0`), docker bridges | `1500` | host default |

It is set in three independent places on purpose. None of them is accidental,
and none is inherited from the host's own links — the host is a 1500 host.

## Why 1280

`1280` is the IPv6 minimum MTU and WireGuard's fixed ceiling. A guest at 1280
can be reached over **any** path that may traverse the tailnet without
depending on ICMP `fragmentation needed` arriving end to end:

- Tailscale carries packets inside WireGuard, whose IP packet budget is 1280.
  A 1500-byte guest packet has to be fragmented or ICMP-signalled down; if
  either is filtered anywhere in the path (Cloudflare, a guest firewall, a
  container network, a locked-down router), the result is the classic
  blackhole: TCP connects, then hangs after the banner, and a 1 KB request
  works while a 4 KB one does not.
- Guests are also reached through the gateway's `/sandboxes/<id>/exec` and
  (`streaming: novnc` goldens) the WebSocket relay; a uniform MTU keeps the
  whole path predictable instead of per-route.

The cost is real but bounded: `1280 − 20 (IPv4) − 20 (TCP) = 1240` byte MSS,
about 14% less payload per packet than a 1500 path. It is invisible for
`exec`, git, apt, CI logs and HTTP APIs. It is visible on multi-GB transfers
(`lsbx put`/`get` of an ISO, qcow2 copies) — where a local-only path pays for a
tailnet problem it does not have.

## Why not "just bump it to 1500"

Because the failure mode is intermittent and misdiagnosed as a hung service.
Any change here has to answer one question first: **does guest traffic ever
cross `tailscale0`?**

- If it can (subnet routing, console streaming from a tailnet client, or a
  future per-sandbox route), 1280 is required unless ICMP is guaranteed to
  flow — which it is not across Cloudflare or a guest firewall.
- If it never can (a host-local CI runner), 1500 is safe *for that guest*, but
  that means two networks, two DHCP ranges and a per-golden declaration — see
  `docs/specs/2026-11-07T1440Z_libvirt-guest-net-tailscale-route/SPEC.md` §4,
  which weighs all four options (keep 1280 / 1500 + PMTUD / split networks /
  MSS clamp at the router) and recommends keeping 1280.

## If you think this is wrong, measure first

Do not change it on intuition. Run, in this order, and record the numbers in
the PR that proposes the change:

1. `iperf3` between two guests on `virbr0`, with the current 1280.
2. The same pair with `ip link set dev <nic> mtu 1500` (temporary, not in the
   XML) — this shows the local-path gain.
3. `iperf3` host ↔ guest, and guest ↔ a tailnet peer, at both MTUs — this is
   where a 1500 guest that must cross `tailscale0` will hang or crawl.
4. A large `lsbx put`/`get` (≥1 GB) at both MTUs, since that is the real
   workload.

A change that only demonstrates step 2 is a regression waiting for step 3.

## Related knobs

- `LSBX_LIBVIRT_IMAGES_DIR` / `LSBX_LIBVIRT_VM_DIR` — where goldens and VM
  disks live (see README "Placement configuration").
- The libvirt network itself is host state, not repo state: changing
  `<mtu>` there means editing the network XML and restarting the network, and
  it affects every domain on it. `lsbx` sets the same value per-interface in
  its domain XML so a guest is correct even if the host network was created
  differently.
