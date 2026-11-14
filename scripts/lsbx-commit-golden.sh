#!/usr/bin/env bash
# lsbx-commit-golden: Merges an ephemeral sandbox's overlay disk into its base golden image.
# Acts as a "save point" for interactive VM customization sessions.
#
# Usage:
#   ./scripts/lsbx-commit-golden.sh <sandbox-id> [golden-key]
#
# Example:
#   ./scripts/lsbx-commit-golden.sh sbx-18e7c5c98a41ddbb-7da4d00c win11-desktop

set -euo pipefail

SANDBOX_ID="${1:-}"
if [ -z "$SANDBOX_ID" ]; then
    echo "Usage: $0 <sandbox-id> [golden-key]" >&2
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

# 1. Gracefully shut down domain if running
if virsh -c qemu:///system domstate "$SANDBOX_ID" >/dev/null 2>&1; then
    STATE=$(virsh -c qemu:///system domstate "$SANDBOX_ID" 2>/dev/null || echo "shut off")
    if [ "$STATE" = "running" ]; then
        echo "--> Gracefully shutting down domain $SANDBOX_ID..."
        virsh -c qemu:///system shutdown "$SANDBOX_ID" >/dev/null 2>&1 || true
        for i in {1..30}; do
            if ! virsh -c qemu:///system domstate "$SANDBOX_ID" >/dev/null 2>&1; then
                break
            fi
            sleep 1
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
