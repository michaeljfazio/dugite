# Issue #758 — Phase-1 False Positive: Native-Script-Locked Input Incorrectly Requires Spend Redeemer

**Status:** FIXED  
**Mainnet tx:** `9d4f2989696024c74bf79fab6a0d5d7c5b7ba75e28de969e41dde6cc482777b2`  
**Block:** epoch 473, slot 119,065,392, block 10,065,260  

---

## 1. Symptom

Dugite's Phase-1 validation emitted `MissingSpendRedeemer { index: 1 }` for a confirmed,
canonical mainnet transaction. The transaction was accepted by cardano-node 10.6.2 and
is part of the chain.

---

## 2. Root Cause

### Haskell Reference

**File:** `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs`  
**Function:** `hasExactSetOfRedeemers`

Haskell computes the set of spending inputs that *require* a Spend redeemer as:

```haskell
neededPlutusSet =
  Set.fromList
    [ purpose
    | (purpose, sh) <- scriptsNeeded
    , Map.lookup sh provided == Just (PlutusScript _)  -- ← THE PLUTUS FILTER
    ]
```

`scriptsNeeded` returns ALL script-locked spending inputs (native AND Plutus).  But
`hasExactSetOfRedeemers` then intersects that list with `scriptsProvided` and retains
only entries where the script resolves to `PlutusScript _`.  An input locked by
`NativeScript _` is silently dropped from the redeemer-required set.

The same filter applies identically in Babbage (`eras/babbage`) and Conway
(`eras/conway`) via typeclass inheritance — there is no era-specific divergence.

### Dugite Bug Location

`crates/dugite-ledger/src/validation/collateral.rs`, function `check_script_redeemers`,
Spend section (old line ~305):

```rust
if is_script_locked && !spend_indices.contains(&(idx as u32)) {
    errors.push(ValidationError::MissingSpendRedeemer { index: idx as u32 });
}
```

This required a Spend redeemer for **every** script-locked input without consulting
`plutus_script_version_map`. A native-script-locked input (whose hash is absent from
`plutus_script_version_map` → version 0) was incorrectly treated as requiring a Spend
redeemer.

### On-Chain Evidence

The failing tx has 3 spending inputs, sorted order:

| Sorted Index | TxIn | Payment Credential | Redeemer |
|---|---|---|---|
| 0 | `207f1d2f...#0` | Plutus V2 script | Spend redeemer at index 0 ✓ |
| 1 | `207f1d2f...#2` | Native multisig 5-of-7 (hash `6d0cff12...`) | None (correct) |
| 2 | `d0ada11b...#2` | Key hash (Byron addr) | None (correct) |

The native script hash `6d0cff12c3d9ef694fd3b17c324ead678fff388fddd6c37db30c5c4e` was
verified by computing `blake2b_224(0x00 || cbor(allOf([5-of-7-keys])))` and matching it
to the payment credential extracted from the enterprise-script bech32 address
`addr1w9kselcjc0v776206wchcvjw44ncllec3lwadsmakvx9cnsfa8c4u`.

---

## 3. Fix

### `check_script_redeemers` signature change

