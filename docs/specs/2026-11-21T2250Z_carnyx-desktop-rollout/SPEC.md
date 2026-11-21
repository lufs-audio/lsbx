# Carnyx desktop rollout and remote CLI

**Status:** rollout contract; host-dependent gates are not deemed complete by a merge.
**Scope:** #55, #53, #58, #51, #56, #57, #60 and the disposition of #52.
**Reconciles:** the Nov 14 console follow-ups and the Nov 21 desktop-audio spec.
Keep the existing project-wide `SPEC.md` pointer unchanged.

## Architecture and constraints

- Carnyx owns sandbox state, qcow2 disks, guest networking and the one libvirt gateway. Calimbra is a TLS reverse proxy only. Clients operate over HTTPS; no second store or remote-libvirt filesystem assumptions.
- Browser: same-origin noVNC → sandbox-scoped WebSocket ticket → gateway → private raw RFB. It is the recovery/agent path; no audio promise.
- Native audio: Moonlight → restricted tailnet path → Sunshine inside a Windows guest. The host's existing Sunshine instance and the Linux CI scale set are out of scope for changes.
- Carnyx's recorded i7-13700K / 188 GiB / UHD 770 / RTX 4080 SUPER is an inventory snapshot, not a capacity guarantee. Windows profile is 8 vCPU / 32 GB. Check actual host baseline before a canary. Start CPU encoding; don't change IOMMU, GPU binding, production networking, host services or production golden disks as part of this PR.
- Preserve the bridge's documented 1280 MTU; measure the actual media path and direct vs relayed Tailscale connections rather than changing the bridge to fix unmeasured loss.

## Units and promotion gates

### A. Browser console (#55/#53)

Keep gateway-owned assets and raw-RFB Windows listener already merged. Remove implicit guest password; prompt only when RFB requests credentials. Never put a guest password in a URL, web storage or log. Reconnect must mint a fresh ticket, tear down earlier transport attempts, and avoid callbacks from stale connections overriding current UI. On the relay, detect vanished clients and close the guest connection; preserve quiet but healthy sessions.

**Host acceptance:** create a fresh Windows and Linux canary. Test both directly over the tailnet gateway and through the normal public HTTPS edge, with a real browser: screen renders, input works, wrong guest password can be retried, tab abort followed by a new tab reconnects within 5 s in the healthy case, idle session survives, and `down` stops new sessions. Verify socket counts return to baseline after repeat abandon/reconnect. Run guest healthchecks and host-to-guest RFB handshake; neither nor an HTTP 101 alone is sufficient. Close #55 only after deployed validation. Close #53 only after both Windows and Linux satisfy recovery; no need to teach the removed bridge to restart.

### B. Linux golden (#58, Linux #51)

Preserve a compatibility recipe with the existing Xvfb/XFCE/x11vnc/websockify topology, recording whether it is an exact import or a reconstruction. Do not alter the legacy checkout. Provision a separate TigerVNC Xvnc/XFCE raw-RFB candidate with Resize enabled in noVNC only for the tested profile. Scope access to the gateway's private network path. Use a VNC password file (not a plaintext argument or a repository credential). Systemd owns session lifetime. Rebuilding the recipe on a fresh base and re-running it on the candidate must work; do not overwrite `agent-web` until a new version passes. Exercise two viewport sizes and check framebuffer dimensions and pointer positions, not just scaling. Keep the previous manifest mapping and disk for rollback. If native resize fails, keep scaling and debug Xvnc negotiation before adding an API.

### C. Gateway CLI (#57)

Opt in with a gateway URL and bearer token read from the environment. Verify TLS normally; require an explicit opt-in for plaintext HTTP, and do not follow redirects with credentials. Gateway mode and explicit local backend selection are mutually exclusive. Use existing REST handlers and keep service-side VM lifecycle ownership. Start with discovery/list/info/status/console, then create, delete, renew, exec and bytes-based file transfer. For file transfer use `/upload` and `/artifacts`, not `/put`/`/get` which use Carnyx paths. Keep local-only verbs explicit errors, preserve machine-readable envelopes/exit codes where the API supplies equivalent results, and never return success on bad authentication. Bound body sizes; downloads must not truncate an existing destination on early failure. Test against a demo service and separately test real TLS and edge body-size ceiling. Do not enable server local-file access for remote CLI.

### D. Windows audio and streaming canary (#60)

