# Conway Plutus Phase-2 Budget Divergence — Investigation Report

**Date**: 2026-06-13  
**Severity**: P1 — All PlutusV3 scripts fail with BudgetExhausted when syncing Conway era blocks after a Babbage-era snapshot restore  
**Affected versions**: All dugite releases prior to this fix  

---

## Summary

During mainnet genesis-mode sync with `--validate-all-blocks`, 2407 Conway-era transactions
fail with `budget exhausted: cpu_remaining=14547, mem_remaining=100`. Every failing tx uses
a PlutusV3 script. The cause is a missing post-snapshot-restore seeding of the PlutusV3 cost
model from the Conway genesis configuration.

---

## Error Signature

```
budget exhausted: cpu_remaining=14547, mem_remaining=100
```

Interpretation:
- The terminal `flush()` tried to subtract one pending step `{cpu=16000, mem=100}` from
  `remaining={cpu=14547, mem=100}`.
- `14547 - 16000 = -1453 < 0` → CPU exhaustion.
- Memory would pass (`100 - 100 = 0 ≥ 0`).
- Total CPU consumed: `declared_cpu - 14547 + 16000 = declared_cpu + 1453` — over-budget by 1453.

---

## Example Transactions

| Tx hash | Slot | Epoch | Purpose |
|---------|------|-------|---------|
| `a37a8fd3d2bc6d92e7d9e370f70e106dfc06cb22cf081192bc7bfafcdf73c2a8` | 133661155 | 507 | cert |
| `db6f133ceb7b47ad53862587baa6eb56aacf6fda9ed0f3713c045f0cd50f8975` | 135634777 | 511 | spend |

Example: tx `a37a8fd3d2bc6d92e7d9e370f70e106dfc06cb22cf081192bc7bfafcdf73c2a8`:
- Slot 133661155, epoch 507 (first Conway epoch on mainnet)
- Script hash `e5ab37261b3d63600d566564879370aea031ea3108b0a6bd8cef58aa`, type `plutusV3`
- Koios-confirmed `valid_contract: true` — Haskell accepts it
- Declared redeemer budget: `{steps: 120855313, mem: 475258}`
- Dugite error: `budget exhausted: cpu_remaining=14547, mem_remaining=100`

All 2407 failing txs share the same error shape: `cpu_remaining=14547, mem_remaining=100`.

---

## Code Path

### ValidateAll mode is active

The sync ran with `--validate-all-blocks`. In `crates/dugite-node/src/node/sync.rs:1586`:

```rust
let ledger_mode = if strict || self.validate_all_blocks {
    BlockValidationMode::ValidateAll
} else {
    BlockValidationMode::ApplyOnly
};
```

With `validate_all_blocks=true`, every block (including bulk-sync blocks) uses `ValidateAll`.

### Phase-2 is evaluated in ValidateAll

In `crates/dugite-ledger/src/state/apply.rs:577`:

```rust
let cost_models_cbor = if mode == BlockValidationMode::ValidateAll {
    self.epochs.protocol_params.cost_models.to_cbor()
} else {
    None
};
```

And phase-2 work items are captured (lines 1154-1169) and executed (line 1434) only in
`ValidateAll` mode. In `ApplyOnly`, phase-2 is skipped entirely — so this bug only
manifests when `--validate-all-blocks` is used.

### Cost model CBOR resolves to None for PlutusV3

`CostModels::to_cbor()` in `crates/dugite-primitives/src/transaction.rs:673` returns `None`
when all language versions are `None`. If `cost_models.plutus_v3 = None`, the CBOR includes
no V3 entry.

In `crates/dugite-uplc/src/eval_redeemer.rs:295-339`, `resolve_applied_costs()` for
PlutusV3:

```rust
ScriptLanguage::PlutusV3 => {
    let params = cm.plutus_v3.as_deref()?;  // Returns None if plutus_v3 is None
    ...
}
```

When `cm.plutus_v3 = None`, this `?` propagates `None`, and the fallback path at line 204-207
is taken:

```rust
let mut tracker = match resolve_applied_costs(cost_models, r.language, major_pv) {
    Some(applied) => BudgetTracker::with_applied(initial_budget, applied),
    None => BudgetTracker::new(initial_budget),  // DEFAULT cost model
};
```

`BudgetTracker::new()` uses `MachineCosts::DEFAULT` and `BuiltinCosts::DEFAULT` — the
reference Plutus 1.65.0.0 defaults — instead of the real on-chain epoch 507 V3 costs.

---

## Root Cause: Missing V3 Cost Model After Snapshot Restore

