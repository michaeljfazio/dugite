---
name: praos-chain-order-v3-verified
description: Exact Praos ChainOrder/comparePraos/VRFTiebreakerFlavor algorithm, RE-VERIFIED at the ouroboros-consensus tag that actually matches cardano-node 11.0.1 (3.0.1.0, not 0.30.0.1). Supersedes the ShouldSwitch/reason details in fork-resolution-chainsel.md.
metadata:
  type: reference
---

# Praos chain order — verified at the CORRECT pin for cardano-node 11.0.1

## Critical methodology correction

`loe-chain-selection.md` claimed tag `release-ouroboros-consensus-0.30.0.1`
(SHA `96a9e1b2...`) "matches cardano-node 11.0.1 pinned dependency" — **this
is wrong**. cardano-node has no `source-repository-package` pin for
ouroboros-consensus; it depends on it purely via CHaP (Cardano Haskell
Packages) with a version bound in `cardano-node/cardano-node.cabal`:

```
ouroboros-consensus:{ouroboros-consensus, lmdb, lsm, cardano, diffusion, protocol} ^>= 3.0.1
```

(verified directly: `curl .../cardano-node/11.0.1/cardano-node/cardano-node.cabal`)

**The correct git tag is `release-ouroboros-consensus-3.0.1.0`**, SHA
`c87aa760001e60f0f0d3353f793eb089adb917e7` (resolved via
`gh api repos/IntersectMBO/ouroboros-consensus/git/refs/tags/<tag>`).

**Lesson: to pin any ouroboros-consensus (or ouroboros-network) lookup to a
specific cardano-node release, always read that release's `.cabal` file's
version bound first (`grep ouroboros-consensus *.cabal`), then resolve the
matching `release-ouroboros-consensus-X.Y.Z.W` tag via the GitHub tags API —
never assume a previously-recorded tag/SHA still applies. The monorepo moved
from per-package versions (`0.30.0.1`) to one shared version number across
all its packages (`3.0.1`) at some point between those two pins, and the
`ChainOrder`/`preferCandidate` API itself changed shape in between (see
below) — a stale pin gives you STRUCTURALLY WRONG code, not just missing
newer fields.**

## API shape change: Bool -> ShouldSwitch(reason)

At `0.30.0.1`, `preferCandidate :: ChainOrderConfig sv -> sv -> sv -> Bool`.

At `3.0.1.0` (matching cardano-node 11.0.1), the signature carries a reason:

```haskell
-- Ouroboros/Consensus/Protocol/Abstract.hs
class Ord sv => ChainOrder sv where
  type ChainOrderConfig sv :: Type
  type ReasonForSwitch sv :: Type
  preferCandidate ::
    ChainOrderConfig sv -> sv {- ours -} -> sv {- cand -} -> ShouldSwitch (ReasonForSwitch sv)

data ShouldSwitch reason = ShouldNotSwitch Ordering | ShouldSwitch reason

shouldSwitch :: ShouldSwitch reason -> Bool
shouldSwitch ShouldSwitch{} = True
shouldSwitch ShouldNotSwitch{} = False
```

`SelectView`'s instance (`svBlockNo` first, `svTiebreakerView` breaks ties):

```haskell
data SelectViewReasonForSwitch p
  = Longer (Comparing (WithOrigin BlockNo))
  | SelectViewTiebreak (ReasonForSwitch (TiebreakerView p))

instance ChainOrder (TiebreakerView p) => ChainOrder (SelectView p) where
  preferCandidate cfg ours cand = case compare (svBlockNo ours) (svBlockNo cand) of
    LT -> ShouldSwitch (Longer (Comparing (At (svBlockNo ours)) (At (svBlockNo cand))))
    EQ -> case preferCandidate cfg (svTiebreakerView ours) (svTiebreakerView cand) of
      ShouldSwitch r -> ShouldSwitch (SelectViewTiebreak r)
      ShouldNotSwitch e -> ShouldNotSwitch e
    GT -> ShouldNotSwitch GT
```

Praos's reason type and `comparePraos` (`ouroboros-consensus-protocol/.../Protocol/Praos/Common.hs`):

