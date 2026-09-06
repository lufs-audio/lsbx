---
name: lsbx
description: >
  Drive lsbx — LUFS Audio's disposable-VM engine and zero-idle CI broker —
  from any agent, local or cloud. Covers the four doors (CLI, HTTP gateway,
  WS console, MCP), the two exe.dev transports (HTTPS token / SSH), auth
  env vars, and the safety rails around golden VMs and leases.
---

# lsbx — disposable sandboxes for agents

`lsbx` provisions short-lived VMs on exe.dev (hosted) or libvirt/KVM
(local), runs work in them over four doors, and tears them down with
verified cleanup. Repo: `lufs-audio/lsbx` (this skill ships in-repo at
`.agents/skills/lsbx/`).

## Doors

- **CLI** (`lsbx <verb>`): up/down/list/exec/status + golden/ci-broker
  subcommands. JSON envelope: `{"status":"success","data":...}` /
  `{"status":"error","code":<exit>,"message":...}`.
- **HTTP gateway** (`lsbx serve`, default `:8244`): REST
  create/exec/info/delete; bearer-auth except the public `/console` page.
- **WS console** + **MCP**: interactive + agent-native surfaces.

## exe.dev auth (pick one)

- `EXE_TOKEN` — account token; HTTPS only, no SSH needed. Control verbs
  (`ls --json`, `cp`, `tag`, `ssh-key add`) AND guest exec
  (`ssh <vm> <cmd>`, exit code via in-band sentinel). ~30 s cap per call;
  stderr merged into body.
- `LSBX_EXEDEV_SSH_KEY` — standalone SSH key auth.
- `LSBX_EXEDEV_SSH_ALIAS` (default `exe.dev`) — the Molimo services' mode.

HTTPS is the right default for cloud agents (no key material); SSH remains
the door for interactive tooling, file transfer, and >30 s jobs.

## Safety rails

- NEVER mutate/delete golden VMs (`lsbx-golden`-tagged) or user VMs like
  `lufs-dev`.
- Operate only on store-tracked sandboxes with bounded leases; the gateway
  caps concurrent sandboxes (default 8) and reaps stale ones (3 h TTL).
- When driving exe.dev directly (not through lsbx), a token needs `ssh`
  in its `cmds` for guest exec and `new`/`cp` for lifecycle; `rm` is
  deliberately rare — gateway destroy is the normal teardown.

## Ops quick reference

- Molimo services: `lsbx-serve.service`, `lsbx-ci-broker-exe.service`
  (Rust, cutover 2026-08-27). Carnyx: `lsbx-ci-broker.service`.
- Verify clean idle state: `lsbx list --json` + the GitHub runner-group
  inventory.
- Build-cache hygiene on long-lived hosts: `cargo clean` before you leave
  (rust-dev skill, Section 9 — molimo's 24 GB `target/` incident).
