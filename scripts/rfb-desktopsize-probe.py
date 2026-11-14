#!/usr/bin/env python3
"""RFB probe: does the guest VNC server announce a framebuffer size change?

Verification tool for lsbx issue #51/#56. It answers: when the Windows guest
changes its own display resolution, does the VNC server re-announce the new
framebuffer to a connected client (legacy `DesktopSize` pseudo-encoding, -223) —
or must we swap the VNC server for one that implements `ExtendedDesktopSize`?

Speaks RFB 3.8 with VNC authentication. The VNC challenge/response is plain
DES-ECB with a per-byte bit-reversed key, computed here via the `openssl` CLI so
this needs no Python crypto dependency (`-provider legacy` is tried first on
OpenSSL 3, where single-DES moved out of the default provider).

Every guest desktop golden binds VNC to `127.0.0.1` only, so tunnel it first:

  ssh -f -N -L 15900:127.0.0.1:5900 -i ~/.ssh/lsbx_guest_key lsbx@<guest-ip>
  ./scripts/rfb-desktopsize-probe.py --port 15900 --password lsbx --seconds 60 &
  # ...now change the guest resolution from a session INSIDE the guest

What a healthy result looks like:

  [  0.19s] *** DesktopSize(-223): new size 2560x1600      <- at connect
  [ 12.40s] *** DesktopSize(-223): new size 1920x1080      <- after the change

Known caveat (measured 2026-11-14, see #56): a display-mode change cannot be
issued from an SSH/session-0 context at all — `ChangeDisplaySettingsEx` returns
DISPLAY_CHANGE_FAILED there. Execute the mode change inside the interactive
console session instead.

Exit codes: 0 = a DesktopSize(-223)/(-308) rect was observed; 3 = none observed
within the window; 2 = protocol/auth failure.
"""
from __future__ import annotations

import argparse
import socket
import struct
import subprocess
import sys
import time


def bit_reverse(b: int) -> int:
    r = 0
    for i in range(8):
        r = (r << 1) | ((b >> i) & 1)
    return r


def vnc_response(password: str, challenge: bytes) -> bytes:
    """16-byte VNC auth response = DES(key, challenge) with a bit-reversed key."""
    key = password.encode()[:8].ljust(8, b"\x00")
    rev = bytes(bit_reverse(b) for b in key).hex()
    # OpenSSL 3 puts single-DES in the legacy provider; try both shapes so the
    # probe works on older and newer installs alike.
    for extra in (["-provider", "legacy", "-provider", "default"], []):
        proc = subprocess.run(
            ["openssl", "enc", "-des-ecb", "-nopad", "-K", rev, *extra],
            input=challenge,
            capture_output=True,
        )
        if proc.returncode == 0 and len(proc.stdout) == 16:
            return proc.stdout
    raise RuntimeError(f"openssl DES failed: {proc.stderr.decode(errors='replace').strip()}")


