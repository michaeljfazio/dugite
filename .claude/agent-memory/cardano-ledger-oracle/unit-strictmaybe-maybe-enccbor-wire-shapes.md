---
name: unit-strictmaybe-maybe-enccbor-wire-shapes
description: Exact wire shape of (), StrictMaybe (default vs encodeNullStrictMaybe), and Maybe/encodeNullMaybe in cardano-ledger-binary — corrects a prior wrong memory that claimed () = array(0)
metadata:
  type: reference
---

# `()` / `StrictMaybe` / `Maybe` EncCBOR wire shapes

Verified verbatim @ pinned SHA `a88b60bdcf3248dfe5a2f9372c188c399233f479` (cardano-ledger,
2026-07-24), file `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/EncCBOR.hs` +
`.../Encoder.hs` + `.../Decoding/DecCBOR.hs`.

## `()` is CBOR `null`, NOT `array(0)` — CORRECTS PRIOR MEMORY

```haskell
instance EncCBOR () where
  encCBOR = const encodeNull        -- 0xf6, one byte

instance DecCBOR () where
  decCBOR = decodeNull
```

**This directly matters for `StashedAVVMAddresses era` in `NewEpochState`** (field [6]): the type
family resolves to `()` for every era except Shelley itself (`StashedAVVMAddresses ShelleyEra =
UTxO ShelleyEra; StashedAVVMAddresses _ = ()`). So for Conway (and every post-Shelley era), field
[6] of `NewEpochState`'s array(7) is a **bare CBOR `null` (`0xf6`)**, not `array(0)` (`0x80`). A
prior memory note (`newepochstate-complete-encoding.md`) said "array(0)" — that was wrong and has
been corrected in that file. If an encoder emits `0x80` there instead of `0xf6`, a decoder
expecting `()` via `decodeNull` will reject it outright (wrong major type: 4 vs 7).

## Three DIFFERENT optional-value encoders exist — do not conflate

1. **`encodeStrictMaybe`** — the DEFAULT `EncCBOR (StrictMaybe a)` instance:
   ```haskell
   instance EncCBOR a => EncCBOR (StrictMaybe a) where
     encCBOR = encodeStrictMaybe encCBOR
   encodeStrictMaybe encodeValue = \case
     SNothing -> encodeListLen 0                    -- array(0)
     SJust x  -> encodeListLen 1 <> encodeValue x    -- array(1)[x]
   ```
   Used whenever a field's declared type is `StrictMaybe a` and the encoder just calls `To field`
   / `encCBOR field` generically (i.e. no explicit override). Examples: `nesRu :: StrictMaybe
   PulsingRewUpdate` (NewEpochState field [4]); `drepAnchor :: StrictMaybe Anchor` in `DRepState`;
   `CommitteeMemberResigned :: StrictMaybe Anchor -> CommitteeAuthorization` (the anchor field is
   `To`'d generically, so it gets the array(0)/array(1) wrapper too).

2. **`encodeNullStrictMaybe`** — an explicit ALTERNATE encoder for `StrictMaybe a`, only used where
   the source explicitly calls it (never the default instance):
   ```haskell
   encodeNullStrictMaybe encodeValue = \case
     SNothing -> encodeNull          -- CBOR null, 0xf6
     SJust x  -> encodeValue x       -- bare value, no wrapper
   ```
   Used for: `spsMetadata`/`sppMetadata :: StrictMaybe PoolMetadata` in `StakePoolState` /
   `StakePoolParams` (`libs/cardano-ledger-core/.../State/StakePool.hs`), and pool relay port/IP
   fields (`StakePoolRelay`'s `SingleHostAddr`/`SingleHostName` port field).

3. **`encodeNullMaybe`** — for plain Haskell `Maybe a` (NOT `StrictMaybe`), same null-or-bare shape:
   ```haskell
   encodeNullMaybe encodeValue = \case
     Nothing -> encodeNull
     Just x  -> encodeValue x
   ```
   Used for `ConwayAccountState`'s `casStakePoolDelegation :: Maybe (KeyHash StakePool)` and
   `casDRepDelegation :: Maybe DRep` (field types are genuinely `Maybe`, not `StrictMaybe`, at this
   commit — corrects a prior memory that called these `encodeNullStrictMaybe`/`StrictMaybe`; wire
   shape is identical either way — null-or-bare — but the Haskell type name was wrong).

**Rule of thumb**: never assume a `StrictMaybe`/`Maybe` field's wire shape from the type alone —
check whether the specific `EncCBOR` instance for the containing record calls the field generically
(`To field` ⇒ default `encodeStrictMaybe`, array-wrapped) or explicitly overrides with
`encodeNullStrictMaybe`/`encodeNullMaybe` (null-or-bare, no array). Both patterns are common and
LOOK similar in source (`!> To x` vs `<> encodeNullStrictMaybe encCBOR x`).
