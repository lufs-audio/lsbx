# Unit 05 — latency-and-encoder-measurement

## Objective
Produce recorded numbers — glass-to-glass video latency, audio latency, host and
guest CPU cost, and packet behaviour — for a `win11` sandbox streaming over a slot,
and use them to settle D2 (encoder tier), D4a (relay overhead) and D10 (the
concurrency cap).

## Context
- Phase SPEC §5 D2 (encoder tiers and their costs), D4a (relay flip condition:
  >2 ms glass-to-glass means revisit kernel NAT), D10 (cap of 2 — to be validated
  here), §3.1 (Sunshine's stats surface).
- Units `03-moonlight-slot-allocation.md` and `04-gateway-slot-relay.md` in this
  folder — the slot and relay this unit measures.
- `scripts/rfb-desktopsize-probe.py` — existing in-repo probe style (a small,
  committed, documented measurement tool; this unit follows that precedent).
- Host facts: 13700K (16C/24T), 188 GiB, no IOMMU, host Sunshine already holding
  47984–48010 (SPEC §2.1–2.3). Software x264 is the baseline (D2 tier A).

## Acceptance criteria
- [ ] `scripts/measure-moonlight-latency.sh` exists, is documented in its own
      header, and produces a machine-readable result file
      (`docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/MEASUREMENTS-05.json`).
- [ ] **Video latency** is measured by two independent methods and both numbers
      are recorded:
      (a) Sunshine/Moonlight's own reported encode + network + decode figures,
      (b) camera-based ground truth: a millisecond counter rendered full-screen in
      the guest, filmed on the client's display at ≥240 fps, difference computed
      from frame timestamps.
- [ ] **Audio latency** is measured as *audio-minus-video offset*: the guest emits
      a simultaneous visual flash and a click; the client records its own video
      and audio output; the offset between the flash (in the streamed video) and
      the click (in the client's audio) gives audio latency relative to the
      already-measured video latency. Both absolute numbers are recorded.
- [ ] **CPU cost** is recorded for: host `pidstat`/`mpstat` during a 1080p60
      stream, guest encoder CPU, and the relay process's CPU (D4a's number).
- [ ] **Concurrency**: two simultaneous 1080p60 streams are attempted with CI
      idle and with CI busy; for each, frame rate and dropped frames are recorded.
- [ ] A 1080p60 stream's **packet loss / retransmit** behaviour over the tailnet
      path is recorded (Moonlight's stats overlay or equivalent).
- [ ] `FINDINGS-05-latency.md` states, explicitly:
      - whether software x264 is usable (D2 tier A verdict),
      - whether the relay costs >2 ms (D4a flip condition, yes/no),
      - the recommended `LSBX_MOONLIGHT_SLOTS` value with the numbers behind it,
      - the measured numbers as a table.
- [ ] The phase SPEC's "Unverified" table is updated with these as resolved.

## Interface contract
```bash
# scripts/measure-moonlight-latency.sh — usage
#
#   scripts/measure-moonlight-latency.sh --sandbox <id> \
#       --method stats|camera|audio-offset|all \
#       --out docs/specs/2026-11-21T1814Z_.../MEASUREMENTS-05.json
#
# Exit non-zero if a requested method could not produce a number.
#
# Output JSON shape:
# {
#   "sandbox_id": "...", "slot": 0, "resolution": "1920x1080", "fps": 60,
#   "video": { "sunshine_encode_ms": 0.0, "network_ms": 0.0, "decode_ms": 0.0,
#              "glass_to_glass_camera_ms": { "min": 0, "median": 0, "max": 0 } },
#   "audio": { "offset_vs_video_ms": 0.0, "absolute_ms": 0.0 },
#   "cpu":   { "host_relay_pct": 0.0, "host_qemu_pct": 0.0, "guest_encoder_pct": 0.0 },
#   "concurrency": { "streams": 2, "ci_busy": false, "fps_achieved": [0, 0], "dropped_frames": [0, 0] },
#   "method_notes": "..."
# }
#
# In-guest helper written by the script (not baked into the golden):
#   C:\lsbx\latency-clock.ps1  — full-screen ms counter; `-Flash` mode emits a
#   visual flash synchronised with a 1 kHz click for the audio-offset method.
```

## Boundaries
- **Touches:** `scripts/measure-moonlight-latency.sh` (new),
  `docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/MEASUREMENTS-05.json`
  (new), `.../FINDINGS-05-latency.md` (new), and the "Unverified" table in the
  phase `SPEC.md`.
- **Does NOT touch:** any `crates/` source, `scripts/provision-win11-golden.ps1`
  (the clock is pushed at measure time, not baked — it must not become golden
  state), `images.carnyx.json`, the host's Sunshine config.

## Verification
```bash
shellcheck scripts/measure-moonlight-latency.sh
scripts/measure-moonlight-latency.sh --sandbox <id> --method all \
  --out docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/MEASUREMENTS-05.json
jq . docs/specs/2026-11-21T1814Z_desktop-audio-and-low-latency-streaming/MEASUREMENTS-05.json
# every numeric field above must be non-null for the methods requested
```
