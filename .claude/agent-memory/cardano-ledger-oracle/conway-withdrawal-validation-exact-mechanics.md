---
name: conway-withdrawal-validation-exact-mechanics
description: Exact PV-gated predicate-failure ADT/tag for a reward withdrawal that doesn't equal the account balance (CERTS at PV<=10, LEDGER at PV>=11), the separate and easy-to-miss ConwayWdrlNotDelegatedToDRep gate that fires BEFORE the amount check, witness/redeemer-index rules for the Rewarding purpose, and confirmation that Conway requires an EXACT-match withdrawal (partial withdrawal is a Dijkstra-only, unreleased relaxation). Live-verified 2026-08-02 against pinned SHA 4f7cb2d6874df70561e32147084ed82cee773e8a.
metadata:
  type: reference
---

## Source files (IntersectMBO/cardano-ledger, pinned 4f7cb2d6874df70561e32147084ed82cee773e8a)
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/Account.hs` — `withdrawalsThatDoNotDrainAccounts`, `categorizeWithdrawals`, `drainAccounts`
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs` — `ConwayCertsPredFailure`, PV<=10 path
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs` — `ConwayLedgerPredFailure`, `validateWithdrawalsDelegated`, PV>=11 path
- `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` — the three `hardforkConway*` PV-gate predicates
- `eras/conway/impl/src/Cardano/Ledger/Conway/UTxO.hs`, `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs` — `getWithdrawingScriptsNeeded`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/UTxO.hs` — `getShelleyWitsVKeyNeededNoGov` (`wdrlAuthors`)
- `eras/conway/impl/src/Cardano/Ledger/Conway/State/Account.hs` — `ConwayAccountState` (4-constructor sum on stakePool/DRep delegation presence)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Transition.hs` — `shelleyRegisterInitialAccounts` (genesis staking path)

## THE GOTCHA: a DRep-delegation gate runs BEFORE the amount check, independent of it

`conwayLedgerTransitionTRC` (Ledger.hs) runs, in order, per Phase-2-valid tx:
1. `validateTreasuryValue`, `validateRefScriptSize`
2. `unless (hardforkConwayBootstrapPhase pv) $ validateWithdrawalsDelegated accounts tx` — **runs whenever `pvMajor pv /= 9`**, i.e. at PV10 and PV11+ alike (bootstrap phase is PV9 ONLY: `hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9`)
3. only then, the amount-exactness check (CERTS at PV<=10, or inline in LEDGER at PV>=11 — see next section)

```haskell
validateWithdrawalsDelegated accounts tx =
  let wdrlsKeyHashes = [kh | (ra,_) <- Map.toList wdrls, Just kh <- [credKeyHash $ ra ^. accountAddressCredentialL]]
      isNotDRepDelegated keyHash = isNothing $ do
        accountState <- lookupAccountState (KeyHashObj keyHash) accounts
        accountState ^. dRepDelegationAccountStateL
      nonExistentDelegations = filter isNotDRepDelegated wdrlsKeyHashes
   in failureOnNonEmpty nonExistentDelegations ConwayWdrlNotDelegatedToDRep
```
**Only applies to `KeyHashObj` reward-account credentials** (`credKeyHash` filters to those; `ScriptHashObj` credentials are exempt from this specific gate). Fails with `ConwayWdrlNotDelegatedToDRep (NonEmpty (KeyHash Staking))` — tag **4** in `ConwayLedgerPredFailure` — carrying the list of un-DRep-delegated keyhashes, REGARDLESS of whether the withdrawn amount is otherwise exactly correct.

**Confirmed: genesis-delegated stake (via `ShelleyGenesisStaking`/`.staking.stake`) has NO DRep delegation by construction.** `ShelleyGenesisStaking`'s `sgsStake` is a plain `(KeyHash Staking, KeyHash StakePool)` list with no DRep field, and `shelleyRegisterInitialAccounts` -> `registerShelleyAccount` only ever sets `stakePoolDelegationAccountStateL`. Conway's own `ConwayAccountState` is a 4-constructor sum (`CASNoDelegation | CASStakePool | CASDRep | CASStakePoolAndDRep`); the only function that can populate `dRepDelegationAccountStateL` is `registerConwayAccount` with an explicit `Delegatee` carrying a DRep component — genesis staking registration structurally cannot supply one. **Practical consequence for a Conway-PV10 devnet genesis-delegated to pool1 with plain `.staking.stake`: the FIRST withdrawal attempt from that reward account will fail with `ConwayWdrlNotDelegatedToDRep`, not any amount-mismatch failure, until a `vote_delegation` / `stake_vote_delegation` cert (delegating to ANY DRep, including the built-in `drep_always_abstain`/`drep_always_no_confidence`) is submitted first.** This gates BOTH the positive and the negative withdrawal-amount test — build the DRep-delegation cert into the devnet setup fixture before either twin.

## Amount-exactness check: exact PV-gated ADT/tag

`withdrawalsThatDoNotDrainAccounts` (Account.hs) categorizes every entry in the tx's `Withdrawals` map against `Accounts`:
```haskell
categorizeWithdrawals amountAcceptable ... =
  -- amountAcceptable = (==) for the exact-match check
  -- returns Nothing if ALL withdrawals are registered AND exactly match balance
  -- else Just (missingOrWrongNetwork :: Withdrawals, wrongAmount :: Map AccountAddress (Mismatch RelEQ Coin))
