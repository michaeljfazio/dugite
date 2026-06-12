# Issue #759 — Phase-1 False Positive: Duplicate Input in Babbage Tx

## Summary

**Classification:** SMALL+SAFE one-liner fix, strong regression tests, zero behavioral change for Conway+.

**Root cause:** Phase-1 Rule 1b fired unconditionally for all protocol versions. Haskell only enforces uniqueness at PV >= 9 (Conway+). The rule must be gated on `protocol_version_major >= 9`.

---

## Evidence

### Transaction under investigation

```
tx hash : 5ca83e216eb4fce8e907ed3597bd290261136ae97fc4cd7fbd5eadf9bbedf09f
block   : 10294413
epoch   : 484  (PV8 = Babbage)
slot    : 123,728,795
```

### Wire structure of body key 0 (spend inputs)

Decoded from Koios MAINNET REST (`/api/v1/tx_cbor`):

```
Key 0 (inputs): plain array(3) — NO tag 258
  [0]  ab2829f03f185af3eb048e1cd256899c7ddb575a112fcfe41a324e94d21707aa  idx=1
  [1]  ab2829f03f185af3eb048e1cd256899c7ddb575a112fcfe41a324e94d21707aa  idx=1   ← IDENTICAL to [0]
  [2]  3bd13603e5e051f0b501da15260d26ad5948e4a3db54f4e3038416edf4f4d95e  idx=0

Key 13 (collateral): array(1)
  [0]  3bd13603e5e051f0b501da15260d26ad5948e4a3db54f4e3038416edf4f4d95e  idx=0

Key 18 (reference_inputs): array(1)
  [0]  e92ac620bf095094d58a19616d1b6debb9b1cf305870264b1a58446c51d7f4b0  idx=0
```

`ab2829f03f...#1` appears **twice in the spend inputs**. This is not a collateral/reference overlap — it is a genuine wire-level duplicate in a plain (non-tag-258) CBOR array.

---

## Haskell Behavior (PV8, Babbage)

### Reference: `cardano-ledger-binary` `decodeSet`

At PV < 9, `decodeSet` routes through the **lenient path**:

```haskell
-- cardano-ledger-binary Cardano.Ledger.Binary.Decoding.Coders
-- PV < 9:  Set.fromList <$> decodeList decoder    (silent dedup)
-- PV >= 9: decodeSetEnforceNoDuplicates (hard fail)
```

`Set.fromList [A, A, B]` = `{A, B}` — the duplicate is silently dropped. No exception, no predicate failure.

### Reference: `BabbageUtxoPredFailure` constructors

```haskell
-- eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxo.hs
data BabbageUtxoPredFailure era
  = AlonzoInBabbageUtxoPredFailure !(AlonzoUtxoPredFailure era)
  | BabbageNonDisjointRefInputs !(Set TxIn)  -- PV9-10 only
```

`AlonzoUtxoPredFailure` has **no** `DuplicateInput` or `DuplicateInputs` constructor. The complete list of Alonzo UTXO failures is: OutsideValidityIntervalUTxO, InputSetEmptyUTxO, FeeTooSmallUTxO, BadInputsUTxO, ValueNotConservedUTxO, OutputTooSmallUTxO, OutputBootAddrAttrsTooBig, MaxTxSizeUTxO, WrongNetwork, WrongNetworkWithdrawal, OutputTooBigUTxO, InsufficientCollateral, ScriptsNotPaidUTxO, ExUnitsTooBigUTxO, CollateralContainsNonADA, WrongNetworkInTxBody, OutsideForecast, TooManyCollateralInputs, NoCollateralInputs.

**Conclusion:** Haskell silently accepts a Babbage tx with duplicate spend inputs. No predicate failure is produced. The duplicate is eliminated by `Set.fromList` before any validation sees it.

---

## Root Cause in Dugite

**File:** `crates/dugite-ledger/src/validation/phase1.rs`

**Location:** lines 624-633 (before fix)

```rust
// Rule 1b: No duplicate inputs   ← NO era/PV gate
{
    let mut seen = HashSet::new();
    for input in &body.inputs {
        if !seen.insert(input) {
            errors.push(ValidationError::DuplicateInput(input.to_string()));
        }
    }
}
```

The check runs unconditionally for ALL protocol versions. For the Babbage tx at PV8 with `ab2829f03f...#1` appearing twice, dugite emits `DuplicateInput("ab2829f03f…")` and rejects the tx. Haskell accepts it.

**Note:** The stake-distribution deduplication in `eras/common.rs:184-189` (the `seen_inputs` HashSet filter) is already correct — it silently deduplicates before consuming UTxOs. That code path only needed the Phase-1 validation gate removed.

---

## The Fix

**File:** `crates/dugite-ledger/src/validation/phase1.rs`, Rule 1b (lines 624-653 after fix)

**Change:** Add `&& params.protocol_version_major >= 9` guard on the error push:

```rust
{
    let mut seen = HashSet::new();
    for input in &body.inputs {
        // PV < 9 (Alonzo/Babbage): Haskell `Set.fromList` silently dedups —
        // no rejection.  PV >= 9 (Conway+): hard-fail mirrors
        // `decodeSetEnforceNoDuplicates`.
        if !seen.insert(input) && params.protocol_version_major >= 9 {
            errors.push(ValidationError::DuplicateInput(input.to_string()));
        }
    }
}
```

This is a **one-line semantic change** with zero risk to Conway+. Conway/Dijkstra txs with duplicate inputs still get `DuplicateInput`. Alonzo/Babbage txs with wire-duplicate inputs are silently accepted (matching Haskell).

---

## Tests Added

Three new tests in `crates/dugite-ledger/src/validation/phase1.rs`:

| Test | Purpose |
|------|---------|
| `test_duplicate_inputs_rejected_at_conway_pv9` | Conway (PV9) still rejects — renamed from old Test 31 |
| `test_duplicate_inputs_accepted_at_babbage_pv8` | Babbage (PV8) accepts duplicate inputs — negative control |
| `test_mainnet_babbage_duplicate_input_5ca83e21_no_false_positive` | Real mainnet tx 5ca83e21 with fixture pinned at `crates/dugite-ledger/src/validation/fixtures/tx-5ca83e21.hex` |

All 3 new tests PASS. Full suite: 1574/1574 pass, 0 failures, 6 skipped (pre-existing skips).

**CI checks:**
- `cargo clippy -p dugite-ledger --all-targets -- -D warnings`: CLEAN
- `cargo fmt --all -- --check`: CLEAN
- `cargo nextest run -p dugite-ledger`: 1574/1574 PASS

---

## Files Changed

| File | Change |
|------|--------|
| `crates/dugite-ledger/src/validation/phase1.rs` | Rule 1b: gate `DuplicateInput` on `pv >= 9`; add 3 tests |
| `crates/dugite-ledger/src/validation/fixtures/tx-5ca83e21.hex` | Real mainnet Babbage tx CBOR fixture (issue #759 pin) |

---

## Risk Assessment: SMALL+SAFE

- **Blast radius:** One `&&` condition on one `errors.push()` call.
- **Conway+ behavior:** Unchanged. PV9 test still fires and passes.
- **Pre-Conway behavior:** Duplicate inputs now silently accepted (Haskell-exact).
- **Downstream impact:** `eras/common.rs` `seen_inputs` dedup was already correct — this fix only removes the false Phase-1 rejection.
- **No SNAPSHOT_VERSION bump needed** — no ledger state struct changes.
- **Recommended placement:** v2.0.6 (already on `main`).
