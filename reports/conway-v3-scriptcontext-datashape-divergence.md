# Conway Phase-2 ScriptContext Divergence Report

**Date:** 2026-06-13  
**Analyst:** Tech-Lead Agent  
**Slot range:** 138832413 – 140990836 (mainnet)

---

## Summary

Four mainnet transactions are logged as phase-2 divergences by dugite — `uplc says scripts fail but block is_valid=true — trusting on-chain consensus`. These split into two independent root causes.

| Tx | Slot | Error | Root Cause |
|----|------|-------|-----------|
| `51f495aa` | 138832413 | `unMapData on non-Map Data` | V2 script + Propose redeemer: should be silently skipped |
| `b2a591ac` | 139262228 | `unMapData on non-Map Data` | V2 script + Propose redeemer: should be silently skipped |
| `71579b77` | 140985919 | `appendByteString: type error: expected ByteString, got Discriminant(3)` | V3 Spend script CEK evaluation divergence |
| `e998e761` | 140990836 | `appendByteString: type error: expected ByteString, got Discriminant(3)` | V3 Spend script CEK evaluation divergence |

---

## Error Class A: V1/V2 Script + Propose/Vote Redeemer (txs 51f495aa, b2a591ac)

### Transaction anatomy

Both transactions carry:
- **PlutusV2 script** at witness key 7 (Conway witness set — V2 scripts slot)
- **Proposing redeemer** at witness key 5 (Conway redeemer map), key `(5, 0)` where 5 = Propose tag
- **Governance proposal** at tx body key 20: `ParameterChange` procedure

The script hash (V2 hash of the bytes at key 7) matches the payment credential of no script-locked input. The script is being invoked as a **proposing script** (governance proposal procedure validation), not a spending script.

### Haskell ground truth

Haskell `cardano-ledger` collects phase-2 scripts via `collectTwoPhaseScriptInputs` → `neededPlutusScripts`. For Conway-era, this calls `transPlutusPurposeV1V2` on each redeemer purpose. For `ConwayProposing` and `ConwayVoting`:

```haskell
-- cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs
transPlutusPurposeV1V2 :: ... => ConwayPlutusPurpose AsItem era -> Either ... PV1.ScriptPurpose
transPlutusPurposeV1V2 purpose =
  case purpose of
    ConwaySpending  item -> ...
    ConwayMinting   item -> ...
    ConwayCertifying item -> ...
    ConwayRewarding item -> ...
    -- WILDCARD arm for V1/V2 — Vote and Propose are NOT V1/V2 purposes:
    purpose -> Left $ inject $ PlutusPurposeNotSupported @era $ hoistPlutusPurpose toAsItem purpose
```

The `Left` result causes the script to be **excluded from the collection** — it is never passed to the CEK evaluator. The transaction is valid because Haskell simply does not invoke this V2 script for these purposes.

### Dugite bug

**File:** `/Users/michaelfazio/Source/dugite/crates/dugite-uplc/src/eval_redeemer.rs`  
**Lines:** 162–166

```rust
RedeemerTag::Vote | RedeemerTag::Propose | RedeemerTag::Guarding => {
    return Err(PhaseTwoError::Internal(format!(
        "eval_resolved_redeemer: tag {:?} is not valid for V1/V2",
        r.tag
    )));
}
```

This hard-errors instead of silently skipping. The `PhaseTwoError::Internal` propagates through `phase_two.rs` and causes a divergence log at the ledger layer.

### Precise fix

The fix should be in **`crates/dugite-uplc/src/phase_two.rs`**, in the redeemer iteration loop at line 286. Before calling `eval_resolved_redeemer`, filter out resolved redeemers that Haskell would never invoke:

```rust
// Mirror Haskell `transPlutusPurposeV1V2` which returns Left
// PlutusPurposeNotSupported for Vote/Propose/Guarding in V1/V2 scripts.
// These are not included in neededPlutusScripts, so they are never evaluated.
use crate::redeemer_resolve::ScriptLanguage;
use dugite_primitives::transaction::RedeemerTag;
if matches!(resolved_r.language, ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2)
    && matches!(resolved_r.tag, RedeemerTag::Vote | RedeemerTag::Propose | RedeemerTag::Guarding)
{
    // V1/V2 scripts are not called for governance purposes — skip.
    // Haskell: transPlutusPurposeV1V2 returns Left PlutusPurposeNotSupported.
    continue;
}
```

The defensive `return Err(...)` in `eval_redeemer.rs:162-166` can then be softened to `unreachable!()` since the filter above prevents it from ever being reached, OR it can remain as a belt-and-suspenders guard — but it must not produce an error that surfaces as a divergence. The primary fix is the filter.

Also update `eval_redeemer.rs:162` to simply return a synthetic success (or unreachable) rather than an error, as defense in depth:

```rust
RedeemerTag::Vote | RedeemerTag::Propose | RedeemerTag::Guarding => {
    // Should have been filtered before reaching here.
    // Haskell: PlutusPurposeNotSupported — V1/V2 not invoked for governance.
    unreachable!(
        "eval_resolved_redeemer: V1/V2 script with governance tag {:?} should be filtered pre-eval",
        r.tag
    );
}
```

---

## Error Class B: V3 Spend Script, `appendByteString: expected ByteString, got Discriminant(3)` (txs 71579b77, e998e761)

### Transaction anatomy

Both transactions carry:
- **PlutusV3 script** at witness key 7 (Conway witness set, key 7 = V3 scripts)
- **Spend redeemer** at witness key 5 (Conway map format) with tag=0 (Spend), index=0
- **Datum** at witness key 4: raw bytes `b"case-01-4423"` (12 bytes), which is `Data::B(b"case-01-4423")`
- Script address: `addr1w85maq5sl5xn0rtph49cy08hfmd79pnj9u80s7g0kle3seg7sh7pr` (tx 71579b77) and `addr1wy77wpqgdfltt4tszrvxtqmgk4hvr9x4u6jxyn0t4mfs2cgsugfsk` (tx e998e761)
- Script hash (V3 = blake2b_224(0x03 || script_bytes)): `e9be8290fd0d378d61bd4b823cf74edbe286722f0ef8790fb7f31865` and `3de704086a7eb5d57010d8658368b56ec194d5e6a4624debaed30561` respectively

Both scripts are **UPLC version 1.1.0** (verified from flat encoding header `0x01 0x01 0x00`), using the SOP `constr`/`case` terms introduced in CIP-0085.

### Error characterization

`Discriminant(3)` is the Rust memory discriminant of `Value::Builtin` (the 4th variant, 0-indexed as 3) in `/Users/michaelfazio/Source/dugite/crates/dugite-uplc/src/machine/value.rs`:

```rust
pub enum Value {
    Const(Constant),  // 0
    Lambda { ... },   // 1
    Delay { ... },    // 2
    Builtin { id, forces, args },  // 3  ← Discriminant(3)
    Constr { tag, args },  // 4
}
```

The error means a **partially-saturated builtin function** was passed as an argument to `appendByteString` where a `Constant::ByteString` value was expected.

### Suspected root cause

The scripts use UPLC 1.1.0 `case` on `Constr` values. Dugite's CEK machine supports `Term::Constr` and `Term::Case` (CIP-0085 SOP). The error `Value::Builtin` passed to `appendByteString` is unusual and suggests that one of the intermediate values the script computes — likely derived from the V3 ScriptContext — has an unexpected shape.

The most plausible cause (not yet definitively confirmed without a runtime dump): a field in the V3 `TxInfo` or `ScriptContext` is encoded differently between dugite and Haskell, causing the script's internal data-extraction path to diverge, and the error falls through to an error-reporting branch that constructs a message using `appendByteString`. This pattern is common in Aiken/Plutarch contracts that use `appendByteString` in their trace/error messages.

