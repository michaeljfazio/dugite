---
name: reward-maturity-mark-set-go-timeline
description: Exact epoch-boundary count from stake registration/delegation to first-withdrawable reward, the genesis mark-snapshot shortcut (one boundary faster than a live mid-run delegation), and why a zero-block pool's members get structurally no reward (not a zero value). Live-verified 2026-08-02 against pinned SHA 4f7cb2d6874df70561e32147084ed82cee773e8a.
metadata:
  type: reference
---

## Source files (IntersectMBO/cardano-ledger, pinned 4f7cb2d6874df70561e32147084ed82cee773e8a)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs` — `startStep`, `completeRupd`, `collectLRs`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs` — `mkPoolRewardInfo`, `rewardOnePoolMember`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Transition.hs` — `createInitialState`, `shelleyRegisterInitialAccounts`, `resetStakeDistribution`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs`, `Rules/Tick.hs` (ordering, cross-verified via prior memory `newepoch-ordering-details.md` / `epoch0-rupd-ssfee-semantics.md`)

## Epoch-boundary count: mid-run registration vs genesis shortcut

**General rule** (bprev/GO derivation, re-derived and cross-checked against the existing `epoch0-rupd-ssfee-semantics.md` trace): a RUPD computed *during* epoch E uses `bprev` = blocks made in epoch E-1, and `es.snapshots.go` = the live stake snapshot as of the end of epoch E-3 (three SNAP rotations back: mark captured at E-2 boundary, promoted to set at E-1 boundary, promoted to go at E boundary). The completed RUPD is applied (`applyRUpd`, crediting `Accounts`) at the START of the boundary E->E+1.

**Mid-run case**: stake credential registers + delegates to an already-active, block-producing pool live during epoch M (not at genesis).
- Boundary M->M+1: SNAP takes the first live snapshot that includes this credential -> new `mark`. (`set`/`go` still predate it.)
- Boundary M+1->M+2: that snapshot rotates `mark -> set`.
- Boundary M+2->M+3: rotates `set -> go`. **First epoch where GO includes the credential = epoch M+3.**
- RUPD computed during epoch M+3 uses bprev = blocks from epoch M+2, stake weight from GO = end-of-epoch-M snapshot (includes the credential). This RUPD applies at boundary M+3->M+4.
- **Reward first appears in the account balance / becomes withdrawable at the START of epoch M+4 — 4 epoch boundaries after the registration epoch M** (M->M+1, M+1->M+2, M+2->M+3, M+3->M+4), PROVIDED the pool produced >=1 block in epoch M+2 (see zero-block-pool note below) and the RUPD's arithmetic doesn't floor the specific member's share to 0.

**Genesis shortcut — confirmed directly in `Transition.hs`, `resetStakeDistribution`**:
```haskell
resetStakeDistribution nes =
  nes
    & nesEsL . esSnapshotsL . ssStakeMarkL .~ initSnapShot
    & nesEsL . esSnapshotsL . ssStakeMarkPoolDistrL .~ poolDistr
    & nesPdL .~ poolDistr
  where
    -- The new stake distribution is made on the basis of a snapshot taken
    -- during the previous epoch. We create a "fake" snapshot in order to
    -- establish an initial stake distribution.
    initSnapShot = snapShotFromInstantStake (addInstantStake (nes ^. utxoL) mempty) dState pState
```
`createInitialState` itself seeds `esSnapshots = emptySnapShots` (mark/set/go ALL empty, `ssFee=0`); `resetStakeDistribution` runs afterward and writes ONLY `ssStakeMarkL` (+ `ssStakeMarkPoolDistrL` + `nesPdL`, the VRF-leader-election pool distribution) directly from genesis stake — `ssStakeSetL`/`ssStakeGoL` are left EMPTY, not backfilled. This means genesis-delegated stake gets a one-boundary head start versus a mid-run delegation, because the very first "populate mark" step (which normally costs one boundary, M->M+1 above) is done for free at chain construction time, before epoch 0 even starts:
- Genesis: mark = genesis stake (pre-seeded), set = empty, go = empty.
- Boundary 0->1: mark(genesis) -> set. New mark = live epoch-0 state.
- Boundary 1->2: set(genesis) -> go. **GO = genesis stake starting epoch 2** (one boundary sooner than the mid-run M+3 formula would give for an equivalent "M=-1").
- RUPD computed during epoch 2 uses bprev = blocks from epoch 1, GO = genesis stake. Applies at boundary 2->3.
- **Reward first withdrawable at the START of epoch 3 — only 3 boundaries from genesis** (0->1, 1->2, 2->3), not 4.

Also note: `nesPd` (the pool distribution consulted for VRF leader-election eligibility, a DIFFERENT mechanism from the reward-snapshot pipeline) is seeded from genesis stake IMMEDIATELY — this is why a genesis-delegated pool can already forge blocks starting epoch 0, even though its delegators' rewards don't mature until epoch 3. Leader-eligibility bootstrap and reward-eligibility bootstrap are two separate mechanisms with two separate timelines; only the reward one gets the mark/set/go multi-epoch delay.

## Zero-block pool -> structurally no reward (not a zero-valued reward)

`mkPoolRewardInfo` (Rewards.hs) short-circuits BEFORE any reward math when the pool made no blocks:
```haskell
case Map.lookup stakePoolId (unBlocksMade blocks) of
  Nothing -> Left $! StakeShare sigma          -- no blocks: ranking info only, NO PoolRewardInfo
  Just numBlocksMade -> ... Right rewardInfo   -- >=1 block: full reward computation
```
`startStep` then does `blockProducingPoolInfo = VMap.mapMaybe (either (const Nothing) Just) allPoolInfo` — pools with `Left` are dropped entirely; no `PoolRewardInfo`, no leader reward, no member rewards computed at all for that pool in that epoch. This is checked against the GO-snapshot pool's presence in `BlocksMade` (bprev, i.e. blocks made in epoch E-1 relative to the RUPD computed in epoch E) — it has nothing to do with whether a delegator's OWN credential was in the go snapshot; it's purely per-pool.

## Pool owner / registered reward-account timing == plain delegator timing

`collectLRs` (PulsingReward.hs `startStep`) credits the LEADER reward to `spssAccountId $ poolPs poolRI` — the pool's registered reward-account credential from its own `StakePoolSnapShot` (NOT necessarily any owner's own stake key; could be any credential the pool designates) — using the exact same `hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered account accounts` eligibility gate as member rewards (`rewardOnePoolMember`'s `prefilter`). Both leader and member rewards are folded into the SAME `rs :: Map Cred (Set Reward)` inside the SAME `completeRupd`, and applied via the SAME `applyRUpd` at the SAME epoch boundary. **There is no timing difference between a pool operator's reward-account credit and a plain delegator's** — only the `RewardType` tag (`LeaderReward` vs `MemberReward`) and the eligibility computation differ. Pool owners are explicitly EXCLUDED from `calcStakePoolMemberReward` via `notPoolOwner` in `rewardOnePoolMember` (they get paid only via the leader-reward path, to whatever credential is actually registered as the pool's reward account — which may or may not coincide with an owner's own stake key).

## Related
[[reward-calc-floor-chain-and-sigma-vs-sigmaA]] — the 3-floor reward arithmetic chain this timeline feeds into.
[[conway-withdrawal-validation-exact-mechanics]] — what happens when the matured reward is actually withdrawn.
