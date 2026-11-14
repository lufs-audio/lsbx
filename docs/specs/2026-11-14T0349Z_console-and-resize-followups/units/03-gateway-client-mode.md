# Unit 03 — Gateway client mode: run `lsbx` from a client against a remote service

## Objective
Add an explicit client mode so a laptop (Klaxon, kora) can drive an `lsbx serve`
instance over its HTTP API — with TLS and a token — instead of requiring an SSH
session on the hypervisor host.

## Context
Read `docs/specs/2026-11-14T0337Z_console-endgame-recon/SPEC.md` §1.1 and §1.6.

Today there is **no** client mode:

- `crates/lsbx-cli/src/lib.rs::connect_libvirt` always builds
  `LibvirtTransport::Local { uri }`. `LibvirtTransport::RemoteSsh { … }` exists in
  `lsbx-backend-libvirt` (with `qemu+ssh://` URI construction and tests) and is
  never constructed by the CLI.
- Wiring it would not be enough: the backend's disk plumbing is client-local
  (`golden_disk::resolve`, `image_ops::qemu_img_create_cow`,
  `std::fs::remove_file(&vm_disk_path)`, the flatten path) — all of it runs
  `qemu-img`/`std::fs` against host paths like `/home/carnyx/ISOs/images/...` on
  whatever machine the CLI runs on.
- The HTTP service already works over TLS with a token and is proven end-to-end:
  `GET https://lsbx.lufshq.com/health` → 401 without a token, 200 with one, and
  `/stream/<id>/vnc` upgrades to 101 through Caddy+Cloudflare.

So the honest options are in `SPEC.md` §5-Q3. This unit implements **Option A**
(gateway client mode) and records why B and C were rejected.

**Architectural constraint (do not violate):** there must be exactly **one**
`lsbx serve` per state store. `LsbxOps` owns a `SandboxStore` and a direct libvirt
connection; a second gateway with its own state directory would diverge and reap
the other's sandboxes. Edge proxies (Caddy on Calimbra) are fine; a second service
is not. This unit therefore makes the CLI a *client* of the single service — it does
not make a second service.

## Acceptance criteria
- [ ] `lsbx` accepts a service target: `--gateway <url>` + `--token` (or
      `LSBX_GATEWAY_URL` / `LSBX_GATEWAY_TOKEN`), and rejects combining `--gateway`
      with `--backend libvirt|exedev` with a clear `Usage` error.
- [ ] Read operations work against a remote service and print the **same** JSON
      envelopes and human tables as the local path: `list`, `info`, `status`,
      `console`, `golden list`, `profiles`, `images`.
- [ ] Lifecycle operations work remotely: `up` (create), `down` (destroy), `renew`,
      `exec`, `put`, `get` — each mapping to the corresponding REST route.
- [ ] `--json` output is byte-identical in shape to the local backend's for `list`
      and `info` for the same sandbox (proven by a test that compares the two
      envelopes for a demo-backed service).
- [ ] A wrong/missing token produces `LsbxError::AuthFailed` (exit code 7), never a
      panic or a silent empty list.
- [ ] TLS is used when the URL is `https://`; a plaintext `http://` URL is accepted
      only with an explicit `--insecure` (mirroring `lsbx serve`'s fail-closed
      stance).
- [ ] `lsbx console <id> --gateway … --open` opens the console page URL the service
      reports (which, after Unit 01, is a page that actually works).
- [ ] README documents the three roles (edge / service / client) with the carnyx +
      calimbra + klaxon topology as the worked example.

## Interface contract
```
lsbx [--gateway <url>] [--token <t> | env LSBX_GATEWAY_TOKEN] <subcommand> …

# REST surface consumed (already implemented in lsbx-gateway):
GET  /health
GET  /sandboxes                       GET  /sandboxes/{id}
POST /sandboxes                       DELETE /sandboxes/{id}
POST /sandboxes/{id}/exec             POST /sandboxes/{id}/put|get
POST /sandboxes/{id}/renew            POST /sandboxes/{id}/console
GET  /consoles                        GET  /consoles/{id}
GET  /images  /profiles  /capabilities

# Client-side error mapping (unchanged taxonomy):
401 -> AuthFailed (7)   404 -> NotFound (4)   429 -> BackendUnavailable (3) w/ retry-after
5xx -> BackendUnavailable (3)                 4xx(body) -> Usage (2)
```

## Boundaries
- **Touches:** `crates/lsbx-cli/src/cli.rs` (flags), `crates/lsbx-cli/src/lib.rs`
  (target selection + dispatch), a new `crates/lsbx-cli/src/gateway_client.rs`,
  `crates/lsbx-cli/Cargo.toml` (HTTP client), `README.md`, `AGENTS.md` (topology).
- **Does NOT touch:** `crates/lsbx-backend-libvirt/**` (no `RemoteSsh` work in this
  unit), `crates/lsbx-stream/**`, `crates/lsbx-broker/**`, goldens/scripts,
  `crates/lsbx-store/**` schemas.

## Verification
```bash
cargo test --workspace

# against the live service
export LSBX_GATEWAY_URL=https://lsbx.lufshq.com
export LSBX_GATEWAY_TOKEN=$(grep -o 'LSBX_GATEWAY_TOKEN=.*' /home/carnyx/lsbx-state/serve.env | cut -d= -f2)
lsbx status                    # real status, same shape as local
lsbx list --json | jq .status  # "success"
ID=$(lsbx up win11 --lease 15m --json | jq -r .id)
lsbx console "$ID"             # a reachable console page URL
lsbx exec "$ID" -- cmd /c "echo remote-client-ok"
lsbx down "$ID"

# negative
LSBX_GATEWAY_TOKEN=wrong lsbx status; echo "exit=$?"     # 7
lsbx --gateway https://x --backend libvirt status; echo "exit=$?"   # 2
```
