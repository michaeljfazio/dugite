---
name: ledgerdb-init-replay-rollback-anchor-mechanism-pinned
description: Exact LedgerDB init/replay/rollback/fork-switch mechanism, verified at the EXACT ouroboros-consensus pin matching cardano-node 11.0.1 (release-ouroboros-consensus-3.0.1.0 @ c87aa760001e60f0f0d3353f793eb089adb917e7). Corrects/extends ledgerdb-v2-diff-retention-and-snapshot-decoupling.md (which was fetched from `main`, unpinned).
metadata:
  type: reference
---

# LedgerDB anchor/replay/rollback mechanism — pinned-SHA verification (oracle-verified 2026-08-03)

## Correction to prior memory
[[ledgerdb-v2-diff-retention-and-snapshot-decoupling]] was fetched from `main`
(2026-07-08) and claimed "V1 LedgerDB fully deleted." **False at the pin that
actually matches cardano-node 11.0.1.** At `release-ouroboros-consensus-3.0.1.0`
(`c87aa760001e60f0f0d3353f793eb089adb917e7`), BOTH backends exist:
`Ouroboros/Consensus/Storage/LedgerDB/V1.hs` (DbChangelog + BackingStore/LMDB)
and `.../V2.hs` (LedgerSeq, in-memory or LSM). Dispatch is via
`LedgerDbBackendArgs m blk = LedgerDbBackendArgsV1 ... | LedgerDbBackendArgsV2 ...`
(`Storage/LedgerDB/Args.hs:87-89`), and V2 is the configured default:
`Args.hs:81-85`, comment verbatim: "This value is the closest thing to a
pre-UTxO-HD node, and as such it will be the default for end-users." Both
backends share one `InitDB`-shaped abstraction (`API.hs`) and exhibit the
IDENTICAL anchor-advancement discipline described below (confirmed for V1 too
at `V1.hs:104-105,449`: `initReapplyBlock` also does `reapplyThenPush` ->
`extend` + `pruneToImmTipOnly` per replayed block).

## 1. Init entry point — `openDB` (`Storage/LedgerDB.hs:51-114`)
Dispatches on `lgrBackendArgs args` to build an `InitDB db m blk` record
(`initFromGenesis`, `initFromSnapshot`, `initReapplyBlock`, `currentTip`,
`mkLedgerDb` — `API.hs:542-557`), then calls `doOpenDB` -> `openDBInternal`
(`LedgerDB.hs:120-174`) which calls `initialize` (`API.hs:579-687`).

## 2. `initialize` / `replayStartingWith` (`API.hs:559-738`)
`initialize` picks a snapshot (newest-first, `tryNewestFirst`) or falls back
to `initFromGenesis`, then ALWAYS calls `replayStartingWith` to catch up to
`replayGoal` (the ImmutableDB tip) — verbatim doc comment (`API.hs:574-578`):
"We do /not/ attempt to use multiple ledger states from disk to construct the
ledger DB. Instead we load only a /single/ ledger state from disk, and
/compute/ all subsequent ones."

`replayStartingWith` (`API.hs:691-738`) folds over the block stream via
`streamAll stream from id (initDb, 0) push` where
`push blk (!db, !replayed) = do { !db' <- initReapplyBlock cfg blk db; ... }`
(`API.hs:705-738`) — **the SAME accumulator `db` (the whole LedgerDB
structure: anchor + sequence) is threaded through every fold step.** There is
no separate "raw ledger state" replay path that bypasses the anchor-bearing
structure — the accumulator IS that structure at every step. This answers "is
there a bulk-replay path that advances the ledger state without carrying the
LedgerDB anchor along": no, structurally excluded by the fold's own type.

## 3. V2 concrete mechanism — `LedgerSeq` (`Storage/LedgerDB/V2/LedgerSeq.hs`)

```haskell
newtype LedgerSeq m l = LedgerSeq
  { getLedgerSeq :: AnchoredSeq (WithOrigin SlotNo) (StateRef m l) (StateRef m l) }
```//(paraphrased shape; see prior memory for full StateRef record)

`mkInitDb` (`V2.hs:82-122`): `initFromGenesis`/`initFromSnapshot` each produce
`LedgerSeq . AS.Empty $ sr` (`V2.hs:87,90`) — i.e. **immediately after loading
from genesis or a disk snapshot, the LedgerSeq is JUST the anchor with an
EMPTY sequence.** `initReapplyBlock = reapplyThenPush` (`V2.hs:98`).

