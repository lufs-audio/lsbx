#!/usr/bin/env bash
# Provision a GitHub-App-authenticated ephemeral CI runner inside a VM (Unit 08).
#
# Ports the token-minting pattern from lufs-runner: RS256 JWT (app id + private
# key) -> installation token -> short-lived registration token, then configure a
# runner that registers with a fresh token and exits after one job (ephemeral).
#
# Run INSIDE the ci-runner golden while preparing it, or on a live sandbox via
# `lufs-sandbox exec`. Config comes from /etc/lsbx-runner.env or env:
#   GITHUB_APP_ID, GITHUB_APP_KEY (path to .pem), GITHUB_SCOPE=org|repo,
#   GITHUB_OWNER (org or owner), GITHUB_REPO (repo, scope=repo),
#   RUNNER_LABELS (or legacy LABELS), RUNNER_GROUP (org scope only)
#
# The runner is disposable like lufs-runner: one job per registration token,
# then the sandbox is destroyed (lease/reap). Never store a long-lived PAT.

set -euo pipefail
umask 077

CONF="${LUFSS_RUNNER_ENV:-/etc/lsbx-runner.env}"
if [ -f "$CONF" ]; then
  # shellcheck disable=SC1090
  source "$CONF"
fi

: "${GITHUB_APP_ID:?set GITHUB_APP_ID}"
: "${GITHUB_APP_KEY:?set GITHUB_APP_KEY (path to .pem)}"
[ -f "$GITHUB_APP_KEY" ] || { echo "GitHub App key not found: $GITHUB_APP_KEY" >&2; exit 1; }
[ "$(stat -c '%a' "$GITHUB_APP_KEY" 2>/dev/null || stat -f '%Lp' "$GITHUB_APP_KEY")" = "600" ] || \
  echo "warning: GitHub App key should have mode 600" >&2
: "${GITHUB_SCOPE:=org}"
case "$GITHUB_SCOPE" in
  org|repo) ;;
  *) echo "GITHUB_SCOPE must be org or repo" >&2; exit 1 ;;
esac
: "${GITHUB_OWNER:?set GITHUB_OWNER or GITHUB_REPO}"
GITHUB_REPO="${GITHUB_REPO:-}"
RUNNER_LABELS="${RUNNER_LABELS:-${LABELS:-exe,lufs}}"
RUNNER_GROUP="${RUNNER_GROUP:-}"
if [[ -n "$RUNNER_GROUP" && "$GITHUB_SCOPE" != "org" ]]; then
  echo "RUNNER_GROUP requires GITHUB_SCOPE=org" >&2
  exit 1
fi
if [[ -n "${RUNNER_HOST_PREFIX:-}" ]]; then
  runner_host="$RUNNER_HOST_PREFIX"
else
  runner_host=$(hostname -s 2>/dev/null || echo runner)
  runner_host=${runner_host#lsbx-}
fi
RUNNER_NAME="lsbx-${runner_host:-runner}-$(date +%s)-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"

urlencode() { python3 -c "import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1],safe=''))" "$1"; }

# -- GitHub App auth (RS256 JWT -> installation token) -------------------------
now=$(date +%s)
iat=$(( now - 60 ))
exp=$(( now + 540 ))
header=$(printf '{"alg":"RS256","typ":"JWT"}' | base64 | tr -d '=\n' | tr '/+' '_-')
payload=$(printf '{"iss":"%s","iat":%s,"exp":%s}' "$GITHUB_APP_ID" "$iat" "$exp" \
  | base64 | tr -d '=\n' | tr '/+' '_-')
sig=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -sign "$GITHUB_APP_KEY" \
  | base64 | tr -d '=\n' | tr '/+' '_-')
JWT="${header}.${payload}.${sig}"

if [ "$GITHUB_SCOPE" = "org" ]; then
  INST_URL="https://api.github.com/orgs/$(urlencode "$GITHUB_OWNER")/installation"
else
  INST_URL="https://api.github.com/repos/$(urlencode "$GITHUB_OWNER")/installation"
fi

