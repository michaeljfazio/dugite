#!/usr/bin/env python3
"""Reduce `cardano-cli debug log-epoch-state` output to per-epoch digest files.

    mkfifo /tmp/epochstate.fifo
    cardano-cli debug log-epoch-state --socket-path … --node-configuration-file … \
        --out-file /tmp/epochstate.fifo &
    scripts/validation/reduce-epoch-state-log.py \
        --fifo /tmp/epochstate.fifo --out-dir reports/mainnet-exactness/cn-logepochstate

WHY A REDUCER RATHER THAN THE RAW FILE
--------------------------------------
`log-epoch-state` emits the WHOLE epoch state as one JSON line per epoch and
never terminates. At mainnet scale a single epoch's stake maps are hundreds of
MB — the same volume wall that made dugite's own dumps 1-2 TB by tip. Streaming
through a FIFO and reducing each line as it arrives keeps the raw state from
ever touching disk.

Large flat maps collapse to `{__count__, __sum__, __digest__}` using the SAME
canonical form as `diff-cstreamer-dumps.py` and dugite's `digest_of_map`
(sorted, `key:value;`, values bare, sha256) so all three sources are
comparable. Everything else is kept verbatim.

WHY THIS ORACLE AT ALL
----------------------
cardano-streamer is pinned to cardano-node 10.6.2 dependencies. For HISTORICAL
epochs that is provably fine — any version that can sync mainnet must compute
identical ledger state or it would fork — but it is NOT fine for Conway, whose
rules are recent. `log-epoch-state` is cardano-node 11.0.1 itself, so it
sidesteps the version question entirely.

Its cost is that it reports epochs only as a running node CROSSES them, so it
has to be attached to a live sync. That is free here: the node is syncing to
tip anyway.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

LARGE_MAP_THRESHOLD = 500


def digest_of_map(m: dict) -> dict:
    """Byte-identical to `digest_of_map` in diff-cstreamer-dumps.py and main.rs."""
    h = hashlib.sha256()
    total = 0
    summable = True
    for k in sorted(m):
        v = m[k]
        if isinstance(v, bool):
            summable = False
            rendered = str(v)
        elif isinstance(v, int):
            if summable:
                total += v
            rendered = str(v)
        elif isinstance(v, str):
            summable = False
            rendered = v
        else:
            summable = False
            rendered = json.dumps(v, sort_keys=True, separators=(",", ":"))
        h.update(k.encode())
        h.update(b":")
        h.update(rendered.encode())
        h.update(b";")
    out = {"__count__": len(m), "__digest__": h.hexdigest()}
    if summable:
        out["__sum__"] = total
    return out


def reduce_tree(obj, key=None):
    if isinstance(obj, dict):
        if len(obj) > LARGE_MAP_THRESHOLD and all(
            isinstance(v, (int, str)) and not isinstance(v, bool) for v in obj.values()
        ):
            return digest_of_map(obj)
        return {k: reduce_tree(v, k) for k, v in obj.items()}
    if isinstance(obj, list):
        if len(obj) > LARGE_MAP_THRESHOLD:
            h = hashlib.sha256()
            for e in sorted(
                json.dumps(x, sort_keys=True, separators=(",", ":"), default=str)
                for x in obj
            ):
                h.update(e.encode())
                h.update(b";")
            return {"__count__": len(obj), "__digest__": h.hexdigest()}
        return [reduce_tree(v) for v in obj]
    return obj


def find_epoch(obj):
    """Locate the epoch number without assuming the top-level shape."""
    if isinstance(obj, dict):
        for k in ("lastEpoch", "epoch", "currentEpoch", "epochNo"):
            v = obj.get(k)
            if isinstance(v, int):
                return v
        for v in obj.values():
            e = find_epoch(v)
            if e is not None:
                return e
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fifo", required=True)
    ap.add_argument("--out-dir", required=True)
    args = ap.parse_args()
    os.makedirs(args.out_dir, exist_ok=True)

    n = 0
    # Line-buffered read of the FIFO; blocks until the writer opens it.
    with open(args.fifo, "r") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError as e:
                # A truncated line is data loss, not noise — say so loudly
                # rather than silently dropping an epoch from the oracle.
                print(f"MALFORMED LINE ({len(line)} bytes): {e}", file=sys.stderr, flush=True)
                continue
            epoch = find_epoch(obj)
            reduced = reduce_tree(obj)
            name = f"{epoch}.json" if epoch is not None else f"unknown-{n:05d}.json"
            with open(os.path.join(args.out_dir, name), "w") as out:
                json.dump(reduced, out, sort_keys=True)
            n += 1
            print(f"reduced epoch {epoch} -> {name}", flush=True)
    print(f"FIFO closed after {n} epochs", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
