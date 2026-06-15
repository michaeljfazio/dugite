---
name: uplc-builtin-flat-id-mismatch
description: UPLC flat wire IDs for BLS G1/G2 compress/uncompress/hashToGroup and UPLC 1.1.0 builtins were swapped; cost.rs DEFAULT table must match enum discriminants
type: reference
---

## Bug Pattern: Builtin flat wire ID vs. DEFAULT cost table misalignment

When adding builtins to `BuiltinId` enum in `term.rs`, TWO places must stay in sync:
1. The enum discriminant value (which becomes the flat wire encoding)
2. The positional entry in `builtin_cost_table()` in `builtin/cost.rs` (indexed by discriminant)

If the discriminant order changes (e.g. correcting a Zone 1/2 wire encoding bug), the DEFAULT cost table entries must be reordered to match.

## Zone 1 fix (#761 Bug 2) — BLS G1/G2 compress/uncompress/hashToGroup

**Symptom**: `appendByteString: type error: expected ByteString, got Discriminant(3)` on mainnet Conway PlutusV3 SPEND txs (e.g. 71579b77). Budget exhaustion (mem_remaining=75 at declared mem=10M) after the initial fix.

**Root cause (two-layer)**:
1. Wire ID mismatch: dugite had `G1_HashToGroup=58, G1_Compress=59, G1_Uncompress=60` but Plutus spec has `Compress=58, Uncompress=59, HashToGroup=60`. Same 3-way rotation for G2 at 65/66/67.
2. After fixing discriminants, `builtin_cost_table()` entries at positions 58/59/60 and 65/66/67 were still in old order — `cost_pair(G1_Compress=58)` returned HashToGroup costs (LinearInX shape), causing `refill_builtin` to consume wrong number of params AND charge wrong costs.

**Correct order (term.rs discriminants AND cost.rs table entries)**:
- 58=G1_Compress (Constant/Constant), 59=G1_Uncompress (Constant/Constant), 60=G1_HashToGroup (LinearInX/Constant)
- 65=G2_Compress (Constant/Constant), 66=G2_Uncompress (Constant/Constant), 67=G2_HashToGroup (LinearInX/Constant)

## Zone 2 fix — UPLC 1.1.0 builtins (89-100)

**Plutus spec order (IntersectMBO/plutus DefaultFun Flat instance)**:
- 89=LengthOfArray, 90=ListToArray, 91=IndexArray, 92=G1_MultiScalarMul, 93=G2_MultiScalarMul, 94=InsertCoin, 95=LookupCoin, 96=UnionValue, 97=ValueContains, 98=ValueData, 99=UnValueData, 100=ScaleValue

**Why the conformance tests didn't catch this**: Upstream UPLC conformance tests use text-format scripts (not flat), so the flat wire ID table is never exercised by them. The bug only manifests on real on-chain PlutusV3 transactions decoded from flat.

## Test: tx_v3_spend_71579b77.json

Mainnet Conway PV9 epoch 523 slot 140988497, is_valid=true. Declared mem=10,000,000 cpu=10,000,000,000. With both fixes applied, passes exactly within declared budget.

## Files changed
- `crates/dugite-uplc/src/term.rs` — enum discriminants
- `crates/dugite-uplc/src/builtin/cost.rs` — DEFAULT cost table positions 58/59/60, 65/66/67, 89-100
- `crates/dugite-uplc/tests/phase2_onchain_budget.rs` — regression tests `onchain_v3_spend_71579b77_validates`, `cek_v3_spend_71579b77_flat_evaluates`
- `crates/dugite-uplc/tests/fixtures/phase2_onchain/tx_v3_spend_71579b77.json` — fixture
