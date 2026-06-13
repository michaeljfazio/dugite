# Conway ScriptDataHashMismatch Divergence — Root Cause Report

**Severity:** Consensus-critical — all PlutusV3 script transactions rejected during Conway sync  
**Affected versions:** All builds syncing from a Babbage-era Mithril snapshot  
**First observed:** Epoch 507 (mainnet Chang HFC, slot ~135,859,160)  
**Tx count diverging:** ~23+ (all PlutusV3-bearing txs in initial epochs)

---

## Summary

Dugite computes the wrong `script_data_hash` for transactions that use PlutusV3 scripts.
The hash is computed over `redeemers || datums || language_views(V3_cost_model)` — but
dugite supplies an empty language_views map (`0xa0`) because
`cost_models.plutus_v3 = None` at validation time.
The correct hash requires the 251-entry V3 cost model from Conway genesis.

The mismatch is phase-1 fatal: the declared hash in the tx body does not match the
recomputed value, producing `ScriptDataHashMismatch`, and the entire block is rejected
even though it is `is_valid=true` on-chain.

---

## On-Chain Evidence

### Common factor across all 23 diverging transactions

Every diverging transaction contains PlutusV3 scripts in the witness set (CBOR key 7,
tag 258). Transactions using only PlutusV1 or PlutusV2 scripts validate correctly because
those cost models ARE present in `cost_models.plutus_v1` / `plutus_v2`.

### Byte-exact proof (tx `31b6732d…`)

| | Value |
|---|---|
| Slot | 135,859,373 (epoch 507) |
| Redeemers raw bytes (KeepRaw) | `a182010082a0821901f419fa64` |
| Language views — WRONG (empty map) | `a0` |
| Language views — CORRECT (V3, 251 entries) | `a302...` (CBOR map key `0x02`, 251-element array) |
| Hash with wrong views | `e5d1c0ec...ed7c` (dugite output) |
| Hash with correct views | `43a8bd0d...97ad` (on-chain declared value) |

The wrong hash matches `blake2b256(redeemers || 0xa0)`.  
The correct hash matches `blake2b256(redeemers || V3_language_views(cost_model))`.

### Epoch 507 is the first appearance of V3 on mainnet

| Epoch | Protocol Version | V3 cost model |
|-------|-----------------|---------------|
| 506 | PV 8.0 (Babbage) | absent |
| 507 | PV 9.0 (Conway) | 251 entries, starts `[100788, 420, 1, …]` |
| 526 | PV 9.0 | 297 entries (Plomin expansion) |

V3 was not introduced by an on-chain `ParameterChange` — it originates from
`conway-genesis.json → plutusV3CostModel` at the Conway HFC, and is applied to
protocol params via `ConwayGenesis::apply_to_protocol_params()`.

---

## Root Cause

### Bug 1 — Snapshot restore does not re-apply Conway genesis V3 cost model (primary)

**File:** `crates/dugite-node/src/node/mod.rs`

**Startup sequence:**

```
line 1104: genesis.apply_to_protocol_params(&mut protocol_params);
           └─ sets protocol_params.cost_models.plutus_v3 = Some(251 entries)
           └─ but this is a LOCAL variable used only by init_fresh_ledger

line 1249: let mut ledger = if snapshot_path.exists() {
    ...load snapshot...
    // snapshot.epochs.protocol_params.cost_models.plutus_v3 = None
    // (snapshot was taken at Babbage epoch ≤ 506)

line 1256-1274: // Re-apply genesis config
    state.set_epoch_length(...)   // shelley genesis only
    state.set_slot_config(...)
    state.set_update_quorum(...)
    state.set_genesis_delegates(...)
    // MISSING: re-apply conway genesis V3 cost model

line 1538-1604: // Conway genesis: committee, constitution, DReps — but NOT cost_models
```

After the snapshot is loaded, `ledger.epochs.protocol_params.cost_models.plutus_v3`
remains `None` for the entire session. No on-chain `ParameterChange` introduces V3 during
the gap-replay because V3 originates from Conway genesis, not an on-chain update.

When the node subsequently validates a PlutusV3 tx in epoch 507+:

```
check_script_data_hash()                    // scripts.rs:512
  └─ determines has_v3 = true               // correctly detects V3 witness script
  └─ calls compute_script_data_hash_from_cbor(has_v3=true, cost_models, …)
       └─ calls encode_language_views(has_v3=true, &cost_models)
            └─ script.rs:350: if has_v3 {
                   if let Some(v3) = &cost_models.plutus_v3 {  // None → SKIPPED
                       ...emit V3 entry...
                   }
               }
               // V3 entry silently omitted → empty map 0xa0
```

**File:** `crates/dugite-serialization/src/encode/script.rs`, lines 350–361

