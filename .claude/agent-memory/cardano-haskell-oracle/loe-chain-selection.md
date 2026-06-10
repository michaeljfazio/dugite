---
name: loe-chain-selection
description: Complete LoE (Limit on Eagerness) implementation in ChainDB ChainSel — type, trimToLoE algorithm, GDD governor, wiring, startup, GC non-gating. VERIFIED at tag release-ouroboros-consensus-0.30.0.1
metadata:
  type: reference
---

## Source — VERIFIED at pinned release tag

Tag: `release-ouroboros-consensus-0.30.0.1`
SHA: `96a9e1b210c092485c8ec3d676c87ac16e2b3c1f`
(Matches cardano-node 11.0.1 pinned dependency)

## Key file locations

- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/API.hs` — `LoE` type + `GetLoEFragment` type
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/ChainSel.hs` — `trimToLoE`, `initialChainSelection`, `constructPreferableCandidates`, `chainSelSync`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/Paths.hs` — `maximalCandidates` with optional k-limit
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/Background.hs` — `copyToImmutableDB`, GC scheduler (NO LoE gate)
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/Types.hs` — `cdbLoE` field in `ChainDbEnv`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/Args.hs` — `cdbsLoE` field, default = `pure LoEDisabled`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Genesis/Governor.hs` — `gddWatcher`, `evaluateGDD`, `densityDisconnect`, `sharedCandidatePrefix`
- `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Node/Genesis.hs` — `GenesisConfig`, `LoEAndGDDConfig`, `mkGenesisNodeKernelArgs`, `setGetLoEFragment`
- `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/NodeKernel.hs` — GDD watcher startup
- `cardano-node/src/Cardano/Node/Configuration/POM.hs` — JSON config key `LowLevelGenesisOptions` / `ConsensusMode`

## LoE type

```haskell
data LoE a
  = LoEDisabled
  | LoEEnabled !a
  deriving (Eq, Show, Generic, NoThunks, Functor, Foldable, Traversable)

