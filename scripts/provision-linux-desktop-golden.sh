#!/usr/bin/env bash
# provision-linux-desktop-golden: reproducible Linux XFCE desktop provisioning.
#
# Provenance (important): the legacy repository itself was NOT available while
# this file was written. The attached read-only GitHub integration returned 404
# for lufs-audio/lufs-sandbox-server and no local checkout existed. Therefore the
# compatibility mode below is a reconstruction, not a byte-for-byte import. It
# is derived from the package and command excerpts recorded in lsbx commit
# 411723b7ea263f7d9ecb027d04e443fcdce2d035 (2026-11-13), especially:
# docs/specs/2026-11-14T0349Z_console-and-resize-followups/units/
# 04-linux-desktop-recipe-import.md. The reported legacy source path was:
# /home/carnyx/repos/lufs-sandbox-server/scripts/build-desktop-golden.sh
#
# Modes are deliberately versioned:
#   compat-xvfb-websockify-v1 (default)
#       Reconstructs the existing static Xvfb -> x11vnc -> websockify stack.
#       This preserves #58's no-behaviour-change contract, but it is incompatible
#       with the current gateway relay, which expects raw RFB on guest port 8000.
#   tigervnc-raw-rfb-v1 (EXPLICIT CANARY; never selected by a manifest here)
#       Runs TigerVNC Xvnc directly on STREAM_PORT as raw RFB. Xvnc supplies the
#       RANDR/ExtendedDesktopSize path needed by #51. The gateway console can opt
#       in per session with /console?target=<id>&resize=remote.
#
# Usage (inside the guest being baked):
#   sudo ./scripts/provision-linux-desktop-golden.sh
#   sudo env LSBX_LINUX_DESKTOP_MODE=tigervnc-raw-rfb-v1 \
#        RFB_PRIVATE_CIDR=192.168.122.0/24 \
#        VNC_PASSWORD_FILE=/run/lsbx-vnc-password \
#        ./scripts/provision-linux-desktop-golden.sh
#
# Static/test helpers (do not need root):
#   ./scripts/provision-linux-desktop-golden.sh --check
#   ./scripts/provision-linux-desktop-golden.sh --print-launcher tigervnc-raw-rfb-v1
#   ./scripts/provision-linux-desktop-golden.sh --emit tigervnc-raw-rfb-v1 \
#       > /tmp/provision-linux-canary-v1.sh
#
# Configuration:
#   DISTRO=auto|ubuntu|debian|arch       (default: auto from /etc/os-release)
#   NOVNC_DIR=/usr/share/novnc           (compat mode only)
#   STREAM_PORT=8000                     (websockify in compat; raw RFB in canary)
#   VNC_PASSFILE=/etc/lsbx-vnc.pass
#   VNC_PASSWORD_FILE=/run/lsbx-vnc-password  (recommended canary input)
#   VNC_PASSWORD=...                     (alternate canary input; never an argv)
#   RFB_PRIVATE_CIDR=192.168.122.0/24    (required canary interface selector)
#   DESKTOP_USER=lsbx
#   DISPLAY_NUMBER=0
#   INITIAL_GEOMETRY=1280x800
#   LSBX_LINUX_DESKTOP_MODE=<mode above>
#
# The script never writes the plaintext VNC password to disk. Compatibility mode
# may default to the existing `lsbx` password and uses x11vnc -storepasswd as
# required by #58. Canary mode has no password default: the operator must provide
# an eight-character VNC password via a root-only file (recommended) or env.
# Canary encoding uses tigervncpasswd -f over stdin, never a password argv.

if [ -z "${BASH_VERSION:-}" ]; then
    exec /bin/bash "$0" "$@"
fi
set -euo pipefail

readonly COMPAT_MODE="compat-xvfb-websockify-v1"
readonly CANARY_MODE="tigervnc-raw-rfb-v1"