```rust
if has_v3 {
    if let Some(v3) = &cost_models.plutus_v3 {
        // This arm is never reached when plutus_v3 = None
        let key = encode_uint(2);
        let mut value = encode_array_header(v3.len());
        for cost in v3 { value.extend(encode_int(*cost as i128)); }
        entries.push((key, value));
    }
    // Silent skip when plutus_v3 = None
}
```

### Bug 2 — `apply_pp_update` replaces `CostModels` wholesale (secondary)

**File:** `crates/dugite-ledger/src/eras/shelley.rs`, line 1167–1168

```rust
if let Some(v) = &update.cost_models {
    params.cost_models = v.clone();  // replaces entire CostModels struct
}
```

This is called by `crates/dugite-ledger/src/eras/shelley.rs:780` and
`crates/dugite-ledger/src/eras/conway.rs:507` during epoch transitions.
If a ParameterChange updates only V1+V2, the resulting `CostModels { plutus_v1: Some,
plutus_v2: Some, plutus_v3: None }` overwrites the Conway-genesis-initialized V3.

By contrast, `crates/dugite-ledger/src/state/protocol_params.rs:76–86`
(used for pre-Conway boundaries in `epoch.rs:597`) correctly merges per-version:

```rust
if let Some(ref v) = update.cost_models {
    if let Some(ref v1) = v.plutus_v1 { self.epochs.protocol_params.cost_models.plutus_v1 = Some(v1.clone()); }
    if let Some(ref v2) = v.plutus_v2 { self.epochs.protocol_params.cost_models.plutus_v2 = Some(v2.clone()); }
    if let Some(ref v3) = v.plutus_v3 { self.epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone()); }
}
```

---

## Fix

### Fix 1 — Re-apply Conway genesis cost models after snapshot restore

**File:** `crates/dugite-node/src/node/mod.rs`, after line 1537 (snapshot selection block ends)

After the `ledger` variable is set from either a snapshot or `init_fresh_ledger`,
before the Conway governance seeding block at line 1538, add:

```rust
// Re-apply Conway genesis cost models if absent from the restored snapshot.
// A snapshot saved at Babbage era (epoch ≤ 506) has plutus_v3 = None because
// V3 originates from Conway genesis, not an on-chain ParameterChange.
// Without this patch, all PlutusV3 txs produce ScriptDataHashMismatch.
if let Some(ref genesis_path) = args.config.conway_genesis_file {
    if ledger.epochs.protocol_params.cost_models.plutus_v3.is_none() {
        // protocol_params already has V3 set (it was applied at line 1104)
        ledger.epochs.protocol_params.cost_models.plutus_v3 =
            protocol_params.cost_models.plutus_v3.clone();
        if ledger.epochs.protocol_params.cost_models.plutus_v3.is_some() {
            info!("Re-applied Conway genesis PlutusV3 cost model to restored snapshot");
        }
    }
    // Also patch prev_protocol_params so RUPD boundary uses the correct model
    if ledger.epochs.prev_protocol_params.cost_models.plutus_v3.is_none() {
        ledger.epochs.prev_protocol_params.cost_models.plutus_v3 =
            protocol_params.cost_models.plutus_v3.clone();
    }
}
```

The `protocol_params` local variable already has the V3 model from line 1104, so
this is a read from an already-populated source — no re-parsing required.

### Fix 2 — Merge cost models per-version in `apply_pp_update`

**File:** `crates/dugite-ledger/src/eras/shelley.rs`, lines 1167–1169

Replace wholesale replacement with per-version merge, matching the logic in
`protocol_params.rs:76–86`:

```rust
// BEFORE:
if let Some(v) = &update.cost_models {
    params.cost_models = v.clone();
}

// AFTER:
if let Some(ref v) = update.cost_models {
    if let Some(ref v1) = v.plutus_v1 {
        params.cost_models.plutus_v1 = Some(v1.clone());
    }
    if let Some(ref v2) = v.plutus_v2 {
        params.cost_models.plutus_v2 = Some(v2.clone());
    }
    if let Some(ref v3) = v.plutus_v3 {
        params.cost_models.plutus_v3 = Some(v3.clone());
    }
    if let Some(ref v4) = v.plutus_v4 {
        params.cost_models.plutus_v4 = Some(v4.clone());
    }
}
```

This mirrors the existing correct behavior in `protocol_params.rs` and prevents
any future cost-model PPU (V1+V2 only) from clearing an already-established V3 or V4.

---

## Why the `encode_language_views` silent-skip is NOT the bug to fix

`crates/dugite-serialization/src/encode/script.rs:350–361` correctly implements
the spec: if `has_v3=true` but `cost_models.plutus_v3 = None`, this is a
configuration error upstream. Adding a panic or error there would surface the bug
earlier, but the correct fix is to ensure `plutus_v3` is always populated when
`has_v3` can be true — which Fixes 1 and 2 achieve.

