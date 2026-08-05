---
name: NewEpochState/EpochState/LedgerState/UTxOState complete CBOR encoding
description: Verified field order and array sizes for all top-level ledger state types from cardano-ledger source
type: reference
---

# Complete Ledger State CBOR Encoding (Verified from Source)

Source file: `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs`

## NewEpochState = array(7)

Encoder: `encodeListLen 7 <> encCBOR e <> encCBOR bp <> encCBOR bc <> encCBOR es <> encCBOR ru <> encCBOR pd <> encCBOR av`

```
array(7)
  [0] nesEL           :: EpochNo           (u64 integer)
  [1] nesBprev        :: BlocksMade        (map keyhash->natural)
  [2] nesBcur         :: BlocksMade        (map keyhash->natural)
  [3] nesEs           :: EpochState        (array(4))
  [4] nesRu           :: StrictMaybe PulsingRewUpdate  (array(0)=SNothing, array(1)[x]=SJust)
  [5] nesPd           :: PoolDistr         (map + total)
  [6] stashedAVVMAddresses :: StashedAVVMAddresses era
                         Conway (and every post-Shelley era): () -- CBOR null (0xf6), NOT array(0)!
                         Shelley only: UTxO ShelleyEra
```

CONFIRMED: nesPd (PoolDistr) is at index 5. stashedAVVM is index 6.

CORRECTION (re-verified 2026-08-05 @ pinned SHA a88b60bdcf3248dfe5a2f9372c188c399233f479):
`instance EncCBOR () where encCBOR = const encodeNull` in cardano-ledger-binary — `()` encodes as
a bare CBOR `null` simple value (one byte, `0xf6`), NOT `encodeListLen 0` (`0x80`). The line above
previously (wrongly) said "array(0)". See [[unit-strictmaybe-maybe-enccbor-wire-shapes]] for the
full breakdown of `()`/`StrictMaybe`/`Maybe` encoders, and [[utxostate-utxo-mempack-asymmetry-debugquery-empty]]
for the utxosUtxo field's own (unrelated) encoding subtlety.

STATUS: EpochState/LedgerState/UTxOState/NewEpochState/ChainAccountState field lists and encoding
order in this file were all re-verified VERBATIM against live source @ SHA
a88b60bdcf3248dfe5a2f9372c188c399233f479 (2026-07-24) on 2026-08-05 — no other drift found.

## EpochState = array(4)

Encoder uses `Rec EpochState !> To esChainAccountState !> To esLState !> To esSnapshots !> To esNonMyopic`

```
array(4)
  [0] esChainAccountState :: ChainAccountState  (array(2) [treasury, reserves])
  [1] esLState            :: LedgerState        (array(2))
  [2] esSnapshots         :: SnapShots          (array(4))
  [3] esNonMyopic         :: NonMyopic
```

NOTE: Field order in the Haskell data declaration is:
  esChainAccountState, esLState, esSnapshots, esNonMyopic
And the EncCBOR instance encodes them in THAT SAME ORDER.
Comment in source: "We get better sharing when encoding ledger state before snapshots"

## LedgerState = array(2)

Encoder: `encodeListLen 2 <> encCBOR lsCertState <> encCBOR lsUTxOState`

```
array(2)
  [0] lsCertState  :: CertState  (Conway: array(3))
  [1] lsUTxOState  :: UTxOState  (array(6))
```

CRITICAL: CertState is encoded FIRST even though the Haskell struct declares UTxOState first!
Comment in source: "encode delegation state first to improve sharing"

## UTxOState = array(6)

Encoder uses `Rec UTxOState !> E (encodeMap encodeMemPack encodeMemPack . unUTxO) utxosUtxo !> To utxosDeposited !> To utxosFees !> To utxosGovState !> To utxosInstantStake !> To utxosDonation`

```
array(6)
  [0] utxosUtxo          :: UTxO     (map with MemPack encoding — NOT standard encCBOR)
  [1] utxosDeposited     :: Coin     (integer)
  [2] utxosFees          :: Coin     (integer)
  [3] utxosGovState      :: GovState (Conway: ConwayGovState array(7))
  [4] utxosInstantStake  :: InstantStake  (ActiveStake VMap)
  [5] utxosDonation      :: Coin     (integer)
```

IMPORTANT: Field previously called `utxosStakeDistr` in older code is now `utxosInstantStake`.
The UTxO field uses MemPack encoding (encodeMap encodeMemPack encodeMemPack), NOT standard encCBOR.
There is NO UTxO-HD variant for ledger snapshots — this is the single CBOR encoding.

## ChainAccountState = array(2)

```
array(2)
  [0] casTreasury :: Coin
  [1] casReserves :: Coin
```

Source: `libs/cardano-ledger-core/src/Cardano/Ledger/State/ChainAccount.hs`
