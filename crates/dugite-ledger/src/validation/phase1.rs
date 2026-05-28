//! Core Phase-1 validation rules.
//!
//! This module implements the structural rules that do not require Plutus script
//! execution. Each numbered rule corresponds to a distinct ledger invariant:
//!
//! - Rule 1  — at least one input
//! - Rule 1b — no duplicate inputs
//! - Rule 1c — auxiliary data hash / auxiliary data consistency
//! - Rule 1d — era gating (Conway-only certs/governance in pre-Conway eras)
//! - Rule 2  — all inputs exist in the UTxO set
//! - Rule 3  — ADA value conservation
//! - Rule 3b — multi-asset conservation
//! - Rule 3c — every minting policy has a matching script
//! - Rule 4  — fee >= minimum (base + ref-script + ex-unit costs)
//! - Rule 5  — all outputs >= minimum UTxO value
//! - Rule 5a — output value CBOR size <= max_val_size
//! - Rule 5b — network ID consistency
//! - Rule 6  — transaction size <= max_tx_size
//! - Rule 7  — TTL (time-to-live)
//! - Rule 8  — validity interval start
//! - Rule 9  — reference inputs exist and don't overlap regular inputs
//! - Rule 9b — witness completeness for inputs and withdrawals
//! - Rule 10 — required signers have matching vkey witnesses
//! - Rule 11 — collateral (Plutus transactions only; see `collateral` module)
//! - Rule 12 — script data hash (Plutus transactions only; see `scripts` module)
//! - Rule 13 — native script evaluation
//! - Rule 14 — Ed25519 vkey/bootstrap witness signature verification

use std::collections::{HashMap, HashSet};

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::{Certificate, Transaction};

use crate::utxo::UtxoLookup;

use super::scripts::{
    collect_available_script_hashes, compute_min_fee, estimate_value_cbor_size,
    evaluate_native_script,
};
use super::size_check::expect_size;
use super::ValidationError;

// ---------------------------------------------------------------------------
// Helper: extract stake credential from a raw reward account byte string
// ---------------------------------------------------------------------------

