---
name: conway-certs-rule-dispatch-and-withdrawal-split
description: Verbatim ConwayCertsPredFailure (2 ctors) + ConwayCertPredFailure (3 ctors, pure dispatcher) + the PV11 hardfork that moves WithdrawalsNotInRewardsCERTS out of CERTS into two LEDGER-level predicates (ConwayWithdrawalsMissingAccounts / ConwayIncompleteWithdrawals); confirms certificate-by-certificate CertState threading order
metadata:
  type: reference
---

## Pin
Live-verified 2026-08-05 at commit `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`
(`gh api repos/IntersectMBO/cardano-ledger/commits/<sha>` resolves, dated
2026-08-04T21:48:51Z, "Merge pull request #5950 …"). Fetched via
`gh api ".../contents/<path>?ref=<sha>" -H "Accept: application/vnd.github.raw"`
— this worked directly (no tree-walk needed this time). Companion to
[[deleg-pool-govcert-verbatim-transitions]] (same SHA family, covers
DELEG/POOL/GOVCERT leaf rules) and [[conway-gov-rule-verbatim-checks]] (GOV).

## The three-layer CERTS -> CERT -> {DELEG,POOL,GOVCERT} dispatch chain

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs` (328 lines) +
`.../Rules/Cert.hs` (284 lines). Two distinct STS rules, easy to conflate:

- **CERTS** (plural) — folds the WHOLE `Seq (TxCert era)` from the tx body.
  State = `CertState era`. Signal = `Seq (TxCert era)`.
- **CERT** (singular) — dispatches ONE `TxCert` to its sub-rule by
  constructor. State = `CertState era`. Signal = `TxCert era`.

### `ConwayCertsPredFailure` — exactly 2 constructors, tags 0-1

```haskell
data ConwayCertsPredFailure era
  = WithdrawalsNotInRewardsCERTS Withdrawals        -- tag 0
  | CertFailure (PredicateFailure (EraRule "CERT" era))  -- tag 1
```

CERTS has **zero own validation logic beyond this one withdrawal check** —
every other failure is a pure wrap of whatever CERT (and beneath it,
DELEG/POOL/GOVCERT) produced. No `MalformedCert`, no "duplicate stake key"
predicate at the CERTS layer — dedup/well-formedness of individual certs is
each sub-rule's own job (see [[deleg-pool-govcert-verbatim-transitions]]).

### `ConwayCertPredFailure` — exactly 3 constructors, tags 1-3 (no 0), PURE dispatcher

```haskell
data ConwayCertPredFailure era
  = DelegFailure (PredicateFailure (EraRule "DELEG" era))    -- tag 1
  | PoolFailure (PredicateFailure (EraRule "POOL" era))      -- tag 2
  | GovCertFailure (PredicateFailure (EraRule "GOVCERT" era)) -- tag 3
```

`certTransition` (`Cert.hs:210-223`) body, verbatim:

```haskell
certTransition = do
  TRC (CertEnv pp currentEpoch committee committeeProposals, certState, c) <- judgmentContext
  case c of
    ConwayTxCertDeleg delegCert ->
      trans @(EraRule "DELEG" era) $ TRC (ConwayDelegEnv pp pools, certState, delegCert)
    ConwayTxCertPool poolCert -> do
      newPState <- trans @(EraRule "POOL" era) $ TRC (PoolEnv currentEpoch pp, certPState, poolCert)
      pure $ certState & certPStateL .~ newPState
    ConwayTxCertGov govCert ->
      trans @(EraRule "GOVCERT" era) $
        TRC (ConwayGovCertEnv pp currentEpoch committee committeeProposals, certState, govCert)
