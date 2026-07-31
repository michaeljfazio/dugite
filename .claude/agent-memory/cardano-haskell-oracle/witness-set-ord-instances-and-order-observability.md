---
name: witness-set-ord-instances-and-order-observability
description: Exact Ord (WitVKey)/Ord BootstrapWitness comparison keys, ScriptHash-ascending script ordering, and decisive proof that witness-set wire order is unobservable at decode/validation but re-canonicalized on fresh MemoBytes construction
type: reference
---

Pinned to cardano-ledger master @ `58ba7795273f9301a9a198930e50a6ca1ee85238` (2026-08-01). Follow-up to [[variable-length-cbor-framing-and-blockbody-hash-over-original-bytes]] for the #939 witness-set-order audit.

## Ord instances (the sort keys)

`libs/cardano-ledger-core/src/Cardano/Ledger/Keys/WitVKey.hs` lines 58-68:
```haskell
instance Typeable kr => Ord (WitVKey kr) where
  compare x y =
    -- It is advised against comparison on keys and signatures directly,
    -- therefore we use hashes of verification keys and signatures for
    -- implementing this Ord instance. ...
    comparing wvkKeyHash x y <> comparing (hashTxBodySignature . wvkSignature) x y
```
Primary key = `wvkKeyHash :: KeyHash Witness` — the memoized Shelley witness key hash (blake2b224 of the raw vkey via `hashKey`, computed once in the `WitVKey` pattern-synonym smart constructor, NOT re-derived on each comparison). Tie-break on `hashTxBodySignature . wvkSignature` only matters if two different keys somehow hash-collide within one `Set` — practically never triggered, present only for `Ord`-law compliance. NOT derived on the whole record; hand-written.

`libs/cardano-ledger-core/src/Cardano/Ledger/Keys/Bootstrap.hs` lines 108-109:
```haskell
instance Ord BootstrapWitness where
  compare = comparing bootstrapWitKeyHash
```
Single key = `bootstrapWitKeyHash` (lines 112-146) — a **different hash algorithm** from `wvkKeyHash`: the Byron-style address-root hash, `hash_crypto (ADDRHASH) . hash_SHA3_256` over `prefix <> vkeyBytes <> chainCodeBytes <> attributesBytes` (the same computation as a Byron address root). Do not conflate the two "witness key hash" concepts — `WitVKey` sorts by plain blake2b224(vkey), `BootstrapWitness` sorts by the Byron addrRoot-style double hash.

## Script ordering (confirms user's claims 3/4 verbatim)

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxWits.hs` lines 511-541, `EncCBOR (AlonzoTxWitsRaw era)`:
```haskell
!> Omit null ( Key 1 $ E
    (encodeWithSetTag . mapMaybe getNativeScript . Map.elems)
    (Map.filter isNativeScript scripts) )
!> Omit null (Key 3 $ encodePlutus SPlutusV1)
!> Omit null (Key 6 $ encodePlutus SPlutusV2)
!> Omit null (Key 7 $ encodePlutus SPlutusV3)
  where
    encodePlutus slang = E
      (encodeWithSetTag . encCBOR . map plutusBinary . Map.elems)
      (Map.mapMaybe (toPlutusScript >=> toPlutusSLanguage slang) scripts)
