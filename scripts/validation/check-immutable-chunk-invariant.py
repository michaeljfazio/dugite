#!/usr/bin/env python3
"""Check that an ImmutableDB's chunks obey cardano-node's chunk invariant.

    for every chunk file NNNNN.chunk, every block's slot must satisfy
        slot // chunkSize == NNNNN

cardano-node's ImmutableDB numbers chunks by a FIXED SLOT RANGE (the Byron
epoch length, constant across every era) and computes `chunkIndex(slot)` to
locate any block. A database that violates the invariant is unreadable by
cardano-node, cardano-cli, cardano-streamer and anything else built on
ouroboros-consensus, which opens with:

    FsResourceDoesNotExist … immutable/06040.primary

naming a chunk that was never written, because the blocks it wants are packed
inside a lower-numbered file (#1081).

**Why this exists rather than just running cardano-node.** The end-to-end check
— hand the database to cardano-node and require it to open — is the ultimate
authority, but it needs a Haskell node, a matching config, and minutes per run.
This reads only the 56-byte secondary-index entries, needs nothing but Python,
and fails on the FIRST chunk that over-runs its range instead of at whatever
point consensus happens to notice.

Verified in both directions before being trusted:

  * a genuine cardano-node ImmutableDB PASSES — mainnet chunk 02351 has max
    slot 50,793,902 inside its nominal range 50,781,600..50,803,199;
  * a dugite-written one FAILS — preview chunk 27577 has max slot 119,278,774
    against a nominal range ending at 119,136,959, an overshoot of 141,815
    slots (32.8 chunk-ranges).

A checker that has only ever been run against a passing database is not known
to be able to fail.

Usage:
    check-immutable-chunk-invariant.py <db>/immutable --chunk-size 21600
    check-immutable-chunk-invariant.py <db>/immutable --chunk-size 4320   # preview

`chunkSize` is the network's BYRON epoch length: 21600 for mainnet and preprod,
4320 for preview. It is not the Shelley epoch length — cardano-node holds the
chunk size constant across eras, so a Shelley epoch spans 20 chunks.

Exit codes:
    0  every chunk satisfies the invariant
    1  at least one violation
    2  nothing was checked (no chunks found) — never reported as a pass
"""
import argparse
import pathlib
import struct
import sys

ENTRY = 56
SLOT_OFFSET = 48  # blockOrEBB is the last 8 bytes, big-endian, ABSOLUTE slot


def entries(path: pathlib.Path):
    raw = path.read_bytes()
    n, rem = divmod(len(raw), ENTRY)
    for i in range(n):
        (slot,) = struct.unpack_from(">Q", raw, i * ENTRY + SLOT_OFFSET)
        yield slot
    if rem:
        # A partial entry means a torn write; report it rather than ignoring it.
        print(f"  {path.name}: {rem} trailing bytes — not a whole index entry")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("immutable_dir")
    ap.add_argument("--chunk-size", type=int, required=True,
                    help="Byron epoch length: mainnet/preprod 21600, preview 4320")
    ap.add_argument("--max-report", type=int, default=10)
    args = ap.parse_args()

    d = pathlib.Path(args.immutable_dir)
    secs = sorted(d.glob("*.secondary"))
    if not secs:
        print(f"VACUOUS: no secondary index files under {d} — nothing was checked.")
        return 2

    violations = []
    checked = 0
    for s in secs:
        try:
            idx = int(s.stem)
        except ValueError:
            continue
        lo = idx * args.chunk_size
        hi = lo + args.chunk_size - 1
        worst = None
        for slot in entries(s):
            # `blockOrEBB` is a UNION, not always a slot: for an Epoch Boundary
            # Block it holds the EPOCH NUMBER, and Byron chunks begin with one.
            # In Byron the chunk index IS the epoch number, so an EBB entry
            # stores exactly `idx`. Without this the checker reports every Byron
            # chunk of a GENUINE cardano-node database as a violation — which is
            # what the control run did, and why the control is run at all.
            if slot == idx:
                continue
            if slot // args.chunk_size != idx:
                if worst is None or slot > worst:
                    worst = slot
        checked += 1
        if worst is not None:
            violations.append((idx, lo, hi, worst))

    print(f"checked {checked} chunks in {d} (chunkSize={args.chunk_size})")
    if not violations:
        print("PASS — every block's slot maps back to the chunk that holds it.")
        return 0

    print(f"\nFAIL — {len(violations)} chunk(s) hold blocks outside their slot range:")
    for idx, lo, hi, worst in violations[: args.max_report]:
        over = worst - hi
        print(f"  chunk {idx:05d}: nominal {lo}..{hi}, saw slot {worst} "
              f"(+{over} slots = {over / args.chunk_size:.1f} chunk-ranges); "
              f"consensus would look for chunk {worst // args.chunk_size:05d}")
    if len(violations) > args.max_report:
        print(f"  … and {len(violations) - args.max_report} more")
    return 1


if __name__ == "__main__":
    sys.exit(main())
