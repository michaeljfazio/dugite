---
name: vrf-key-uniqueness-pv11-gate
description: VRFKeyHashAlreadyRegistered check must gate at PV>=11, NOT PV>=9; Haskell hardforkConwayDisallowDuplicatedVRFKeys = pvMajor > 10
metadata:
  type: project
---

## VRF Key Uniqueness Check Protocol Version Gate

`hardforkConwayDisallowDuplicatedVRFKeys :: ProtVer -> Bool` is defined as:

```haskell
-- eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs
hardforkConwayDisallowDuplicatedVRFKeys pv = pvMajor pv > natVersion @10
```

This returns True ONLY at PV >= 11. NOT at PV 9 (Conway bootstrap) or PV 10.

**Why:** Confirmed by mainnet epoch 523 (PV 9.0) where pool `a319b697` (PETRA) first-registered with VRF key `b279f6f5` already held by pool `8b9a73ac` (also PETRA, different pool ID). cardano-node accepts; dugite rejects with VRFKeyHashAlreadyRegistered.

**The bug:** `crates/dugite-ledger/src/validation/mod.rs:3468` uses `>= 9` instead of `>= 11`.

**Fix:** Change gate from `protocol_version_major >= 9` to `protocol_version_major >= 11`.

Also fix error variant doc at line 1527 and update 3 tests in `validation/tests.rs` that use PV 9 to test this check (lines 11413, 11496, 11655) — all must use PV 11.

**Additional semantics (for future PV 11+ work):**
- `psVRFKeyHashes` Haskell type: `Map VRFVerKeyHash (NonZero Word64)` (refcount)
- Dugite uses `HashMap<Hash32, Hash28>` (vrf_key → pool_id) — correct for the uniqueness check; insufficient for full refcount POOLREAP semantics
- Retiring pools (in `psRetiring`) keep their VRF key in `psVRFKeyHashes` until POOLREAP executes
- New pool check: `Map.notMember sppVrf psVRFKeyHashes` (no exemptions)
- Re-reg check: `sppVrf == current_sps_vrf || Map.notMember sppVrf psVRFKeyHashes`

**Report:** `/Users/michaelfazio/Source/dugite/reports/conway-vrf-key-already-registered-divergence.md`
