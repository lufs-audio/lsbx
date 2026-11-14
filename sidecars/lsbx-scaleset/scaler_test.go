package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/actions/scaleset"
)

type fakeBackend struct {
	mu       sync.Mutex
	vms      map[string]VM // id -> vm
	n        int
	upErr    error
	uploads  [][]string // file names per Put
	execs    []string
	downs    []string
	noRunner bool // pgrep fails
}

func newFakeBackend() *fakeBackend { return &fakeBackend{vms: map[string]VM{}} }

func (f *fakeBackend) Up(_ context.Context, name string, lease time.Duration) (VM, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.upErr != nil {
		return VM{}, f.upErr
	}
	f.n++
	vm := VM{ID: "sbx-" + name, Name: name, LeaseExpiresAt: time.Now().Add(lease)}
	f.vms[vm.ID] = vm
	return vm, nil
}

func (f *fakeBackend) Put(_ context.Context, id, src, dst string) error {
	entries, err := os.ReadDir(src)
	if err != nil {
		return err
	}
	var names []string
	for _, e := range entries {
		names = append(names, e.Name())
		b, _ := os.ReadFile(filepath.Join(src, e.Name()))
		if strings.Contains(string(b), "PRIVATE KEY") {
			names = append(names, "LEAKED-KEY:"+e.Name())
		}
	}
	f.mu.Lock()
	f.uploads = append(f.uploads, names)
	f.mu.Unlock()
	return nil
}

func (f *fakeBackend) Exec(_ context.Context, id string, _ time.Duration, script string) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.execs = append(f.execs, script)
	if strings.HasPrefix(script, "pgrep") && f.noRunner {
		return "", errors.New("guest exit 1")
	}
	return "ok", nil
}

func (f *fakeBackend) Down(_ context.Context, id string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.downs = append(f.downs, id)
	delete(f.vms, id)
	return nil
}

func (f *fakeBackend) List(context.Context) ([]VM, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []VM
	for _, v := range f.vms {
		out = append(out, v)
	}
	return out, nil
}

func (f *fakeBackend) live() int { f.mu.Lock(); defer f.mu.Unlock(); return len(f.vms) }

type fakeAPI struct {
	mu      sync.Mutex
	nextID  int
	runners map[string]int // registered runners
	refuse  map[string]bool
	removed []int64
}

func newFakeAPI() *fakeAPI { return &fakeAPI{runners: map[string]int{}, refuse: map[string]bool{}} }

func (a *fakeAPI) GenerateJitRunnerConfig(_ context.Context, s *scaleset.RunnerScaleSetJitRunnerSetting, _ int) (*scaleset.RunnerScaleSetJitRunnerConfig, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.nextID++
	a.runners[s.Name] = a.nextID
	return &scaleset.RunnerScaleSetJitRunnerConfig{Runner: &scaleset.RunnerReference{ID: a.nextID, Name: s.Name}, EncodedJITConfig: "base64-jit"}, nil
}

func (a *fakeAPI) GetRunnerByName(_ context.Context, name string) (*scaleset.RunnerReference, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if id, ok := a.runners[name]; ok {
		return &scaleset.RunnerReference{ID: id, Name: name}, nil
	}
	return nil, nil
}

func (a *fakeAPI) RemoveRunner(_ context.Context, id int64) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	for n, rid := range a.runners {
		if int64(rid) == id {
			if a.refuse[n] {
				return errors.New("400: runner is busy")
			}
			delete(a.runners, n)
		}
	}
	a.removed = append(a.removed, id)
	return nil
}

func (a *fakeAPI) deregister(name string) { a.mu.Lock(); delete(a.runners, name); a.mu.Unlock() }

type fakeAcq struct{ acquired []int64 }

func (f *fakeAcq) AcquireJobs(_ context.Context, ids []int64) ([]int64, error) {
	f.acquired = append(f.acquired, ids...)
	return ids, nil
}

type harness struct {
	s   *Scaler
	b   *fakeBackend
	api *fakeAPI
	acq *fakeAcq
	now time.Time
}

func newHarness(t *testing.T, max int) *harness {
	t.Helper()
	dir := t.TempDir()
	script := filepath.Join(dir, "run-jit-runner.sh")
	os.WriteFile(script, []byte("#!/bin/sh\n"), 0o755)
	cfg := Config{Name: "lsbx-test", MaxRunners: max, Lease: time.Hour, BootTimeout: time.Minute,
		IdleTimeout: 3 * time.Minute, GuestScript: script, StatePath: filepath.Join(dir, "state.json"), RunnerUser: "exedev"}
	h := &harness{b: newFakeBackend(), api: newFakeAPI(), acq: &fakeAcq{}, now: time.Date(2026, 11, 14, 20, 0, 0, 0, time.UTC)}
	h.s = NewScaler(cfg, slog.New(slog.NewTextHandler(io.Discard, nil)), h.b, h.api, h.acq, 42)
	h.s.now = func() time.Time { return h.now }
	return h
}