INST_ID=$(curl -fsSL -H "Authorization: Bearer $JWT" -H "Accept: application/vnd.github+json" \
  "$INST_URL" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')

TOKEN_URL="https://api.github.com/app/installations/$INST_ID/access_tokens"
if [ "$GITHUB_SCOPE" = "org" ]; then
  # GitHub App permission: Organization -> Self-hosted runners: Read and write.
  TOKEN_PERMISSIONS='{"organization_self_hosted_runners":"write"}'
else
  # GitHub App permission: Repository -> Administration: Read and write.
  TOKEN_PERMISSIONS='{"administration":"write"}'
fi
TOKENS=$(curl -fsSL -X POST -H "Authorization: Bearer $JWT" -H "Accept: application/vnd.github+json" \
  -d "{\"permissions\":$TOKEN_PERMISSIONS}" "$TOKEN_URL")
TOKEN=$(echo "$TOKENS" | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')

# Registration token: org or repo scoped, then the runner `config.sh` URL.
if [ "$GITHUB_SCOPE" = "org" ]; then
  RUNNER_URL="https://github.com/$GITHUB_OWNER"
  REGTOK=$(curl -fsSL -X POST -H "Authorization: Bearer $TOKEN" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/orgs/$(urlencode "$GITHUB_OWNER")/actions/runners/registration-token" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
else
  : "${GITHUB_REPO:?GITHUB_REPO required for scope=repo}"
  RUNNER_URL="https://github.com/$GITHUB_OWNER/$GITHUB_REPO"
  REGTOK=$(curl -fsSL -X POST -H "Authorization: Bearer $TOKEN" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$(urlencode "$GITHUB_OWNER")/$(urlencode "$GITHUB_REPO")/actions/runners/registration-token" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
fi

# Runner binary (x64) + configure as ephemeral -----------------------------------
RUNNER_VERSION="${RUNNER_VERSION:-2.336.0}"
RUNNER_SHA256="${RUNNER_SHA256:-04cf0be1aff4c3ec3554466c39124ca250e3effd8873bb7e8d68535aa9505d5d}"
RUNNER_DIR="${RUNNER_DIR:-/opt/actions-runner}"
RUNNER_USER="${RUNNER_USER:-${SUDO_USER:-exedev}}"

id "$RUNNER_USER" >/dev/null 2>&1 || {
  echo "runner user does not exist: $RUNNER_USER" >&2
  exit 1
}
if [ ! -x "$RUNNER_DIR/run.sh" ]; then
  # CI jobs use python3 -m venv; the dependency is part of the golden
    # contract and must not be silently skipped during provisioning.
    if command -v apt-get >/dev/null 2>&1; then
      apt-get update -qq
      apt-get install -y -qq python3-venv >/dev/null
    fi
  mkdir -p "$RUNNER_DIR" && cd "$RUNNER_DIR"
  archive="actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"
  curl -fsSL -o "$archive" \
    "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/${archive}"
  echo "${RUNNER_SHA256}  ${archive}" | sha256sum -c -
  tar -xzf "$archive" && rm "$archive"
  chown -R "$RUNNER_USER:$RUNNER_USER" "$RUNNER_DIR"
  # The dependency installer requires root (it exits with "Need to run with
  # sudo privilege" for any non-root uid). This script is run through sudo, so
  # invoke it directly as root.
  "$RUNNER_DIR/bin/installdependencies.sh"
fi

chown -R "$RUNNER_USER:$RUNNER_USER" "$RUNNER_DIR"
CONFIG_ARGS=(
  --url "$RUNNER_URL"
  --token "$REGTOK"
  --name "$RUNNER_NAME"
  --labels "$RUNNER_LABELS"
  --ephemeral
  --unattended
)
if [[ -n "$RUNNER_GROUP" ]]; then
  CONFIG_ARGS+=(--runnergroup "$RUNNER_GROUP")
fi
runuser -u "$RUNNER_USER" -- "$RUNNER_DIR/config.sh" "${CONFIG_ARGS[@]}"

echo "Runner registered: $RUNNER_NAME (ephemeral). Run it with:"
echo "  $RUNNER_DIR/run.sh"
echo "The VM is disposable: destroy it via lease/reap after the job."