MODE="${LSBX_LINUX_DESKTOP_MODE:-$COMPAT_MODE}"
DISTRO="${DISTRO:-auto}"
NOVNC_DIR="${NOVNC_DIR:-/usr/share/novnc}"
STREAM_PORT="${STREAM_PORT:-8000}"
VNC_PASSFILE="${VNC_PASSFILE:-/etc/lsbx-vnc.pass}"
VNC_PASSWORD_FILE="${VNC_PASSWORD_FILE:-}"
# Capture an explicitly supplied env secret, then immediately remove it from the
# exported environment before this shell starts apt, encoders, or any other child.
VNC_PASSWORD_ENV_SET=0
VNC_PASSWORD_VALUE=''
if [[ -v VNC_PASSWORD ]]; then
    VNC_PASSWORD_ENV_SET=1
    VNC_PASSWORD_VALUE=$VNC_PASSWORD
fi
unset VNC_PASSWORD
RFB_PRIVATE_CIDR="${RFB_PRIVATE_CIDR:-}"
DESKTOP_USER="${DESKTOP_USER:-lsbx}"
DISPLAY_NUMBER="${DISPLAY_NUMBER:-0}"
INITIAL_GEOMETRY="${INITIAL_GEOMETRY:-1280x800}"
LAUNCHER_PATH="${LSBX_DESKTOP_LAUNCHER:-/usr/local/libexec/lsbx-linux-desktop}"
UNIT_PATH="${LSBX_DESKTOP_UNIT:-/etc/systemd/system/lsbx-desktop.service}"
CHECK_PATH="${LSBX_DESKTOP_CHECK:-/usr/local/bin/lsbx-linux-desktop-check}"

log() { printf '[linux-desktop] %s\n' "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }

usage() {
    cat <<EOF
Usage: $0 [--mode $COMPAT_MODE|$CANARY_MODE]
       $0 --check
       $0 --print-launcher [$COMPAT_MODE|$CANARY_MODE]
       $0 --emit $COMPAT_MODE|$CANARY_MODE
EOF
}

validate_mode() {
    case "$1" in
        "$COMPAT_MODE"|"$CANARY_MODE") ;;
        *) die "unsupported mode '$1'" ;;
    esac
}