func (h *harness) scale(t *testing.T, msg *scaleset.RunnerScaleSetMessage) {
	t.Helper()
	if err := h.s.Scale(context.Background(), msg); err != nil {
		t.Fatal(err)
	}
	h.s.wg.Wait()
}

func stats(assigned int) *scaleset.RunnerScaleSetMessage {
	return &scaleset.RunnerScaleSetMessage{Statistics: &scaleset.RunnerScaleSetStatistic{TotalAssignedJobs: assigned}}
}

func (h *harness) states() map[string]string {
	h.s.mu.Lock()
	defer h.s.mu.Unlock()
	out := map[string]string{}
	for n, r := range h.s.runners {
		out[n] = r.State
	}
	return out
}

func TestScaleUpUploadsOnlyJITConfig(t *testing.T) {
	h := newHarness(t, 3)
	msg := stats(2)
	msg.JobAvailableMessages = []*scaleset.JobAvailable{{JobMessageBase: scaleset.JobMessageBase{RunnerRequestID: 7}}}
	h.scale(t, msg)
	if len(h.acq.acquired) != 1 || h.acq.acquired[0] != 7 {
		t.Fatalf("acquired = %v", h.acq.acquired)
	}
	if h.b.live() != 2 {
		t.Fatalf("live VMs = %d, want 2", h.b.live())
	}
	for _, st := range h.states() {
		if st != stateIdle {
			t.Fatalf("state %s, want idle", st)
		}
	}
	for _, u := range h.b.uploads {
		got := strings.Join(u, ",")
		if got != "jitconfig,run-jit-runner.sh" {
			t.Fatalf("upload = %s; want only jitconfig + script (no key, no env)", got)
		}
	}
}

func TestMaxRunnersCaps(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(5))
	if h.b.live() != 2 {
		t.Fatalf("live = %d, want cap 2", h.b.live())
	}
	h.scale(t, nil)
	if h.b.n != 2 {
		t.Fatalf("booted %d VMs, want 2 (no re-provisioning on tick)", h.b.n)
	}
}

func TestJobLifecycleTearsDownVM(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(1))
	var name string
	for n := range h.states() {
		name = n
	}
	h.scale(t, &scaleset.RunnerScaleSetMessage{JobStartedMessages: []*scaleset.JobStarted{{RunnerName: name}}})
	if h.states()[name] != stateBusy {
		t.Fatalf("want busy")
	}
	h.api.deregister(name) // ephemeral runner removes itself
	msg := stats(0)
	msg.JobCompletedMessages = []*scaleset.JobCompleted{{RunnerName: name, Result: "succeeded"}}
	h.scale(t, msg)
	if h.b.live() != 0 || len(h.states()) != 0 {
		t.Fatalf("VM not torn down: live=%d states=%v", h.b.live(), h.states())
	}
	if h.b.n != 1 {
		t.Fatalf("booted %d VMs, want exactly 1", h.b.n)
	}
}

func TestSurplusIdleTrimmedAfterGraceAndRefusalKeeps(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(2))
	h.scale(t, stats(0)) // both jobs cancelled
	if h.b.live() != 2 {
		t.Fatalf("trimmed before grace")
	}
	var names []string
	for n := range h.states() {
		names = append(names, n)
	}
	h.api.refuse[names[0]] = true // GitHub just handed this one a job
	h.now = h.now.Add(4 * time.Minute)
	h.scale(t, nil)
	if h.b.live() != 1 || h.states()[names[0]] != stateIdle {
		t.Fatalf("want refused runner kept, other trimmed: live=%d states=%v", h.b.live(), h.states())
	}
	if _, ok := h.api.runners[names[1]]; ok {
		t.Fatalf("trimmed runner still registered")
	}
}

func TestProvisionFailureCleansUpAndBacksOff(t *testing.T) {
	h := newHarness(t, 2)
	h.b.upErr = errors.New("libvirt down")
	h.scale(t, stats(1))
	if len(h.states()) != 0 {
		t.Fatalf("failed runner kept: %v", h.states())
	}
	h.b.upErr = nil
	h.scale(t, nil) // still inside backoff
	if h.b.live() != 0 {
		t.Fatalf("retried inside backoff")
	}
	h.now = h.now.Add(time.Minute)
	h.scale(t, nil)
	if h.b.live() != 1 {
		t.Fatalf("did not retry after backoff")
	}
}