type GetLoEFragment m blk = m (LoE (AnchoredFragment (HeaderWithTime blk)))
```

## trimToLoE — exact algorithm

Applied in `constructPreferableCandidates`, AFTER diffs are built, BEFORE the `preferAnchoredCandidate` filter.

```haskell
trimToLoE :: LoE (AnchoredFragment blk') -> ChainDiff (Header blk) -> ChainDiff (Header blk)
trimToLoE LoEDisabled diff = diff
trimToLoE (LoEEnabled loe) diff =
  case Diff.apply curChain diff of
    Nothing -> error "trimToLoE: precondition 1 violated"
    Just cand ->
      case AF.intersect cand loe of
        Nothing -> error "trimToLoE: precondition 2 violated"
        Just (candPrefix, _, candSuffix, loeSuffix) ->
          let trimmedCandSuffix = AF.takeOldest (fromIntegral k) candSuffix
              trimmedCand =
                if AF.null loeSuffix
                  then fromJust $ AF.join candPrefix trimmedCandSuffix
                  else candPrefix
           in Diff.diff curChain trimmedCand
```

**Three outcomes:**
1. `AF.null loeSuffix` (LoE tip is on candidate chain): keep `candPrefix ++ takeOldest(k) candSuffix`
2. `not (AF.null loeSuffix)` (LoE tip extends beyond candidate): trim to `candPrefix` only (the common prefix up to the LoE fork point — which is the intersection, so candidate = full prefix up to intersection)
3. `LoEDisabled`: no trimming

**`maxExtra` / `k`**: `k = configSecurityParam`'s security parameter (2160 on mainnet). Exactly `AF.takeOldest (fromIntegral k) candSuffix` — at most k blocks past the LoE tip.

## sanitizeLoEFrag

Before trimToLoE is called, the LoE fragment is sanitized:
```haskell
sanitizeLoEFrag loeFrag0 =
  case AF.splitAfterPoint loeFrag0 (AF.anchorPoint curChain) of
    Just (_, frag) -> frag
    Nothing -> AF.Empty $ AF.castAnchor $ AF.anchor curChain
```
If the LoE fragment doesn't intersect the current chain (temporary), use the empty fragment anchored at the immutable tip → full selection freeze.

## Where trimToLoE is applied

ONLY in `constructPreferableCandidates`. This function is called from:
1. `chainSelectionForBlock` (every new block via `chainSelSync ChainSelAddBlock`)
2. `chainSelSync ChainSelReprocessLoEBlocks` (LoE unblocking)

NOT applied in `chainSelection` or `validateCandidate` — just trimmed diffs go in.

## initial chain selection (startup)

`initialChainSelection` uses `maximalCandidates` with a k-limit:
```haskell
suffixesAfterI = Paths.maximalCandidates succsOf (unNonZero <$> limit) (AF.anchorToPoint i)
 where
  limit = case loE of
    LoEDisabled -> Nothing
    LoEEnabled () -> Just k
```
When LoE is enabled, VolatileDB candidate chains are capped at k blocks from immutable tip. The `loE` parameter is `void initialLoE` (unit, not the live fragment) — it only tests enabled/disabled, doesn't use the actual fragment value.

## copyToImmutableDB — NOT gated by LoE

`copyToImmutableDB` in `Background.hs` only checks:
```
nbToCopy = max 0 $ AF.length curChain - AF.length curChainVolSuffix
```
`curChain` = full chain (>k blocks), `curChainVolSuffix` = volatile portion (≤k blocks). The k-deep flush is purely structural; the LoE fragment is NOT consulted. VolatileDB GC is also purely slot-based (no LoE gate).

## GDD governor — who computes/updates varLoEFrag

In `NodeKernel`:
1. `varLoEFragment` TVar allocated, initially `AF.Empty AF.AnchorGenesis`
2. `setGetLoEFragment (readTVar varGsmState) (readTVar varLoEFragment) lgnkaLoEFragmentTVar` wires up the GSM-aware getter
3. `forkLinkedWatcher "NodeKernel.GDD" (gddWatcher cfg tracer chainDB rateLimit readGsmState handles varLoEFragment)` starts the background watcher

`gddWatcher` is a `Watcher` that fires when `GsmState` changes OR when any peer's `csLatestSlot`/`csIdling` changes (fingerprint is `Map peer (StrictMaybe (WithOrigin SlotNo), Bool)`).

`wNotify`:
- `GDDPreSyncing`: no-op
- `GDDCaughtUp`: call `triggerChainSelectionAsync` (let LoE-postponed blocks be selected)
- `GDDSyncing stateView`: call `evaluateGDD` → compute `loeFrag` via `sharedCandidatePrefix` → `swapTVar varLoEFrag loeFrag` → if head hash changed → `triggerChainSelectionAsync`; then `threadDelay (rateLimit - elapsed)` (default 1.0s)

## sharedCandidatePrefix

Computes the LoE fragment = intersection of current chain with ALL candidate fragments, anchored at immutable tip:
```haskell
sharedCandidatePrefix curChain candidates =
  second getCompose $
    stripCommonPrefix (AF.castAnchor $ AF.anchor curChain) $
      Compose immutableTipSuffixes
```
Each candidate is split at the immutable tip; if no intersection, treated as empty (anchored at immutable tip). The LoE fragment ends at the EARLIEST intersection across all peers.

## setGetLoEFragment — GSM-gated behaviour

```haskell
getLoEFragment = atomically $ readGsmState >>= \case
  GSM.PreSyncing ->
    pure $ ChainDB.LoEEnabled $ AF.Empty AF.AnchorGenesis  -- most conservative
  GSM.Syncing ->
    ChainDB.LoEEnabled <$> readLoEFragment               -- live GDD fragment
  GSM.CaughtUp ->
    pure ChainDB.LoEDisabled                               -- LoE disabled
```

**At-origin / empty fragment behaviour**: When `LoEEnabled (AF.Empty AF.AnchorGenesis)` is returned:
- `sanitizeLoEFrag` with `splitAfterPoint loeFrag0 (anchorPoint curChain)`:
  - If curChain is anchored at the immutable tip, splitAfterPoint of an empty-at-genesis frag at immutable tip fails → `Nothing` branch → returns `AF.Empty (castAnchor (anchor curChain))`
  - The sanitized LoE frag = empty, anchored at immutable tip
- `AF.intersect cand (AF.Empty (immutableTipAnchor))`:
  - The AF.Empty anchor IS an intersection point
  - `candPrefix` = fragment from immutable tip to immutable tip (empty)
  - `candSuffix` = the entire candidate chain
  - `loeSuffix` = empty (the LoE is empty, nothing beyond intersection)
- `AF.null loeSuffix = True` → `trimmedCand = candPrefix ++ takeOldest(k, candSuffix)`
- **Selection is NOT completely frozen**. Up to k blocks from immutable tip can be adopted.
- Selection freezes only PAST k blocks from immutable tip (the k-limit bites).

**CORRECTION from old memory**: The claim "selection is frozen" was WRONG. Empty LoE → `loeSuffix` is null → case 1 (not case 2) → k blocks allowed. Full freeze only if `loeSuffix` is non-null (candidate diverges BEFORE LoE tip).

## LoEDisabled behaviour (Praos mode)

`trimToLoE LoEDisabled diff = diff` — identity, no trimming. `initialChainSelection` uses `Nothing` size limit → full VolatileDB traversal. GDD watcher never started (`LoEAndGDDDisabled -> pure ()`).

## Configuration (cardano-node)

JSON config key `ConsensusMode`:
- `"GenesisMode"` → `mkGenesisConfig (Just flags)` → `LoEAndGDDEnabled LoEAndGDDParams{lgpGDDRateLimit}`
- `"PraosMode"` → `mkGenesisConfig Nothing` → all Genesis components disabled

`LowLevelGenesisOptions` JSON key maps to `GenesisConfigFlags`:
- `gcfEnableLoEAndGDD` (default `True`) — whether LoE+GDD is enabled within GenesisMode
- `gcfGDDRateLimit` (default `1.0` seconds) — minimum interval between GDD evaluations

`defaultConsensusMode` (from `cardano-network`) is `GenesisMode` as of cardano-node 10.5+/11.x.

## VolatileDB GC — unaffected by LoE

GC is scheduled by `scheduleGC` from `copyToImmutableDBRunner` after `copyToImmutableDB`. The trigger is `AF.length curChain > AF.length curChainVolSuffix` (purely k-depth structural). LoE fragment is never consulted for GC scheduling or execution.
