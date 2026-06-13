# Conway VRF Key Uniqueness: `VRFKeyHashAlreadyRegistered` Divergence

**Date:** 2026-06-13  
**Slot:** 140991166  
**Epoch:** 523  
**Tx hash:** `054c270b6fed2be05cec72077e5dea18c041eb33e9c632a8f9c750d70b1c02df`  
**Block height:** 11139740  
**Symptom:** dugite phase-1 rejects confirmed block; cardano-node accepts it.  
**Severity:** Hard block — prevents mainnet sync past epoch 523.

---

## 1. On-Chain Transaction Facts

Fetched via Koios mainnet (`https://api.koios.rest/api/v1/tx_info`):

| Field | Value |
|---|---|
| Tx hash | `054c270b6fed2be05cec72077e5dea18c041eb33e9c632a8f9c750d70b1c02df` |
| Slot | 140991166 (epoch 523) |
| Protocol version at epoch 523 | **PV 9.0** (Conway bootstrap phase) |
| Certificate type | `pool_update` (PoolRegistration) |
| Registering pool | `a319b6970041da33e6baa44bfe8902898ec148c6ba88c783913bcab4` (PETRA) |
| VRF key hash | `b279f6f548e9063ed062136e25652ad697f88004ca7c57b7c2a09800ef2fdfb1` |

The tx is pool `a319b697`'s **first-ever** registration (no prior history). The VRF key `b279f6f5` is also held by pool `8b9a73accec94d747dbeba74c3da0b0871936c1cd2ea9b99c6a0acb1` (also PETRA, different pool ID, same operator), which registered with VRF key `b279f6f5` at epoch 290 and remains active at epoch 523 (scheduled to retire only at epoch 560, which is in May 2025).

**Summary:** A new pool is registering with a VRF key that is already held by a different (but same-operator), still-active pool. cardano-node accepts this. dugite rejects it.

---

## 2. Root Cause

### dugite's Check (incorrect gate)

`crates/dugite-ledger/src/validation/mod.rs`, line 3468:

```rust
if params.protocol_version_major >= 9 {
    if let Some(vrf_keys) = registered_vrf_keys {
        for cert in &tx.body.certificates {
            if let Certificate::PoolRegistration(pool_params) = cert {
                if let Some(&existing_pool) = vrf_keys.get(&pool_params.vrf_keyhash) {
                    if existing_pool != pool_params.operator {
                        errors.push(ValidationError::VrfKeyHashAlreadyRegistered {
                            vrf_keyhash: pool_params.vrf_keyhash.to_hex(),
                            existing_pool_id: existing_pool.to_hex(),
                        });
                    }
                }
            }
        }
    }
}
```

This fires at `protocol_version_major >= 9`, i.e., from PV 9 (Conway bootstrap) onward.

### Canonical Haskell Gate (authoritative source)

**File:** `eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs`

```haskell
hardforkConwayDisallowDuplicatedVRFKeys :: ProtVer -> Bool
hardforkConwayDisallowDuplicatedVRFKeys pv = pvMajor pv > natVersion @10
```

`natVersion @10` is the type-level natural 10. The function returns `True` only when `pvMajor > 10`, i.e., **PV 11 or higher**.

**File:** `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs` (New pool registration branch, `Nothing` case):

```haskell
Nothing -> do
  when (hardforkConwayDisallowDuplicatedVRFKeys pv) $ do
    Map.notMember sppVrf psVRFKeyHashes
      ?! injectFailure (VRFKeyHashAlreadyRegistered sppId sppVrf)
```

### Protocol Version Timeline

| PV major | `hardforkConwayBootstrapPhase` | `hardforkConwayDisallowDuplicatedVRFKeys` | dugite gate |
|---|---|---|---|
| 8 (Babbage) | False | **False** | False (correct) |
| 9 (Conway bootstrap) | True | **False** (check inactive) | **True** (WRONG) |
| 10 | False | **False** (check still inactive) | **True** (WRONG) |
| 11+ (future) | False | **True** (check active) | True (correct) |

At PV 9 (epoch 523), Haskell's `hardforkConwayDisallowDuplicatedVRFKeys pv = 9 > 10 = False`. The entire VRF uniqueness check is skipped. Duplicate VRF keys across pools are **valid at PV 9 and PV 10**.

dugite's `protocol_version_major >= 9` fires the check two major versions too early, rejecting transactions that Haskell accepts at PV 9 and PV 10.

### Secondary Error: The Existing Pool Logic Is Structurally Correct But Irrelevant

The logic at line 3477 (`if existing_pool != pool_params.operator`) correctly exempts a pool re-registering with its own VRF key. However, in this case pool `a319b697` and `8b9a73ac` are genuinely different pool IDs (same operator/owner, different pool IDs created independently), so the exemption does not apply and the check fires. The structural logic is sound — the gate is simply wrong.

---

## 3. Haskell Behavior: Full POOL Rule Summary

For completeness, when `hardforkConwayDisallowDuplicatedVRFKeys` IS active (PV 11+), the checks are:

**New pool (`Nothing` branch — pid not in `psStakePools`):**
```haskell
Map.notMember sppVrf psVRFKeyHashes ?! VRFKeyHashAlreadyRegistered sppId sppVrf
```
Reject if VRF key appears in `psVRFKeyHashes` for ANY pool. No exemptions.