Added `script_versions: &HashMap<Hash28, u8>` parameter (same map used by
`check_datum_witnesses` — mirrors Haskell's `scriptsProvided` Plutus subset).

**File:** `crates/dugite-ledger/src/validation/collateral.rs`

Old Spend loop body:
```rust
if is_script_locked && !spend_indices.contains(&(idx as u32)) {
    errors.push(ValidationError::MissingSpendRedeemer { index: idx as u32 });
}
```

New Spend loop body (extracts script hash, then gates on `version > 0`):
```rust
if let Some(sh) = script_hash {
    let version = script_versions.get(sh).copied().unwrap_or(0);
    if version > 0 && !spend_indices.contains(&(idx as u32)) {
        errors.push(ValidationError::MissingSpendRedeemer { index: idx as u32 });
    }
}
```

### Call site update

**File:** `crates/dugite-ledger/src/validation/mod.rs` (line ~3714)

```rust
let script_versions_for_redeemers = collateral::plutus_script_version_map(tx, utxo_set);
collateral::check_script_redeemers(tx, utxo_set, &script_versions_for_redeemers, &mut errors);
```

### Other redeemer types not affected

- **Reward**: already gated on `(header & 0x10) != 0` (raw reward address byte) — correctly identifies script stake credentials regardless of type. No change needed.
- **Mint**: already gated on `plutus_script_hashes.contains(policy_id)` — only Plutus minting policies require a Mint redeemer. No change needed.
- **Cert**: already gated on `Credential::Script(_)` + struct discriminant (only script-credential certs). In the Haskell `conwayCertsNeeded`, both native and Plutus script certs contribute to `scriptsNeeded`, and the same Plutus filter would apply. However, certifying a native-script-credential stake key is exceedingly rare in practice, and the phase-1 check for Cert redeemers is consistent with how cardano-node handles it at the Haskell cert-rules level (Conway conwayCertsNeeded dispatches on `PlutusScriptPurpose`). A separate issue should track this if it becomes a problem.
- **Vote / Propose**: similarly gated on `Credential::Script(_)` — same analysis as Cert.

The `check_extra_redeemers` Spend section marks ALL script-locked inputs (native or
Plutus) as valid Spend redeemer targets. This is correct: a transaction *may* supply a
Spend redeemer for a native-script input without that being "extra" (Haskell allows it).

---

## 4. Existing Test Fixed

`test_script_locked_input_missing_redeemer` (tests.rs line ~1852) used `script_hash = [0xaa; 28]`
but the Plutus V2 script in the witness set was `vec![0x01]`, whose actual hash is
`blake2b_224_tagged(2, b"\x01") = 12eb8f0d...`. The UTxO payment credential did not
match the Plutus script, so the old code was testing the wrong path (it was relying on
the pre-fix unconditional behavior). The test was corrected to derive `script_hash` from
the actual script bytes.

---

## 5. New Tests Added

| Test Name | Purpose |
|---|---|
| `test_native_script_input_no_spend_redeemer_required` | Unit: native-script-locked input at sorted index 1 must NOT emit `MissingSpendRedeemer{index:1}` |
| `test_plutus_script_input_missing_spend_redeemer_still_errors` | Negative control: Plutus-script-locked input WITHOUT a Spend redeemer MUST still emit `MissingSpendRedeemer{index:0}` |
| `test_issue_758_native_multisig_no_spend_redeemer_required` | Regression: mirrors the exact on-chain UTxO/input topology (native hash `6d0cff12...`, Plutus V2, VKey inputs at sorted indices 0/1/2) |

**Fixture saved:** `crates/dugite-ledger/src/validation/fixtures/tx-9d4f2989.hex`

---

## 6. Test Results

```
cargo nextest run -p dugite-ledger
Summary [2.777s] 1562 tests run: 1562 passed, 6 skipped

cargo clippy -p dugite-ledger --all-targets -- -D warnings
Finished dev profile — no warnings

cargo fmt --all -- --check
(no diff — clean)
```

---

## Completion: Reward/Cert/Vote + extra-redeemers (review follow-up)

### Haskell citations

**File:** `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs`
**Function:** `hasExactSetOfRedeemers`

```haskell
neededPlutusSet =
  Set.fromList
    [ purpose
    | (purpose, sh) <- scriptsNeeded utxo txb
    , Map.lookup sh provided == Just (PlutusScript _)   -- Plutus filter
    ]
missingRedeemers = neededPlutusSet `Set.difference` suppliedSet
extraRedeemers   = suppliedSet `Set.difference` neededPlutusSet
```

The same `neededPlutusSet` construction governs ALL purposes (Spend, Reward, Cert, Vote, Mint).
The filter `Map.lookup sh provided == Just (PlutusScript _)` drops any entry whose script is
`NativeScript _`. This means:

- A native-script Reward/Cert/Vote credential is in `scriptsNeeded` but dropped from
  `redeemersNeeded` → no Reward/Cert/Vote redeemer required.
- A redeemer supplied for a native-script purpose is in `suppliedSet` but NOT in
  `neededPlutusSet` → `ExtraRedeemers`.

Conway voting: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs`,
`conwayVotesNeeded`; same filter applied via the shared `hasExactSetOfRedeemers`.

### Fix per purpose

All changes are in `crates/dugite-ledger/src/validation/collateral.rs`.

**Finding 1 — `check_script_redeemers`:**

| Purpose | Old gate | New gate |
|---------|----------|----------|
| Reward (line ~370) | `(header & 0x10) != 0` — fires for ANY script stake credential | Extract bytes 1..29 as `Hash28`; gate on `script_versions.get(&sh) > 0` |
| Cert (line ~480) | `if let Some(Credential::Script(_)) = script_cred` — fires for ANY script credential | `if let Some(Credential::Script(sh)) = script_cred` + `script_versions.get(sh) > 0` |
| Vote (line ~521) | `matches!(cred, Credential::Script(_))` — fires for ANY script voter | Extract `Hash28` from `Credential::Script(h)`; gate on `script_versions.get(sh) > 0` |

**Finding 2 — `check_extra_redeemers` (new `script_versions` parameter):**

All four purposes (Spend/Cert/Reward/Vote) narrowed: only Plutus script hashes (version > 0
in `script_versions`) count as valid purposes. Native-script purposes are excluded from
`valid_purposes`, so a redeemer pointing at one is correctly flagged `ExtraRedeemer`.

Call site in `crates/dugite-ledger/src/validation/mod.rs` (line ~3728) updated to pass
the already-computed `script_versions_for_redeemers`.

**Existing tests corrected:**

8 existing tests in `crates/dugite-ledger/src/validation/tests.rs` were testing the wrong
semantic: they used hardcoded script hashes (`Hash28::from_bytes([0xXX; 28])`) that did not
match the actual Plutus script in the witness set, so `script_versions` was empty and the
tests were relying on the pre-fix unconditional behavior. Each was corrected to derive the
script hash via `blake2b_224_tagged(version, &script_bytes)` from the actual script bytes
in the witness set.

Tests fixed:
- `test_script_withdrawal_missing_reward_redeemer`
- `test_cert_redeemer_conway_deregistration_missing`
- `test_cert_redeemer_pre_conway_deregistration_missing`
- `test_cert_redeemer_positional_index_mixed_certs`
- `test_cert_redeemer_drep_unreg_requires_cert_reg_does_not`
- `test_cert_redeemer_committee_cold_resign_missing`
- `test_vote_redeemer_script_drep_missing`
- `test_vote_redeemer_wrong_index_rejected`

### New tests added (in `crates/dugite-ledger/src/validation/collateral.rs`)

| Test Name | What it verifies |
|-----------|-----------------|
| `test_reward_native_script_no_redeemer_required` | Native-script reward account — no Reward redeemer needed (positive) |
| `test_reward_plutus_script_missing_redeemer_errors` | Plutus-script reward account — MissingRedeemer{Reward,0} when absent (negative control) |
| `test_cert_native_script_no_redeemer_required` | Native-script StakeDeregistration — no Cert redeemer needed (positive) |
| `test_cert_plutus_script_missing_redeemer_errors` | Plutus-script StakeDeregistration — MissingRedeemer{Cert,0} when absent (negative control) |
| `test_vote_native_script_no_redeemer_required` | Native-script DRep voter — no Vote redeemer needed (positive) |
| `test_vote_plutus_script_missing_redeemer_errors` | Plutus-script DRep voter — MissingRedeemer{Vote,0} when absent (negative control) |
| `test_extra_redeemer_native_script_reward_is_extra` | Reward redeemer for native-script withdrawal → ExtraRedeemers (Finding 2) |
| `test_extra_redeemer_native_script_cert_is_extra` | Cert redeemer for native-script cert → ExtraRedeemers (Finding 2) |
| `test_extra_redeemer_native_script_vote_is_extra` | Vote redeemer for native-script voter → ExtraRedeemers (Finding 2) |
| `test_extra_redeemer_native_script_spend_is_extra` | Spend redeemer for native-script-locked input → ExtraRedeemers (Finding 2) |

### Test results

```
cargo nextest run -p dugite-ledger
Summary [3.042s] 1572 tests run: 1572 passed, 6 skipped

cargo clippy -p dugite-ledger --all-targets -- -D warnings
Finished dev profile — no warnings

cargo fmt --all -- --check
(no diff — clean)
```