func TestRecoverKeepsBusyAndReapsOrphans(t *testing.T) {
	h := newHarness(t, 2)
	busy := "lsbx-test-aaaa0001"
	h.b.vms["sbx-busy"] = VM{ID: "sbx-busy", Name: busy}
	h.b.vms["sbx-orphan"] = VM{ID: "sbx-orphan", Name: "lsbx-test-dead0002"}
	h.b.vms["sbx-other"] = VM{ID: "sbx-other", Name: "someone-elses-vm"}
	h.api.runners[busy] = 9
	p := persisted{ScaleSetID: 42, Assigned: 1, Runners: map[string]*Runner{
		busy:                 {Name: busy, SandboxID: "sbx-busy", State: stateBusy, CreatedAt: h.now},
		"lsbx-test-boot0003": {Name: "lsbx-test-boot0003", State: stateBooting, CreatedAt: h.now},
	}}
	b, _ := json.Marshal(p)
	os.WriteFile(h.s.cfg.StatePath, b, 0o600)
	if err := h.s.Recover(context.Background()); err != nil {
		t.Fatal(err)
	}
	h.s.wg.Wait()
	h.scale(t, stats(1)) // the running job is still assigned
	if _, ok := h.b.vms["sbx-busy"]; !ok {
		t.Fatalf("busy VM destroyed on restart")
	}
	if _, ok := h.b.vms["sbx-orphan"]; ok {
		t.Fatalf("orphan VM not reaped")
	}
	if _, ok := h.b.vms["sbx-other"]; !ok {
		t.Fatalf("foreign VM touched")
	}
	if h.b.n != 0 {
		t.Fatalf("restart created %d duplicate VMs", h.b.n)
	}
}

func TestGCRetiresRunnerThatFinishedUnseen(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(1))
	var name string
	for n := range h.states() {
		name = n
	}
	h.scale(t, &scaleset.RunnerScaleSetMessage{JobStartedMessages: []*scaleset.JobStarted{{RunnerName: name}}})
	h.api.deregister(name) // finished while we missed JobCompleted
	h.s.mu.Lock()
	h.s.assigned = 0
	h.s.mu.Unlock()
	h.now = h.now.Add(time.Minute)
	h.scale(t, nil)
	if h.b.live() != 0 {
		t.Fatalf("finished runner's VM not reaped")
	}
}

func TestGCReplacesDeadIdleRunner(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(1))
	h.b.noRunner = true
	h.now = h.now.Add(3 * time.Minute)
	h.scale(t, nil) // gc retires the dead one
	h.b.noRunner = false
	h.now = h.now.Add(time.Minute)
	h.scale(t, nil) // reconcile replaces it
	if h.b.n != 2 || h.b.live() != 1 {
		t.Fatalf("booted=%d live=%d, want replacement", h.b.n, h.b.live())
	}
}

func TestLoadConfigRequiresOrgURL(t *testing.T) {
	for k, v := range map[string]string{"LSBX_SS_URL": "https://github.com/lufs-audio/lsbx", "LSBX_SS_NAME": "x",
		"GITHUB_APP_ID": "1", "GITHUB_APP_KEY": "/k", "LSBX_SS_GUEST_SCRIPT": "/s", "LSBX_SS_STATE": "/st"} {
		t.Setenv(k, v)
	}
	if _, err := LoadConfig(); err == nil || !strings.Contains(err.Error(), "org URL") {
		t.Fatalf("repo URL accepted: %v", err)
	}
	t.Setenv("LSBX_SS_URL", "https://github.com/lufs-audio")
	t.Setenv("LSBX_SS_LABELS", "lsbx-default, lsbx-carnyx,lufs")
	c, err := LoadConfig()
	if err != nil || len(c.Labels) != 3 || c.MaxRunners != 2 {
		t.Fatalf("cfg=%+v err=%v", c, err)
	}
}

type fakeSweeper struct {
	domains   []string
	destroyed []string
}

func (f *fakeSweeper) Domains(_ context.Context, prefix string) ([]string, error) {
	var out []string
	for _, d := range f.domains {
		if strings.HasPrefix(d, prefix) {
			out = append(out, d)
		}
	}
	return out, nil
}

func (f *fakeSweeper) Destroy(_ context.Context, name string) error {
	f.destroyed = append(f.destroyed, name)
	return nil
}

func TestSweepDestroysOnlyUntrackedPrefixedDomains(t *testing.T) {
	h := newHarness(t, 2)
	h.scale(t, stats(1))
	var tracked string
	for n := range h.states() {
		tracked = n
	}
	sw := &fakeSweeper{domains: []string{tracked, "lsbx-test-0rphan01", "lsbx-linux-other", "win11-desktop"}}
	h.s.sweeper = sw
	h.now = h.now.Add(time.Minute)
	h.scale(t, nil)
	if strings.Join(sw.destroyed, ",") != "lsbx-test-0rphan01" {
		t.Fatalf("destroyed = %v, want only the untracked prefixed orphan", sw.destroyed)
	}
}
