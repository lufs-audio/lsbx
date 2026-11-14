package main

import (
	"fmt"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"
)

// Config is read from the environment (systemd EnvironmentFile). Every key is
// prefixed LSBX_SS_ except the GitHub App ones, which reuse the names the
// polling broker's ci-runner.env already uses.
type Config struct {
	RegistrationURL string   // LSBX_SS_URL, e.g. https://github.com/lufs-audio
	Name            string   // LSBX_SS_NAME, e.g. lsbx-linux
	Labels          []string // LSBX_SS_LABELS (comma separated); defaults to Name
	RunnerGroup     string   // LSBX_SS_RUNNER_GROUP, e.g. continuo
	MinRunners      int      // LSBX_SS_MIN_RUNNERS (default 0 = zero idle)
	MaxRunners      int      // LSBX_SS_MAX_RUNNERS (default 2)

	AppID          string // GITHUB_APP_ID
	AppKeyPath     string // GITHUB_APP_KEY (path to PEM; never leaves the host)
	InstallationID int64  // GITHUB_APP_INSTALLATION_ID (optional; looked up if 0)

	LsbxBin       string        // LSBX_SS_LSBX_BIN
	LsbxArgs      []string      // LSBX_SS_LSBX_ARGS: global flags (backend/images/state-dir)
	Profile       string        // LSBX_SS_PROFILE (default ci)
	Lease         time.Duration // LSBX_SS_LEASE: hard cap on a VM's life (default 3h)
	BootTimeout   time.Duration // LSBX_SS_BOOT_TIMEOUT: VM up -> runner registered (default 10m)
	IdleTimeout   time.Duration // LSBX_SS_IDLE_TIMEOUT: surplus idle runner grace (default 3m)
	RunnerTarball string        // LSBX_SS_RUNNER_TARBALL: host-cached actions-runner-linux-x64-*.tar.gz
	GuestScript   string        // LSBX_SS_GUEST_SCRIPT: scripts/run-jit-runner.sh
	RunnerUser    string        // LSBX_SS_RUNNER_USER (default exedev)
	StatePath     string        // LSBX_SS_STATE: JSON state file
	VirshURI      string        // LSBX_SS_VIRSH_URI: enables the libvirt orphan sweep
	VMDir         string        // LSBX_LIBVIRT_VM_DIR: where orphan disks live
	LogLevel      string        // LSBX_SS_LOG_LEVEL
}

func env(key, def string) string {
	if v, ok := os.LookupEnv(key); ok && strings.TrimSpace(v) != "" {
		return strings.TrimSpace(v)
	}
	return def
}

func splitList(s string) []string {
	var out []string
	for _, f := range strings.FieldsFunc(s, func(r rune) bool { return r == ',' || r == ' ' || r == '\n' }) {
		if f = strings.TrimSpace(f); f != "" {
			out = append(out, f)
		}
	}
	return out
}

func LoadConfig() (Config, error) {
	var errs []string
	atoi := func(key, def string) int {
		n, err := strconv.Atoi(env(key, def))
		if err != nil {
			errs = append(errs, fmt.Sprintf("%s: %v", key, err))
		}
		return n
	}
	dur := func(key, def string) time.Duration {
		d, err := time.ParseDuration(env(key, def))
		if err != nil {
			errs = append(errs, fmt.Sprintf("%s: %v", key, err))
		}
		return d
	}
	c := Config{
		RegistrationURL: env("LSBX_SS_URL", ""),
		Name:            env("LSBX_SS_NAME", ""),
		Labels:          splitList(env("LSBX_SS_LABELS", "")),
		RunnerGroup:     env("LSBX_SS_RUNNER_GROUP", "default"),
		MinRunners:      atoi("LSBX_SS_MIN_RUNNERS", "0"),
		MaxRunners:      atoi("LSBX_SS_MAX_RUNNERS", "2"),
		AppID:           env("GITHUB_APP_ID", ""),
		AppKeyPath:      env("GITHUB_APP_KEY", ""),
		LsbxBin:         env("LSBX_SS_LSBX_BIN", "lsbx"),
		LsbxArgs:        strings.Fields(env("LSBX_SS_LSBX_ARGS", "")),
		Profile:         env("LSBX_SS_PROFILE", "ci"),
		Lease:           dur("LSBX_SS_LEASE", "3h"),
		BootTimeout:     dur("LSBX_SS_BOOT_TIMEOUT", "10m"),
		IdleTimeout:     dur("LSBX_SS_IDLE_TIMEOUT", "3m"),
		RunnerTarball:   env("LSBX_SS_RUNNER_TARBALL", ""),
		GuestScript:     env("LSBX_SS_GUEST_SCRIPT", ""),
		RunnerUser:      env("LSBX_SS_RUNNER_USER", "exedev"),
		StatePath:       env("LSBX_SS_STATE", ""),
		VirshURI:        env("LSBX_SS_VIRSH_URI", ""),
		VMDir:           env("LSBX_LIBVIRT_VM_DIR", ""),
		LogLevel:        env("LSBX_SS_LOG_LEVEL", "info"),
	}
	if v := env("GITHUB_APP_INSTALLATION_ID", ""); v != "" {
		id, err := strconv.ParseInt(v, 10, 64)
		if err != nil {
			errs = append(errs, "GITHUB_APP_INSTALLATION_ID: "+err.Error())
		}
		c.InstallationID = id
	}
	if len(c.Labels) == 0 {
		c.Labels = []string{c.Name}
	}
	req := map[string]string{
		"LSBX_SS_URL": c.RegistrationURL, "LSBX_SS_NAME": c.Name,
		"GITHUB_APP_ID": c.AppID, "GITHUB_APP_KEY": c.AppKeyPath,
		"LSBX_SS_GUEST_SCRIPT": c.GuestScript, "LSBX_SS_STATE": c.StatePath,
	}
	for k, v := range req {
		if v == "" {
			errs = append(errs, k+" is required")
		}
	}
	if c.MaxRunners < 1 || c.MinRunners < 0 || c.MinRunners > c.MaxRunners {
		errs = append(errs, "need 0 <= LSBX_SS_MIN_RUNNERS <= LSBX_SS_MAX_RUNNERS, MAX >= 1")
	}
	if c.RegistrationURL != "" {
		if _, err := c.Org(); err != nil {
			errs = append(errs, err.Error())
		}
	}
	if len(errs) > 0 {
		return c, fmt.Errorf("invalid configuration:\n  %s", strings.Join(errs, "\n  "))
	}
	return c, nil
}

// Org returns the organisation from an org-level registration URL.
func (c Config) Org() (string, error) {
	u, err := url.Parse(c.RegistrationURL)
	if err != nil {
		return "", fmt.Errorf("LSBX_SS_URL: %w", err)
	}
	parts := strings.Split(strings.Trim(u.Path, "/"), "/")
	if len(parts) != 1 || parts[0] == "" {
		return "", fmt.Errorf("LSBX_SS_URL must be an org URL like https://github.com/<org>, got %q", c.RegistrationURL)
	}
	return parts[0], nil
}
