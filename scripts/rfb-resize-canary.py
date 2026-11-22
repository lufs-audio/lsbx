#!/usr/bin/env python3
"""Exercise the TigerVNC raw-RFB resize canary without a browser.

This is intentionally separate from rfb-desktopsize-probe.py: that probe is
passive and serves the Windows/TightVNC investigation, while this canary sends
RFB SetDesktopSize (client message 251) and requires a successful
ExtendedDesktopSize (-308) acknowledgement.

Typical use against a guest provisioned with
LSBX_LINUX_DESKTOP_MODE=tigervnc-raw-rfb-v1:

  scripts/rfb-resize-canary.py --host <guest-private-ip> --port 8000 \
      --width 1600 --height 900

Run from the gateway host or an approved private management path. The probe
prompts without echoing, or reads LSBX_VNC_PASSWORD for a noninteractive test.

Protocol reference: TigerVNC's rfbproto.rst / RFC 6143 conventions. The
SetDesktopSize message itself is the ExtendedDesktopSize extension used by
TigerVNC and noVNC.

Exit codes: 0 resize acknowledged; 3 unsupported/rejected/timed out;
2 protocol or authentication failure.
"""
from __future__ import annotations

import argparse
import getpass
import os
import socket
import struct
import sys
import time

EXTENDED_DESKTOP_SIZE = -308
DESKTOP_SIZE = -223
LAST_RECT = -224


def recv_exact(sock: socket.socket, size: int) -> bytes:
    data = bytearray()
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise ConnectionError("RFB peer closed the connection")
        data.extend(chunk)
    return bytes(data)


def bit_reverse(value: int) -> int:
    result = 0
    for bit in range(8):
        result = (result << 1) | ((value >> bit) & 1)
    return result


def vnc_response(password: str, challenge: bytes) -> bytes:
    # VNC authentication uses legacy DES. TripleDES with a single repeated
    # key is equivalent; use the library in-process so the key isn't in argv.
    try:
        from cryptography.hazmat.primitives.ciphers import Cipher, modes
        try:
            from cryptography.hazmat.decrepit.ciphers.algorithms import TripleDES
        except ImportError:
            from cryptography.hazmat.primitives.ciphers.algorithms import TripleDES
    except ImportError as error:
        raise RuntimeError("install python3-cryptography to use this probe") from error
    key = password.encode("ascii")[:8].ljust(8, b"\0")
    reversed_key = bytes(bit_reverse(value) for value in key)
    # Spell the single-DES-equivalent EDE key as K1=K2=K3. Newer
    # cryptography releases deprecate the 8-byte TripleDES shorthand, while
    # the 24-byte repeated form preserves VNC authentication semantics.
    encryptor = Cipher(TripleDES(reversed_key * 3), modes.ECB()).encryptor()
    return encryptor.update(challenge) + encryptor.finalize()


def authenticate(sock: socket.socket, password: str) -> tuple[int, int, str]:
    server_version = recv_exact(sock, 12)
    if not server_version.startswith(b"RFB "):
        raise RuntimeError(f"unexpected server greeting: {server_version!r}")
    sock.sendall(b"RFB 003.008\n")
    count = recv_exact(sock, 1)[0]
    if count == 0:
        reason_length = struct.unpack(">I", recv_exact(sock, 4))[0]
        raise RuntimeError(recv_exact(sock, reason_length).decode(errors="replace"))
    security_types = recv_exact(sock, count)
    if 2 not in security_types:
        raise RuntimeError(f"server does not offer VNC authentication: {list(security_types)}")
    sock.sendall(b"\x02")
    sock.sendall(vnc_response(password, recv_exact(sock, 16)))
    result = struct.unpack(">I", recv_exact(sock, 4))[0]
    if result != 0:
        reason = "authentication failed"
        if result == 1:
            try:
                length = struct.unpack(">I", recv_exact(sock, 4))[0]
                reason = recv_exact(sock, length).decode(errors="replace")
            except (ConnectionError, socket.timeout):
                pass
        raise RuntimeError(reason)

    sock.sendall(b"\x01")  # ClientInit(shared=True)
    init = recv_exact(sock, 24)
    width, height = struct.unpack(">HH", init[:4])
    name_length = struct.unpack(">I", init[20:24])[0]
    name = recv_exact(sock, name_length).decode(errors="replace")
    print(f"server={server_version.decode().strip()} desktop={name!r} initial={width}x{height}")
    return width, height, name


def set_encodings(sock: socket.socket) -> None:
    encodings = [EXTENDED_DESKTOP_SIZE, DESKTOP_SIZE, LAST_RECT]
    sock.sendall(
        struct.pack(">BxH", 2, len(encodings))
        + b"".join(struct.pack(">i", encoding) for encoding in encodings)
    )


