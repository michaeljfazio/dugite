---
name: ledgerdb-v2-diff-retention-and-snapshot-decoupling
description: How LedgerDB V2 (LedgerSeq/StateRef) retains full materialized ledger states across the k-block volatile window, why rollback needs no diff reverse-application, and why disk-snapshotting never touches/clears the in-memory retention
type: reference
---

## Repo/commit context

All citations from `IntersectMBO/ouroboros-consensus`, `main` branch (fetched
2026-07-08). The **V1 LedgerDB (`DbChangelog`/`AnchorlessDbChangelog` +
separate `BackingStore`)** has been **fully deleted** from `main` — there is
no `V1/` directory left under
`ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/LedgerDB/`.
Only comments referencing "In V1: ..." remain (e.g.
`Storage/LedgerDB/Snapshots.hs:273`). Current production architecture is
**V2 only**, with three backends behind a common `Backend` typeclass
(`Storage/LedgerDB/V2/Backend.hs`): `Mem` (in-package,
`Storage/LedgerDB/V2/InMemory.hs`), `LSM` (separate package
`ouroboros-consensus-lsm`, `Storage/LedgerDB/V2/LSM.hs`), and legacy `LMDB`
(only a `SnapshotBackend` tag `UTxOHDLMDBSnapshot` survives for reading old
snapshots — no `LMDB.hs` implementation module found in the tree anymore).

Could not pin the exact ouroboros-consensus package version cardano-node
11.0.1 resolves via CHaP (no `cabal.project.freeze` in cardano-node repo;
resolution is index-state-based). Treat this memory as "current mainline
design," not confirmed byte-for-byte against 11.0.1's exact dependency
snapshot — but there is no V1 fallback path in the current tree at all.

## Core finding: NOT a diff-sequence-to-reverse-apply design

**Every retained state in the volatile window holds a FULLY MATERIALIZED
UTxO table, not a bare diff.** There is no separate "DiffSeq" that rollback
reverse-applies against a shared base. Rollback is a pure structural trim of
an `AnchoredSeq`, dropping full nodes — nothing is recomputed or reverse-applied.

### Key types — `Storage/LedgerDB/V2/LedgerSeq.hs`

```haskell
data StateRef m l blk = StateRef
  { state  :: !(l blk EmptyMK)             -- ledger state w/o UTxO tables
  , tables :: !(LedgerTablesHandle m l blk) -- handle onto a FULL table snapshot
  }

newtype LedgerSeq m l blk = LedgerSeq
  { getLedgerSeq :: AnchoredSeq (WithOrigin SlotNo) (StateRef m l blk) (StateRef m l blk) }
```

`AnchoredSeq` here is the standard ouroboros-network `AnchoredSeq` (same
structure used for the candidate/selected chain fragment) — an anchor plus a
finger-tree-like sequence supporting O(log n) splits from either end.

### Why full states, not diffs — explicit haddock (API.hs, top of file)

> "Maintaining the past \(k\) in-memory ledger states: we might roll back up
> to \(k\) blocks when switching to a more preferable fork... This means
> that we need access to all ledger states of the past \(k\) blocks... **Note
> that applying a block to a ledger state is not an invertible operation, so
> it is not possible to simply /unapply/ \(C_1\) and \(C_2\) to obtain
> \(I\).**"

This is the explicit design rationale: ledger-state application is not
invertible, so Haskell does NOT attempt reverse-diff application for
rollback. It keeps `k` full states resident instead.

### How each state gets its "full table" cheaply — `duplicateWithDiffs`

`LedgerTablesHandle` (LedgerSeq.hs) has:
```haskell
duplicateWithDiffs :: l blk EmptyMK -> l blk DiffMK -> m (LedgerTablesHandle m l blk)
```
"Create a new handle by duplicating this one and push some diffs to it."
Called on **every block push** (`implForkerPush` in `V2/Forker.hs:109-126`,
and `reapplyBlock`/`reapplyThenPush` in `LedgerSeq.hs:235-262`):
1. duplicate the PREVIOUS state's table handle (cheap — persistent/CoW
   structure, not a deep copy)
2. apply that one block's diff into the duplicate
3. the result is a brand-new, independently-full, self-contained handle
   for the NEW state — pushed as the new `StateRef` at the tip of `LedgerSeq`

