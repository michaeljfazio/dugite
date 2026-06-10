---
name: gdd-governor-deep-dive
description: EXHAUSTIVE GDD governor reference for cardano-node 11.0.1 / ouroboros-consensus 3.0.1: gddWatcher internals, Watcher fingerprint, sharedCandidatePrefix, all 4 densityDisconnect guards verbatim, GenesisWindow source (3k/f via computeStabilityWindow), DensityTooLow throwTo kill, LoE trimToLoE +k, CSJ jumper fragment visibility, GSM gating, startup wiring, all defaults
metadata:
  type: reference
---

# GDD Governor Deep Dive

Source: `IntersectMBO/ouroboros-consensus` tag `release-ouroboros-consensus-3.0.1.0` (commit `c87aa760001e`, ships with cardano-node 11.0.1 via `^>= 3.0.1` constraint). File is byte-identical at `release-ouroboros-consensus-diffusion-0.24.0.0` (sha `effb0a5c924f`).

## Primary File

`ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Genesis/Governor.hs`

## Module Exports

```haskell
module Ouroboros.Consensus.Genesis.Governor
  ( DensityBounds (..)
  , GDDDebugInfo (..)
  , GDDStateView (..)
  , TraceGDDEvent (..)
  , densityDisconnect
  , gddWatcher
  , sharedCandidatePrefix
  )
```

---

## Key Types

### ChainSyncState (State.hs)

```haskell
data ChainSyncState blk = ChainSyncState
  { csCandidate :: !(AnchoredFragment (HeaderWithTime blk))
  -- ^ Current candidate fragment
  , csIdling :: !Bool
  -- ^ True when peer's last msg was MsgAwaitReply (no more headers currently)
  , csLatestSlot :: !(StrictMaybe (WithOrigin SlotNo))
  -- ^ Slot of latest received header (may be beyond forecast horizon,
  --   before the fragment is extended)
  }
```

Initial state: `ChainSyncState { csCandidate = AF.Empty AF.AnchorGenesis, csLatestSlot = SNothing, csIdling = False }`

### ChainSyncClientHandle (State.hs)

```haskell
data ChainSyncClientHandle m blk = ChainSyncClientHandle
  { cschGDDKill :: !(m ())
  -- ^ Fires throwTo tid DensityTooLow on the ChainSync client thread
  , cschOnGsmStateChanged :: !(GsmState -> Time -> STM m ())
  , cschState :: !(StrictTVar m (ChainSyncState blk))
  , cschJumping :: !(StrictTVar m (ChainSyncJumpingState m blk))
  , cschJumpInfo :: !(StrictTVar m (Maybe (JumpInfo blk)))
  }
```

### GDDStateView

```haskell
data GDDStateView m blk peer = GDDStateView
  { gddCtxCurChain       :: AnchoredFragment (HeaderWithTime blk)
  , gddCtxImmutableLedgerSt :: ExtLedgerState blk EmptyMK
  , gddCtxKillActions    :: Map peer (m ())
  , gddCtxStates         :: Map peer (ChainSyncState blk)
  }
```

### DensityBounds

```haskell
data DensityBounds blk = DensityBounds
  { clippedFragment :: AnchoredFragment (Header blk)
  -- ^ Candidate suffix clipped to the genesis window [loeHead, loeHead+sgen)
  , offersMoreThanK :: Bool
  -- ^ totalBlockCount = AF.length candidateSuffix > k
  , lowerBound :: Word64
  -- ^ AF.length clippedFragment (certain blocks within genesis window)
  , upperBound :: Word64
  -- ^ lowerBound + potentialSlots
  --   potentialSlots=0 if hasBlockAfter, else firstSlotAfterGenesisWindow - AF.headSlot(clipped) - 1
  , hasBlockAfter :: Bool
  -- ^ max(AF.headSlot candidateSuffix, latestSlot) >= NotOrigin firstSlotAfterGenesisWindow
  , latestSlot :: WithOrigin SlotNo
  -- ^ csLatestSlot from ChainSyncState (unwrapped from StrictMaybe)
  , idling :: Bool
  -- ^ csIdling from ChainSyncState
  }
```

### GsmState

