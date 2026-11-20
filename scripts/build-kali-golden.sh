#!/usr/bin/env bash
# build-kali-golden: provisions a Kali golden image — desktop, agent-drivable
# Chromium, and (mode-dependent) the Tor egress stack.
#
# Usage:
#   ./scripts/build-kali-golden.sh --emit <locked|open> > /tmp/provision-kali-<mode>.sh
#   lsbx golden build --from kali-genericcloud-base \
#                     --script /tmp/provision-kali-<mode>.sh \
#                     --flavor desktop --cpu 4 --memory 8GB --os linux \
#                     --streaming novnc <name>
#
#   ./scripts/build-kali-golden.sh --mode <locked|open> [--dry-run]   # in-guest, for testing
#   ./scripts/build-kali-golden.sh --check                            # static self-check
#
# Why --emit exists
# -----------------
# `crates/lsbx-golden/src/build.rs` copies the provisioning script to a FIXED
# remote path and executes it as `["sh", "/tmp/lsbx-golden-build-script.sh"]` —
# sh (dash on Kali), no arguments, and no second file alongside it. A mode
# therefore cannot be passed as an argument, an env var, or a companion script.
# The only mechanism that survives that contract is baking the mode into the
# file's own bytes, which is what --emit does: it substitutes the sentinel on
# the KALI_MODE line below and writes a fully self-contained script.
#
# What it does, in order:
#   1. Re-exec under bash (build.rs invokes us with `sh`) and under sudo
#   2. apt update, then install base + desktop + browser packages
#   3. [locked] install tor + proxychains4, write tor-route / tor-mode / IP rules
#   4. [open]   install aria2c, write the "NO TOR" motd warning
#   5. Enable lsbx-desktop, chromium wrapper, and the tor units
#   6. APPLY THE TOR RULES LAST — the package installs above must not run
#      through the redirect
#
# Usage recommendations:
#   - Never apply the Tor rules before apt finishes: a redirected apt is slow
#     and can time out mid-provision, leaving a half-built golden.
#   - `tor-mode` is provisioned by this script only in `locked` images. There is
#     no runtime "turn Tor off" switch in an `open` image, by design — the two
#     goldens are the guarantee, and tor-mode's `open` state is a fail-closed
#     DROP+allowlist, never an unrestricted passthrough.
#   - 4 vCPU / 8 GB is specified in the phase spec, not here. At 4 GB the two
#     concurrent Chromium builds (system + Playwright's) push into swap.
#
# Example:
#   ./scripts/build-kali-golden.sh --emit locked > /tmp/p-locked.sh
#   bash -n /tmp/p-locked.sh && lsbx golden build --from kali-genericcloud-base \
#     --script /tmp/p-locked.sh --flavor desktop --cpu 4 --memory 8GB \
#     --os linux --streaming novnc --register kali-tor

KALI_MODE="__KALI_MODE__"
KALI_STATE_DIR="/var/lib/lsbx"
# noVNC is served from /usr/share/novnc and Chromium's CDP port defaults to
# 9222 (overridable at runtime via LSBX_CDP_PORT inside lsbx-chromium). Both
# are written literally in the quoted heredocs below so they survive unexpanded.

set -euo pipefail

# ---------------------------------------------------------------------------
# Static self-check (runs on the host; never provisions anything)
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--check" ]; then
  rc=0
  bash -n "$0" || rc=1
  if grep -q '^KALI_MODE="__KALI_MODE__"' "$0"; then
    echo "ok: mode sentinel present (source form; use --emit to bake)" >&2
  else
    echo "ok: mode already baked as '$(grep -m1 '^KALI_MODE=' "$0" | cut -d'"' -f2)'" >&2
  fi
  echo "ok: syntax" >&2
  exit $rc
fi

if [ "${1:-}" = "--emit" ]; then
  emit_mode="${2:-}"
  case "$emit_mode" in
    locked|open) ;;
    *) echo "usage: $0 --emit <locked|open>" >&2; exit 2 ;;
  esac
  sed "s/^KALI_MODE=\"__KALI_MODE__\"/KALI_MODE=\"${emit_mode}\"/" "$0"
  exit 0
fi

