---
name: lsq-acquire-forker-pins-whole-extledgerstate
description: Exact LSQ MsgAcquire/forker mechanism at release-ouroboros-consensus-3.0.1.0 (cn 11.0.1's pin) — what gets pinned, which queries touch tables vs pure state, AcquireFailure semantics, and forker/chain-selection synchronicity. Directly informs dugite #1068.
metadata:
  type: reference
---

# LSQ acquire/forker mechanism — pinned at release-ouroboros-consensus-3.0.1.0

Repo/pin: `IntersectMBO/ouroboros-consensus` tag `release-ouroboros-consensus-3.0.1.0`
@ `c87aa760001e60f0f0d3353f793eb089adb917e7` (matches cn 11.0.1's `.cabal` bound
`^>= 3.0.1`; same pin as [[ledgerdb-init-replay-rollback-anchor-mechanism-pinned]]).

## Entry point (cardano-node side, confirmed verbatim)
`ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Network/NodeToClient.hs`:
```haskell
hStateQueryServer = \reg ->
  localStateQueryServer (ExtLedgerCfg cfg) $ \target ->
    ChainDB.allocInRegistryReadOnlyForkerAtPoint getChainDB target reg
```
`allocInRegistryReadOnlyForkerAtPoint` (`ChainDB/Impl/Query.hs`) just wraps
`LedgerDB.openReadOnlyForker cdbLedgerDB` in a `ResourceRegistry` `allocate`.

## Q1: what MsgAcquire VolatileTip pins — BOTH state and tables, together
`openStateRefAtTarget` (`Storage/LedgerDB/V2.hs:561-591`):
```haskell
openStateRefAtTarget ldbEnv target =
  openStateRef ldbEnv $ \l -> case target of
    Right VolatileTip -> pure $ currentHandle l
    Right ImmutableTip -> pure $ anchorHandle l
    Right (SpecificPoint pt) -> case rollback pt l of
      Nothing | pointSlot pt < pointSlot immTip -> throwError $ PointTooOld Nothing
              | otherwise -> throwError PointNotOnChain
      Just t' -> pure $ currentHandle t'
    Left n -> case rollbackN n l of ...
```
`StateRef` (`V2/LedgerSeq.hs`):
```haskell
data StateRef m l = StateRef
  { state  :: !(l EmptyMK)             -- for Shelley-based eras: the WHOLE
                                        -- NewEpochState (pools/DReps/gov/
                                        -- pparams/stake distr/treasury/
                                        -- reserves) minus the UTxO table
  , tables :: !(LedgerTablesHandle m l) -- separate handle onto UTxO at same point
  }
```
`openStateRef` (`V2.hs:535-545`) duplicates the table handle before returning:
```haskell
openStateRef ldbEnv project =
  RAWLock.withReadAccess (ldbOpenHandlesLock ldbEnv) $ \() -> do
    tst <- project <$> atomically (getVolatileLedgerSeq ldbEnv)
    for tst $ \st -> do { tables' <- duplicate (tables st); pure st{tables = tables'} }
```
`newForker` (`V2.hs:640-673`) seeds the forker's OWN private TVar with exactly
this `StateRef`: `lseq <- newTVarIO (LedgerSeq . AS.Empty $ st)`. That private
TVar is never touched again for a read-only forker (see Q3).

**Verdict: option (b).** Both halves — NewEpochState-equivalent header AND
UTxO tables — are pinned coherently at the same instant, not independently.

## Q2: which queries touch tables vs pure state
`ReadOnlyForker` record (`Storage/LedgerDB/Forker.hs:247-258`):
```haskell
data ReadOnlyForker m l = ReadOnlyForker
  { roforkerClose           :: !(m ())
  , roforkerReadTables      :: !(LedgerTables l KeysMK -> m (LedgerTables l ValuesMK))
  , roforkerRangeReadTables :: !(RangeQueryPrevious l -> m (LedgerTables l ValuesMK, Maybe (TxIn l)))
  , roforkerGetLedgerState  :: !(STM m (l EmptyMK))
  , roforkerReadStatistics  :: !(m Statistics)
  }
```
`answerQuery` (`Ledger/Query.hs:251-277`) dispatches on a `QueryFootprint`
GADT index (`QFNoTables | QFLookupTables | QFTraverseTables`) — EVERY branch
reads only from the `forker` argument, never from `getVolatileTip`/
`getCurrentLedger`/anything live:
```haskell
answerQuery config forker query = case query of
  BlockQuery bq -> case sing of
    SQFNoTables       -> answerPureBlockQuery config bq <$> atomically (roforkerGetLedgerState forker)
    SQFLookupTables   -> answerBlockQueryLookup config bq forker
    SQFTraverseTables -> answerBlockQueryTraverse config bq forker
  GetChainBlockNo -> headerStateBlockNo . headerState <$> atomically (roforkerGetLedgerState forker)
  ...
```
Shelley footprint classification (`Shelley/Ledger/Query.hs:149-378`, grepped
every constructor): **`GetCurrentPParams`, `GetStakePools`, `GetGovState`,
`DebugNewEpochState`, `GetProposals`, `GetRatifyState`, `GetDRepState`,
`GetDRepStakeDistr`, `GetPoolDistr`/`GetPoolDistr2`,
`GetCommitteeMembersState`, `GetConstitution`, `GetFuturePParams`,
`GetStakeDistribution`, `GetAccountState` are ALL `QFNoTables`** — answered
purely from the pinned `l EmptyMK`, never touch the table handle.
Only `GetUTxOByTxIn` is `QFLookupTables` (`roforkerReadTables`, exact-key);
only `GetUTxOByAddress`/`GetUTxOWhole` are `QFTraverseTables`
(`roforkerRangeReadTables`, looped full scan — GetUTxOByAddress is NOT an
indexed lookup, UTxO-HD tables are keyed by TxIn, so it's a linear scan
filtered client-side by `filterGetUTxOByAddressOne`).

## Q3: can a session see two different points? NO.
`implForkerGetLedgerState = fmap current . readTVar . foeLedgerSeq`
(`V2/Forker.hs:97-101`) reads the forker's OWN private TVar, seeded once and
never mutated for a read-only forker — `ReadOnlyForker`'s field set
(`readOnlyForker`, `Forker.hs:269-277`) simply omits `forkerPush`/
`forkerCommit`, so nothing in the exposed API can move it. The LSQ server
state machine (`LocalStateQuery/Server.hs`) reuses the SAME `forker` value
for every `MsgQuery` until `MsgRelease`/`MsgReAcquire`/`MsgDone`. Only
`MsgReAcquire` can move the point (closes old forker, opens a fresh one).

## Q4: AcquireFailure semantics
```haskell
data GetForkerError = PointNotOnChain | PointTooOld !(Maybe ExceededRollback)
```
maps directly: `PointTooOld{} -> AcquireFailurePointTooOld`,
`PointNotOnChain -> AcquireFailurePointNotOnChain`
(`LocalStateQuery/Server.hs:54-59`). Both come ONLY from the
`Right (SpecificPoint pt)` branch above.

**VolatileTip (and ImmutableTip) can NEVER fail** — their case arms are bare
`pure $ currentHandle l` / `pure $ anchorHandle l`, no `throwError`. `for tst
$ \st -> ...` (`for` over `Either GetForkerError`) only short-circuits on
`Left`; there is no `Left` in the VolatileTip/ImmutableTip arms. The only way
`MsgAcquire VolatileTip` fails to reach `MsgAcquired` is the DB being closed
(`ClosedDBError`, an exception that tears down the connection, not a
`GetForkerError`/`MsgFailure`).

## Q5: forker vs chain-selection synchronicity — synchronous, no cadence
`implForkerCommit` (`V2/Forker.hs:130-167`, called by chain selection on
block adopt) writes the new sequence into `foeSwitchVar env` inside ONE
`atomically`/STM transaction. Field doc confirms identity:
`foeSwitchVar :: !(StrictTVar m (LedgerSeq m l)) -- ^ This TVar is the same
as the LedgerDB one`. A new forker's `openStateRef`/`getVolatileLedgerSeq`
reads that EXACT SAME TVar via a single `atomically` read (`V2.hs:520-545`).
GHC STM is linearizable: any acquire processed after a commit's transaction
completes is GUARANTEED to observe that commit's `StateRef`, not a stale
one — no polling loop, no refresh interval, no rate limit. The only extra
synchronization is `ldbOpenHandlesLock` (a `RAWLock`), taken as read-access
by `openStateRef` and write-access by GC/pruning — this can make Acquire
briefly BLOCK if GC is mid-flight, but never changes WHICH state is
captured; it only protects the table-handle-close race between the STM
snapshot and the immediately-following `duplicate` call.

## Relevance to dugite
Directly informs **#1068** (UTxO queries read live ledger while every other
LSQ query reads the pinned acquisition snapshot — CLAUDE.md "Current Focus").
Upstream's discipline to replicate: ONE `StateRef`-equivalent captured once
at acquire time, holding BOTH the NewEpochState-equivalent header AND a
detached UTxO table view coherently from the same instant, immutable for the
whole Acquire..Release session. No background refresh/polling design is
correct here — the target property is single-writer/single-reader
linearizability on a shared mutable cell (Rust analogue: read `Arc<...>`
once at acquire, hold it for the session), not a timer-based snapshot.

See also: [[ledgerdb-v2-diff-retention-and-snapshot-decoupling]],
[[ledgerdb-init-replay-rollback-anchor-mechanism-pinned]] (same V2 LedgerDB,
different angle — full state retention across the k-window / anchor
discipline during replay, not the LSQ acquire/forker path itself).
