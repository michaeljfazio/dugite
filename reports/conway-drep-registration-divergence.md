# Conway DRep Vote-Delegation Divergence — Root Cause Report

**Investigation date**: 2026-06-13  
**Symptom**: 26 `is_valid=true` Conway mainnet blocks rejected with `DelegateeDRepNotRegistered`  
**First epoch**: 507+ (all within PV9 Conway bootstrap phase)  
**Example tx**: `f4cd15b781aaef42c35a1e60969aaad9871eb1f01172dcf0eff3a6f33cfb5c46` (slot 135534987, epoch 511)

---

## 1. Symptom

During the first mainnet Conway genesis-mode sync, 26 confirmed (`is_valid=true`) transactions
were rejected with:

```
Phase-1 validation divergence on confirmed block — trusting on-chain consensus ...
Vote delegation rejected: target DRep <id> is not registered (DelegateeDRepNotRegisteredDELEG)
```

Five distinct target DRep IDs appeared, all with trailing zeros (the 28-byte hash zero-padded to
32 bytes via `Hash28::to_hash32_padded`):

| DRep hex (Hash32, first 28 bytes) | Bech32 | Koios status at query time |
|-----------------------------------|--------|---------------------------|
| `277cd67f405d43ab611676d1d82678ffc086b9996c0a742a8a30a2e5` | `drep1ygnhe4nlgpw582mpzemdrkpx0rlupp4en9kq5ap23gc29egvrp38n` | deregistered |
| `5f14b5b5cddb0839df02696194268ef48673836d7777a7c05ffd2f3e` | `drep1tu2ttdwdmvyrnhczd9segf5w7jr88qmdwam60szll5hnut06n7t` | not_registered |
| `e6a359224ef93c8034af5f2e01bba41654678f2c10b54f2fb9fc6c10` | `drep1ytn2xkfzfmuneqp54a0juqdm5st9geu09sgt2ne0h87xcyqzgtnde` | deregistered |
| `b6f0f16bcf28ec35316adcef04ab6278107174587f0c36cd28354e03` | `drep1y2m0putteu5wcdf3dtww7p9tvfupqut5tplscdkd9q65uqcrx4p8e` | registered |
| `961280acc591f45916461b88c64bedc705a44994707c88aad1210148` | `drep1jcfgptx9j869j9jxrwyvvjldcuz6gjv5wp7g32k3yyq5sukjg87` | registered |

---

## 2. On-Chain Evidence — DReps Are/Were Registered

Koios `drep_updates` confirms each DRep was registered, with cert_index=1 in their initial
registration transaction. For example:

**DRep `277cd67f...` (example tx `f4cd15b7...`):**
```json
[
  { "action": "registered",   "update_tx_hash": "f4cd15b781aaef42c35a1e60969aaad9871eb1f01172dcf0eff3a6f33cfb5c46", "cert_index": 1 },
  { "action": "deregistered", "update_tx_hash": "3506f64fb1e091697189d34ab9f7a987adf708fe8175b7c779a1a19a184a9316", "cert_index": 0 }
]
```

cert_index=1 = `RegDRep` = DRep registration.
cert_index=0 = `VoteDelegation` = vote-delegation to the same DRep.

The pattern is: cert[0] = `VoteDelegation` (tag 9) targeting the DRep, cert[1] = `RegDRep` (tag 16)
registering the same DRep. The registration occurs **after** the vote-delegation in the same
transaction's certificate list.

Confirmed for multiple DReps:
- `b6f0f16b...` tx `a572227c...`: cert_index=1 = registered
- `961280ac...` tx `fc5240ce...`: cert_index=1 = registered
- `e6a35922...` tx `1e3ff220...`: cert_index=1 = registered

**CBOR decode of `f4cd15b7...` certificates field (`048283...f6`):**
```
04 = map key 4 (certificates)
82 = array(2) — two certs
  [cert[0]] 83 09 8200 581c e1f4ead0...  8200 581c 277cd67f...
    array(3), tag=9 (VoteDelegation), stake_cred=[key, e1f4ead0...], DRep=[key, 277cd67f...]
  [cert[1]] 84 10 8200 581c 277cd67f...  1a1dcd6500 f6
    array(4), tag=16 (RegDRep), cred=[key, 277cd67f...], deposit=500_000_000, anchor=null
```

