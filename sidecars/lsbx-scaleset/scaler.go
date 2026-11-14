package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/actions/scaleset"
	"github.com/actions/scaleset/listener"
)

// RunnerAPI is the part of *scaleset.Client the scaler uses (fakeable in tests).
type RunnerAPI interface {
	GenerateJitRunnerConfig(ctx context.Context, s *scaleset.RunnerScaleSetJitRunnerSetting, scaleSetID int) (*scaleset.RunnerScaleSetJitRunnerConfig, error)
	GetRunnerByName(ctx context.Context, name string) (*scaleset.RunnerReference, error)
	RemoveRunner(ctx context.Context, runnerID int64) error
}

// JobAcquirer is the part of the message session the scaler uses.
type JobAcquirer interface {
	AcquireJobs(ctx context.Context, requestIDs []int64) ([]int64, error)
}

const (
	stateBooting = "booting" // VM being created / runner being started
	stateIdle    = "idle"    // runner started, no job yet
	stateBusy    = "busy"    // JobStarted received
	stateDone    = "done"    // being torn down
)

// Runner is one VM + one ephemeral JIT runner, named identically.
type Runner struct {
	Name      string    `json:"name"`
	SandboxID string    `json:"sandbox_id,omitempty"`
	RunnerID  int       `json:"runner_id,omitempty"`
	State     string    `json:"state"`
	CreatedAt time.Time `json:"created_at"`
	ReadyAt   time.Time `json:"ready_at,omitempty"`
	Job       string    `json:"job,omitempty"`
}

type persisted struct {
	ScaleSetID int                `json:"scale_set_id"`
	Assigned   int                `json:"assigned"`
	Runners    map[string]*Runner `json:"runners"`
	SavedAt    time.Time          `json:"saved_at"`
}

type Scaler struct {
	cfg        Config
	log        *slog.Logger
	backend    Backend
	api        RunnerAPI
	acq        JobAcquirer
	sweeper    Sweeper // optional: libvirt orphans below lsbx's bookkeeping
	scaleSetID int
	now        func() time.Time
	gcEvery    time.Duration

	bg     context.Context // provisioning/teardown outlive one Scale() call
	cancel context.CancelFunc
	wg     sync.WaitGroup

	mu        sync.Mutex
	runners   map[string]*Runner
	assigned  int
	lastGC    time.Time
	gcRunning bool
	failures  int
	holdUntil time.Time // backoff after provisioning failures
}

func NewScaler(cfg Config, log *slog.Logger, b Backend, api RunnerAPI, acq JobAcquirer, scaleSetID int) *Scaler {
	bg, cancel := context.WithCancel(context.Background())
	return &Scaler{
		cfg: cfg, log: log, backend: b, api: api, acq: acq, scaleSetID: scaleSetID,
		now: time.Now, gcEvery: 30 * time.Second, bg: bg, cancel: cancel,
		runners: map[string]*Runner{},
	}
}

var _ listener.Scaler = (*Scaler)(nil)

// Scale handles one listener message. msg == nil is an empty long-poll and is
// used as the periodic tick. Only AcquireJobs errors are returned: returning
// an error makes the listener exit (systemd restarts it), which is right for
// a broken session and wrong for a VM that failed to boot.
func (s *Scaler) Scale(ctx context.Context, msg *scaleset.RunnerScaleSetMessage) error {
	if msg != nil {
		if len(msg.JobAvailableMessages) > 0 {
			ids := make([]int64, 0, len(msg.JobAvailableMessages))
			for _, j := range msg.JobAvailableMessages {
				ids = append(ids, j.RunnerRequestID)
			}
			if _, err := s.acq.AcquireJobs(ctx, ids); err != nil {
				return fmt.Errorf("acquire jobs: %w", err)
			}
			s.log.Info("acquired jobs", "count", len(ids))
		}
		for _, j := range msg.JobStartedMessages {
			s.jobStarted(j)
		}
		for _, j := range msg.JobCompletedMessages {
			s.jobCompleted(j)
		}
		if msg.Statistics != nil {
			s.mu.Lock()
			if s.assigned != msg.Statistics.TotalAssignedJobs {
				s.log.Info("assigned jobs changed", "from", s.assigned, "to", msg.Statistics.TotalAssignedJobs)
			}
			s.assigned = msg.Statistics.TotalAssignedJobs
			s.mu.Unlock()
		}
	}
	s.reconcile()
	s.maybeGC()
	s.save()
	return nil
}

