---
name: alonzo-auxdata-decoder-full-key-guard-and-cddl-audit
description: Full DecCBOR(AlonzoTxAuxData) audit for #1013-class dugite bug — ALL keys 2-5 guardPlutus-gated (not just key 5), unknown keys hard-fail in BOTH pre/post-PV12 branches, per-era CDDL key-range confirmed
type: reference
---

Pinned commit verified real via `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`:
`4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (master HEAD as of 2026-08-05, merge
of PR #5950). Same commit as [[dijkstra-ppu-keys-38-39-and-auxdata-key5]] —
this entry supersedes that one's key-5-only framing with the FULL picture
across all four Plutus keys, and adds the unknown-key-rejection + CDDL
findings that entry didn't check.

## 1. `DecCBOR (Annotator (AlonzoTxAuxDataRaw era))` — full instance

File: `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxAuxData.hs` (lines ~245-315
at the pinned SHA). Top-level dispatch by peeked CBOR token type
(`decodeTxAuxDataByTokenType`, same file, ~line 320): `TypeMapLen*` →
Shelley-format decoder (bare metadata map, no tag), `TypeListLen*` →
Allegra-format decoder (`[metadata, [native_script]]`), `TypeTag*` → the
tag-259 sparse-map decoder that is this memo's subject. Within the tag-259
branch there IS a PV>=12 split, exactly mirroring `PParamsUpdate`:

```haskell
( ifDecoderVersionAtLeast
    (natVersion @12)
    ( do
        assertTag 259
        decodeSparseKeyed
          TypeName
          []
          (pure emptyAlonzoTxAuxDataRaw)
          decoderByKey
    )
    decodeAlonzo   -- decodeAlonzo = decode $ TagD 259 $
                    --   SparseKeyed "AlonzoTxAuxData" (pure emptyAlonzoTxAuxDataRaw) auxDataField []
)
```

`decoderByKey` (PV>=12 path) and `auxDataField` (PV<12, `Coders`-based path)
are two DIFFERENT MECHANISMS with IDENTICAL field logic — not a behavior
change, a refactor. Neither has an unknown-key fallback:

```haskell
decoderByKey acc = \case
  0 -> Just $ do !x <- decCBOR; pure (\ad -> ad {atadrMetadata = x}) <$> acc
  1 -> Just $ do !x <- sequence <$> decodeStrictSeq decCBOR
                 pure (\scripts ad -> ad {atadrNativeScripts = atadrNativeScripts ad <> scripts}) <$> x <*> acc
  2 -> decodeAddPlutus PlutusV1
  3 -> decodeAddPlutus PlutusV2
  4 -> decodeAddPlutus PlutusV3
  5 -> decodeAddPlutus PlutusV4
  _ -> Nothing
  where
    decodeAddPlutus lang = Just $ do
      guardPlutus lang
      !x <- decCBOR
      pure $ addPlutusScripts lang x <$> acc

auxDataField 0 = fieldA (\x ad -> ad {atadrMetadata = x}) From
auxDataField 1 = fieldAA (\x ad -> ad {atadrNativeScripts = atadrNativeScripts ad <> x})
                          (D (sequence <$> decodeStrictSeq decCBOR))
auxDataField 2 = fieldA (addPlutusScripts PlutusV1) (D (guardPlutus PlutusV1 >> decCBOR))
auxDataField 3 = fieldA (addPlutusScripts PlutusV2) (D (guardPlutus PlutusV2 >> decCBOR))
auxDataField 4 = fieldA (addPlutusScripts PlutusV3) (D (guardPlutus PlutusV3 >> decCBOR))
auxDataField 5 = fieldA (addPlutusScripts PlutusV4) (D (guardPlutus PlutusV4 >> decCBOR))
auxDataField n = invalidField n
```

**Both branches hard-fail on an unrecognized key** — same class as #1013
(`ProtocolParamUpdate`'s `SparseKeyed`/`decodeSparseKeyed`). Traced to source:

- PV>=12: `decoderByKey acc _ = Nothing` for key not in {0..5} →
  `decodeSparseKeyed`'s `step` (`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Decoder.hs:1279-1283`):
  `Nothing -> failMsg $ "Unknown field key " <> show key` → `fail (show name <> ": " <> msg)`.
- PV<12: `auxDataField n = invalidField n` (`Decoding/Coders.hs:159-160`) =
  `field (const Void) (Invalid n)`. `decodeClosed (Invalid k) = invalidKey k`
  (`Decoding/Coders.hs:524`). `invalidKey` is defined in
  `Cardano.Ledger.Binary.Plain` (`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Plain.hs:95-96`):
  `invalidKey k = cborError $ DecoderErrorCustom msg (Text.pack $ show k)` where
  `msg = "<TypeRep> not a valid key:"` — a hard `MonadFail`/`cborError` throw,
  re-exported through `Decoding/Decoder.hs` into `Decoding/Coders.hs`.

No leniency anywhere. dugite's `_ => { r.skip()?; }` catch-all in
`decode_alonzo_auxiliary_data` (`crates/dugite-serialization/src/decode/era_alonzo.rs`)
is a confirmed divergence, identical class to #1013.

## 2. Keys 2/3/4 guard status — ALL FOUR plutus keys are `guardPlutus`-gated, not just key 5

Correcting the framing in [[dijkstra-ppu-keys-38-39-and-auxdata-key5]] (which
only checked key 5): **every one of keys 2, 3, 4, 5 calls `guardPlutus <lang>`**
before `decCBOR`ing the script list, in both the PV>=12 and PV<12 branches
(see quoted source above). `guardPlutus` —
`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs:639-647`:

```haskell
guardPlutus :: Language -> Decoder s ()
guardPlutus lang =
  let v = case lang of
        PlutusV1 -> natVersion @5
        PlutusV2 -> natVersion @7
        PlutusV3 -> natVersion @9
        PlutusV4 -> natVersion @12
   in unlessDecoderVersionAtLeast v $
        fail (show lang <> " is not supported until " <> show v <> " major protocol version")