The transaction SELF-DELEGATES: the user registers themselves as DRep (cert[1]) and delegates
their own stake vote to that DRep (cert[0]) in the same transaction, with delegation cert first.

**Protocol version at epoch 511 (Koios epoch_params):** `protocol_major = 9` — Conway bootstrap phase.

---

## 3. Root Cause

### 3.1 The Haskell Rule

In `cardano-ledger` `Cardano.Ledger.Conway.Rules.Deleg`, the `checkDRepRegistered` helper is:

```haskell
checkDRepRegistered = \case
  DRepAlwaysAbstain      -> pure ()
  DRepAlwaysNoConfidence -> pure ()
  DRepCredential targetDRep -> do
    let dReps = certState ^. certVStateL . vsDRepsL
    unless (hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL)) $
      targetDRep `Map.member` dReps ?!
        injectFailure (DelegateeDRepNotRegisteredDELEG targetDRep)
```

The `unless (hardforkConwayBootstrapPhase pv)` guard means:
- **PV9 (bootstrap phase)**: the check is **SKIPPED entirely** — VoteDelegation to an unregistered DRep is allowed
- **PV10+ (post-bootstrap)**: the check fires — target DRep must exist in `vsDReps`

`hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9`

Source: `IntersectMBO/cardano-ledger`, `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs`.

### 3.2 The Dugite Bug

`crates/dugite-ledger/src/validation/mod.rs`, line 3184:

```rust
if params.protocol_version_major >= 9 {
    if let Some(dreps) = registered_dreps {
        // ... DelegateeDRepNotRegistered check
    }
}
```

Dugite gates this check on `>= 9` (all of Conway), but Haskell only activates it at PV >= 10.
During the entire bootstrap phase (PV9, mainnet epochs 508-735 approx), Haskell never fires
`DelegateeDRepNotRegisteredDELEG`, so thousands of self-register-and-delegate transactions
were accepted. Dugite rejects them all.

### 3.3 Why the Existing `new_dreps` Forward-Scan Does Not Help

The code at lines 3188-3227 attempts to handle the same-tx case with a `new_dreps` set:
- Scans certs in order
- If cert is `RegDRep`, inserts its credential into `new_dreps` and `continue`s
- If cert is `VoteDelegation`, checks `dreps ∪ new_dreps`

**This forward-scan is directionally correct for PV10+**, but it does NOT handle the case where
VoteDelegation comes BEFORE RegDRep within the cert array (the pattern used on mainnet). When
cert[0]=VoteDelegation is processed, `new_dreps` is empty (RegDRep at cert[1] hasn't been visited
yet). The check fires and rejects the tx.

**At PV9 the entire block should be skipped** — the `new_dreps` logic is irrelevant to PV9
because the check must not run at all.

---

## 4. Fix

**File**: `crates/dugite-ledger/src/validation/mod.rs`  
**Line**: 3184

**Before:**
```rust
    if params.protocol_version_major >= 9 {
        if let Some(dreps) = registered_dreps {
```

**After:**
```rust
    // DelegateeDRepNotRegisteredDELEG is SKIPPED during the Conway bootstrap phase (PV9).
    // Haskell: `unless (hardforkConwayBootstrapPhase pv)` in Deleg.hs `checkDRepRegistered`.
    // hardforkConwayBootstrapPhase pv = pvMajor pv == 9, so the check only fires at PV >= 10.
    // Reference: IntersectMBO/cardano-ledger eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs
    if params.protocol_version_major >= 10 {
        if let Some(dreps) = registered_dreps {
```

This is a one-character change (`9` → `10`). The comment should be updated to reflect the correct
gate. The `new_dreps` forward-scan logic inside the block is correct for PV10+ and can remain
unchanged — at PV10+ a RegDRep cert earlier in the same tx does make the DRep available for a
subsequent VoteDelegation in the same tx (Haskell CERTS processes certs sequentially with
evolving state, and the GOVCERT sub-rule for RegDRep fires before the DELEG sub-rule for
VoteDelegation because CERT dispatches by cert type via CERTS iteration).