func (s *Scaler) jobStarted(j *scaleset.JobStarted) {
	s.mu.Lock()
	defer s.mu.Unlock()
	attrs := []any{"runner", j.RunnerName, "repo", j.OwnerName + "/" + j.RepositoryName,
		"job", j.JobDisplayName, "run", j.WorkflowRunID}
	if !j.QueueTime.IsZero() && !j.RunnerAssignTime.IsZero() {
		attrs = append(attrs, "queued_to_runner", j.RunnerAssignTime.Sub(j.QueueTime).Round(time.Second).String())
	}
	r, ok := s.runners[j.RunnerName]
	if !ok {
		s.log.Warn("job started on unknown runner", attrs...)
		return
	}
	r.State = stateBusy
	r.Job = fmt.Sprintf("%s/%s#%d %s", j.OwnerName, j.RepositoryName, j.WorkflowRunID, j.JobDisplayName)
	s.log.Info("job started", attrs...)
}

func (s *Scaler) jobCompleted(j *scaleset.JobCompleted) {
	s.log.Info("job completed", "runner", j.RunnerName, "result", j.Result,
		"repo", j.OwnerName+"/"+j.RepositoryName, "job", j.JobDisplayName, "run", j.WorkflowRunID)
	if j.RunnerName == "" {
		return // cancelled before assignment: surplus runners are trimmed by reconcile
	}
	// The ephemeral runner deregisters itself after its one job.
	s.retire(j.RunnerName, "job completed: "+j.Result, false)
}

// target is how many runners should exist.
func (s *Scaler) target() int {
	return min(s.cfg.MaxRunners, s.cfg.MinRunners+s.assigned)
}

func (s *Scaler) reconcile() {
	s.mu.Lock()
	defer s.mu.Unlock()
	live := 0
	var idle []*Runner
	for _, r := range s.runners {
		if r.State == stateDone {
			continue
		}
		live++
		if r.State == stateIdle {
			idle = append(idle, r)
		}
	}
	target := s.target()
	switch {
	case target > live:
		if s.now().Before(s.holdUntil) {
			return
		}
		for range target - live {
			r := &Runner{Name: s.newName(), State: stateBooting, CreatedAt: s.now()}
			s.runners[r.Name] = r
			s.log.Info("scaling up", "runner", r.Name, "live", live, "target", target)
			s.wg.Add(1)
			go s.provision(r.Name)
		}
	case target < live:
		// Trim surplus idle runners (a job was cancelled before it reached
		// them). Only after a grace period, oldest first; RemoveRunner refuses
		// a runner that has just been handed a job, so this cannot kill work.
		sort.Slice(idle, func(i, j int) bool { return idle[i].ReadyAt.Before(idle[j].ReadyAt) })
		surplus := live - target
		for _, r := range idle {
			if surplus == 0 {
				break
			}
			if s.now().Sub(r.ReadyAt) < s.cfg.IdleTimeout {
				continue
			}
			surplus--
			s.retireLocked(r, "surplus idle runner", true)
		}
	}
}

func (s *Scaler) newName() string {
	b := make([]byte, 4)
	_, _ = rand.Read(b)
	return s.cfg.Name + "-" + hex.EncodeToString(b)
}

func (s *Scaler) prefix() string { return s.cfg.Name + "-" }

