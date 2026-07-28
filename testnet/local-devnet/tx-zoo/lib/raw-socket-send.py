#!/usr/bin/env python3
"""Write arbitrary (usually malformed) bytes to a unix or TCP socket and report
what the peer did with the connection.

Vendored to remove the `socat` dependency that made tx-zoo's 08r skip on every
run: python3 is already a hard harness dependency (anchor hashing, the anchor
HTTP server), `socat` is not installed by default on macOS or on minimal Linux
CI images. stdlib only.

Usage
-----
  raw-socket-send.py --unix /tmp/ld-501/dbp.sock --hex deadbeef
  raw-socket-send.py --tcp 127.0.0.1:3002 --file frame.bin --expect-close

Output: one JSON object on stdout, e.g.

  {"connected": true, "sent": 25, "outcome": "closed-by-peer",
   "received": 0, "received_hex": "", "elapsed_s": 0.004}

`outcome` is one of:
  closed-by-peer  the peer sent EOF (clean close) — the usual correct answer
                  to a malformed frame
  reset           the peer reset the connection (also a close, harsher)
  open            the peer kept the connection open until --read-timeout
                  expired without sending EOF
  refused / error  we never got to send anything

Exit status: 0 sent successfully; 2 could not connect or send;
3 `--expect-close` was requested and the peer left the connection open.
"""

import argparse
import errno
import json
import socket
import sys
import time


def read_payload(args):
    if args.hex is not None:
        return bytes.fromhex(args.hex.strip())
    if args.file is not None:
        with open(args.file, "rb") as f:
            return f.read()
    return sys.stdin.buffer.read()


def connect(args):
    if args.unix:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(args.connect_timeout)
        sock.connect(args.unix)
        return sock
    host, _, port = args.tcp.rpartition(":")
    sock = socket.create_connection((host, int(port)), timeout=args.connect_timeout)
    return sock


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    target = ap.add_mutually_exclusive_group(required=True)
    target.add_argument("--unix", help="path to a unix-domain socket")
    target.add_argument("--tcp", help="HOST:PORT")
    src = ap.add_mutually_exclusive_group()
    src.add_argument("--hex", help="payload as a hex string")
    src.add_argument("--file", help="payload file (raw bytes)")
    ap.add_argument("--connect-timeout", type=float, default=5.0)
    ap.add_argument(
        "--read-timeout",
        type=float,
        default=5.0,
        help="how long to wait for the peer to answer or close (default 5s)",
    )
    ap.add_argument(
        "--expect-close",
        action="store_true",
        help="exit 3 if the peer keeps the connection open",
    )
    ap.add_argument(
        "--max-read",
        type=int,
        default=65536,
        help="stop reading after this many bytes (default 64 KiB)",
    )
    args = ap.parse_args(argv)

    result = {
        "connected": False,
        "sent": 0,
        "outcome": "error",
        "received": 0,
        "received_hex": "",
        "elapsed_s": 0.0,
    }
    started = time.monotonic()
    sock = None
    try:
        payload = read_payload(args)
        sock = connect(args)
        result["connected"] = True
        sock.settimeout(args.read_timeout)
        if payload:
            sock.sendall(payload)
        result["sent"] = len(payload)

        # Read until EOF (peer closed = the expected reaction to a bad frame),
        # the read timeout (peer kept it open), or max-read bytes.
        chunks = bytearray()
        outcome = "open"
        while len(chunks) < args.max_read:
            try:
                data = sock.recv(min(4096, args.max_read - len(chunks)))
            except socket.timeout:
                outcome = "open"
                break
            except OSError as exc:
                outcome = "reset" if exc.errno in (errno.ECONNRESET, errno.EPIPE) else "error"
                result["error"] = str(exc)
                break
            if not data:
                outcome = "closed-by-peer"
                break
            chunks += data
        result["received"] = len(chunks)
        result["received_hex"] = bytes(chunks[:512]).hex()
        result["outcome"] = outcome
    except (OSError, ValueError) as exc:
        result["error"] = str(exc)
        if not result["connected"]:
            result["outcome"] = "refused"
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        result["elapsed_s"] = round(time.monotonic() - started, 4)
        print(json.dumps(result))

    if not result["connected"]:
        return 2
    if args.expect_close and result["outcome"] not in ("closed-by-peer", "reset"):
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