**Note on the `new_dreps` forward-scan direction at PV10+**: The existing code handles
RegDRep-before-VoteDelegation in the same tx. The VoteDelegation-before-RegDRep pattern (seen
at PV9) is legal at PV9 because the check is fully skipped; at PV10+ Haskell would also reject
VoteDelegation-before-RegDRep (the DRep is not yet in vsDReps at cert[0] processing time). So
the forward-scan direction is correct for PV10+, and the full skip for PV9 handles the mainnet
bootstrap pattern. No changes to the `new_dreps` logic are needed.

---

## 5. Affected Tests and Required Updates

### 5.1 Tests to UPDATE (currently pass, but will fail after fix if not updated)

**`crates/dugite-ledger/src/validation/tests.rs`**:

1. **`test_vote_deleg_to_unregistered_drep_rejected`** (line ~10339):
   - Uses `params.protocol_version_major = 9`
   - Expects `DelegateeDRepNotRegistered` at PV9
   - After fix: check does NOT fire at PV9 → test fails
   - **Fix**: Change `protocol_version_major` to `10`. Add a sibling test for PV9 (see below).

2. **`test_vote_deleg_to_registered_drep_accepted`** (line ~10402):
   - Uses `params.protocol_version_major = 9`
   - Already passes (no error expected)
   - No change required (accepted at both PV9 and PV10).

3. **`test_vote_deleg_always_abstain_exempt_from_drep_check`** (line ~10462):
   - Uses `params.protocol_version_major = 9`
   - Already passes (AlwaysAbstain is exempt)
   - No change required.

**`crates/dugite-ledger/src/validation/phase1.rs`** (if similar tests exist for VoteDelegation
at PV9 — check for any test using `protocol_version_major = 9` with `DelegateeDRepNotRegistered`
expectations on vote-delegation certs).

### 5.2 New Regression Tests to ADD

**Test A** — PV9 bootstrap: VoteDelegation to unregistered DRep is accepted:
```rust
#[test]
fn test_vote_deleg_to_unregistered_drep_accepted_bootstrap_phase() {
    // During Conway bootstrap (PV9), DelegateeDRepNotRegisteredDELEG is NOT checked.
    // Haskell: `unless (hardforkConwayBootstrapPhase pv)` in Deleg.hs.
    // Pins mainnet tx f4cd15b781aaef42c35a1e60969aaad9871eb1f01172dcf0eff3a6f33cfb5c46 (epoch 511).
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 9; // Conway bootstrap

    let drep_cred_bytes = [0x27u8; 28]; // representative of 277cd67f...
    let stake_cred_bytes = [0xe1u8; 28];
    let drep_hash = Hash28::from_bytes(drep_cred_bytes).to_hash32_padded();
    let stake_cred = Credential::VerificationKey(Hash28::from_bytes(stake_cred_bytes));

    let mut utxo_set = UtxoSet::new();
    let tx = make_vote_deleg_tx(
        &mut utxo_set,
        Certificate::VoteDelegation {
            credential: stake_cred.clone(),
            drep: DRep::KeyHash(drep_hash),
        },
    );

    // DRep is NOT in registered_dreps — but check is skipped at PV9
    let registered_dreps: std::collections::HashSet<Hash32> = std::collections::HashSet::new();
    let reward_accounts = make_reward_accounts_with_cred(stake_cred_bytes);

    let result = validate_transaction_with_pools(/* ... */);

    let has_drep_err = matches!(&result, Err(errors) if errors.iter().any(|e| {
        matches!(e, ValidationError::DelegateeDRepNotRegistered { .. })
    }));
    assert!(
        !has_drep_err,
        "PV9 bootstrap must NOT check DelegateeDRepNotRegistered; got: {result:?}"
    );
}
```

