#!/usr/bin/env bash
# lsbx-commit-golden: merges an ephemeral sandbox's overlay disk into its
# base golden image — the "save point" for an interactive customization session.
#
# Usage:
#   ./scripts/lsbx-commit-golden.sh [--force] <sandbox-id> [golden-key]
#
#   --force   hard power-off the guest if it has not honoured an ACPI shutdown
#             within LSBX_COMMIT_SHUTDOWN_WAIT (default 180 s) and flatten anyway.
#             Without it, the script refuses to convert a live disk.
#
# Env overrides: LSBX_STATE_DIR, LSBX_LIBVIRT_IMAGES_DIR, LSBX_LIBVIRT_VM_DIR,
#                LSBX_COMMIT_SHUTDOWN_WAIT
#
# What it does, in order:
#   1. ACPI-shutdown the domain and WAIT for it to disappear (aborts if it won't)
#   2. qemu-img convert  overlay -> <golden>.new   (deliberately UNCOMPRESSED)
#   3. qemu-img resize   <golden>.new -> the previous golden's virtual size
#   4. atomically swap   <golden>.new -> <golden>.qcow2
#   5. remove the sandbox's overlay, seed ISO, and state record
#
# Usage recommendations:
#   - Run it from a shell on the hypervisor host, not through a pipe: the
#     conversion can take minutes for a 50 GiB+ overlay.
#   - The sandbox is DESTROYED by this script. Anything not installed inside the
#     guest is lost, so finish the session before committing.
#   - Never compress the output (no `-c`): a golden is a live COW backing file,
#     so compression costs an inflate on every guest read. See issue #54.
#   - Resizing the qcow2 does NOT grow the guest partition. If you bumped the
#     virtual size, extend the filesystem inside the guest too:
#       reagentc /disable; diskpart delete partition override (the inert Recovery
#       partition between C: and the free space); Resize-Partition; reagentc /enable
#   - Verify afterwards:  lsbx golden verify <golden-key>   and   lsbx up <profile>
#
# Example:
#   ./scripts/lsbx-commit-golden.sh sbx-18e7cb8ac0ca9b71-ab2474e9 win11-desktop

set -euo pipefail

FORCE=0
if [ "${1:-}" = "--force" ]; then
    FORCE=1
    shift
fi

SANDBOX_ID="${1:-}"
if [ -z "$SANDBOX_ID" ]; then
    echo "Usage: $0 [--force] <sandbox-id> [golden-key]" >&2
    exit 1
fi

GOLDEN_KEY="${2:-win11-desktop}"
STATE_DIR="${LSBX_STATE_DIR:-/home/carnyx/lsbx-state}"
GOLDEN_DIR="${LSBX_LIBVIRT_IMAGES_DIR:-/home/carnyx/ISOs/images/goldens}"
WORK_DIR="${LSBX_LIBVIRT_VM_DIR:-/home/carnyx/ISOs/images/work}"

OVERLAY_DISK="$WORK_DIR/$SANDBOX_ID.qcow2"
GOLDEN_DISK="$GOLDEN_DIR/$GOLDEN_KEY.qcow2"
NEW_GOLDEN="$GOLDEN_DIR/$GOLDEN_KEY.qcow2.new"

if [ ! -f "$OVERLAY_DISK" ]; then
    echo "Error: Sandbox overlay disk not found: $OVERLAY_DISK" >&2
    exit 1
fi

echo "=== lsbx Golden Commit ==="
echo "Sandbox ID:   $SANDBOX_ID"
echo "Overlay Disk: $OVERLAY_DISK"
echo "Target Golden: $GOLDEN_DISK"

# 1. Gracefully shut down the domain, and REFUSE to convert a live disk.
#
# Converting while the guest still has the overlay open produces a torn image
# (the qcow2 is not quiesced), so a failed shutdown must abort the whole commit
# rather than fall through to `qemu-img convert`. Windows in particular can take
# minutes to honour an ACPI shutdown when an installer or update is mid-flight;
# `--force` is the escape hatch (hard power-off, then convert).
SHUTDOWN_WAIT="${LSBX_COMMIT_SHUTDOWN_WAIT:-180}"
if virsh -c qemu:///system domstate "$SANDBOX_ID" >/dev/null 2>&1; then
    STATE=$(virsh -c qemu:///system domstate "$SANDBOX_ID" 2>/dev/null || echo "shut off")
    if [ "$STATE" = "running" ]; then
        echo "--> Gracefully shutting down domain $SANDBOX_ID (waiting up to ${SHUTDOWN_WAIT}s)..."
        virsh -c qemu:///system shutdown "$SANDBOX_ID" >/dev/null 2>&1 || true
        waited=0
        while virsh -c qemu:///system domstate "$SANDBOX_ID" >/dev/null 2>&1; do
            if [ "$waited" -ge "$SHUTDOWN_WAIT" ]; then
                if [ "${FORCE:-0}" = "1" ]; then
                    echo "--> still running after ${SHUTDOWN_WAIT}s; --force: hard power-off" >&2
                    virsh -c qemu:///system destroy "$SANDBOX_ID" >/dev/null 2>&1 || true
                else
                    echo "Error: domain $SANDBOX_ID still running after ${SHUTDOWN_WAIT}s." >&2
                    echo "       Refusing to flatten a live disk (it would be torn)." >&2
                    echo "       Let the guest finish shutting down and re-run, or pass --force." >&2
                    exit 1
                fi
                break
            fi
            sleep 2
            waited=$((waited + 2))
        done
    fi
fi

# 2. Flatten overlay into a standalone golden image.
#
# Deliberately NOT `-c` (compressed). A golden is a *runtime backing file*: every
# sandbox is a COW overlay on it, so a compressed backing cluster costs a zlib
# inflate on every read, taken in QEMU's single main loop. That is a constant
# tax on exactly the workloads these VMs exist for (installers, asset imports,
# builds). Compression belongs in a transfer/export step, never in a live
# backing image. Rebuilding this way also un-compresses an already-compressed
# golden. See lsbx issue #54.
echo "--> Flattening overlay into new golden (uncompressed - see #54)..."
qemu-img convert -O qcow2 -p "$OVERLAY_DISK" "$NEW_GOLDEN"

# 3. Preserve virtual disk size
echo "--> Aligning virtual disk size..."
VIRT_SIZE=$(qemu-img info "$GOLDEN_DISK" | grep "virtual size:" | awk '{print $3$4}')
if [ -n "$VIRT_SIZE" ]; then
    qemu-img resize "$NEW_GOLDEN" "$VIRT_SIZE" >/dev/null 2>&1 || true
fi

# 4. Atomic swap
echo "--> Replacing golden image..."
rm -f "$GOLDEN_DISK"
mv "$NEW_GOLDEN" "$GOLDEN_DISK"

# 5. Cleanup sandbox overlay and state record
echo "--> Cleaning up overlay artifacts..."
rm -f "$OVERLAY_DISK"
rm -f "$WORK_DIR/$SANDBOX_ID-cidata.iso"
rm -f "$STATE_DIR/state/$SANDBOX_ID.json"
rm -f "/home/carnyx/ISOs/images/state/state/$SANDBOX_ID.json"

echo "=== Successfully saved sandbox $SANDBOX_ID to golden '$GOLDEN_KEY' ==="
