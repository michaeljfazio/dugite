---
name: reapply-validatenone-predicate-skip-mechanics
description: Exact STS mechanism by which ValidateNone/reapplySTS skips ALL Predicate-DSL validation checks (withdrawals, everything else) while state transitions (drainAccounts etc.) always run unconditionally. Live-verified 2026-07-07.
metadata:
  type: reference
---

## The question this answers

When Haskell reapplies a trusted block from the ImmutableDB (consensus replay path), does the STS
machinery skip Phase-1-style validation predicates (e.g. Conway's withdrawal-amount-must-equal-
reward-balance check), or does it re-run everything and potentially reject blocks that are already
known-good on the canonical chain?

**Answer: it skips ALL `Predicate`-DSL checks unconditionally, in every rule, while every ordinary
state computation still runs in full.** This is a general mechanism, not specific to withdrawals.

## Core mechanism (`libs/small-steps/src/Control/State/Transition/Extended.hs`)

```haskell
data ValidationPolicy = ValidateAll | ValidateNone | ValidateSuchThat ([Label] -> Bool)

data ApplySTSOpts ep = ApplySTSOpts
  { asoAssertions :: AssertionPolicy
  , asoValidation :: ValidationPolicy
  , asoEvents :: SingEP ep
  }
```

`reapplySTS` (used when the caller has "previously applied this STS, and can guarantee that it
completed successfully"):

```haskell
reapplySTS ctx = applySTSOpts defaultOpts ctx <&> fst
  where
    defaultOpts = ApplySTSOpts
      { asoAssertions = AssertionsOff
      , asoValidation = ValidateNone
      , asoEvents = EPDiscard
      }
```

The STS free-monad interpreter (`applyRuleInternal`'s `runClause`) handles the `Predicate` clause —
the desugaring target of every `?!` / `runTest` / `failOnNonEmptyMap` / `validateTrans` call in every
rule in the codebase — like this:

```haskell
runClause (Predicate cond orElse val) =
  case vp of
    ValidateNone -> pure val
    _ -> case cond of
      Success x -> pure x
      Failure errs -> modify (first (map orElse (reverse (NE.toList errs)) <>)) >> pure val
```

Under `ValidateNone`, `cond` (the actual boolean/Validation check) is **never evaluated** and **no
PredicateFailure is ever recorded** — the rule immediately continues with `val`, the same
continuation value it would have used on success. `Label`/`validateIf` (used by `?!#`-style "static"
checks) is likewise gated: `ValidateNone -> False` means the guarded subrule never runs.

Crucially, `val` is a fixed constant baked into each call site (typically `()`), **not a
success/failure-dependent branch** — Predicate clauses only ever gate whether an error gets
appended to the failure list; they never change what the state computation itself calculates. All
real ledger arithmetic (deposit refunds, `drainAccounts`, `Map.adjust`, deposit/treasury bookkeeping)
is implemented via ordinary `Lift`/monadic code in the same `do`-block, structurally independent of
any `Predicate` clause, so it always executes identically regardless of `ValidationPolicy`.

## Block-level wiring (`eras/shelley/impl/src/Cardano/Ledger/Shelley/API/Validation.hs`)

```haskell
applyBlockNoValidaton :: ApplyBlock h era => Globals -> NewEpochState era -> Block h era -> NewEpochState era
applyBlockNoValidaton globals newEpochState block = newEpochStateResult
  where
    (newEpochStateResult, _failure, _events) =
      applyBlock EPDiscard ValidateNone globals newEpochState block
```

This runs the **entire BBODY rule** (→ LEDGERS → LEDGER → UTXOW/CERTS/GOV/DELEGS/...) with
`ValidateNone`. `applyTick` (TICK rule, i.e. epoch-boundary processing) is *always* run with
`ValidateNone` regardless of caller — the code comment says "since it can't fail anyways" (TICK's
PredicateFailure type is `Void` in practice; see [[bounded-ratio-decode-and-enact-totality]] for the
same total-rule pattern in ENACT).

This is the function an `ApplyBlock`-class-based consensus reapply path (ouroboros-consensus'
`reapplyBlock`/`tickThenReapply`) calls for the Cardano ledger integration. So: **every** validation
predicate throughout the whole block-application STS tree is a no-op during trusted replay, not just
withdrawals.

## Concrete worked example: Conway withdrawals (proves the state transition is validation-independent)

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs` (`conwayLedgerTransitionTRC`):

```haskell
unless (hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL)) $ do
  runTest $ validateWithdrawalsDelegated accounts tx        -- Predicate clause -> no-op under ValidateNone

certState' <-
  if hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule $ pp ^. ppProtocolVersionL
    then do
      let withdrawals = tx ^. bodyTxL . withdrawalsTxBodyL
      Shelley.testIncompleteAndMissingWithdrawals (certState ^. certDStateL . accountsL) withdrawals
        -- ^ also desugars to Predicate clauses (failOnNonEmptyMap) -> no-op under ValidateNone
      pure $ certState
        & updateDormantDRepExpiries tx curEpochNo
        & updateVotingDRepExpiries tx curEpochNo (pp ^. ppDRepActivityL)
        & certDStateL . accountsL %~ drainAccounts withdrawals   -- ALWAYS runs, unconditionally
    else pure certState
```

`runTest = validateTrans injectFailure` and `validateTrans t v = liftF $ Predicate v t ()` —
confirmed same `Predicate` primitive. `testIncompleteAndMissingWithdrawals`'s two checks
(`ShelleyWithdrawalsMissingAccounts` / `ShelleyIncompleteWithdrawals`, renamed
`ConwayWithdrawalsMissingAccounts` / `ConwayIncompleteWithdrawals` in Conway's own predicate-failure
type) both go through `failOnNonEmptyMap cond onNonEmpty = liftF $ Predicate (failureOnNonEmptyMap cond onNonEmpty) id ()` — same mechanism.

The actual state mutation, `drainAccounts` (`libs/cardano-ledger-core/src/Cardano/Ledger/State/Account.hs`):

```haskell
drainAccounts (Withdrawals wdrls) = updateAccountBalances (\_ _ -> mempty) wdrls
```

This **unconditionally sets every withdrawn account's balance to `Coin 0`**, regardless of whether
the tx's claimed withdrawal amount actually equals the current balance. It is not a subtraction of
the declared amount — it is a hard reset to zero for every key in the withdrawals map. This is why
skipping the validation predicate is *safe*, not just permitted: the state transition was never
computed from the (possibly-wrong) declared amount in the first place — the "amount must equal
current balance" predicate exists purely so that Phase-1 value-conservation (inputs+withdrawals ==
outputs+fee+deposits) holds for the *declared* withdrawal amount used elsewhere in UTXO balance
checks, not to gate what drainAccounts computes.

Shelley's plain (non-Conway) equivalent lives in `ledgerTransition` in
`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ledger.hs` — same pattern, `drainAccounts`
called unconditionally right after (not gated on) `testIncompleteAndMissingWithdrawals`.

## Practical implication for a Rust reimplementation (Dugite)

For a trusted-replay / "ApplyOnly" path that mirrors `reapplySTS`/`applyBlockNoValidaton`:
- It is **correct** to skip all Phase-1-style validation predicates (withdrawal-exact-match,
  DRep-delegation-of-withdrawal-account, treasury-value-match, ref-script-size, etc.) — this matches
  Haskell's real, live behavior, not an approximation.
- It is **only safe** if the state-transition code for withdrawals mirrors `drainAccounts`'s exact
  semantics: unconditionally **set** each withdrawn account's reward balance to zero, never
  conditionally validate-then-subtract, and never subtract the tx's declared amount. If Dugite's
  replay path instead does `balance -= declared_amount`, a real accounting divergence (e.g. from a
  reward-calculation bug, see [[reward-calc-floor-chain-and-sigma-vs-sigmaA]]) will leave a residual
  nonzero balance instead of self-healing to zero the way Haskell's unconditional reset does — so the
  *symptom* (rejected/mismatched block) is a genuine signal of an upstream reward-calc bug even though
  the *validation check itself* should not be enforced during replay.
- The same reasoning generalizes: for ANY validation predicate skipped during ApplyOnly replay, audit
  whether Dugite's corresponding state-transition code is written as "validate then compute" (unsafe
  to skip only the validate half) vs "compute unconditionally, with the predicate a pure side-channel
  error report" (safe to skip, matches Haskell).

## See also
[[project_dugite_genesisdeleg_mir_gaps_2026_07_06]] for a related total-vs-partial STS rule pattern
(MIR/GenesisDeleg, `PredicateFailure = Void`).
