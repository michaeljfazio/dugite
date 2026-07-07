---
name: byron-duplicate-inputs-and-shelley-translation
description: Byron→Shelley translation zeroes all Shelley pot fields (utxosFees, utxosDeposited, casTreasury, ssFee); Byron fees are burned and absorbed implicitly into reserves via maxSupply-minus-liveUTxO. Re-verified live 2026-07-06 after prior memory file was lost.
metadata:
  type: reference
---

## Status
Re-verified live against `master` on IntersectMBO/cardano-ledger on 2026-07-06 (the
original file with this name was indexed in MEMORY.md but had been lost/never
written — this replaces it with directly-fetched, line-cited content).

## Canonical function

- Module: `Cardano.Ledger.Shelley.API.ByronTranslation`
- File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/API/ByronTranslation.hs`
- Entry points (both still exist, names unchanged from what's commonly assumed):
  - `translateToShelleyLedgerState :: FromByronTranslationContext -> EpochNo -> Byron.ChainValidationState -> NewEpochState ShelleyEra`
    — thin wrapper, just pulls `Byron.cvsUtxo cvs` and calls the `FromUtxo` version.
  - `translateToShelleyLedgerStateFromUtxo :: FromByronTranslationContext -> EpochNo -> Byron.UTxO -> NewEpochState ShelleyEra`
    — the actual translation logic (confirmed correct name, not renamed/moved).

## Exact construction (lines 103-181 as of 2026-07-06)

```haskell
translateToShelleyLedgerStateFromUtxo transCtxt epochNo utxoByron =
  NewEpochState
    { nesEL = epochNo
    , nesBprev = BlocksMade Map.empty
    , nesBcur = BlocksMade Map.empty
    , nesEs = epochState
    , nesRu = SNothing
    , nesPd = def
    , stashedAVVMAddresses = ...
    }
  where
    reserves :: Coin
    reserves =
      word64ToCoin (fbtcMaxLovelaceSupply transCtxt) <-> sumCoinUTxO utxoShelley

    epochState :: EpochState ShelleyEra
    epochState =
      EpochState
        { esChainAccountState =
            ChainAccountState
              { casTreasury = Coin 0
              , casReserves = reserves
              }
        , esSnapshots = emptySnapShots
        , esLState = ledgerState
        , esNonMyopic = def
        }
        & prevPParamsEpochStateL .~ pparams
        & curPParamsEpochStateL .~ pparams

    utxoShelley = translateUTxOByronToShelley utxoByron

    ledgerState =
      LedgerState
        { lsUTxOState =
            UTxOState
              { utxosUtxo = utxoShelley
              , utxosDeposited = Coin 0
              , utxosFees = Coin 0
              , utxosGovState = emptyGovState
              , utxosInstantStake = mempty
              , utxosDonation = mempty
              }
        , lsCertState = mkShelleyCertState def dState
        }
```

`emptySnapShots` (from `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs`,
line ~393-395):
```haskell
emptySnapShots :: SnapShots
emptySnapShots =
  SnapShots emptySnapShot (calculatePoolDistr emptySnapShot) emptySnapShot emptySnapShot (Coin 0)
```
The 5th field is `ssFee`, i.e. `ssFee = Coin 0`.

`sumCoinUTxO` (from `libs/cardano-ledger-core/src/Cardano/Ledger/State/UTxO.hs`):
```haskell
sumCoinUTxO :: EraTxOut era => UTxO era -> Coin
sumCoinUTxO = sumAllCoin . unUTxO
```
Sums the ADA value of every TxOut currently in the (translated) UTxO map — i.e. the
live circulating supply at the Byron/Shelley boundary.

## Key facts (CONFIRMED, not hedged)

1. **`utxosFees = Coin 0`** and **`ssFee = Coin 0`** (via `emptySnapShots`) are both
   explicitly set at translation. Also `utxosDeposited = Coin 0` and
   `casTreasury = Coin 0`. All four pot-like fields start at zero.
2. **Byron fees are burned** — they never appear anywhere in the post-translation
   Shelley `NewEpochState`. There is no field that carries forward an "accumulated
   Byron fee total"; no such total is ever computed or read from `Byron.UTxO` /
   `Byron.ChainValidationState` during translation.
3. **This is not a "reset of an accumulated value" — it's simply that no accumulator
   ever existed.** Byron's ledger state (`Byron.UTxO`, the UTxO map inside
   `ChainValidationState`) has no running fee-pot field at all. In Byron, a tx's fee
   is just the arithmetic gap between summed inputs and summed outputs; nothing in
   Byron ever materializes that gap into a stored value anywhere. So Haskell isn't
   "zeroing out Byron fees" — Byron never had a fee pot to zero.
4. **Reserves implicitly absorb the burned value.** `reserves = maxLovelaceSupply -
   sumCoinUTxO(translated, zero-value-TxOuts-filtered)`. Every lovelace ever paid as
   a Byron fee is, by construction, absent from the live UTxO (it was never an
   output). Since `reserves` is defined purely as `maxSupply - circulatingSupply`,
   all historically-burned Byron fees flow into `casReserves`, not into
   `utxosFees`/`ssFee`. This is a byproduct of the reserves formula, not a separate
   explicit "add old fees to reserves" step — there is no such step in the code.
5. Translation also filters out zero-value Byron TxOuts before summing/converting
   (`translateUTxOByronToShelley`, `txOutShelley ^. coinTxOutL /= zero`) — a few
   testnets injected zero-value TxOuts at Byron genesis; Shelley onward disallows
   zero-value outputs so they're dropped here.
6. `dup TxIn` handling note (carried over from the original memory summary, not
   re-verified this session but consistent with `Map.fromList` semantics in
   `translateUTxOByronToShelley`): duplicate `TxIn` keys collapse via `Map.fromList`
   (last-wins on duplicate keys), not rejected — this is about map construction
   during translation, separate from the pot-zeroing question above.

## Rust translation notes (Dugite)

- Relevant crate/module: wherever dugite implements the Byron→Shelley
  `on_era_transition` handler (per project memory
  `project_conway_from_genesis_rupd_fix_2026_05_28` naming convention, likely in
  `dugite-ledger` epoch-transition/era-bootstrap code).
- Confirms the working theory in the P0 question this file was written to answer:
  dugite's `epoch_fees` (Shelley-side fee pot) must be **hard-set to 0** at the
  Byron→Shelley boundary, matching `utxosFees = Coin 0` — not carried forward from
  any Byron-side fee accumulation dugite might independently track.
  - If dugite maintains a `ss_fee`/snapshot-fee equivalent, it must also be zeroed
    at this boundary (matches `emptySnapShots`'s `ssFee = Coin 0`).
  - `casTreasury` equivalent must also be zeroed (not just reserves/fees).
  - `utxosDeposited` equivalent must also be zeroed (no Shelley deposits exist
    pre-translation).
  - The reserves computation must be `maxLovelaceSupply - sum(translated live UTxO
    value, after dropping zero-value TxOuts)` — if dugite's `epoch_fees` (or
    anything else) is *added into or subtracted from* this reserves formula to
    "account for" Byron fees, that would double-count: the burn is already fully
    and correctly reflected by the fact that fee-consumed lovelace was never a
    UTxO output in the first place.