Two backend implementations, same pattern, different cost model:
- **Mem** (`V2/InMemory.hs` `implDuplicateWithDiffs`, ~line 132): wraps a
  Haskell `Data.Map.Strict`; new map built via `Diff.applyDiff vals d` —
  cheap due to structural sharing of the persistent map, but the whole
  resulting map is a genuine, independently-readable `LedgerTables ...
  ValuesMK` (i.e. the FULL UTxO set at that block), not a diff record.
- **LSM** (`ouroboros-consensus-lsm/.../V2/LSM.hs` `implDuplicateWithDiffs`,
  ~line 261): `duplicateLSMTable` (O(1) copy-on-write LSM-tree table
  duplicate, `LSM.duplicate`) then `LSM.updates t vec` writes that block's
  diff into the duplicate. Same shape: every `StateRef` ends up owning its
  own independently-queryable LSM table handle covering the full UTxO set
  at that point, just backed by an LSM-tree instead of an in-memory `Map`.

So "diffs" only ever exist as a **transient, one-block-wide payload** used
to go from state N-1's full table to state N's full table at push time.
They are never persisted as a standalone forward-only or reverse-appliable
log spanning the volatile window.

## Point 2 — retention invariant tied to k

`isSaturated` (LedgerSeq.hs:431-433):
```haskell
isSaturated :: GetTip (l blk) => SecurityParam -> LedgerSeq m l blk -> Bool
isSaturated (SecurityParam k) db = maxRollback db >= unNonZero k
```
`maxRollback = fromIntegral . AS.length . getLedgerSeq` — literally the
count of retained `StateRef` nodes. The AnchoredSeq is expected to hold
(once saturated) **at least `k` volatile states on top of the anchor**,
matching the haddock's "Maintaining the past k in-memory ledger states."
`rollbackN n` (LedgerSeq.hs:352-361) returns `Nothing` if `n > maxRollback
ldb`, i.e. rollback is only ever possible within however many states are
currently resident, capped at k by design, capped below k only during
startup/replay or if fewer than k blocks have been seen yet.

## Point 3 & 5 — disk snapshot is completely decoupled from live retention

Two entirely separate operations reading the SAME `ldbSeq` TVar, but one is
read-only w.r.t. it and the other mutates it:

**`implTryTakeSnapshot`** (`Storage/LedgerDB/V2.hs:329-395`):
```haskell
handles <- RAWLock.withReadAccess (ldbOpenHandlesLock env) $ \() -> do
  lseq@(LedgerSeq immutableStates) <- atomically $ do
    LedgerSeq states <- readTVar $ ldbSeq env
    volSuffix <- getVolatileSuffix (ldbGetVolatileSuffix env)
    pure $ LedgerSeq $ AS.dropNewest (AS.length (volSuffix states)) states
  ...
  Monad.forM snapshotSlots $ \slot -> do
    let pruneStrat = LedgerDbPruneBeforeSlot (slot + 1)
    (slot,) <$> (duplicateStateRef $ anchorHandle $ snd $ prune pruneStrat lseq)
```
- It only ever reads `ldbSeq env` once (`readTVar`), then works on a purely
  **local, non-shared value** (`lseq`). `prune` here is a pure function
  called on that local copy; its `close`-old-handles action (the `fst` of
  `prune`'s result) is **discarded, never run** — because it's not the live
  sequence.
- It picks candidate snapshot slots only from `immutableStates`, i.e.
  states **already outside the volatile suffix** (`AS.dropNewest
  (length volSuffix)` strips off exactly the still-rollback-eligible tail
  before even considering snapshot candidates) — so snapshotting can only
  ever target states that are no longer within the rollback window anyway.
- It `duplicateStateRef` (fresh handle duplicate, via `duplicate` not
  `duplicateWithDiffs`) the chosen state's table handle, serializes THAT
  duplicate to disk (`takeSnapshot snapManager ... h` then `close . tables
  $ h` — closes only the temporary duplicate), and **never writes back to
  `ldbSeq env`**.

**`implGarbageCollect`** (`Storage/LedgerDB/V2.hs:320-327`) is the ONLY
function that mutates the live sequence by pruning:
```haskell
implGarbageCollect env slotNo = do
  ...
  close <- atomically $ stateTVar (ldbSeq env) $ prune (LedgerDbPruneBeforeSlot slotNo)
```
This is invoked from `Storage/ChainDB/Impl/Background.hs`'s
`copyToImmutableDBRunner`, triggered by `copyToImmutableDB` (blocks older
than `k` from the chain tip get moved VolatileDB → ImmutableDB) — i.e. it
fires off **chain-growth / immutable-tip advancement**, computed from
`SecurityParam`/k, completely independent of whether/when a disk snapshot
was ever taken. Snapshot-writing and volatile-state garbage collection are
two unrelated triggers that happen to read the same TVar.

**Conclusion for the Dugite bug**: In Haskell's actual design, writing a
disk snapshot **never** clears, prunes, or otherwise touches the live
in-memory per-block state/table retention for the volatile window. The
premise "snapshotting reclaims memory by dropping in-memory diffs" has no
analogue in the current Haskell architecture — there is no in-memory
diff-only structure to reclaim from; every volatile-window block already
carries a full materialized table, and eviction of old (immutable, i.e.
no-longer-rollback-eligible) states happens strictly via `garbageCollect`
tied to immutable-tip advancement (k), never via snapshot writes. **Clearing
Dugite's DiffSeq on snapshot-write is a bug — there is no Haskell precedent
for coupling those two operations, and doing so breaks rollback into the
still-volatile part of the window whenever a snapshot lands inside it.**

## Point 4 — rollback mechanics, no reverse-apply

`rollbackN`/`rollbackToPoint`/`rollback` (LedgerSeq.hs:352-512) are pure
`AnchoredSeq` operations: `AS.dropNewest` (drop n from the tip) or
`AS.rollback`/`AS.splitAtMeasure` (find and cut at a `Point`/slot). Both are
documented as \( O(\log(\min(i,n-i))) \). No block is re-applied and no
diff is reverse-applied — the dropped `StateRef`s are simply discarded
(their handles get `close`d by the caller), and whichever `StateRef` is now
the new tip is **already** a fully materialized state+table pair, ready to
serve queries or have new blocks pushed onto it immediately.

Fork-switch path: chain selection opens a "forker" over the current
`LedgerSeq` (`withForkerByRollback`, API.hs ~line 645), which internally
does `rollback pt` to intersect, then repeatedly `implForkerPush` (Forker.hs
:109-126) — each push duplicates-with-diffs the current tip's handle and
extends — and finally `implForkerCommit` (Forker.hs:128-173) splices the
forker's local `LedgerSeq` prefix back onto the shared `ldbSeq` TVar via
`AS.splitAfterMeasure`/`AS.join`, closing only the now-superseded old
branch's states.

## Practical Rust/Dugite translation note

If Dugite's `LedgerSeq` (states) + `DiffSeq` (diffs) split is meant to
mirror this design, the correct mapping is:
- Dugite's `LedgerSeq` entries should each own (or reference via COW/Arc) a
  **fully materialized UTxO snapshot** for that block, not just a state
  header — analogous to `StateRef.tables`.
- The "diff" is only ever a **transient computation artifact** produced
  when applying a new block (old full table + new block's diff → new full
  table), not a standalone structure that must be retained/replayed later.
- Disk-snapshot writing should read from whichever `StateRef`/`LedgerSeq`
  entry is already outside the rollback window (or, simplest: always
  snapshot at/behind the immutable tip) and must be a **read-only,
  independent** operation w.r.t. the live in-memory volatile-window
  structure — it must never prune, clear, or invalidate anything still
  within reach of `k`-bounded rollback.
- Eviction of old volatile-window entries belongs solely to the
  immutable-tip-advancement path (k-bounded chain growth), not to the
  snapshot-write path. If Dugite currently clears its DiffSeq inside the
  snapshot-write code path, that coupling should be removed; reclaim memory
  instead via whatever function already prunes on immutable-tip advance
  (the Dugite analogue of `garbageCollect`/`implGarbageCollect`).

See also: [chaindb-architecture](chaindb-architecture.md),
[mithril-snapshot-ledger-init](mithril-snapshot-ledger-init.md) for adjacent
ChainDB/snapshot init context (those predate this deep dive and describe
the on-disk snapshot *file format*, not this in-memory retention design).