// provision boots the VM, mints a JIT config and starts the runner in the
// guest. The App private key never leaves the host: the guest receives only
// the single-use JIT config.
func (s *Scaler) provision(name string) {
	defer s.wg.Done()
	ctx, cancel := context.WithTimeout(s.bg, s.cfg.BootTimeout)
	defer cancel()
	start := s.now()
	err := s.provisionSteps(ctx, name)
	s.mu.Lock()
	r := s.runners[name]
	if err == nil && r != nil && r.State == stateBooting {
		r.State, r.ReadyAt = stateIdle, s.now()
		s.failures, s.holdUntil = 0, time.Time{}
		s.log.Info("runner started", "runner", name, "vm", r.SandboxID, "took", s.now().Sub(start).Round(time.Second).String())
		s.mu.Unlock()
		s.save()
		return
	}
	if err == nil {
		err = errors.New("runner retired while booting")
	}
	s.failures++
	backoff := min(time.Duration(1<<min(s.failures, 6))*5*time.Second, 5*time.Minute)
	s.holdUntil = s.now().Add(backoff)
	s.log.Error("provisioning failed", "runner", name, "error", err, "retry_in", backoff.String())
	if r != nil {
		s.retireLocked(r, "provisioning failed", true)
	}
	s.mu.Unlock()
}

func (s *Scaler) provisionSteps(ctx context.Context, name string) error {
	vm, err := s.backend.Up(ctx, name, s.cfg.Lease)
	if err != nil {
		return fmt.Errorf("lsbx up: %w", err)
	}
	if !s.update(name, func(r *Runner) { r.SandboxID = vm.ID }) {
		return errors.New("runner vanished")
	}
	s.save()
	jit, err := s.api.GenerateJitRunnerConfig(ctx, &scaleset.RunnerScaleSetJitRunnerSetting{Name: name, WorkFolder: "_work"}, s.scaleSetID)
	if err != nil {
		return fmt.Errorf("generate JIT config: %w", err)
	}
	if jit.Runner != nil {
		s.update(name, func(r *Runner) { r.RunnerID = jit.Runner.ID })
	}
	stage, err := s.stage(name, jit.EncodedJITConfig)
	if err != nil {
		return err
	}
	defer os.RemoveAll(stage)
	if _, err := s.backend.Exec(ctx, vm.ID, time.Minute, "sudo rm -rf /tmp/lsbx-ss"); err != nil {
		return fmt.Errorf("guest cleanup: %w", err)
	}
	if err := s.backend.Put(ctx, vm.ID, stage, "/tmp/lsbx-ss"); err != nil {
		return fmt.Errorf("lsbx put: %w", err)
	}
	out, err := s.backend.Exec(ctx, vm.ID, 4*time.Minute,
		"sudo RUNNER_USER="+shellQuote(s.cfg.RunnerUser)+" bash /tmp/lsbx-ss/run-jit-runner.sh /tmp/lsbx-ss")
	if err != nil {
		return fmt.Errorf("start runner: %w (out: %s)", err, trim(out))
	}
	return nil
}

// stage builds the upload directory: guest script, JIT config (0400) and,
// when configured, the host-cached runner tarball (hard-linked, not copied).
func (s *Scaler) stage(name, jit string) (string, error) {
	dir, err := os.MkdirTemp(filepath.Dir(s.cfg.StatePath), "stage-"+name+"-")
	if err != nil {
		return "", err
	}
	fail := func(err error) (string, error) { os.RemoveAll(dir); return "", err }
	if err := os.WriteFile(filepath.Join(dir, "jitconfig"), []byte(jit), 0o400); err != nil {
		return fail(err)
	}
	if err := copyFile(s.cfg.GuestScript, filepath.Join(dir, "run-jit-runner.sh"), 0o755); err != nil {
		return fail(fmt.Errorf("guest script: %w", err))
	}
	if s.cfg.RunnerTarball != "" {
		dst := filepath.Join(dir, "actions-runner.tar.gz")
		if err := os.Link(s.cfg.RunnerTarball, dst); err != nil {
			if err := copyFile(s.cfg.RunnerTarball, dst, 0o644); err != nil {
				return fail(fmt.Errorf("runner tarball: %w", err))
			}
		}
	}
	return dir, nil
}

func (s *Scaler) update(name string, f func(*Runner)) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	r, ok := s.runners[name]
	if ok {
		f(r)
	}
	return ok
}

func (s *Scaler) retire(name, reason string, removeRunner bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if r, ok := s.runners[name]; ok {
		s.retireLocked(r, reason, removeRunner)
	}
}