Opt in by exact Windows backing golden key with `LSBX_LIBVIRT_AUDIO_GOLDEN`; no wildcard or defaults. XML adds one guest ICH9 device explicitly attached to a discard (`none`) audio backend; no QEMU dependency on host PipeWire. Do not route host audio or add a device to Linux/other Windows goldens. Version a separate canary image and use a bounded, store-tracked lease. Stage a pinned Sunshine release in the running clone only; record installer hash/version, guest driver and client versions. Disable UPnP, restrict traffic to the approved private path, and do not bake credentials, pairing state, UUIDs or key material into the golden. Keep VNC viable. Start with QXL, SDR H.264 software encoding at 1080p60, and test an actual DAW project in addition to a tone. Verify input, audio sync, cold boot and reconnect. Test the intended Windows session/service context, not just a manual launch. If QXL capture fails, evaluate one signed VDD on the disposable clone, explicitly checking VNC does not go black. Don't treat a VDD as GPU acceleration.

**Capacity gate:** run idle baseline and one/then two 15-minute streams during a representative CI job, recording guest encode time, dropped/lost frames, audible dropouts, audio/video sync, p50/p95 input-to-display, host CPU/IO, and CI wall time; label network path. Proposed initial go/no-go for one stream on a direct tailnet connection: p95 input-to-display ≤100 ms, dropped frames ≤1%, no audible dropouts, and CI wall time ≤120% of a comparable baseline. Compare repeated baselines before blaming a single outlier. These are acceptance targets, not measured values. Reduce bitrate or evaluate 1080p30 if missed; document what passes instead of claiming 60 fps. Enforce a cap of one until the two-stream case passes. Playback/editing only; not real-time audio monitoring. Hardware encoding is a separate host-change proposal if CPU encode fails the actual workload.

### E. Streaming allocator and resize (after D)

Before a reusable media relay, prove supported Moonlight clients on the exact candidate port mapping including Sunshine's negative TCP offset. Check pair, reboot, two simultaneous unique guests, and slot reuse; a slot is routing, not identity. Moonlight presents the PIN, Sunshine accepts it. Generate per-VM identity once, retain it across reboot of that VM only, and default to re-pair on replacement. Pair through an approved management channel; don't include the admin UI in the general media listener set. Use an approved private canary path for this test (no subnet-wide publication or public exposure).

After that gate, define an explicit acquire/release/status API and `lsbx moonlight` client. `lsbx serve` is the single allocator. Acquire only on demand for a live supported sandbox; repeated acquire is idempotent. Reserve an actual disjoint port set including negative offsets. Bind directly to the tailnet address, fail closed on bind failure, disable UPnP, allow only approved tailnet principals, and never infer an arbitrary guest address/port from user input. Use bounded buffers, a cap/expiry for per-client UDP state, and cancellation tied to the specific sandbox/lease generation. On `down`, expiry or missing guest, close listeners and actively cancel flows before reusing ports. On restart reconcile against the store and guest identity before reopening. Measure relay overhead vs the private canary path; no privileged NAT helper unless separately approved. Benchmarks, restart, partial bind, exhaustion, concurrent acquire and stale flow isolation are required before promotion.

For browser Windows resize, first change dimensions inside the active session and prove the exact display TightVNC captures emits an updated framebuffer. The current QXL template already has 64 MiB; TightVNC's client-driven extension is not an option on the pinned release. Prefer the proven VDD control method if it controls that display, else use a narrow interactive-session helper with an authenticated local channel. Do not expose arbitrary guest commands through a resize endpoint. If the probe succeeds, implement authenticated `POST /sandboxes/{id}/display` with bounded presets, debounce and serialized requests; return the applied mode/failure, and fall back to scaling on error. Test login, mode change, reconnect and recovery console. A Moonlight-only resize doesn't close browser #51.

### F. Explicit non-goals / disposition (#52)

Plain TCP forwarding to raw RFB is not a browser UI. No new socat/tailcat integration in this rollout. Reopen design only for a named client that cannot use the existing HTTPS console or the tailnet. If browser audio is the requirement, test Guacamole/RDP on a separate profile first; check app compatibility and session semantics. If the same Windows console session is mandatory, timebox one authenticated WebRTC bridge spike *after* the native path works. No new public media ports or replacement of the recovery door.

## Delivery and rollback

Ship A–D's locally verifiable changes separately from host evidence and E's gated production work. Merge must not replace a golden or restart Carnyx. Before canary save the current gateway binary/config and exact manifest/disk mapping; use versioned clone disks, separate profiles, and a narrow test account/network policy. Stop testing with `lsbx down` after collecting redacted results. Roll back by removing the canary profile and audio selection and restoring prior manifest/binary if changed; never modify or reset an active golden disk. Record every real-host result and any declined gate in the operator runbook. Do not auto-close GitHub issues from a code-only PR.