KALI_DRY_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) KALI_MODE="${2:-}"; shift 2 ;;
    --dry-run) KALI_DRY_RUN=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$KALI_MODE" in
  locked|open) ;;
  *)
    echo "ERROR: mode is '${KALI_MODE}'." >&2
    echo "This file is the source template. Run it with --mode, or use" >&2
    echo "'$0 --emit <locked|open>' to produce a self-contained script." >&2
    exit 2
    ;;
esac

# ---------------------------------------------------------------------------
# Re-exec: build.rs invokes `sh <script>`, which is dash on Kali.
# ---------------------------------------------------------------------------
if [ -z "${BASH_VERSION:-}" ]; then
  exec /bin/bash "$0" "$@"
fi

if [ "$(id -u)" -ne 0 ]; then
  if [ "$KALI_DRY_RUN" -eq 1 ]; then
    echo "[kali-$KALI_MODE] dry-run as uid $(id -u); continuing unprivileged (no writes)" >&2
  else
    exec sudo -E /bin/bash "$0" "$@"
  fi
fi

log() { printf '[kali-%s] %s\n' "$KALI_MODE" "$*" >&2; }

run() {
  if [ "$KALI_DRY_RUN" -eq 1 ]; then
    log "DRY-RUN: $*"
  else
    "$@"
  fi
}

write_file() {
  dest="$1"
  if [ "$KALI_DRY_RUN" -eq 1 ]; then
    log "DRY-RUN: write $dest"
    cat >/dev/null
    return 0
  fi
  mkdir -p "$(dirname "$dest")"
  cat >"$dest"
}

log "provisioning start; mode=$KALI_MODE dry_run=$KALI_DRY_RUN"

# ---------------------------------------------------------------------------
# 1. Base packages
# ---------------------------------------------------------------------------
export DEBIAN_FRONTEND=noninteractive
run apt-get update -y
run apt-get install -y --no-install-recommends \
  ca-certificates curl git jq openssh-server sudo ufw \
  kali-tools-top10 nmap nikto gobuster tcpdump tshark \
  procps python3

# The state dir must exist before any unit writes /var/lib/lsbx/tor-mode.
run mkdir -p "$KALI_STATE_DIR"

# ---------------------------------------------------------------------------
# 2. SSH hardening + sudoers (parity with lsbx-kali-v1)
# ---------------------------------------------------------------------------
write_file /etc/ssh/sshd_config.d/99-lsbx-hardening.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
X11Forwarding no
MaxAuthTries 4
EOF

write_file /etc/sudoers.d/99-lsbx <<'EOF'
lsbx   ALL=(ALL) NOPASSWD:ALL
exedev ALL=(ALL) NOPASSWD:ALL
EOF
run chmod 0440 /etc/sudoers.d/99-lsbx

# ---------------------------------------------------------------------------
# 3. Desktop stack — Xvfb-backed XFCE + x11vnc + websockify on :8000
#    Mirrors the agent-web convention: noVNC page on guest port 8000.
# ---------------------------------------------------------------------------
run apt-get install -y --no-install-recommends \
  xfce4 xfce4-terminal x11vnc xvfb dbus-x11 websockify novnc

write_file /usr/local/bin/lsbx-desktop-start <<'EOF'
#!/bin/bash
# Starts the noVNC desktop: Xvfb -> xfce4-session -> x11vnc -> websockify.
set -euo pipefail
DISPLAY_NUM=":0"
GEOMETRY="1920x1080x24"
cleanup() {
  [ -n "${XVFB_PID:-}" ]  && kill "$XVFB_PID"  2>/dev/null || true
  [ -n "${XFCE_PID:-}" ]  && kill "$XFCE_PID"  2>/dev/null || true
  [ -n "${VNC_PID:-}" ]   && kill "$VNC_PID"   2>/dev/null || true
  [ -n "${WS_PID:-}" ]    && kill "$WS_PID"    2>/dev/null || true
}
trap cleanup EXIT INT TERM
Xvfb "$DISPLAY_NUM" -screen 0 "$GEOMETRY" -nolisten tcp & XVFB_PID=$!
sleep 1
export DISPLAY="$DISPLAY_NUM"
dbus-run-session -- xfce4-session & XFCE_PID=$!
sleep 2
x11vnc -display "$DISPLAY_NUM" -localhost -rfbport 5900 -forever -shared -nopw -quiet & VNC_PID=$!
sleep 1
websockify --web=/usr/share/novnc 0.0.0.0:8000 127.0.0.1:5900 & WS_PID=$!
wait
EOF
run chmod 0755 /usr/local/bin/lsbx-desktop-start

