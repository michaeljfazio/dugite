---
name: v2v1-paramname-vanrossem-extension-live
description: CRITICAL - PlutusV1 and PlutusV2 ParamName enums are NOT frozen at their Babbage-era counts; they get the full batch4a+batch5+batch6 extension at vanRossemPV=11 (batch4b earlier at plominPV=10 for V2 only), and this is ALREADY LIVE on preview today (332-length cost model arrays), not future/speculative work.
metadata:
  type: reference
---

Verified 2026-07-06 against IntersectMBO/plutus `master`
(`plutus-ledger-api/src/PlutusLedgerApi/{V1,V2,V3}/ParamName.hs`) AND
empirically against LIVE preview protocol parameters (both via
`preview.koios.rest/api/v1/cli_protocol_params` and the koios MCP
`cli_protocol_params` tool), preview `protocolVersion.major = 11` at time
of check.

## The finding

PlutusV1 and PlutusV2 `ParamName` are commonly assumed frozen at their
original era-launch sizes (V1=Alonzo batch1=~166/167 params, V2=Vasil+
Valentine batch1+2+3=175 params — see prior dugite framing "V2 params
0..174, 175 total, Babbage-era"). **This is now WRONG as of PV11.** Per
`builtinsIntroducedIn` (see [[plutus-builtin-availability-gate]] section 1):

```haskell
PlutusV1 -> [ (alonzoPV, batch1), (vanRossemPV, batch2++batch3++batch4++batch5++batch6) ]
PlutusV2 -> [ (vasilPV, batch1++batch2), (valentinePV, batch3), (plominPV, batch4b), (vanRossemPV, batch4a++batch5++batch6) ]
PlutusV3 -> [ (changPV, batch1..4), (plominPV, batch5), (vanRossemPV, batch6) ]
```

At `vanRossemPV=11`, **both V1 and V2 receive batch6** (`ExpModInteger,
DropList, LengthOfArray, ListToArray, IndexArray,
Bls12_381_G1/G2_multiScalarMul, InsertCoin, LookupCoin, UnionValue,
ValueContains, ValueData, UnValueData, ScaleValue`) — the same batch6 V3
gets. This is not hypothetical: preview is at PV11 *today* and the live
`costModels` map already carries the extended arrays:

| Language | Ground-truth length (source `ParamName` count AND live preview array) |
|---|---|
| PlutusV1 | **332** |
| PlutusV2 | **332** |
| PlutusV3 | **350** |

(Confirmed two independent ways: `wc -l` on the cleaned constructor list
from each `ParamName.hs`, AND `len(costModels["PlutusV1"/"PlutusV2"/"PlutusV3"])`
from a live `cli_protocol_params` fetch on preview — both agree exactly.)

## V2's tail starts exactly where dugite's old boundary was

V2 constructor index 174 (0-based) = `VerifySchnorrSecp256k1Signature'memory'arguments`
— i.e. dugite's previously-assumed "175 total" cutoff is exactly correct
as the boundary of the *original* Vasil+Valentine base, but **is not the
end of the array**. Index 175 onward (157 more entries) is:
`IntegerToByteString'cpu'arguments'c0` (batch4b, inserted first —
historically added at `plominPV=10`, before the big vanRossem bump) →
`ByteStringToInteger'*` (175-184) → `CekConstrCost'exBudgetCPU/Memory`,
`CekCaseCost'exBudgetCPU/Memory` (185-188) → full BLS12-381 batch4a
(G1/G2 add/neg/scalarMul/multiScalarMul/equal/compress/uncompress/
hashToGroup, millerLoop, mulMlResult, finalVerify) + `Keccak_256` +
`Blake2b_224` → batch5 bitwise ops (`AndByteString..Ripemd_160`,
starting at index 233) → batch6 (`ExpModInteger..ScaleValue`, starting
at index 279, ending at index 331 = `ScaleValue'memory'arguments'slope`,
the last of 332).

**V2's field-level ordering is NOT byte-identical to V3's tail** even
where the same builtins appear — two divergences beyond the position
shift of the IntegerToByteString/ByteStringToInteger block:
- V2's `DivideInteger`/`ModInteger`/`QuotientInteger`/`RemainderInteger`
  cpu cost-model shape (within the *original* 0-174 base, already
  implemented in dugite) uses a simple 2-field
  `intercept`/`slope`-style shape, whereas V3 uses the newer 7-field
  `c00,c01,c02,c10,c11,c20,minimum` quadratic-with-minimum shape for the
  same four builtins. This is a real, source-confirmed structural
  difference between languages for builtins both have had since genesis
  — not an artifact of the tail extension. Verify dugite's existing V2
  divide/mod/quotient/remainder walker matches the 2-field shape, not
  V3's 7-field shape, before assuming any code can be shared between the
  V2 and V3 walkers for these four builtins.
- V2's `ModInteger`/`RemainderInteger` memory models lack the extra
  `'minimum'` field V3 has for the same builtins (V2 mem = 2 fields
  intercept/slope only; V3 mem = 3 fields including minimum).

## CONFIRMED 2026-07-07: exact V1/V2 tails, cross-tag stability, ledger-side proof

Re-verified from scratch by direct `curl` of the raw `ParamName.hs` files
at BOTH `1.62.0.0` (what cardano-node 11.0.1 bundles) and `1.65.0.0`
(dugite's pinned corpus tag) — not from memory recall. Result: **byte-identical**
(`diff` exit 0) for both `V1/ParamName.hs` and `V2/ParamName.hs` across the
two tags; `Versions.hs` differs only by an unrelated addition
(`MaxBounds`/`maxBoundsByPV`, the 32-byte-header/1024-constr-field caps) —
`batch1`..`batch6` and `builtinsIntroducedIn` are textually unchanged. So
**no version drift between 1.62.0.0 and 1.65.0.0 for this question** —
dugite's corpus pin is safe to use as ground truth here.

Counts confirmed by parsing the `data ParamName = ... deriving stock` block
(stripping `--` comments first, since 2 constructors per V1/V2 file are
split across a comment line with the constructor name on a following line
lacking its own leading `|` — a naive per-line grep undercounts by exactly
that many): **V1 = 332, V2 = 332, V3 = 350**, both tags, confirmed again.

**Ledger-side proof this is the REAL on-chain expectation, not a
plutus-internal-only artifact**: `cardano-ledger` (`libs/cardano-ledger-core/
src/Cardano/Ledger/Plutus/CostModels.hs`):
```haskell
plutusVXParamNames :: Language -> [Text]
plutusVXParamNames PlutusV1 = P.showParamName <$> [minBound .. maxBound :: PV1.ParamName]
plutusVXParamNames PlutusV2 = P.showParamName <$> [minBound .. maxBound :: PV2.ParamName]
```
and `mkCostModel lang cm` dispatches straight to `PV1.mkEvaluationContext`/
`PV2.mkEvaluationContext` (the plutus-ledger-api function that zips the
`[Int64]` param list against `[minBound..maxBound::ParamName]` via
`tagWithParamNames`) — there is no separate, smaller, ledger-side length
check. Whatever `[minBound..maxBound]` is for the language's `ParamName` IS
the length the ledger validates and the length `PParamsUpdate`/genesis
`costModels` must supply. Confirms 332/332/350 is genuinely on-chain, not
a plutus-package-internal max that gets truncated before reaching consensus.

### V1 full tail (166 entries, 0-based indices 166-331 — dugite's current cutoff is 166, i.e. indices 0-165)
Order: `SerialiseData'{cpu'intercept,cpu'slope,mem'intercept,mem'slope}` (166-169) →
`VerifyEcdsaSecp256k1Signature'{cpu,mem}` (170-171) →
`VerifySchnorrSecp256k1Signature'{cpu'intercept,cpu'slope,mem}` (172-174) →
`CekConstrCost'{exBudgetCPU,exBudgetMemory}`, `CekCaseCost'{exBudgetCPU,exBudgetMemory}` (175-178) →
full BLS12-381 G1+G2 (add/compress/equal/hashToGroup/neg/scalarMul/uncompress) + finalVerify/millerLoop/mulMlResult (179-216, 38 fields) →
`Keccak_256` (217-219) → `Blake2b_224` (220-222) →
`IntegerToByteString'{cpu'c0,c1,c2,mem'intercept,slope}` (223-227, 5 fields) →
`ByteStringToInteger'{...same 5-field shape}` (228-232) →
batch5 bitwise (`AndByteString`..`Ripemd_160`) (233-278) →
batch6 (`ExpModInteger`..`ScaleValue`) (279-331, 53 fields, exactly the table in
[[v3-paramname-vanrossem-tail-297-349]] shifted to start at 279 instead of V3's 297).

### V2 full tail (147 entries, 0-based indices 185-331 — dugite's current cutoff is 185, i.e. indices 0-184)
Order: `CekConstrCost'{exBudgetCPU,exBudgetMemory}`, `CekCaseCost'{exBudgetCPU,exBudgetMemory}` (185-188) →
full BLS12-381 block (189-226, 38 fields) → `Keccak_256` (227-229) →
`Blake2b_224` (230-232) → batch5 bitwise (233-278, SAME as V1) →
batch6 (279-331, SAME as V1, byte-identical constructor names AND absolute
indices from here on). **No IntegerToByteString/ByteStringToInteger in V2's
tail** — those were already added to V2's base at Plomin (PV10), sitting at
indices 175-184 inside dugite's existing 185, immediately before the "End
of original cost model parameters" marker.

### Why V1 and V2 tails become byte-identical (same absolute index) from index 233 onward
V1's tail carries 19 extra entries before reaching the shared bitwise/batch6
block that V2's tail doesn't need: 9 from re-adding SerialiseData(4)+
VerifyEcdsaSecp256k1Signature(2)+VerifySchnorrSecp256k1Signature(3) (V2
already had these in its base since Vasil/Valentine) + 10 from re-adding
IntegerToByteString/ByteStringToInteger (V2 already had these in its base
since Plomin). 19 = exactly V1's tail length (166) minus V2's tail length
(147). This is also exactly why dugite's own existing V1=166/V2=185 base
counts differ by 19 — that 19-entry gap is fully accounted for by
Serialise(4)+Ecdsa(2)+Schnorr(3)+IntegerToByteString(5)+ByteStringToInteger(5)=19.

### Correction to a prior imprecise framing
IntegerToByteString/ByteStringToInteger cost shape is a **5-field-per-builtin**
shape (`cpu'arguments'c0`, `c1`, `c2` + `memory'arguments'intercept`, `slope`),
NOT 6 fields and NOT the 2-field div/mod shape. Confirmed directly from the
`ParamName.hs` source constructor list (5 constructors per builtin, both in
V1's tail at 223-232 and V2's base at 175-184) — do not use a 6-field walker
for these two builtins in any language.

## Dugite impact (issue-worthy)

`crates/dugite-*/.../cost_apply.rs`'s `apply_v1`/`apply_v2` (if V1 exists)
almost certainly only walk the pre-vanRossem base (175 for V2). Since
preview is *already* PV11 (per CLAUDE.md "Current Focus"), any V1/V2
Plutus script on preview today should be receiving a 332-length
cost-model array from the ledger. If dugite's walker only consumes 175
entries and either errors or silently drops/misindexes the rest, this is
a live, present-tense conformance bug on the network dugite is actively
soak-testing against — not a future-PV concern. Recommend treating this
as at least as urgent as the V3 batch6 (van Rossem) work already known
about. See [[v3-paramname-vanrossem-tail-297-349]] for the exact shape
table (same builtins, same shapes, apply to V1/V2's batch6 slice too —
just at different array offsets: V2 batch6 starts at index 279, not 297).
