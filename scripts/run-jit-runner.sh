#!/usr/bin/env bash
# run-jit-runner.sh STAGE_DIR
#
# Runs INSIDE a disposable lsbx CI VM, as root, started by lsbx-scaleset
# (sidecars/lsbx-scaleset). STAGE_DIR contains:
#   jitconfig               single-use JIT runner config (base64), mode 0400
#   actions-runner.tar.gz   optional host-cached runner, used only when the
#                           golden does not already ship $RUNNER_DIR
#
# No GitHub App key or registration token ever reaches the guest: the JIT
# config registers exactly one ephemeral runner for one job and is useless
# afterwards. The file is deleted as soon as it is read.
set -euo pipefail

STAGE="${1:?usage: run-jit-runner.sh STAGE_DIR}"
RUNNER_DIR="${RUNNER_DIR:-/opt/actions-runner}"
RUNNER_USER="${RUNNER_USER:-exedev}"
RUNNER_LOG="${RUNNER_LOG:-/var/log/lsbx-runner.log}"
MIN_JIT_VERSION="2.311.0"
WAIT_SECONDS="${WAIT_SECONDS:-120}"

die() { echo "run-jit-runner: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || die "must run as root"
id "$RUNNER_USER" >/dev/null 2>&1 || die "runner user does not exist: $RUNNER_USER"
[ -s "$STAGE/jitconfig" ] || die "no JIT config in $STAGE"

if [ ! -x "$RUNNER_DIR/run.sh" ]; then
  [ -f "$STAGE/actions-runner.tar.gz" ] || die "golden has no $RUNNER_DIR and no runner tarball was staged"
  mkdir -p "$RUNNER_DIR"
  tar -xzf "$STAGE/actions-runner.tar.gz" -C "$RUNNER_DIR"
  # Ubuntu goldens normally already carry the runner's .NET dependencies;
  # only fall back to the (slow, apt-based) installer when libicu is missing.
  if ! ldconfig -p | grep -q libicu; then
    "$RUNNER_DIR/bin/installdependencies.sh"
  fi
fi
rm -f "$STAGE/actions-runner.tar.gz"

version="$("$RUNNER_DIR/bin/Runner.Listener" --version 2>/dev/null | tr -d '[:space:]')"
[ -n "$version" ] || die "cannot determine runner version in $RUNNER_DIR"
if [ "$(printf '%s\n%s\n' "$MIN_JIT_VERSION" "$version" | sort -V | head -1)" != "$MIN_JIT_VERSION" ]; then
  die "runner $version is older than $MIN_JIT_VERSION and cannot use --jitconfig"
fi
chown -R "$RUNNER_USER:$RUNNER_USER" "$RUNNER_DIR"

ACTIONS_RUNNER_INPUT_JITCONFIG="$(cat "$STAGE/jitconfig")"
export ACTIONS_RUNNER_INPUT_JITCONFIG
rm -f "$STAGE/jitconfig"

: >"$RUNNER_LOG"
chown "$RUNNER_USER:$RUNNER_USER" "$RUNNER_LOG"
# runuser keeps the exported environment (HOME/USER/SHELL are reset).
setsid -f /usr/sbin/runuser -u "$RUNNER_USER" -- "$RUNNER_DIR/run.sh" </dev/null >>"$RUNNER_LOG" 2>&1
unset ACTIONS_RUNNER_INPUT_JITCONFIG

for _ in $(seq "$WAIT_SECONDS"); do
  if grep -q "Listening for Jobs" "$RUNNER_LOG"; then
    echo "runner $version listening"
    exit 0
  fi
  if grep -qiE "^.*(error|failed|exited with)" "$RUNNER_LOG" && ! pgrep -f Runner.Listener >/dev/null; then
    tail -n 30 "$RUNNER_LOG" >&2
    die "runner exited before listening"
  fi
  sleep 1
done
tail -n 30 "$RUNNER_LOG" >&2
die "runner did not reach 'Listening for Jobs' within ${WAIT_SECONDS}s"