def recv_exact(sock: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("peer closed")
        buf += chunk
    return buf


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=5900)
    ap.add_argument("--password", default="lsbx")
    ap.add_argument("--seconds", type=float, default=60.0)
    args = ap.parse_args()

    t0 = time.time()
    sock = socket.create_connection((args.host, args.port), timeout=10)
    sock.settimeout(2.0)

    version = recv_exact(sock, 12)
    print(f"[{time.time()-t0:6.2f}s] server version: {version!r}", flush=True)
    sock.sendall(b"RFB 003.008\n")

    n_types = recv_exact(sock, 1)[0]
    types = recv_exact(sock, n_types)
    print(f"[{time.time()-t0:6.2f}s] security types: {list(types)}", flush=True)
    if 2 not in types:
        print("FATAL: VNC authentication not offered", flush=True)
        return 2
    sock.sendall(bytes([2]))
    challenge = recv_exact(sock, 16)
    sock.sendall(vnc_response(args.password, challenge))
    result = struct.unpack(">I", recv_exact(sock, 4))[0]
    print(f"[{time.time()-t0:6.2f}s] security result: {result} (0 = ok)", flush=True)
    if result != 0:
        return 2

    sock.sendall(bytes([1]))  # ClientInit, shared
    init = recv_exact(sock, 24)
    width, height = struct.unpack(">HH", init[0:4])
    name_len = struct.unpack(">I", init[20:24])[0]
    name = recv_exact(sock, name_len).decode(errors="replace")
    print(f"[{time.time()-t0:6.2f}s] ServerInit: {width}x{height} name={name!r}", flush=True)

    # Encodings: Raw, CopyRect, DesktopSize(-223), ExtendedDesktopSize(-308),
    # LastRect(-224), Cursor(-239)
    encs = [0, 1, -223, -308, -224, -239]
    payload = struct.pack(">BxH", 2, len(encs)) + b"".join(
        struct.pack(">i", e) for e in encs
    )
    sock.sendall(payload)
    print(f"[{time.time()-t0:6.2f}s] SetEncodings sent: {encs}", flush=True)

    def request_full(h: int, w: int) -> None:
        sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, w, h))

    request_full(height, width)

    deadline = t0 + args.seconds
    seen_size_change = False
    size_changes = []
    rects = 0
    cur_w, cur_h = width, height

    def request_incremental() -> None:
        # FramebufferUpdateRequest(incremental=1, x=0, y=0, w, h)
        sock.sendall(struct.pack(">BBHHHH", 3, 1, 0, 0, cur_w, cur_h))

    while time.time() < deadline:
        try:
            msg_type = recv_exact(sock, 1)[0]
        except socket.timeout:
            # A TightVNC server with nothing to send is silent; nudge it so a
            # resize is observed promptly rather than waiting for the next
            # unrelated screen change.
            request_incremental()
            continue
        except ConnectionError:
            print("peer closed", flush=True)
            break

        if msg_type == 0:  # FramebufferUpdate
            _pad, n_rects = struct.unpack(">BH", recv_exact(sock, 3))
            for _ in range(n_rects):
                x, y, w, h, enc = struct.unpack(">HHHHi", recv_exact(sock, 12))
                rects += 1
                if enc == -223:  # DesktopSize
                    seen_size_change = True
                    cur_w, cur_h = w, h
                    size_changes.append((round(time.time() - t0, 2), w, h))
                    print(
                        f"[{time.time()-t0:6.2f}s] *** DesktopSize(-223): new size {w}x{h}",
                        flush=True,
                    )
                elif enc == -308:  # ExtendedDesktopSize
                    seen_size_change = True
                    hdr = recv_exact(sock, 4)
                    num_screens = hdr[0]
                    recv_exact(sock, 16 * max(num_screens, 1))
                    cur_w, cur_h = w, h
                    size_changes.append((round(time.time() - t0, 2), w, h))
                    print(
                        f"[{time.time()-t0:6.2f}s] *** ExtendedDesktopSize(-308): {w}x{h}"
                        f" screens={num_screens}",
                        flush=True,
                    )
                elif enc == 0:  # Raw, 32bpp
                    recv_exact(sock, w * h * 4)
                elif enc == -239:  # Cursor
                    recv_exact(sock, w * h * 4 + ((w + 7) // 8) * h)
                elif enc == 1:  # CopyRect
                    recv_exact(sock, 4)
                # LastRect(-224) / unknown: no payload
            request_incremental()
        elif msg_type == 1:  # SetColourMapEntries
            _pad, _first, n = struct.unpack(">BHH", recv_exact(sock, 5))
            recv_exact(sock, n * 6)
        elif msg_type == 2:  # Bell
            pass
        elif msg_type == 3:  # ServerCutText: pad(3) + len(4) + text
            recv_exact(sock, 3)
            length = struct.unpack(">I", recv_exact(sock, 4))[0]
            recv_exact(sock, length)
        else:
            print(f"[{time.time()-t0:6.2f}s] unhandled msg type {msg_type}", flush=True)
            break

    sock.close()
    print(
        f"rects={rects} size_changes={size_changes}",
        flush=True,
    )
    if seen_size_change:
        print("RESULT: size change reported by server (-223/-308)", flush=True)
        return 0
    print("RESULT: no DesktopSize rect observed within the window", flush=True)
    return 3


if __name__ == "__main__":
    sys.exit(main())