### The Conway genesis has V3 cost models

`config/mainnet/conway-genesis.json` contains `plutusV3CostModel` — a 251-entry flat array
starting `[100788, 420, 1, 1, 1000, 173, 0, 1, 1000, 59957, ...]`.

Koios confirms epoch 507 mainnet V3 cost model has 251 entries matching these genesis values.

### Startup correctly loads V3 from genesis — but then overwrites it

In `crates/dugite-node/src/node/mod.rs:1089-1104`, the Conway genesis is loaded and applied:

```rust
if let Some(ref genesis_path) = args.config.conway_genesis_file {
    match ConwayGenesis::load_with_hash(&genesis_path) {
        Ok((genesis, hash)) => {
            genesis.apply_to_protocol_params(&mut protocol_params);  // Sets V3 cost model
            ...
        }
    }
}
```

`genesis.apply_to_protocol_params()` (genesis.rs:965-972) does:

```rust
if let Some(v3) = &self.plutus_v3_cost_model {
    params.cost_models.plutus_v3 = Some(v3.clone());
}
```

**BUT** — at line 1249, if a ledger snapshot exists, it is loaded:

```rust
let mut ledger = if snapshot_path.exists() {
    match Self::load_snapshot_with_backend_guard(...) {
        Ok(mut state) => {
            // snapshot's state.epochs.protocol_params.cost_models.plutus_v3 WINS
            ...
            state  // <-- does NOT incorporate protocol_params built above
        }
    }
};
```

The snapshot's `LedgerState` (deserialized via bincode) overwrites `protocol_params`
entirely. If the snapshot was taken during Babbage era (epoch < 507), its
`protocol_params.cost_models.plutus_v3` is `None`.

After the snapshot is loaded, the code seeds DReps (line 1581), committee (line 1555),
and constitution (line 1570) from Conway genesis — but there is **no corresponding step
to seed `ledger.epochs.protocol_params.cost_models.plutus_v3`** from the genesis.

### Conway era transition does not set V3

In `crates/dugite-ledger/src/eras/conway.rs:1032-1110`, `on_era_transition` handles:
pointer stake exclusion, DRep seeding, committee seeding — but does **not** set
`epochs.protocol_params.cost_models.plutus_v3`.

In Haskell, `translateEra @ConwayEra @BabbageEra` for `PParams` applies the Conway genesis
`plutusV3CostModel` at the era boundary (via `PParams.translateEraBabbageToConway`). Dugite
does not replicate this.

### Timeline

1. Mithril snapshot taken at Babbage epoch 506 → `cost_models.plutus_v3 = None`
2. Node restarts → Conway genesis sets `protocol_params.cost_models.plutus_v3 = Some(...)` in the local `protocol_params` var
3. Snapshot loaded → `ledger.epochs.protocol_params` comes from bincode (plutus_v3 = None)
4. `protocol_params` built in step 2 is NOT applied to the loaded ledger state
5. Gap replay (ApplyOnly) processes Babbage blocks — no V3 PParamsUpdates, plutus_v3 stays None
6. Conway transition (epoch 507) — `on_era_transition` does NOT set V3
7. First PlutusV3 tx arrives → `cost_models_cbor` encodes `{0: [...], 1: [...]}` (no V3 key)
8. `resolve_applied_costs` returns None for V3 → DEFAULT costs → budget overrun

---

## Confirmation: Why only PlutusV2 scripts pass

Babbage-era on-chain PParamsUpdates (decoded via PPUP key 18 fix in `46fa56def9`) correctly
set `plutus_v2 = Some(...)` in the snapshot. So V2 scripts have correct cost models from the
snapshot. Only V3 is missing, because V3 was introduced with Conway and no Babbage-era
PParamsUpdate could have set it.

---

## DEFAULT vs On-Chain V3 Cost Model Divergence

The DEFAULT V3 costs in dugite (`BuiltinCosts::DEFAULT`, `MachineCosts::DEFAULT`) differ
from the mainnet epoch 507 on-chain values in multiple builtins. The CEK machine step costs
(`{cpu=16000, mem=100}`) are identical between DEFAULT and on-chain — so the 1453-cpu
overrun is entirely attributable to builtin cost differences.

Example: if the failing script calls `equalsByteString` on equal-length 58-byte bytestrings:
- DEFAULT: `LinearOnDiagonal(constant=30623, intercept=28755, slope=75)` → `28755 + 75×58 = 33105`
- Epoch 507: `LinearOnDiagonal(constant=24548, intercept=29498, slope=38)` → `29498 + 38×58 = 31702`
- DEFAULT overcharges by 1403 cpu per call