write_file /etc/systemd/system/lsbx-desktop.service <<'EOF'
[Unit]
Description=LSBX desktop (XFCE + x11vnc + websockify)
After=network.target

[Service]
Type=simple
User=lsbx
Group=lsbx
Environment=HOME=/home/lsbx
ExecStart=/usr/local/bin/lsbx-desktop-start
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

# ---------------------------------------------------------------------------
# 4. Browser layer — system Chromium (CDP on loopback only) + Playwright's own
#    Chromium + Firefox ESR. Deliberately NOT Google Chrome: no third-party
#    apt repo in a Tor-capable image.
# ---------------------------------------------------------------------------
run apt-get install -y --no-install-recommends chromium firefox-esr nodejs npm

write_file /usr/local/bin/lsbx-chromium <<'EOF'
#!/bin/bash
# Launches the system Chromium with a CDP endpoint on LOOPBACK ONLY.
# A remote-debugging-port is remote code execution in the browser's context;
# it must never bind 0.0.0.0 or the guest's 192.168.122.x address.
set -euo pipefail
if [ "$(id -u)" -eq 0 ]; then
  echo "lsbx-chromium: refusing to run as root (no --no-sandbox in this image)" >&2
  exit 1
fi
CDP_PORT="${LSBX_CDP_PORT:-9222}"
exec chromium \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port="$CDP_PORT" \
  --user-data-dir="$HOME/.config/lsbx-chromium" \
  "$@"
EOF
run chmod 0755 /usr/local/bin/lsbx-chromium

# Playwright's bundled Chromium must live in the *guest user's* cache, so this
# runs as lsbx, not root.
if [ "$KALI_DRY_RUN" -eq 1 ]; then
  log "DRY-RUN: sudo -u lsbx npx playwright install --with-deps chromium"
else
  log "installing Playwright's bundled Chromium as lsbx (large download)"
  sudo -u lsbx -H bash -lc 'npx --yes playwright install --with-deps chromium' \
    || log "WARN: playwright install failed; golden will still verify on system Chromium"
fi

# ---------------------------------------------------------------------------
# 5. Mode-specific: Tor stack (locked) or torrent tool + warning (open)
# ---------------------------------------------------------------------------
if [ "$KALI_MODE" = "locked" ]; then

  run apt-get install -y --no-install-recommends tor proxychains4

  write_file /usr/local/bin/tor-route <<'EOF'
#!/bin/bash
# Installs the transparent Tor egress rules. Fail-closed by construction:
# traffic is REDIRECTed into Tor's TransPort, so if tor dies the redirect
# points at a dead port and connections FAIL rather than escaping.
#
# Closes two gaps inherited from lsbx-kali-v1:
#   - IPv6: v1's rules were v4-only, so SLAAC traffic bypassed REDIRECT
#   - non-DNS UDP: QUIC/UDP-443 cannot be carried by Tor; it is REJECTed
set -euo pipefail

OWNER_UID="$(id -u debian-tor 2>/dev/null || echo 0)"

for t in iptables ip6tables; do
  command -v "$t" >/dev/null 2>&1 || { echo "tor-route: $t missing" >&2; exit 1; }
done

# ---- v4 NAT: redirect into Tor -------------------------------------------
iptables -t nat -N LSBX_TOR_OUT 2>/dev/null || iptables -t nat -F LSBX_TOR_OUT
iptables -t nat -A LSBX_TOR_OUT -m owner --uid-owner "$OWNER_UID" -j RETURN
iptables -t nat -A LSBX_TOR_OUT -o lo -j RETURN
iptables -t nat -A LSBX_TOR_OUT -d 127.0.0.0/8    -j RETURN
iptables -t nat -A LSBX_TOR_OUT -d 192.168.0.0/16 -j RETURN
iptables -t nat -A LSBX_TOR_OUT -d 10.0.0.0/8     -j RETURN
iptables -t nat -A LSBX_TOR_OUT -d 172.16.0.0/12  -j RETURN
iptables -t nat -A LSBX_TOR_OUT -p udp --dport 53 -j REDIRECT --to-ports 5353
iptables -t nat -A LSBX_TOR_OUT -p tcp            -j REDIRECT --to-ports 9040
iptables -t nat -C OUTPUT -j LSBX_TOR_OUT 2>/dev/null \
  || iptables -t nat -A OUTPUT -j LSBX_TOR_OUT

