// lsbx-scaleset is the push-dispatch front end of the lsbx CI broker: it
// registers a GitHub Actions runner scale set, long-polls GitHub's job queue
// for it (outbound HTTPS only, no repo list, no REST polling) and boots one
// disposable lsbx VM per assigned job with a single-use JIT runner config.
//
// See docs/specs/2026-11-14T1900Z_ci-broker-push-dispatch/ and README.md.
package main

import (
	"context"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"slices"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/actions/scaleset"
	"github.com/actions/scaleset/listener"
	"github.com/golang-jwt/jwt/v5"
)

var version = "dev"

func main() {
	cmd := "run"
	if len(os.Args) > 1 {
		cmd = os.Args[1]
	}
	var err error
	switch cmd {
	case "run":
		err = run()
	case "status":
		err = status()
	case "delete-scale-set":
		err = deleteScaleSet()
	case "version":
		fmt.Println(version)
	default:
		err = fmt.Errorf("usage: lsbx-scaleset [run|status|delete-scale-set|version]")
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "lsbx-scaleset:", err)
		os.Exit(1)
	}
}

func logger(level string) *slog.Logger {
	var l slog.Level
	_ = l.UnmarshalText([]byte(level))
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: l}))
}

func newClient(ctx context.Context, cfg *Config, log *slog.Logger) (*scaleset.Client, error) {
	key, err := os.ReadFile(cfg.AppKeyPath)
	if err != nil {
		return nil, fmt.Errorf("read GitHub App key: %w", err)
	}
	if cfg.InstallationID == 0 {
		org, _ := cfg.Org()
		id, err := lookupInstallation(ctx, cfg.AppID, key, org)
		if err != nil {
			return nil, fmt.Errorf("look up App installation for %s: %w", org, err)
		}
		cfg.InstallationID = id
		log.Info("resolved App installation", "org", org, "installation_id", id)
	}
	return scaleset.NewClientWithGitHubApp(scaleset.ClientWithGitHubAppConfig{
		GitHubConfigURL: cfg.RegistrationURL,
		GitHubAppAuth:   scaleset.GitHubAppAuth{ClientID: cfg.AppID, InstallationID: cfg.InstallationID, PrivateKey: string(key)},
		SystemInfo:      scaleset.SystemInfo{System: "lsbx-scaleset", Version: version, Subsystem: "listener"},
	}, scaleset.WithHTTPClient(httpCounter))
}

// countingClient tallies outbound requests so the REST budget claim (phase
// done criterion 3) is measured, not asserted: api.github.com is the
// rate-limited REST API; everything else is the Actions service (long-poll,
// JIT, runner admin), which does not draw on the 5,000/h REST budget.
type countingClient struct {
	inner         *http.Client
	rest, actions atomic.Int64
}

func (c *countingClient) Do(req *http.Request) (*http.Response, error) {
	if req.URL.Host == "api.github.com" {
		c.rest.Add(1)
	} else {
		c.actions.Add(1)
	}
	return c.inner.Do(req)
}

var httpCounter = &countingClient{inner: scaleset.NewHTTPClient(scaleset.HTTPClientConfig{})}

func reportRequests(ctx context.Context, log *slog.Logger, every time.Duration) {
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			log.Info("github requests", "window", every.String(),
				"rest_api", httpCounter.rest.Swap(0), "actions_service", httpCounter.actions.Swap(0))
		}
	}
}

