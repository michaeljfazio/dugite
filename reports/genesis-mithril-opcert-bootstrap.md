# Genesis-mode Mithril bootstrap failure: per-peer opcert counters not seeded

**Date:** 2026-06-12
**Network reproduced on:** preprod (epoch 294, Mithril snapshot tip slot 125,536,544)
**Severity:** blocks ALL chainsync on a Mithril-bootstrapped genesis-mode node

## Reported symptom

A node configured with `--consensus-mode genesis` "cannot bootstrap from
mithril due to missing snapshot files." The "missing files" framing is the
reporter's interpretation; the snapshot files are present and import cleanly.
The real failure is downstream: every peer is disconnected immediately after
the first header, so the node never leaves the snapshot tip (`blocks_applied=0`).

## Reproduction

```
dugite-node mithril-import --network-magic 1 --database-path ./db-preprod-repro   # OK
dugite-node run --consensus-mode genesis --database-path ./db-preprod-repro …      # wedged
```

Pre-fix log (rc2):
```
INFO  Reseeded consensus opcert counters from post-replay ledger state count=255
WARN  Praos: opcert counter over-incremented (CounterOverIncrementedOCERT)
        slot=125539468 pool=85eb86b4… got=5 last_seen=0
WARN  chainsync task failed … eager header validation failed at slot 125539468:
        Operational cert counter over-incremented: got 5, last seen 0 (max increment is 1)
```
Every peer disconnected on its first Conway header; `blocks_applied_total=0`.

Koios preprod ground truth for the offending pool (TPREP,
`pool1sh4cddrln788xmnjnsqhdwj9e7th3c3ck3zjk7ny9znwj44t8he`): `op_cert_counter = 5`
— so `got=5` is CORRECT and the rejection is a dugite false positive. The
decoded Haskell snapshot carries the same value (probed directly:
`opcert_counters.len = 255`, `TPREP entry = Some(5)`, `max = 463`).

## Root cause

Eager per-peer header validation (`eager_validate_header`, `node/sync.rs`) runs
against a **per-peer** opcert-counter map (`CandidateChainState::eager_opcert_counters`,
introduced in #652 C1 so one peer's fork cannot mutate another's — or the
global — counter state). `validate_header_full_with_counters` swaps that map
in wholesale via `std::mem::take`.

The per-peer map is created EMPTY (`or_default()`). Inside the OCERT check,
an absent-but-known pool falls back to counter `0`, so the predicate becomes
`got=5 > 0+1` → `CounterOverIncrementedOCERT`. The global, snapshot-derived
counter map (correctly holding TPREP=5) is discarded by the per-peer swap and
never consulted.

A **from-genesis** node never hit this: it accumulates each pool's counter
from 0 as it applies the chain, so the per-peer map is already warm by the
time eager validation runs near the tip. Only a node that JUMPED to a
mid-chain tip via a Mithril snapshot — where pools already carry arbitrary
counters (preprod max 463) — is affected. That is exactly the genesis-mode
bootstrap path in the report.

## Fix

`node/sync.rs`: before each eager validation, lazily seed the pool's entry in
the per-peer map from the global (snapshot-derived) counters when absent
(`seed_peer_counter_from_global`). An existing per-peer entry is preserved (a
peer's own fork may have legitimately advanced past the snapshot value). The
non-eager apply path was never affected (it reads the global map directly).

## Validation

- 4 unit tests (`seed_peer_counter_tests`): seeds absent pool, preserves
  existing peer entry, leaves globally-absent pool absent, and the end-to-end
  predicate (un-seeded rejects / seeded accepts the snapshot counter).
- Live on preprod (rc3, genesis mode, same db-preprod-repro): **0**
  `over-incremented` WARNs, **0** eager header validation failures, 5 hot
  peers, Conway chain extended past the snapshot tip (block 4,814,526 >
  snapshot 4,812,366). Pre-fix this DB+binary wedged at `blocks_applied=0`
  with every peer disconnected on its first header.

## Out of scope (separately tracked): genesis-mode near-tip stall

After the fix, the genesis-mode node bulk-syncs forward but stalls ~279
blocks short of the live preprod tip (applied slot 125,586,355 vs live
125,593,384, `gsm_state=0`/PreSyncing). A PRAOS-mode run on the **same DB**
reaches the live tip exactly (slot 125,593,495 = `max_peer_tip_slot`, 8 hot
peers, 0 opcert WARNs), proving the stall is genesis-mode-specific and NOT a
regression of this fix nor part of the reported bootstrap failure.

This is the documented genesis-mode tip-following incompleteness — the node's
own boot log warns "Genesis-specific peer selection are not yet implemented",
and the genesis-mode audit records LoE unenforced (dead fn) and CaughtUp
unreachable live. Completing Full Genesis (LoE enforcement, GDD, CaughtUp
transition) is a separate roadmap effort, not a v2.0.5 deliverable. Operators
who need to reach the live tip today should bootstrap with
`--consensus-mode praos` (the default).