# ---- v4 filter: reject non-DNS UDP (QUIC and friends) --------------------
iptables -N LSBX_TOR_UDP 2>/dev/null || iptables -F LSBX_TOR_UDP
iptables -A LSBX_TOR_UDP -m owner --uid-owner "$OWNER_UID" -j RETURN
iptables -A LSBX_TOR_UDP -o lo -j RETURN
iptables -A LSBX_TOR_UDP -d 192.168.0.0/16 -j RETURN
iptables -A LSBX_TOR_UDP -d 10.0.0.0/8     -j RETURN
iptables -A LSBX_TOR_UDP -d 172.16.0.0/12  -j RETURN
iptables -A LSBX_TOR_UDP -p udp --dport 53 -j RETURN
iptables -A LSBX_TOR_UDP -p udp            -j REJECT --reject-with icmp-port-unreachable
iptables -C OUTPUT -j LSBX_TOR_UDP 2>/dev/null \
  || iptables -A OUTPUT -j LSBX_TOR_UDP

# ---- v6: no egress at all except loopback / link-local / ULA ------------
ip6tables -N LSBX_V6_OUT 2>/dev/null || ip6tables -F LSBX_V6_OUT
ip6tables -A LSBX_V6_OUT -o lo         -j RETURN
ip6tables -A LSBX_V6_OUT -d fe80::/10  -j RETURN
ip6tables -A LSBX_V6_OUT -d fc00::/7   -j RETURN
ip6tables -A LSBX_V6_OUT -j REJECT --reject-with icmp6-adm-prohibited
ip6tables -C OUTPUT -j LSBX_V6_OUT 2>/dev/null \
  || ip6tables -A OUTPUT -j LSBX_V6_OUT

# Defensive: this unit can also be started by systemd on boot, before any
# provisioning-side mkdir. A missing dir here previously failed the whole unit
# *after* the iptables rules had already been applied.
mkdir -p /var/lib/lsbx
echo locked > /var/lib/lsbx/tor-mode
echo "tor-route: locked egress active (v4 redirect, v6 closed, non-DNS UDP rejected)" >&2
EOF
  run chmod 0755 /usr/local/bin/tor-route

  write_file /usr/local/bin/tor-mode <<'EOF'
#!/bin/bash
# tor-mode — flips the egress model in a *locked* golden.
#
#   locked  REDIRECT into Tor (default; fail-closed because Tor-down => dead port)
#   open    redirect removed, OUTPUT policy DROP + allowlist. NEVER unrestricted:
#           "open" means "direct to the allowlist only", not "leak everything".
#
# There is no way to reach an unrestricted state through this helper, by design.
set -euo pipefail
STATE_FILE=/var/lib/lsbx/tor-mode
ALLOWLIST=/etc/lsbx/tor-allowlist

[ "$(id -u)" -eq 0 ] || {
  # `status` is a pure read and must work unprivileged, so healthchecks and
  # agents can query the mode without sudo. Only mode *changes* need root.
  if [ "${1:-status}" = "status" ]; then
    cat /var/lib/lsbx/tor-mode 2>/dev/null || echo unknown
    exit 0
  fi
  echo "tor-mode: needs root" >&2
  exit 1
}

allow_base() {
  iptables -A OUTPUT -o lo -j ACCEPT
  iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A OUTPUT -d 192.168.0.0/16 -j ACCEPT
  iptables -A OUTPUT -d 10.0.0.0/8     -j ACCEPT
  iptables -A OUTPUT -d 172.16.0.0/12  -j ACCEPT
}

