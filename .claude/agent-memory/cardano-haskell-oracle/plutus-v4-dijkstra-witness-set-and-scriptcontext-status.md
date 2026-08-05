---
name: plutus-v4-dijkstra-witness-set-and-scriptcontext-status
description: Definitive upstream status of PlutusV4/Dijkstra as of 2026-08-05 — witness-set wire key, Language ordinal, cost model, builtins, and the two-repos-out-of-sync ScriptContext situation
type: reference
---

Researched live 2026-08-05 for dugite issue #1000 (PlutusV4 script evaluation).
Pinned SHAs (CORRECTED 2026-08-05 — the original write-up had these two
SWAPPED; verified via `gh api repos/<owner>/<repo>/commits/<sha>` before this
correction): cardano-ledger `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`
("Merge pull request #5950 ... Drop `EncCBORGroup BlockBody` in Dijkstra",
2026-08-04T21:48:51Z), plutus `c4f649fac4a18929f550ffebf07c9e7371355d9d`
("Remove the `deriving-aeson` dependency (#7871)", 2026-08-05T01:03:29Z).
Both cloned shallow from default branch (master). Every other citation in
this file already refers to the correct repo by name at each site — only
the header pairing above was backwards.

## Verdict: Dijkstra/PlutusV4 is upstream, on master, but explicitly self-described as non-functional scaffolding. The plutus repo and cardano-ledger repo have TWO INDEPENDENT, NOT-YET-RECONCILED notions of "V4".

cardano-ledger CHANGELOG.md verbatim (10.6 section): "Introduction of a new
`Dijkstra` era and `PlutusV4` placeholders that for the most part mimic Conway
era bahavior for now". 10.7/11.0 section: "Plethora of features for Dijkstra
era, which as a whole is not functional yet." `eras/dijkstra/impl/src/.../TxInfo.hs`:
`toPlutusScriptPurpose _ = error "stub: PlutusV4 not yet implemented"` — a
literal runtime-crash stub in the EraPlutusTxInfo 'PlutusV4 DijkstraEra instance.

## 1. Dijkstra transaction_witness_set CBOR key for plutus_v4_scripts: DOES NOT EXIST YET

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxWits.hs` (shared by Alonzo
through Dijkstra — Dijkstra's own `TxWits.hs` just does
`type TxWits DijkstraEra = AlonzoTxWits DijkstraEra`, no new field). The
production `DecCBOR (Annotator (AlonzoTxWitsRaw era))` instance branches on
`ifDecoderVersionAtLeast (natVersion @12)` — natVersion@12 IS the Dijkstra/PV12
gate — and BOTH branches (`decoderByKey` for PV>=12, `txWitnessField` for
below) only handle keys 0-7 identically to Conway:
0=vkey,1=native,2=bootstrap,3=v1,4=data,5=redeemers,6=v2,7=v3. Any other key:
`decoderByKey acc = \case ... _ -> Nothing` / `txWitnessField n = invalidField n`.
This is dugite's exact existing shape — dugite is currently byte-exact correct
here and should NOT add a plutus_v4_scripts field.

The CDDL spec generator (`eras/dijkstra/impl/cddl/lib/.../HuddleSpec.hs`,
aspirational/test-fixture code, NOT the production decoder) has:
```
instance HuddleRule "transaction_witness_set" DijkstraEra where
  huddleRuleNamed pname p =
    pname
      =.= mp
        [ ... idx 0..7 as above ...
        -- TODO: Add plutus_v4_script at index 8 once AlonzoTxWitsRaw encoder/decoder supports it
        ]
