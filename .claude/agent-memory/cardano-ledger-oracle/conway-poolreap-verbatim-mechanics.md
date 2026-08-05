---
name: conway-poolreap-verbatim-mechanics
description: Conway POOLREAP full mechanics, verbatim-sourced at pinned SHA — ordering vs SNAP, unregistered-account refund destination, active delegation purge (REFUTES a prior "dangling delegation" belief), futurePoolParams-vs-retirement precedence, and deposit-amount provenance.
metadata:
  type: project
---

Live-verified 2026-08-05 at commit `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (master HEAD,
resolves via `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`, authored 2026-08-04).

**Conway does NOT define its own POOLREAP.** `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs`
embeds `Cardano.Ledger.Shelley.Rules.PoolReap.POOLREAP` verbatim (`Embed (Shelley.POOLREAP era) (EPOCH era)`,
`wrapFailed = \case {}` since `PredicateFailure (POOLREAP era) = Void` — it can never fail). All logic lives
in `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/PoolReap.hs`, function `poolReapTransition`.

This reflects a **post-UMap-refactor architecture** — `libs/cardano-ledger-core/CHANGELOG.md` line 271
records "Remove the `UMap` module"; `StakePoolState` now carries its own `spsDelegators :: Set (Credential
Staking)` field and `Accounts`/`AccountState` (`EraAccounts` class) replaced the old `UMap`/`RewDepUView`
model entirely. Any prior finding phrased in terms of `UMap`/`RewDepUView`/`Delegations` map predates this
and should be re-derived, not trusted as-is.

## 1. Ordering within EPOCH: SNAP runs BEFORE POOLREAP

`Cardano.Ledger.Conway.Rules.Epoch.epochTransition` (verbatim):

```haskell
snapshots1 <-
  trans @(EraRule "SNAP" era) $ TRC (Shelley.SnapEnv ledgerState0 curPParams, snapshots0, ())

Shelley.PoolreapState utxoState1 chainAccountState1 certState1 <-
  trans @(EraRule "POOLREAP" era) $
    TRC ((), Shelley.PoolreapState utxoState0 chainAccountState0 certState0, eNo)
```

SNAP is called first, consuming `ledgerState0` (pre-POOLREAP DState/PState). Consequence: the "mark"
snapshot taken at a boundary where a pool ALSO retires this same epoch still includes that pool's stake
(the pool and its delegations aren't removed until the POOLREAP call immediately after). This is correct
per-spec, not a bug — it just means retirement's effect on snapshots is visible starting the NEXT boundary.

Full rule ordering inside `EPOCH`: **SNAP → POOLREAP → (RATIFY effects consumed from the frozen pulser,
not re-run — see [[project_988_pulser_applied_not_recomputed]]) → HARDFORK (conditional on PV change)**.
`transitionRules = [epochTransition]` — one flat do-block, not a sub-STS chain beyond the four `trans` calls
shown.

## 2. Unregistered reward-account deposit refund → TREASURY, not dropped

`poolReapTransition` verbatim (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/PoolReap.hs`):

```haskell
(refunds, unclaimedDeposits) =
  Map.partitionWithKey
    (\stakeCred _ -> isAccountRegistered stakeCred accounts) -- (k ∈ dom (rewards ds))
    accountRefunds

refunded = fold refunds
unclaimed = fold unclaimedDeposits
...
pure $
  PoolreapState
    us {utxosDeposited = utxosDeposited us <-> fromCompact (unclaimed <> refunded)}
    a {casTreasury = casTreasury a <+> fromCompact unclaimed}
    ( cs
        & certDStateL . accountsL
          %~ removeStakePoolDelegations (delegsToClear cs retired)
               . addToBalanceAccounts refunds
        ...
    )
```

`isAccountRegistered` = `Map.member cred (accounts ^. accountsMapL)` (`libs/cardano-ledger-core/src/
Cardano/Ledger/State/Account.hs`). Both `refunded` and `unclaimed` leave the `utxosDeposited` obligation
pot (both are "no longer owed"), but only `refunded` (registered accounts) is credited via
`addToBalanceAccounts refunds`; `unclaimed` (unregistered accounts) is added straight to
`casTreasury` (`ChainAccountState`'s treasury field). Never silently dropped, never left in the deposit pot.

## 3. Delegations to a retiring pool ARE actively purged — REFUTES the "dangling delegation" claim

```haskell
delegsToClear cState pools =
  foldMap spsDelegators $
    Map.restrictKeys (cState ^. certPStateL . psStakePoolsL) pools
...
& certDStateL . accountsL
    %~ removeStakePoolDelegations (delegsToClear cs retired)
         . addToBalanceAccounts refunds
```

`removeStakePoolDelegations` (`Account.hs`):

```haskell
removeStakePoolDelegations ::
  EraAccounts era => Set (Credential Staking) -> Accounts era -> Accounts era
removeStakePoolDelegations creds accounts =
  accounts
    & accountsMapL
      %~ ( \accountsMap ->
             foldr
               (Map.adjust (stakePoolDelegationAccountStateL .~ Nothing))
               accountsMap
               creds
```

`StakePoolState.spsDelegators :: Set (Credential Staking)` is the pool's own ledger-maintained
delegator-credential set (`libs/cardano-ledger-core/src/Cardano/Ledger/State/StakePool.hs`, field added
per CHANGELOG "Add `spsDelegators` field to `StakePool`"). For every retiring pool, POOLREAP reads that
pool's `spsDelegators` (from `cs`, i.e. BEFORE the pool is stripped from `psStakePools` later in the same
expression) and nulls `stakePoolDelegationAccountStateL` (`Maybe (KeyHash StakePool)`) on each of those
accounts, in the SAME transition, at the SAME boundary the pool is removed.