```haskell
data PraosReasonForSwitch c
  = HigherOCert (Comparing Word64)
  | VRFTiebreak (Comparing (OutputVRF (VRF c)))

comparePraos :: VRFTiebreakerFlavor -> PraosTiebreakerView c -> PraosTiebreakerView c
             -> ShouldSwitch (PraosReasonForSwitch c)
comparePraos tiebreakerFlavor ours cand =
  case ( issueNoArmed ours cand
       , (compare `on` ptvIssueNo) ours cand
       , vrfArmed ours cand
       , (compare `on` Down . ptvTieBreakVRF) ours cand
       ) of
    (True, LT, _, _) -> ShouldSwitch (HigherOCert (Comparing (ptvIssueNo ours) (ptvIssueNo cand)))
    (True, GT, _, _) -> ShouldNotSwitch GT
    (True, EQ, True, GT) -> ShouldNotSwitch GT
    (True, EQ, True, EQ) -> ShouldNotSwitch EQ
    (True, EQ, True, LT) -> ShouldSwitch (VRFTiebreak (Comparing (ptvTieBreakVRF ours) (ptvTieBreakVRF cand)))
    (True, EQ, False, _) -> ShouldNotSwitch EQ
    (False, _, True, GT) -> ShouldNotSwitch GT
    (False, _, True, EQ) -> ShouldNotSwitch EQ
    (False, _, True, LT) -> ShouldSwitch (VRFTiebreak (Comparing (ptvTieBreakVRF ours) (ptvTieBreakVRF cand)))
    (False, _, False, _) -> ShouldNotSwitch EQ
 where
  issueNoArmed v1 v2 = ptvSlotNo v1 == ptvSlotNo v2 && ptvIssuer v1 == ptvIssuer v2
  vrfArmed v1 v2 = case tiebreakerFlavor of
    UnrestrictedVRFTiebreaker -> True
    RestrictedVRFTiebreaker maxDist -> slotDist (ptvSlotNo v1) (ptvSlotNo v2) <= maxDist
```

`Ord (PraosTiebreakerView c)` (used ONLY for sorting multiple candidates,
never for the switch decision) hardcodes `UnrestrictedVRFTiebreaker`
regardless of the era's actual configured flavor:
```haskell
instance Crypto c => Ord (PraosTiebreakerView c) where
  compare x y = case comparePraos UnrestrictedVRFTiebreaker x y of
    ShouldSwitch{} -> LT
    ShouldNotSwitch o -> o
```

## VRFTiebreakerFlavor default — NOT configurable, hardcoded by era

`ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Config.hs`,
`mkShelleyBlockConfig`:
```haskell
shelleyVRFTiebreakerFlavor
  | isBeforeConway (Proxy @era) = UnrestrictedVRFTiebreaker
  | otherwise =
      -- See 'RestrictedVRFTiebreaker' for context. 5 slots is the "usual" value
      -- we consider when talking about the maximum propagation delay.
      RestrictedVRFTiebreaker 5
```
No CLI flag, no genesis field, no config JSON key — purely a function of
`isBeforeConway` on the era type parameter. Byron/Shelley/Allegra/
Mary/Alonzo/Babbage = `UnrestrictedVRFTiebreaker` (VRF always compared on
tie). Conway (and beyond, until changed upstream) = `RestrictedVRFTiebreaker
5` — 5 **slots**, not KES periods, not seconds directly (multiply by
`slotLength` for wall-clock; dugite's local-devnet has `slotLength=1.0`, so
5 slots = 5 seconds there).

## Why the VRF tiebreak exists at all — the "Frankfurt problem" (verbatim doc comment)

This is the single most important passage for judging any "first-seen wins"
claim. From `Common.hs`'s haddock on the `ChainOrder (PraosTiebreakerView c)`
instance:

> 2. The main motivation to do VRF comparisons is to avoid the "Frankfurt problem":
>
>     With only the first two rules for the chain order, almost all blocks with
>     equal block number are equally preferrable. Consider two block issuers
>     minting blocks in very nearby slots. As we never change our selection
>     from one chain to an equally preferrable one, the first block to arrive
>     at another pool is the one to be adopted, and will be extended the next
>     time the pool is elected if no blocks with a higher block number arrive
>     in the meantime. We observed that this effectively incentivizes block
>     producers to concentrate geographically (historically, in Frankfurt) in
>     order to minimize their diffusion times. This works against the goal of
>     geographic decentralisation.
>
>     Also, with the VRF tiebreaker, a block with a somewhat lower propagation
>     speed has a random chance to be selected instead of the one that arrived
>     first by pools before the next block is forged.

Read literally: **without** the VRF tiebreaker, "first-seen wins" IS exactly
what happens on a same-blockNo tie (this is precisely the historical
behavior the tiebreaker was added to fix). **With** it armed
(`UnrestrictedVRFTiebreaker`, or `RestrictedVRFTiebreaker` within `maxDist`),
the outcome is instead a deterministic, symmetric function of
(issuer, slot, opcert counter, VRF output) — not arrival order. It is ONLY
when `RestrictedVRFTiebreaker`'s slot-distance gate disarms the VRF check
(`vrfArmed=False`, i.e. `|slotOurs - slotCand| > maxDist` AND different
issuer/slot so `issueNoArmed=False` too) that the decision reverts to
`ShouldNotSwitch EQ` in BOTH directions — i.e. genuine "whoever this node
selected first, permanently, for blocks AT THIS SAME HEIGHT" behavior. This
is a real, narrow, era-specific carve-out, not the general rule.

**Non-transitivity** (documented, load-bearing for reasoning about 3+
competing forgers): with `RestrictedVRFTiebreaker maxDist=5`, three tips a
(slot 0), b (slot 3), c (slot 6), pairwise different issuers, VRF values
a=3,b=2,c=1: `preferCandidate a b` and `preferCandidate b c` both hold, but
NOT `preferCandidate a c`, despite `a < c` under `Ord`. A chain-selection
test with 3+ forgers must not assume "better VRF always wins" transitively
once slot gaps exceed the restriction window.

