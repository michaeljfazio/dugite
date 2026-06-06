---
name: kes-signing-verification
description: Complete KES signing/verification path for TPraos (Shelley/Allegra/Mary/Alonzo) and Praos (Babbage+) headers — what bytes are signed, field order, period offset, opcert verification
type: reference
---

## Q: What bytes does KES sign?

**Answer: A FRESH RE-SERIALIZATION via `getSignableRepresentation`, NOT the original wire bytes.**

Both `BHBody` (TPraos) and `HeaderBody` (Praos) implement `SignableRepresentation`:

```haskell
-- TPraos BHBody — cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/BHeader.hs:169-170
instance Crypto c => SignableRepresentation (BHBody c) where
  getSignableRepresentation bh = serialize' (pvMajor (bprotver bh)) bh

-- Praos HeaderBody — ouroboros-consensus/ouroboros-consensus-protocol/src/.../Praos/Header.hs:102-103
instance Crypto crypto => SignableRepresentation (HeaderBody crypto) where
  getSignableRepresentation hb = serialize' (pvMajor (hbProtVer hb)) hb
```

`serialize'` = `BSL.toStrict . serialize version` = fresh `encCBOR` run.
Source: `cardano-ledger/libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding.hs:56-57`

**The `BHeader`/`Header` type IS memoized** (MemoBytes stores original wire bytes for hashing), but `verifySignedKES` extracts the `BHBody`/`HeaderBody` struct value and calls `getSignableRepresentation` on it → re-encodes via `EncCBOR`.

**Implication for Rust**: The KES message is `serialize'(pvMajor, bhbody)`. If re-encoding your `BHBody` struct gives different bytes than the wire bytes, verification will fail. The `EncCBOR (BHBody c)` instance must match exactly.

## TPraos: `BHeader` / `BHBody` structure

File: `cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/BHeader.hs`

```
BHeader c = array(2)[BHBody, KES.SignedKES]
```

**`BHBody` field list (in EncCBOR order):**
The array length = 9 + listLen(OCert) + listLen(ProtVer).
OCert has listLen=4, ProtVer has listLen=2. Total = 9+4+2 = 15 elements.

1. `bheaderBlockNo` — BlockNo (uint)
2. `bheaderSlotNo`  — SlotNo (uint)
3. `bheaderPrev`    — PrevHash (null | bytes(32))
4. `bheaderVk`      — VKey BlockIssuer (bytes(32))
5. `bheaderVrfVk`   — VRF.VerKeyVRF (bytes(32), encoded via `encodeVerKeyVRF`)
6. `bheaderEta`     — `VRF.CertifiedVRF c Nonce`  = the NONCE VRF proof
7. `bheaderL`       — `VRF.CertifiedVRF c Natural` = the LEADER VRF proof
8. `bsize`          — Word32 (uint)
9. `bhash`          — Hash HASH EraIndependentBlockBody (bytes(32))
-- then OCert group (4 flat fields, NOT wrapped in extra array):
10. `ocertVkHot`    — KES VerKey (bytes)
11. `ocertN`        — Word64 counter (uint)
12. `ocertKESPeriod`— Word (uint)
13. `ocertSigma`    — DSIGN.SignedDSIGN (bytes)
-- then ProtVer group (2 flat fields):
14. `pvMajor`       — Version (uint)
15. `pvMinor`       — Word (uint)

**TPraos has TWO separate VRF proofs** (bheaderEta for nonce, bheaderL for leader).
OCert and ProtVer are inline (group encoding, no wrapping array).

## Praos: `Header` / `HeaderBody` structure

File: `ouroboros-consensus/ouroboros-consensus-protocol/src/.../Praos/Header.hs`

```
Header crypto = array(2)[HeaderBody, KES.SignedKES]
```

**`HeaderBody` field list (in EncCBOR order):** (Rec encoder, 10 fields)

1. `hbBlockNo`  — BlockNo (uint)
2. `hbSlotNo`   — SlotNo (uint)
3. `hbPrev`     — PrevHash (null | bytes(32))
4. `hbVk`       — VKey BlockIssuer (bytes(32))
5. `hbVrfVk`    — VRF.VerKeyVRF (bytes, via `encodeVerKeyVRF`)
6. `hbVrfRes`   — `VRF.CertifiedVRF c InputVRF` = SINGLE unified VRF proof
7. `hbBodySize` — Word32 (uint)
8. `hbBodyHash` — Hash HASH EraIndependentBlockBody (bytes(32))
9. `hbOCert`    — OCert (via `mapCoder unCBORGroup From`, i.e. it IS wrapped in a 4-element array in the Praos case unlike the flat group encoding in TPraos)
10. `hbProtVer` — ProtVer

**Key difference**: Praos drops the separate nonce VRF (`bheaderEta`) and uses a single `InputVRF` value for both nonce and leader. OCert is encoded as a proper 4-element CBOR array in the Rec encoder (not a flat group).

## KES Verification Call Site

### TPraos (OCERT STS rule)
File: `cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/OCert.hs:99`

```haskell
verifySignedKES () vk_hot t bhb sigma ?!: InvalidKesSignatureOCERT kp_ c0_ t
```
where:
- `sigma` = the `KES.SignedKES` extracted from `BHeader` pattern match: `BHeader bhb sigma`
- `bhb` = `BHBody` struct (NOT raw bytes)
- `t` = KES period offset = `kp_ - c0_` (see below)