`reapplyThenPush` (`LedgerSeq.hs:239-249`):
```haskell
reapplyThenPush cfg ap db = do
  newSt <- reapplyBlock (ledgerDbCfgComputeLedgerEvents cfg) (ledgerDbCfg cfg) ap db
  let (m, db') = pruneToImmTipOnly $ extend newSt db
  m
  pure db'
```
`pruneToImmTipOnly = prune LedgerDbPruneAll` (`LedgerSeq.hs:339-343`), whose
own doctest states the postcondition: new anchor = old head/tip, sequence
becomes empty. **Every single block replayed during startup is: extend, then
immediately collapse back to a single-anchor/empty-sequence LedgerSeq.** This
is the mechanism, not a "paraphrase": the anchor is NOT set once at genesis
and left alone — it is reassigned on EVERY replayed block, always equal to
"the state produced by the most recently applied block," because every block
sourced from the ImmutableDB stream is, by definition, already immutable (no
rollback risk), so there is no reason to retain it in the volatile sequence.

Only once `openDB` returns (replay finished, `mkLedgerDb` wraps the resulting
single-anchor `LedgerSeq` into the live `LedgerDBEnv`) does the live path stop
auto-pruning on every push: `extend` (chain selection, via Forker) grows the
sequence WITHOUT collapsing it, and pruning back to just-past-`k` is a
SEPARATE, later action — `implGarbageCollect` (`V2.hs:317-324`) calling
`prune (LedgerDbPruneBeforeSlot slotNo)`, invoked only from
`copyToImmutableDB` (VolatileDB -> ImmutableDB migration, k-bounded). See
prior memory for that half (still accurate, re-confirmed at this pin with
line numbers V2.hs:317-395 vs the `main`-fetched 320-395/329-395).

**Practical bug-fix framing for Dugite**: the premise "anchor fixed once at
startup, deltas pushed for the live tip, reconstruction chimera" has NO
Haskell analogue to "fix by adding an invariant on top" — Haskell's anchor is
already always in lockstep with the current state because IT MOVES on every
push during replay (collapse-per-block) and is moved by an explicit, separate
k-bounded GC on the live path. A Dugite fix should mirror this exactly:
during startup replay, the "anchor" (or whatever plays that role) must be
REASSIGNED to each newly-computed state after every block (equivalent to
`extend` then `pruneToImmTipOnly`), not left as the pre-replay value while a
separate delta-log grows unboundedly under it.

## 4. `AnchoredSeq`/`Anchorable` — NO built-in chain-linkage invariant
(`ouroboros-network` @ tag `ouroboros-network-1.1.0.0`,
SHA `a98c88583fa27ac4e567095f8766216442cbb74d` — resolved from cardano-node
11.0.1's own `.cabal` bound `ouroboros-network:{api,...} ^>= 1.1`; matches
prior memory's independently-derived pin in
[[haa-outbound-connections-state-verified]]).

`ouroboros-network/api/lib/Ouroboros/Network/AnchoredSeq.hs`:
```haskell
data AnchoredSeq v a b = AnchoredSeq
  { anchor      :: !a
  , unanchorSeq :: !(StrictFingerTree (Measure v) (MeasuredWith v a b))
  }

class (Ord v, Bounded v) => Anchorable v a b | a -> v where
  asAnchor :: b -> a
  getAnchorMeasure :: Proxy b -> a -> v

pattern (:>) :: Anchorable v a b => AnchoredSeq v a b -> b -> AnchoredSeq v a b
pattern s' :> b <- (viewRight -> ConsR s' b)
  where
    AnchoredSeq a ft :> b = AnchoredSeq a (ft FT.|> MeasuredWith b)
```
`(:>)` (append) does **NOT** check that `b`'s predecessor matches the current
head — `AnchoredSeq`/`Anchorable` carry ONLY a generic ordered measure (e.g.
slot), no hash-linkage concept at all. The doc-comment on `viewLeft`
(`AnchoredSeq.hs`, near line 209) says plainly: prepending "would change the
anchor... but we have no information about the predecessor of the block we'd
be prepending" — i.e. `AnchoredSeq` is agnostic to causal linkage; it is a
purely structural (finger-tree) sequence-with-anchor container.

**The actual hash-chain invariant lives one layer up**, at
`Ouroboros.Network.AnchoredFragment` (same package/pin), for `block`-typed
fragments specifically:
```haskell
-- AnchoredFragment.hs:254-256
valid :: HasFullHeader block => AnchoredFragment block -> Bool
valid (Empty _) = True
valid (af :> b) = valid af && validExtension af b

-- AnchoredFragment.hs:321-324
validExtension af bSucc =
    blockInvariant bSucc &&
    bSucc `isValidSuccessorOf` headAnchor af
```
`isValidSuccessorOf'` (`AnchoredFragment.hs:273-318`) checks prevHash match,
strictly-increasing slot, and blockNo == tip+1 or tip (EBB case).