```
Network mismatch and missing-account both fall into the SAME "invalid/missing" bucket (`lookupAccount` returns `Nothing` for either). Over-withdrawal and under-withdrawal are NOT distinguished — `amountAcceptable = (==)` (`RelEQ`), so any deviation in either direction lands in the SAME "incomplete" bucket as a `Mismatch { mismatchSupplied :: Coin, mismatchExpected :: Coin }` (wire order `[supplied, expected]`, confirmed via the default `EncCBOR (Mismatch r a)` instance — NOT swapped, unlike `ConwayTreasuryValueMismatch` which explicitly reverses it).

**PV<=10 (current local-devnet default, `shelley-genesis.json` -> `protocolVersion.major = 10`)**: `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule pv = pvMajor pv > natVersion @10` is FALSE, so the check runs in **CERTS**, as the base case of `conwayCertsTransition` when the certificate list has been fully consumed (i.e. BEFORE any certs in the same tx are applied — a withdrawal's exactness is checked against the PRE-cert-application balance, so you cannot deregister-and-then-satisfy the withdrawal check with the refund in the same tx):
```haskell
WithdrawalsNotInRewardsCERTS Withdrawals   -- tag 0 in ConwayCertsPredFailure
  -- carries: unWithdrawals invalid <> fmap mismatchSupplied incomplete
  -- i.e. missing-account AND wrong-amount cases MERGED into one Withdrawals map,
  -- and for the wrong-amount case only the SUPPLIED (attempted) value survives —
  -- the actual balance is NOT reported at this PV.
```
Wrapped up to `LEDGER` as `ConwayCertsFailure (WithdrawalsNotInRewardsCERTS ...)` — tag **2** in `ConwayLedgerPredFailure` wrapping tag **0** in `ConwayCertsPredFailure`.

**PV>=11**: `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule` is TRUE. The check moves directly into `conwayLedgerTransitionTRC` (LEDGER rule itself, not CERTS), split into two DISTINCT constructors — per the doc comment: *"both invalid withdrawals ... and incomplete withdrawals were being reported with WithdrawalsNotInRewardsCERTS but now ConwayWithdrawalsMissingAccounts and ConwayIncompleteWithdrawals are the new predicate failures ... to report the two separate cases"*:
```haskell
ConwayWithdrawalsMissingAccounts Withdrawals                              -- tag 8, missing account / wrong network
ConwayIncompleteWithdrawals (NonEmptyMap AccountAddress (Mismatch RelEQ Coin))  -- tag 9, wrong amount, NOW carries BOTH supplied and expected
```
Both are direct, un-wrapped constructors of `ConwayLedgerPredFailure` (no CERTS wrapping) at PV>=11.

**Answer key for the user's sub-questions**:
- Over-withdraw vs under-withdraw: SAME failure either way, at either PV regime (equality check, direction not distinguished).
- Unregistered/missing-account vs wrong-amount: SAME constructor at PV<=10 (merged into `WithdrawalsNotInRewardsCERTS`); genuinely DIFFERENT constructors at PV>=11 (`ConwayWithdrawalsMissingAccounts` vs `ConwayIncompleteWithdrawals`).
- ADT/location: NEVER `ConwayDelegPredFailure`, NEVER any `UTXOW`-level failure — it's `ConwayCertsPredFailure` (PV<=10) or `ConwayLedgerPredFailure` directly (PV>=11).

## Exact-match is Conway-current; Dijkstra (unreleased) has a partial-withdrawal relaxation

`withdrawalsThatExceedAccountBalance` (Account.hs, `RelLTEQ`/`<=` instead of `RelEQ`/`==`) exists in the SAME module but its only caller found repo-wide is `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Rules/Entities.hs`, gated by an internal `if/else` alongside `ExceededBalancesInWithdrawals`/`IncompleteWithdrawals` constructors. This is unreleased, forward-looking groundwork only — **for Conway (current mainnet/preprod/preview and this project's local devnet), withdrawal amount must equal the account balance EXACTLY; no partial withdrawal exists.**

## Witnessing and redeemer-pointer indexing for the Rewarding (Withdrawing) purpose

- vkey witness: `getShelleyWitsVKeyNeededNoGov`'s `wdrlAuthors` requires `credKeyHashWitness` of the withdrawal's `AccountAddress` credential for every `KeyHashObj`-credentialed reward account (`Nothing` for `ScriptHashObj` — no vkey witness needed there).
- script witness: `getWithdrawingScriptsNeeded` (Alonzo/UTxO.hs, reused verbatim by Conway) requires `credScriptHash` of the credential for every `ScriptHashObj`-credentialed reward account.
- `ConwayPlutusPurpose` wire tag for Rewarding/Withdrawing = **3** (`ConwayWithdrawing`), confirmed exact (see [[conway-plutus-purpose-witness-and-indexing]]).
- Redeemer pointer index = position in `Map.keys (unWithdrawals wdrls)`, i.e. ascending `Ord AccountAddress` = **`Network` first** (`Testnet < Mainnet`), then the staking `Credential`'s own `Ord` (`ScriptHashObj` sorts before `KeyHashObj`, then by hash bytes). For a single-network tx this reduces to "sorted by credential, script-before-key."

## Multi-withdrawal in one tx

`Withdrawals` wraps a plain `Map AccountAddress Coin` — withdrawing from N distinct reward accounts in one tx is structurally normal (no special multi-withdrawal restriction). Each entry is validated independently (all must simultaneously pass both the DRep-delegation gate, for KeyHash credentials, and the exact-balance gate); the required witness set is the UNION of the per-credential requirements above; the redeemer-pointer index set is simply the sorted-position enumeration over however many entries are present.

## Related
[[reward-maturity-mark-set-go-timeline]] — when a reward account's balance actually becomes nonzero.
[[conway-plutus-purpose-witness-and-indexing]] — full 6-purpose witness/indexing table this extends for purpose 3 specifically.