```haskell
data GsmState = PreSyncing | Syncing | CaughtUp
```

### GDDTrigger

```haskell
data GDDTrigger a = GDDPreSyncing | GDDSyncing a | GDDCaughtUp
  deriving (Eq)
```

---

## GenesisWindow: Where sgen Comes From

### Type

```haskell
newtype GenesisWindow = GenesisWindow { unGenesisWindow :: Word64 }
```

It lives in `EraParams.eraGenesisWin :: !GenesisWindow` (per-era HardFork parameter).

### Shelley+ formula (from `Shelley/Ledger/Ledger.hs`)

```haskell
shelleyEraParams genesis = HardFork.EraParams
  { ...
  , eraGenesisWin = GenesisWindow stabilityWindow
  }
 where
  stabilityWindow = SL.computeStabilityWindow
    (unNonZero $ SL.sgSecurityParam genesis)
    (SL.sgActiveSlotCoeff genesis)
```

`computeStabilityWindow` (from `cardano-ledger/eras/shelley/impl/src/Cardano/Ledger/Shelley/StabilityWindow.hs`):

```haskell
computeStabilityWindow :: Word64 -> ActiveSlotCoeff -> Word64
computeStabilityWindow k asc =
  ceiling $ (3 * fromIntegral k) /. f
 where
  f = positiveUnitIntervalNonZeroRational . activeSlotVal $ asc
```

**Result: sgen = ceiling(3k/f) slots**

The same genesis window is used for ALL Shelley-based eras (Shelley, Allegra, Mary, Alonzo, Babbage, Conway, Dijkstra) — they all call `shelleyEraParams` with the same `ShelleyGenesis`.

The default for a non-Cardano chain is `eraGenesisWin = GenesisWindow (unNonZero k * 2)` (2k slots).

### Runtime lookup

In `evaluateGDD`:
```haskell
msgen :: Maybe GenesisWindow
msgen = eitherToMaybe $ runQuery qry summary
 where
  slot = succWithOrigin $ AF.headSlot loeFrag
  qry = qryFromExpr $ slotToGenesisWindow slot
  summary = hardForkSummary (configLedger cfg) (ledgerState immutableLedgerSt)
```

