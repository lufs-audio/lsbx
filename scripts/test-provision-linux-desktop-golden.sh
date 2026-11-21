#!/usr/bin/env bash
# Static regression tests for provision-linux-desktop-golden.sh.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROVISION="$ROOT/scripts/provision-linux-desktop-golden.sh"
CANARY="$ROOT/scripts/rfb-resize-canary.py"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() { printf 'not ok: %s\n' "$*" >&2; exit 1; }
pass() { printf 'ok: %s\n' "$*"; }

bash -n "$PROVISION" "$0"
python3 - "$CANARY" "$TMP/rfb-resize-canary.pyc" <<'PY'
import py_compile, sys
py_compile.compile(sys.argv[1], cfile=sys.argv[2], doraise=True)
PY
pass 'bash/python syntax'

"$PROVISION" --check
pass 'provisioner self-check'

"$PROVISION" --print-launcher compat-xvfb-websockify-v1 >"$TMP/compat"
"$PROVISION" --print-launcher tigervnc-raw-rfb-v1 >"$TMP/canary"
"$PROVISION" --emit tigervnc-raw-rfb-v1 >"$TMP/emitted-canary.sh"
bash -n "$TMP/compat" "$TMP/canary" "$TMP/emitted-canary.sh"
grep -q '^export LSBX_LINUX_DESKTOP_MODE=tigervnc-raw-rfb-v1$' "$TMP/emitted-canary.sh" \
    || fail 'emitted canary did not bake its versioned mode'
bash "$TMP/emitted-canary.sh" --check >/dev/null

grep -q 'Xvfb' "$TMP/compat" || fail 'compat launcher lost Xvfb'
grep -q 'x11vnc' "$TMP/compat" || fail 'compat launcher lost x11vnc'
grep -q 'websockify --web' "$TMP/compat" || fail 'compat launcher lost websockify'
pass 'compatibility mode remains the reconstructed static stack'

grep -q 'Xtigervnc' "$TMP/canary" || fail 'canary launcher lost TigerVNC'
grep -q -- '-rfbport 8000' "$TMP/canary" || fail 'canary does not expose raw RFB on 8000'
grep -q -- '-SecurityTypes VncAuth' "$TMP/canary" || fail 'canary lost VNC authentication'
grep -q -- '-localhost no' "$TMP/canary" || fail 'canary is unreachable by the host relay'
# Generated launcher must retain this runtime variable.
# shellcheck disable=SC2016
grep -q -- '-interface "$RFB_BIND_ADDRESS"' "$TMP/canary" \
    || fail 'canary lost its non-wildcard private-interface bind'
grep -q 'RFB_PRIVATE_CIDR' "$TMP/canary" || fail 'canary lost private CIDR selection'
if grep -q 'websockify\|Xvfb\|x11vnc' "$TMP/canary"; then
    fail 'canary accidentally contains the compatibility bridge'
fi
pass 'canary is raw TigerVNC RFB and isolated from compatibility mode'

if grep -E '(echo|printf).*VNC_PASSWORD.*>' "$PROVISION" | grep -vq 'passwd_bin'; then
    fail 'provisioner appears to write plaintext VNC_PASSWORD without an encoder'
fi
grep -q 'x11vnc -storepasswd' "$PROVISION" || fail 'compat password is not encoded by x11vnc'
grep -q 'tigervncpasswd.*vncpasswd' "$PROVISION" || fail 'canary password encoder lookup missing'
grep -q '^unset VNC_PASSWORD$' "$PROVISION" || fail 'canary secret remains exported to children'
if grep -E '(tigervncpasswd|passwd_bin).*(VNC_PASSWORD|secret)' "$PROVISION" | grep -vq 'printf'; then
    fail 'canary password appears in an encoder argv'
fi
pass 'password files use VNC encoders; canary secret is stdin-only'

if env -u LSBX_VNC_PASSWORD python3 "$CANARY" --seconds 0.1 >"$TMP/probe-no-password" 2>&1; then
    fail 'RFB canary unexpectedly accepted a missing password'
fi
grep -q 'provide a VNC password' "$TMP/probe-no-password" \
    || fail 'RFB canary missing-password error is unclear'
pass 'RFB canary has no password default'

if [ "$(id -u)" -ne 0 ]; then
    if "$PROVISION" --mode tigervnc-raw-rfb-v1 >"$TMP/nonroot.out" 2>&1; then
        fail 'actual provisioning unexpectedly succeeded without root'
    fi
    grep -q 'run as root' "$TMP/nonroot.out" || fail 'non-root failure is not explicit'
    pass 'actual provisioning refuses non-root'
else
    printf 'ok: non-root refusal (skipped while test itself is root)\n'
fi

# No manifest should opt into the canary. It remains a per-bake/per-console test.
if grep -q 'tigervnc-raw-rfb-v1' "$ROOT/images.json" "$ROOT/images.carnyx.json"; then
    fail 'a production manifest unexpectedly opts into the canary'
fi
pass 'no production manifest/default opt-in'
