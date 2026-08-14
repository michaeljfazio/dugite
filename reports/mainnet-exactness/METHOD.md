# Ledger-state exactness vs cardano-node — method and standing results

## What the comparison is

dugite and cardano-node are replayed over the SAME chain from genesis, and both
dump full ledger state at the first block of each new epoch. cardano-streamer
reads a real cardano-node ImmutableDB, so the oracle side is cardano-node's own
computed state, not a re-implementation and not an indexer.

Both sides dump at the same instant by construction: cstreamer's
`dumpEpochSnapshots` fires at `siFinal` when `isFirstSlotOfNewEpoch` using the
post-block state of the first block of the new epoch; dugite's `run_dump_snapshot`
writes post-`apply_block` when `current_epoch > last_epoch`. Both label with the
NEW epoch.

The comparator (`scripts/validation/diff-cstreamer-dumps.py`) walks both trees
leaf by leaf, is era-aware, bisects to the FIRST divergent epoch per field, counts
leaf COMPARISONS rather than rows, and **exits 3 rather than printing PASS when it
compared nothing**. Exit 2 on a schema gap is deliberate and must never be relaxed
for a green: a field absent on one side was never compared at all.

## Oracle

`oracle-bin/cstreamer-full-era-pv11`, built from
`michaeljfazio/cardano-streamer` branch `dugite/full-era-ledger-dumps`. Adds over
upstream: Byron dumping (with a slot-derived epoch, because Byron cannot report
its own), era/PV-gated protocol parameters, named and exact-rational governance
thresholds, exact `executionUnitPrices`, and the obsolete-node bound taken from
`ProtVerHigh ConwayEra` rather than cardano-api's hardcoded `natVersion @10`.

**Always check the sha256 against `PROVENANCE.txt` before trusting a run** — two
branches share one dist-newstyle, and a stale oracle is invisible in the output.

## Standing result — preprod, all eras including PV11

306 paired epochs, **4,622,661 leaf comparisons**. The first comparison ever to
cover PV11 (epochs 293-306).

Everything compares and matches except:

| item | scope | why |
|---|---|---|
| `byronProtocolParams.{maxBlockSize,maxTxSize,scriptVersion}`, `byronUpdateEpoch` | 3 Byron epochs | #1084 — dugite models no Byron update system |
| `byronDelegation` | 3 Byron epochs | #1084 — dugite models no Byron delegation map |
| `rupdNext`, `rupdApplied`, `snapshots.set` | epochs 4-5 | definitional first-Shelley: no prior epoch to have produced them |

Pots at epoch 306 are byte-identical to db-sync via Koios:
`treasury 1959103174719172`, `reserves 12979123112128607`.

## What this comparison found

Four dugite defects, every one in a field nobody had compared before:

1. `psDRepDistr` shed DReps cardano-node keeps — 10,578 missing (epoch, DRep)
   pairs over 143 Conway epochs. Consensus-adjacent: it is `RatifyEnv.reDRepDistr`.
2. A PlutusV3 cost model held through Alonzo and Babbage, where cardano-node has
   none.
3. `poolParams.owners` emitted in wire order where upstream's `Set` is ascending.
4. `poolDistribution` emitted in hash-map order, so the same binary over the same
   chain produced different bytes on consecutive runs.

(3) and (4) are the same shape: the ledger and the LSQ encoder were right, and
only the dump serialiser was not.

## Reading a report

- exit 0 — clean
- exit 1 — a real value divergence
- exit 2 — a SCHEMA GAP: a field expected in its era was absent on one side, so
  it was never compared. Not a pass.
- exit 3 — VACUOUS: nothing was compared. Always a harness fault.

`instantaneousRewards` is a deliberate honest gap: it holds PENDING MIR transfers
that the boundary applies and clears, and both sides dump at the first block of
the new epoch — an epoch-PHASE field whose interesting phase contains no dump
point. Across 66 oracle dumps including the 28 Shelley epochs where mainnet
carried MIR certificates: 0 non-empty, 0 non-zero deltas.