```
`scripts :: Map ScriptHash (Script era)`. `Map.elems` on `Data.Map.Map` is defined to yield values in ascending-key order (structural invariant of the balanced tree) — so native scripts (key 1) and each Plutus language bucket (keys 3/6/7, kept as SEPARATE per-language maps) are each independently ScriptHash-ascending.

## DECISIVE: wire order is unobservable at decode/validation; canonicalized only on FRESH MemoBytes construction

Decode targets, same file, lines 613-702:
- `txwitsVKey`/`txwitsBoot` decode into `Set a` via `addrWitsSetDecoder`/`setOrListWitsDecoder`. PV>=12: `decodeNonEmptySetLikeEnforceNoDuplicates Set.insert (\s -> (Set.size s, s)) decCBOR`; PV 9-11: `allowTag setTag *> Set.fromList . NE.toList <$> decodeNonEmptyList decCBOR`; PV<9: `Set.fromList <$> decodeList decCBOR`. **Every branch folds into a genuine `Data.Set`.**
- native/plutus scripts decode into `Map ScriptHash (Script era)` / per-language `Map ScriptHash (Plutus l)` via `Map.fromList`/`noDuplicateNonEmptySetAsMapDecoderAnn` — also order-discarding.

`decodeNonEmptySetLikeEnforceNoDuplicates`/`decodeSetLikeEnforceNoDuplicates`/`decodeListLikeEnforceNoDuplicates` (`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Decoder.hs` lines 1043-1107) enforce **only** that `len == count` (post-insertion collection size equals number of elements decoded) — i.e. a pure duplicate check. **No order check exists anywhere in this call chain.** So: (a) rejection-for-non-canonical-order is categorically FALSE — no such check exists at any PV.

Is order therefore "(c) entirely unobservable"? **Depends which code path re-touches the value** — this is the crux, and a single a/b/c answer is wrong without qualification:

- **Passthrough/relay/forge-from-mempool** (the common node path: decode a tx, keep it in mempool, include it in a forged block by segment bytes) — `AlonzoTxWits era = MkAlonzoTxWits (MemoBytes (AlonzoTxWitsRaw era))`, and `EncCBOR (MemoBytes t) = encCBOR (MemoBytes _ bytes _hash) = encodePreEncoded (fromShort bytes)` (`libs/cardano-ledger-core/src/Cardano/Ledger/MemoBytes/Internal.hs` line 167) — this **replays the exact stored bytes verbatim**, whatever those bytes' internal order was (decode via `withSlice`/`Annotator` retains the original span — same discipline as [[variable-length-cbor-framing-and-blockbody-hash-over-original-bytes]]'s `AlonzoBlockBody` finding). **Non-canonical wire order survives byte-for-byte through this path.** True "(c) unobservable" here — Haskell does NOT re-sort on relay/forge-from-received-bytes.

- **Fresh construction / field-mutation via lens** — the `AlonzoTxWits` pattern synonym and any lens setter (`addrTxWitsL`, etc.) call `mkMemoizedEra @era $ AlonzoTxWitsRaw ...` (line 413), which is `mkMemoized v rawType = wrapMemoBytes (mkMemoBytes rawType (serialize v rawType))` (`MemoBytes/Internal.hs` lines 291-295) — `serialize v rawType` invokes `EncCBOR (AlonzoTxWitsRaw era)`, the `Map.elems`/`Set`-Foldable-based encoder above. Because the underlying containers are `Data.Set`/`Data.Map` (ordered by `Ord`), this **always** emits canonical ascending order regardless of insertion order. Any tool that builds/modifies a tx this way (cardano-api/cardano-cli transaction construction, any "rebuild from scratch" code) produces canonically-sorted output — "(b) accepted but not byte-identical" applies here.

**Recommendation given to dugite (in response to #939 follow-up)**: preserving wire order on the passthrough/relay/forge-from-received-bytes path is CORRECT and matches Haskell exactly — do not sort there; sorting would itself be a divergence from Haskell (which never re-sorts on relay). Sorting is only required for a fresh-construction code path (e.g. dugite-cli building a transaction from scratch, not derived from previously-decoded bytes) that needs byte-identical output vs. canonical Haskell/`cardano-cli` construction. Sort keys for that case: WitVKey by blake2b224(vkey) (`wvkKeyHash`); BootstrapWitness by the Byron addrRoot-style hash (`bootstrapWitKeyHash`, NOT the same hash as WitVKey); native and each Plutus-language script bucket independently by ScriptHash ascending.
