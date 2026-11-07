# Phase Spec — TOML config files for the Rust lsbx CLI

**Timestamp:** `2026-11-07T1440Z`
**Slug:** `config-file-toml`
**Status:** Proposed. Decides shape; does not implement.
**Supersedes:** nothing. Fills a gap the Python → Rust cutover left open.
**Related:** `docs/specs/2026-11-01T2354Z_calimbra-incus-backend/SPEC.md` (D7, per-host manifests), issue record TBD.

---

## 0. Problem, in one paragraph

The Python `lsbx` read `~/.config/lsbx/config.yaml`. The Rust rewrite reads **no
config file at all**: `--config <PATH>` is declared in `crates/lsbx-cli/src/cli.rs:25`
and never consumed, and `resolve_state_dir` / `resolve_images_path`
(`crates/lsbx-cli/src/lib.rs:363,392`) resolve flag → env → hardcoded default.
The live consequence was observed on carnyx on 2026-11-07: with a valid YAML
config present, a bare `lsbx status` reported `backend demo`,
`sandbox_count 0`, and `(no profiles registered)` — the tool describing a
machine state that did not exist. Operators worked around it with fish universal
variables (`LSBX_BACKEND`, `LSBX_STATE_DIR`, `LSBX_IMAGES`,
`LSBX_LIBVIRT_IMAGES_DIR`, `LSBX_LIBVIRT_VM_DIR`), which is a per-shell fix that
systemd units, `sh -c`, and cron do not inherit.

## 1. Decisions

