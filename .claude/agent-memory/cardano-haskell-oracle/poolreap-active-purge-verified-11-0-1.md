---
name: poolreap-active-purge-verified-11-0-1
description: POOLREAP actively clears a retiring pool's delegators' delegation pointer in the SAME transition, confirmed at the exact commit cardano-node 11.0.1 ships (not master-only)
type: reference
---

**Verdict**: cardano-node 11.0.1 ships cardano-ledger with ACTIVE-PURGE
POOLREAP semantics, not dangling-delegation semantics. A retired pool's
former delegators are excluded from `ssTotalActiveStake`/active-stake sums
starting the very next snapshot.

**Pin chain** (cardano-node 11.0.1 tag → commit `97036a66bcf8c89f687ae57a048eecc0389977ef`,
GPG-verified annotated tag `704cd2e4c2ffa84cc2fb4a54ba98b849054baa9c`):
cardano-node's `cabal.project` has no ledger git pin (see
[[chap-dependency-pinning-methodology]]); resolves via CHaP index-state
`2026-05-02T16:21:41Z` through `cardano-testnet.cabal`'s
`cardano-ledger-shelley >=1.16` bound to the highest CHaP version published
before that cutoff:

- `cardano-ledger-shelley` **1.18.1.0**, CHaP-published 2026-04-13T13:33:52Z
  → cardano-ledger commit **`b7c17cf31871062b7883c46e3f367cb5e1b5db6c`**
  (commit message itself: "Bump version for cardano-ledger-shelley")
- `cardano-ledger-core` **1.20.0.0**, CHaP-published 2026-04-13T10:18:15Z
  → cardano-ledger commit **`94e9618c91a16ec08db477632a158b630722089b`**
  (`cardano-ledger-shelley` 1.19.0.0 / `cardano-ledger-core` 1.21.0.0 both
  postdate cardano-node's cutoff by ~3 months — 2026-07-29 — and are NOT
  what 11.0.1 resolves to)

**At `b7c17cf3...`**, `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/PoolReap.hs`
already has the fully-refactored mechanism:

```haskell
delegsToClear cState pools =
  foldMap spsDelegators $
    Map.restrictKeys (cState ^. certPStateL . psStakePoolsL) pools
-- ...
& certDStateL . accountsL
    %~ removeStakePoolDelegations (delegsToClear cs retired)
      . addToBalanceAccounts refunds
& certPStateL . psStakePoolsL %~ (`Map.withoutKeys` retired)
```
Pool removal from `psStakePools` and delegation-pointer clearing happen in
the SAME composed state update (one POOLREAP transition, one epoch).

`removeStakePoolDelegations` (`libs/cardano-ledger-core/src/Cardano/Ledger/State/Account.hs`
@ `94e9618c9...`):
```haskell
removeStakePoolDelegations creds accounts =
  accounts & accountsMapL %~ (\m -> foldr (Map.adjust (stakePoolDelegationAccountStateL .~ Nothing)) m creds)
```
Sets `stakePoolDelegationAccountStateL` to `Nothing` — an explicit pointer
clear, not a lazy/deferred cleanup.

**Downstream confirmation this actually excludes the stake**:
`resolveActiveInstantStakeCredentials` (`libs/cardano-ledger-core/.../Stake.hs`)
requires `poolId <- accountState ^. stakePoolDelegationAccountStateL` inside
a `Maybe` do-block per credential — `Nothing` short-circuits the whole
credential OUT of the returned map via `Map.mapMaybeMissing`/
`Map.zipWithMaybeAMatched`. Doc comment: *""active" stake means any stake
credential that is registered **and delegated to a stake pool**."* This
feeds `ssActiveStake` → `ssTotalActiveStake = sumAllActiveStake ssActiveStake`
in `SnapShots.hs`. An undelegated (post-POOLREAP) credential is dropped
entirely, not merely un-attributed to a pool.

**Not master-only / not new**: `libs/cardano-ledger-core/src/Cardano/Ledger/State/Account.hs`
(the module `removeStakePoolDelegations` lives in) was created 2025-05-24 —
confirmed the SAME mechanism (`removeStakePoolDelegations retired` inline,
pre-`delegsToClear` refactor) is already present in `cardano-ledger-shelley`
1.17.0.0 (CHaP timestamp 2025-09-10, commit `faa7a9dc347697b11d4da5b7818b1731e11aeeef`)
— eight months before cardano-node 11.0.1 shipped. The verdict is robust
across at least three consecutive shelley releases (1.17.0.0 → 1.18.0.0 →
1.18.1.0), so it does not hinge on getting the exact CHaP patch-version
resolution right.

**Naming note, not a contradiction**: `StakePoolSnapShot`/`spssStake` (an
aggregate per-pool figure inside a `SnapShot`, built by `mkStakePoolSnapShot`)
and `StakePoolState`/`spsDelegators` (POOLREAP's live/current state) are TWO
DIFFERENT types that coexist in the same commit — a research pass citing
`spssStake` was not necessarily looking at stale code, just a different type
than one citing `spsDelegators`.

**Practical implication for a Rust reimplementation modeling
dangling-delegation** (pool removed from the pool map but the delegator's
own pointer left untouched): that model diverges from what real
preview/preprod/mainnet nodes run under 11.0.1. If an on-chain byte-exact
match was previously observed with a dangling model, re-derive the actual
cause — it is not explained by upstream POOLREAP behavior as verified here,
and is most likely either a coincidental agreement (e.g. no pool actually
retired in the relevant window) or a different, unrelated fix landing in the
same commit.