```

Zero `?!`/`failBecause`/`runTest` calls anywhere in `certTransition` — CERT
is a **total dispatcher with no predicate logic of its own**. Note POOL
alone operates on the narrower `PState era` (via `certPStateL`), not the
full `CertState`; DELEG and GOVCERT both take/return the full `CertState`.

### Full CBOR nesting for a cert-caused rejection

`ConwayLedgerPredFailure` tag 2 (`ConwayCertsFailure`) wraps
`ConwayCertsPredFailure` tag 1 (`CertFailure`) wraps `ConwayCertPredFailure`
tag 1/2/3 (`DelegFailure`/`PoolFailure`/`GovCertFailure`) wraps the leaf
DELEG (tags 1-8) / POOL (shared Shelley ctor, no CERT-level tag needed since
POOL has only one failure path type) / GOVCERT (tags 0-5) ADT. A Rust
decoder for `MsgRejectTx` must walk exactly this 4-deep Summands chain.

## Certificate-by-certificate threading — CONFIRMED from source, not inferred

`conwayCertsTransition` (`Certs.hs:215-246`):

```haskell
conwayCertsTransition = do
  TRC (env@(CertsEnv tx pp currentEpoch committee committeeProposals), certState, certificates) <-
    judgmentContext
  case certificates of
    Empty -> ...  -- see withdrawal-check section below
    gamma :|> txCert -> do
      certState' <- trans @(CERTS era) $ TRC (env, certState, gamma)
      trans @(EraRule "CERT" era) $
        TRC (CertEnv pp currentEpoch committee committeeProposals, certState', txCert)
```

This recurses on `gamma` (the `Seq` with its LAST element unsnoc'd via
`:|>`) before applying `txCert` (the last element) to the recursion's
result. Working through `[c1,c2,c3]`: the innermost call reaches `Empty`
first (running the withdrawal/DRep-expiry base case on the ORIGINAL
`certState`), then c1 is applied via CERT, then c2 sees the state c1
already mutated, then c3 sees c1+c2's mutations. **Certs are threaded
strictly in tx-body order, each one seeing every earlier cert's mutation in
the SAME transaction** — confirmed directly from the recursion shape, not
inferred. This is why e.g. `ConwayRegDelegCert` followed later in the same
cert list by a `ConwayUnRegCert` on the same credential is well-defined and
legal (register then unregister in one tx).

## THE PV>=11 split: `WithdrawalsNotInRewardsCERTS` -> two LEDGER-level predicates

Gate: `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule pv = pvMajor pv
> natVersion @10` (`Era.hs:283-284`, i.e. **PV11+**). This is a DIFFERENT
gate from `hardforkConwayBootstrapPhase` (PV9 exactly) and
`hardforkConwayDELEGIncorrectDepositsAndRefunds` (also PV11+, but a
different concern — see [[deleg-pool-govcert-verbatim-transitions]]).

### PV <= 10 (pre-hardfork): CERTS does the check, ONE combined predicate

`Certs.hs`'s `Empty` case (base of the recursion above, i.e. runs BEFORE any
cert in the tx is applied):

```haskell
Empty ->
  if hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule $ pp ^. ppProtocolVersionL
    then pure certState   -- PV>=11: this whole block is now a no-op in CERTS
    else do
      network <- liftSTS $ asks networkId
      let accounts = certState ^. certDStateL . accountsL
          withdrawals = tx ^. bodyTxL . withdrawalsTxBodyL
      failOnJust
        (withdrawalsThatDoNotDrainAccounts withdrawals network accounts)
        ( \(invalid, incomplete) ->
            WithdrawalsNotInRewardsCERTS $
              Withdrawals $ unWithdrawals invalid <> fmap mismatchSupplied incomplete
        )
      pure $ certState
        & updateDormantDRepExpiries tx currentEpoch
        & updateVotingDRepExpiries tx currentEpoch (pp ^. ppDRepActivityL)
        & certDStateL . accountsL %~ drainAccounts withdrawals
```

At PV<=10 the two failure CLASSES (bad-account withdrawals and
wrong-amount withdrawals) are **merged into one `Withdrawals` map** and one
constructor — `invalid`'s amounts kept as-is, `incomplete`'s entries reduced
to just `mismatchSupplied` (the expected/actual balance is DISCARDED at this
PV — a Rust port matching pre-11 wire behavior must not try to recover the
expected balance from this payload, it isn't there).

### PV >= 11 (post-hardfork): CERTS's `Empty` case is a no-op; LEDGER does it, split in two

`Rules/Ledger.hs`, `conwayLedgerTransitionTRC` (lines ~379-392), runs BEFORE
`trans @(EraRule "CERTS" era)` is even invoked, against the certState AS OF
BEFORE this tx's own certs are applied (comment in source: "we need to make
sure we are using the certState before certificates are applied, because
otherwise it would not be possible to unregister an account address and
withdraw all funds from it in the same transaction"):

```haskell
unless (hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL)) $
  runTest $ validateWithdrawalsDelegated accounts tx    -- ConwayWdrlNotDelegatedToDRep, see below

certState' <-
  if hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule $ pp ^. ppProtocolVersionL
    then do
      let withdrawals = tx ^. bodyTxL . withdrawalsTxBodyL
      Shelley.testIncompleteAndMissingWithdrawals (certState ^. certDStateL . accountsL) withdrawals
      pure $ certState
        & updateDormantDRepExpiries tx curEpochNo
        & updateVotingDRepExpiries tx curEpochNo (pp ^. ppDRepActivityL)
        & certDStateL . accountsL %~ drainAccounts withdrawals
    else pure certState
```

`Shelley.testIncompleteAndMissingWithdrawals` (`eras/shelley/impl/.../Rules/Ledger.hs:341-359`,
era-generic, reused by Conway via `InjectRuleFailure`) is the function that
now produces the split:

```haskell
testIncompleteAndMissingWithdrawals accounts withdrawals = do
  network <- liftSTS $ asks networkId
  let (missingWithdrawals, incompleteWithdrawals) =
        case withdrawalsThatDoNotDrainAccounts withdrawals network accounts of
          Nothing -> (Map.empty, Map.empty)
          Just (missing, incomplete) -> (unWithdrawals missing, incomplete)
  failOnNonEmptyMap missingWithdrawals $
    injectFailure . ShelleyWithdrawalsMissingAccounts . Withdrawals . NEM.toMap
  failOnNonEmptyMap incompleteWithdrawals $ injectFailure . ShelleyIncompleteWithdrawals
