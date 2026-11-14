package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// VM is the subset of an lsbx sandbox record the scaler needs.
type VM struct {
	ID             string    `json:"id"`
	Name           string    `json:"name"`
	LeaseExpiresAt time.Time `json:"lease_expires_at"`
}

// Backend is how the scaler drives VMs. The production implementation shells
// out to the lsbx CLI, so the sidecar is "just another lsbx caller" exactly
// like the polling broker (no special access path, no second VM manager).
type Backend interface {
	Up(ctx context.Context, name string, lease time.Duration) (VM, error)
	Put(ctx context.Context, id, src, dst string) error
	Exec(ctx context.Context, id string, timeout time.Duration, script string) (string, error)
	Down(ctx context.Context, id string) error
	List(ctx context.Context) ([]VM, error)
}

type LsbxCLI struct {
	Bin     string
	Global  []string // --backend/--images/--state-dir
	Profile string
}

type envelope struct {
	Status  string          `json:"status"`
	Data    json.RawMessage `json:"data"`
	Code    int             `json:"code"`
	Message string          `json:"message"`
}

func (l *LsbxCLI) run(ctx context.Context, out any, args ...string) error {
	argv := append(append([]string{"--json", "--quiet"}, l.Global...), args...)
	cmd := exec.CommandContext(ctx, l.Bin, argv...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	runErr := cmd.Run()
	var env envelope
	if err := json.Unmarshal(lastJSONLine(stdout.Bytes()), &env); err != nil {
		return fmt.Errorf("lsbx %s: %v (stderr: %s)", args[0], firstErr(runErr, err), trim(stderr.String()))
	}
	if env.Status != "success" {
		return fmt.Errorf("lsbx %s: %s (code %d)", args[0], env.Message, env.Code)
	}
	if out != nil {
		if err := json.Unmarshal(env.Data, out); err != nil {
			return fmt.Errorf("lsbx %s: decode data: %w", args[0], err)
		}
	}
	// exec exits with the guest's exit code; that is reported via the DTO.
	if runErr != nil && args[0] != "exec" {
		return fmt.Errorf("lsbx %s: %v (stderr: %s)", args[0], runErr, trim(stderr.String()))
	}
	return nil
}

func (l *LsbxCLI) Up(ctx context.Context, name string, lease time.Duration) (VM, error) {
	var vm VM
	err := l.run(ctx, &vm, "up", l.Profile, "--name", name,
		"--lease", strconv.Itoa(int(lease.Seconds()))+"s")
	return vm, err
}

func (l *LsbxCLI) Put(ctx context.Context, id, src, dst string) error {
	return l.run(ctx, nil, "put", id, src, dst)
}

func (l *LsbxCLI) Exec(ctx context.Context, id string, timeout time.Duration, script string) (string, error) {
	var out struct {
		ExitCode int    `json:"exit_code"`
		Stdout   string `json:"stdout"`
		Stderr   string `json:"stderr"`
	}
	err := l.run(ctx, &out, "exec", id, "--timeout", strconv.Itoa(int(timeout.Seconds())), "--", "sh", "-c", script)
	if err != nil {
		return "", err
	}
	if out.ExitCode != 0 {
		return out.Stdout, fmt.Errorf("guest exit %d: %s", out.ExitCode, trim(out.Stderr))
	}
	return out.Stdout, nil
}

func (l *LsbxCLI) Down(ctx context.Context, id string) error {
	return l.run(ctx, nil, "down", id)
}

func (l *LsbxCLI) List(ctx context.Context) ([]VM, error) {
	var vms []VM
	err := l.run(ctx, &vms, "list")
	return vms, err
}

func lastJSONLine(b []byte) []byte {
	lines := bytes.Split(bytes.TrimSpace(b), []byte("\n"))
	for i := len(lines) - 1; i >= 0; i-- {
		if l := bytes.TrimSpace(lines[i]); len(l) > 0 && l[0] == '{' {
			return l
		}
	}
	return bytes.TrimSpace(b)
}

func firstErr(errs ...error) error {
	for _, e := range errs {
		if e != nil {
			return e
		}
	}
	return nil
}

func trim(s string) string {
	s = strings.TrimSpace(s)
	if len(s) > 400 {
		s = s[:400] + "…"
	}
	return s
}