### Praos (`doValidateKESSignature`)
File: `ouroboros-consensus/ouroboros-consensus-protocol/src/.../Protocol/Praos.hs:637`

```haskell
KES.verifySignedKES () vk_hot t (Views.hvSigned b) (Views.hvSignature b)
  ?!: InvalidKesSignatureOCERT kp_ c0_ t praosMaxKESEvo
```
where `Views.hvSigned b = headerBody` (the `HeaderBody` struct).

### TPraos `verifyHeaderIntegrity` (consensus layer)
File: `ouroboros-consensus/ouroboros-consensus-cardano/src/shelley/.../Protocol/TPraos.hs:76`

```haskell
isRight $ SL.verifySignedKES () ocertVkHot t hdrBody hdrSignature
 where SL.BHeader hdrBody hdrSignature = hdr
```

### Praos `verifyHeaderIntegrity` (consensus layer)
File: `ouroboros-consensus/ouroboros-consensus-cardano/src/shelley/.../Protocol/Praos.hs:130`

```haskell
isRight $ KES.verifySignedKES () ocertVkHot t headerBody headerSig
 where Header{headerBody, headerSig} = header
```

## KES Period Offset Computation

### TPraos (OCert.hs:82-92)
```haskell
kp@(KESPeriod kp_) <- liftSTS $ kesPeriod s    -- kesPeriod = slot `div` slotsPerKESPeriod
let t = if kp_ >= c0_ then kp_ - c0_ else 0
verifySignedKES () vk_hot t bhb sigma
```

### Praos (Praos.hs:651-654)
```haskell
kp@(KESPeriod kp_) =
  KESPeriod . fromIntegral $ s `div` praosSlotsPerKESPeriod
let t = if kp_ >= c0_ then kp_ - c0_ else 0
```

So: `t = floor(slot / slotsPerKESPeriod) - ocert_kes_period_start`

- `verifySignedKES () vk_hot t body sig`
- The `()` is the `ContextKES` (unit)
- `vk_hot` = `ocertVkHot` from the operational certificate
- `t` = relative period offset (0-based since start of this KES key)
- `body` = `BHBody` or `HeaderBody` struct

## OCert Cold-Key Signature Verification

Also in TPraos OCert.hs:98 and Praos.hs:635:

```haskell
verifySignedDSIGN vkcold (ocertToSignable $ bheaderOCert bhb) tau ?! InvalidSignatureOCERT n c0
```

`ocertToSignable` produces `OCertSignable` with `SignableRepresentation`:
```haskell
getSignableRepresentation (OCertSignable vk counter period) =
  rawSerialiseVerKeyKES vk   -- raw bytes of KES hot vkey
  <> BS.word64BE counter     -- 8 bytes big-endian counter
  <> BS.word64BE (fromIntegral $ unKESPeriod period)  -- 8 bytes big-endian KES start period
```
(NOT CBOR — raw concatenation)

## `verifySignedKES` internals

```haskell
verifySignedKES ctxt vk j a (SignedKES sig) = verifyKES ctxt vk j a sig
```

`verifyKES` (via `verifyDSIGN` internally) calls `getSignableRepresentation a` → `serialize' pvMajor body` → Ed25519 signature verification over those bytes.

## Summary: Wire format of KES-signed message

For **TPraos** (Shelley/Allegra/Mary/Alonzo), the KES-signed bytes are:
```
serialize' (pvMajor bprotver) bhbody
= CBOR array(15)[blockNo, slotNo, prevHash, issuerVk, vrfVk,
                  etaVrfCert, leaderVrfCert,
                  bodySize, bodyHash,
                  ocertHotVk, ocertN, ocertKESPeriod, ocertSigma,
                  pvMajor, pvMinor]
```

For **Praos** (Babbage+), the KES-signed bytes are:
```
serialize' (pvMajor hbProtVer) headerBody
= CBOR Rec(HeaderBody)[blockNo, slotNo, prevHash, issuerVk, vrfVk,
                        unifiedVrfCert,
                        bodySize, bodyHash,
                        ocert(array(4)[hotVk,n,kesPeriod,sigma]),
                        protVer]
```

## Permalinks

- TPraos BHBody + SignableRepresentation: https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/BHeader.hs#L139-L170
- TPraos BHeader memoized type: https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/BHeader.hs#L245-L273
- TPraos OCERT rule verifySignedKES: https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/OCert.hs#L76-L99
- TPraos OCert.ocertToSignable: https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/OCert.hs#L144-L162
- Praos HeaderBody + SignableRepresentation: https://github.com/IntersectMBO/ouroboros-consensus/blob/main/ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/Header.hs#L56-L103
- Praos doValidateKESSignature: https://github.com/IntersectMBO/ouroboros-consensus/blob/main/ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos.hs#L618-L663
- TPraos verifyHeaderIntegrity: https://github.com/IntersectMBO/ouroboros-consensus/blob/main/ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Protocol/TPraos.hs#L73-L92
- Praos verifyHeaderIntegrity: https://github.com/IntersectMBO/ouroboros-consensus/blob/main/ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Protocol/Praos.hs#L127-L146
- serialize': https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding.hs#L56-L57
- mkMemoized (shows annotation is NOT used for signing): https://github.com/IntersectMBO/cardano-ledger/blob/master/libs/cardano-ledger-core/src/Cardano/Ledger/MemoBytes/Internal.hs#L291-L292
- verifySignedKES: https://github.com/IntersectMBO/cardano-base/blob/master/cardano-crypto-class/src/Cardano/Crypto/KES/Class.hs#L548-L556