def request_update(sock: socket.socket, width: int, height: int) -> None:
    sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))


def send_resize(sock: socket.socket, width: int, height: int) -> None:
    # SetDesktopSize: type, pad, framebuffer w/h, screen count, pad, then one
    # screen record (id, x, y, w, h, flags).
    sock.sendall(
        struct.pack(">BBHHBB", 251, 0, width, height, 1, 0)
        + struct.pack(">IHHHHI", 0, 0, 0, width, height, 0)
    )


def read_update(sock: socket.socket) -> list[tuple[int, int, int, int, int, int]]:
    """Return (encoding, status, reason, width, height, screens) pseudo-rects."""
    message_type = recv_exact(sock, 1)[0]
    if message_type == 2:  # Bell
        return []
    if message_type == 3:  # ServerCutText
        recv_exact(sock, 3)
        length = struct.unpack(">I", recv_exact(sock, 4))[0]
        recv_exact(sock, length)
        return []
    if message_type != 0:
        raise RuntimeError(f"unexpected RFB server message type {message_type}")

    _padding, rectangle_count = struct.unpack(">BH", recv_exact(sock, 3))
    result = []
    for _ in range(rectangle_count):
        x, y, width, height, encoding = struct.unpack(">HHHHi", recv_exact(sock, 12))
        if encoding == EXTENDED_DESKTOP_SIZE:
            header = recv_exact(sock, 4)
            screens = header[0]
            recv_exact(sock, screens * 16)
            result.append((encoding, x, y, width, height, screens))
        elif encoding == DESKTOP_SIZE:
            result.append((encoding, 0, 0, width, height, 1))
        elif encoding == LAST_RECT:
            continue
        else:
            raise RuntimeError(
                f"server sent unrequested encoding {encoding}; cannot skip unknown payload"
            )
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--width", type=int, default=1600)
    parser.add_argument("--height", type=int, default=900)
    parser.add_argument("--seconds", type=float, default=10.0)
    args = parser.parse_args()
    if not (64 <= args.width <= 65535 and 64 <= args.height <= 65535):
        parser.error("width and height must be between 64 and 65535")

    password = os.environ.pop("LSBX_VNC_PASSWORD", None)
    if not password and sys.stdin.isatty():
        password = getpass.getpass("Canary VNC password: ")
    if not password:
        parser.error("provide a VNC password at the prompt or through LSBX_VNC_PASSWORD")
    try:
        with socket.create_connection((args.host, args.port), timeout=5) as sock:
            sock.settimeout(min(2.0, args.seconds))
            initial_width, initial_height, _name = authenticate(sock, password)
            set_encodings(sock)

            # Let the server advertise ExtendedDesktopSize support first, just as
            # noVNC does before enabling resizeSession.
            request_update(sock, initial_width, initial_height)
            advertised = False
            advertisement_deadline = time.monotonic() + min(3.0, args.seconds / 2)
            while time.monotonic() < advertisement_deadline:
                try:
                    rects = read_update(sock)
                except socket.timeout:
                    break
                for encoding, status, reason, width, height, screens in rects:
                    print(
                        f"advertisement encoding={encoding} status={status} reason={reason} "
                        f"size={width}x{height} screens={screens}"
                    )
                    if encoding == EXTENDED_DESKTOP_SIZE:
                        advertised = True
                if advertised:
                    break
            if not advertised:
                print("RESULT: server did not advertise ExtendedDesktopSize", file=sys.stderr)
                return 3

            print(f"requesting={args.width}x{args.height}")
            send_resize(sock, args.width, args.height)
            request_update(sock, args.width, args.height)
            deadline = time.monotonic() + args.seconds
            while time.monotonic() < deadline:
                try:
                    rects = read_update(sock)
                except socket.timeout:
                    request_update(sock, args.width, args.height)
                    continue
                for encoding, status, reason, width, height, screens in rects:
                    print(
                        f"response encoding={encoding} status={status} reason={reason} "
                        f"size={width}x{height} screens={screens}"
                    )
                    if encoding != EXTENDED_DESKTOP_SIZE:
                        continue
                    if status != 0:
                        print(
                            f"RESULT: resize rejected (status={status}, reason={reason})",
                            file=sys.stderr,
                        )
                        return 3
                    if (width, height) == (args.width, args.height):
                        print("RESULT: ExtendedDesktopSize resize acknowledged")
                        return 0
            print("RESULT: timed out waiting for requested size", file=sys.stderr)
            return 3
    except (ConnectionError, OSError, RuntimeError, struct.error) as error:
        print(f"FATAL: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