// retireLocked tears down a runner's VM (and, if asked, its runner
// registration). For surplus runners the registration is removed FIRST: if
// GitHub refuses (the runner was just handed a job) the VM is kept.
func (s *Scaler) retireLocked(r *Runner, reason string, removeRunner bool) {
	if r.State == stateDone {
		return
	}
	surplus := reason == "surplus idle runner"
	prev := r.State
	r.State = stateDone
	s.log.Info("retiring runner", "runner", r.Name, "vm", r.SandboxID, "reason", reason)
	s.wg.Add(1)
	go func(r Runner) {
		defer s.wg.Done()
		ctx, cancel := context.WithTimeout(context.WithoutCancel(s.bg), 3*time.Minute)
		defer cancel()
		if removeRunner {
			if err := s.removeRunner(ctx, r); err != nil {
				if surplus {
					s.log.Info("runner refused removal; keeping it", "runner", r.Name, "error", err)
					s.update(r.Name, func(x *Runner) { x.State = prev; x.ReadyAt = s.now() })
					return
				}
				s.log.Warn("remove runner failed", "runner", r.Name, "error", err)
			}
		}
		if r.SandboxID != "" {
			var err error
			for attempt := range 3 {
				if err = s.backend.Down(ctx, r.SandboxID); err == nil {
					break
				}
				time.Sleep(time.Duration(attempt+1) * 5 * time.Second)
			}
			if err != nil {
				s.log.Error("lsbx down failed; VM left for lease expiry", "runner", r.Name, "vm", r.SandboxID, "error", err)
			}
		}
		s.mu.Lock()
		delete(s.runners, r.Name)
		s.mu.Unlock()
		s.save()
		s.log.Info("runner retired", "runner", r.Name)
	}(*r)
}

func (s *Scaler) removeRunner(ctx context.Context, r Runner) error {
	id := int64(r.RunnerID)
	if id == 0 {
		ref, err := s.api.GetRunnerByName(ctx, r.Name)
		if err != nil {
			return err
		}
		if ref == nil {
			return nil
		}
		id = int64(ref.ID)
	}
	if err := s.api.RemoveRunner(ctx, id); err != nil {
		if strings.Contains(err.Error(), "404") || strings.Contains(strings.ToLower(err.Error()), "not found") {
			return nil
		}
		return err
	}
	return nil
}

func (s *Scaler) maybeGC() {
	s.mu.Lock()
	if s.gcRunning || s.now().Sub(s.lastGC) < s.gcEvery {
		s.mu.Unlock()
		return
	}
	s.gcRunning, s.lastGC = true, s.now()
	s.mu.Unlock()
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		s.gc()
		s.mu.Lock()
		s.gcRunning = false
		s.mu.Unlock()
	}()
}

// gc catches what messages miss: lease expiry, runners that finished while
// the listener was down (deregistered without a JobCompleted we saw), and
// runner processes that died in an idle VM.
func (s *Scaler) gc() {
	ctx, cancel := context.WithTimeout(s.bg, 2*time.Minute)
	defer cancel()
	s.mu.Lock()
	var check []Runner
	for _, r := range s.runners {
		if r.State == stateDone {
			continue
		}
		if s.now().Sub(r.CreatedAt) > s.cfg.Lease {
			s.retireLocked(r, "lease expired", true)
			continue
		}
		if r.State == stateIdle || r.State == stateBusy {
			check = append(check, *r)
		}
	}
	s.mu.Unlock()
	s.sweep(ctx)
	for _, r := range check {
		ref, err := s.api.GetRunnerByName(ctx, r.Name)
		if err != nil {
			s.log.Warn("gc: runner lookup failed", "runner", r.Name, "error", err)
			continue
		}
		if ref == nil {
			s.retire(r.Name, "runner deregistered (job finished)", false)
			continue
		}
		if r.State == stateIdle && s.now().Sub(r.ReadyAt) > 2*time.Minute {
			if _, err := s.backend.Exec(ctx, r.SandboxID, 30*time.Second, "pgrep -f Runner.Listener >/dev/null"); err != nil {
				s.retire(r.Name, "runner process not running", true)
			}
		}
	}
}