case "${1:-status}" in
  locked)
    iptables -P OUTPUT ACCEPT
    iptables -t nat -C OUTPUT -j LSBX_TOR_OUT 2>/dev/null \
      || iptables -t nat -A OUTPUT -j LSBX_TOR_OUT
    if command -v logger >/dev/null 2>&1; then
      logger -t tor-mode "mode=locked by ${SUDO_USER:-root}"
    fi
    echo locked > "$STATE_FILE"
    echo locked
    ;;
  open)
    iptables -t nat -D OUTPUT -j LSBX_TOR_OUT 2>/dev/null || true
    iptables -F OUTPUT
    iptables -P OUTPUT DROP
    allow_base
    if [ -f "$ALLOWLIST" ]; then
      while IFS= read -r entry; do
        case "$entry" in ''|\#*) continue ;; esac
        iptables -A OUTPUT -d "$entry" -j ACCEPT
      done < "$ALLOWLIST"
    fi
    if command -v logger >/dev/null 2>&1; then
      logger -t tor-mode "mode=open by ${SUDO_USER:-root} (DROP + allowlist)"
    fi
    echo open > "$STATE_FILE"
    echo open
    ;;
  status)
    cat "$STATE_FILE" 2>/dev/null || echo unknown
    ;;
  *)
    echo "usage: tor-mode [locked|open|status]" >&2
    exit 2
    ;;
esac
EOF
  run chmod 0755 /usr/local/bin/tor-mode

  write_file /etc/lsbx/tor-allowlist <<'EOF'
# Hosts/CIDRs reachable in `tor-mode open`. Consumed only in open mode.
# Deliberately minimal; add with intent, then journalctl -t tor-mode to audit.
deb.debian.org
http.kali.org
EOF

  write_file /etc/systemd/system/lsbx-tor-route.service <<'EOF'
[Unit]
Description=LSBX transparent Tor egress rules
After=network-online.target tor.service
Wants=network-online.target
Requires=tor.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/tor-route

[Install]
WantedBy=multi-user.target
EOF

  # Debian/Kali's torrc is not guaranteed to include a torrc.d directory, so
  # append under a marker rather than relying on %include support.
  if ! grep -q '^# lsbx-begin' /etc/tor/torrc 2>/dev/null; then
    if [ "$KALI_DRY_RUN" -eq 1 ]; then
      log "DRY-RUN: append Tor ports to /etc/tor/torrc"
    else
      cat >>/etc/tor/torrc <<'EOF'
# lsbx-begin
TransPort 127.0.0.1:9040
DNSPort 127.0.0.1:5353
SocksPort 127.0.0.1:9050
# lsbx-end
EOF
    fi
  fi

  write_file /etc/proxychains4.conf <<'EOF'
strict_chain
proxy_dns
tcp_read_time_out 15000
tcp_connect_time_out 8000
[ProxyList]
socks5 127.0.0.1 9050
EOF

else # ---- mode: open -------------------------------------------------------

  run apt-get install -y --no-install-recommends aria2

  write_file /etc/motd.d/00-kali-no-tor <<'EOF'

  ------------------------------------------------------------------
   This golden has NO Tor. All egress is direct from the hypervisor.
   For torified egress use:   lsbx up kali-tor
  ------------------------------------------------------------------

EOF

fi

# ---------------------------------------------------------------------------
# 6. UFW — 8000 from the libvirt subnet only; default deny inbound
# ---------------------------------------------------------------------------
run ufw --force reset
run ufw default deny incoming
run ufw default allow outgoing
run ufw allow 22/tcp
run ufw allow from 192.168.122.0/24 to any port 8000 proto tcp
run ufw --force enable

# ---------------------------------------------------------------------------
# 7. Enable services. Tor rules LAST, so the installs above never ran through
#    the redirect (a redirected apt is slow and can time out mid-provision).
# ---------------------------------------------------------------------------
run systemctl daemon-reload
run systemctl enable lsbx-desktop.service
run systemctl start lsbx-desktop.service

if [ "$KALI_MODE" = "locked" ]; then
  run systemctl enable tor.service
  run systemctl enable lsbx-tor-route.service
  run systemctl start tor.service
  run systemctl start lsbx-tor-route.service
fi

if [ "$KALI_MODE" = "locked" ]; then
  log "done: locked golden (Tor egress, no torrent client)"
else
  log "done: open golden (no Tor, aria2c present)"
fi