```
So key **8** is the planned slot per the CDDL author's own TODO, not yet wired.
Confirmed independently by `eras/dijkstra/impl/testlib/.../Examples.hs`:
"NOTE: PlutusV4 scripts are NOT part of Dijkstra's transaction_witness_set
CDDL (only V1/V2/V3 are). Including them here would cause a roundtrip
failure as they get silently dropped during serialization." — and by
`Binary/Golden.hs`'s `witsDuplicatePlutus` test helper, which enumerates
SPlutusV1=3/SPlutusV2=6/SPlutusV3=7 and hits
`l -> error "Unsupported plutus version"` for SPlutusV4.

PlutusV4 scripts CAN currently appear only as: (a) TxOut reference scripts
(`referenceScriptTxOutL`, script tag 4 in the `script` CDDL sum
`arr [4, plutus_v4_script]`), and (b) auxiliary_data_map key 5 (parallel to
witness keys but a DIFFERENT map — TxAuxData's own SparseKeyed field 5, see
`Alonzo/TxAuxData.hs` `auxDataField 5 = fieldA (addPlutusScripts PlutusV4)`,
already implemented). NOT as witness-set scripts.

## 2. AlonzoTxWitsRaw is NOT "one field per language" — it's a single unified Map

`atwrScriptTxWits :: !(Map ScriptHash (Script era))` — ONE map holding
native + V1 + V2 + V3 scripts together, keyed by hash. Per-language
separation happens ONLY at the CBOR wire layer: the `EncCBOR` instance
(`Keyed (\a b c d e f g h -> ...) !> Key 3 $ encodePlutus SPlutusV1 !> Key 6
$ encodePlutus SPlutusV2 !> Key 7 $ encodePlutus SPlutusV3`) filters the
unified map by language per key at encode time and merges back on decode
(`toScript @'PlutusV1 d <> toScript @'PlutusV2 e <> toScript @'PlutusV3 f`).
So dugite's wire-level "one Vec<Vec<u8>> per language key" model is still
correct for the WIRE FORMAT; it just doesn't need to mirror Haskell's
internal unified-map storage choice.

## 3. Language ordinal / wire details — confirmed, matches dugite

`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs`:
```
data Language = PlutusV1 | PlutusV2 | PlutusV3 | PlutusV4
  deriving (Eq, Generic, Show, Ord, Enum, Bounded, Ix, Read)
```
V4 = index 3 (0-indexed), `EncCBOR Language = encodeEnum`.
`instance PlutusLanguage 'PlutusV4 where plutusLanguageTag _ = 0x04` — matches
dugite's existing hash-prefix byte. `guardPlutus`: `PlutusV4 -> natVersion @12`
— PlutusV4 scripts (as ref-scripts/aux-data) are undecodable below major PV
12, confirming dugite's PV12=Dijkstra assumption is exactly right on both the
era-switch AND the language-gate axis.

Critically: `newtype PlutusArgs 'PlutusV4 = PlutusV4Args {unPlutusV4Args ::
PV3.ScriptContext}` and `evaluatePlutusRunnable`/`mkTermToEvaluate` for V4 all
call straight into `PV3.evaluateScriptRestricting`/`PV3.deserialiseScript`/
`P.mkTermToEvaluate P.PlutusV3 ...` — i.e. **cardano-ledger's PlutusV4, as
wired today, IS PlutusV3 semantics under a V4 tag**, byte-for-byte.

## 4/5. plutus repo: DefaultFun batch7 + BuiltinSemanticsVariant — NO V4 tie-in exists

`plutus-ledger-api/src/PlutusLedgerApi/Common/Versions.hs`:
`data PlutusLedgerLanguage = PlutusV1 | PlutusV2 | PlutusV3` — **no V4
constructor in the plutus package's own ledger-language type**, confirmed by
plutus issue #7342 ("Define PlutusV4 script context", OPEN) whose first
checklist item "Define `PlutusV4` version in `plutus-ledger-api`" (i.e. add
it to this very sum type) is UNCHECKED. Because of this, `machineParametersFor`
and `V3.EvaluationContext.mkEvaluationContext`'s PV->variant selector CANNOT
have a V4 case — the type wouldn't compile. `BuiltinSemanticsVariant DefaultFun`
has exactly 5 constructors (A-E, `plutus-core/.../Default/Builtins.hs` line
~1079), no F.

`batch7 :: [DefaultFun] = [MultiIndexArray, Policies]` (wire IDs 101, 102) —
"Builtins that are implemented but not yet approved for release in any
protocol version." This is NOT gated to any ledger language (can't be — no LL
to gate under) and is unrelated to the V4 effort; do not conflate. No
"sized"-family builtins exist anywhere in the plutus repo — dugite's deleted
`builtin/sized.rs` placeholder had no upstream basis.

## 6. Cost model params: V4 = literally PV3.ParamName, no distinct list exists