| # | Question | Decision | Why |
|---|---|---|---|
| D1 | YAML or TOML? | **TOML.** Do not add a YAML parser. | Matches every other LUFS primitive (`lrex`'s `lufs-recorder.example.toml`, `~/.config/lufs-recorder/config.toml`); `serde + toml` is already in the workspace's dependency family. A YAML parser is a new dependency and a second schema for the same data. |
| D2 | Config path resolution | **`--config` → `LSBX_CONFIG` → `$XDG_CONFIG_HOME/lsbx/config.toml` (default `~/.config/lsbx/config.toml`) → built-in defaults.** | Exactly `lrex/src/config.rs`'s documented order. One rule for the whole family. |
| D3 | Project-local override | **Yes: `.lsbx.toml` in the CWD, layered over the user config, same shape, any subset of fields.** | `lrex`'s `ProjectOverride` precedent. It is how a repo pins its own `images` manifest without touching the host config. |
| D4 | Missing file | **Not an error.** Built-in defaults, same as today. | A CLI that cannot run before a config exists is a regression; `resolve_images_path`'s doc comment already commits to this stance for a missing `images.json`. |
| D5 | Malformed file | **Hard error, exit 6 (`Config`), naming the file and line.** Never fall back. | The whole failure being fixed is a *silent* fallback to fabricated defaults. Falling back again would reproduce it with extra steps. |
| D6 | Unknown keys | **`#[serde(deny_unknown_fields)]`.** | A typo'd `state_dir` that silently leaves the default in place is the bug class this spec exists to kill. |
| D7 | Env vars | **Kept, and they outrank the file** (flag → env → file → default). | Units already set env via `EnvironmentFile`; breaking that would break `lsbx-serve` and `lsbx-ci-broker`. Env is the deployment channel, the file is the operator channel. |
| D8 | Legacy YAML | **Read once, migrate, never parse again.** On first run, if `config.yaml` exists and `config.toml` does not, emit a TOML file plus a one-line notice; do not keep a YAML reader. | Removes a dead format without a hand-written migration step on six hosts. |
| D9 | `config --show` / `--init` / `--set` | **Become real.** `--show` prints the merged view with the *source* of each value (flag/env/file/default); `--init` writes the documented defaults; `--set` edits the user file. | Today `--show` "synthesizes a demo-backend view" — it lies. Reporting provenance is what makes the next silent-fallback bug impossible to hide. |
| D10 | `[gateway] host`/`port` in the file vs flags | **Both exist; a flag always wins.** The file supplies the default the unit's `ExecStart` would otherwise hardcode; `serve --host/--port` overrides it per invocation. | Decided 2026-11-07. The unit is the deployment authority and must keep working when someone runs `lsbx serve --port 9000` by hand; but a host that only ever uses one port should not have to restate it in the unit *and* remember it in three shell aliases. |
| D11 | One manifest per host, or a per-host map inside one file | **One file per host** (`images.carnyx.json`, `images.json`), selected by `images = "<path>"`. No map. | Decided 2026-11-07. Resolves O2 below, and matches the calimbra spec's D7: "sharing one manifest across hosts is how the live `agent-base` inconsistency happened." |
| D12 | Should `lsbx bootstrap` render units that use `LSBX_CONFIG`? | **Yes.** Generated units get `Environment=LSBX_CONFIG=/etc/lsbx/config.toml` (or the host's path) plus the secret `EnvironmentFile`, instead of five `Environment=` lines. | Decided 2026-11-07. It is the only way the file and the units cannot drift; today `serve` and `ci-broker` already disagree about `LSBX_STATE_DIR` (see the lsbx skill notes), which is exactly the drift a rendered config removes. |

## 2. Schema

Mirrors today's env names one-for-one so the migration is mechanical.

```toml
# ~/.config/lsbx/config.toml
backend = "libvirt"                  # libvirt | exedev | demo | auto
state_dir = "/home/carnyx/lsbx-state"
images = "/home/carnyx/repos/lsbx/images.carnyx.json"

[libvirt]
uri = "qemu:///system"
images_dir = "/home/carnyx/ISOs/images/goldens"
vm_dir = "/home/carnyx/ISOs/images/work"
guest_user = "lsbx"

[gateway]
token_env = "LSBX_GATEWAY_TOKEN"     # name of the var holding the secret, never the secret
host = "100.125.210.60"
port = 8243

[stream]
guest_port = 8000

[defaults]
lease = "1h"
ready_timeout = "300s"
```

Rules:

- Every key optional; absent ⇒ the existing default (so an empty file changes nothing).
- Durations are human strings (`"3h"`, `"300s"`) parsed by the same helper the
  `--lease`/`--reap-ttl` flags use, so flag and file cannot drift.
- **No secrets in the file.** `token_env` names a variable, following the
  existing `LSBX_EXEDEV_TOKEN_ENV` convention. A literal `[gateway] token = ...`
  is rejected by `deny_unknown_fields`.
- `images` may be a path *or* a per-host map (`{ carnyx = "...", molimo = "..." }`)
  — deferred; see §5 open question O2.

## 3. Implementation units (suggested split)

1. **`lsbx-config` crate** (new): `Config` struct + `load(explicit: Option<PathBuf>, cwd: &Path) -> Result<Loaded, LsbxError>`, `Loaded` carrying the merged `Config` plus per-field `Provenance`. Unit tests for the four resolution steps, the project override, and `deny_unknown_fields`.
2. **Wire the CLI**: `resolve_state_dir`, `resolve_images_path`, and `build_backend` read `Loaded` instead of calling `std::env::var` directly. The precedence table lives in one place, and `--show` prints it.
3. **YAML migration shim**: one-shot read of `config.yaml` only when no TOML exists; write TOML; notice on stderr. Delete after all hosts are cut over (target: with the exe.dev decommission).
4. **Docs**: replace the `--config` "declared but unused" note in `README.md` and the kora/carnyx operating notes; document that systemd units use `LSBX_CONFIG` (D12).
5. **`lsbx-bootstrap` units** (D12): render `Environment=LSBX_CONFIG=<path>` plus the secret `EnvironmentFile`, and delete the duplicated `LSBX_STATE_DIR` / `LSBX_IMAGES` / `LSBX_BACKEND` lines. Doing this *before* a host's config file exists would break that host, so it lands after unit 3's migration has run everywhere.

## 4. Verification

- Unit: precedence is flag → env → file → default, asserted per field.
- Unit: malformed TOML ⇒ exit 6, message contains path and position; nothing runs.
- Unit: unknown key ⇒ error naming the key.
- Unit: project-local `.lsbx.toml` overrides only the keys it sets.
- Integration: on carnyx, `lsbx --config /tmp/x.toml status` must agree with the
  env-var form already proven to work (`backend libvirt`, `sandbox_count N`).
- Regression: with **no** config and **no** env, `lsbx status` must still say
  `demo` — but `config --show` must say `backend = demo (default)` so the
  provenance is visible.

## 5. Open questions

- **O1 (RESOLVED, D10)** `[gateway] host`/`port` live in the file as defaults;
  a flag always wins.
- **O2 (RESOLVED, D11)** One manifest file per host, selected by config. No
  per-host map inside a single file.
- **O3 (RESOLVED, D12)** `lsbx bootstrap` renders `LSBX_CONFIG` units plus the
  secret `EnvironmentFile`. Its own change, so it does not ride along with the
  config crate, but it is decided and in scope.

Nothing else is open. The remaining work is unit 1 (`lsbx-config` crate) →
unit 2 (CLI wiring) → unit 3 (YAML shim) → unit 4 (docs), with unit 5
(bootstrap rendering, D12) last because it depends on a stable file path.

## 6. Out of scope

- Reading YAML at runtime (D8 — migration only).
- A config *schema version* field. If the shape needs breaking changes later,
  add one then, with evidence.
- Per-user configs on shared hosts (`LSBX_CONFIG` covers it).