// lookupInstallation resolves GET /orgs/{org}/installation with an App JWT.
func lookupInstallation(ctx context.Context, appID string, pemKey []byte, org string) (int64, error) {
	block, _ := pem.Decode(pemKey)
	if block == nil {
		return 0, errors.New("App key is not PEM")
	}
	var signer any
	if k, err := x509.ParsePKCS1PrivateKey(block.Bytes); err == nil {
		signer = k
	} else if k, err := x509.ParsePKCS8PrivateKey(block.Bytes); err == nil {
		signer = k
	} else {
		return 0, errors.New("App key: unsupported key format")
	}
	now := time.Now()
	tok, err := jwt.NewWithClaims(jwt.SigningMethodRS256, jwt.RegisteredClaims{
		IssuedAt: jwt.NewNumericDate(now.Add(-time.Minute)), ExpiresAt: jwt.NewNumericDate(now.Add(9 * time.Minute)), Issuer: appID,
	}).SignedString(signer)
	if err != nil {
		return 0, err
	}
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, "https://api.github.com/orgs/"+org+"/installation", nil)
	req.Header.Set("Authorization", "Bearer "+tok)
	req.Header.Set("Accept", "application/vnd.github+json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return 0, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	var out struct {
		ID int64 `json:"id"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return 0, err
	}
	return out.ID, nil
}

func runnerGroupID(ctx context.Context, c *scaleset.Client, name string) (int, error) {
	if name == "" || strings.EqualFold(name, scaleset.DefaultRunnerGroup) {
		return 1, nil
	}
	g, err := c.GetRunnerGroupByName(ctx, name)
	if err != nil {
		return 0, err
	}
	return g.ID, nil
}

func labelsOf(ss *scaleset.RunnerScaleSet) []string {
	var out []string
	for _, l := range ss.Labels {
		out = append(out, l.Name)
	}
	slices.Sort(out)
	return out
}

// ensureScaleSet gets or creates the scale set and keeps its labels in sync
// with the config. The scale set is persistent: it is NOT deleted on exit, so
// a restart never drops queued jobs.
func ensureScaleSet(ctx context.Context, c *scaleset.Client, cfg Config, log *slog.Logger) (*scaleset.RunnerScaleSet, error) {
	gid, err := runnerGroupID(ctx, c, cfg.RunnerGroup)
	if err != nil {
		return nil, fmt.Errorf("runner group %q: %w", cfg.RunnerGroup, err)
	}
	want := &scaleset.RunnerScaleSet{Name: cfg.Name, RunnerGroupID: gid, RunnerSetting: scaleset.RunnerSetting{DisableUpdate: true}}
	for _, l := range cfg.Labels {
		want.Labels = append(want.Labels, scaleset.Label{Name: l})
	}
	ss, err := c.GetRunnerScaleSet(ctx, gid, cfg.Name)
	if err != nil {
		return nil, fmt.Errorf("get scale set: %w", err)
	}
	if ss == nil {
		log.Info("creating scale set", "name", cfg.Name, "group", cfg.RunnerGroup, "labels", cfg.Labels)
		return c.CreateRunnerScaleSet(ctx, want)
	}
	wantLabels := slices.Clone(cfg.Labels)
	slices.Sort(wantLabels)
	if !slices.Equal(labelsOf(ss), wantLabels) {
		// Measured 2026-11-14: UpdateRunnerScaleSet silently ignores label
		// changes; labels only take effect at creation. Recreate, but only
		// when nothing is assigned, so no queued or running job is dropped.
		if ss.Statistics != nil && (ss.Statistics.TotalAssignedJobs > 0 || ss.Statistics.TotalRegisteredRunners > 0) {
			return nil, fmt.Errorf("labels changed %v -> %v but scale set is busy (assigned=%d runners=%d); will retry",
				labelsOf(ss), wantLabels, ss.Statistics.TotalAssignedJobs, ss.Statistics.TotalRegisteredRunners)
		}
		log.Warn("labels changed; recreating scale set", "name", cfg.Name, "from", labelsOf(ss), "to", wantLabels)
		if err := c.DeleteRunnerScaleSet(ctx, ss.ID); err != nil {
			return nil, fmt.Errorf("delete scale set for label change: %w", err)
		}
		return c.CreateRunnerScaleSet(ctx, want)
	}
	return ss, nil
}

func run() error {
	cfg, err := LoadConfig()
	if err != nil {
		return err
	}
	log := logger(cfg.LogLevel).With("scale_set", cfg.Name)
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	unlock, err := lockFile(cfg.StatePath + ".lock")
	if err != nil {
		return err
	}
	defer unlock()

	client, err := newClient(ctx, &cfg, log)
	if err != nil {
		return err
	}
	ss, err := ensureScaleSet(ctx, client, cfg, log)
	if err != nil {
		return err
	}
	client.SetSystemInfo(scaleset.SystemInfo{System: "lsbx-scaleset", Version: version, Subsystem: "listener", ScaleSetID: ss.ID})
	log.Info("scale set ready", "id", ss.ID, "labels", labelsOf(ss), "max_runners", cfg.MaxRunners)

	backend := &LsbxCLI{Bin: cfg.LsbxBin, Global: cfg.LsbxArgs, Profile: cfg.Profile}

	// A crashed predecessor can leave a session that blocks a new one until
	// it expires; retry for a few minutes before giving up to systemd.
	host, _ := os.Hostname()
	var session *scaleset.MessageSessionClient
	for attempt := 1; ; attempt++ {
		session, err = client.MessageSessionClient(ctx, ss.ID, host+"/"+cfg.Name)
		if err == nil {
			break
		}
		if attempt == 12 || ctx.Err() != nil {
			return fmt.Errorf("create message session: %w", err)
		}
		log.Warn("message session not available yet; retrying", "attempt", attempt, "error", err)
		select {
		case <-time.After(20 * time.Second):
		case <-ctx.Done():
		}
	}
	defer session.Close(context.WithoutCancel(ctx))

	scaler := NewScaler(cfg, log.WithGroup("scaler"), backend, client, session, ss.ID)
	if cfg.VirshURI != "" {
		scaler.sweeper = &VirshSweeper{URI: cfg.VirshURI, VMDir: cfg.VMDir}
	}
	if err := scaler.Recover(ctx); err != nil {
		return fmt.Errorf("recover: %w", err)
	}
	defer scaler.Shutdown(4 * time.Minute)

	l, err := listener.New(session, listener.Config{ScaleSetID: ss.ID, MaxRunners: cfg.MaxRunners, Logger: log.WithGroup("listener")})
	if err != nil {
		return err
	}
	go reportRequests(ctx, log, time.Hour)
	log.Info("listening for jobs")
	if err := l.Run(ctx, scaler); err != nil && !errors.Is(err, context.Canceled) {
		return fmt.Errorf("listener: %w", err)
	}
	log.Info("stopping")
	return nil
}

func status() error {
	cfg, err := LoadConfig()
	if err != nil {
		return err
	}
	b, err := os.ReadFile(cfg.StatePath)
	if err != nil {
		return err
	}
	_, err = os.Stdout.Write(append(b, '\n'))
	return err
}

func deleteScaleSet() error {
	cfg, err := LoadConfig()
	if err != nil {
		return err
	}
	ctx := context.Background()
	log := logger(cfg.LogLevel)
	c, err := newClient(ctx, &cfg, log)
	if err != nil {
		return err
	}
	gid, err := runnerGroupID(ctx, c, cfg.RunnerGroup)
	if err != nil {
		return err
	}
	ss, err := c.GetRunnerScaleSet(ctx, gid, cfg.Name)
	if err != nil {
		return err
	}
	if ss == nil {
		fmt.Println("no such scale set:", cfg.Name)
		return nil
	}
	if err := c.DeleteRunnerScaleSet(ctx, ss.ID); err != nil {
		return err
	}
	fmt.Println("deleted scale set", cfg.Name, ss.ID)
	return nil
}

func lockFile(path string) (func(), error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		return nil, fmt.Errorf("another lsbx-scaleset holds %s", path)
	}
	return func() { f.Close() }, nil
}