`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/CostModels.hs`:
```haskell
costModelInitParamCount lang = case lang of
  ...
  PlutusV4 ->
    -- This number will continue to change until we are ready to hard fork into Dijkstra era
    251
...
plutusVXParamNames PlutusV4 = P.showParamName <$> [minBound .. maxBound :: PV3.ParamName]
...
mkEvaluationContext cm = case lang of
  ...
  PlutusV4 -> PV3.mkEvaluationContext
```
No `PlutusLedgerApi.V4.ParamName` module exists in plutus (confirmed: only
`V4.hs`, `V4/Contexts.hs`, `V4/Data/Contexts.hs`, `Data/V4.hs` — no ParamName
file). V4's cost model param list IS V3's `PV3.ParamName` list, dynamically
(`[minBound..maxBound]`), not a frozen count — and cardano-ledger's own
comment flags this whole area as provisional pre-hard-fork.

## 7. THE NUANCE: plutus repo has a SEPARATE, more advanced "V4 ledger api types" effort that cardano-ledger has NOT adopted yet

Initial grep for the literal string "PlutusV4" in the plutus repo returned
ZERO hits — WRONG CONCLUSION if stopped there. plutus PR #7846 "Plutus V4
ledger api types as per #7342" (merged 2026-07-24T03:05:29Z, commit
`fdbe32b20bd02a4f27a9654ecc3648a2c8fa2968`, base `master`) added
`plutus-ledger-api/src/PlutusLedgerApi/V4.hs`, `V4/Contexts.hs`,
`V4/Data/Contexts.hs`, `Data/V4.hs` — a genuinely NEW, DISTINCT
`ScriptContext`/`TxInfo`/`ScriptPurpose`/`ScriptInfo`/`TopTxInfo`/
`TopTxInfoSimplified` set (not a re-export of V3's), matching Dijkstra's
nested-tx model: `TxInfo` has `txInfoSubTxIx :: Maybe Integer`,
`txInfoDirectDeposits`, `txInfoAccountBalanceIntervals`,
`txInfoRequiredTopLevelGuards`, `txInfoGuards :: [Credential]` (replacing V3's
signatories per CIP-112 "Guards"/"Observers"); `ScriptPurpose` adds `Guarding
ScriptHash Integer`; `ScriptInfo` adds `GuardingScript Integer (Maybe
TopTxInfo)`; `TopTxInfo` wraps `[TxInfo]` sub-transactions +
`AccountBalanceIntervals`. PR body: "ledger API types are not fully finalized
yet." Companion PR #7876 "Add Plutus V4 Address type" is still OPEN
(unmerged) as of research date.

**But cardano-ledger's `Language.hs` (pinned SHA above, 12 days AFTER #7846
merged) still wraps `PV3.ScriptContext`, not this new `V4.Contexts.ScriptContext`
— zero cardano-ledger references to `PlutusLedgerApi.V4` anywhere in the repo.**
The two repos' notions of "PlutusV4" are independently scaffolded and not yet
reconciled. Driving CIPs per plutus#7342: CIP-112 (Observe script
type/Guards) and CIP-118 (Nested Transactions) — neither is in the
well-known/stable CIP list; both still in flux.

## Implication for dugite

Do NOT implement a `plutus_v4_scripts` witness-set field, a V4-specific
BuiltinSemanticsVariant, or a V4-specific ParamName/cost-model list — none of
these exist upstream at either pinned SHA. The only currently-real,
byte-exact-checkable V4 surface is: (a) Language enum ordinal 3 / wire tag
0x04 / hash prefix 0x04, (b) natVersion@12 gate, (c) V4-as-reference-script
and V4-in-aux-data-map-key-5 decoding using **V3 script bytes semantics**
(since ledger-side V4 == V3 evaluation today), (d) cost model wire slot
present but content = whatever V3's ParamName currently is. Treat dugite's
existing `ScriptRef::PlutusV4` + cost-model slot 3 as already correctly
"versioned but inert" — do not add witness/builtin/cost-model divergence
ahead of upstream; that would be overclaiming compliance with something that
doesn't exist yet. Re-check plutus#7342 and cardano-ledger CHANGELOG.md before
any future V4 work — this is a fast-moving, explicitly-WIP area.