```

Both `failOnNonEmptyMap` calls ALWAYS run (STS accumulation, see below) —
**both failures can fire in the same tx** if it has some withdrawals to
unregistered/wrong-network accounts AND some withdrawals with a wrong
amount to OTHER, valid accounts.

Conway's own ADT (`Rules/Ledger.hs:117-127`, `ConwayLedgerPredFailure`,
CBOR tags 1-9, no 0) is where these land at the Conway type level:

```haskell
| ConwayWithdrawalsMissingAccounts Withdrawals                              -- tag 8
| ConwayIncompleteWithdrawals (NonEmptyMap AccountAddress (Mismatch RelEQ Coin)) -- tag 9
```

**These are the "EXACT two names" the user needs**:
`ConwayWithdrawalsMissingAccounts` (tag 8) and `ConwayIncompleteWithdrawals`
(tag 9) — both live in `ConwayLedgerPredFailure` (the LEDGER rule's own
ADT), NOT `ConwayCertsPredFailure`. `WithdrawalsNotInRewardsCERTS` (CERTS
tag 0) becomes permanently unreachable/dead at PV>=11.

### The exact classification formula (shared by both PV eras)

`libs/cardano-ledger-core/src/Cardano/Ledger/State/Account.hs:203-265`,
`withdrawalsThatDoNotDrainAccounts` = `categorizeWithdrawals (\amt acc ->
amt == fromCompact (acc ^. balanceAccountStateL))`. For each `(AccountAddress,
Coin)` entry in the tx's `Withdrawals`:

```haskell
lookupAccount (AccountAddress aaNetworkId (AccountId credential))
  | aaNetworkId == networkId = lookupAccountState credential accounts
  | otherwise = Nothing
```

- Account not found **OR wrong network id** (both routes produce `Nothing`
  from `lookupAccount` — a network-id-mismatched withdrawal target is
  **indistinguishable from an unregistered one**, there is no separate
  "wrong network" withdrawal predicate) -> goes into the `missing` map,
  keyed by `AccountAddress`, value = the withdrawal amount as supplied.
- Account found but `withdrawalAmount /= fromCompact(account balance)` ->
  goes into the `incomplete` map, value = `Mismatch { mismatchSupplied =
  withdrawalAmount, mismatchExpected = actual account balance }`
  (`RelEQ`). **Not `<=` — a withdrawal must drain the account to EXACTLY
  zero, partial withdrawals are always rejected**, at every PV.
- If ALL withdrawals pass (`Map.foldrWithKey' checkBadWithdrawals True
  withdrawals`), the fast path returns `Nothing` and neither map is even
  allocated.

### `ConwayWdrlNotDelegatedToDRep` — the OTHER withdrawal check, separate gate

Also in `Rules/Ledger.hs` (`ConwayLedgerPredFailure` tag 4), gated
`unless (hardforkConwayBootstrapPhase pv)` i.e. **active PV>=10** (skipped
only during PV9 bootstrap) — NOT the same gate as the missing/incomplete
split (PV11+). `validateWithdrawalsDelegated` (`Ledger.hs:473-488`):
for every withdrawal whose target credential is a `KeyHashObj` (script-based
reward accounts are exempt — `credKeyHash` returns `Nothing` for those, so
they never enter `wdrlsKeyHashes`), require that account to have a
`dRepDelegationAccountStateL` (i.e. be DRep-delegated); the failing set
(`NonEmpty (KeyHash Staking)`) is reported together via
`failureOnNonEmpty ... ConwayWdrlNotDelegatedToDRep`. Checked against the
PRE-CERTS `accounts` (same certState-before-this-tx's-own-certs rule as
everything else in this block). So a full negative-test matrix for
withdrawal failures at PV>=11 needs THREE independent, co-accumulating
predicates: `ConwayWdrlNotDelegatedToDRep` (KeyHash reward account not
DRep-delegated), `ConwayWithdrawalsMissingAccounts` (unregistered or wrong
network), `ConwayIncompleteWithdrawals` (wrong amount) — all three CAN fire
simultaneously in one adversarial tx.

## Open / not independently re-verified in this pass
- `EraCertState`/`Withdrawals` exact CBOR shape (map `AccountAddress ->
  Coin`) — took on faith from type names, didn't pull the `EncCBOR
  Withdrawals` instance itself.
- POOL's zero contribution to a CERT-level predicate beyond wrapping —
  already covered in [[deleg-pool-govcert-verbatim-transitions]], not
  re-derived here.