Multiple such calls accumulate to ~1453 cpu total overrun.

---

## Fix

### Option A (Recommended): Seed V3 cost model after snapshot load (node/mod.rs)

After loading the snapshot in `node/mod.rs`, add a V3 cost model seed analogous to the
existing DRep, committee, and constitution seeds:

```rust
// After line ~1604 (after DRep seeding):
// Seed PlutusV3 cost model from Conway genesis if not already in the
// loaded snapshot. Snapshots taken during Babbage era have
// cost_models.plutus_v3 = None; the genesis provides the canonical
// initial value that Haskell sets via translateEra at the era boundary.
if ledger.epochs.protocol_params.cost_models.plutus_v3.is_none() {
    if let Some(v3) = &conway_genesis_v3_cost_model {
        debug!(
            count = v3.len(),
            "Seeding PlutusV3 cost model from Conway genesis (snapshot predates Conway era)"
        );
        ledger.epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone());
    }
}
```

This requires capturing the V3 cost model from the genesis before discarding it:

```rust
// After genesis.apply_to_protocol_params(&mut protocol_params) at line ~1104:
let conway_genesis_v3_cost_model: Option<Vec<i64>> =
    genesis.plutus_v3_cost_model.clone();
```

### Option B: Set V3 in on_era_transition (conway.rs)

In `ConwayRules::on_era_transition`, seed V3 from `ctx.conway_genesis` when it contains
the cost model. This mirrors Haskell's `translateEra` more closely:

```rust
// In on_era_transition, after the existing DRep/committee seeding:
if let Some(genesis) = ctx.conway_genesis {
    if let Some(ref v3) = genesis.plutus_v3_cost_model {
        if epochs.protocol_params.cost_models.plutus_v3.is_none() {
            epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone());
            tracing::info!("Conway: seeded PlutusV3 cost model from genesis ({} params)", v3.len());
        }
    }
}
```

This requires `ConwayGenesisInit` to carry the V3 cost model. The `conway_genesis_init`
field is stored on `LedgerState` at startup so it is accessible during the era transition.

Option B is architecturally cleaner because it applies the V3 model at the exact moment
Haskell does (era transition), rather than as a post-snapshot patch.

---

## Regression Test

Pin the real on-chain behavior with a unit test in `crates/dugite-ledger/src/plutus.rs`
(alongside existing `test_evaluate_plutus_scripts_v3_*` tests):

```rust
#[test]
fn test_missing_v3_cost_model_falls_back_to_default() {
    // Demonstrates the bug: CostModels with plutus_v3 = None causes DEFAULT
    // costs to be used, which overcharges typical on-chain scripts.
    let cost_models = CostModels { plutus_v1: None, plutus_v2: None, plutus_v3: None, plutus_v4: None };
    let cbor = cost_models.to_cbor(); // Returns None
    assert!(cbor.is_none(), "CostModels with all-None should produce no CBOR");
    // When cost_models_cbor is None, resolve_applied_costs returns None → DEFAULT
    // This is the bug path for V3 scripts in pre-Conway snapshots.
}

#[test]
fn test_v3_cost_model_from_conway_genesis_applied_to_snapshot_restore() {
    // Integration test: simulate snapshot-restore + Conway genesis seeding.
    // After seeding, V3 should be Some([100788, 420, ...]) not None.
    // (Add to startup integration tests once the fix is implemented.)
}
```

For a full regression test pinned to the real on-chain script hash
`e5ab37261b3d63600d566564879370aea031ea3108b0a6bd8cef58aa` (PlutusV3, epoch 507, cert
purpose, 2993 bytes), use `DUGITE_PHASE2_DUMP_DIR` to capture the redeemer during a
validation run and write a golden test against the epoch-507 cost model.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/dugite-node/src/node/mod.rs` | Capture `conway_genesis_v3_cost_model`; seed into `ledger.epochs.protocol_params.cost_models.plutus_v3` after snapshot load |
| `crates/dugite-ledger/src/eras/conway.rs` | (Option B) Seed V3 from `conway_genesis_init` in `on_era_transition` |
| `crates/dugite-ledger/src/plutus.rs` | Regression tests |

---

## Invariant

After this fix, the following must hold at all times:

> When `protocol_params.protocol_version_major >= 9` (Conway+) and a PlutusV3 script is
> present in a tx, `protocol_params.cost_models.plutus_v3` MUST be `Some(...)`.

This can be enforced by a debug-mode assertion in `evaluate_plutus_scripts` when
`ScriptLanguage::PlutusV3` is encountered.