**Direct verdict on the prior finding**: dangling delegations are NOT a real behavior of current
cardano-ledger. There is no window — not even one snapshot boundary — where an account's delegation
still points at a removed pool key. The `assertions` block on `POOLREAP` even has a companion invariant
("PoolReap may not create or remove account addresses") confirming the account itself survives, only its
`stakePoolDelegationAccountStateL` is cleared. If dugite currently leaves dangling references (only
lazily corrected on the delegator's next `Delegation`/`RegPool`-adjacent cert), that is a byte-exact
divergence: dugite's POOLREAP-equivalent must actively null the per-account stake-pool delegation field
for every credential in the retiring pool's delegator set, in the same epoch-boundary step that removes
the pool.

## 4. futurePoolParams merge precedence vs same-boundary retirement

```haskell
ps0 = cs0 ^. certPStateL
ps =
  ps0
    { psStakePools =
        Map.merge
          Map.dropMissing        -- keys only in psFutureStakePoolParams: dropped
          Map.preserveMissing    -- keys only in psStakePools: kept as-is
          ( Map.zipWithMatched $ \_ futureParams currentState ->
              mkStakePoolState
                (currentState ^. spsDepositL)
                (currentState ^. spsDelegatorsL)
                futureParams
          )
          (ps0 ^. psFutureStakePoolParamsL)
          (ps0 ^. psStakePoolsL)
    , psFutureStakePoolParams = Map.empty
    }
cs = cs0 & certPStateL .~ ps
...
retired = Set.fromDistinctAscList [k | (k, v) <- Map.toAscList (psRetiring ps), v == e]
retiringPools = Map.restrictKeys (psStakePools ps) retired   -- reads the POST-MERGE map
```

The future-params merge runs FIRST and unconditionally, for every pool with a pending re-registration,
regardless of whether that pool is also retiring this boundary. `retiringPools` is then read from the
ALREADY-MERGED `psStakePools ps` — i.e. if a pool both has a pending `futurePoolParams` entry and retires
in the same epoch, the `StakePoolState` used to compute its refund (`spsAccountId`, used to build
`accountRefunds`) is the MERGED one. `mkStakePoolState` (`StakePool.hs`) takes
`spsAccountId = aaId (sppAccountAddress spp)` from the third arg, i.e. from `futureParams`, not from
`currentState`. So a same-boundary "re-register with a new reward account, then retire" sequence pays the
refund to the NEW (future) reward account, not the original one. `spsDeposit`/`spsDelegators` are
explicitly carried from `currentState` (unaffected by the merge — see Q5).

Net observable state: regardless of the merge, the pool is unconditionally stripped from `psStakePools` at
the end of the same transition (`certPStateL . psStakePoolsL %~ (\`Map.withoutKeys\` retired)`) — so
retirement always wins on EXISTENCE. But the merge is not a no-op: it can change WHERE the refund is paid.
This is a genuinely subtle two-step (merge-then-retire, not override-and-discard) — verify dugite's
implementation reproduces both the merge AND its effect on refund destination, not just "retirement wins."

## 5. Deposit refunded = ORIGINAL REGISTRATION-TIME amount, immutable across re-registrations

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`, `poolTransition`, `RegPool` cert, NEW pool
branch:

```haskell
pure $
  ps
    & psStakePoolsL
      %~ Map.insert sppId (mkStakePoolState (pp ^. ppPoolDepositCompactL) mempty stakePoolParams)
```

— `spsDeposit` is set ONCE, from the CURRENT `ppPoolDeposit` PParam at the moment of the pool's very
FIRST `RegPool` cert. The re-registration branch (pool already in `psStakePools`) explicitly comments
**"The deposit does not change."** and only touches `psFutureStakePoolParamsL`/`psRetiringL`/VRF-hash
bookkeeping — `spsDeposit` is untouched. POOLREAP's own future-params merge also explicitly preserves it
(`currentState ^. spsDepositL` passed through unchanged). So the refund at retirement
(`spsDeposit sps` for `sps <- Map.elems retiringPools`) is the deposit paid at the pool's original
registration, NOT the current `poolDeposit` protocol-parameter value — governance changes to `poolDeposit`
after a pool registers have zero effect on that pool's eventual refund.

## Rust translation notes (dugite-ledger)

- POOLREAP-equivalent must run strictly after SNAP within the epoch-boundary step, consuming pre-POOLREAP
  DState/PState for SNAP's mark-snapshot computation.
- Per-pool state needs its own delegator-credential set (mirroring `spsDelegators`) so retirement can purge
  exactly that set's delegation pointers — do not model this via a global reverse-scan or lazy correction.
- Unregistered-account refunds must route to treasury (not dropped, not left in the deposit pot).
- Deposit amount must be captured once at first registration into the pool's own state record and never
  re-read from live PParams on refund or on re-registration.
- If dugite defers future-pool-params application to a point AFTER retirement removal (or skips it
  entirely for a pool that's also retiring), the reward-account-for-refund edge case in Q4 will diverge in
  the rare case of "re-register with new reward account + retire, same epoch."