**Conclusion for Q2d-style questions**: `LedgerSeq` (ledger states) does NOT
get this hash-chain check from `AnchoredSeq` itself — there IS no `blockHash`
concept for a `StateRef`. Its coherence instead comes from **construction
discipline, not a runtime/type-level assertion**: `reapplyBlock`
(`LedgerSeq.hs:251-267`) always computes the new `StateRef` by applying the
next block to `currentHandle db` (the existing head of the SAME `LedgerSeq`
value being folded), and `extend` (`LedgerSeq.hs:321-327`) is the only
function that appends — so a `StateRef` can only ever reach the sequence by
having been derived, in the same call, from that sequence's own current head.
The blocks themselves are separately guaranteed to form a valid chain by
`AnchoredFragment.valid`/`isValidSuccessorOf` at the ChainDB/candidate-fragment
layer, upstream of ever reaching `initReapplyBlock`/`forkerPush`.

## 5. Rollback / fork-switch — pure indexing, never re-derived from disk
(`V2.hs:561-624`, `LedgerSeq.hs:463-517`)

`rollbackToPoint`/`rollback` (`LedgerSeq.hs:474-517`) are pure `AS.rollback`
calls — O(log n) structural split/search over the EXISTING in-memory
`AnchoredSeq`, matching by exact `Point` equality
(`(== pt) . getTip . either state state`). No re-application, no snapshot
read.

Fork-switch entry point, `openStateRefAtTarget` (`V2.hs:561-591`):
```haskell
openStateRefAtTarget ldbEnv target =
  openStateRef ldbEnv $ \l -> case target of
    Right VolatileTip -> pure $ currentHandle l
    Right ImmutableTip -> pure $ anchorHandle l
    Right (SpecificPoint pt) -> do
      let immTip = getTip $ anchor l
      case rollback pt l of
        Nothing
          | pointSlot pt < pointSlot immTip -> throwError $ PointTooOld Nothing
          | otherwise -> throwError PointNotOnChain
        Just t' -> pure $ currentHandle t'
    Left n -> case rollbackN n l of
      Nothing -> throwError $ PointTooOld $ Just ExceededRollback{..}
      Just l' -> pure $ currentHandle l'
```
If the intersection point isn't found in the retained window, this returns an
error (`PointTooOld` / `PointNotOnChain`, `Forker.hs:179-187`) — it NEVER
falls back to reconstructing a wrong/stale state. `PointTooOld` fires when the
point is strictly older than the retained anchor (the point is gone,
structurally — no attempt is made to serve it from a stale anchor); the
distinct `ExceededRollback` payload records `rollbackMaximum`/
`rollbackRequested` for diagnostics.

`newForker` (`V2.hs:640-673`) then seeds a FRESH, forker-LOCAL `LedgerSeq`:
`lseq <- newTVarIO (LedgerSeq . AS.Empty $ st)` (`V2.hs:654`) — its anchor IS
that exact retained `StateRef` (full state + its own table handle), taken
directly from the live sequence, not recomputed. New blocks extend this local
copy (`implForkerPush`, `V2_Forker.hs:111-128`, itself `extend` + duplicate-
with-diffs, no prune). On success, `implForkerCommit` (`V2_Forker.hs:130-167`)
splices the forker's local prefix back onto the shared `ldbSeq` TVar via
`AS.splitAfterMeasure` (cut the old sequence at the intersection) + `AS.join`
(append the forker's extension) — again pure `AnchoredSeq` structural ops, no
re-derivation. A `CriticalInvariantViolation` exception fires
(`V2_Forker.hs:169-175`) if the split/join ever fails to find a match — this
is the "should be impossible" assertion boundary, not a silent-degrade path.

**Verdict for Q3-style questions**: rollback within the volatile/k-window is
option (i) — pure in-memory indexing into the retained `AnchoredSeq`/
`LedgerSeq`, both for read-only rollback (`rollbackToPoint`) and for opening a
fork-switch forker (`openStateRefAtTarget` + `newForker`). There is no code
path where a "stale anchor" gets silently combined with "recent deltas" to
produce a wrong state — either the exact retained `StateRef` at the
intersection is found (guaranteed already-correct, since it was produced by
threading the SAME fold/extend discipline as every other state), or the
operation fails loudly (`PointTooOld`/`PointNotOnChain`/
`CriticalInvariantViolation`).

## Repo/pin summary for this memory
- ouroboros-consensus: tag `release-ouroboros-consensus-3.0.1.0` @
  `c87aa760001e60f0f0d3353f793eb089adb917e7` (matches cardano-node 11.0.1
  `.cabal` bound `^>= 3.0.1`; see [[praos-chain-order-v3-verified]]).
- ouroboros-network: tag `ouroboros-network-1.1.0.0` @
  `a98c88583fa27ac4e567095f8766216442cbb74d` (matches cardano-node 11.0.1
  `.cabal` bound `ouroboros-network:{api,...} ^>= 1.1`).
