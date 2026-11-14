package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// Sweeper finds and destroys VMs that exist below lsbx's own bookkeeping.
//
// Measured 2026-11-14: killing `lsbx up` mid-boot (listener SIGKILL, or the
// boot timeout cancelling the CLI) leaves a running libvirt domain plus its
// qcow2/cidata files that `lsbx list` does not know about, so `lsbx down`
// cannot reach it. Until lsbx records a sandbox before defining its domain,
// the scaler sweeps domains that carry its own name prefix and that it is not
// tracking. It never touches a name outside its prefix.
type Sweeper interface {
	Domains(ctx context.Context, prefix string) ([]string, error)
	Destroy(ctx context.Context, name string) error
}

type VirshSweeper struct {
	URI   string // e.g. qemu:///system
	VMDir string // LSBX_LIBVIRT_VM_DIR: <name>.qcow2, <name>-cidata.iso
}

func (v *VirshSweeper) virsh(ctx context.Context, args ...string) (string, error) {
	out, err := exec.CommandContext(ctx, "virsh", append([]string{"-c", v.URI}, args...)...).CombinedOutput()
	return string(out), err
}

func (v *VirshSweeper) Domains(ctx context.Context, prefix string) ([]string, error) {
	out, err := v.virsh(ctx, "list", "--all", "--name")
	if err != nil {
		return nil, fmt.Errorf("virsh list: %v: %s", err, trim(out))
	}
	var names []string
	for _, n := range strings.Fields(out) {
		if strings.HasPrefix(n, prefix) {
			names = append(names, n)
		}
	}
	return names, nil
}

func (v *VirshSweeper) Destroy(ctx context.Context, name string) error {
	_, _ = v.virsh(ctx, "destroy", name) // fails harmlessly if already shut off
	// lsbx defines transient domains: destroy alone removes them, and
	// undefine then fails with "not found". Only a domain that still exists
	// after destroy needs undefining.
	if _, err := v.virsh(ctx, "dominfo", name); err == nil {
		if out, err := v.virsh(ctx, "undefine", name, "--nvram"); err != nil {
			if out2, err2 := v.virsh(ctx, "undefine", name); err2 != nil {
				return fmt.Errorf("virsh undefine %s: %s / %s", name, trim(out), trim(out2))
			}
		}
	}
	if v.VMDir != "" {
		for _, pat := range []string{name + ".*", name + "-*"} {
			matches, _ := filepath.Glob(filepath.Join(v.VMDir, pat))
			for _, m := range matches {
				if err := os.Remove(m); err != nil {
					return err
				}
			}
		}
	}
	return nil
}