The slot queried is the **first slot after the LoE tip** (so if the LoE tip is in the last slot of an era, the query returns the next era's window). Uses the immutable ledger state (not current chain tip) to guard against the summary being past the horizon.

Returns `Nothing` if past the horizon → GDD skips the evaluation silently.

---

## gddWatcher: STM Watcher

```haskell
gddWatcher ::
  TopLevelConfig blk ->
  Tracer m (TraceGDDEvent peer blk) ->
  ChainDB m blk ->
  DiffTime ->          -- rateLimit (default 1.0 seconds)
  STM m GsmState ->
  STM m (Map peer (ChainSyncClientHandle m blk)) ->
  StrictTVar m (AnchoredFragment (HeaderWithTime blk)) ->
  Watcher m (GDDTrigger (GDDStateView m blk peer))
             (GDDTrigger (Map peer (StrictMaybe (WithOrigin SlotNo), Bool)))
```

### Initial state

`wInitial = Nothing` — the watcher fires once immediately on startup to establish the initial fingerprint.

### What triggers re-evaluation (`wFingerprint`)

In `Syncing` state, the fingerprint is:
```haskell
Map.map (\css -> (csLatestSlot css, csIdling css)) gddCtxStates
```

The watcher wakes up when **any peer's `csLatestSlot` or `csIdling` changes**. This means:
- A new header arrives (the ChainSync client updates `csLatestSlot` BEFORE extending the fragment)
- A peer goes idle (sends MsgAwaitReply) or becomes un-idle
- The GSM state changes (due to the outer GDDTrigger comparison)

`csCandidate` itself does NOT appear in the fingerprint — only `csLatestSlot` and `csIdling`. This is intentional: there can be a brief delay between `csLatestSlot` updating and the fragment extending, and the watcher is guaranteed to wake again.

### Per-GsmState behavior

| GsmState | wNotify behavior |
|---|---|
| PreSyncing | `pure ()` — GDD does nothing; HAA not satisfied |
| CaughtUp | `triggerChainSelectionAsync` — unblock any LoE-postponed blocks; no GDD eval |
| Syncing | run full GDD eval + rate-limit sleep |

### Rate limit

```haskell
wNotify (GDDSyncing stateView) = do
  t0 <- getMonotonicTime
  loeFrag <- evaluateGDD cfg tracer stateView
  oldLoEFrag <- atomically $ swapTVar varLoEFrag loeFrag
  when (AF.headHash oldLoEFrag /= AF.headHash loeFrag) $
    void $ ChainDB.triggerChainSelectionAsync chainDB
  tf <- getMonotonicTime
  threadDelay $ rateLimit - diffTime tf t0
```

Default `rateLimit = 1.0` second (`defaultGDDRateLimit` in `Node/Genesis.hs`). Configured via `gcfGDDRateLimit` / `lgpGDDRateLimit`. The delay is `max(0, rateLimit - elapsed)` — if evaluation takes longer than rateLimit, the next wakeup happens immediately.

---

## sharedCandidatePrefix

```haskell
sharedCandidatePrefix ::
  AnchoredFragment (HeaderWithTime blk) ->   -- curChain (anchor = immutable tip)
  [(peer, AnchoredFragment (HeaderWithTime blk))] ->   -- all candidates
  ( AnchoredFragment (HeaderWithTime blk)   -- loeFrag
  , [(peer, AnchoredFragment (HeaderWithTime blk))]  -- candidateSuffixes (after loeFrag)
  )
sharedCandidatePrefix curChain candidates =
  second getCompose $
    stripCommonPrefix (AF.castAnchor $ AF.anchor curChain) $
      Compose immutableTipSuffixes
 where
  immutableTip = AF.anchorPoint curChain

  splitAfterImmutableTip (peer, frag) =
    case AF.splitAfterPoint frag immutableTip of
      Nothing -> (peer, AF.takeOldest 0 curChain)   -- empty, anchored at immutable tip
      Just (_, suffix) -> (peer, suffix)

  immutableTipSuffixes = map splitAfterImmutableTip candidates
```

`stripCommonPrefix` computes the longest common prefix of all fragments (anchored at the immutable tip), via pairwise `AF.intersect`. The result is the LoE fragment.

**The immutable tip is the anchor of `curChain`** — typically k blocks behind the current selection tip.

### LoE tip semantics

The LoE tip is the youngest header present on ALL candidate fragments. ChainSel is constrained to not select beyond LoE tip + k blocks (trimToLoE in ChainSel.hs).

### Handling peers without candidates

If `AF.splitAfterPoint frag immutableTip = Nothing` (fragment does not include the immutable tip), the peer's contribution is treated as an **empty fragment anchored at the immutable tip**. This is the CSJ case (see Note [CSJ truncates the candidate fragments]):

- CSJ can cause a peer's fragment to recede without a rollback (jump back)
- GDD treats such peers as "offering no blocks after the intersection"
- The LoE is not moved back; it is the ChainSync client's responsibility to update the fragment or disconnect

**Note**: If there are zero candidates, `stripCommonPrefix` returns `AF.Empty sharedAnchor` — LoE = empty fragment at immutable tip.

---

## densityDisconnect: The Full Algorithm

```haskell
densityDisconnect ::
  ( Ord peer, LedgerSupportsProtocol blk ) =>
  GenesisWindow ->
  SecurityParam ->
  Map peer (ChainSyncState blk) ->
  [(peer, AnchoredFragment (HeaderWithTime blk))] ->
  AnchoredFragment (HeaderWithTime blk) ->
  ([peer], [(peer, DensityBounds blk)])
densityDisconnect (GenesisWindow sgen) (SecurityParam k) states candidateSuffixes loeFrag =
  (losingPeers, densityBounds)
```

### Step 1: Compute loeIntersectionSlot and firstSlotAfterGenesisWindow

```haskell
loeIntersectionSlot = AF.headSlot loeFrag
firstSlotAfterGenesisWindow = succWithOrigin loeIntersectionSlot + SlotNo sgen
```

So if LoE tip is at slot S, the genesis window covers slots `[S+1, S+sgen]` (sgen slots).

### Step 2: Per-peer DensityBounds (densityBounds list comprehension)

For each `(peer, candidateSuffix)` in `candidateSuffixes`:

```haskell
-- clip to genesis window
(clippedFragment, _) = AF.splitAtSlot firstSlotAfterGenesisWindow candidateSuffix

-- peer must have sent at least one header (csLatestSlot must be SJust)
-- peers with csLatestSlot=SNothing are SKIPPED (handled by timeouts instead)
state <- maybeToList (states Map.!? peer)
latestSlot <- toList (csLatestSlot state)   -- SNothing → skip

idling = csIdling state

hasBlockAfter =
  max (AF.headSlot candidateSuffix) latestSlot
    >= NotOrigin firstSlotAfterGenesisWindow

potentialSlots =
  if hasBlockAfter
    then 0
    else unknownTrailingSlots

unknownTrailingSlots =
  unSlotNo $
    firstSlotAfterGenesisWindow - succWithOrigin (AF.headSlot clippedFragment)

lowerBound = fromIntegral $ AF.length clippedFragment

upperBound = lowerBound + potentialSlots

totalBlockCount = fromIntegral (AF.length candidateSuffix)
offersMoreThanK = totalBlockCount > unNonZero k
```

Peers with `csLatestSlot = SNothing` (never sent any header) are excluded from `densityBounds` entirely.

### Step 3: Identify losingPeers (four guards, all must pass)

For each `peer0` (the candidate for disconnection) and for each `peer1` (the comparator), peer0 is added to losingPeers if ALL four guards hold:

**Guard 1** — peer0 has committed to a chain:
```haskell
guard $ idling0 || not (AF.null frag0) || hasBlockAfter0
```
Do NOT disconnect peer0 if: it is not idling AND it has sent no headers AND it hasn't signalled blocks beyond the window. Wait until it declares idle or sends a header.

**Guard 2** — peer0 and peer1 disagree after the intersection:
```haskell
guard $ AF.lastPoint frag0 /= AF.lastPoint frag1
```
Do not disconnect peer0 based on peer1 if they agree on the same last block within the genesis window.

**Guard 3** — peer1 is a credible comparator:
```haskell
guard $ offersMoreThanK || lb0 == ub0
```
Either peer1 offers more than k total blocks (so it is a potential honest chain), OR peer0 has shown all its genesis-window blocks (lb0==ub0, no unknowns). This prevents disconnecting competing honest peers when nearly caught up.

**Guard 4** — peer1's density is at least as good as peer0's upper bound:
```haskell
guard $ lb1 >= (if idling0 then lb0 else ub0)
```
If peer0 is idling (no more headers), compare lb1 ≥ lb0 (peer1 as dense as peer0 at minimum). If peer0 is not idling, compare lb1 ≥ ub0 (peer1 beats peer0's optimistic upper bound). "As good" suffices to disconnect: the honest chain is expected strictly denser.

### Result deduplication

```haskell
losingPeers = nubOrd $ densityBounds >>= \(peer0, ...) -> do ...
```

`nubOrd` ensures each peer appears at most once even if multiple peer1 comparators trigger it.

---

## Kill Mechanism

When `losingPeers` is non-empty:

```haskell
for_ losingPeersNE $ \peer -> killActions Map.! peer
```

`killActions` = `Map.map cschGDDKill handles`. Each entry is:

```haskell
cschGDDKill = throwTo tid DensityTooLow
```

Where `DensityTooLow` is a constructor of `ChainSyncClientException`:

```haskell
data ChainSyncClientException
  = ...
  | DensityTooLow

instance Exception ChainSyncClientException
```

The `throwTo tid` delivers an async exception to the ChainSync client thread, which terminates it. The ChainSync client's bracket/cleanup then removes the peer's handle from `varChainSyncHandles`. This triggers the watcher again (csLatestSlot map changes), causing GDD to re-evaluate with updated candidates.

---

## LoE Fragment Handoff to ChainSel

### 1. The TVar

In NodeKernel (at startup):
```haskell
varLoEFragment <- newTVarIO $ AF.Empty AF.AnchorGenesis
```
Initial value: empty fragment anchored at genesis.

### 2. The GetLoEFragment function

`setGetLoEFragment` installs a dynamic function:
```haskell
getLoEFragment :: ChainDB.GetLoEFragment m blk
getLoEFragment = atomically $ readGsmState >>= \case
  GSM.PreSyncing -> pure $ ChainDB.LoEEnabled $ AF.Empty AF.AnchorGenesis
  GSM.Syncing    -> ChainDB.LoEEnabled <$> readLoEFragment
  GSM.CaughtUp   -> pure ChainDB.LoEDisabled
```

- PreSyncing: most conservative (empty @ genesis → no blocks can be selected past genesis)
- Syncing: uses the actual GDD-computed LoE fragment
- CaughtUp: LoE disabled (normal Praos selection)

### 3. LoE enforcement in ChainSel (ChainSel.hs `trimToLoE`)

```haskell
trimToLoE (LoEEnabled loe) diff =
  case AF.intersect cand loe of
    Nothing -> error "precondition violated"
    Just (candPrefix, _, candSuffix, loeSuffix) ->
      let trimmedCandSuffix = AF.takeOldest (fromIntegral k) candSuffix
          trimmedCand =
            if AF.null loeSuffix
              then fromJust $ AF.join candPrefix trimmedCandSuffix
              else candPrefix
       in Diff.diff curChain trimmedCand
```

- If the candidate does not contain the LoE tip (LoE is ahead of candidate): candidate is trimmed to the candPrefix portion (no more than the LoE allows)
- If the candidate contains the LoE tip: candidate is trimmed to LoE tip + up to k blocks

**ChainSel allows at most k blocks beyond the LoE tip.**

### 4. triggerChainSelectionAsync

GDD calls this when `AF.headHash oldLoEFrag /= AF.headHash loeFrag`. This wakes the ChainDB background thread which reprocesses LoE-postponed blocks (`ChainSelReprocessLoEBlocks`). The reprocess logic fetches all direct successor blocks from VolatileDB and runs chainSelection on them.

---

## GSM State Gating: When is GDD Active?

| GsmState | GDD evaluation | LoE fragment used |
|---|---|---|
| PreSyncing | NOT evaluated | Empty @ genesis (most conservative) |
| Syncing | EVALUATED (full densityDisconnect) | GDD-computed loeFrag |
| CaughtUp | NOT evaluated | LoEDisabled (selection unconstrained) |

The GSM state change causes `wFingerprint` to return a different `GDDTrigger` variant, which differs from the cached fingerprint, waking the watcher.

---

## CSJ (ChainSync Jumping) Relationship

### Fragment visibility to GDD

ALL registered peers — Dynamo, Objector, Jumpers, and Disengaged — have a `ChainSyncClientHandle` in `varChainSyncHandles` with a `cschState` TVar. GDD reads `cschcMap varChainSyncHandles` and traverses all states. There is no filtering by CSJ role.

### What candidates Jumpers contribute

Jumpers do NOT receive new headers via ChainSync (only Dynamo and Objector do). A jumper's `csCandidate` is updated only when it accepts a jump:

```haskell
-- In Jumping.hs:updateChainSyncState
csState{csCandidate = fragment, csLatestSlot = SJust (AF.headSlot fragment)}
```

Where `fragment = jTheirFragment jump` (the dynamo's candidate fragment up to the jump point).

**Therefore jumpers contribute a candidate fragment anchored at the LoE tip and extending to the jump point** — typically the end of the CSJ jump window (default 2*2160 = 4320 slots = 1 Byron forecast range).

A jumper that has never accepted a jump has `csCandidate = AF.Empty AF.AnchorGenesis` (initial state). In `sharedCandidatePrefix`, this yields a split failure against the current immutable tip → treated as empty fragment at immutable tip (Note [CSJ truncates the candidate fragments]).

### LoE and CSJ interaction

GDD's comment in `densityDisconnect`:
> "ChainSync jumping depends on this function to disconnect either of any two peers that offer different chains and provided a header in the last slot of the genesis window or later. Either of them should be disconnected, even if both of them are serving adversarial chains."

This means guard 4 (`lb1 >= lb0` when idling) is intentionally designed to disconnect one of two competing dynamo/objector pairs when they have equal density — you always drop the "peer0" (first encountered in the list) in that case.

### Disengaged peers

Disengaged peers have their CSJ state set to `Disengaged`. Their `csCandidate` continues to be downloaded normally (headers continue to arrive). They are visible to GDD and participate in density comparison.

---

## Startup Wiring (NodeKernel.hs)

```haskell
case gnkaLoEAndGDDArgs genesisArgs of
  LoEAndGDDDisabled -> pure ()
  LoEAndGDDEnabled lgArgs -> do
    varLoEFragment <- newTVarIO $ AF.Empty AF.AnchorGenesis
    setGetLoEFragment
      (readTVar varGsmState)
      (readTVar varLoEFragment)
      (lgnkaLoEFragmentTVar lgArgs)
    void $
      forkLinkedWatcher registry "NodeKernel.GDD" $
        gddWatcher
          cfg
          (gddTracer tracers)
          chainDB
          (lgnkaGDDRateLimit lgArgs)
          (readTVar varGsmState)
          (cschcMap varChainSyncHandles)
          varLoEFragment
```

- GDD runs as a `forkLinkedWatcher` — linked to the NodeKernel resource registry. If it crashes, the node crashes.
- `cschcMap varChainSyncHandles` = `STM m (Map peer (ChainSyncClientHandle m blk))` = all currently connected peers
- The initial LoE fragment is `AF.Empty AF.AnchorGenesis` (most conservative)
- `lgnkaGDDRateLimit` = `defaultGDDRateLimit = 1.0` second unless overridden

## Configuration Defaults

From `Node/Genesis.hs`:
```haskell
defaultGDDRateLimit     = 1.0   -- seconds
defaultBlockFetchGracePeriod = 10  -- seconds
defaultCapacity         = 100_000  -- LoP tokens
defaultRate             = 500      -- tokens/second
defaultCSJJumpSize      = 2 * 2160  -- Byron forecast range slots
```

`GenesisConfig` sets `gcfEnableLoEAndGDD = True` by default.

---

## Rust Translation Notes

1. **GenesisWindow is per-era, per-slot query**: Use the HardFork history interpreter to resolve the genesis window for the slot `loeFrag_head + 1`. Return `None` if past horizon (skip GDD evaluation).

2. **Fingerprint-based wake**: The Haskell `Watcher` wakes only when `(csLatestSlot, csIdling)` changes for ANY peer. In Rust, use a tokio `watch` channel or similar on each peer's state, or poll the entire map under a `Mutex` with a generation counter.

3. **Rate limit is a sleep AFTER evaluation**: The `threadDelay` call at the end means GDD can run faster than 1s if `threadDelay` returns early (it won't), but the sleep is `max(0, 1.0 - elapsed)`. Don't implement it as a fixed-interval timer.

4. **csLatestSlot=SNothing → excluded entirely**: Peers who have never sent a header are excluded from `densityBounds` (the `toList (csLatestSlot state)` guard). They should be handled by the LoP (Limit on Patience) timeout, not GDD.

5. **offersMoreThanK uses candidateSuffix length (ALL blocks), not clippedFragment**: `totalBlockCount = AF.length candidateSuffix`. Only blocks AFTER the LoE tip count.

6. **Guard 3 short-circuit**: If `offersMoreThanK` is false AND `lb0 < ub0` (peer0 still has unknown slots), peer0 is NOT disconnected regardless of peer1's density. This is important for nearly-caught-up scenarios.

7. **Guard 4 asymmetry**: Compare lb1 ≥ ub0 when NOT idling (strict upper bound of peer0), lb1 ≥ lb0 when idling (peer0 has declared no more headers, use actual count). Equal density is sufficient to disconnect.

8. **Kill is async exception to ChainSync thread**: In Rust, send on a disconnect channel that the ChainSync task listens on.

9. **LoE enforces k-block limit beyond LoE tip**: `AF.takeOldest k candSuffix` — candidate extension beyond LoE tip is capped at k blocks.

10. **CSJ jumpers have truncated fragments**: A jumper's csCandidate is only as long as the most recently accepted jump. Fresh jumpers have empty fragments at AnchorGenesis → treated as empty at immutable tip by sharedCandidatePrefix.