```

So: key 2 (PlutusV1) needs decoder PV>=5, key 3 (PlutusV2) needs PV>=7, key 4
(PlutusV3) needs PV>=9, key 5 (PlutusV4) needs PV>=12. This directly answers
"is a PlutusV2 list at key 3 rejected during Alonzo-era decoding (PV 5-6)":
**YES** — `guardPlutus PlutusV2` requires PV>=7, so a key-3 entry decoded
while the CBOR decoder's tracked protocol version is 5 or 6 (real-chain
Alonzo) fails with `"PlutusV2 is not supported until major protocol version 7"`.
The gate is PV-keyed, not era-keyed — but since each era's `ProtVer` range is
itself bounded by the HFC (Alonzo can only ever carry PV 5-6 on a real chain),
the PV gate reproduces the era-CDDL cap in section 3 below as an emergent
property, not by checking the era type directly. `AlonzoTxAuxDataRaw` is one
type reused verbatim across eras (see below) — there is no per-era `decCBOR`
override that could check era identity even if it wanted to.

## 3. CDDL differs per era — key range genuinely widens Alonzo→Babbage→Conway→Dijkstra

Files: `eras/{alonzo,babbage,conway,dijkstra}/impl/cddl/data/{alonzo,babbage,conway,dijkstra}.cddl`
(Huddle-generated, checked into the repo, exercised by
`Test.Cardano.Ledger.<Era>.Binary.CddlSpec`). The `auxiliary_data_map`
production genuinely differs — this is a REAL per-era cap, not stale docs:

```
-- alonzo.cddl:626-628
auxiliary_data_map =
  #6.259({? 0 : metadata, ? 1 : [* native_script], ? 2 : [* plutus_v1_script]})

-- babbage.cddl:651-658
auxiliary_data_map =
  #6.259(
    { ? 0 : metadata
    , ? 1 : [* native_script]
    , ? 2 : [* plutus_v1_script]
    , ? 3 : [* plutus_v2_script]
    }
  )

-- conway.cddl:826-834 adds ? 4 : [* plutus_v3_script]
-- dijkstra.cddl:898-907 adds ? 5 : [* plutus_v4_script]
```

Alonzo CDDL caps at key 2, Babbage extends to key 3, Conway to key 4,
Dijkstra to key 5 — confirming the user's hypothesis exactly. But this is
enforced at RUNTIME purely via the `guardPlutus` PV floor from section 2,
NOT via any era-keyed branch in the Haskell decoder. Confirmed by checking
`eras/{babbage,conway,dijkstra}/impl/src/Cardano/Ledger/<Era>/TxAuxData.hs`
(all three, full file, no decode override): each is just
`type TxAuxData <Era>Era = AlonzoTxAuxData <Era>Era` plus lens/validate
wiring — the literal `AlonzoTxAuxDataRaw`/`DecCBOR` code in section 1 is
reused byte-for-byte across Alonzo/Babbage/Conway/Dijkstra. **Confirms the
premise of dugite's ONE shared `decode_alonzo_auxiliary_data`** — Haskell
itself has exactly one implementation here too.

## Implication for dugite's fix (#1013-class, aux-data variant)

Correct fix is a single unconditional rule for the shared decoder, not
per-era branching:
1. Reject any tag-259 map key outside `{0,1,2,3,4,5}` unconditionally (all
   eras/PVs) — matches both `decoderByKey _ -> Nothing` and
   `auxDataField n = invalidField n`.
2. For keys 2-5 specifically, additionally gate on the tx's protocol version:
   key2 needs PV>=5, key3 PV>=7, key4 PV>=9, key5 PV>=12 — reproduces
   `guardPlutus`. In practice key 2's gate is unreachable-false (no era with
   PV<5 can reach the tag-259 branch legitimately) but should still be
   coded explicitly, matching Haskell, for the adversarial-input case
   (CLAUDE.md: dugite-node is adversarial-deployment software).
3. Do NOT try to key reachability off `era` alone (era→max-key mapping) —
   Haskell doesn't have that check either; it falls out of PV, and coding it
   as an era-keyed cap would diverge from Haskell the moment a hard-fork
   boundary tx straddles two eras at the same key.

See also [[dijkstra-ppu-keys-38-39-and-auxdata-key5]] (key 5 / `eraMaxLanguage`
enumeration-drop caveat, still accurate) and the #1013 memory in project
history (`.claude/agent-memory/` PPU sparse-keyed precedent).