## Peras weight scaffolding (dormant in current Conway/Praos operation)

At `3.0.1.0` (unlike `0.30.0.1`), `compareAnchoredFragments` and
`preferAnchoredCandidate`
(`ouroboros-consensus/.../Util/AnchoredFragment.hs`) take an extra
`PerasWeightSnapshot blk` parameter (Peras = the upcoming fast-finality
certificate scheme). Both functions explicitly short-circuit to the
pre-Peras behavior:
```haskell
compareAnchoredFragments cfg weights frag1 frag2
  | isEmptyPerasWeightSnapshot weights = {- exact old blockNo+tiebreak path -}
  | otherwise = {- weighted comparison over Peras-boosted suffixes -}
```
A standard Conway devnet/testnet/mainnet node today has an empty weight
snapshot (no Peras certs are being produced/validated yet), so this always
takes the `isEmptyPerasWeightSnapshot` branch — **the effective algorithm
for any current cardano-node 11.0.1 deployment (including dugite's
local-devnet) is exactly the pre-Peras blockNo+PraosTiebreakerView algorithm
above**, just wrapped in an `Either`-tagged reason type
(`ReasonForSwitch' blk = Either (ReasonForSwitch (WithEmptyFragment
(WeightedSelectView p))) (ReasonForSwitch (SelectView p))`, `Right` = the
non-Peras path taken here).

## Where this is consumed: ChainSel.hs

`ouroboros-consensus/.../Storage/ChainDB/Impl/ChainSel.hs` (same tag):
- Candidates are filtered by `shouldSwitch . preferAnchoredCandidate bcfg weights curChain`
  — only `ShouldSwitch` survives; `ShouldNotSwitch` (including the `EQ` tie
  case) is dropped. This is the "incumbent wins ties" rule, unchanged in
  spirit from the Bool-returning version, just expressed via `shouldSwitch`.
- Sorting multiple surviving candidates: `sortCandidates = sortBy (flip
  (compareAnchoredFragments bcfg weights curChain) `on` fst)` — this is
  `Ord`-based (hence always `UnrestrictedVRFTiebreaker` per the `Ord`
  instance above), independent of which `VRFTiebreakerFlavor` governs the
  actual switch decision.
- `switchTo` (~L900s): emits `ChainDB.ChangingSelection` first, then either
  `AddedToCurrentChain` (`getRollback chainDiff == 0`) or `SwitchedToAFork`
  (otherwise) — both now carry a 5th field, the `ReasonForSwitch' blk`
  (`Longer`/`HigherOCert`/`VRFTiebreak`), via `SelectionChangedInfo`'s
  `newSuffixSelectView`/`oldSuffixSelectView`.

See [[chaindb-addblock-tracer-namespaces]] for the cardano-node-side trace
namespace strings and exactly which detail level exposes the reason field.

## Practical takeaway for a two-forger convergence test

1. Rule 1 (strictly-higher `blockNo` wins) is checked BEFORE any tiebreak and
   is completely independent of `VRFTiebreakerFlavor`. A standoff at equal
   height (from `RestrictedVRFTiebreaker`'s slot-gate disarming, or from a
   genuine `ShouldNotSwitch EQ`) is automatically broken the instant either
   side's chain extends one block further — no re-org logic needed, no
   "unsticking" mechanism required. Don't assert "converges within N slots at
   the SAME height"; assert "converges once one side is ahead by blockNo",
   which for any meaningfully-stake-skewed 2-forger devnet should be within
   one or two block intervals of the standoff forming.
2. A permanent-EQ standoff at one height requires BOTH: different issuers
   (`issueNoArmed=False`) AND (Conway) slot distance > 5 (`vrfArmed=False`).
   With `slotLength=1.0` in dugite's local-devnet spec, that means the two
   competing blocks must be forged >5 seconds apart in slot terms while
   still tying on blockNo — plausible only if the trailing forger is
   significantly delayed/desynced, not from ordinary loopback propagation
   latency (sub-second).
   With slotLength=1.0 that only takes ~5s of slot drift, not of
   propagation delay per se — the trigger is the SLOT NUMBER gap between the
   two blocks' mint times, not wall-clock arrival time at the observing node.
3. There is no arrival-order / "first-seen" primitive anywhere in
   `comparePraos`/`preferCandidate` — the decision is a pure function of
   (issuer identity, slot, opcert issue number, VRF output) on both sides,
   so two honest nodes that both eventually see both competing blocks and
   run chain selection to completion MUST reach the same `ShouldSwitch`/
   `ShouldNotSwitch` verdict for that same pair, regardless of which one each
   node downloaded first — UNLESS that verdict is `ShouldNotSwitch EQ` (the
   disarmed-tiebreak case above), in which case each node keeps whatever it
   already had selected before the EQ competitor arrived, which CAN differ
   node-to-node if they adopted their initial chain in different order. That
   node-to-node divergence is the only way "first-seen" enters the picture,
   and it is scoped to one height, self-resolving once blockNo diverges.