The key candidates for ScriptContext encoding divergence that could cause a `Value::Builtin` to appear:

1. **V3 `txInfoSignatories`**: Each signatory should be `Data::B(28_bytes)`. Confirmed correct in dugite (`data_bs28`, `padded_signer_to_pubkeyhash` truncates 32→28).

2. **V3 `txid`**: Should be `B(32_bytes)` (bare bytestring), NOT `Constr 0 [B bytes]`. Confirmed correct in dugite (`data_bs32(&self.txid)` at TxInfoV3::to_data() line 848).

3. **`scriptContextRedeemer`**: The redeemer `Data::Constr(0, [B(2192)])` is embedded as-is in the ScriptContextV3. Confirmed correct.

4. **V3 `txInfoFee`**: Should be bare `I(lovelace)`, NOT a Value map. Confirmed correct.

5. **The `ScriptInfo::Spending` datum wrapper**: For `Some(d)` where `d = Data::B(b"case-01-4423")`, dugite emits `Constr 0 [B("case-01-4423")]`. This is `Just (Datum d)` = correct.

6. **`txInfoRedeemers` map key encoding**: The `ScriptPurpose::Spending(outref)` key for the redeemers map uses `ScriptPurpose::to_data_v3()` which emits `Constr 1 [Constr 0 [B(32), I(idx)]]`. This is the V3 bare-txid form — confirmed correct.

**Without running the node with `DUGITE_DUMP_APPLIED_DIR` to capture the exact applied flat term**, definitive byte-level identification of the divergent field is not possible in static analysis. However, the error is not from the ScriptContext shape described above.

**Alternative hypothesis**: The UPLC 1.1.0 SOP `case` handling in dugite's CEK machine (`machine/step.rs` `Frame::Cases` dispatch) may have a subtle difference from Haskell when processing a script that uses `case` on a value derived from a builtin application. Specifically, `Frame::ApplyValue` pushes payload values in reverse order at lines 407-408:

```rust
for arg in payload.into_iter().rev() {
    kont.push(Frame::ApplyValue { argument: arg })?;
}
```

This applies the first payload arg first (top of stack = first arg). This is the correct behavior per the Plutus 1.1.0 spec (branch receives `arg_0`, then `arg_1`, ...). However, if the `Constr` that is being case-matched has its args in the wrong order due to a serialization issue in dugite's ScriptContext builder, the script could receive args in the wrong order and attempt to apply `appendByteString` to a `Value::Builtin`.

### Verification method needed

To confirm the exact divergent field, run with:

```bash
DUGITE_DUMP_APPLIED_DIR=/tmp/phase2-dump dugite-node run ...
```

Then compare the flat-encoded applied program against Haskell's reference:

```bash
# After capturing applied-Spend-0.flat
aiken uplc decode applied-Spend-0.flat
# Compare against Haskell cardano-ledger's collectTwoPhaseScriptInputs output
```

The divergent field will appear as a structural mismatch in the arguments passed to the validator.

---

## Files and Lines Affected

| File | Location | Issue |
|------|----------|-------|
| `crates/dugite-uplc/src/phase_two.rs` | Line 286 (for loop over `resolved_redeemers`) | Add pre-eval filter for V1/V2 + Vote/Propose/Guarding |
| `crates/dugite-uplc/src/eval_redeemer.rs` | Lines 162–166 | Change from `PhaseTwoError::Internal` to `unreachable!()` |

---

## Fix Priority

**Error Class A** (51f495aa, b2a591ac): **High** — root cause confirmed, fix is simple and safe.

**Error Class B** (71579b77, e998e761): **Medium** — requires runtime data capture to confirm the divergent field before implementing a fix. The scripts are UPLC 1.1.0 and are `is_valid=true` on-chain, so the divergence is a false negative that only costs log noise. No ledger state corruption results.