validate_config() {
    validate_mode "$MODE"
    [[ "$STREAM_PORT" =~ ^[0-9]+$ ]] || die "STREAM_PORT must be numeric"
    (( STREAM_PORT >= 1 && STREAM_PORT <= 65535 )) || die "STREAM_PORT must be 1..65535"
    [[ "$DISPLAY_NUMBER" =~ ^[0-9]+$ ]] || die "DISPLAY_NUMBER must be numeric"
    [[ "$INITIAL_GEOMETRY" =~ ^[0-9]+x[0-9]+$ ]] || die "INITIAL_GEOMETRY must be WIDTHxHEIGHT"
    [[ "$DESKTOP_USER" =~ ^[a-z_][a-z0-9_-]*[$]?$ ]] || die "invalid DESKTOP_USER"
    for path in "$NOVNC_DIR" "$VNC_PASSFILE" "$LAUNCHER_PATH" "$UNIT_PATH" "$CHECK_PATH"; do
        [[ "$path" = /* && "$path" != *$'\n'* ]] || die "paths must be absolute and single-line: '$path'"
    done
}

shell_quote() { printf '%q' "$1"; }

render_compat_launcher() {
    local q_user q_display q_geometry q_pass q_novnc q_port
    q_user=$(shell_quote "$DESKTOP_USER")
    q_display=$(shell_quote ":$DISPLAY_NUMBER")
    q_geometry=$(shell_quote "$INITIAL_GEOMETRY")
    q_pass=$(shell_quote "$VNC_PASSFILE")
    q_novnc=$(shell_quote "$NOVNC_DIR")
    q_port=$(shell_quote "$STREAM_PORT")
    cat <<EOF
#!/usr/bin/env bash
# Reconstructed compatibility stack: fixed Xvfb, x11vnc on loopback, websockify.
set -euo pipefail
export HOME=\$(getent passwd $q_user | cut -d: -f6)
export DISPLAY=$q_display
pids=()
cleanup() {
    local pid
    for pid in "\${pids[@]:-}"; do kill "\$pid" 2>/dev/null || true; done
}
trap cleanup EXIT INT TERM
Xvfb "\$DISPLAY" -screen 0 ${q_geometry}x24 -nolisten tcp & pids+=("\$!")
sleep 1
dbus-run-session -- startxfce4 & pids+=("\$!")
sleep 2
x11vnc -display "\$DISPLAY" -rfbauth $q_pass -localhost -listen 127.0.0.1 \\
    -rfbport 5900 -shared -forever -o /var/log/lsbx-x11vnc.log & pids+=("\$!")
sleep 1
websockify --web $q_novnc 0.0.0.0:$q_port 127.0.0.1:5900 & pids+=("\$!")
wait -n "\${pids[@]}"
EOF
}

render_canary_launcher() {
    local q_user q_display q_geometry q_pass q_port q_private_cidr
    q_user=$(shell_quote "$DESKTOP_USER")
    q_display=$(shell_quote ":$DISPLAY_NUMBER")
    q_geometry=$(shell_quote "$INITIAL_GEOMETRY")
    q_pass=$(shell_quote "$VNC_PASSFILE")
    q_port=$(shell_quote "$STREAM_PORT")
    q_private_cidr=$(shell_quote "$RFB_PRIVATE_CIDR")
    cat <<EOF
#!/usr/bin/env bash
# Canary v1: TigerVNC Xvnc owns the X display and serves raw RFB directly.
set -euo pipefail
export HOME=\$(getent passwd $q_user | cut -d: -f6)
export DISPLAY=$q_display
XVNC=\$(command -v Xtigervnc || command -v Xvnc || true)
[ -n "\$XVNC" ] || { echo 'TigerVNC Xvnc binary not found' >&2; exit 1; }
RFB_PRIVATE_CIDR=$q_private_cidr
RFB_BIND_ADDRESS=\$(python3 - "\$RFB_PRIVATE_CIDR" <<'PY'
import ipaddress, json, subprocess, sys
network = ipaddress.ip_network(sys.argv[1], strict=False)
interfaces = json.loads(subprocess.check_output(["ip", "-j", "-4", "addr", "show", "scope", "global"]))
for interface in interfaces:
    for address in interface.get("addr_info", []):
        candidate = ipaddress.ip_address(address["local"])
        if candidate in network:
            print(candidate)
            raise SystemExit(0)
raise SystemExit(f"no global IPv4 address belongs to required private CIDR {network}")
PY
)
[ -n "\$RFB_BIND_ADDRESS" ] || { echo 'private RFB bind address not found' >&2; exit 1; }
printf '%s\n' "\$RFB_BIND_ADDRESS" > /run/lsbx-desktop/rfb-bind-address
pids=()
cleanup() {
    local pid
    for pid in "\${pids[@]:-}"; do kill "\$pid" 2>/dev/null || true; done
}
trap cleanup EXIT INT TERM
"\$XVNC" "\$DISPLAY" -geometry $q_geometry -depth 24 -rfbport $q_port \\
    -rfbauth $q_pass -SecurityTypes VncAuth -localhost no \\
    -interface "\$RFB_BIND_ADDRESS" -AlwaysShared \\
    -desktop lsbx-tigervnc-raw-rfb-v1 & pids+=("\$!")
for _ in {1..50}; do
    DISPLAY="\$DISPLAY" xrandr --query >/dev/null 2>&1 && break
    sleep 0.1
done
DISPLAY="\$DISPLAY" xrandr --query >/dev/null
dbus-run-session -- startxfce4 & pids+=("\$!")
wait -n "\${pids[@]}"
EOF
}

render_launcher() {
    case "$1" in
        "$COMPAT_MODE") render_compat_launcher ;;
        "$CANARY_MODE") render_canary_launcher ;;
        *) die "unsupported launcher mode '$1'" ;;
    esac
}

render_unit() {
    cat <<EOF
[Unit]
Description=LSBX Linux desktop ($MODE)
After=network.target

[Service]
Type=simple
User=$DESKTOP_USER
Group=$DESKTOP_USER
Environment=HOME=/home/$DESKTOP_USER
RuntimeDirectory=lsbx-desktop
RuntimeDirectoryMode=0750
ExecStart=$LAUNCHER_PATH
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
}

render_healthcheck() {
    cat <<EOF
#!/usr/bin/env bash
set -euo pipefail
systemctl is-active --quiet lsbx-desktop.service
if [ '$MODE' = '$COMPAT_MODE' ]; then
    curl -fsS --retry 5 --retry-delay 1 --retry-connrefused \\
        http://127.0.0.1:$STREAM_PORT/vnc.html >/dev/null
else
    RFB_BIND_ADDRESS=\$(cat /run/lsbx-desktop/rfb-bind-address)
    python3 - "\$RFB_BIND_ADDRESS" '$STREAM_PORT' <<'PY'
import socket, sys
with socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=5) as sock:
    banner = sock.recv(12)
if not banner.startswith(b"RFB "):
    raise SystemExit(f"unexpected RFB banner: {banner!r}")
print(banner.decode("ascii", "replace").strip())
PY
    DISPLAY=:$DISPLAY_NUMBER xrandr --query
fi
EOF
}

resolve_distro() {
    if [ "$DISTRO" != auto ]; then
        case "$DISTRO" in ubuntu|debian|arch) printf '%s\n' "$DISTRO"; return ;; esac
        die "DISTRO must be auto, ubuntu, debian, or arch"
    fi
    [ -r /etc/os-release ] || die "DISTRO=auto requires /etc/os-release"
    # shellcheck disable=SC1091
    . /etc/os-release
    case "${ID:-}" in
        ubuntu) printf 'ubuntu\n' ;;
        debian) printf 'debian\n' ;;
        arch) printf 'arch\n' ;;
        *)
            case " ${ID_LIKE:-} " in
                *' debian '*) printf 'debian\n' ;;
                *' arch '*) printf 'arch\n' ;;
                *) die "unsupported distro ID '${ID:-unknown}'" ;;
            esac
            ;;
    esac
}

install_arch_compat_packages() {
    local packages=(xorg-server-xvfb x11vnc)
    local candidate
    # Arch has moved these Python/web assets between official/AUR packaging over
    # time. Use official packages if the configured repositories provide them;
    # never curl an unversioned installer or silently use AUR as root.
    for candidate in websockify python-websockify; do
        if pacman -Si "$candidate" >/dev/null 2>&1; then packages+=("$candidate"); break; fi
    done
    for candidate in novnc noVNC; do
        if pacman -Si "$candidate" >/dev/null 2>&1; then packages+=("$candidate"); break; fi
    done
    pacman -Syu --noconfirm --needed "${packages[@]}"
    command -v websockify >/dev/null || die "Arch repositories do not provide websockify; install a reviewed package before compat mode"
    [ -f "$NOVNC_DIR/vnc.html" ] || die "Arch repositories did not populate $NOVNC_DIR/vnc.html"
}

install_packages() {
    local distro=$1
    export DEBIAN_FRONTEND=noninteractive
    case "$distro" in
        ubuntu|debian)
            apt-get update -y
            apt-get install -y --no-install-recommends \
                ca-certificates curl dbus-x11 iproute2 openssh-server python3 \
                xfce4 xfce4-terminal x11-xserver-utils xauth
            if [ "$MODE" = "$COMPAT_MODE" ]; then
                apt-get install -y --no-install-recommends novnc websockify x11vnc xvfb
            else
                apt-get install -y --no-install-recommends \
                    tigervnc-standalone-server tigervnc-tools xfonts-base
            fi
            ;;
        arch)
            pacman -Syu --noconfirm --needed \
                ca-certificates curl dbus iproute2 openssh python python-pyxdg \
                xfce4 xfce4-terminal xorg-xauth xorg-xrandr
            if [ "$MODE" = "$COMPAT_MODE" ]; then
                install_arch_compat_packages
            else
                pacman -Syu --noconfirm --needed tigervnc
            fi
            ;;
    esac
}

ensure_user() {
    if ! id "$DESKTOP_USER" >/dev/null 2>&1; then
        useradd --create-home --shell /bin/bash "$DESKTOP_USER"
    fi
}

load_password_input() {
    local LC_ALL=C
    if [ -n "$VNC_PASSWORD_FILE" ] && [ "$VNC_PASSWORD_ENV_SET" -eq 1 ]; then
        die "provide only one of VNC_PASSWORD_FILE or VNC_PASSWORD"
    fi
    if [ -n "$VNC_PASSWORD_FILE" ]; then
        [[ "$VNC_PASSWORD_FILE" = /* && "$VNC_PASSWORD_FILE" != *$'\n'* ]] \
            || die "VNC_PASSWORD_FILE must be an absolute, single-line path"
        [ ! -L "$VNC_PASSWORD_FILE" ] || die "VNC_PASSWORD_FILE must not be a symlink"
        [ -f "$VNC_PASSWORD_FILE" ] || die "VNC_PASSWORD_FILE is not a regular file"
        [ "$(stat -c %u "$VNC_PASSWORD_FILE")" -eq 0 ] || die "VNC_PASSWORD_FILE must be owned by root"
        local file_mode
        file_mode=$(stat -c %a "$VNC_PASSWORD_FILE")
        (( (8#$file_mode & 077) == 0 )) || die "VNC_PASSWORD_FILE must not be group/world accessible"
        IFS= read -r VNC_PASSWORD_VALUE <"$VNC_PASSWORD_FILE" || [ -n "$VNC_PASSWORD_VALUE" ]
    fi

    if [ "$MODE" = "$CANARY_MODE" ]; then
        [ -n "$VNC_PASSWORD_VALUE" ] || die "canary requires explicit VNC_PASSWORD_FILE (recommended) or VNC_PASSWORD"
        # VNC authentication only uses eight bytes. Require exactly eight
        # printable ASCII characters so operators cannot mistake a longer input
        # for additional entropy that the protocol silently discards.
        [[ "$VNC_PASSWORD_VALUE" =~ ^[[:graph:]]{8}$ ]] \
            || die "canary VNC password must be exactly 8 printable ASCII characters"
        [ -n "$RFB_PRIVATE_CIDR" ] \
            || die "canary requires RFB_PRIVATE_CIDR for a non-wildcard private-interface bind"
        [[ "$RFB_PRIVATE_CIDR" =~ ^[0-9.]+/[0-9]{1,2}$ ]] \
            || die "RFB_PRIVATE_CIDR must be an IPv4 CIDR"
    elif [ -z "$VNC_PASSWORD_VALUE" ]; then
        VNC_PASSWORD_VALUE=lsbx
    fi
}

write_password_file() {
    local secret=$VNC_PASSWORD_VALUE
    VNC_PASSWORD_VALUE=''
    install -d -m 0755 "$(dirname "$VNC_PASSFILE")"
    rm -f "$VNC_PASSFILE"
    if [ "$MODE" = "$COMPAT_MODE" ]; then
        # Compatibility contract from #58. This legacy command necessarily puts
        # the compatibility password in x11vnc's argv for the encoder's lifetime.
        x11vnc -storepasswd "$secret" "$VNC_PASSFILE" >/dev/null
    else
        local passwd_bin
        passwd_bin=$(command -v tigervncpasswd || command -v vncpasswd || true)
        [ -n "$passwd_bin" ] || die "TigerVNC password encoder not found"
        # printf is a Bash builtin; the encoder receives the secret only on
        # stdin. VNC_PASSWORD was unset before any child process was launched,
        # so the canary secret is in neither argv nor the child environment.
        printf '%s\n' "$secret" | "$passwd_bin" -f >"$VNC_PASSFILE"
    fi
    secret=''
    chown "$DESKTOP_USER:$DESKTOP_USER" "$VNC_PASSFILE"
    chmod 0600 "$VNC_PASSFILE"
}

write_files() {
    install -d -m 0755 "$(dirname "$LAUNCHER_PATH")" "$(dirname "$CHECK_PATH")"
    render_launcher "$MODE" >"$LAUNCHER_PATH"
    render_unit >"$UNIT_PATH"
    render_healthcheck >"$CHECK_PATH"
    chmod 0755 "$LAUNCHER_PATH" "$CHECK_PATH"
    if [ "$MODE" = "$COMPAT_MODE" ]; then
        [ -f "$NOVNC_DIR/vnc.html" ] || die "noVNC entrypoint missing: $NOVNC_DIR/vnc.html"
        cat >"$NOVNC_DIR/index.html" <<'EOF'
<!doctype html>
<meta charset="utf-8">
<title>lsbx console compatibility bridge</title>
<script>location.replace('/vnc.html?resize=scale' + location.hash)</script>
<noscript><meta http-equiv="refresh" content="0; url=/vnc.html?resize=scale"></noscript>
EOF
    fi
}

static_check() {
    validate_config
    local tmp
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN
    render_launcher "$COMPAT_MODE" >"$tmp/compat.sh"
    render_launcher "$CANARY_MODE" >"$tmp/canary.sh"
    render_unit >"$tmp/unit"
    render_healthcheck >"$tmp/check.sh"
    bash -n "$0" "$tmp/compat.sh" "$tmp/canary.sh" "$tmp/check.sh"
    grep -q 'websockify --web' "$tmp/compat.sh"
    grep -q 'Xtigervnc' "$tmp/canary.sh"
    grep -q -- '-SecurityTypes VncAuth' "$tmp/canary.sh"
    grep -q -- "-rfbport $STREAM_PORT" "$tmp/canary.sh"
    grep -q "ExecStart=$LAUNCHER_PATH" "$tmp/unit"
    log "ok: syntax and both versioned launchers"
}

ACTION=provision
PRINT_MODE=''
while (( $# )); do
    case "$1" in
        --mode)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            MODE=$2; shift 2
            ;;
        --check) ACTION=check; shift ;;
        --print-launcher)
            ACTION=print
            if [ "$#" -ge 2 ] && [[ "$2" != --* ]]; then PRINT_MODE=$2; shift 2; else shift; fi
            ;;
        --emit)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            ACTION=emit; PRINT_MODE=$2; shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done

validate_config
case "$ACTION" in
    check) static_check; exit 0 ;;
    print) render_launcher "${PRINT_MODE:-$MODE}"; exit 0 ;;
    emit)
        validate_mode "$PRINT_MODE"
        printf '#!/usr/bin/env bash\nexport LSBX_LINUX_DESKTOP_MODE=%q\n' "$PRINT_MODE"
        tail -n +2 "$0"
        exit 0
        ;;
esac

[ "$(id -u)" -eq 0 ] || die "run as root inside the guest being baked"
load_password_input
resolved_distro=$(resolve_distro)
log "provisioning mode=$MODE distro=$resolved_distro user=$DESKTOP_USER port=$STREAM_PORT"
if [ "$MODE" = "$CANARY_MODE" ]; then
    log "CANARY ONLY: this script does not register it or change production defaults"
fi
install_packages "$resolved_distro"
ensure_user
write_password_file
write_files
systemctl daemon-reload
systemctl enable lsbx-desktop.service
systemctl restart lsbx-desktop.service
"$CHECK_PATH"
log "complete: $MODE"