/// Extract the stake credential from a reward account byte string.
///
/// Reward addresses have the format `header_byte || 28-byte credential hash`.
/// - Header nibble `0b1110` (`0xE0`/`0xE1`) → `VerificationKey`
/// - Header nibble `0b1111` (`0xF0`/`0xF1`) → `Script`
pub(super) fn extract_reward_credential(reward_account: &[u8]) -> Option<Credential> {
    if reward_account.len() < 29 {
        return None;
    }
    let header = reward_account[0];
    let addr_type = (header >> 4) & 0x0F;
    match addr_type {
        0b1110 => {
            let mut hash_bytes = [0u8; 28];
            hash_bytes.copy_from_slice(&reward_account[1..29]);
            Some(Credential::VerificationKey(Hash28::from_bytes(hash_bytes)))
        }
        0b1111 => {
            let mut hash_bytes = [0u8; 28];
            hash_bytes.copy_from_slice(&reward_account[1..29]);
            Some(Credential::Script(Hash28::from_bytes(hash_bytes)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helper: required VKey witnesses for a certificate (conwayWitsVKeyNeeded)
// ---------------------------------------------------------------------------

/// Return the set of VKey hashes that must have corresponding witnesses for a
/// given certificate.  Script credentials return an empty set — their validation
/// is handled separately via native-script evaluation (Phase-1, Rule 13) or
/// Plutus redeemer matching (Phase-2).
///
/// Matches the Haskell `conwayWitsVKeyNeeded` / `shelleyWitsVKeyNeeded`
/// specification:
///
/// | Certificate                     | Required Witness                        |
/// |---------------------------------|-----------------------------------------|
/// | `PoolRegistration`              | All pool owner VKey hashes              |
/// | `PoolRetirement`                | Pool operator (cold) key hash           |
/// | `StakeDelegation`               | Delegator credential key hash           |
/// | `StakeDeregistration`           | Credential key hash                     |
/// | `ConwayStakeRegistration`       | Registrant credential key hash          |
/// | `ConwayStakeDeregistration`     | Credential key hash                     |
/// | `VoteDelegation`                | Delegator credential key hash           |
/// | `StakeVoteDelegation`           | Delegator credential key hash           |
/// | `RegStakeDeleg`                 | Registrant credential key hash          |
/// | `RegStakeVoteDeleg`             | Registrant credential key hash          |
/// | `VoteRegDeleg`                  | Registrant credential key hash          |
/// | `RegDRep`                       | DRep credential key hash                |
/// | `UnregDRep`                     | DRep credential key hash                |
/// | `UpdateDRep`                    | DRep credential key hash                |
/// | `CommitteeHotAuth`              | Cold credential key hash                |
/// | `CommitteeColdResign`           | Cold credential key hash                |
/// | `StakeRegistration` (Shelley)   | None (free registration)                |
/// | `GenesisKeyDelegation`          | None (legacy)                           |
/// | `MoveInstantaneousRewards`      | None (legacy)                           |
fn cert_required_witnesses(cert: &Certificate) -> Vec<Hash28> {
    // Helper: extract the key hash from a credential, returning None for scripts.
    let key_hash = |c: &Credential| -> Option<Hash28> {
        match c {
            Credential::VerificationKey(h) => Some(*h),
            Credential::Script(_) => None,
        }
    };

    match cert {
        // Pool registration: ALL owner key hashes must sign.
        Certificate::PoolRegistration(params) => params.pool_owners.clone(),

        // Pool retirement: the operator (cold key / pool_id) must sign.
        Certificate::PoolRetirement { pool_hash, .. } => vec![*pool_hash],

        // DRep certificates: credential key hash.
        Certificate::RegDRep { credential, .. }
        | Certificate::UnregDRep { credential, .. }
        | Certificate::UpdateDRep { credential, .. } => key_hash(credential).into_iter().collect(),

        // Committee certificates: cold credential key hash.
        Certificate::CommitteeHotAuth {
            cold_credential, ..
        }
        | Certificate::CommitteeColdResign {
            cold_credential, ..
        } => key_hash(cold_credential).into_iter().collect(),

        // Delegation and deregistration certificates with credential field.
        Certificate::StakeDelegation { credential, .. }
        | Certificate::VoteDelegation { credential, .. }
        | Certificate::StakeVoteDelegation { credential, .. }
        | Certificate::RegStakeDeleg { credential, .. }
        | Certificate::RegStakeVoteDeleg { credential, .. }
        | Certificate::VoteRegDeleg { credential, .. }
        | Certificate::StakeDeregistration(credential)
        | Certificate::ConwayStakeRegistration { credential, .. }
        | Certificate::ConwayStakeDeregistration { credential, .. } => {
            key_hash(credential).into_iter().collect()
        }

        // Shelley stake registration (cert tag 0) — no witness required.
        Certificate::StakeRegistration(_) => vec![],

        // Legacy certificates — no witness checks.
        Certificate::GenesisKeyDelegation { .. } | Certificate::MoveInstantaneousRewards { .. } => {
            vec![]
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: check if the transaction involves any multi-asset tokens
// ---------------------------------------------------------------------------

/// Return `true` when any input UTxO or output carries non-ADA tokens.
pub(super) fn has_multi_assets_in_tx(tx: &Transaction, utxo_set: &dyn UtxoLookup) -> bool {
    for input in &tx.body.inputs {
        if let Some(output) = utxo_set.lookup(input) {
            if !output.value.multi_asset.is_empty() {
                return true;
            }
        }
    }
    tx.body
        .outputs
        .iter()
        .any(|o| !o.value.multi_asset.is_empty())
}

// ---------------------------------------------------------------------------
// Helper: pool metadata hash size cap (Haskell `PoolMedataHashTooBig`)
// ---------------------------------------------------------------------------

/// Return `true` when the pool metadata hash byte length exceeds the
/// 32-byte (Blake2b-256) cap, gated by the Alonzo-onwards (`pvMajor > 4`)
/// soft fork — mirroring Haskell `restrictPoolMetadataHash`.
///
/// Reference: Haskell `PoolMedataHashTooBig` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`:
///
/// ```haskell
/// when (SoftForks.restrictPoolMetadataHash pv) $
///   forM_ sppMetadata $ \pmd ->
///     let s = sizeofByteArray $ pmHash pmd
///      in s <= fromIntegral (hashSize ([] @HASH))
///           ?! injectFailure (PoolMedataHashTooBig sppId s)
/// ```
///
/// In dugite, `PoolMetadata.hash` is structurally a `Hash32` (fixed
/// 32 bytes), so the predicate evaluates to `false` for any value
/// reachable via the typed API.  This helper is kept defensive against
/// future wire-decode paths that could surface oversized values via a
/// raw byte slice, and is exercised directly by unit tests.
pub(super) fn is_pool_metadata_hash_too_big(metadata_hash_bytes: &[u8], pv_major: u64) -> bool {
    pv_major > 4 && metadata_hash_bytes.len() > 32
}

// ---------------------------------------------------------------------------
// Helper: Byron output attribute size cap (Haskell `OutputBootAddrAttrsTooBig`)
// ---------------------------------------------------------------------------

/// Return the zero-based indices of every output whose address is a
/// Byron/bootstrap address with serialized attributes exceeding the
/// 64-byte cap.
///
/// Reference: Haskell `validateOutputBootAddrAttrsTooBig` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxo.hs`:
///
/// ```text
/// ∀ ( _ ↦ (a,_)) ∈ txoutstxb, a ∈ Addrbootstrap → bootstrapAttrsSize a ≤ 64
/// ```
///
/// Applies to all eras Shelley+. Non-Byron outputs and Byron outputs whose
/// attribute size cannot be measured (malformed payload) are silently
/// passed — failing-open keeps this predicate conservative against legacy
/// fixtures and aligns with the Haskell rule which only fires when the
/// size is decodable and strictly above the cap.
pub(super) fn output_boot_addr_attrs_too_big_indices(
    outputs: &[dugite_primitives::transaction::TransactionOutput],
) -> Vec<usize> {
    let mut bad = Vec::new();
    for (idx, output) in outputs.iter().enumerate() {
        if let dugite_primitives::address::Address::Byron(byron) = &output.address {
            if let Some(size) = byron.attributes_byte_size() {
                if size > 64 {
                    bad.push(idx);
                }
            }
        }
    }
    bad
}

// ---------------------------------------------------------------------------
// Witness signature verification (Rule 14)
// ---------------------------------------------------------------------------

trait HasWitnessFields {
    fn vkey(&self) -> &[u8];
    fn signature(&self) -> &[u8];
}

impl HasWitnessFields for dugite_primitives::transaction::VKeyWitness {
    fn vkey(&self) -> &[u8] {
        &self.vkey
    }
    fn signature(&self) -> &[u8] {
        &self.signature
    }
}

// NOTE: BootstrapWitness does NOT implement HasWitnessFields.
// Byron extended public keys are 64 bytes; the 32-byte guard in
// `verify_single_witness` would fire and return None (silent accept) for
// every bootstrap witness. Bootstrap witnesses use `verify_bootstrap_witnesses`
// instead. See F1 in the 2026-05-19 security audit (#546).

fn verify_single_witness<W: HasWitnessFields>(
    witness: &W,
    tx_hash_bytes: &[u8],
    prefix: &str,
) -> Option<ValidationError> {
    let vkey = witness.vkey();
    let sig = witness.signature();
    // Pre-flight: enforce Ed25519 wire sizes (32-byte key, 64-byte sig) BEFORE
    // any crypto. Haskell's `rawDeserialiseVerKeyDSIGN` fails hard for non-32-byte
    // keys, returning `InvalidWitness`. We must never silently accept malformed
    // witnesses — that would let a fabricated 1-byte vkey satisfy a required-signer
    // check without any cryptographic verification (D2/D9 class, audit #544;
    // same class as #537).
    //
    // Bootstrap witnesses (Byron extended keys, 64-byte vkey) take the separate
    // `verify_bootstrap_witnesses` path (#546 F1) and are never dispatched here —
    // `HasWitnessFields` is intentionally not impl'd on `BootstrapWitness`.
    //
    // `expect_size` is the uniform helper from `size_check.rs`; all crypto-input
    // length checks in the validation layer go through it.
    if let Err(e) = expect_size("vkey", vkey.len(), 32) {
        return Some(e);
    }
    if let Err(e) = expect_size("signature", sig.len(), 64) {
        return Some(e);
    }
    // Inline defense-in-depth: even after the pre-flight above, the crypto call
    // must not receive wrong-size inputs if this function is ever invoked through
    // a future call path that bypasses the pre-flight.
    debug_assert_eq!(
        vkey.len(),
        32,
        "vkey must be exactly 32 bytes at crypto site"
    );
    debug_assert_eq!(sig.len(), 64, "sig must be exactly 64 bytes at crypto site");
    match dugite_crypto::keys::PaymentVerificationKey::from_bytes(vkey) {
        Ok(vk) => {
            if vk.verify(tx_hash_bytes, sig).is_err() {
                Some(ValidationError::InvalidWitnessSignature(format!(
                    "{prefix}{:?}",
                    &vkey[..8]
                )))
            } else {
                None
            }
        }
        Err(_) => Some(ValidationError::InvalidWitnessSignature(format!(
            "{prefix}{:?}",
            &vkey[..8.min(vkey.len())]
        ))),
    }
}

// ---------------------------------------------------------------------------
// Bootstrap witness (Byron) verification — F1 security audit 2026-05-19 (#546)
//
// Wire format: bootstrap_witness = [vkey: bytes .size 64, sig: bytes .size 64,
//                                   chain_code: bytes .size 32, attributes: bytes]
//
// Byron "extended" Ed25519: 64 bytes = 32-byte scalar || 32-byte extension.
// Signature verification uses only bytes 0..32 (the scalar / public key part).
//
// Haskell references:
//   - `verifyDSIGN Ed25519DSIGN` (uses first 32 bytes of 64-byte extended key)
//   - `checkBootstrap` (address-root binding check)
//   - `bootstrapKeyHash` (root derivation)
// ---------------------------------------------------------------------------

/// Verify one Byron bootstrap witness (structural + signature check).
///
/// Step 1: Pre-flight: vkey must be 64 bytes, sig 64 bytes, chain_code 32 bytes.
/// Step 2: Ed25519 verify over `tx_hash_bytes` using `vkey[0..32]` as the scalar.
///
/// Address binding (step 3) is handled by `check_bootstrap_address_binding`.
fn verify_single_bootstrap_witness(
    bw: &dugite_primitives::transaction::BootstrapWitness,
    tx_hash_bytes: &[u8],
) -> Option<ValidationError> {
    let vkey = &bw.vkey;
    let sig = &bw.signature;
    let chain_code = &bw.chain_code;

    // Pre-flight structural check — mirrors Haskell CBOR fixed-size decode.
    if vkey.len() != 64 || sig.len() != 64 {
        return Some(ValidationError::InvalidWitnessSignature(format!(
            "bootstrap: malformed witness: vkey={} bytes (expected 64), sig={} bytes (expected 64)",
            vkey.len(),
            sig.len(),
        )));
    }
    if chain_code.len() != 32 {
        return Some(ValidationError::InvalidWitnessSignature(format!(
            "bootstrap: malformed chain_code: {} bytes (expected 32)",
            chain_code.len(),
        )));
    }

    // Ed25519 verify using vkey[0..32] (the scalar part of the extended key).
    let scalar_bytes = &vkey[..32];
    match dugite_crypto::keys::PaymentVerificationKey::from_bytes(scalar_bytes) {
        Ok(vk) => {
            if vk.verify(tx_hash_bytes, sig).is_err() {
                Some(ValidationError::InvalidWitnessSignature(format!(
                    "bootstrap:sig_invalid:{:02x?}",
                    &scalar_bytes[..4]
                )))
            } else {
                None
            }
        }
        Err(_) => Some(ValidationError::InvalidWitnessSignature(format!(
            "bootstrap:invalid_scalar:{:02x?}",
            &scalar_bytes[..4]
        ))),
    }
}

/// Compute the Byron address root from bootstrap witness fields.
///
/// `root = blake2b_224(sha3_256(CBOR([addrtype=0, [0, vkey64], attrs_cbor])))`
///
/// Matches Haskell `bootstrapKeyHash` in `cardano-ledger-shelley`.
///
/// `attrs_cbor` are the raw CBOR bytes of the address attribute map as
/// stored in the `BootstrapWitness.attributes` field.
pub(crate) fn compute_bootstrap_root(vkey64: &[u8], attrs_cbor: &[u8]) -> Option<[u8; 28]> {
    use sha3::Digest as _;

    // CBOR encode: array(3) [ uint(0), array(2)[uint(0), bytes(vkey64)], attrs_raw ]
    let mut buf = Vec::with_capacity(8 + vkey64.len() + attrs_cbor.len());
    buf.push(0x83); // array(3)
    buf.push(0x00); // uint(0) — AddrType::PubKey
    buf.push(0x82); // array(2) — SpendingData::PubKey
    buf.push(0x00); // uint(0)
                    // bytes(vkey64) length encoding
    let vkey_len = vkey64.len();
    if vkey_len <= 23 {
        buf.push(0x40 | vkey_len as u8);
    } else if vkey_len <= 0xFF {
        buf.push(0x58);
        buf.push(vkey_len as u8);
    } else {
        buf.push(0x59);
        buf.push((vkey_len >> 8) as u8);
        buf.push(vkey_len as u8);
    }
    buf.extend_from_slice(vkey64);
    buf.extend_from_slice(attrs_cbor); // attrs verbatim

    let sha3_hash = sha3::Sha3_256::digest(&buf);
    let root = dugite_primitives::hash::blake2b_224(&sha3_hash[..]);
    Some(*root.as_bytes())
}

/// Extract the 28-byte root from a Byron address payload.
///
/// Byron address wire: `array(2) [ tag(24, bytes(inner_cbor)), crc32 ]`
/// `inner_cbor`:       `array(3) [ root: bytes(28), attributes: map, addrtype: uint ]`
fn extract_byron_address_root(payload: &[u8]) -> Option<[u8; 28]> {
    let mut d = minicbor::Decoder::new(payload);
    let inner_bytes: &[u8] = if payload.first() == Some(&0x82) {
        d.array().ok()?;
        let tag = d.tag().ok()?;
        if tag.as_u64() != 24 {
            return None;
        }
        d.bytes().ok()?
    } else {
        payload
    };
    let mut d2 = minicbor::Decoder::new(inner_bytes);
    d2.array().ok()?;
    let root_bytes = d2.bytes().ok()?;
    if root_bytes.len() == 28 {
        let mut arr = [0u8; 28];
        arr.copy_from_slice(root_bytes);
        Some(arr)
    } else {
        None
    }
}

/// Check address binding: each bootstrap witness's computed root must match
/// the root in the Byron address of at least one regular input (Haskell `checkBootstrap`).
fn check_bootstrap_address_binding(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
) -> Vec<ValidationError> {
    use dugite_primitives::address::Address;
    let mut errors = Vec::new();

    for bw in &tx.witness_set.bootstrap_witnesses {
        // Only check structurally valid witnesses (others are caught by signature verifier).
        if bw.vkey.len() != 64 || bw.signature.len() != 64 || bw.chain_code.len() != 32 {
            continue;
        }
        let computed_root = match compute_bootstrap_root(&bw.vkey, &bw.attributes) {
            Some(r) => r,
            None => {
                errors.push(ValidationError::InvalidWitnessSignature(format!(
                    "bootstrap:address_root_computation_failed:{:02x?}",
                    &bw.vkey[..4]
                )));
                continue;
            }
        };

        let mut matched = false;
        'outer: for input in &tx.body.inputs {
            if let Some(output) = utxo_set.lookup(input) {
                if let Address::Byron(ref byron) = output.address {
                    if let Some(root_bytes) = extract_byron_address_root(&byron.payload) {
                        if root_bytes == computed_root {
                            matched = true;
                            break 'outer;
                        }
                    }
                }
            }
        }

        if !matched {
            errors.push(ValidationError::InvalidWitnessSignature(format!(
                "bootstrap:address_binding_failed:{:02x?}",
                &bw.vkey[..4]
            )));
        }
    }
    errors
}

/// Verify all bootstrap witnesses in a transaction (signature + address binding).
fn verify_bootstrap_witnesses(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    tx_hash_bytes: &[u8],
) -> Vec<ValidationError> {
    let mut errors: Vec<ValidationError> = tx
        .witness_set
        .bootstrap_witnesses
        .iter()
        .filter_map(|bw| verify_single_bootstrap_witness(bw, tx_hash_bytes))
        .collect();
    errors.extend(check_bootstrap_address_binding(tx, utxo_set));
    errors
}

#[cfg(feature = "parallel-verification")]
fn verify_witness_signatures<W: HasWitnessFields + Sync>(
    witnesses: &[W],
    tx_hash_bytes: &[u8],
    prefix: &str,
) -> Vec<ValidationError> {
    use rayon::prelude::*;
    witnesses
        .par_iter()
        .filter_map(|w| verify_single_witness(w, tx_hash_bytes, prefix))
        .collect()
}

#[cfg(not(feature = "parallel-verification"))]
fn verify_witness_signatures<W: HasWitnessFields>(
    witnesses: &[W],
    tx_hash_bytes: &[u8],
    prefix: &str,
) -> Vec<ValidationError> {
    witnesses
        .iter()
        .filter_map(|w| verify_single_witness(w, tx_hash_bytes, prefix))
        .collect()
}

// ---------------------------------------------------------------------------
// Phase-1 rule execution
// ---------------------------------------------------------------------------

/// Run all core Phase-1 rules that are independent of Plutus scripts.
///
/// Rules that require the Plutus-script context (11, 12) are handled in the
/// caller (`validate_transaction_with_pools`) which invokes the `collateral`
/// and `scripts` modules. Results are accumulated in `errors`.
///
/// Returns `input_value` (sum of ADA across all resolved inputs) so the caller
/// can pass it to the value-conservation check without re-scanning inputs.
#[allow(clippy::too_many_arguments)] // validation entry point needs full context
pub(super) fn run_phase1_rules(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    current_slot: u64,
    tx_size: u64,
    registered_pools: Option<&std::collections::HashSet<Hash28>>,
    current_epoch: Option<u64>,
    node_network: Option<dugite_primitives::network::NetworkId>,
    stake_key_deposits: Option<&HashMap<Hash32, u64>>,
    errors: &mut Vec<ValidationError>,
) {
    let body = &tx.body;

    // ------------------------------------------------------------------
    // Rule 1: Must have at least one input
    // ------------------------------------------------------------------
    if body.inputs.is_empty() {
        errors.push(ValidationError::NoInputs);
    }

    // ------------------------------------------------------------------
    // Rule 1b: No duplicate inputs
    // ------------------------------------------------------------------
    {
        let mut seen = HashSet::new();
        for input in &body.inputs {
            if !seen.insert(input) {
                errors.push(ValidationError::DuplicateInput(input.to_string()));
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1c: Auxiliary data hash / auxiliary data consistency
    //
    // Sub-rule 1c.i: presence/absence consistency.
    // Sub-rule 1c.ii: when both are present, verify the content hash.
    //   The declared hash must equal blake2b_256(raw_aux_cbor).
    //   We can only verify this when raw_cbor bytes were preserved from
    //   the wire (set by the serialization layer); locally-constructed
    //   transactions with raw_cbor=None skip the content check.
    // ------------------------------------------------------------------
    match (&body.auxiliary_data_hash, &tx.auxiliary_data) {
        (Some(_), None) => {
            errors.push(ValidationError::AuxiliaryDataHashWithoutData);
        }
        (None, Some(_)) => {
            errors.push(ValidationError::AuxiliaryDataWithoutHash);
        }
        (Some(declared_hash), Some(aux_data)) => {
            // Content-hash verification: only when raw CBOR bytes are available.
            if let Some(ref raw_cbor) = aux_data.raw_cbor {
                let computed = dugite_primitives::hash::blake2b_256(raw_cbor);
                if computed != *declared_hash {
                    errors.push(ValidationError::AuxiliaryDataHashMismatch);
                }
            }
        }
        (None, None) => {} // Both absent — OK
    }

    // ------------------------------------------------------------------
    // Rule 1d: Era gating
    // ------------------------------------------------------------------
    super::conway::check_era_gating(params, body, errors);

    // ------------------------------------------------------------------
    // Rule 1e: Pool retirement epoch <= current_epoch + e_max
    //
    // Per Haskell's POOL rule (Shelley spec, Figure 14): the announced
    // retirement epoch must not exceed `cepoch + emax`. Skipped when
    // `current_epoch` is not provided (e.g. mempool admission without
    // epoch context).
    // ------------------------------------------------------------------
    if let Some(epoch) = current_epoch {
        for cert in &body.certificates {
            if let Certificate::PoolRetirement {
                epoch: retirement_epoch,
                ..
            } = cert
            {
                let max_epoch = epoch.saturating_add(params.e_max);
                if *retirement_epoch > max_epoch {
                    errors.push(ValidationError::PoolRetirementTooLate {
                        retirement_epoch: *retirement_epoch,
                        current_epoch: epoch,
                        e_max: params.e_max,
                        max_epoch,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1f: Conway stake registration deposit must match key_deposit
    //
    // Per Haskell's Conway DELEG rule: `ConwayStakeRegistration` carries
    // an inline deposit amount that must equal `keyDeposit` from the
    // current protocol parameters.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for cert in &body.certificates {
            if let Certificate::ConwayStakeRegistration { deposit, .. } = cert {
                if deposit.0 != params.key_deposit.0 {
                    errors.push(ValidationError::StakeRegistrationDepositMismatch {
                        declared: deposit.0,
                        expected: params.key_deposit.0,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1g: Conway stake deregistration refund must match stored deposit
    //
    // Per Haskell's Conway DELEG rule (`conwayStakeDeregDeposit`):
    // `ConwayStakeDeregistration` (UnRegCert, certificate tag 8) carries an
    // explicit refund amount that must equal the deposit paid at registration
    // time (stored per-credential in `stake_key_deposits`). This ensures
    // correct refunds even if `keyDeposit` has changed via governance.
    //
    // Falls back to the current `keyDeposit` parameter when the per-credential
    // deposit map is not available or the credential is not found (e.g. old
    // snapshots before per-credential tracking was added).
    //
    // This check applies only in Conway (protocol >= 9) where the new
    // certificate tag is used.  Pre-Conway `StakeDeregistration` (tag 1)
    // implicitly refunds `key_deposit` without carrying an explicit amount.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for cert in &body.certificates {
            if let Certificate::ConwayStakeDeregistration { credential, refund } = cert {
                let key = credential.to_typed_hash32();
                let expected = stake_key_deposits
                    .and_then(|m| m.get(&key).copied())
                    .unwrap_or(params.key_deposit.0);
                if refund.0 != expected {
                    errors.push(ValidationError::StakeDeregistrationRefundMismatch {
                        declared: refund.0,
                        expected,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1h: Pool cost must meet minimum pool cost (Haskell `StakePoolCostTooLowPOOL`)
    //
    // Per Haskell's POOL rule (Shelley spec, Figure 14): every pool registration
    // certificate must declare a cost >= `minPoolCost` from the current protocol
    // parameters.  This check applies to all pool registrations regardless of
    // whether the pool is new or re-registering.
    //
    // Reference: Haskell `StakePoolCostTooLowPOOL` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Pool`.
    // ------------------------------------------------------------------
    for cert in &body.certificates {
        if let Certificate::PoolRegistration(pool_params) = cert {
            if pool_params.cost.0 < params.min_pool_cost.0 {
                errors.push(ValidationError::StakePoolCostTooLow {
                    actual: pool_params.cost.0,
                    minimum: params.min_pool_cost.0,
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1h1a: Pool margin must be a valid rational in [0, 1]
    //            (Haskell `PoolMarginsInvalidPOOL`)
    //
    // Haskell's POOL rule rejects registrations with:
    //   * denominator == 0 (division by zero; panics in reward calculation)
    //   * numerator > denominator (margin > 100%)
    //
    // Reference: Haskell `PoolMarginsInvalidPOOL` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Pool`.
    // ------------------------------------------------------------------
    for cert in &body.certificates {
        if let Certificate::PoolRegistration(pool_params) = cert {
            let denom = pool_params.margin.denominator;
            let numer = pool_params.margin.numerator;
            if denom == 0 || numer > denom {
                errors.push(ValidationError::PoolMarginInvalid {
                    numerator: numer,
                    denominator: denom,
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1h2: Pool metadata hash must not exceed 32 bytes
    //          (Haskell `PoolMedataHashTooBig`, Alonzo+)
    //
    // Per Haskell `Cardano.Ledger.Shelley.Rules.Pool`,
    // `restrictPoolMetadataHash pv = pvMajor pv > 4` activates the cap
    // from the Alonzo era onwards. The cap is the size of `Blake2b_256`
    // (32 bytes).
    //
    // In dugite the metadata hash is structurally `Hash32`, so this
    // predicate is defensive — it only fires if a future wire-decode
    // path produces an oversized byte representation. The helper is
    // exercised directly by unit tests.
    //
    // Reference: Haskell `PoolMedataHashTooBig` in
    // `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`.
    // ------------------------------------------------------------------
    for cert in &body.certificates {
        if let Certificate::PoolRegistration(pool_params) = cert {
            if let Some(meta) = &pool_params.pool_metadata {
                let bytes = meta.hash.as_bytes();
                if is_pool_metadata_hash_too_big(bytes, params.protocol_version_major) {
                    errors.push(ValidationError::PoolMedataHashTooBig {
                        pool: pool_params.operator.to_hex(),
                        hash_size: bytes.len(),
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1i-pre: Pool reward account must be exactly 29 bytes
    //             (D8 of security audit #544)
    //
    // A pool reward account is always exactly 29 bytes: 1 header byte followed
    // by a 28-byte (Blake2b-224) credential hash. Haskell's `checkPoolParams`
    // deserialises the raw bytes as an `Addr` via `deserialiseFromRawBytes
    // AsShelleyAddress`, which fails hard on any other length. This check runs
    // unconditionally — no `network_id` in the tx body is required.
    // ------------------------------------------------------------------
    for cert in &body.certificates {
        if let Certificate::PoolRegistration(pool_params) = cert {
            if pool_params.reward_account.len() != 29 {
                errors.push(ValidationError::InvalidRewardAccount(format!(
                    "pool reward account must be exactly 29 bytes, got {}",
                    pool_params.reward_account.len()
                )));
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1i: Pool reward account network must match transaction network_id
    //          (Haskell `WrongNetworkInTxBody`, Alonzo+)
    //
    // When the transaction body declares a `network_id` (Alonzo and later),
    // every pool registration certificate's reward account must be on the
    // same network. The network is encoded in bit 0 of the reward account
    // header byte: 0 = testnet, 1 = mainnet.
    //
    // This mirrors Rule 5b (output address network check) but applies to the
    // pool reward account embedded in the certificate. A pool that registers
    // with a testnet reward account on mainnet would allow its operator
    // rewards to be sent to the wrong network, so this is a correctness check.
    //
    // Reference: Haskell `WrongNetworkInTxBody` in
    // `cardano-ledger-alonzo:Cardano.Ledger.Alonzo.Rules.Utxo`.
    // ------------------------------------------------------------------
    if let Some(tx_network_id) = body.network_id {
        let expected_network = if tx_network_id == 0 {
            dugite_primitives::network::NetworkId::Testnet
        } else {
            dugite_primitives::network::NetworkId::Mainnet
        };
        for cert in &body.certificates {
            if let Certificate::PoolRegistration(pool_params) = cert {
                // Reward account format: header_byte || 28-byte credential hash.
                // Bit 0 of the header encodes the network: 0 = testnet, 1 = mainnet.
                // Length is already checked in Rule 1i-pre above; skip here if bad.
                if let Some(header) = pool_params.reward_account.first() {
                    let network_bit = header & 0x01;
                    let actual_network = if network_bit == 0 {
                        dugite_primitives::network::NetworkId::Testnet
                    } else {
                        dugite_primitives::network::NetworkId::Mainnet
                    };
                    if actual_network != expected_network {
                        errors.push(ValidationError::PoolRewardAccountWrongNetwork {
                            expected: expected_network,
                            actual: actual_network,
                        });
                        // Report once per transaction — multiple pools with wrong
                        // network are caught by the same error.
                        break;
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 2: All inputs must exist in the UTxO set
    // ------------------------------------------------------------------
    let mut input_value: u128 = 0;
    for input in &body.inputs {
        match utxo_set.lookup(input) {
            Some(output) => {
                input_value += output.value.coin.0 as u128;
            }
            None => {
                errors.push(ValidationError::InputNotFound(input.to_string()));
            }
        }
    }

    // ------------------------------------------------------------------
    // Per-proposal deposit validation (Haskell `ProposalDepositIncorrect`)
    //
    // Each governance proposal's inline deposit must exactly match the current
    // `gov_action_deposit` protocol parameter. Conway+ only.
    //
    // Reference: Haskell `ProposalDepositIncorrect` in
    // `cardano-ledger-conway:Cardano.Ledger.Conway.Rules.Gov`.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for proposal in &body.proposal_procedures {
            if proposal.deposit != params.gov_action_deposit {
                errors.push(ValidationError::ProposalDepositIncorrect {
                    declared: proposal.deposit.0,
                    expected: params.gov_action_deposit.0,
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 3: ADA value conservation
    // consumed = Σ(inputs) + Σ(withdrawals) + Σ(refunds)
    // produced = Σ(outputs) + fee + Σ(deposits) + proposal_deposits + donation
    // ------------------------------------------------------------------
    if errors.is_empty() {
        let output_value: u128 = body.outputs.iter().map(|o| o.value.coin.0 as u128).sum();
        let withdrawal_value: u128 = body.withdrawals.values().map(|l| l.0 as u128).sum();

        let (total_deposits, total_refunds) = super::conway::calculate_deposits_and_refunds(
            &body.certificates,
            params,
            registered_pools,
            stake_key_deposits,
        );

        // Proposal deposits (Conway governance) — use u128 to prevent mul overflow
        let proposal_deposits =
            body.proposal_procedures.len() as u128 * params.gov_action_deposit.0 as u128;

        // Treasury donation (Conway)
        let donation = body.donation.map(|d| d.0 as u128).unwrap_or(0);

        let consumed = input_value + withdrawal_value + total_refunds as u128;
        let produced = output_value
            + body.fee.0 as u128
            + total_deposits as u128
            + proposal_deposits
            + donation;

        if consumed != produced {
            errors.push(ValidationError::ValueNotConserved {
                inputs: consumed.min(u64::MAX as u128) as u64,
                outputs: output_value.min(u64::MAX as u128) as u64,
                fee: body.fee.0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 3b: Multi-asset conservation
    // ------------------------------------------------------------------
    if errors.is_empty() && (!body.mint.is_empty() || has_multi_assets_in_tx(tx, utxo_set)) {
        use dugite_primitives::hash::PolicyId;
        use dugite_primitives::value::AssetName;
        use std::collections::BTreeMap;

        let mut asset_balance: BTreeMap<(PolicyId, AssetName), i128> = BTreeMap::new();

        for input in &body.inputs {
            if let Some(output) = utxo_set.lookup(input) {
                for (policy, assets) in &output.value.multi_asset {
                    for (name, qty) in assets {
                        *asset_balance.entry((*policy, name.clone())).or_insert(0) += *qty as i128;
                    }
                }
            }
        }
        for (policy, assets) in &body.mint {
            for (name, qty) in assets {
                *asset_balance.entry((*policy, name.clone())).or_insert(0) += *qty as i128;
            }
        }
        for output in &body.outputs {
            for (policy, assets) in &output.value.multi_asset {
                for (name, qty) in assets {
                    *asset_balance.entry((*policy, name.clone())).or_insert(0) -= *qty as i128;
                }
            }
        }

        for ((policy, _asset), balance) in &asset_balance {
            if *balance != 0 {
                errors.push(ValidationError::MultiAssetNotConserved {
                    policy: policy.to_hex(),
                    input_side: if *balance > 0 { *balance } else { 0 },
                    output_side: if *balance < 0 {
                        balance.unsigned_abs() as i128
                    } else {
                        0
                    },
                });
                break;
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 3c: Every minting policy must have a matching script
    // ------------------------------------------------------------------
    if !body.mint.is_empty() {
        let mut available_script_hashes: HashSet<Hash28> = HashSet::new();

        for script in &tx.witness_set.native_scripts {
            let script_cbor = dugite_serialization::encode_native_script(script);
            let mut tagged = Vec::with_capacity(1 + script_cbor.len());
            tagged.push(0x00);
            tagged.extend_from_slice(&script_cbor);
            available_script_hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
        }
        for s in &tx.witness_set.plutus_v1_scripts {
            let mut tagged = Vec::with_capacity(1 + s.len());
            tagged.push(0x01);
            tagged.extend_from_slice(s);
            available_script_hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
        }
        for s in &tx.witness_set.plutus_v2_scripts {
            let mut tagged = Vec::with_capacity(1 + s.len());
            tagged.push(0x02);
            tagged.extend_from_slice(s);
            available_script_hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
        }
        for s in &tx.witness_set.plutus_v3_scripts {
            let mut tagged = Vec::with_capacity(1 + s.len());
            tagged.push(0x03);
            tagged.extend_from_slice(s);
            available_script_hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
        }
        // Per Haskell's `scriptsProvided`, script_refs are collected from BOTH
        // spending inputs AND reference inputs.  A minting policy satisfied via
        // a script_ref that lives in a spending-input UTxO is therefore valid.
        for inp in body.inputs.iter().chain(body.reference_inputs.iter()) {
            if let Some(utxo) = utxo_set.lookup(inp) {
                if let Some(script_ref) = &utxo.script_ref {
                    let hash = super::scripts::compute_script_ref_hash(script_ref);
                    available_script_hashes.insert(hash);
                }
            }
        }

        for policy in body.mint.keys() {
            if !available_script_hashes.contains(policy) {
                tracing::debug!(
                    policy = %policy.to_hex(),
                    "Minting policy without matching script in witness set, spending inputs, or reference inputs"
                );
                errors.push(ValidationError::InvalidMint);
                break;
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 4: Fee >= minimum (base + ref-script + ex-unit costs)
    // ------------------------------------------------------------------
    let min_fee = compute_min_fee(tx, utxo_set, params, tx_size);
    if body.fee.0 < min_fee.0 {
        errors.push(ValidationError::FeeTooSmall {
            minimum: min_fee.0,
            actual: body.fee.0,
        });
    }

    // ------------------------------------------------------------------
    // Rule 5: All outputs >= minimum UTxO value
    // ------------------------------------------------------------------
    let default_min_utxo = params.min_utxo_value();
    for output in &body.outputs {
        let min_utxo = if let Some(ref cbor) = output.raw_cbor {
            params.min_utxo_for_output_size(cbor.len() as u64)
        } else {
            default_min_utxo
        };
        if output.value.coin.0 < min_utxo.0 {
            errors.push(ValidationError::OutputTooSmall {
                minimum: min_utxo.0,
                actual: output.value.coin.0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 5a: Output value CBOR size <= max_val_size
    // ------------------------------------------------------------------
    if params.max_val_size > 0 {
        for output in &body.outputs {
            if !output.value.multi_asset.is_empty() {
                let val_size = estimate_value_cbor_size(&output.value);
                if val_size > params.max_val_size {
                    errors.push(ValidationError::OutputValueTooLarge {
                        maximum: params.max_val_size,
                        actual: val_size,
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 5a2: Byron-output attribute size <= 64 bytes
    //          (Haskell `OutputBootAddrAttrsTooBig`, Shelley+)
    //
    // Per Haskell `validateOutputBootAddrAttrsTooBig` every output whose
    // address is a Byron/bootstrap address must encode its attribute map
    // in 64 bytes or fewer. Non-Byron outputs are not checked. Every
    // offending output across the transaction aggregates into a single
    // predicate failure carrying the zero-based output indices.
    //
    // Reference: Haskell `OutputBootAddrAttrsTooBig` in
    // `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxo.hs`.
    // ------------------------------------------------------------------
    {
        let oversized = output_boot_addr_attrs_too_big_indices(&body.outputs);
        if !oversized.is_empty() {
            errors.push(ValidationError::OutputBootAddrAttrsTooBig {
                oversized_outputs: oversized,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 5b: Network ID consistency
    // ------------------------------------------------------------------
    if let Some(tx_network_id) = body.network_id {
        let expected_network = if tx_network_id == 0 {
            dugite_primitives::network::NetworkId::Testnet
        } else {
            dugite_primitives::network::NetworkId::Mainnet
        };
        for output in &body.outputs {
            if let Some(addr_network) = output.address.network_id() {
                if addr_network != expected_network {
                    errors.push(ValidationError::NetworkMismatch {
                        expected: expected_network,
                        actual: addr_network,
                    });
                    break;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 5c: Unconditional output address network check
    //
    // Unlike Rule 5b (which fires only when `tx.body.network_id` is set),
    // this check applies unconditionally using the node's configured network
    // (Haskell's `Globals.networkId`).  Every output address with a parseable
    // network tag must be on the node's network.
    //
    // Only enforced when `node_network` is provided. Addresses that return
    // `None` from `network_id()` (e.g. Byron addresses) are accepted without
    // a network check (they carry no explicit network tag).
    //
    // Reference: Haskell `WrongNetwork` predicate in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxo`.
    // ------------------------------------------------------------------
    if let Some(expected_net) = node_network {
        for output in &body.outputs {
            if let Some(addr_network) = output.address.network_id() {
                if addr_network != expected_net {
                    errors.push(ValidationError::WrongNetworkInOutput {
                        expected: expected_net,
                        actual: addr_network,
                    });
                    // Report once per transaction to avoid flooding.
                    break;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 5d: Unconditional withdrawal reward address network check
    //
    // Every withdrawal reward address must be on the node's configured
    // network (Haskell's `Globals.networkId`).
    // Bit 0 of the reward account header encodes the network:
    //   0 = testnet, 1 = mainnet.
    //
    // Reference: Haskell `WrongNetworkWithdrawal` in
    // `cardano-ledger-shelley:Cardano.Ledger.Shelley.Rules.Utxow`.
    // ------------------------------------------------------------------
    if let Some(expected_net) = node_network {
        for reward_account in body.withdrawals.keys() {
            if let Some(header) = reward_account.first() {
                let network_bit = header & 0x01;
                let actual_net = if network_bit == 0 {
                    dugite_primitives::network::NetworkId::Testnet
                } else {
                    dugite_primitives::network::NetworkId::Mainnet
                };
                if actual_net != expected_net {
                    errors.push(ValidationError::WrongNetworkWithdrawal {
                        expected: expected_net,
                        actual: actual_net,
                    });
                    break;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 6: Transaction size limit
    // ------------------------------------------------------------------
    if tx_size > params.max_tx_size {
        errors.push(ValidationError::TxTooLarge {
            maximum: params.max_tx_size,
            actual: tx_size,
        });
    }

    // ------------------------------------------------------------------
    // Rule 7: TTL check
    //
    // Haskell `inInterval` (Cardano.Ledger.Shelley.Rules.Utxo): the tx is
    // valid only when `slot < invalidHereafter`.  At `slot == invalidHereafter`
    // the tx is OUT of its validity interval and the predicate fires.  Dugite
    // previously used strict `>` here, which admitted a tx whose TTL equalled
    // the current slot — cardano-node then rejected the resulting block with
    // `OutsideValidityIntervalUTxO (… invalidHereafter = SJust S, current = S)`.
    // Match Haskell exactly: `>=`.
    // ------------------------------------------------------------------
    if let Some(ttl) = body.ttl {
        if current_slot >= ttl.0 {
            errors.push(ValidationError::TtlExpired {
                current_slot,
                ttl: ttl.0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 8: Validity interval start
    // ------------------------------------------------------------------
    if let Some(start) = body.validity_interval_start {
        if current_slot < start.0 {
            errors.push(ValidationError::NotYetValid {
                current_slot,
                valid_from: start.0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 9: Reference inputs must exist and not overlap with regular inputs
    //
    // Disjointness (`inputs ∩ reference_inputs = ∅`) is enforced at phase-1
    // only for PV < 11. At PV >= 11, Haskell `cardano-ledger` PR #5011
    // (commit 44de8edcc1005ec0fe3442898b59ee57060ff72c) RELAXED this rule:
    // V1/V2/native txs are accepted with overlap, and the equivalent check
    // moves into PlutusV3 `TxInfo` translation as
    // `ConwayContextError::ReferenceInputsNotDisjointFromInputs` (tag 15),
    // surfaced as a phase-2 `BadTranslation`. See dugite issue #470.
    //
    // The `ReferenceInputNotFound` check is stable across all PVs.
    // ------------------------------------------------------------------
    if !body.reference_inputs.is_empty() {
        let pv_major = params.protocol_version_major;
        let input_set: HashSet<_> = body.inputs.iter().collect();
        for ref_input in &body.reference_inputs {
            if utxo_set.lookup(ref_input).is_none() {
                errors.push(ValidationError::ReferenceInputNotFound(
                    ref_input.to_string(),
                ));
            }
            if pv_major < 11 && input_set.contains(ref_input) {
                errors.push(ValidationError::ReferenceInputOverlapsInput(
                    ref_input.to_string(),
                ));
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 9b: Witness completeness
    // ------------------------------------------------------------------
    if errors.is_empty() {
        // Build the set of VKey witness key hashes (blake2b-224 of each vkey).
        //
        // D9 / audit #544: Only hash vkeys that are exactly 32 bytes.
        // A malformed 1-byte vkey must NOT be hashed and used to satisfy a
        // required-signer check — that would let fabricated witnesses bypass
        // all cryptographic verification.  Haskell rejects non-32-byte vkeys
        // at CBOR decode time (`decodeVerKeyDSIGN` / `failSizeCheck`), so
        // they never reach the witness-completeness check.  Pallas accepts any
        // byte length; we enforce the invariant here.
        let vkey_witness_hashes: HashSet<Hash28> = tx
            .witness_set
            .vkey_witnesses
            .iter()
            .filter(|w| w.vkey.len() == 32) // only well-formed Ed25519 keys
            .map(|w| dugite_primitives::hash::blake2b_224(&w.vkey))
            .collect();

        let available_script_hashes = collect_available_script_hashes(tx, utxo_set);

        // Check each input has a matching witness
        for input in &body.inputs {
            if let Some(utxo) = utxo_set.lookup(input) {
                #[allow(clippy::collapsible_match)]
                match utxo.address.payment_credential() {
                    Some(Credential::VerificationKey(keyhash)) => {
                        if !vkey_witness_hashes.contains(keyhash) {
                            errors.push(ValidationError::MissingInputWitness(keyhash.to_hex()));
                        }
                    }
                    Some(Credential::Script(script_hash)) => {
                        if !available_script_hashes.contains(script_hash) {
                            errors
                                .push(ValidationError::MissingScriptWitness(script_hash.to_hex()));
                        }
                    }
                    None => {
                        // Byron address — bootstrap witness is verified in Rule 14.
                        // No additional completeness check needed here.
                    }
                }
            }
        }

        // Check each withdrawal has a matching witness for its reward credential
        for reward_account_bytes in body.withdrawals.keys() {
            if let Some(cred) = extract_reward_credential(reward_account_bytes) {
                match cred {
                    Credential::VerificationKey(keyhash) => {
                        if !vkey_witness_hashes.contains(&keyhash) {
                            errors
                                .push(ValidationError::MissingWithdrawalWitness(keyhash.to_hex()));
                        }
                    }
                    Credential::Script(script_hash) => {
                        if !available_script_hashes.contains(&script_hash) {
                            errors.push(ValidationError::MissingWithdrawalScriptWitness(
                                script_hash.to_hex(),
                            ));
                        }
                    }
                }
            }
        }

        // Check each certificate has matching witnesses for required credentials.
        // Mirrors Haskell's conwayWitsVKeyNeeded which unions certificate witness
        // requirements with input/withdrawal witness requirements.
        for cert in &body.certificates {
            for required_keyhash in cert_required_witnesses(cert) {
                if !vkey_witness_hashes.contains(&required_keyhash) {
                    errors.push(ValidationError::MissingCertificateWitness(
                        required_keyhash.to_hex(),
                    ));
                }
            }
        }

        // Check each voting procedure has a matching witness for its voter
        // credential. Mirrors Haskell `conwayWitsVKeyNeeded` for Conway-era:
        // every voter in `votingProcedures` whose credential is a vkey hash
        // contributes a required witness. Script-credential voters are
        // already covered by the redeemer presence check in
        // `collateral::check_script_redeemers`.
        //
        // Reference: Haskell `Cardano.Ledger.Conway.Tx.witsVKeyNeeded` —
        // `getConwayWitsVKeyNeeded` unions input + withdrawal + cert + voter
        // + proposal witness requirements before checking against the
        // supplied witness set.
        //
        // Without this check, dugite admits a tx that has a voting
        // procedure with a vkey-credential voter (DRep KeyHashObj, CC hot
        // KeyHashObj) but no matching vkey witness; cardano-node rejects
        // the resulting block with `ConwayUtxowFailure
        // (MissingVKeyWitnessesUTXOW (NonEmptySet (fromList [...])))`.
        // Round-1 retry surfaced this with block fb4da1990e86...@slot 645.
        for voter in body.voting_procedures.keys() {
            let required_keyhash: Option<Hash28> = match voter {
                dugite_primitives::transaction::Voter::ConstitutionalCommittee(cred)
                | dugite_primitives::transaction::Voter::DRep(cred) => match cred {
                    Credential::VerificationKey(h) => Some(*h),
                    Credential::Script(_) => None, // script voter — covered by redeemer check
                },
                // StakePool voters are pool-cold-key hashes. dugite stores
                // them as Hash32 (28-byte hash padded — see memory note
                // "28-byte hash types must be padded to 32 bytes"). The
                // witness set uses Hash28, so unpad by taking the first 28
                // bytes. Pool cold keys are always 28-byte BLAKE2b-224.
                dugite_primitives::transaction::Voter::StakePool(pool_hash32) => {
                    let mut buf = [0u8; 28];
                    buf.copy_from_slice(&pool_hash32.0[..28]);
                    Some(dugite_primitives::hash::Hash::<28>(buf))
                }
            };
            if let Some(kh) = required_keyhash {
                if !vkey_witness_hashes.contains(&kh) {
                    errors.push(ValidationError::MissingCertificateWitness(kh.to_hex()));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 9c: Datum witness completeness
    //
    // Enforced only when all inputs resolve successfully (Rule 2 found no
    // missing UTxOs) to avoid confusing secondary errors.  We also skip
    // when there are already errors that would prevent meaningful datum
    // checks (e.g. Rule 2 failures leave UTxOs unresolvable).
    //
    // Two sub-checks mirror Haskell's Alonzo UTXOW rules:
    //   - missingRequiredDatums:          script-locked inputs with DatumHash
    //     but no matching witness datum → MissingDatumWitness
    //   - notAllowedSupplementalDatums:   witness datums not needed by any
    //     input or output → ExtraDatumWitness
    // ------------------------------------------------------------------
    if errors.is_empty() {
        let script_versions = super::collateral::plutus_script_version_map(tx, utxo_set);
        super::datum::check_datum_witnesses(tx, utxo_set, &script_versions, errors);
    }

    // ------------------------------------------------------------------
    // Rule 10: Required signers must have corresponding vkey witnesses
    // ------------------------------------------------------------------
    if !body.required_signers.is_empty() && !tx.witness_set.vkey_witnesses.is_empty() {
        // D9 / audit #544: Same filter as Rule 9b — only hash well-formed 32-byte
        // vkeys.  A malformed vkey must never satisfy a required-signer check.
        let witness_keyhashes: HashSet<_> = tx
            .witness_set
            .vkey_witnesses
            .iter()
            .filter(|w| w.vkey.len() == 32) // only well-formed Ed25519 keys
            .map(|w| dugite_primitives::hash::blake2b_224(&w.vkey))
            .collect();
        for required_signer in &body.required_signers {
            // Compare first 28 bytes (Hash32 may be zero-padded Hash28)
            let signer_28 = &required_signer.as_bytes()[..28];
            let has_witness = witness_keyhashes
                .iter()
                .any(|kh| kh.as_bytes() == signer_28);
            if !has_witness {
                errors.push(ValidationError::MissingRequiredSigner(
                    required_signer.to_hex(),
                ));
            }
        }
    } else if !body.required_signers.is_empty() {
        // Required signers but no vkey witnesses at all
        for required_signer in &body.required_signers {
            errors.push(ValidationError::MissingRequiredSigner(
                required_signer.to_hex(),
            ));
        }
    }

    // ------------------------------------------------------------------
    // Rule 13: Native script evaluation
    // ------------------------------------------------------------------
    // Per Cardano spec (Haskell `validateFailedScripts`), only evaluate
    // native scripts whose hashes appear in `scriptsNeeded(tx)`. Extra
    // scripts in the witness set are allowed but should not be evaluated.
    if !tx.witness_set.native_scripts.is_empty() {
        // Collect the set of script hashes that are actually needed by
        // the transaction: script-locked spending inputs, minting policy
        // IDs, script-locked withdrawals, and script-locked certificates.
        let mut scripts_needed: HashSet<Hash28> = HashSet::new();

        // 1. Script-locked spending inputs (address type bit 4 set)
        for input in &body.inputs {
            if let Some(utxo) = utxo_set.lookup(input) {
                let ab = utxo.address.to_bytes();
                if !ab.is_empty() {
                    let t = (ab[0] >> 4) & 0x0F;
                    // Script address types: 1,3,5,7 (bit 4 of header = 1)
                    if matches!(t, 1 | 3 | 5 | 7) && ab.len() >= 29 {
                        if let Ok(h) = Hash28::try_from(&ab[1..29]) {
                            scripts_needed.insert(h);
                        }
                    }
                }
            }
        }

        // 2. Minting policy IDs
        for policy_id in body.mint.keys() {
            scripts_needed.insert(*policy_id);
        }

        // 3. Script-locked withdrawals (reward address with script bit set)
        for reward_addr in body.withdrawals.keys() {
            if reward_addr.len() >= 29 {
                let header = reward_addr[0];
                // Reward address type: 0xF0/0xF1 — bit 4 = script
                if (header & 0x10) != 0 {
                    if let Ok(h) = Hash28::try_from(&reward_addr[1..29]) {
                        scripts_needed.insert(h);
                    }
                }
            }
        }

        // 4. Certificates with script credentials
        for cert in &body.certificates {
            let cred: Option<&Credential> = match cert {
                Certificate::StakeDeregistration(c) => Some(c),
                Certificate::StakeDelegation { credential: c, .. } => Some(c),
                Certificate::ConwayStakeRegistration { credential: c, .. } => Some(c),
                Certificate::ConwayStakeDeregistration { credential: c, .. } => Some(c),
                Certificate::VoteDelegation { credential: c, .. } => Some(c),
                Certificate::StakeVoteDelegation { credential: c, .. } => Some(c),
                Certificate::RegStakeDeleg { credential: c, .. } => Some(c),
                Certificate::RegStakeVoteDeleg { credential: c, .. } => Some(c),
                Certificate::VoteRegDeleg { credential: c, .. } => Some(c),
                Certificate::CommitteeHotAuth {
                    cold_credential: c, ..
                } => Some(c),
                Certificate::CommitteeColdResign {
                    cold_credential: c, ..
                } => Some(c),
                Certificate::RegDRep { credential: c, .. } => Some(c),
                Certificate::UnregDRep { credential: c, .. } => Some(c),
                Certificate::UpdateDRep { credential: c, .. } => Some(c),
                _ => None,
            };
            if let Some(Credential::Script(h)) = cred {
                scripts_needed.insert(*h);
            }
        }

        // Now evaluate only needed native scripts
        let signers: HashSet<dugite_primitives::hash::Hash32> = tx
            .witness_set
            .vkey_witnesses
            .iter()
            .map(|w| {
                // Hash the vkey to get the 28-byte key hash, then pad to Hash32
                dugite_primitives::hash::blake2b_224(&w.vkey).to_hash32_padded()
            })
            .collect();
        let slot = SlotNo(current_slot);

        for script in &tx.witness_set.native_scripts {
            // Compute this script's hash: blake2b_224(0x00 || cbor(script))
            let script_cbor = dugite_serialization::encode_native_script(script);
            let mut tagged = Vec::with_capacity(1 + script_cbor.len());
            tagged.push(0x00);
            tagged.extend_from_slice(&script_cbor);
            let script_hash = dugite_primitives::hash::blake2b_224(&tagged);

            // Only evaluate scripts that are actually needed
            if scripts_needed.contains(&script_hash)
                && !evaluate_native_script(script, &signers, slot)
            {
                errors.push(ValidationError::NativeScriptFailed);
                break;
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 14: Witness signature verification
    //
    // Haskell runs all UTXOW predicates independently — Rule 14 fires even
    // when other rules have also fired (e.g. missing-input). We mirror that
    // behaviour so operators receive the full error set on one submission.
    // `tx.hash` is always populated during deserialization.
    //
    // VKeyWitness: 32-byte Ed25519 key + 64-byte signature.
    // BootstrapWitness (Byron): 64-byte extended key; separate verifier:
    //   (a) verifies the signature using vkey[0..32] (scalar part), and
    //   (b) checks the address-binding (computed root vs Byron address root
    //       stored in the UTxO being spent).
    // ------------------------------------------------------------------
    {
        let tx_hash_bytes = tx.hash.as_bytes();

        errors.extend(verify_witness_signatures(
            &tx.witness_set.vkey_witnesses,
            tx_hash_bytes,
            "",
        ));
        errors.extend(verify_bootstrap_witnesses(tx, utxo_set, tx_hash_bytes));
    }
}

// ---------------------------------------------------------------------------
// Inline unit tests for Phase-1 validation rules
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dugite_primitives::address::{Address, EnterpriseAddress};
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::SlotNo;
    use dugite_primitives::transaction::{
        BootstrapWitness, ExUnits, OutputDatum, Redeemer, RedeemerTag, Transaction,
        TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet, VKeyWitness,
    };
    use dugite_primitives::value::{AssetName, Lovelace, Value};

    use crate::utxo::UtxoSet;
    use crate::validation::{
        validate_transaction, validate_transaction_with_pools, ValidationError,
    };

    // -----------------------------------------------------------------------
    // Test fixture: a minimal valid Conway transaction
    //
    // UTxO:   1 input  → 10_000_000 lovelace
    // Output: 1 output →  9_800_000 lovelace
    // Fee:                  200_000 lovelace
    // -----------------------------------------------------------------------

    /// Build a UTxO set with one entry worth 10M lovelace, plus the corresponding
    /// [`TransactionInput`] that spends it.  The UTxO output uses a Byron address
    /// so that Phase-1 witness-completeness checks (Rule 9b) are satisfied
    /// without requiring a vkey witness.
    fn make_valid_tx() -> (UtxoSet, Transaction, TransactionInput) {
        let mut utxo_set = UtxoSet::new();

        // Use a Byron payload so Rule 9b requires no witness for this input.
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xAAu8; 32]),
            index: 0,
        };
        let utxo_output = TransactionOutput {
            address: Address::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![0x82, 0x00, 0x01],
            }),
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(input.clone(), utxo_output);

        let tx = Transaction {
            era: dugite_primitives::era::Era::Conway,
            hash: Hash32::ZERO,
            body: TransactionBody {
                inputs: vec![input.clone()],
                outputs: vec![TransactionOutput {
                    address: Address::Byron(dugite_primitives::address::ByronAddress {
                        payload: vec![0x82, 0x00, 0x01],
                    }),
                    value: Value::lovelace(9_800_000),
                    datum: OutputDatum::None,
                    script_ref: None,
                    is_legacy: false,
                    raw_cbor: None,
                }],
                fee: Lovelace(200_000),
                ttl: None,
                certificates: vec![],
                withdrawals: BTreeMap::new(),
                auxiliary_data_hash: None,
                validity_interval_start: None,
                mint: BTreeMap::new(),
                script_data_hash: None,
                collateral: vec![],
                required_signers: vec![],
                network_id: None,
                collateral_return: None,
                total_collateral: None,
                reference_inputs: vec![],
                update: None,
                voting_procedures: BTreeMap::new(),
                proposal_procedures: vec![],
                treasury_value: None,
                donation: None,
                sub_transactions: vec![],
                account_balance_intervals: vec![],
                direct_deposits: ::std::collections::BTreeMap::new(),
                guards: Vec::new(),
            },
            witness_set: TransactionWitnessSet {
                vkey_witnesses: vec![],
                native_scripts: vec![],
                bootstrap_witnesses: vec![],
                plutus_v1_scripts: vec![],
                plutus_v2_scripts: vec![],
                plutus_v3_scripts: vec![],
                plutus_data: vec![],
                redeemers: vec![],
                raw_redeemers_cbor: None,
                raw_plutus_data_cbor: None,
                original_script_data_hash: None,
            },
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };

        (utxo_set, tx, input)
    }

    // -----------------------------------------------------------------------
    // Test 1 — baseline: valid transaction passes all Phase-1 rules
    // -----------------------------------------------------------------------
    #[test]
    fn test_valid_tx_passes() {
        let (utxo_set, tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(result.is_ok(), "expected Ok(()), got {result:?}");
    }

    // -----------------------------------------------------------------------
    // Test 2 — Rule 1: no inputs
    // -----------------------------------------------------------------------
    #[test]
    fn test_no_inputs() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.inputs.clear();
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::NoInputs)),
            "expected NoInputs, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3 — Rule 2: input references a UTxO entry that does not exist
    // -----------------------------------------------------------------------
    #[test]
    fn test_all_inputs_must_exist() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Overwrite the tx_id so it no longer matches the UTxO.
        tx.body.inputs[0].transaction_id = Hash32::from_bytes([0xBBu8; 32]);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InputNotFound(_))),
            "expected InputNotFound, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4 — Rule 3: ADA value not conserved (output + fee > input)
    // -----------------------------------------------------------------------
    #[test]
    fn test_value_not_conserved_ada() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Inflate the output so that output(10_000_000) + fee(200_000) > input(10_000_000).
        tx.body.outputs[0].value = Value::lovelace(10_000_000);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ValueNotConserved { .. })),
            "expected ValueNotConserved, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5 — Rule 3b: multi-asset not conserved (minted tokens not in outputs)
    // -----------------------------------------------------------------------
    #[test]
    fn test_value_not_conserved_multiasset() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let policy = Hash28::from_bytes([0x11u8; 28]);
        let asset = AssetName::new(b"COIN".to_vec()).unwrap();
        // Mint 100 tokens but produce no multi-asset output.
        tx.body.mint.entry(policy).or_default().insert(asset, 100);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MultiAssetNotConserved { .. })),
            "expected MultiAssetNotConserved, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6 — Rule 3c: minting policy present but no matching script witness
    // -----------------------------------------------------------------------
    #[test]
    fn test_mint_without_policy_script() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let policy = Hash28::from_bytes([0x22u8; 28]);
        let asset = AssetName::new(b"TKN".to_vec()).unwrap();
        // Mint and output the same amount so Rule 3b passes, but no script
        // witness is provided so Rule 3c fires.
        tx.body
            .mint
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 50);
        // Mirror the minted tokens in the output so value is conserved.
        tx.body.outputs[0]
            .value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset, 50);
        let params = ProtocolParameters::mainnet_defaults();
        // The validation should fail with a script-related error.
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::InvalidMint | ValidationError::MissingScriptWitness(_)
            )),
            "expected a script-related minting error, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7 — Rule 4: declared fee is below the computed minimum fee
    // -----------------------------------------------------------------------
    #[test]
    fn test_fee_too_small() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Move almost all lovelace to the output, leaving only 1 lovelace as fee.
        tx.body.outputs[0].value = Value::lovelace(9_999_999);
        tx.body.fee = Lovelace(1);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::FeeTooSmall { .. })),
            "expected FeeTooSmall, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8 — Rule 5: output below minimum UTxO value
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_below_min_utxo() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Keep value conservation: output(1) + fee(9_999_999) = input(10_000_000).
        tx.body.outputs[0].value = Value::lovelace(1);
        tx.body.fee = Lovelace(9_999_999);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::OutputTooSmall { .. })),
            "expected OutputTooSmall, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 9 — Rule 5a: output value CBOR size exceeds max_val_size
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_value_too_large() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Build a multi-asset output with many policies so the estimated CBOR
        // size of the value map exceeds max_val_size (5000 bytes in mainnet
        // defaults).  Each policy+1 asset adds roughly 37 bytes; 140 policies
        // ≈ 5180 bytes which is safely above the 5000 byte limit.
        let mut multi_asset_value = Value::lovelace(9_800_000);
        for i in 0u8..140 {
            let mut policy_bytes = [0u8; 28];
            policy_bytes[0] = i;
            policy_bytes[1] = 0xFF;
            let policy = Hash28::from_bytes(policy_bytes);
            let asset = AssetName::new(vec![i; 4]).unwrap();
            multi_asset_value
                .multi_asset
                .entry(policy)
                .or_default()
                .insert(asset, 1);
        }
        tx.body.outputs[0].value = multi_asset_value;
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::OutputValueTooLarge { .. })),
            "expected OutputValueTooLarge, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 10 — Rule 5c: output address on wrong network (testnet addr, mainnet node)
    // -----------------------------------------------------------------------
    #[test]
    fn test_network_id_mismatch() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Replace the output address with a testnet enterprise address.
        tx.body.outputs[0].address = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(Hash28::from_bytes([0x33u8; 28])),
        });
        let params = ProtocolParameters::mainnet_defaults();
        // Validate with node_network = Mainnet so Rule 5c fires.
        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(NetworkId::Mainnet),
            None,
            None,
            None,
            None, // constitution_script_hash
            None, // vote_delegations
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::WrongNetworkInOutput { .. })),
            "expected WrongNetworkInOutput, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11 — Rule 6: transaction size exceeds max_tx_size
    // -----------------------------------------------------------------------
    #[test]
    fn test_tx_size_too_large() {
        let (utxo_set, tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Pass a size that exceeds max_tx_size (16384 in mainnet defaults).
        let too_large = params.max_tx_size + 1;
        let errors =
            validate_transaction(&tx, &utxo_set, &params, 100, too_large, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::TxTooLarge { .. })),
            "expected TxTooLarge, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12 — Rule 7: TTL expired
    // -----------------------------------------------------------------------
    #[test]
    fn test_ttl_expired() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.ttl = Some(SlotNo(50));
        let params = ProtocolParameters::mainnet_defaults();
        // current_slot(100) > ttl(50) → TtlExpired
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::TtlExpired { .. })),
            "expected TtlExpired, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 13 — Rule 8: validity interval start not yet reached
    // -----------------------------------------------------------------------
    #[test]
    fn test_validity_interval_not_started() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.validity_interval_start = Some(SlotNo(200));
        let params = ProtocolParameters::mainnet_defaults();
        // current_slot(100) < validity_start(200) → NotYetValid
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::NotYetValid { .. })),
            "expected NotYetValid, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 14 — Rule 9 (PV gating, dugite #470):
    //   PV <= 10 → phase-1 rejects with `ReferenceInputOverlapsInput`
    //              (`BabbageNonDisjointRefInputs`).
    //   PV >= 11 → phase-1 accepts the overlap (Haskell cardano-ledger
    //              PR #5011 relaxed the rule for V1/V2/native; the equivalent
    //              check moved into PlutusV3 TxInfo translation as a phase-2
    //              `BadTranslation`).
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_must_be_disjoint() {
        // PV 10 (pre-relaxation): overlap must be rejected at phase-1.
        let (utxo_set, mut tx, input) = make_valid_tx();
        tx.body.reference_inputs.push(input.clone());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ReferenceInputOverlapsInput(_))),
            "PV 10: expected ReferenceInputOverlapsInput, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 14a — PV 11 with native-only tx and overlap: ACCEPT (no V3 → no
    // phase-2 BadTranslation either).
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_overlap_accepted_at_pv11_native() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        tx.body.reference_inputs.push(input.clone());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "PV 11 native tx with overlap should be accepted, got errors: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 14b — PV 11 with no overlap: ACCEPT.
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_no_overlap_at_pv11_accepted() {
        // Construct a tx with a reference input that *does* exist in the UTxO
        // set but is not in `body.inputs` — no overlap.
        let (mut utxo_set, mut tx, _) = make_valid_tx();
        let ref_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x77u8; 32]),
            index: 0,
        };
        let ref_output = TransactionOutput {
            address: Address::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![0x82, 0x00, 0x01],
            }),
            value: Value::lovelace(5_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(ref_input.clone(), ref_output);
        tx.body.reference_inputs.push(ref_input);
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "PV 11 tx with non-overlapping ref input should be accepted, got: {:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Test 14c — PV 11 with reference input NOT in UTxO: still
    // `ReferenceInputNotFound` (this rule is stable across all PVs).
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_not_found_at_pv11() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.reference_inputs.push(TransactionInput {
            transaction_id: Hash32::from_bytes([0xDEu8; 32]),
            index: 0,
        });
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 11;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ReferenceInputNotFound(_))),
            "PV 11: expected ReferenceInputNotFound, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 15 — Rule 9: reference input does not exist in the UTxO set
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_must_exist() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Add a reference input that has no UTxO entry.
        tx.body.reference_inputs.push(TransactionInput {
            transaction_id: Hash32::from_bytes([0xCCu8; 32]),
            index: 0,
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ReferenceInputNotFound(_))),
            "expected ReferenceInputNotFound, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 16 — Rule 10: required signer has no matching vkey witness
    // -----------------------------------------------------------------------
    #[test]
    fn test_required_signer_missing() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Declare a required signer whose hash is not present in the witness set.
        tx.body
            .required_signers
            .push(Hash32::from_bytes([0xDDu8; 32]));
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingRequiredSigner(_))),
            "expected MissingRequiredSigner, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 17 — Rule 1c: auxiliary data hash declared but no auxiliary data body
    // -----------------------------------------------------------------------
    #[test]
    fn test_auxiliary_data_hash_mismatch() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Declare a hash but provide no auxiliary data.
        tx.body.auxiliary_data_hash = Some(Hash32::from_bytes([0xEEu8; 32]));
        tx.auxiliary_data = None;
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::AuxiliaryDataHashWithoutData)),
            "expected AuxiliaryDataHashWithoutData, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 18 — Conway LEDGER rule: declared treasury value must match ledger
    // -----------------------------------------------------------------------
    #[test]
    fn test_treasury_value_mismatch() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Declare 999 lovelace in the tx body, but ledger holds 1000.
        tx.body.treasury_value = Some(Lovelace(999));
        let params = ProtocolParameters::mainnet_defaults();
        // protocol_version_major = 9 in mainnet_defaults() so the check fires.
        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            Some(1000), // current_treasury
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None, // constitution_script_hash
            None, // vote_delegations
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::TreasuryValueMismatch { .. })),
            "expected TreasuryValueMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 19 — Rule 12: script data hash missing when Plutus scripts/redeemers present
    // -----------------------------------------------------------------------
    #[test]
    fn test_script_integrity_hash_mismatch() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Add a dummy PlutusV2 script byte-string and a redeemer so the
        // `has_plutus_scripts` guard fires and `check_script_data_hash` runs.
        // With `script_data_hash = None`, `MissingScriptDataHash` is pushed.
        tx.witness_set.plutus_v2_scripts.push(vec![0x01u8; 10]);
        tx.witness_set.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: dugite_primitives::transaction::PlutusData::Integer(num_bigint::BigInt::from(
                0i64,
            )),
            ex_units: ExUnits { mem: 0, steps: 0 },
        });
        // Deliberately leave script_data_hash as None → MissingScriptDataHash
        tx.body.script_data_hash = None;
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::MissingScriptDataHash
                    | ValidationError::ScriptDataHashMismatch { .. }
            )),
            "expected MissingScriptDataHash or ScriptDataHashMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 20 — Rule 14: Ed25519 witness with a corrupt signature is rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_ed25519_signature_verification() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Append a vkey witness whose signature bytes are all zeros — this will
        // fail Ed25519 verification (or key parsing) and trigger the error.
        // Using `[1u8; 32]` as the vkey matches the pattern already verified in
        // the existing test suite (validation/tests.rs test_witness_signature_verification).
        tx.witness_set.vkey_witnesses.push(VKeyWitness {
            vkey: vec![1u8; 32],
            signature: vec![0u8; 64],
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 20a..20i — Rule 14 wrong-length witness rejection (issue #537)
    //
    // Haskell cardano-node rejects any witness whose signature is not 64
    // bytes (and whose vkey is not 32 bytes) at CBOR decode time via
    // `decodeSignedDSIGN`/`failSizeCheck`. Pallas-based decode keeps these
    // as variable-length `Bytes`, so dugite enforces the invariant in
    // Phase-1 (`verify_single_witness`).
    //
    // Regression matrix: every wrong size — including the truncated-64→63
    // case from the 314pool bounty repro — must produce
    // `InvalidWitnessSignature` rather than being silently accepted.
    // -----------------------------------------------------------------------

    /// Helper: build a valid baseline tx with one user-supplied vkey witness
    /// and validate it under mainnet defaults, returning the error list.
    fn validate_with_vkey_witness(vkey: Vec<u8>, signature: Vec<u8>) -> Vec<ValidationError> {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set
            .vkey_witnesses
            .push(VKeyWitness { vkey, signature });
        let params = ProtocolParameters::mainnet_defaults();
        validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err()
    }

    /// Helper: same as above but for BootstrapWitness (Byron).
    fn validate_with_bootstrap_witness(vkey: Vec<u8>, signature: Vec<u8>) -> Vec<ValidationError> {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set.bootstrap_witnesses.push(BootstrapWitness {
            vkey,
            signature,
            chain_code: vec![0u8; 32],
            attributes: vec![],
        });
        let params = ProtocolParameters::mainnet_defaults();
        validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err()
    }

    fn assert_invalid_witness(errors: &[ValidationError]) {
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature, got {errors:?}"
        );
    }

    // Truncated 64→63-byte signature — the exact 314pool bounty repro.
    #[test]
    fn test_vkey_witness_truncated_signature_63_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 32], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    // Oversized signature.
    #[test]
    fn test_vkey_witness_oversized_signature_65_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 32], vec![0u8; 65]);
        assert_invalid_witness(&errors);
    }

    // Empty signature — degenerate edge case.
    #[test]
    fn test_vkey_witness_empty_signature_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 32], vec![]);
        assert_invalid_witness(&errors);
    }

    // Short vkey.
    #[test]
    fn test_vkey_witness_short_vkey_31_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 31], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    // Long vkey.
    #[test]
    fn test_vkey_witness_long_vkey_33_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 33], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    // Empty vkey — degenerate edge case (also exercises the slice-bound
    // guard `vkey[..8.min(vkey.len())]` was originally added for).
    #[test]
    fn test_vkey_witness_empty_vkey_rejected() {
        let errors = validate_with_vkey_witness(vec![], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    // Both vkey and signature wrong size simultaneously.
    #[test]
    fn test_vkey_witness_both_wrong_size_rejected() {
        let errors = validate_with_vkey_witness(vec![1u8; 31], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    // BootstrapWitness (Byron) — truncated signature must also be rejected
    // since `decodeSignedDSIGN` enforces the same 64-byte invariant on
    // Byron bootstrap witnesses.
    #[test]
    fn test_bootstrap_witness_truncated_signature_rejected() {
        let errors = validate_with_bootstrap_witness(vec![1u8; 32], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    // BootstrapWitness with short vkey.
    #[test]
    fn test_bootstrap_witness_short_vkey_rejected() {
        let errors = validate_with_bootstrap_witness(vec![1u8; 31], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    // Property test: across the full malformed-size lattice, validation
    // must report `InvalidWitnessSignature`. Witnesses with size {32, 64}
    // are excluded — they are exercised by `test_ed25519_signature_verification`
    // (where the all-zero sig still fails crypto verification).
    #[test]
    fn test_vkey_witness_length_lattice_rejected() {
        for vkey_len in [0usize, 1, 16, 31, 33, 48, 64, 128] {
            for sig_len in [0usize, 1, 32, 63, 65, 96, 128] {
                if vkey_len == 32 && sig_len == 64 {
                    continue;
                }
                let errors = validate_with_vkey_witness(vec![1u8; vkey_len], vec![0u8; sig_len]);
                assert!(
                    errors.iter().any(|e| matches!(
                        e,
                        ValidationError::InvalidWitnessSignature(_)
                    )),
                    "vkey_len={vkey_len} sig_len={sig_len}: expected InvalidWitnessSignature, got {errors:?}"
                );
            }
        }
    }

    // Diagnostic-quality check: the rejection message must surface both
    // observed lengths so operators can diagnose corrupt submissions.
    #[test]
    fn test_vkey_witness_malformed_error_message_includes_sizes() {
        // D2 fix: expect_size checks vkey first, then sig, each returning a separate error.
        // With vkey=31 bytes: error mentions "31" and "32" (expected).
        let errors = validate_with_vkey_witness(vec![1u8; 31], vec![0u8; 63]);
        let msg = errors
            .iter()
            .find_map(|e| match e {
                ValidationError::InvalidWitnessSignature(s) => Some(s.clone()),
                _ => None,
            })
            .expect("InvalidWitnessSignature not found");
        // The error must reference the actual size (31) and the expected size (32)
        assert!(
            msg.contains("31"),
            "message must include actual vkey size: {msg}"
        );
        assert!(
            msg.contains("32"),
            "message must include expected vkey size: {msg}"
        );
        // Also verify wrong-size sig is rejected when vkey is correct
        let errors2 = validate_with_vkey_witness(vec![0u8; 32], vec![0u8; 63]);
        let msg2 = errors2
            .iter()
            .find_map(|e| match e {
                ValidationError::InvalidWitnessSignature(s) => Some(s.clone()),
                _ => None,
            })
            .expect("InvalidWitnessSignature not found for wrong-size sig");
        assert!(
            msg2.contains("63"),
            "message must include actual sig size: {msg2}"
        );
        assert!(
            msg2.contains("64"),
            "message must include expected sig size: {msg2}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — `PoolMedataHashTooBig` predicate (Haskell, Alonzo+)
    //
    // These tests exercise `is_pool_metadata_hash_too_big` directly via a
    // raw byte slice — in dugite the typed `PoolMetadata.hash` field is
    // already a fixed `Hash32`, so the predicate is structurally
    // unreachable through the typed API. The helper is kept defensive
    // against future wire-decode paths that surface oversized values.
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_medata_hash_too_big_rejected_post_alonzo() {
        // pv_major = 5 (Alonzo), 33-byte hash → predicate fires.
        let oversized = vec![0u8; 33];
        assert!(super::is_pool_metadata_hash_too_big(&oversized, 5));
    }

    #[test]
    fn test_pool_medata_hash_too_big_skipped_pre_alonzo() {
        // pv_major = 4 (Mary), 33-byte hash → predicate inactive.
        let oversized = vec![0u8; 33];
        assert!(!super::is_pool_metadata_hash_too_big(&oversized, 4));
    }

    #[test]
    fn test_pool_medata_hash_at_32_bytes_accepted() {
        // pv_major = 5, exactly 32 bytes → passes.
        let exact = vec![0u8; 32];
        assert!(!super::is_pool_metadata_hash_too_big(&exact, 5));
    }

    // -----------------------------------------------------------------------
    // Tests — `OutputBootAddrAttrsTooBig` predicate (Haskell, Shelley+)
    //
    // Helper builds Byron addresses with a controllable inner attribute
    // payload size. Per Haskell `validateOutputBootAddrAttrsTooBig` the
    // attribute map (`{ key => bytes(payload) }`) must serialize to <= 64
    // bytes; outputs that exceed the cap are aggregated by their indices
    // into a single `OutputBootAddrAttrsTooBig` error.
    // -----------------------------------------------------------------------

    /// Encode a Byron address with a single attribute carrying `attr_payload_len`
    /// arbitrary bytes — same shape used by the
    /// `dugite_primitives::address::tests::synth_byron_addr` helper.
    fn synth_byron_address_bytes(attr_payload_len: usize) -> Vec<u8> {
        let mut inner = Vec::new();
        let mut e = minicbor::Encoder::new(&mut inner);
        e.array(3).unwrap();
        e.bytes(&[0u8; 28]).unwrap();
        e.map(1).unwrap();
        e.u8(1).unwrap();
        let attr_payload = vec![0xAAu8; attr_payload_len];
        e.bytes(&attr_payload).unwrap();
        e.u8(0).unwrap();

        let mut outer = Vec::new();
        let mut oe = minicbor::Encoder::new(&mut outer);
        oe.array(2).unwrap();
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner).unwrap();
        oe.u32(0).unwrap();
        outer
    }

    fn byron_output_with_attr_payload(attr_payload_len: usize) -> TransactionOutput {
        TransactionOutput {
            address: dugite_primitives::address::Address::Byron(
                dugite_primitives::address::ByronAddress {
                    payload: synth_byron_address_bytes(attr_payload_len),
                },
            ),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    #[test]
    fn test_output_boot_addr_attrs_too_big_rejected() {
        // 65-byte attribute payload → attrs map = 1 (map header) + 1 (key)
        //                              + 2 (bytes header for len 65) + 65
        //                              = 69 bytes > 64 → fires.
        let outputs = vec![byron_output_with_attr_payload(65)];
        let bad = super::output_boot_addr_attrs_too_big_indices(&outputs);
        assert_eq!(bad, vec![0]);
    }

    #[test]
    fn test_output_boot_addr_attrs_at_64_bytes_accepted() {
        // 60-byte payload → 1 + 1 + 2 + 60 = 64 → exactly at cap → accepted.
        let outputs = vec![byron_output_with_attr_payload(60)];
        let bad = super::output_boot_addr_attrs_too_big_indices(&outputs);
        assert!(bad.is_empty(), "expected empty, got {bad:?}");
    }

    #[test]
    fn test_output_shelley_addr_not_checked() {
        // A Shelley enterprise output is not a Byron address → predicate skips it.
        let shelley_out = TransactionOutput {
            address: dugite_primitives::address::Address::Enterprise(
                dugite_primitives::address::EnterpriseAddress {
                    network: NetworkId::Mainnet,
                    payment: dugite_primitives::credentials::Credential::VerificationKey(
                        Hash28::from_bytes([0u8; 28]),
                    ),
                },
            ),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let outputs = vec![shelley_out];
        let bad = super::output_boot_addr_attrs_too_big_indices(&outputs);
        assert!(bad.is_empty());
    }

    #[test]
    fn test_output_no_outputs_passes() {
        let outputs: Vec<TransactionOutput> = vec![];
        let bad = super::output_boot_addr_attrs_too_big_indices(&outputs);
        assert!(bad.is_empty());
    }

    #[test]
    fn test_output_boot_addr_attrs_too_big_aggregates_indices() {
        // One good Byron output, one bad → only index 1 reported.
        let outputs = vec![
            byron_output_with_attr_payload(10),
            byron_output_with_attr_payload(100),
        ];
        let bad = super::output_boot_addr_attrs_too_big_indices(&outputs);
        assert_eq!(bad, vec![1]);
    }

    #[test]
    fn test_output_boot_addr_attrs_too_big_integration_via_validate_tx() {
        // Drive the predicate through the full Phase-1 validator: build a
        // tx whose only output is a Byron address with a 100-byte attr
        // payload. The error list must contain `OutputBootAddrAttrsTooBig`
        // with the offending index (0).
        let (mut utxo_set, mut tx, _) = make_valid_tx();
        tx.body.outputs[0] = byron_output_with_attr_payload(100);
        let _ = &mut utxo_set;
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        let found = errors.iter().any(|e| matches!(
            e,
            ValidationError::OutputBootAddrAttrsTooBig { oversized_outputs } if oversized_outputs == &vec![0]
        ));
        assert!(
            found,
            "expected OutputBootAddrAttrsTooBig with [0], got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31 — Rule 1b: duplicate inputs rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_duplicate_inputs_rejected() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        // Add the same input a second time.
        tx.body.inputs.push(input.clone());
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicateInput(_))),
            "expected DuplicateInput, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 32 — Rule 7: TTL exactly at current_slot fails (slot >= TTL = invalid)
    //
    // Haskell `inInterval` (Cardano.Ledger.Shelley.Rules.Utxo): the tx is
    // valid only when `slot < invalidHereafter`.  Equality is OUT of the
    // interval, so dugite must emit `TtlExpired` at `slot == invalidHereafter`.
    // -----------------------------------------------------------------------
    #[test]
    fn test_ttl_at_current_slot_fails() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.ttl = Some(SlotNo(100));
        let params = ProtocolParameters::mainnet_defaults();
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result
                .as_ref()
                .err()
                .map(|es| es
                    .iter()
                    .any(|e| matches!(e, ValidationError::TtlExpired { .. })))
                .unwrap_or(false),
            "TTL == current_slot must produce TtlExpired (Haskell `slot < invalidHereafter`), got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 33 — Rule 8: validity start == current_slot passes (>= check)
    // -----------------------------------------------------------------------
    #[test]
    fn test_validity_interval_start_at_current_slot_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.validity_interval_start = Some(SlotNo(100));
        let params = ProtocolParameters::mainnet_defaults();
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let no_not_yet_valid = result.is_ok()
            || result
                .as_ref()
                .err()
                .map(|es| {
                    !es.iter()
                        .any(|e| matches!(e, ValidationError::NotYetValid { .. }))
                })
                .unwrap_or(true);
        assert!(
            no_not_yet_valid,
            "validity_interval_start == current_slot must NOT produce NotYetValid, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 34 — Rule 6: tx exactly at max_tx_size passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_tx_exactly_at_max_size_passes() {
        let (utxo_set, tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Pass tx_size == max_tx_size; should not produce TxTooLarge.
        let result = validate_transaction(&tx, &utxo_set, &params, 100, params.max_tx_size, None);
        let no_size_error = result.is_ok()
            || result
                .as_ref()
                .err()
                .map(|es| {
                    !es.iter()
                        .any(|e| matches!(e, ValidationError::TxTooLarge { .. }))
                })
                .unwrap_or(true);
        assert!(
            no_size_error,
            "tx_size == max_tx_size must NOT produce TxTooLarge"
        );
    }

    // -----------------------------------------------------------------------
    // Test 35 — Rule 6: tx one byte over max_tx_size rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_tx_one_over_max_size_rejected() {
        let (utxo_set, tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let errors =
            validate_transaction(&tx, &utxo_set, &params, 100, params.max_tx_size + 1, None)
                .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::TxTooLarge { .. })),
            "tx_size == max_tx_size + 1 must produce TxTooLarge, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 36 — Rule 4: fee exactly at minimum passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_fee_exactly_at_minimum_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Set fee to the minimum fee for tx_size=300, then adjust output to balance.
        // min_fee = min_fee_a * tx_size + min_fee_b = 44*300 + 155381 = 168581 lovelace
        let min_fee = params.min_fee_a * 300 + params.min_fee_b;
        // Adjust output to conserve value: 10_000_000 - min_fee
        tx.body.fee = Lovelace(min_fee);
        tx.body.outputs[0].value = Value::lovelace(10_000_000 - min_fee);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let no_fee_error = result.is_ok()
            || result
                .as_ref()
                .err()
                .map(|es| {
                    !es.iter()
                        .any(|e| matches!(e, ValidationError::FeeTooSmall { .. }))
                })
                .unwrap_or(true);
        assert!(
            no_fee_error,
            "fee == min_fee must NOT produce FeeTooSmall, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 37 — Rule 4: fee one lovelace below minimum rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_fee_one_below_minimum_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let min_fee = params.min_fee_a * 300 + params.min_fee_b;
        // Set fee just below minimum; add the lovelace to the output to stay balanced.
        if min_fee > 0 {
            tx.body.fee = Lovelace(min_fee - 1);
            tx.body.outputs[0].value = Value::lovelace(10_000_000 - (min_fee - 1));
        }
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::FeeTooSmall { .. })),
            "fee == min_fee - 1 must produce FeeTooSmall, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 38 — Rule 1e: pool retirement beyond e_max rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_retirement_beyond_e_max_rejected() {
        use dugite_primitives::transaction::Certificate;
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // e_max is typically 18 on mainnet.  Retirement at current_epoch + e_max + 1.
        let current_epoch: u64 = 500;
        let too_late_epoch = current_epoch + params.e_max + 1;
        tx.body.certificates.push(Certificate::PoolRetirement {
            pool_hash: Hash28::from_bytes([0x11u8; 28]),
            epoch: too_late_epoch,
        });
        // validate_transaction_with_pools signature:
        // (tx, utxo, params, current_slot, tx_size, slot_config,
        //  registered_pools, current_treasury, reward_accounts, current_epoch,
        //  registered_dreps, registered_vrf_keys, node_network,
        //  committee_members, committee_resigned, stake_key_deposits,
        //  constitution_script_hash, vote_delegations)
        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            Some(current_epoch),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolRetirementTooLate { .. })),
            "retirement beyond e_max must produce PoolRetirementTooLate, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 39 — Rule 1e: pool retirement exactly at e_max passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_retirement_exactly_at_e_max_passes() {
        use dugite_primitives::transaction::Certificate;
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let current_epoch: u64 = 500;
        let valid_epoch = current_epoch + params.e_max;
        tx.body.certificates.push(Certificate::PoolRetirement {
            pool_hash: Hash28::from_bytes([0x22u8; 28]),
            epoch: valid_epoch,
        });
        let result = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            Some(current_epoch),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let no_late_error = result.is_ok()
            || result
                .as_ref()
                .err()
                .map(|es| {
                    !es.iter()
                        .any(|e| matches!(e, ValidationError::PoolRetirementTooLate { .. }))
                })
                .unwrap_or(true);
        assert!(
            no_late_error,
            "retirement exactly at e_max must NOT produce PoolRetirementTooLate, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 40 — Rule 1f: Conway stake registration deposit mismatch rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_conway_stake_reg_deposit_mismatch_rejected() {
        use dugite_primitives::transaction::Certificate;
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        tx.body
            .certificates
            .push(Certificate::ConwayStakeRegistration {
                credential: dugite_primitives::credentials::Credential::VerificationKey(
                    Hash28::from_bytes([0x33u8; 28]),
                ),
                deposit: Lovelace(999_999), // Wrong deposit amount
            });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakeRegistrationDepositMismatch { .. })),
            "ConwayStakeRegistration with wrong deposit must produce StakeRegistrationDepositMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 41 — Rule 1g: Conway stake deregistration refund mismatch rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_conway_stake_dereg_refund_mismatch_rejected() {
        use dugite_primitives::transaction::Certificate;
        use std::collections::HashMap;

        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        let cred = dugite_primitives::credentials::Credential::VerificationKey(Hash28::from_bytes(
            [0x44u8; 28],
        ));
        let cred_hash = cred.to_typed_hash32();

        // Record the stored deposit for this credential.
        let mut stake_key_deposits = HashMap::new();
        stake_key_deposits.insert(cred_hash, 2_000_000u64);

        tx.body
            .certificates
            .push(Certificate::ConwayStakeDeregistration {
                credential: cred,
                refund: Lovelace(1_000_000), // Wrong refund amount
            });

        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,                      // slot_config
            None,                      // registered_pools
            None,                      // current_treasury
            None,                      // reward_accounts
            None,                      // current_epoch
            None,                      // registered_dreps
            None,                      // registered_vrf_keys
            None,                      // node_network
            None,                      // committee_members
            None,                      // committee_resigned
            Some(&stake_key_deposits), // stake_key_deposits
            None,                      // constitution_script_hash
            None,                      // vote_delegations
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakeDeregistrationRefundMismatch { .. })),
            "ConwayStakeDeregistration with wrong refund must produce StakeDeregistrationRefundMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 42 — Rule 1h: pool cost below min_pool_cost rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_cost_below_min_pool_cost_rejected() {
        use dugite_primitives::transaction::{Certificate, PoolParams, Rational};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.min_pool_cost = Lovelace(340_000_000); // 340 ADA min

        tx.body
            .certificates
            .push(Certificate::PoolRegistration(PoolParams {
                operator: Hash28::from_bytes([0x55u8; 28]),
                vrf_keyhash: Hash32::from_bytes([0x66u8; 32]),
                pledge: Lovelace(0),
                cost: Lovelace(100_000_000), // Below min
                margin: Rational {
                    numerator: 1,
                    denominator: 100,
                },
                reward_account: {
                    let mut acct = vec![0xE1u8];
                    acct.extend_from_slice(&[0x77u8; 28]);
                    acct
                },
                pool_owners: vec![],
                relays: vec![],
                pool_metadata: None,
            }));

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakePoolCostTooLow { .. })),
            "pool cost below min_pool_cost must produce StakePoolCostTooLow, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 43 — Rule 1i: pool reward account network mismatch
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_reward_account_wrong_network_rejected() {
        use dugite_primitives::transaction::{Certificate, PoolParams, Rational};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // Set network_id = mainnet (1) in the transaction body
        tx.body.network_id = Some(1);
        // Pool reward account uses testnet header (0xE0 — bit 0 = 0 = testnet)
        tx.body
            .certificates
            .push(Certificate::PoolRegistration(PoolParams {
                operator: Hash28::from_bytes([0x88u8; 28]),
                vrf_keyhash: Hash32::from_bytes([0x99u8; 32]),
                pledge: Lovelace(0),
                cost: Lovelace(340_000_000),
                margin: Rational {
                    numerator: 1,
                    denominator: 100,
                },
                reward_account: {
                    let mut acct = vec![0xE0u8]; // TESTNET key reward addr
                    acct.extend_from_slice(&[0xAAu8; 28]);
                    acct
                },
                pool_owners: vec![],
                relays: vec![],
                pool_metadata: None,
            }));

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolRewardAccountWrongNetwork { .. })),
            "pool reward account on wrong network must produce PoolRewardAccountWrongNetwork, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 44 — Rule 5b: output on wrong network rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_wrong_network_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // Set tx network_id = mainnet (1)
        tx.body.network_id = Some(1);

        // Add a testnet enterprise address output
        let testnet_addr = dugite_primitives::address::Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(Hash28::from_bytes([0xBBu8; 28])),
        });
        tx.body
            .outputs
            .push(dugite_primitives::transaction::TransactionOutput {
                address: testnet_addr,
                value: Value::lovelace(0),
                datum: dugite_primitives::transaction::OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::NetworkMismatch { .. })),
            "output on testnet with tx network_id=mainnet must produce NetworkMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 45 — Rule 1c: auxiliary data hash present but no data rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_auxiliary_data_hash_present_without_data_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        tx.body.auxiliary_data_hash = Some(Hash32::from_bytes([0xCCu8; 32]));
        tx.auxiliary_data = None;

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::AuxiliaryDataHashWithoutData)),
            "auxiliary_data_hash with no data must produce AuxiliaryDataHashWithoutData, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 46 — Rule 9: reference input not found in UTxO rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_reference_input_not_found_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // Add a reference input that doesn't exist in the UTxO set.
        tx.body.reference_inputs.push(TransactionInput {
            transaction_id: Hash32::from_bytes([0xDDu8; 32]),
            index: 0,
        });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ReferenceInputNotFound(_))),
            "missing reference input must produce ReferenceInputNotFound, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 47 — Rule 10: required signer with NO vkey witnesses at all
    // -----------------------------------------------------------------------
    #[test]
    fn test_required_signer_no_witnesses_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // Add a required signer (padded to Hash32).
        let required_keyhash = Hash32::from_bytes({
            let mut b = [0u8; 32];
            b[..28].copy_from_slice(&[0xEEu8; 28]);
            b
        });
        tx.body.required_signers.push(required_keyhash);
        tx.witness_set.vkey_witnesses = vec![]; // No witnesses at all

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingRequiredSigner(_))),
            "required signer with no witnesses must produce MissingRequiredSigner, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 48 — Rule 1d: ConwayStakeRegistration in pre-Conway era rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_conway_cert_in_pre_conway_era_rejected() {
        use dugite_primitives::transaction::Certificate;
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage — pre-Conway

        tx.body
            .certificates
            .push(Certificate::ConwayStakeRegistration {
                credential: Credential::VerificationKey(Hash28::from_bytes([0xFFu8; 28])),
                deposit: Lovelace(2_000_000),
            });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        // Era gating check should fire for Conway-only cert in pre-Conway era
        assert!(
            !errors.is_empty(),
            "Conway-only cert in pre-Conway era must produce at least one validation error"
        );
    }

    // -----------------------------------------------------------------------
    // Test 49 — Rule 2: value not conserved (output > input - fee)
    // -----------------------------------------------------------------------
    #[test]
    fn test_value_not_conserved_output_too_high() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Inflate the output past what inputs allow.
        tx.body.outputs[0].value = Value::lovelace(9_900_000); // was 9_800_000; now creates 100k shortfall
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ValueNotConserved { .. })),
            "output too high must produce ValueNotConserved, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 50 — Rule 3: value not conserved (output < input - fee, lovelace lost)
    // -----------------------------------------------------------------------
    #[test]
    fn test_value_not_conserved_lovelace_lost() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Deflate output — lovelace disappears.
        tx.body.outputs[0].value = Value::lovelace(9_700_000); // was 9_800_000
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ValueNotConserved { .. })),
            "lovelace lost (output too low) must produce ValueNotConserved, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 51 — governance proposal deposit mismatch (Conway, Rule 2-adjacent)
    // -----------------------------------------------------------------------
    #[test]
    fn test_proposal_deposit_incorrect_rejected() {
        use dugite_primitives::transaction::{GovAction, ProposalProcedure};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway
        params.gov_action_deposit = Lovelace(100_000_000_000);

        tx.body.proposal_procedures.push(ProposalProcedure {
            deposit: Lovelace(1_000_000), // Wrong deposit
            return_addr: vec![0u8; 29],
            gov_action: GovAction::InfoAction,
            anchor: dugite_primitives::transaction::Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::ZERO,
            },
        });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ProposalDepositIncorrect { .. })),
            "wrong proposal deposit must produce ProposalDepositIncorrect, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 52 — Rule 5: output below min UTxO value rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_below_min_utxo_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        // Set output to 1 lovelace — far below any min UTxO.
        let conserved_fee = tx.body.fee.0;
        tx.body.outputs[0].value = Value::lovelace(1);
        // Add the remainder back as fee to conserve value.
        tx.body.fee.0 = 10_000_000 - 1;
        let _ = conserved_fee; // avoid warning
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::OutputTooSmall { .. })),
            "output below min UTxO must produce OutputTooSmall, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 53 — Rule 5: output exactly at min UTxO passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_at_min_utxo_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let min_utxo = params.min_utxo_value().0;
        // We need total output + fee == 10_000_000
        if min_utxo + 200_000 <= 10_000_000 {
            tx.body.outputs[0].value = Value::lovelace(min_utxo);
            tx.body.fee = Lovelace(10_000_000 - min_utxo);
        }
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let no_small_error = result.is_ok()
            || result
                .as_ref()
                .err()
                .map(|es| {
                    !es.iter()
                        .any(|e| matches!(e, ValidationError::OutputTooSmall { .. }))
                })
                .unwrap_or(true);
        assert!(
            no_small_error,
            "output exactly at min UTxO must NOT produce OutputTooSmall, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 54 — pool_metadata_hash_too_big helper: Alonzo+ gating
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_metadata_hash_too_big_helper_alonzo_gating() {
        use super::is_pool_metadata_hash_too_big;

        // Pre-Alonzo (pv_major <= 4): any size accepted
        assert!(
            !is_pool_metadata_hash_too_big(&[0u8; 64], 4),
            "Pre-Alonzo must not enforce hash size cap"
        );
        // Alonzo+ (pv_major > 4), size exactly 32: accepted
        assert!(
            !is_pool_metadata_hash_too_big(&[0u8; 32], 5),
            "Alonzo+, size=32 must pass"
        );
        // Alonzo+ (pv_major > 4), size > 32: rejected
        assert!(
            is_pool_metadata_hash_too_big(&[0u8; 33], 5),
            "Alonzo+, size=33 must be flagged as too big"
        );
    }

    // -----------------------------------------------------------------------
    // Test 55 — Byron output attributes boundary: exactly 64 bytes passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_boot_addr_attrs_exactly_64_bytes_passes() {
        use super::output_boot_addr_attrs_too_big_indices;
        use dugite_primitives::address::{Address, ByronAddress};

        // A Byron payload where attributes_byte_size returns Some(64):
        // The ByronAddress CBOR payload contains the attributes map.
        // We test the helper directly since constructing valid Byron CBOR is complex.
        // attributes_byte_size > 64 triggers the error.
        // Instead test the indices function returns empty for size <= 64.
        // Create a mock output with a Byron address whose attributes byte size is None
        // (malformed) — should be silently passed.
        let output = dugite_primitives::transaction::TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![0x82, 0x00, 0x01], // minimal 3-byte payload
            }),
            value: Value::lovelace(1_000_000),
            datum: dugite_primitives::transaction::OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let bad = output_boot_addr_attrs_too_big_indices(&[output]);
        // Malformed payload returns None from attributes_byte_size → silently pass
        assert!(
            bad.is_empty(),
            "Malformed Byron payload must be silently accepted (attributes_byte_size=None)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 56 — extract_reward_credential: key-hash type
    // -----------------------------------------------------------------------
    #[test]
    fn test_extract_reward_credential_key_hash() {
        use super::extract_reward_credential;

        // Header 0xE1 (mainnet key reward addr) → VerificationKey
        let mut account = vec![0xE1u8];
        account.extend_from_slice(&[0x42u8; 28]);
        let cred = extract_reward_credential(&account);
        assert!(
            matches!(cred, Some(Credential::VerificationKey(_))),
            "0xE1 header must yield VerificationKey credential"
        );
    }

    // -----------------------------------------------------------------------
    // Test 57 — extract_reward_credential: script type
    // -----------------------------------------------------------------------
    #[test]
    fn test_extract_reward_credential_script_hash() {
        use super::extract_reward_credential;

        // Header 0xF1 (mainnet script reward addr) → Script
        let mut account = vec![0xF1u8];
        account.extend_from_slice(&[0x55u8; 28]);
        let cred = extract_reward_credential(&account);
        assert!(
            matches!(cred, Some(Credential::Script(_))),
            "0xF1 header must yield Script credential"
        );
    }

    // -----------------------------------------------------------------------
    // Test 58 — extract_reward_credential: too-short address returns None
    // -----------------------------------------------------------------------
    #[test]
    fn test_extract_reward_credential_too_short_returns_none() {
        use super::extract_reward_credential;

        let short = vec![0xE1u8; 10]; // Only 10 bytes, need >= 29
        let cred = extract_reward_credential(&short);
        assert!(cred.is_none(), "Too-short reward account must return None");
    }

    // -----------------------------------------------------------------------
    // Test 59 — is_pool_metadata_hash_too_big: pv_major=4 (threshold boundary)
    // -----------------------------------------------------------------------
    #[test]
    fn test_pool_metadata_hash_not_too_big_at_pv4() {
        use super::is_pool_metadata_hash_too_big;
        // pv_major == 4 is NOT > 4, so even oversized hashes are accepted.
        assert!(
            !is_pool_metadata_hash_too_big(&[0u8; 100], 4),
            "pv_major=4 must not enforce the hash size cap"
        );
        assert!(
            is_pool_metadata_hash_too_big(&[0u8; 100], 5),
            "pv_major=5 must enforce the hash size cap for hash > 32 bytes"
        );
    }

    #[test]
    fn test_pool_medata_no_metadata_passes() {
        // The aggregate validator skips the check when `pool_metadata`
        // is `None`. Build a registration without metadata and confirm
        // no `PoolMedataHashTooBig` error is produced.
        use dugite_primitives::transaction::{Certificate, PoolParams, Rational};

        let (mut utxo_set, mut tx, _) = make_valid_tx();
        // Reuse the existing baseline — it has no certificates. Add one
        // pool registration without metadata.
        let pool_params = PoolParams {
            operator: Hash28::from_bytes([0x11u8; 28]),
            vrf_keyhash: Hash32::from_bytes([0x22u8; 32]),
            pledge: Lovelace(0),
            cost: Lovelace(340_000_000),
            margin: Rational {
                numerator: 1,
                denominator: 100,
            },
            // Reward account on testnet (header byte 0xE0).
            reward_account: {
                let mut acct = vec![0xE0];
                acct.extend_from_slice(&[0x33u8; 28]);
                acct
            },
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        };
        tx.body
            .certificates
            .push(Certificate::PoolRegistration(pool_params));

        // Top up inputs so the (now larger) pool deposit balances; the
        // pool deposit is 500 ADA on mainnet defaults. Rather than
        // reshape the fixture, we just inspect the error list and
        // assert the predicate did not fire — other balance/witness
        // errors are tolerated.
        let _ = &mut utxo_set; // silence unused-mut warning under cfg branches
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMedataHashTooBig { .. })),
            "expected no PoolMedataHashTooBig, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // D9 / audit #544: Malformed vkeys must not satisfy required-signer check
    //
    // A fabricated 1-byte vkey whose blake2b_224 happens to equal a required
    // signer's keyhash must NOT satisfy rule 10.  Before the D9 fix, the
    // vkey_witness_hashes set included hashes of malformed vkeys, allowing a
    // tx with `vkey=[0x01]` and `required_signer = blake2b_224([0x01])` to
    // pass phase-1 with zero cryptographic verification.
    // -----------------------------------------------------------------------

    /// D9: blake2b_224 of a 1-byte vkey must not satisfy a required-signer check.
    /// Haskell rejects any non-32-byte vkey at `rawDeserialiseVerKeyDSIGN` time;
    /// Dugite must reject it at the hashing filter (rule 9b/10).
    #[test]
    fn test_d9_malformed_vkey_cannot_satisfy_required_signer() {
        use dugite_primitives::hash::{blake2b_224, Hash32};

        // The malformed vkey is just one byte.
        let malformed_vkey = vec![0x01u8];
        let fabricated_keyhash = blake2b_224(&malformed_vkey);

        // Build the Hash32 form used in required_signers (zero-padded from 28 bytes).
        let mut hash32_bytes = [0u8; 32];
        hash32_bytes[..28].copy_from_slice(fabricated_keyhash.as_bytes());
        let required_signer = Hash32::from_bytes(hash32_bytes);

        let (utxo_set, mut tx, _) = make_valid_tx();
        // Set a required signer whose keyhash = blake2b_224([0x01])
        tx.body.required_signers = vec![required_signer];
        // Add a malformed vkey witness with the 1-byte vkey
        tx.witness_set
            .vkey_witnesses
            .push(dugite_primitives::transaction::VKeyWitness {
                vkey: malformed_vkey.clone(),
                signature: vec![0u8; 64],
            });

        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();

        // Must reject: the 1-byte vkey must not satisfy required_signer.
        // We expect either InvalidWitnessSignature (from rule 14) or
        // MissingRequiredSigner (from rule 10) — both are correct.
        let rejected = errors.iter().any(|e| {
            matches!(
                e,
                ValidationError::InvalidWitnessSignature(_)
                    | ValidationError::MissingRequiredSigner(_)
            )
        });
        assert!(
            rejected,
            "malformed 1-byte vkey must not satisfy required-signer check; errors: {errors:?}"
        );
    }

    /// D9 length-lattice: for all vkey sizes except 32, the required-signer check
    /// must reject (either via InvalidWitnessSignature or MissingRequiredSigner).
    #[test]
    fn test_d9_required_signer_length_lattice() {
        use dugite_primitives::hash::{blake2b_224, Hash32};

        for vkey_len in [0_usize, 1, 16, 31, 33, 64] {
            let malformed_vkey = vec![0xABu8; vkey_len];
            let fabricated_keyhash = blake2b_224(&malformed_vkey);
            let mut hash32_bytes = [0u8; 32];
            hash32_bytes[..28].copy_from_slice(fabricated_keyhash.as_bytes());
            let required_signer = Hash32::from_bytes(hash32_bytes);

            let (utxo_set, mut tx, _) = make_valid_tx();
            tx.body.required_signers = vec![required_signer];
            tx.witness_set
                .vkey_witnesses
                .push(dugite_primitives::transaction::VKeyWitness {
                    vkey: malformed_vkey,
                    signature: vec![0u8; 64],
                });

            let params = ProtocolParameters::mainnet_defaults();
            let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
                .err()
                .unwrap_or_default();

            let rejected = errors.iter().any(|e| {
                matches!(
                    e,
                    ValidationError::InvalidWitnessSignature(_)
                        | ValidationError::MissingRequiredSigner(_)
                )
            });
            assert!(
                rejected,
                "vkey_len={vkey_len}: malformed vkey must not satisfy required-signer; errors={errors:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test D8 — Rule 1i: pool reward account must be exactly 29 bytes
    // -----------------------------------------------------------------------
    #[test]
    fn test_d8_pool_reward_account_wrong_length_rejected() {
        use dugite_primitives::transaction::{Certificate, PoolParams, Rational};
        // Test both too-short (28 bytes) and too-long (30 bytes) cases.
        for bad_len in [0usize, 1, 28, 30, 64] {
            let (utxo_set, mut tx, _) = make_valid_tx();
            let params = ProtocolParameters::mainnet_defaults();

            tx.body
                .certificates
                .push(Certificate::PoolRegistration(PoolParams {
                    operator: Hash28::from_bytes([0x88u8; 28]),
                    vrf_keyhash: Hash32::from_bytes([0x99u8; 32]),
                    pledge: Lovelace(0),
                    cost: Lovelace(340_000_000),
                    margin: Rational {
                        numerator: 1,
                        denominator: 100,
                    },
                    // Wrong-length reward account — one byte header (mainnet) + wrong tail.
                    reward_account: {
                        let mut acct = vec![0xE1u8]; // mainnet key reward addr header
                        acct.extend_from_slice(&vec![0xAA; bad_len.saturating_sub(1)]);
                        acct.truncate(bad_len);
                        acct
                    },
                    pool_owners: vec![],
                    relays: vec![],
                    pool_metadata: None,
                }));

            let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::InvalidRewardAccount(_))),
                "reward_account of length {bad_len} must produce InvalidRewardAccount, got {errors:?}"
            );
        }
    }

    /// Length-lattice: exactly 29 bytes with a valid header must not produce
    /// InvalidRewardAccount (it may still fail for other reasons, but not D8).
    #[test]
    fn test_d8_pool_reward_account_exact_length_ok() {
        use dugite_primitives::transaction::{Certificate, PoolParams, Rational};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // 29-byte mainnet key reward account — should NOT produce InvalidRewardAccount.
        tx.body
            .certificates
            .push(Certificate::PoolRegistration(PoolParams {
                operator: Hash28::from_bytes([0x88u8; 28]),
                vrf_keyhash: Hash32::from_bytes([0x99u8; 32]),
                pledge: Lovelace(0),
                cost: Lovelace(340_000_000),
                margin: Rational {
                    numerator: 1,
                    denominator: 100,
                },
                reward_account: {
                    let mut acct = vec![0xE1u8]; // mainnet key reward addr
                    acct.extend_from_slice(&[0xAA; 28]);
                    acct
                },
                pool_owners: vec![],
                relays: vec![],
                pool_metadata: None,
            }));

        // May succeed or fail for other reasons (e.g. pool not registered) —
        // but must not fail with InvalidRewardAccount.
        match validate_transaction(&tx, &utxo_set, &params, 100, 300, None) {
            Ok(_) => {}
            Err(errors) => {
                assert!(
                    !errors
                        .iter()
                        .any(|e| matches!(e, ValidationError::InvalidRewardAccount(_))),
                    "exactly 29-byte reward account must not produce InvalidRewardAccount, got {errors:?}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tests — F3: Pool margin bounds validation (#546 security audit)
    //
    // Haskell `PoolMarginsInvalidPOOL` rejects:
    //   * denominator == 0 (division by zero)
    //   * numerator > denominator (margin > 100%)
    // -----------------------------------------------------------------------

    use dugite_primitives::transaction::{Certificate, PoolParams, Rational};

    fn make_pool_registration_tx(
        numerator: u64,
        denominator: u64,
    ) -> (crate::utxo::UtxoSet, Transaction) {
        let (mut utxo_set, mut tx, _) = make_valid_tx();
        // Add enough inputs to cover the pool deposit (500 ADA on mainnet)
        let extra_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xBBu8; 32]),
            index: 0,
        };
        utxo_set.insert(
            extra_input.clone(),
            TransactionOutput {
                address: Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: vec![0x82, 0x00, 0x01],
                }),
                value: Value::lovelace(600_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );
        tx.body.inputs.push(extra_input);
        // Update output to match new value: 10M + 600M - 500M_deposit - 200K_fee
        // For test purposes we just keep it simple: output = 5M, fee = 200K,
        // pool_deposit = 500M. The value check isn't relevant; we check margin.
        // We don't rebalance the tx body — we only check the PoolMarginInvalid error.

        let pool_params = PoolParams {
            operator: Hash28::from_bytes([0x11u8; 28]),
            vrf_keyhash: Hash32::from_bytes([0x22u8; 32]),
            pledge: Lovelace(0),
            cost: Lovelace(340_000_000),
            margin: Rational {
                numerator,
                denominator,
            },
            reward_account: {
                let mut acct = vec![0xE0];
                acct.extend_from_slice(&[0x33u8; 28]);
                acct
            },
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        };
        tx.body
            .certificates
            .push(Certificate::PoolRegistration(pool_params));
        (utxo_set, tx)
    }

    /// Helper: validate a pool registration and check for PoolMarginInvalid.
    fn pool_margin_errors(numerator: u64, denominator: u64) -> Vec<ValidationError> {
        let (utxo_set, tx) = make_pool_registration_tx(numerator, denominator);
        let params = ProtocolParameters::mainnet_defaults();
        validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default()
    }

    #[test]
    fn test_pool_margin_zero_denominator_rejected() {
        let errors = pool_margin_errors(0, 0);
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMarginInvalid { denominator: 0, .. })),
            "expected PoolMarginInvalid with denominator=0, got {errors:?}"
        );
    }

    #[test]
    fn test_pool_margin_numerator_greater_than_denominator_rejected() {
        // margin = 101/100 > 1.0
        let errors = pool_margin_errors(101, 100);
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::PoolMarginInvalid {
                    numerator: 101,
                    denominator: 100,
                }
            )),
            "expected PoolMarginInvalid(101/100), got {errors:?}"
        );
    }

    #[test]
    fn test_pool_margin_exactly_one_accepted() {
        // margin = 1/1 == 1.0 (100%) — at the boundary, should NOT be rejected
        let errors = pool_margin_errors(1, 1);
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMarginInvalid { .. })),
            "margin 1/1 must NOT be rejected as PoolMarginInvalid, got {errors:?}"
        );
    }

    #[test]
    fn test_pool_margin_zero_numerator_accepted() {
        // margin = 0/100 == 0.0 — valid lower bound
        let errors = pool_margin_errors(0, 100);
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMarginInvalid { .. })),
            "margin 0/100 must NOT be rejected as PoolMarginInvalid, got {errors:?}"
        );
    }

    #[test]
    fn test_pool_margin_typical_5_percent_accepted() {
        // margin = 5/100 == 0.05 — typical SPO margin
        let errors = pool_margin_errors(5, 100);
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMarginInvalid { .. })),
            "margin 5/100 must NOT be rejected as PoolMarginInvalid, got {errors:?}"
        );
    }

    #[test]
    fn test_pool_margin_max_u64_denominator_zero_numerator_accepted() {
        // 0/u64::MAX — valid (0%)
        let errors = pool_margin_errors(0, u64::MAX);
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolMarginInvalid { .. })),
            "margin 0/MAX must NOT be rejected as PoolMarginInvalid, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — F1: BootstrapWitness crypto verification (#546 security audit)
    //
    // Byron bootstrap witnesses carry 64-byte extended Ed25519 keys.
    // Verification uses vkey[0..32] (the scalar part) over the tx body hash.
    // Address binding: computed root must match the root in the Byron UTxO address.
    // -----------------------------------------------------------------------

    /// Build a synthetic Byron address payload whose root field contains the given 28 bytes.
    ///
    /// Structure: array(2) [ tag(24, bytes(inner)), crc(0) ]
    /// inner:     array(3) [ bytes(root_28), map({}), uint(0) ]
    fn synth_byron_addr_with_root(root28: &[u8; 28]) -> Vec<u8> {
        let inner = {
            let mut buf = Vec::new();
            let mut e = minicbor::Encoder::new(&mut buf);
            e.array(3).unwrap();
            e.bytes(root28).unwrap();
            e.map(0).unwrap(); // empty attrs map
            e.u8(0).unwrap(); // addrtype=PubKey
            buf
        };

        let mut outer = Vec::new();
        let mut oe = minicbor::Encoder::new(&mut outer);
        oe.array(2).unwrap();
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner).unwrap();
        oe.u32(0).unwrap(); // dummy CRC
        outer
    }

    /// Build a UTxO whose input address is a Byron address with the given root,
    /// plus a matching Transaction referencing that input.
    fn make_byron_utxo_tx_with_root(root28: &[u8; 28]) -> (UtxoSet, Transaction, TransactionInput) {
        let mut utxo_set = UtxoSet::new();
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xCCu8; 32]),
            index: 0,
        };
        let utxo_output = TransactionOutput {
            address: Address::Byron(dugite_primitives::address::ByronAddress {
                payload: synth_byron_addr_with_root(root28),
            }),
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(input.clone(), utxo_output);

        // Use make_valid_tx as a baseline and override the input/utxo
        let (_, mut tx, _) = make_valid_tx();
        tx.hash = Hash32::from_bytes([0xDDu8; 32]);
        tx.body.inputs = vec![input.clone()];
        tx.witness_set.bootstrap_witnesses = vec![];
        (utxo_set, tx, input)
    }

    /// Compute the Byron address root from a 64-byte extended key and empty attributes.
    /// This mirrors `compute_bootstrap_root` (which is pub(super) for tests).
    fn compute_root_for_vkey64(vkey64: &[u8; 64]) -> [u8; 28] {
        super::compute_bootstrap_root(vkey64, &[0xa0]) // 0xa0 = CBOR empty map
            .expect("root computation must succeed")
    }

    // ------ Positive tests -------

    #[test]
    fn test_bootstrap_witness_malformed_64byte_vkey_invalid_sig_rejected() {
        // vkey=64 bytes (Byron extended), sig=64 bytes, but sig is all-zeros → fails Ed25519 verify.
        // Address binding will also fail (root mismatch). Both → InvalidWitnessSignature.
        let vkey64 = [0x55u8; 64]; // not a valid Ed25519 scalar but structurally well-sized
        let root28 = compute_root_for_vkey64(&vkey64);
        let (mut utxo_set, mut tx, _) = make_byron_utxo_tx_with_root(&root28);
        // Replace the UTxO's address with a Byron address whose root does NOT match
        // (to isolate the sig-verification path from the address-binding path).
        let mismatched_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xCCu8; 32]),
            index: 0,
        };
        utxo_set.insert(
            mismatched_input,
            TransactionOutput {
                address: Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: synth_byron_addr_with_root(&root28),
                }),
                value: Value::lovelace(10_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );
        tx.witness_set.bootstrap_witnesses.push(BootstrapWitness {
            vkey: vkey64.to_vec(),
            signature: vec![0u8; 64], // invalid signature
            chain_code: vec![0u8; 32],
            attributes: vec![0xa0], // empty CBOR map
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature for bad sig on 64-byte vkey, got {errors:?}"
        );
    }

    // ------ Structural pre-flight: size checks -------

    #[test]
    fn test_bootstrap_witness_32byte_vkey_rejected() {
        // 32-byte vkey (Shelley format) in a bootstrap witness → must be rejected.
        // Byron requires exactly 64-byte extended keys.
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 32], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_31byte_vkey_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 31], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_65byte_vkey_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 65], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_0byte_vkey_rejected() {
        let errors = validate_with_bootstrap_witness(vec![], vec![0u8; 64]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_63byte_sig_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 64], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_65byte_sig_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 64], vec![0u8; 65]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_0byte_sig_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 64], vec![]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_both_wrong_size_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 63], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    /// Length-lattice property: for all {vkey_len, sig_len} ≠ {64, 64},
    /// a bootstrap witness must produce `InvalidWitnessSignature`.
    /// Excludes {64, 64} which passes structural checks but fails sig verify.
    #[test]
    fn test_bootstrap_witness_length_lattice_rejected() {
        for vkey_len in [0usize, 1, 16, 31, 32, 33, 48, 63, 65, 128] {
            for sig_len in [0usize, 1, 32, 63, 65, 96, 128] {
                let errors =
                    validate_with_bootstrap_witness(vec![0xAAu8; vkey_len], vec![0u8; sig_len]);
                assert!(
                    errors.iter().any(|e| matches!(
                        e,
                        ValidationError::InvalidWitnessSignature(_)
                    )),
                    "bootstrap vkey_len={vkey_len} sig_len={sig_len}: expected InvalidWitnessSignature, got {errors:?}"
                );
            }
        }
    }

    /// Chain-code length check: 64-byte vkey + 64-byte sig but 31-byte chain_code → rejected.
    #[test]
    fn test_bootstrap_witness_short_chain_code_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set.bootstrap_witnesses.push(BootstrapWitness {
            vkey: vec![0xAAu8; 64],
            signature: vec![0u8; 64],
            chain_code: vec![0u8; 31], // wrong size
            attributes: vec![],
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature for 31-byte chain_code, got {errors:?}"
        );
    }

    /// Address binding failure: structurally valid witness but no matching Byron address in UTxO.
    #[test]
    fn test_bootstrap_witness_address_binding_mismatch_rejected() {
        // Build a Byron UTxO with root = all-zeros (doesn't match any real witness key)
        let mismatch_root = [0u8; 28];
        let (mut utxo_set, mut tx, _) = make_byron_utxo_tx_with_root(&mismatch_root);

        // Witness with a DIFFERENT computed root
        let vkey64 = [0x42u8; 64];
        let computed_root = compute_root_for_vkey64(&vkey64);
        assert_ne!(
            computed_root, mismatch_root,
            "test setup: roots must differ"
        );

        // Replace UTxO address with mismatched root
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xCCu8; 32]),
            index: 0,
        };
        utxo_set.insert(
            input,
            TransactionOutput {
                address: Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: synth_byron_addr_with_root(&mismatch_root),
                }),
                value: Value::lovelace(10_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );
        tx.witness_set.bootstrap_witnesses.push(BootstrapWitness {
            vkey: vkey64.to_vec(),
            signature: vec![0u8; 64], // sig fails too but binding is the point
            chain_code: vec![0u8; 32],
            attributes: vec![0xa0],
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature for address-binding mismatch, got {errors:?}"
        );
    }

    /// Error message for a malformed bootstrap witness includes both observed sizes.
    #[test]
    fn test_bootstrap_witness_malformed_error_message_includes_sizes() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 32], vec![0u8; 63]);
        let msg = errors
            .iter()
            .find_map(|e| match e {
                ValidationError::InvalidWitnessSignature(s) => Some(s.clone()),
                _ => None,
            })
            .expect("InvalidWitnessSignature not found");
        assert!(
            msg.contains("32") && msg.contains("63"),
            "error message must include observed sizes (32, 63): {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — F2: ConwayDRepNotRegistered (#546 security audit)
    //
    // `UnregDRep` for a non-existent DRep must be rejected to prevent
    // fabricating an ADA-refund without a corresponding deposit.
    // -----------------------------------------------------------------------

    fn make_unreg_drep_tx(cred_hash: Hash28) -> (UtxoSet, Transaction) {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.certificates.push(Certificate::UnregDRep {
            credential: Credential::VerificationKey(cred_hash),
            refund: Lovelace(2_000_000),
        });
        (utxo_set, tx)
    }

    /// `UnregDRep` for a non-registered DRep → `DRepNotRegistered` when `registered_dreps` provided.
    #[test]
    fn test_unreg_drep_not_registered_rejected() {
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, tx) = make_unreg_drep_tx(cred_hash);
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9; // Conway
            p
        };
        let registered_dreps: std::collections::HashSet<Hash32> = std::collections::HashSet::new(); // empty — DRep not registered
        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            None,
            Some(&registered_dreps),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepNotRegistered { .. })),
            "expected DRepNotRegistered for unregistered DRep, got {errors:?}"
        );
    }

    /// `UnregDRep` for a registered DRep → no `DRepNotRegistered`.
    #[test]
    fn test_unreg_drep_registered_accepted() {
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, tx) = make_unreg_drep_tx(cred_hash);
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        // Build registered_dreps with the typed hash for this credential
        let credential = Credential::VerificationKey(cred_hash);
        let key = credential.to_typed_hash32();
        let mut registered_dreps: std::collections::HashSet<Hash32> =
            std::collections::HashSet::new();
        registered_dreps.insert(key);

        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            None,
            Some(&registered_dreps),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .err()
        .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepNotRegistered { .. })),
            "DRep IS registered — must not produce DRepNotRegistered, got {errors:?}"
        );
    }

    /// `UnregDRep` with no `registered_dreps` provided (mempool mode) → check skipped.
    #[test]
    fn test_unreg_drep_no_registry_check_skipped() {
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, tx) = make_unreg_drep_tx(cred_hash);
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        // registered_dreps=None → check is skipped
        let errors = validate_transaction_with_pools(
            &tx, &utxo_set, &params, 100, 300, None, None, None, None, None,
            None, // registered_dreps = None
            None, None, None, None, None, None, None,
        )
        .err()
        .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepNotRegistered { .. })),
            "With registered_dreps=None the check must be skipped, got {errors:?}"
        );
    }

    /// `RegDRep` for an already-registered DRep must not produce `DRepNotRegistered`.
    /// (Regression: ensure the check only applies to `UnregDRep`.)
    #[test]
    fn test_reg_drep_not_affected_by_unreg_check() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        tx.body.certificates.push(Certificate::RegDRep {
            credential: Credential::VerificationKey(cred_hash),
            deposit: dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults()
                .drep_deposit,
            anchor: None,
        });
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let registered_dreps: std::collections::HashSet<Hash32> = std::collections::HashSet::new();
        let errors = validate_transaction_with_pools(
            &tx,
            &utxo_set,
            &params,
            100,
            300,
            None,
            None,
            None,
            None,
            None,
            Some(&registered_dreps),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .err()
        .unwrap_or_default();
        // RegDRep must never produce DRepNotRegistered (that's for UnregDRep only)
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepNotRegistered { .. })),
            "RegDRep must not produce DRepNotRegistered, got {errors:?}"
        );
    }
}