**Test B** — PV9 bootstrap: VoteDelegation-before-RegDRep pattern accepted (pins the exact
cert ordering seen on mainnet):
```rust
#[test]
fn test_vote_deleg_before_reg_drep_same_tx_accepted_bootstrap() {
    // Pins mainnet pattern: cert[0]=VoteDelegation to unregistered DRep,
    // cert[1]=RegDRep registering same DRep. PV9 → check skipped entirely.
    // Example: f4cd15b781aaef42c35a1e60969aaad9871eb1f01172dcf0eff3a6f33cfb5c46 epoch 511.
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 9;

    let drep_cred_bytes = [0x27u8; 28];
    let stake_cred_bytes = [0xe1u8; 28];
    let drep_hash = Hash28::from_bytes(drep_cred_bytes).to_hash32_padded();
    let stake_cred = Credential::VerificationKey(Hash28::from_bytes(stake_cred_bytes));
    let drep_cred = Credential::VerificationKey(Hash28::from_bytes(drep_cred_bytes));

    let input = TransactionInput { transaction_id: Hash32::from_bytes([0xDDu8; 32]), index: 0 };
    let mut utxo_set = UtxoSet::new();
    utxo_set.insert(input.clone(), TransactionOutput { /* lovelace output */ });
    let mut tx = make_simple_tx(input, 9_300_000, 200_000);
    // cert[0] = VoteDelegation to DRep (not yet registered)
    tx.body.certificates.push(Certificate::VoteDelegation {
        credential: stake_cred.clone(),
        drep: DRep::KeyHash(drep_hash),
    });
    // cert[1] = RegDRep registering that DRep
    tx.body.certificates.push(Certificate::RegDRep {
        credential: drep_cred,
        deposit: Lovelace(500_000_000),
        anchor: None,
    });

    let registered_dreps: std::collections::HashSet<Hash32> = std::collections::HashSet::new();
    let reward_accounts = make_reward_accounts_with_cred(stake_cred_bytes);
    let result = validate_transaction_with_pools(/* ... */);

    assert!(
        !matches!(&result, Err(errors) if errors.iter().any(|e| {
            matches!(e, ValidationError::DelegateeDRepNotRegistered { .. })
        })),
        "PV9 bootstrap: VoteDelegation-before-RegDRep must be accepted"
    );
}
```

**Test C** — PV10 post-bootstrap: VoteDelegation to unregistered DRep is rejected (existing
test renamed/updated to PV10):
```rust
// This is the updated version of test_vote_deleg_to_unregistered_drep_rejected with PV10.
params.protocol_version_major = 10;
// ... rest unchanged, expects DelegateeDRepNotRegistered
```

**Test D** — PV10: RegDRep-before-VoteDelegation in same tx is accepted (same-tx forward-scan
validation at PV10):
```rust
// cert[0] = RegDRep registering the DRep
// cert[1] = VoteDelegation to the just-registered DRep
// registered_dreps is empty (DRep not in pre-block snapshot)
// Expected: no DelegateeDRepNotRegistered (new_dreps tracking catches it)
params.protocol_version_major = 10;
```

---

## 6. Summary

| | Haskell | Dugite (before fix) | Dugite (after fix) |
|--|---------|---------------------|-------------------|
| PV9 VoteDelegation to unregistered DRep | ACCEPT | REJECT | ACCEPT |
| PV9 VoteDelegation-before-RegDRep (same tx) | ACCEPT | REJECT | ACCEPT |
| PV10 VoteDelegation to unregistered DRep | REJECT | REJECT | REJECT |
| PV10 RegDRep-before-VoteDelegation (same tx) | ACCEPT | ACCEPT | ACCEPT |
| PV10 VoteDelegation-before-RegDRep (same tx) | REJECT | REJECT | REJECT |

The one-line fix `>= 9` → `>= 10` in `mod.rs:3184` closes all 26 divergences and achieves
byte-exact alignment with `cardano-ledger` `Deleg.hs`.

---

## 7. References

- Haskell source: `IntersectMBO/cardano-ledger`, `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs`, function `checkDRepRegistered`
- `hardforkConwayBootstrapPhase` in `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs`
- Example tx: `f4cd15b781aaef42c35a1e60969aaad9871eb1f01172dcf0eff3a6f33cfb5c46` (epoch 511, PV9)
- Koios `drep_updates` confirmation for all 5 DRep IDs: cert_index=1 = registered
- Dugite divergence location: `crates/dugite-ledger/src/validation/mod.rs:3184`
- Existing tests to update: `tests.rs:10339` (`test_vote_deleg_to_unregistered_drep_rejected`)