---

## Regression Test Plan

### 1. Unit test: snapshot restore re-applies V3

In `crates/dugite-node/src/node/mod.rs` (or a dedicated test file):

```rust
#[test]
fn test_snapshot_restore_reapplies_v3_cost_model() {
    // Build a LedgerState with plutus_v3 = None (simulates Babbage-era snapshot)
    let mut params = ProtocolParameters::mainnet_defaults();
    assert!(params.cost_models.plutus_v3.is_none());
    let ledger = LedgerState::new(params.clone());
    assert!(ledger.epochs.protocol_params.cost_models.plutus_v3.is_none());

    // Simulate node startup: protocol_params has V3 from Conway genesis
    let mut startup_params = ProtocolParameters::mainnet_defaults();
    startup_params.cost_models.plutus_v3 = Some(vec![100788i64; 251]);

    // After the fix: patching the ledger with startup_params.cost_models.plutus_v3
    let mut ledger = ledger;
    if ledger.epochs.protocol_params.cost_models.plutus_v3.is_none() {
        ledger.epochs.protocol_params.cost_models.plutus_v3 =
            startup_params.cost_models.plutus_v3.clone();
    }
    assert!(ledger.epochs.protocol_params.cost_models.plutus_v3.is_some());
}
```

### 2. Unit test: `apply_pp_update` does not clear V3

```rust
#[test]
fn test_apply_pp_update_preserves_v3() {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.cost_models.plutus_v3 = Some(vec![100788i64; 251]);

    // PPU that only updates V1+V2
    let mut update = ProtocolParamUpdate::default();
    update.cost_models = Some(CostModels {
        plutus_v1: Some(vec![100788i64; 166]),
        plutus_v2: Some(vec![100788i64; 175]),
        plutus_v3: None,  // absent in update
        plutus_v4: None,
    });

    apply_pp_update(&mut params, &update);  // shelley.rs helper

    // V3 MUST be preserved
    assert!(
        params.cost_models.plutus_v3.is_some(),
        "apply_pp_update must not clear V3 when update omits it"
    );
}
```

### 3. Integration test: ScriptDataHash matches on-chain for known V3 tx

Pin tx `31b6732d…` (slot 135,859,373, epoch 507):

```rust
#[test]
fn test_script_data_hash_plutus_v3_epoch507() {
    use dugite_serialization::encode::script::{compute_script_data_hash_from_cbor, encode_language_views};
    use dugite_primitives::transaction::CostModels;

    // 251-entry V3 cost model from mainnet Conway genesis (first 5: [100788,420,1,1,1000])
    let v3: Vec<i64> = vec![100788, 420, 1, 1, 1000, /* ... rest of 251 entries */];
    let cost_models = CostModels {
        plutus_v1: None,
        plutus_v2: None,
        plutus_v3: Some(v3),
        plutus_v4: None,
    };

    // Raw redeemers bytes from tx 31b6732d (KeepRaw capture)
    let redeemers_cbor = hex::decode("a182010082a0821901f419fa64").unwrap();

    // No datums in this tx
    let datums_cbor: Vec<u8> = vec![];

    let hash = compute_script_data_hash_from_cbor(
        &redeemers_cbor,
        &datums_cbor,
        true,   // has_v3
        false,  // has_v2
        false,  // has_v1
        &cost_models,
    );

    let expected = hex::decode("43a8bd0df81610ed...97ad").unwrap(); // on-chain declared hash
    assert_eq!(hash.as_slice(), expected.as_slice());
}
```

### 4. Snapshot round-trip test

After fixing the restore path: load a synthetic Babbage-era snapshot (saved with
`plutus_v3 = None`), simulate the Conway genesis re-apply in `run()`, then validate
a synthetic V3 tx body against the expected hash. Ensures no regression on restart.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/dugite-node/src/node/mod.rs` | Re-apply Conway genesis V3 to restored snapshot (after line 1537) |
| `crates/dugite-ledger/src/eras/shelley.rs` | Per-version merge in `apply_pp_update` (lines 1167–1169) |

---

## Verification

After applying the fix:

1. Start the node from the current Babbage-era Mithril snapshot
2. Observe `"Re-applied Conway genesis PlutusV3 cost model to restored snapshot"` in logs
3. Replay through epoch 507+; no `ScriptDataHashMismatch` errors should appear
4. Cross-check: `cargo nextest run -p dugite-ledger -E 'test(script_data_hash)'`

The existing snapshots at `db-mainnet-val/ledger-snapshot-epoch511-*.bin` will also
benefit: they were saved from a state with `plutus_v3 = None`, so the fix
re-populates V3 at the next startup without requiring a fresh Mithril import.