**Re-registration (`Just stakePoolState` branch — pid already in `psStakePools`):**
```haskell
sppVrf == stakePoolState ^. spsVrfL
  || Map.notMember sppVrf psVRFKeyHashes
  ?! VRFKeyHashAlreadyRegistered sppId sppVrf
```
Allow if the new VRF key matches the pool's currently-activated VRF key, OR is not in use by any pool.

**`psVRFKeyHashes` type:** `Map (VRFVerKeyHash StakePoolVRF) (NonZero Word64)` — a reference-count map. A key's count > 1 if multiple pool IDs within `psFutureStakePoolParams` (same-epoch multiple re-registrations) share a VRF key.

**Retirement scheduling does NOT remove VRF keys.** A pool scheduled to retire (in `psRetiring`) remains in `psStakePools` and `psVRFKeyHashes` until POOLREAP executes at the retirement epoch.

---

## 4. Proposed Fix

### Primary Fix — Gate correction

**File:** `crates/dugite-ledger/src/validation/mod.rs`

**Line 3468:**
```rust
// BEFORE (wrong — fires at PV 9+, two versions too early):
if params.protocol_version_major >= 9 {

// AFTER (correct — mirrors hardforkConwayDisallowDuplicatedVRFKeys = pvMajor pv > natVersion @10):
if params.protocol_version_major >= 11 {
```

**Lines 1527-1530** (error variant doc comment, also incorrect):
```rust
// BEFORE:
/// Enforced only when `protocol_version_major >= 9` (Conway). In earlier
/// eras, multiple pools sharing a VRF key is theoretically possible (though
/// inadvisable). From Conway onward, Haskell rejects duplicate VRF keys to
/// prevent ambiguity in the VRF-based leader election.

// AFTER:
/// Enforced only when `protocol_version_major >= 11` (post-Dijkstra / PV 11+,
/// controlled by `hardforkConwayDisallowDuplicatedVRFKeys = pvMajor pv > 10`).
/// At PV 9 (Conway bootstrap) and PV 10, pools may share VRF keys — Haskell
/// does not enforce uniqueness until PV 11. From PV 11 onward, duplicate VRF
/// keys across different pool IDs are rejected to prevent ambiguity in the
/// VRF-based leader election.
```

### Secondary Fix — Test corrections

Three existing tests in `crates/dugite-ledger/src/validation/tests.rs` use `protocol_version_major = 9` to assert VRF dedup behavior that does NOT apply at PV 9:

1. **`test_vrf_key_already_registered_rejected_in_conway`** (line 11413): Uses PV 9 and expects rejection. Must be updated to use PV 11.
2. **`test_vrf_key_already_registered_same_pool_allowed`** (line 11496): Uses PV 9. Must be updated to PV 11.
3. **`test_vrf_key_dedup_no_map_skips_check`** (line 11646): Uses PV 9. Update to PV 11 (or add a PV 9 variant confirming no check fires regardless of map content).

Additionally, the existing `test_vrf_key_dedup_skipped_pre_conway` (line 11571) tests PV 8 and asserts no rejection — this test is correct behavior-wise but must be supplemented with:

- A test at PV 9 confirming VRF dedup is ALSO skipped (not just PV 8).
- A test at PV 10 confirming VRF dedup is ALSO skipped.
- A test at PV 11 confirming VRF dedup IS enforced.

### No Changes Needed Elsewhere

- `crates/dugite-node/src/node/serve.rs` line 872 is just error formatting — no logic change needed.
- `crates/dugite-ledger/src/state/apply.rs` — the `registered_vrf_keys` map is built correctly from `pool_params`; the map semantics and population are not the issue.
- `crates/dugite-ledger/src/state/certificates.rs` — pool apply path is correct.

---

## 5. Impact Assessment

| Scenario | Epochs affected |
|---|---|
| PV 9 pool registration with VRF key of active (possibly-retiring) different pool | Epochs 509–? (all of mainnet Conway bootstrap at PV 9) |
| PV 10 pool registration with same | Epochs after PV 10 HF until PV 11 |
| PV 11+ pool registration with duplicate VRF key | Correctly rejected (not yet active on mainnet as of 2026-06-13) |

The immediately failing sync stall is at epoch 523, but any epoch from 509 (Conway HF on mainnet) through the eventual PV 11 upgrade where this pattern occurs will cause a divergence. The pattern (new pool reusing VRF key from an active/retiring pool owned by same operator) is uncommon but on-chain.

---

## 6. Verification

After fixing the gate to `>= 11`:

```bash
# Confirm the fix builds
cargo build -p dugite-ledger

# Confirm tests pass (the 3 affected tests need their PV updated to 11 first)
cargo nextest run -p dugite-ledger -E 'test(vrf_key)'

# Confirm sync past slot 140991166 on mainnet
# Expected: no VRFKeyHashAlreadyRegistered divergence logged for tx 054c270b...
```

---

## 7. Memory Note

This divergence class is now indexed:

- **Haskell gate:** `hardforkConwayDisallowDuplicatedVRFKeys = pvMajor > 10` (i.e., `>= 11`)
- **NOT `>= 9`**, despite the check being conceptually a Conway-era feature
- The bootstrap phase (`pvMajor == 9`) and early post-bootstrap (`pvMajor == 10`) explicitly do NOT enforce VRF key uniqueness across pools
- The `psVRFKeyHashes` refcount type in Haskell is `Map VRFVerKeyHash (NonZero Word64)` — dugite uses `HashMap<Hash32, Hash28>` (vrf_key → pool_id) which is adequate for the check but doesn't model the full refcount semantics needed for POOLREAP cleanup
