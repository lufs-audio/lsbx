# Unit 02 — Tor mode architecture: fail-closed `locked`, IPv6/QUIC closure

## Objective
Harden the `locked` egress model so that it fails closed for every traffic class the v1 rules
missed (IPv6, non-DNS UDP), and add a logged `tor-mode` helper whose `open` state is a
default-DROP allowlist rather than an unrestricted passthrough.

## Context
- `docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md` §2.3 — the v1 rule set, in order:
  return for the `debian-tor` UID; return loopback and RFC 1918; redirect outbound UDP/53 to
  `DNSPort:5353`; redirect outbound TCP SYN to `TransPort:9040`.
- Two gaps are inherited from v1 and must be closed here:
  1. **IPv6** — the v1 rules are v4-only. A guest with SLAAC on `virbr0` can bypass `REDIRECT`.
  2. **Non-DNS UDP** — `QUIC`/UDP 443 cannot be carried by Tor; v1 leaves it undefined.
- Fail-closed principle: `REDIRECT` into a dead port already fails closed. Any `open` state must
  preserve that property via policy `DROP` plus an explicit allowlist.
- `docs/specs/2026-09-17T2114Z_kali-tor-golden/SPEC.md` §2.3 also records the constraint that
  host-to-guest SSH from `192.168.122.0/24` must keep working undisturbed.

## Acceptance criteria
- [ ] With `tor` stopped, `curl -fsS --max-time 5 https://example.com` fails (no egress).
- [ ] With `tor` running, `curl -fsS --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/api/ip` succeeds.
- [ ] IPv6 egress is blocked: `curl -6 -fsS --max-time 5 https://example.com` fails while locked.
- [ ] UDP/443 egress is dropped while locked (verified with a UDP probe; no leak).
- [ ] Host→guest SSH from `192.168.122.1` and RFC 1918 ranges are unaffected.
- [ ] `tor-mode open` installs `OUTPUT` policy `DROP` plus an allowlist file; it never yields
      unrestricted egress. `tor-mode locked` restores the redirect.
- [ ] Current mode is readable at `/var/lib/lsbx/tor-mode` and identical to what is enforced.
- [ ] Every mode change is logged to the journal with the invoking user.

## Interface contract
```bash
# /usr/local/bin/tor-mode
#   tor-mode locked    -> restore REDIRECT model (default at boot)
#   tor-mode open      -> remove redirect; OUTPUT policy DROP + allowlist
#   tor-mode status    -> prints "locked" | "open"; exit 0
#   tor-mode           -> prints status and exits 0
#
# /etc/lsbx/tor-allowlist       one CIDR or host per line; consumed in open mode only
# /var/lib/lsbx/tor-mode        contains exactly "locked" or "open", no trailing text
#
# iptables chains (v4 and v6): LSBX_TOR_OUT, LSBX_TOR_OPEN
```

## Boundaries
- **Touches:** `scripts/build-kali-golden.sh` (the `locked` provisioning block).
- **Does NOT touch:** `crates/**`, `images.carnyx.json`, desktop or browser packages, `aria2c`.

## Verification
```bash
lsbx exec <id> -- sudo tor-mode status
lsbx exec <id> -- curl -fsS --max-time 5 https://example.com ; echo "expect non-zero: $?"
lsbx exec <id> -- sudo systemctl stop tor
lsbx exec <id> -- curl -fsS --max-time 5 https://example.com ; echo "expect failure while tor down"
lsbx exec <id> -- curl -6 -fsS --max-time 5 https://example.com ; echo "expect failure (v6 closed)"
lsbx exec <id> -- cat /var/lib/lsbx/tor-mode
```