// sweep destroys domains with our prefix that no tracked runner owns.
func (s *Scaler) sweep(ctx context.Context) {
	if s.sweeper == nil {
		return
	}
	names, err := s.sweeper.Domains(ctx, s.prefix())
	if err != nil {
		s.log.Warn("sweep: list domains failed", "error", err)
		return
	}
	for _, n := range names {
		s.mu.Lock()
		_, tracked := s.runners[n]
		s.mu.Unlock()
		if tracked {
			continue
		}
		s.log.Warn("sweep: destroying orphan domain unknown to lsbx", "domain", n)
		if err := s.sweeper.Destroy(ctx, n); err != nil {
			s.log.Error("sweep: destroy failed", "domain", n, "error", err)
		}
		// Best effort: an orphan never ran a job, but it may hold a runner registration.
		if ref, err := s.api.GetRunnerByName(ctx, n); err == nil && ref != nil {
			_ = s.api.RemoveRunner(ctx, int64(ref.ID))
		}
	}
}

// Recover reconciles persisted state with the VMs that actually exist.
// Runners with a live VM are kept (a job may be running on them); anything
// half-made or unknown that carries our name prefix is destroyed.
func (s *Scaler) Recover(ctx context.Context) error {
	var p persisted
	if b, err := os.ReadFile(s.cfg.StatePath); err == nil {
		if err := json.Unmarshal(b, &p); err != nil {
			s.log.Warn("state file unreadable; starting empty", "error", err)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if p.ScaleSetID != 0 && p.ScaleSetID != s.scaleSetID {
		s.log.Warn("scale set id changed; previous runners are stale", "old", p.ScaleSetID, "new", s.scaleSetID)
	}
	vms, err := s.backend.List(ctx)
	if err != nil {
		return fmt.Errorf("list VMs: %w", err)
	}
	byName := map[string]VM{}
	for _, vm := range vms {
		if strings.HasPrefix(vm.Name, s.prefix()) {
			byName[vm.Name] = vm
		}
	}
	defer s.sweep(ctx)
	s.mu.Lock()
	defer s.mu.Unlock()
	for name, r := range p.Runners {
		vm, ok := byName[name]
		keep := ok && p.ScaleSetID == s.scaleSetID && (r.State == stateIdle || r.State == stateBusy)
		if keep {
			r.SandboxID = vm.ID
			if r.ReadyAt.IsZero() {
				r.ReadyAt = s.now()
			}
			s.runners[name] = r
			delete(byName, name)
			s.log.Info("recovered runner", "runner", name, "state", r.State, "vm", vm.ID)
			continue
		}
		if ok {
			r.SandboxID = vm.ID
			delete(byName, name)
		} else {
			r.SandboxID = ""
		}
		s.runners[name] = r
		r.State = stateIdle // let retireLocked act
		s.retireLocked(r, "stale after restart", true)
	}
	for name, vm := range byName {
		r := &Runner{Name: name, SandboxID: vm.ID, State: stateIdle, CreatedAt: s.now()}
		s.runners[name] = r
		s.retireLocked(r, "orphan VM", true)
	}
	return nil
}

func (s *Scaler) save() {
	s.mu.Lock()
	p := persisted{ScaleSetID: s.scaleSetID, Assigned: s.assigned, Runners: map[string]*Runner{}, SavedAt: s.now().UTC()}
	for k, v := range s.runners {
		c := *v
		p.Runners[k] = &c
	}
	s.mu.Unlock()
	b, _ := json.MarshalIndent(p, "", "  ")
	tmp := s.cfg.StatePath + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err == nil {
		err = os.Rename(tmp, s.cfg.StatePath)
		if err != nil {
			s.log.Warn("save state", "error", err)
		}
	}
}

// Shutdown stops new work and waits (bounded) for in-flight goroutines.
// It deliberately does NOT destroy busy runners: a restart must let running
// jobs finish (Recover picks them up again).
func (s *Scaler) Shutdown(timeout time.Duration) {
	done := make(chan struct{})
	go func() { s.wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(timeout):
		s.log.Warn("shutdown: in-flight work did not finish; cancelling")
		s.cancel()
		<-done
	}
	s.cancel()
	s.save()
}

func copyFile(src, dst string, mode os.FileMode) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, mode)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	return out.Close()
}

func shellQuote(s string) string { return "'" + strings.ReplaceAll(s, "'", `'\''`) + "'" }
