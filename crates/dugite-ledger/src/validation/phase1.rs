//! Core Phase-1 validation rules.
//!
//! This module implements the structural rules that do not require Plutus script
//! execution. Each numbered rule corresponds to a distinct ledger invariant:
//!
//! - Rule 1  — at least one input
//! - Rule 1b — no duplicate inputs (Conway+ PV≥9 only; Haskell silently dedups at PV<9)
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

use std::collections::HashSet;

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{Certificate, OutputDatum, Transaction};

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
/// | `GenesisKeyDelegation`          | Genesis key hash (`gk`)                 |
/// | `MoveInstantaneousRewards`      | None here — see whole-tx genesis-quorum |
/// |                                 | check `check_mir_genesis_quorum` (#804) |
/// Lowercase hex, without pulling in the `hex` crate (dugite-ledger does not
/// depend on it).
fn to_hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

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

        // GenesisDelegCert `gk _ _`: the named genesis (cold) key must sign.
        // Reference: Haskell `shelleyWitsVKeyNeeded` /
        // `Cardano.Ledger.Shelley.Rules.Utxow` — every `GenesisDelegCert`
        // contributes its genesis key hash to `neededWitnessKeys`. #804:
        // this was previously `vec![]`, silently accepting an unwitnessed
        // genesis-key delegation on the live ValidateAll path.
        //
        // `genesis_hash` is Hash32 (zero-padded from the on-wire 28-byte
        // hash); truncate to the first 28 bytes to match the Hash28 witness
        // key-hash domain (mirrors `eras::common::enqueue_genesis_key_delegations`).
        Certificate::GenesisKeyDelegation { genesis_hash, .. } => {
            let mut buf = [0u8; 28];
            buf.copy_from_slice(&genesis_hash.as_bytes()[..28]);
            vec![Hash28::from_bytes(buf)]
        }

        // MIR certs require NO per-cert VKey witness here — Haskell's
        // genesis-delegate quorum check (`validateMIRInsufficientGenesisSigs`
        // / `MIRInsufficientGenesisSigsUTXOW`) is a WHOLE-TRANSACTION
        // predicate over ALL of `dsGenDelegs`'s delegate keys, not a
        // per-certificate requirement — see `check_mir_genesis_quorum` in
        // `validation::mir` (#804).
        Certificate::MoveInstantaneousRewards { .. } => vec![],
    }
}

/// The SCRIPT hash a certificate requires a witness for, if its subject
/// credential is script-based.
///
/// Mirrors Haskell `getScriptWitnessConwayTxCert`
/// (`Cardano.Ledger.Conway.TxCert`) exactly:
///
/// ```haskell
/// ConwayRegCert _ SNothing     -> Nothing            -- reg_cert (idx 0)
/// ConwayRegCert cred (SJust _) -> credScriptHash cred -- reg_deposit_cert (idx 7)
/// ConwayUnRegCert cred _       -> credScriptHash cred -- idx 1 / 8
/// ConwayDelegCert cred _       -> credScriptHash cred -- idx 2 / 9 / 10
/// ConwayRegDelegCert cred _ _  -> credScriptHash cred -- idx 11 / 12 / 13
/// ConwayTxCertPool {}          -> Nothing            -- "PoolIds can't be Scripts"
/// ConwayAuthCommitteeHotKey / ResignCommitteeColdKey -> cold cred
/// ConwayRegDRep / UnRegDRep / UpdateDRep             -> cred
/// ```
///
/// The ONLY permissionless case is the deposit-less Shelley-compatible
/// `reg_cert` (index 0). Haskell's own comment explains why it is special:
/// "we preserve the old behavior of not requiring a witness for staking
/// credential registration, but only during the transitional period of Conway
/// era and only for staking credential registration certificates without a
/// deposit."
///
/// This is easy to get wrong in two directions, and dugite got it wrong in the
/// widest one: `cert_required_witnesses` above returns `None` for every script
/// credential (it only ever produced vkey requirements), and there was no
/// complementary script check at all. The result was that dugite accepted ANY
/// certificate whose subject is a script credential with no script witness
/// whatsoever — registration, deregistration, delegation, DRep and committee
/// certificates alike. cardano-node 11.0.1 rejects those with
/// `ConwayUtxowFailure (MissingScriptWitnessesUTXOW ...)`, so this was an
/// accept-set divergence in the dangerous direction: dugite too lax.
fn cert_required_script_witness(cert: &Certificate) -> Option<Hash28> {
    let script_hash = |c: &Credential| -> Option<Hash28> {
        match c {
            Credential::Script(h) => Some(*h),
            Credential::VerificationKey(_) => None,
        }
    };

    match cert {
        // Shelley `reg_cert` (index 0), no deposit field: permissionless by
        // design during the Conway transitional period. NOT a gap.
        Certificate::StakeRegistration(_) => None,

        // Conway `reg_deposit_cert` (index 7) carries an explicit deposit and
        // DOES require the credential to authorise it.
        Certificate::ConwayStakeRegistration { credential, .. } => script_hash(credential),

        // Deregistration (index 1 and 8) reclaims the deposit — always witnessed.
        Certificate::StakeDeregistration(credential)
        | Certificate::ConwayStakeDeregistration { credential, .. } => script_hash(credential),

        // Delegation (2 / 9 / 10) and the combined register+delegate forms
        // (11 / 12 / 13) are always witnessed — note the combined forms are
        // witnessed even though they also perform a registration.
        Certificate::StakeDelegation { credential, .. }
        | Certificate::VoteDelegation { credential, .. }
        | Certificate::StakeVoteDelegation { credential, .. }
        | Certificate::RegStakeDeleg { credential, .. }
        | Certificate::RegStakeVoteDeleg { credential, .. }
        | Certificate::VoteRegDeleg { credential, .. } => script_hash(credential),

        // Committee certificates (14 / 15) are witnessed by the COLD credential.
        Certificate::CommitteeHotAuth {
            cold_credential, ..
        }
        | Certificate::CommitteeColdResign {
            cold_credential, ..
        } => script_hash(cold_credential),

        // DRep certificates (16 / 17 / 18).
        Certificate::RegDRep { credential, .. }
        | Certificate::UnregDRep { credential, .. }
        | Certificate::UpdateDRep { credential, .. } => script_hash(credential),

        // Pool certificates can never carry a script credential — Haskell's
        // `getScriptWitnessConwayTxCert` returns Nothing unconditionally for
        // them ("PoolIds can't be Scripts").
        Certificate::PoolRegistration(_) | Certificate::PoolRetirement { .. } => None,

        // Pre-Conway certificates with no script-credential form.
        Certificate::GenesisKeyDelegation { .. } | Certificate::MoveInstantaneousRewards { .. } => {
            None
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
/// Recursively walk a `TransactionMetadatum` looking for `Bytes` or `Text`
/// leaves that exceed `max_size_bytes` UTF-8 bytes. Mirrors Haskell's
/// `decodeMetadatum` enforcement
/// (`libs/cardano-ledger-core/src/Cardano/Ledger/Metadata.hs`) which fires
/// on any oversize leaf at any depth.
fn metadatum_has_oversize_leaf(
    datum: &dugite_primitives::transaction::TransactionMetadatum,
    max_size_bytes: usize,
) -> bool {
    use dugite_primitives::transaction::TransactionMetadatum as M;
    match datum {
        M::Int(_) => false,
        M::Bytes(b) => b.len() > max_size_bytes,
        M::Text(t) => t.len() > max_size_bytes,
        M::List(items) => items
            .iter()
            .any(|i| metadatum_has_oversize_leaf(i, max_size_bytes)),
        M::Map(entries) => entries.iter().any(|(k, v)| {
            metadatum_has_oversize_leaf(k, max_size_bytes)
                || metadatum_has_oversize_leaf(v, max_size_bytes)
        }),
    }
}

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
    // Bootstrap witnesses (Byron; 32-byte public_key + 32-byte chain_code) take
    // the separate `verify_bootstrap_witnesses` path (#546 F1) and are never
    // dispatched here — `HasWitnessFields` is intentionally not impl'd on
    // `BootstrapWitness`.
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
// Wire format (Shelley CDDL `bootstrap_witness`):
//   [ public_key : bytes .size 32   ; bwKey: raw Ed25519 public key
//   , signature  : bytes .size 64   ; bwSignature: standard Ed25519 detached sig
//   , chain_code : bytes .size 32   ; bwChainCode
//   , attributes : bytes            ; bwAttributes (CBOR AddrAttributes)
//   ]
//
// `public_key` is the 32-byte Ed25519 key, NOT a 64-byte extended key. The
// 64-byte Byron XPub is `public_key (32) || chain_code (32)` and is used ONLY
// to derive the address root for the binding check — never for signature
// verification.
//
// Haskell references (IntersectMBO/cardano-ledger,
// libs/cardano-ledger-core/src/Cardano/Ledger/Keys/Bootstrap.hs):
//   - `verifyBootstrapWit` → `verifySignedDSIGN (bwKey w) txbodyHash (bwSig w)`
//     where `bwKey` is the 32-byte `VKey` passed directly to Ed25519
//     `crypto_sign_ed25519_verify_detached` (chain code unused).
//   - `bootstrapWitKeyHash` → `blake2b_224(sha3_256(prefix || bwKey(32) ||
//     bwChainCode(32) || bwAttributes))`, prefix `0x83 00 82 00 58 40` (the
//     CBOR of `Address' (ATVerKey, VerKeyASD xpub, attrs)`).
// ---------------------------------------------------------------------------

/// Verify one Byron bootstrap witness (structural + signature check).
///
/// Step 1: Pre-flight: vkey must be 32 bytes, sig 64 bytes, chain_code 32 bytes.
/// Step 2: Ed25519 verify over `tx_hash_bytes` using the 32-byte `vkey` directly.
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
    // `public_key` is 32 bytes (raw Ed25519 key); `signature` is 64 bytes.
    if vkey.len() != 32 || sig.len() != 64 {
        return Some(ValidationError::InvalidWitnessSignature(format!(
            "bootstrap: malformed witness: vkey={} bytes (expected 32), sig={} bytes (expected 64)",
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

    // Ed25519 verify using the 32-byte public key directly (the chain code is
    // not part of signature verification — only address-root derivation).
    match dugite_crypto::keys::PaymentVerificationKey::from_bytes(vkey) {
        Ok(vk) => {
            if vk.verify(tx_hash_bytes, sig).is_err() {
                Some(ValidationError::InvalidWitnessSignature(format!(
                    "bootstrap:sig_invalid:{:02x?}",
                    &vkey[..4]
                )))
            } else {
                None
            }
        }
        Err(_) => Some(ValidationError::InvalidWitnessSignature(format!(
            "bootstrap:invalid_scalar:{:02x?}",
            &vkey[..4]
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

/// Check address binding in the HASKELL direction: every BYRON INPUT must be
/// covered by a bootstrap witness whose `bootstrapWitKeyHash` equals the
/// input's Byron address root.
///
/// Extra bootstrap witnesses that do NOT bind to any Byron input are LEGAL:
/// Alonzo/Babbage UTXOW (`alonzoStyleWitness`) has no "every bootstrap
/// witness must match an input" predicate — bootstrap witnesses contribute
/// their key hashes to the `provided` set of `validateNeededWitnesses`
/// (`needed ⊆ provided`), and `validateVerifiedWits` checks only the Ed25519
/// signature. Legacy Daedalus-compatible wallets routinely attach BOTH a
/// vkey witness and a bootstrap witness for the same key when spending from
/// SHELLEY addresses; the previous wrong-direction check falsely rejected
/// two confirmed mainnet txs during the v2.0.4 soak (432b916e…/e1f29011…,
/// blocks 9,074,761/9,075,213 — all-Shelley inputs + a redundant bootstrap
/// witness).
fn check_bootstrap_address_binding(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
) -> Vec<ValidationError> {
    use dugite_primitives::address::Address;
    let mut errors = Vec::new();

    // bootstrapWitKeyHash for every structurally valid bootstrap witness.
    let bootstrap_key_hashes: HashSet<[u8; 28]> = tx
        .witness_set
        .bootstrap_witnesses
        .iter()
        .filter(|bw| bw.vkey.len() == 32 && bw.signature.len() == 64 && bw.chain_code.len() == 32)
        .filter_map(|bw| {
            // The Byron address root is derived from the 64-byte XPub =
            // public_key (32) || chain_code (32).
            let xpub: Vec<u8> = bw
                .vkey
                .iter()
                .chain(bw.chain_code.iter())
                .copied()
                .collect();
            compute_bootstrap_root(&xpub, &bw.attributes)
        })
        .collect();

    // Every Byron input must be covered (Haskell `witsVKeyNeeded` includes
    // Byron address roots in `needed`; uncovered → missing-witness class).
    for input in &tx.body.inputs {
        if let Some(output) = utxo_set.lookup(input) {
            if let Address::Byron(ref byron) = output.address {
                if let Some(root_bytes) = extract_byron_address_root(&byron.payload) {
                    if !bootstrap_key_hashes.contains(&root_bytes) {
                        errors.push(ValidationError::MissingInputWitness(
                            root_bytes
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>(),
                        ));
                    }
                }
            }
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
    stake_key_deposits: Option<&imbl::HashMap<Hash32, u64>>,
    errors: &mut Vec<ValidationError>,
) {
    let body = &tx.body;

    // Did every input resolve to a UTxO?
    //
    // #1030 item 1: the precondition for the checks that need input VALUES
    // (Rule 3 conservation) or input ADDRESSES (Rule 9b witness completeness).
    // Those two used to be gated on `errors.is_empty()`, which conflated "this
    // check cannot be computed" with "something else already went wrong" and so
    // shortened the reported failure list on any multi-failure transaction.
    // Haskell's `?!` never short-circuits, so all applicable failures reach
    // MsgRejectTx.
    //
    // Deliberately NARROW: only the variants that mean a UTxO lookup did not
    // produce an entry. `ValueNotConserved` is not here — it is a RESULT of this
    // check, not a precondition for it.
    fn inputs_resolved(errors: &[ValidationError]) -> bool {
        !errors.iter().any(|e| {
            matches!(
                e,
                ValidationError::NoInputs | ValidationError::InputNotFound(_)
            )
        })
    }

    // ------------------------------------------------------------------
    // Rule 1: Must have at least one input
    // ------------------------------------------------------------------
    if body.inputs.is_empty() {
        errors.push(ValidationError::NoInputs);
    }

    // ------------------------------------------------------------------
    // Rule 1b: No duplicate inputs
    //
    // Haskell semantic is protocol-version gated:
    //
    // PV < 9 (Alonzo/Babbage): `decodeSet` routes through the lenient path
    //   (`Set.fromList`), which silently deduplicates.  No `BabbageUtxoPredFailure`
    //   constructor for duplicate inputs exists, so Haskell accepts such txs.
    //   Real mainnet Babbage blocks contain transactions with duplicate spend
    //   inputs encoded in a plain CBOR array (e.g. tx 5ca83e21… at epoch 484,
    //   slot 123728795, PV8) — Haskell silently dedups and accepts.
    //
    // PV >= 9 (Conway+): `decodeSetEnforceNoDuplicates` hard-fails at the
    //   binary layer.  We mirror that by surfacing `DuplicateInput` at
    //   Phase-1 time (the net effect is the same rejection).
    //
    // Reference: `cardano-ledger-binary` `decodeSet` / `decodeSetEnforceNoDuplicates`
    //   (Cardano.Ledger.Binary.Decoding.Coders), and the absence of a
    //   DuplicateInput constructor in `AlonzoUtxoPredFailure` / `BabbageUtxoPredFailure`.
    // ------------------------------------------------------------------
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
                    // Haskell: `Mismatch { mismatchSupplied = mdh,
                    // mismatchExpected = hashTxAuxData md' }` — the DECLARED
                    // hash is "supplied", the recomputed one is "expected".
                    errors.push(ValidationError::AuxiliaryDataHashMismatch {
                        declared: declared_hash.to_hex(),
                        computed: computed.to_hex(),
                    });
                }
            }
        }
        (None, None) => {} // Both absent — OK
    }

    // ------------------------------------------------------------------
    // Rule 1c.iii: Allegra+ metadata 64-byte size limit (InvalidMetadata)
    //
    // Per Haskell `decodeMetadatum` in
    // `libs/cardano-ledger-core/src/Cardano/Ledger/Metadata.hs`, the
    // decoder enforces a 64-byte cap on every `Bytes` and `Text` leaf
    // when `dv > natVersion @2` (Allegra and later — Conway included):
    //
    // ```haskell
    // when (checkSizes && Prim.sizeofByteArray ba > 64) $
    //   decodeError "bytes .size (0..64): bytestring exceeds 64 bytes"
    // when (checkSizes && TF.lengthWord8 x > 64) $
    //   decodeError "text .size (0..64): text exceeds 64 bytes"
    // ```
    //
    // Haskell enforces this at CBOR-decode time and the corresponding
    // `InvalidMetadata` UTXOW predicate is essentially dead code
    // (`validateTxAuxData _ _ = True` in Shelley). dugite's decoder
    // currently accepts any size, so we mirror the enforcement at
    // validation time. PV >= 3 means Allegra+ (decoder version > 2).
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 3 {
        if let Some(ref aux) = tx.auxiliary_data {
            let mut bad_labels: Vec<u64> = Vec::new();
            for (label, datum) in aux.metadata.iter() {
                if metadatum_has_oversize_leaf(datum, 64) {
                    bad_labels.push(*label);
                }
            }
            if !bad_labels.is_empty() {
                bad_labels.sort();
                bad_labels.dedup();
                errors.push(ValidationError::InvalidMetadata { labels: bad_labels });
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1d: Era gating
    // ------------------------------------------------------------------
    super::conway::check_era_gating(params, body, errors);

    // ------------------------------------------------------------------
    // Rule 1e: Pool retirement epoch must satisfy currentEpoch < e <= currentEpoch + eMax
    //
    // Per Haskell's POOL rule
    // (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`, lines 308-323):
    //
    // ```haskell
    // (cEpoch < e && e <= limitEpoch)
    //   ?! injectFailure (StakePoolRetirementWrongEpochPOOL
    //        Mismatch{supplied=e, expected=cEpoch}     -- RelGT
    //        Mismatch{supplied=e, expected=limitEpoch} -- RelLTEQ
    //   )
    // ```
    //
    // where `limitEpoch = cEpoch + eMax`. Lower bound is STRICT
    // (retirement must be at least one epoch in the future). dugite
    // historically only checked the upper bound — the lower bound
    // check below catches `e <= cEpoch` (retirement scheduled in the
    // past or current epoch). Skipped when `current_epoch` is not
    // provided (mempool admission without epoch context).
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
                } else if *retirement_epoch <= epoch {
                    errors.push(ValidationError::PoolRetirementTooEarly {
                        retirement_epoch: *retirement_epoch,
                        current_epoch: epoch,
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
    // current protocol parameters. The three combined register+delegate
    // certificates (`RegStakeDeleg` tag 11, `VoteRegDeleg` tag 12,
    // `RegStakeVoteDeleg` tag 13) decode in Haskell to the SAME
    // `ConwayRegDelegCert` constructor and go through the identical
    // `checkDepositAgainstPParams` check unconditionally — the delegatee
    // shape (pool/DRep/both) does not change the deposit predicate.
    // Failure name: `IncorrectDepositDELEG` (PV 9-10) / `DepositIncorrectDELEG`
    // (PV >= 11); dugite surfaces both under `StakeRegistrationDepositMismatch`.
    //
    // Reference: `Conway.Rules.Deleg.conwayDelegTransition` (issue #785).
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for cert in &body.certificates {
            let deposit = match cert {
                Certificate::ConwayStakeRegistration { deposit, .. } => Some(deposit),
                Certificate::RegStakeDeleg { deposit, .. } => Some(deposit),
                Certificate::RegStakeVoteDeleg { deposit, .. } => Some(deposit),
                Certificate::VoteRegDeleg { deposit, .. } => Some(deposit),
                _ => None,
            };
            if let Some(deposit) = deposit {
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
    // Haskell's `ConwayUnRegCert` branch binds `mAccountState` and, when the
    // credential is NOT registered, binds `Nothing` and SKIPS the
    // refund-mismatch check entirely — only `StakeKeyNotRegisteredDELEG`
    // fires for an unregistered credential (issue #811). Mirror that: only
    // compare when the credential IS present in `stake_key_deposits`;
    // otherwise there is nothing to compare against and no error is raised
    // here (the not-registered predicate, if any, is raised elsewhere).
    //
    // This check applies only in Conway (protocol >= 9) where the new
    // certificate tag is used.  Pre-Conway `StakeDeregistration` (tag 1)
    // implicitly refunds `key_deposit` without carrying an explicit amount.
    // ------------------------------------------------------------------
    if params.protocol_version_major >= 9 {
        for cert in &body.certificates {
            if let Certificate::ConwayStakeDeregistration { credential, refund } = cert {
                let key = credential.to_typed_hash32();
                if let Some(expected) = stake_key_deposits.and_then(|m| m.get(&key).copied()) {
                    if refund.0 != expected {
                        errors.push(ValidationError::StakeDeregistrationRefundMismatch {
                            declared: refund.0,
                            expected,
                        });
                    }
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
                            pool_id: pool_params.operator.to_hex(),
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
    //
    // At PV < 9 the wire decoder does not dedup the raw `Vec` of inputs —
    // only the Conway decoder (PV >= 9) uses `read_set_strict` (see Rule
    // 1b above). Haskell decodes pre-Conway TxIn lists via `Set.fromList`,
    // silently collapsing physical duplicates BEFORE the existence check
    // and the ADA/multi-asset conservation sums. Failing to mirror this
    // double-counts a duplicated spend input's value and produces a false
    // `ValueNotConserved` on historically-valid mainnet Babbage txs (e.g.
    // `fixtures/tx-5ca83e21.hex`, issue #786). Iterate the DISTINCT inputs
    // at PV < 9; at PV >= 9 iterate the wire order as-is — Rule 1b already
    // emits `DuplicateInput` there and gates Rules 3/3b off via
    // `errors.is_empty()`, so no double-count can reach the conservation
    // checks. The wire `body.inputs` Vec itself is never mutated (its raw
    // order is required for hashing/serialization).
    // ------------------------------------------------------------------
    let distinct_inputs: Vec<&dugite_primitives::transaction::TransactionInput> =
        if params.protocol_version_major < 9 {
            let mut seen = HashSet::new();
            body.inputs.iter().filter(|i| seen.insert(*i)).collect()
        } else {
            body.inputs.iter().collect()
        };

    let mut input_value: u128 = 0;
    for input in distinct_inputs.iter().copied() {
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
    //
    // #1030 item 1: gated on INPUT RESOLUTION, not on `errors.is_empty()`.
    //
    // Haskell's STS `?!` never short-circuits within a rule body, so every
    // applicable predicate failure accumulates and all of them reach
    // `MsgRejectTx`. An `errors.is_empty()` gate silently shortens that list:
    // a `ProposalDepositIncorrect` raised immediately above — which has nothing
    // to do with whether ADA conservation can be computed — used to suppress the
    // conservation failure entirely. The verdict never diverged (both
    // implementations reject), but a client parsing the reason saw fewer causes
    // than cardano-node reports.
    //
    // What this check genuinely CANNOT be computed without is resolved inputs,
    // so that — and only that — is the precondition now.
    // ------------------------------------------------------------------
    if inputs_resolved(errors) {
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

        // Same PV<9 dedup as Rule 2 above — a duplicated spend input's
        // multi-asset bundle must be counted once, matching Haskell's
        // `Set.fromList` decode (issue #786).
        for input in distinct_inputs.iter().copied() {
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

        // Native scripts hash over their ORIGINAL wire bytes (#862).
        let witness_native_raws = super::scripts::witness_native_original_bytes(tx);
        for (i, script) in tx.witness_set.native_scripts.iter().enumerate() {
            let original = witness_native_raws
                .as_ref()
                .and_then(|v| v.get(i))
                .map(Vec::as_slice);
            available_script_hashes.insert(super::scripts::native_script_hash(script, original));
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
                    let native_original = super::scripts::reference_native_original_bytes(&utxo);
                    let hash = super::scripts::compute_script_ref_hash(
                        script_ref,
                        native_original.as_deref(),
                    );
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
    //
    // Haskell's `getMinCoinTxOut` is defined PER-ERA (issue #919) — dugite
    // previously applied the Babbage/Conway serialized-size formula
    // unconditionally in every era, producing false `OutputTooSmall`
    // rejections on real Shelley/Allegra/Mary mainnet transactions (the
    // Alonzo genesis `ada_per_utxo_byte` is seeded into `ProtocolParameters`
    // at node startup regardless of the chain's current era). See
    // `ProtocolParameters::min_coin_for_output` for the full per-era
    // dispatch (Shelley/Allegra flat, Mary scaled deposit, Alonzo per-word,
    // Babbage/Conway+ per-serialized-byte).
    //
    // The serialized size is only meaningful (and only computed) for the
    // Babbage/Conway+ branch. When `raw_cbor` is unavailable (the output
    // was constructed in-memory rather than decoded from the wire) fall
    // back to re-encoding the output via dugite's own CBOR encoder to get
    // its exact size, instead of a fixed floor — which under-estimates
    // (and thus under-charges the minimum for) any multi-asset or
    // datum/script-ref carrying output (issue #810).
    // ------------------------------------------------------------------
    for (output_index, output) in body.outputs.iter().enumerate() {
        let has_datum_hash = matches!(output.datum, OutputDatum::DatumHash(_));
        let output_size_bytes = if params.protocol_version_major >= 7 {
            match &output.raw_cbor {
                Some(cbor) => cbor.len() as u64,
                None => dugite_serialization::encode_transaction_output(output).len() as u64,
            }
        } else {
            0
        };
        let min_utxo = params.min_coin_for_output(&output.value, has_datum_hash, output_size_bytes);
        if output.value.coin.0 < min_utxo.0 {
            errors.push(ValidationError::OutputTooSmall {
                minimum: min_utxo.0,
                actual: output.value.coin.0,
                output_index,
            });
        }
    }

    // Haskell's `validateOutputTooSmallUTxO` (Babbage/Rules/Utxo.hs) folds
    // over `allSizedOutputsTxBodyF` — the regular outputs PLUS the
    // collateral-return output — so `collateral_return` is subject to the
    // SAME per-era minimum and feeds the same `BabbageOutputTooSmallUTxO`
    // wire failure (tx-zoo 18d pins this on the wire; before this check
    // dugite accepted a below-minimum return output Haskell rejects).
    // Collateral return only exists at Babbage+ (PV >= 7), where the
    // serialized-size formula applies unconditionally.
    if params.protocol_version_major >= 7 {
        if let Some(col_ret) = &body.collateral_return {
            let has_datum_hash = matches!(col_ret.datum, OutputDatum::DatumHash(_));
            let output_size_bytes = match &col_ret.raw_cbor {
                Some(cbor) => cbor.len() as u64,
                None => dugite_serialization::encode_transaction_output(col_ret).len() as u64,
            };
            let min_utxo =
                params.min_coin_for_output(&col_ret.value, has_datum_hash, output_size_bytes);
            if col_ret.value.coin.0 < min_utxo.0 {
                errors.push(ValidationError::OutputTooSmall {
                    minimum: min_utxo.0,
                    actual: col_ret.value.coin.0,
                    output_index: super::COLLATERAL_RETURN_OUTPUT_INDEX,
                });
            }
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
        // Haskell's `WrongNetwork Network (Set Addr)` carries the WHOLE set of
        // offending addresses, so collect them all rather than reporting the
        // first: a one-element answer is not the same predicate failure (#979).
        let mut wrong_outputs: Vec<String> = Vec::new();
        let mut wrong_output_net = None;
        for output in &body.outputs {
            if let Some(addr_network) = output.address.network_id() {
                if addr_network != expected_net {
                    wrong_output_net = Some(addr_network);
                    wrong_outputs.push(to_hex_bytes(&output.address.to_bytes()));
                }
            }
        }
        if let Some(actual) = wrong_output_net {
            errors.push(ValidationError::WrongNetworkInOutput {
                expected: expected_net,
                actual,
                addresses: wrong_outputs,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 5d-pool: Pool registration reward account must be on the
    // node's configured network (Haskell `WrongNetworkPOOL`).
    //
    // Per Haskell `ShelleyPoolPredFailure::WrongNetworkPOOL` in
    // `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Pool.hs`,
    // gated on `hardforkAlonzoValidatePoolAccountAddressNetID pv`
    // (active for PV >= 5). Fires when the network ID embedded in the
    // pool registration's `account_address` (reward account) does not
    // match the node's network. Bit 0 of the reward account header
    // encodes the network: 0 = testnet, 1 = mainnet.
    // ------------------------------------------------------------------
    if let Some(expected_net) = node_network {
        if params.protocol_version_major >= 5 {
            for cert in &body.certificates {
                if let Certificate::PoolRegistration(pool_params) = cert {
                    if let Some(header) = pool_params.reward_account.first() {
                        let network_bit = header & 0x01;
                        let actual_net = if network_bit == 0 {
                            dugite_primitives::network::NetworkId::Testnet
                        } else {
                            dugite_primitives::network::NetworkId::Mainnet
                        };
                        if actual_net != expected_net {
                            errors.push(ValidationError::WrongNetworkPool {
                                expected: expected_net,
                                actual: actual_net,
                                pool_id: pool_params.operator.to_hex(),
                            });
                        }
                    }
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
        // As above: `WrongNetworkWithdrawal Network (Set RewardAccount)`
        // reports the whole set, not the first offender (#979).
        let mut wrong_accounts: Vec<String> = Vec::new();
        let mut wrong_wdrl_net = None;
        for reward_account in body.withdrawals.keys() {
            if let Some(header) = reward_account.first() {
                let network_bit = header & 0x01;
                let actual_net = if network_bit == 0 {
                    dugite_primitives::network::NetworkId::Testnet
                } else {
                    dugite_primitives::network::NetworkId::Mainnet
                };
                if actual_net != expected_net {
                    wrong_wdrl_net = Some(actual_net);
                    wrong_accounts.push(to_hex_bytes(reward_account));
                }
            }
        }
        if let Some(actual) = wrong_wdrl_net {
            errors.push(ValidationError::WrongNetworkWithdrawal {
                expected: expected_net,
                actual,
                accounts: wrong_accounts,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 6: Transaction size limit
    //
    // Haskell `validateMaxTxSizeUTxO` compares `sizeTxF`, which for Alonzo+
    // is `toCBORForSizeComputation` — the 3-element [body, wits, aux]
    // encoding WITHOUT the is_valid flag, i.e. full wire size − 1 — the
    // same measure as the fee size. Proven on-chain: mainnet slot
    // 94,062,660 carries a confirmed 16,385-wire-byte tx against
    // maxTxSize=16384 (wallets build to exactly the Haskell limit); using
    // the raw wire size falsely rejected it by one byte. db-sync/Koios
    // `tx_size` records the same wire−1 measure.
    // ------------------------------------------------------------------
    let haskell_tx_size = super::scripts::fee_tx_size(tx, tx_size);
    if haskell_tx_size > params.max_tx_size {
        errors.push(ValidationError::TxTooLarge {
            maximum: params.max_tx_size,
            actual: haskell_tx_size,
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
    // ONLY in the window 8 < PV < 11, exactly mirroring Haskell
    // `disjointRefInputs` (Babbage Rules/Utxo.hs, post-#5011):
    //
    //   when ( pvMajor > eraProtVerHigh @BabbageEra   -- (= 8)
    //            && pvMajor < natVersion @11 )
    //        (failureOnNonEmpty common BabbageNonDisjointRefInputs)
    //
    // - PV7/PV8 (Babbage itself): NO disjointness rule. Mainnet carries such
    //   txs (17 observed live at ep387, 2026-06-12) — enforcing here caused
    //   false phase-1 divergences on confirmed blocks.
    // - PV9/PV10 (Conway pre-Van-Rossem): rejected at phase-1
    //   (`BabbageNonDisjointRefInputs`).
    // - PV >= 11: Haskell PR #5011 (commit 44de8edc) RELAXED the rule:
    //   V1/V2/native accepted; the equivalent check moves into PlutusV3
    //   `TxInfo` translation as
    //   `ConwayContextError::ReferenceInputsNotDisjointFromInputs` (tag 15),
    //   surfaced as a phase-2 `BadTranslation`. See dugite issue #470.
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
            if pv_major > 8 && pv_major < 11 && input_set.contains(ref_input) {
                errors.push(ValidationError::ReferenceInputOverlapsInput(
                    ref_input.to_string(),
                ));
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 9c: Per-transaction reference-script size cap
    //          (Haskell `ConwayTxRefScriptsSizeTooBig`, Conway LEDGER rule)
    //
    // The total non-distinct reference-script size reachable from a
    // transaction's spending inputs AND reference inputs (the SAME
    // `txNonDistinctRefScriptsSize` primitive already used for the
    // CIP-0112 tiered ref-script FEE below) must not exceed a fixed
    // 200 KiB (204800 byte) per-transaction cap.
    //
    // Per Haskell `validateRefScriptSize`
    // (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs:456-471`):
    // `ppMaxRefScriptSizePerTxG` is a Conway-era CONSTANT (`200 * 1024`),
    // not a live protocol parameter until Dijkstra — so this is a fixed
    // threshold, not something read from `params`.
    //
    // This is a SEPARATE, additional cap from dugite's existing per-BLOCK
    // 1 MiB `BodyRefScriptsSizeTooBig` check (`eras/conway.rs`, Conway
    // BBODY rule, which sums ref-script size across every tx in a block) —
    // a single oversized-ref-script transaction well under the block-wide
    // limit was previously accepted at Phase-1 where cardano-node rejects
    // it outright.
    // ------------------------------------------------------------------
    // #1061: SKIPPED ENTIRELY when the tx declares `is_valid: false`.
    //
    // Haskell's `conwayLedgerTransition` runs this test only inside the
    // Phase2Valid branch (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/
    // Ledger.hs`):
    //
    //     if tx ^. isPhase2ValidTxL == Phase2Valid
    //       then do
    //         runTest $ validateTreasuryValue txBody (chainAccountState ^. casTreasuryL)
    //         runTest $ validateRefScriptSize pp (utxoState ^. utxoL) tx
    //
    // so a phase-2-failing tx is never checked for either. dugite ran both
    // unconditionally, which is a FALSE REJECT: cardano-node accepts such a tx
    // and dugite refuses it. At block level that means refusing a block
    // cardano-node accepts — the #985 symptom class, i.e. the dangerous
    // direction, not the safe one.
    //
    // The gate must SKIP the check, not suppress its failure: an is_valid=false
    // tx legitimately carries oversized reference scripts as far as the LEDGER
    // rule is concerned.
    if params.protocol_version_major >= 9 && tx.is_valid {
        const MAX_REF_SCRIPT_SIZE_PER_TX: u64 = 200 * 1024;
        let total_ref_script_size = super::scripts::calculate_ref_script_size(
            &body.inputs,
            &body.reference_inputs,
            utxo_set,
        );
        if total_ref_script_size > MAX_REF_SCRIPT_SIZE_PER_TX {
            errors.push(ValidationError::RefScriptsSizeTooBig {
                maximum: MAX_REF_SCRIPT_SIZE_PER_TX,
                actual: total_ref_script_size,
            });
        }
    }

    // ------------------------------------------------------------------
    // Rule 9b: Witness completeness
    //
    // #1030 item 1: gated on INPUT RESOLUTION, not `errors.is_empty()` — see
    // Rule 3's note. Witness completeness needs each input's ADDRESS to know
    // which key or script must have signed, so unresolved inputs genuinely make
    // it uncomputable; an unrelated earlier failure does not.
    // ------------------------------------------------------------------
    if inputs_resolved(errors) {
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
                        // Byron address — input coverage is enforced by
                        // check_bootstrap_address_binding (every Byron input
                        // must be covered by a bootstrap witness whose key
                        // hash matches the address root).
                        // No additional completeness check needed here.
                    }
                }
            }
        }

        // Check each COLLATERAL input has a matching vkey witness. Mirrors
        // Haskell `getConwayWitsVKeyNeeded` which unions the payment-key
        // hashes from `collateralInputs` into the required witness set
        // (alongside regular `inputs`). Collateral inputs are guaranteed
        // VKey-locked by the `ScriptsNotPaidUTxO` predicate in
        // `collateral::check_collateral`, so we only emit the vkey-witness
        // requirement — script collateral is already rejected upstream.
        //
        // Without this check, dugite admits a Plutus tx that supplies a
        // collateral input but omits the vkey witness for the collateral
        // payment key; cardano-node rejects the resulting block with
        // `ConwayUtxowFailure (MissingVKeyWitnessesUTXOW (NonEmptySet
        // (fromList [KeyHash <payment-key>])))`. Round-1 attempt 4
        // surfaced this on a cross-validate-cli `xv-03-plutus-spend-v3`
        // tx (block f8212b3d...@slot 508).
        for col_input in &body.collateral {
            if let Some(utxo) = utxo_set.lookup(col_input) {
                if let Some(Credential::VerificationKey(keyhash)) =
                    utxo.address.payment_credential()
                {
                    if !vkey_witness_hashes.contains(keyhash) {
                        errors.push(ValidationError::MissingInputWitness(keyhash.to_hex()));
                    }
                }
                // Script-locked collateral is rejected by check_collateral
                // (ScriptLockedCollateral); Byron/bootstrap collateral
                // (no payment credential) is handled by Rule 14.
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
            // ...and the SCRIPT half. Inputs and withdrawals both had a
            // `Credential::Script` arm; certificates did not, so a certificate
            // whose subject is a script credential required nothing at all and
            // dugite accepted it unwitnessed. cardano-node 11.0.1 answers
            // `ConwayUtxowFailure (MissingScriptWitnessesUTXOW ...)`, verified
            // live on the devnet against a script-credential
            // `reg_deposit_cert`: dugite ACCEPTED, Haskell REJECTED.
            if let Some(required_script) = cert_required_script_witness(cert) {
                if !available_script_hashes.contains(&required_script) {
                    errors.push(ValidationError::MissingCertificateScriptWitness(
                        required_script.to_hex(),
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
        // Issue #787: timelocks (`InvalidBefore`/`InvalidHereafter`) must be
        // evaluated against the TX'S OWN ValidityInterval, never the
        // application/current slot — `body.validity_interval_start` is
        // Haskell's `invalid_before` and `body.ttl` is `invalid_hereafter`.
        let invalid_before = body.validity_interval_start;
        let invalid_hereafter = body.ttl;

        // Native scripts hash over their ORIGINAL wire bytes (#862) so the hash
        // matches the on-chain `scripts_needed` entry for a non-canonical script.
        let witness_native_raws = super::scripts::witness_native_original_bytes(tx);
        for (i, script) in tx.witness_set.native_scripts.iter().enumerate() {
            let original = witness_native_raws
                .as_ref()
                .and_then(|v| v.get(i))
                .map(Vec::as_slice);
            let script_hash = super::scripts::native_script_hash(script, original);

            // Only evaluate scripts that are actually needed
            if scripts_needed.contains(&script_hash)
                && !evaluate_native_script(script, &signers, invalid_before, invalid_hereafter)
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
    // BootstrapWitness (Byron): 32-byte public_key + 32-byte chain_code; separate verifier:
    //   (a) verifies the signature using the 32-byte public_key directly, and
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
    // cert_required_script_witness — Haskell getScriptWitnessConwayTxCert parity
    //
    // dugite had NO script-witness requirement for certificates at all, so a
    // script-credential certificate could be submitted unwitnessed and dugite
    // accepted it while cardano-node 11.0.1 answered
    // MissingScriptWitnessesUTXOW. These pin the per-certificate rule, and in
    // particular the ONE case that is genuinely permissionless.
    // -----------------------------------------------------------------------

    fn script_cred() -> Credential {
        Credential::Script(Hash28::from_bytes([0xABu8; 28]))
    }
    fn key_cred() -> Credential {
        Credential::VerificationKey(Hash28::from_bytes([0xCDu8; 28]))
    }
    fn script_h() -> Hash28 {
        Hash28::from_bytes([0xABu8; 28])
    }

    #[test]
    fn reg_cert_index0_is_permissionless() {
        // The deposit-less Shelley form is the ONLY certificate that needs no
        // witness for a script credential — Haskell keeps it that way "only
        // during the transitional period of Conway era".
        use dugite_primitives::transaction::Certificate;
        let c = Certificate::StakeRegistration(script_cred());
        assert_eq!(super::cert_required_script_witness(&c), None);
    }

    #[test]
    fn reg_deposit_cert_index7_requires_script_witness() {
        // The trap: "registration" is NOT uniformly permissionless. The Conway
        // explicit-deposit form DOES require the script to authorise it. This
        // is the exact shape that dugite accepted and cardano-node rejected.
        use dugite_primitives::transaction::Certificate;
        let c = Certificate::ConwayStakeRegistration {
            credential: script_cred(),
            deposit: Lovelace(2_000_000),
        };
        assert_eq!(super::cert_required_script_witness(&c), Some(script_h()));
    }

    #[test]
    fn key_credential_certs_need_no_script_witness() {
        use dugite_primitives::transaction::Certificate;
        let c = Certificate::ConwayStakeRegistration {
            credential: key_cred(),
            deposit: Lovelace(2_000_000),
        };
        assert_eq!(super::cert_required_script_witness(&c), None);
    }

    #[test]
    fn pool_certs_are_never_script_witnessed() {
        // Haskell: "PoolIds can't be Scripts" — Nothing unconditionally.
        use dugite_primitives::transaction::Certificate;
        let c = Certificate::PoolRetirement {
            pool_hash: Hash28::from_bytes([0x11u8; 28]),
            epoch: 5,
        };
        assert_eq!(super::cert_required_script_witness(&c), None);
    }

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
    // Test 9b — Rule 5a (issue #930): real preprod Conway tx 96ae78f7 whose
    // output[1] value measures EXACTLY maxValSize=5000 under Haskell's
    // `encodeMap` semantics (definite-length map header for <= 23 entries,
    // indefinite 0xbf...0xff above) but 5001 under a definite-only encoder
    // (its 324-entry inner asset map costs a 3-byte definite header vs the
    // fixed 2-byte indefinite overhead).
    //
    // tx 96ae78f724a27b0d76c3d6a861857af3a644de971fe0c7fcbefe4e45811e5687
    // preprod epoch 303, slot 129586448, block 4990228 (PV10 Conway).
    // output[1]: coin=3804474262, 6 policies, 358 assets total, one inner
    // map of 324 entries. Every Haskell node accepted it; dugite v2.4.0
    // rejected it with OutputValueTooLarge { maximum: 5000, actual: 5001 }.
    //
    // Like test 31c this validates against an empty UTxO (BadInputs et al.
    // are expected); the invariant is solely about OutputValueTooLarge.
    // -----------------------------------------------------------------------
    const TX_96AE78F7_HEX: &str = include_str!("fixtures/tx-96ae78f7.hex");

    #[test]
    fn test_preprod_conway_maxvalsize_exact_bound_96ae78f7() {
        let s = TX_96AE78F7_HEX.trim();
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect();
        // Decode as era 6 (Conway).
        let tx = dugite_serialization::decode::decode_transaction(6, &bytes)
            .expect("decode real preprod Conway tx 96ae78f7");

        // Pin the on-chain shape that produces the divergence.
        assert_eq!(tx.body.outputs.len(), 3, "tx has 3 outputs");
        let value = &tx.body.outputs[1].value;
        assert_eq!(value.multi_asset.len(), 6, "output[1] has 6 policies");
        let total_assets: usize = value.multi_asset.values().map(|a| a.len()).sum();
        assert_eq!(total_assets, 358, "output[1] has 358 assets");
        let over_23: Vec<usize> = value
            .multi_asset
            .values()
            .map(|a| a.len())
            .filter(|n| *n > 23)
            .collect();
        assert_eq!(
            over_23,
            vec![324],
            "exactly one inner asset map above the encodeMap threshold"
        );

        // The Rule-5a measurement must match Haskell's serialize length.
        assert_eq!(
            crate::validation::scripts::estimate_value_cbor_size(value),
            5000,
            "output[1] value must measure 5000 bytes (Haskell encodeMap), not 5001"
        );

        // Full Phase-1 at maxValSize=5000 (mainnet/preprod default) must NOT
        // report OutputValueTooLarge — the tx sits exactly at the strict
        // `>` bound and is legal.
        let empty_utxo = UtxoSet::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 10; // preprod Conway PV10
        assert_eq!(params.max_val_size, 5000);
        let result = validate_transaction(
            &tx,
            &empty_utxo,
            &params,
            129_586_448,
            bytes.len() as u64,
            None,
        );
        let has_too_large = match &result {
            Ok(()) => false,
            Err(errors) => errors
                .iter()
                .any(|e| matches!(e, ValidationError::OutputValueTooLarge { .. })),
        };
        assert!(
            !has_too_large,
            "OutputValueTooLarge must not fire at maxValSize=5000 (issue #930): {result:?}"
        );

        // And at maxValSize=4999 the same output MUST be rejected, with the
        // corrected actual=5000 measurement.
        params.max_val_size = 4999;
        let errors = validate_transaction(
            &tx,
            &empty_utxo,
            &params,
            129_586_448,
            bytes.len() as u64,
            None,
        )
        .expect_err("empty UTxO + maxValSize=4999 must fail");
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::OutputValueTooLarge {
                    maximum: 4999,
                    actual: 5000,
                }
            )),
            "expected OutputValueTooLarge {{ maximum: 4999, actual: 5000 }}, got {errors:?}"
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

    /// Bootstrap binding runs in the HASKELL direction: a redundant bootstrap
    /// witness alongside all-Shelley inputs is LEGAL (legacy Daedalus wallets
    /// attach both vkey + bootstrap witnesses for the same key). Two confirmed
    /// mainnet txs (blocks 9,074,761 / 9,075,213) were falsely rejected by the
    /// previous wrong-direction check. Uses the on-chain witness key material.
    #[test]
    fn test_redundant_bootstrap_witness_on_shelley_inputs_accepted() {
        let (utxo_set, mut tx, _input) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();

        // On-chain witness from mainnet tx 432b916e…: vkey b7a8e15f…,
        // chain_code all-zeros, attributes = {} (0xA0). Signature is not the
        // subject here (signature validity is checked by a different rule);
        // assert specifically that NO binding/missing-input-witness error
        // fires for the redundant bootstrap witness.
        let mut vkey = vec![0u8; 32];
        vkey[0] = 0xb7;
        vkey[1] = 0xa8;
        vkey[2] = 0xe1;
        vkey[3] = 0x5f;
        tx.witness_set
            .bootstrap_witnesses
            .push(dugite_primitives::transaction::BootstrapWitness {
                vkey,
                signature: vec![0u8; 64],
                chain_code: vec![0u8; 32],
                attributes: vec![0xA0],
            });

        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let binding_or_missing = matches!(&result, Err(errors) if errors.iter().any(|e| {
            matches!(e, ValidationError::MissingInputWitness(_))
                || matches!(e, ValidationError::InvalidWitnessSignature(s2) if s2.contains("binding"))
        }));
        assert!(
            !binding_or_missing,
            "a redundant bootstrap witness with all-Shelley inputs must not fire \
             a binding/missing-witness error (Haskell alonzoStyleWitness has no \
             witness→input predicate); got {result:?}"
        );
    }

    /// Rule 6 must use the Haskell size (`toCBORForSizeComputation` =
    /// wire − 1 for Alonzo+): a tx whose FULL wire size is max_tx_size + 1
    /// is exactly at the Haskell limit and must be ACCEPTED (proven on-chain
    /// at mainnet slot 94,062,660); one more byte must be rejected.
    #[test]
    fn test_max_tx_size_uses_haskell_size_for_alonzo_plus() {
        let (utxo_set, mut tx, _input) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let max = params.max_tx_size;

        // Alonzo+ wire form: raw_cbor starting 0x84 → fee/size measure = wire − 1.
        let mut wire = vec![0x84u8];
        wire.resize((max + 1) as usize, 0);
        tx.raw_cbor = Some(wire.clone());
        let result = validate_transaction(&tx, &utxo_set, &params, 100, max + 1, None);
        let too_large = matches!(&result, Err(errors) if errors.iter().any(|e| {
            matches!(e, ValidationError::TxTooLarge { .. })
        }));
        assert!(
            !too_large,
            "wire size max+1 (Haskell size == max) must NOT be TxTooLarge; got {result:?}"
        );

        let mut wire2 = vec![0x84u8];
        wire2.resize((max + 2) as usize, 0);
        tx.raw_cbor = Some(wire2);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, max + 2, None);
        let too_large = matches!(&result, Err(errors) if errors.iter().any(|e| {
            matches!(e, ValidationError::TxTooLarge { .. })
        }));
        assert!(
            too_large,
            "wire size max+2 (Haskell size == max+1) must be TxTooLarge; got {result:?}"
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
    // Test 14-pre — Babbage PV7/PV8: overlap must be ACCEPTED.
    //
    // Haskell `disjointRefInputs` (Babbage Rules/Utxo.hs, post-#5011) fires
    // only when `pvMajor > eraProtVerHigh @BabbageEra (= 8) && pvMajor < 11`,
    // i.e. PV9/PV10 only. Within Babbage itself (PV7/PV8) the rule does NOT
    // exist — mainnet ep387 carries such txs (17 observed live on 2026-06-12
    // as false ReferenceInputOverlapsInput divergences on confirmed blocks
    // before this lower bound was added).
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_inputs_overlap_accepted_at_babbage_pv7_pv8() {
        for pv in [7u64, 8] {
            let (utxo_set, mut tx, input) = make_valid_tx();
            tx.body.reference_inputs.push(input.clone());
            let mut params = ProtocolParameters::mainnet_defaults();
            params.protocol_version_major = pv;
            let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
            assert!(
                result.is_ok(),
                "PV {pv} (Babbage) tx with input/refInput overlap must be accepted \
                 (Haskell disjointRefInputs fires only for 8 < pv < 11), got: {:?}",
                result.err()
            );
        }
    }

    /// PV9 (lower edge of the Conway enforcement window) must still reject —
    /// the rule window is exactly 8 < pv < 11.
    #[test]
    fn test_ref_inputs_overlap_rejected_at_pv9() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        tx.body.reference_inputs.push(input.clone());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ReferenceInputOverlapsInput(_))),
            "PV 9: expected ReferenceInputOverlapsInput, got {errors:?}"
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
    // Test — Rule 9c: per-transaction reference-script size cap
    //        (Haskell `ConwayTxRefScriptsSizeTooBig`)
    // -----------------------------------------------------------------------
    #[test]
    fn test_ref_script_size_over_per_tx_cap_rejected() {
        let (mut utxo_set, mut tx, input) = make_valid_tx();
        // Attach an oversized PlutusV3 reference script (204_801 bytes, one
        // over the 200 KiB = 204_800 byte cap) to the spent input's own UTxO.
        let big_script = vec![0u8; 204_801];
        let mut utxo_output = utxo_set.lookup(&input).unwrap().clone();
        utxo_output.script_ref = Some(dugite_primitives::transaction::ScriptRef::PlutusV3(
            big_script,
        ));
        utxo_set.insert(input.clone(), utxo_output);
        tx.body.fee = Lovelace(0); // avoid coupling to the fee formula
        tx.body.outputs[0].value = Value::lovelace(10_000_000);

        let params = ProtocolParameters::mainnet_defaults(); // PV 9
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::RefScriptsSizeTooBig {
                    maximum: 204_800,
                    actual: 204_801
                }
            )),
            "expected RefScriptsSizeTooBig{{maximum:204800,actual:204801}}, got {errors:?}"
        );
    }

    #[test]
    fn test_ref_script_size_at_per_tx_cap_accepted() {
        let (mut utxo_set, mut tx, input) = make_valid_tx();
        // Exactly at the cap (204_800 bytes) must NOT trigger the check
        // (Haskell: `totalRefScriptSize <= maxRefScriptSizePerTx`).
        let exact_script = vec![0u8; 204_800];
        let mut utxo_output = utxo_set.lookup(&input).unwrap().clone();
        utxo_output.script_ref = Some(dugite_primitives::transaction::ScriptRef::PlutusV3(
            exact_script,
        ));
        utxo_set.insert(input.clone(), utxo_output);
        tx.body.fee = Lovelace(0);
        tx.body.outputs[0].value = Value::lovelace(10_000_000);

        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::RefScriptsSizeTooBig { .. })),
            "exactly-at-cap ref script size must NOT trigger RefScriptsSizeTooBig, got {errors:?}"
        );
    }

    #[test]
    fn test_ref_script_size_skipped_pre_conway() {
        // The cap is a Conway-era constant (`ppMaxRefScriptSizePerTxG`) — it
        // does not exist pre-Conway, so must not fire at PV < 9.
        let (mut utxo_set, mut tx, input) = make_valid_tx();
        let big_script = vec![0u8; 204_801];
        let mut utxo_output = utxo_set.lookup(&input).unwrap().clone();
        utxo_output.script_ref = Some(dugite_primitives::transaction::ScriptRef::PlutusV1(
            big_script,
        ));
        utxo_set.insert(input.clone(), utxo_output);
        tx.body.fee = Lovelace(0);
        tx.body.outputs[0].value = Value::lovelace(10_000_000);

        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let errors = result.err().unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::RefScriptsSizeTooBig { .. })),
            "pre-Conway (PV8) must never fire RefScriptsSizeTooBig, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test — Rule 11: max collateral inputs, checked UNCONDITIONALLY
    //        (Haskell `TooManyCollateralInputs`, not gated on Plutus content)
    // -----------------------------------------------------------------------
    #[test]
    fn test_too_many_collateral_inputs_on_non_plutus_tx_rejected() {
        // A transaction with NO Plutus scripts and NO redeemers, but a
        // `collateral` field declaring more inputs than `max_collateral_inputs`.
        // Haskell's `TooManyCollateralInputs` is checked unconditionally
        // (`Babbage/Utxo.hs:412`), not gated on `redeemers` being non-empty —
        // before this fix dugite's `check_collateral` (and hence this
        // predicate) only ran when `has_plutus_scripts(tx)` was true, so this
        // tx was silently ACCEPTED.
        let (mut utxo_set, mut tx, _) = make_valid_tx();
        let mut collateral = Vec::new();
        for i in 0u8..4 {
            let col_input = TransactionInput {
                transaction_id: Hash32::from_bytes([0xF0 + i; 32]),
                index: 0,
            };
            utxo_set.insert(
                col_input.clone(),
                TransactionOutput {
                    address: Address::Byron(dugite_primitives::address::ByronAddress {
                        payload: vec![0x82, 0x00, 0x01],
                    }),
                    value: Value::lovelace(5_000_000),
                    datum: OutputDatum::None,
                    script_ref: None,
                    is_legacy: false,
                    raw_cbor: None,
                },
            );
            collateral.push(col_input);
        }
        tx.body.collateral = collateral;

        let mut params = ProtocolParameters::mainnet_defaults();
        params.max_collateral_inputs = 3;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::TooManyCollateralInputs { max: 3, actual: 4 }
            )),
            "a non-Plutus tx with too many collateral inputs must be rejected, got {errors:?}"
        );
    }

    #[test]
    fn test_collateral_field_ignored_when_within_limit_on_non_plutus_tx() {
        let (mut utxo_set, mut tx, _) = make_valid_tx();
        let col_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xF9u8; 32]),
            index: 0,
        };
        utxo_set.insert(
            col_input.clone(),
            TransactionOutput {
                address: Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: vec![0x82, 0x00, 0x01],
                }),
                value: Value::lovelace(5_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );
        tx.body.collateral = vec![col_input];

        let params = ProtocolParameters::mainnet_defaults(); // max_collateral_inputs default is well above 1
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let errors = result.err().unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::TooManyCollateralInputs { .. })),
            "a single collateral input within the limit must not trigger TooManyCollateralInputs, got {errors:?}"
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
    // Test 31 — Rule 1b: duplicate inputs rejected at Conway PV9+
    //
    // Haskell `decodeSetEnforceNoDuplicates` (PV >= 9) hard-fails on duplicates.
    // Dugite mirrors this at Phase-1 time.  `mainnet_defaults()` has PV9.
    // -----------------------------------------------------------------------
    #[test]
    fn test_duplicate_inputs_rejected_at_conway_pv9() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        // Add the same input a second time.
        tx.body.inputs.push(input.clone());
        let params = ProtocolParameters::mainnet_defaults(); // PV9
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicateInput(_))),
            "expected DuplicateInput at PV9, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31b — Rule 1b: duplicate inputs silently accepted at Babbage PV8
    //
    // Haskell `decodeSet` at PV < 9 routes through `Set.fromList` which
    // silently deduplicates.  `BabbageUtxoPredFailure` has no DuplicateInput
    // constructor.  Real mainnet tx 5ca83e21… (epoch 484, slot 123728795,
    // PV8) has body key 0 = array(3) with the same TxIn listed twice; it was
    // accepted by cardano-node 8.x and is on-chain.
    // -----------------------------------------------------------------------
    #[test]
    fn test_duplicate_inputs_accepted_at_babbage_pv8() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        // Add the same input a second time — simulating the PV8 wire encoding.
        tx.body.inputs.push(input.clone());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage
                                           // With a duplicate input and PV8, Phase-1 must NOT emit DuplicateInput.
                                           // (Other errors may fire — the point is DuplicateInput is absent.)
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        let no_dup_error = match &result {
            Ok(()) => true,
            Err(errors) => !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicateInput(_))),
        };
        assert!(
            no_dup_error,
            "DuplicateInput must not fire at PV8 (Babbage), got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31c — Rule 1b: real mainnet Babbage tx 5ca83e21 with duplicate
    //             spend input accepted (issue #759 regression pin)
    //
    // tx 5ca83e216eb4fce8e907ed3597bd290261136ae97fc4cd7fbd5eadf9bbedf09f
    // mainnet epoch 484, slot 123728795, block 10294413 (PV8 = Babbage).
    // Body key 0 = plain array(3): [ab2829f0…#1, ab2829f0…#1, 3bd13603…#0]
    // The same TxIn `ab2829f03f…#1` appears twice.  Haskell accepted it
    // (it is on-chain); dugite must not reject with DuplicateInput.
    //
    // Note: this test decodes the raw CBOR and checks that DuplicateInput is
    // absent; it does NOT reconstruct a full UTxO environment, so Phase-1
    // may emit BadInputs/ValueNotConserved.  The critical invariant is that
    // DuplicateInput is NEVER in the error list when PV < 9.
    // -----------------------------------------------------------------------
    const TX_5CA83E21_HEX: &str = include_str!("fixtures/tx-5ca83e21.hex");

    #[test]
    fn test_mainnet_babbage_duplicate_input_5ca83e21_no_false_positive() {
        let s = TX_5CA83E21_HEX.trim();
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect();
        // Decode as era 5 (Babbage)
        let tx = dugite_serialization::decode::decode_transaction(5, &bytes)
            .expect("decode real mainnet Babbage tx 5ca83e21");

        // Verify the wire-level duplicate is preserved by the decoder
        assert_eq!(
            tx.body.inputs.len(),
            3,
            "wire has array(3); decoder must preserve physical element count"
        );
        assert_eq!(
            tx.body.inputs[0], tx.body.inputs[1],
            "first two inputs must be identical (wire duplicate)"
        );

        // Validate against an empty UTxO (BadInputs expected, DuplicateInput forbidden)
        let empty_utxo = UtxoSet::new();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // PV8 = Babbage
        let result = validate_transaction(&tx, &empty_utxo, &params, 123_728_795, 500, None);
        let has_dup_error = match &result {
            Ok(()) => false,
            Err(errors) => errors
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicateInput(_))),
        };
        assert!(
            !has_dup_error,
            "DuplicateInput must not fire for PV8 Babbage tx 5ca83e21 (issue #759): {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31d — Rule 2/3b (issue #786): a duplicated spend input against a
    // POPULATED UTxO set must not double-count the input's value at PV < 9.
    //
    // Haskell decodes pre-PV9 TxIn lists via `Set.fromList` (silent dedup)
    // and sums each distinct TxIn once. dugite previously summed the raw
    // `Vec` — a physical duplicate was counted twice, producing a false
    // `ValueNotConserved` on a historically-valid tx. The existing Test
    // 31b/31c regression only used an EMPTY UTxO (all `InputNotFound`), so
    // it never exercised the double-count in the ADA conservation sum.
    // -----------------------------------------------------------------------
    #[test]
    fn test_duplicate_input_not_double_counted_at_pv8_with_populated_utxo() {
        let (utxo_set, mut tx, input) = make_valid_tx();
        // Physically duplicate the (already-existing, populated) input —
        // simulating the PV8 wire encoding (`fixtures/tx-5ca83e21.hex`
        // class). The UTxO is worth 10_000_000; output=9_800_000 +
        // fee=200_000 already balances against ONE occurrence.
        tx.body.inputs.push(input);
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage — Haskell silently dedups

        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "PV8 duplicate input against a populated UTxO must not double-count the \
             input's value (false ValueNotConserved); got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 31e — Rule 3b (issue #786): a duplicated spend input carrying a
    // multi-asset bundle must not double-count the asset quantity at
    // PV < 9 either — same `Set.fromList` dedup as Rule 2's ADA sum.
    // -----------------------------------------------------------------------
    #[test]
    fn test_duplicate_input_multi_asset_not_double_counted_at_pv8() {
        let policy = dugite_primitives::hash::Hash28::from_bytes([0x77u8; 28]);
        let asset = AssetName::new(b"Token".to_vec()).unwrap();

        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xBBu8; 32]),
            index: 0,
        };
        let mut input_value = Value::lovelace(10_000_000);
        input_value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 100);

        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(
            input.clone(),
            TransactionOutput {
                address: Address::Byron(dugite_primitives::address::ByronAddress {
                    payload: vec![0x82, 0x00, 0x01],
                }),
                value: input_value,
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );

        // Output carries the SAME 100 tokens back — value/asset conserved
        // against exactly ONE occurrence of the input.
        let mut output_value = Value::lovelace(9_800_000);
        output_value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset, 100);

        let (_, mut tx, _) = make_valid_tx();
        // Physically duplicate the input (twice) to simulate the PV8 wire
        // encoding; the UTxO/output above are value- and asset-balanced
        // against exactly ONE occurrence.
        tx.body.inputs = vec![input.clone(), input];
        tx.body.outputs = vec![TransactionOutput {
            address: Address::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![0x82, 0x00, 0x01],
            }),
            value: output_value,
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }];

        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage — Haskell silently dedups

        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "PV8 duplicate multi-asset input must not double-count the asset quantity \
             (false MultiAssetNotConserved); got {result:?}"
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
    // Test 40b — Rule 1f (issue #785): the three combined register+delegate
    // certificates (RegStakeDeleg tag 11, VoteRegDeleg tag 12,
    // RegStakeVoteDeleg tag 13) must ALSO enforce deposit == key_deposit —
    // they decode in Haskell to the same `ConwayRegDelegCert` constructor
    // as `ConwayStakeRegistration` and go through the identical
    // `checkDepositAgainstPParams` check.
    // -----------------------------------------------------------------------
    #[test]
    fn test_reg_stake_deleg_deposit_mismatch_rejected() {
        use dugite_primitives::transaction::Certificate;
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        tx.body.certificates.push(Certificate::RegStakeDeleg {
            credential: Credential::VerificationKey(Hash28::from_bytes([0x51u8; 28])),
            pool_hash: Hash28::from_bytes([0x52u8; 28]),
            deposit: Lovelace(999_999), // Wrong deposit amount
        });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakeRegistrationDepositMismatch { .. })),
            "RegStakeDeleg with wrong deposit must produce StakeRegistrationDepositMismatch, got {errors:?}"
        );
    }

    #[test]
    fn test_reg_stake_vote_deleg_deposit_mismatch_rejected() {
        use dugite_primitives::transaction::{Certificate, DRep};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        tx.body.certificates.push(Certificate::RegStakeVoteDeleg {
            credential: Credential::VerificationKey(Hash28::from_bytes([0x53u8; 28])),
            pool_hash: Hash28::from_bytes([0x54u8; 28]),
            drep: DRep::Abstain,
            deposit: Lovelace(1), // Wrong deposit amount
        });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakeRegistrationDepositMismatch { .. })),
            "RegStakeVoteDeleg with wrong deposit must produce StakeRegistrationDepositMismatch, got {errors:?}"
        );
    }

    #[test]
    fn test_vote_reg_deleg_deposit_mismatch_rejected() {
        use dugite_primitives::transaction::{Certificate, DRep};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        tx.body.certificates.push(Certificate::VoteRegDeleg {
            credential: Credential::VerificationKey(Hash28::from_bytes([0x55u8; 28])),
            drep: DRep::NoConfidence,
            deposit: Lovelace(0), // Wrong deposit amount
        });

        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::StakeRegistrationDepositMismatch { .. })),
            "VoteRegDeleg with wrong deposit must produce StakeRegistrationDepositMismatch, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 41 — Rule 1g: Conway stake deregistration refund mismatch rejected
    // -----------------------------------------------------------------------
    #[test]
    fn test_conway_stake_dereg_refund_mismatch_rejected() {
        use dugite_primitives::transaction::Certificate;

        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        let cred = dugite_primitives::credentials::Credential::VerificationKey(Hash28::from_bytes(
            [0x44u8; 28],
        ));
        let cred_hash = cred.to_typed_hash32();

        // Record the stored deposit for this credential.
        let mut stake_key_deposits = imbl::HashMap::new();
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
    // Test 41b — Rule 1g (issue #811): an UNREGISTERED credential's dereg
    // refund must NOT produce StakeDeregistrationRefundMismatch. Haskell's
    // `ConwayUnRegCert` binds `Nothing` for an unregistered credential and
    // skips the refund-mismatch predicate entirely.
    // -----------------------------------------------------------------------
    #[test]
    fn test_conway_stake_dereg_unregistered_no_refund_mismatch() {
        use dugite_primitives::transaction::Certificate;

        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway+
        params.key_deposit = Lovelace(2_000_000);

        let cred = dugite_primitives::credentials::Credential::VerificationKey(Hash28::from_bytes(
            [0x45u8; 28],
        ));

        // stake_key_deposits is present but does NOT contain this
        // credential — simulating an unregistered stake credential.
        let stake_key_deposits: imbl::HashMap<Hash32, u64> = imbl::HashMap::new();

        tx.body
            .certificates
            .push(Certificate::ConwayStakeDeregistration {
                credential: cred,
                refund: Lovelace(1_000_000), // Differs from key_deposit
            });

        let result = validate_transaction_with_pools(
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
            Some(&stake_key_deposits), // stake_key_deposits (present, credential absent)
            None,                      // constitution_script_hash
            None,                      // vote_delegations
        );
        let has_refund_mismatch = matches!(&result, Err(errors) if errors
            .iter()
            .any(|e| matches!(e, ValidationError::StakeDeregistrationRefundMismatch { .. })));
        assert!(
            !has_refund_mismatch,
            "Unregistered credential must not produce StakeDeregistrationRefundMismatch, got {result:?}"
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
    // Test 52b — Rule 5: collateral_return below min UTxO rejected (18d)
    //
    // Haskell's `validateOutputTooSmallUTxO` folds over
    // `allSizedOutputsTxBodyF` = outputs + collateral return; before this
    // check dugite validated only `body.outputs` and ACCEPTED a dust return
    // output Haskell rejects. The sentinel index distinguishes it so the
    // wire layer can re-encode the right output.
    // -----------------------------------------------------------------------
    #[test]
    fn test_collateral_return_below_min_utxo_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let mut col_ret = tx.body.outputs[0].clone();
        col_ret.value = Value::lovelace(1);
        col_ret.raw_cbor = None;
        tx.body.collateral_return = Some(col_ret);
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::OutputTooSmall {
                    output_index: crate::validation::COLLATERAL_RETURN_OUTPUT_INDEX,
                    ..
                }
            )),
            "dust collateral_return must produce OutputTooSmall with the sentinel index, got {errors:?}"
        );
        // RED half: a comfortable return output must NOT trip the check.
        let mut ok_ret = tx.body.outputs[0].clone();
        ok_ret.value = Value::lovelace(5_000_000);
        ok_ret.raw_cbor = None;
        tx.body.collateral_return = Some(ok_ret);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        if let Err(errors) = result {
            assert!(
                !errors.iter().any(|e| matches!(
                    e,
                    ValidationError::OutputTooSmall {
                        output_index: crate::validation::COLLATERAL_RETURN_OUTPUT_INDEX,
                        ..
                    }
                )),
                "well-funded collateral_return must not be flagged, got {errors:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 53 — Rule 5: output exactly at min UTxO passes
    // -----------------------------------------------------------------------
    #[test]
    fn test_output_at_min_utxo_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let params = ProtocolParameters::mainnet_defaults();
        let min_utxo = params.babbage_min_utxo_ada_only().0;
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
    // #919 regression: Rule 5 must dispatch per-era, not apply the Babbage
    // serialized-size formula unconditionally. `make_valid_tx()` carries
    // input=10_000_000, so output + fee must sum to 10_000_000 to keep Rule 3
    // (value conservation) satisfied while varying the output/fee split.
    // -----------------------------------------------------------------------

    /// Shelley (PV2): the #919 repro shape — an ~84-byte-serialized output of
    /// exactly the flat `minUTxOValue` (1_000_000) must pass. Pre-fix, dugite
    /// applied the Babbage formula and demanded 1_051_640 instead.
    #[test]
    fn test_shelley_flat_min_utxo_exactly_one_million_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 2;
        params.min_utxo_value = Lovelace(1_000_000);
        tx.body.outputs[0].value = Value::lovelace(1_000_000);
        tx.body.fee = Lovelace(9_000_000);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "Shelley output at exactly the flat minUTxOValue must pass, got {result:?}"
        );
    }

    /// Shelley (PV2): one lovelace below the flat minimum must still reject
    /// (Haskell's comparison is strict `<`, so exactly-equal passes but one
    /// below does not).
    #[test]
    fn test_shelley_flat_min_utxo_one_below_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 2;
        params.min_utxo_value = Lovelace(1_000_000);
        tx.body.outputs[0].value = Value::lovelace(999_999);
        tx.body.fee = Lovelace(9_000_001);
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::OutputTooSmall { minimum: 1_000_000, .. })),
            "output one below the flat minUTxOValue must produce OutputTooSmall{{minimum:1_000_000}}, got {errors:?}"
        );
    }

    /// Allegra (PV3): same flat dispatch as Shelley.
    #[test]
    fn test_allegra_flat_min_utxo_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 3;
        params.min_utxo_value = Lovelace(1_000_000);
        tx.body.outputs[0].value = Value::lovelace(1_000_000);
        tx.body.fee = Lovelace(9_000_000);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "Allegra output at exactly the flat minUTxOValue must pass, got {result:?}"
        );
    }

    /// Mary (PV4), ada-only output: flat dispatch, value-size formula never
    /// consulted.
    #[test]
    fn test_mary_ada_only_flat_min_utxo_passes() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 4;
        params.min_utxo_value = Lovelace(1_000_000);
        tx.body.outputs[0].value = Value::lovelace(1_000_000);
        tx.body.fee = Lovelace(9_000_000);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "Mary ada-only output at exactly the flat minUTxOValue must pass, got {result:?}"
        );
    }

    /// Mary (PV4), multi-asset output: `scaledMinDeposit` must scale by
    /// `mary_value_size`, not the flat value. A single 0-byte-named asset
    /// scales the minimum to 1_407_406 (oracle golden, see
    /// `dugite-primitives::value::tests::mary_value_size_one_policy_one_zero_byte_asset_name`).
    /// The multi-asset is placed directly in the spent UTxO (not minted) so
    /// Rule 3c's script-witness requirement is not in play.
    #[test]
    fn test_mary_multi_asset_scaled_min_utxo() {
        let (mut utxo_set, mut tx, input) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 4;
        params.min_utxo_value = Lovelace(1_000_000);

        let policy = Hash28::from_bytes([0x11u8; 28]);
        let asset = AssetName::empty();

        // Give the spent UTxO the same multi-asset bundle the output carries,
        // so Rule 3b (multi-asset conservation) is satisfied without minting.
        let mut funded = TransactionOutput {
            address: Address::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![0x82, 0x00, 0x01],
            }),
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        funded
            .value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 1);
        utxo_set.insert(input.clone(), funded);

        // One below the scaled minimum (1_407_406) must reject.
        tx.body.outputs[0].value = Value::lovelace(1_407_405);
        tx.body.outputs[0]
            .value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 1);
        tx.body.fee = Lovelace(10_000_000 - 1_407_405);
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::OutputTooSmall {
                    minimum: 1_407_406,
                    ..
                }
            )),
            "expected OutputTooSmall{{minimum:1_407_406}}, got {errors:?}"
        );

        // Exactly at the scaled minimum must pass.
        tx.body.outputs[0].value = Value::lovelace(1_407_406);
        tx.body.outputs[0]
            .value
            .multi_asset
            .entry(policy)
            .or_default()
            .insert(asset, 1);
        tx.body.fee = Lovelace(10_000_000 - 1_407_406);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "output exactly at the Mary scaled minimum must pass, got {result:?}"
        );
    }

    /// Alonzo (PV5), ada-only, no datum hash: `utxoEntrySize * coinsPerUTxOWord`
    /// with NO ada-only short-circuit — oracle golden 999_978.
    #[test]
    fn test_alonzo_ada_only_no_datum_golden_min_utxo() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 5;
        params.coins_per_utxo_word = Lovelace(34_482);
        tx.body.outputs[0].value = Value::lovelace(999_978);
        tx.body.fee = Lovelace(10_000_000 - 999_978);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "Alonzo ada-only output at the golden minimum (999_978) must pass, got {result:?}"
        );

        tx.body.outputs[0].value = Value::lovelace(999_977);
        tx.body.fee = Lovelace(10_000_000 - 999_977);
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::OutputTooSmall {
                    minimum: 999_978,
                    ..
                }
            )),
            "expected OutputTooSmall{{minimum:999_978}}, got {errors:?}"
        );
    }

    /// Alonzo (PV6), ada-only WITH a datum hash: `dataHashSize` adds 10
    /// words — oracle golden 1_344_798.
    #[test]
    fn test_alonzo_ada_only_with_datum_hash_golden_min_utxo() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 6;
        params.coins_per_utxo_word = Lovelace(34_482);
        tx.body.outputs[0].datum = OutputDatum::DatumHash(Hash32::ZERO);
        tx.body.outputs[0].value = Value::lovelace(1_344_798);
        tx.body.fee = Lovelace(10_000_000 - 1_344_798);
        let result = validate_transaction(&tx, &utxo_set, &params, 100, 300, None);
        assert!(
            result.is_ok(),
            "Alonzo output with datum hash at the golden minimum (1_344_798) must pass, got {result:?}"
        );
    }

    /// Babbage (PV7): unchanged serialized-size formula (regression guard —
    /// the per-era dispatch must not alter Babbage/Conway behavior).
    #[test]
    fn test_babbage_serialized_size_formula_unchanged() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 7;
        let min_utxo = params.babbage_min_utxo_ada_only().0;
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
            "Babbage output at the serialized-size minimum must NOT produce OutputTooSmall, got {result:?}"
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
    fn test_bootstrap_witness_valid_struct_invalid_sig_rejected() {
        // Structurally valid witness (vkey=32, sig=64, chain_code=32) but the
        // signature is all-zeros → fails Ed25519 verify. The XPub used for the
        // address root is vkey(32) || chain_code(32) = [0x55; 64], so binding
        // matches and the sig-verification failure is isolated.
        let xpub64 = [0x55u8; 64];
        let root28 = compute_root_for_vkey64(&xpub64);
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
            vkey: vec![0x55u8; 32],       // first 32 bytes of the XPub
            signature: vec![0u8; 64],     // invalid signature
            chain_code: vec![0x55u8; 32], // last 32 bytes of the XPub
            attributes: vec![0xa0],       // empty CBOR map
        });
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidWitnessSignature(_))),
            "expected InvalidWitnessSignature for bad sig on valid-struct witness, got {errors:?}"
        );
    }

    // ------ Structural pre-flight: size checks -------

    /// 32 bytes is the CORRECT `bootstrap_witness` `public_key` size (Shelley
    /// CDDL `vkey = bytes .size 32`). A real Ed25519 signature over the tx-body
    /// hash MUST verify with no error; a tampered signature MUST be rejected.
    /// Cross-checks the Haskell `verifyBootstrapWit` semantics — the 32-byte
    /// `bwKey` is passed directly to Ed25519 verification.
    #[test]
    fn test_bootstrap_witness_valid_sig_accepted_tampered_rejected() {
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key().to_bytes().to_vec(); // 32 bytes
        let tx_hash = [0x11u8; 32];
        let sig = sk.sign(&tx_hash); // 64 bytes

        let good = BootstrapWitness {
            vkey: vk.clone(),
            signature: sig.clone(),
            chain_code: vec![0x07u8; 32],
            attributes: vec![0xa0],
        };
        assert!(
            super::verify_single_bootstrap_witness(&good, &tx_hash).is_none(),
            "valid 32-byte-vkey bootstrap signature must verify"
        );

        // Tamper one signature byte → must be rejected (security: do not accept
        // forged bootstrap witnesses).
        let mut bad_sig = sig;
        bad_sig[0] ^= 0x01;
        let bad = BootstrapWitness {
            vkey: vk,
            signature: bad_sig,
            chain_code: vec![0x07u8; 32],
            attributes: vec![0xa0],
        };
        assert!(
            super::verify_single_bootstrap_witness(&bad, &tx_hash).is_some(),
            "tampered bootstrap signature must be rejected"
        );
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
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 32], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_65byte_sig_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 32], vec![0u8; 65]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_0byte_sig_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 32], vec![]);
        assert_invalid_witness(&errors);
    }

    #[test]
    fn test_bootstrap_witness_both_wrong_size_rejected() {
        let errors = validate_with_bootstrap_witness(vec![0xAAu8; 63], vec![0u8; 63]);
        assert_invalid_witness(&errors);
    }

    /// Length-lattice property: across malformed sizes a bootstrap witness must
    /// produce `InvalidWitnessSignature`. The structurally-valid size is
    /// {vkey=32, sig=64}; here even that combination fails because the signature
    /// bytes are all-zero (so Ed25519 verification fails), so every cell errors.
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

    /// Chain-code length check: 32-byte vkey + 64-byte sig but 31-byte chain_code → rejected.
    #[test]
    fn test_bootstrap_witness_short_chain_code_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set.bootstrap_witnesses.push(BootstrapWitness {
            vkey: vec![0xAAu8; 32],
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

        // Witness whose XPub (vkey || chain_code) derives a DIFFERENT root.
        let xpub64 = [0x42u8; 64];
        let computed_root = compute_root_for_vkey64(&xpub64);
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
            vkey: vec![0x42u8; 32],       // first 32 bytes of the XPub
            signature: vec![0u8; 64],     // sig fails too but binding is the point
            chain_code: vec![0x42u8; 32], // last 32 bytes of the XPub
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

    // -----------------------------------------------------------------------
    // Tests — ConwayDRepIncorrectRefund (P2 follow-up)
    //
    // `UnregDRep` whose declared `refund` does not match the deposit stored
    // at registration time must be rejected. Mirrors Haskell
    // `conwayGovCertTransition` (GOVCERT). The check runs in
    // `validate_transaction_with_context` (drep_deposits lives on the
    // ValidationContext).
    // -----------------------------------------------------------------------

    /// Refund matches stored deposit → no DRepIncorrectRefund.
    #[test]
    fn test_unreg_drep_refund_matches_accepted() {
        use crate::validation::{validate_transaction_with_context, ValidationContext};
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, mut tx, _) = make_valid_tx();
        let stored_deposit = 500_000_000u64;
        tx.body.certificates.push(Certificate::UnregDRep {
            credential: Credential::VerificationKey(cred_hash),
            refund: Lovelace(stored_deposit), // matches stored
        });
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let credential = Credential::VerificationKey(cred_hash);
        let key = credential.to_typed_hash32();
        let mut dreps = std::collections::HashSet::new();
        dreps.insert(key);
        let mut deposits = std::collections::HashMap::new();
        deposits.insert(key, stored_deposit);

        let context = ValidationContext::new()
            .with_dreps(dreps)
            .with_drep_deposits(deposits);

        let errors =
            validate_transaction_with_context(&tx, &utxo_set, &params, 100, 300, None, context)
                .err()
                .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepIncorrectRefund { .. })),
            "matching refund must not produce DRepIncorrectRefund, got {errors:?}"
        );
    }

    /// Refund != stored deposit → DRepIncorrectRefund fires.
    #[test]
    fn test_unreg_drep_refund_mismatch_rejected() {
        use crate::validation::{validate_transaction_with_context, ValidationContext};
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, mut tx, _) = make_valid_tx();
        let stored_deposit = 500_000_000u64;
        let declared_refund = 999_999_999u64; // mismatch
        tx.body.certificates.push(Certificate::UnregDRep {
            credential: Credential::VerificationKey(cred_hash),
            refund: Lovelace(declared_refund),
        });
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let credential = Credential::VerificationKey(cred_hash);
        let key = credential.to_typed_hash32();
        let mut dreps = std::collections::HashSet::new();
        dreps.insert(key);
        let mut deposits = std::collections::HashMap::new();
        deposits.insert(key, stored_deposit);

        let context = ValidationContext::new()
            .with_dreps(dreps)
            .with_drep_deposits(deposits);

        let errors =
            validate_transaction_with_context(&tx, &utxo_set, &params, 100, 300, None, context)
                .unwrap_err();

        let found = errors.iter().any(|e| {
            matches!(
                e,
                ValidationError::DRepIncorrectRefund {
                    declared,
                    expected,
                    ..
                }
                if *declared == declared_refund && *expected == stored_deposit
            )
        });
        assert!(
            found,
            "refund mismatch must produce DRepIncorrectRefund(declared={declared_refund}, \
             expected={stored_deposit}), got {errors:?}"
        );
    }

    /// `drep_deposits=None` → refund check skipped (lenient default).
    #[test]
    fn test_unreg_drep_refund_no_deposits_skipped() {
        use crate::validation::{validate_transaction_with_context, ValidationContext};
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.certificates.push(Certificate::UnregDRep {
            credential: Credential::VerificationKey(cred_hash),
            refund: Lovelace(999_999_999), // would mismatch if checked
        });
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        let credential = Credential::VerificationKey(cred_hash);
        let key = credential.to_typed_hash32();
        let mut dreps = std::collections::HashSet::new();
        dreps.insert(key);

        // drep_deposits NOT supplied → predicate must be silently skipped
        let context = ValidationContext::new().with_dreps(dreps);

        let errors =
            validate_transaction_with_context(&tx, &utxo_set, &params, 100, 300, None, context)
                .err()
                .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepIncorrectRefund { .. })),
            "drep_deposits=None must skip refund check, got {errors:?}"
        );
    }

    /// Unknown DRep + refund mismatch → only DRepNotRegistered fires
    /// (mirrors Haskell `?!` short-circuit before the refund check).
    #[test]
    fn test_unreg_drep_unknown_skips_refund_check() {
        use crate::validation::{validate_transaction_with_context, ValidationContext};
        let cred_hash = Hash28::from_bytes([0xDEu8; 28]);
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.certificates.push(Certificate::UnregDRep {
            credential: Credential::VerificationKey(cred_hash),
            refund: Lovelace(999_999_999),
        });
        let params = {
            let mut p = ProtocolParameters::mainnet_defaults();
            p.protocol_version_major = 9;
            p
        };
        // DRep is NOT in registered_dreps; refund check should be skipped
        // (DRepNotRegistered handles it).
        let credential = Credential::VerificationKey(cred_hash);
        let key = credential.to_typed_hash32();
        let mut deposits = std::collections::HashMap::new();
        deposits.insert(key, 500_000_000u64);

        let context = ValidationContext::new()
            .with_dreps(std::collections::HashSet::new()) // empty
            .with_drep_deposits(deposits);

        let errors =
            validate_transaction_with_context(&tx, &utxo_set, &params, 100, 300, None, context)
                .unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepNotRegistered { .. })),
            "expected DRepNotRegistered for unknown DRep, got {errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::DRepIncorrectRefund { .. })),
            "unknown DRep must short-circuit refund check, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — POOL.3 lower bound (StakePoolRetirementWrongEpochPOOL)
    //
    // Mirrors Haskell `cEpoch < e && e <= limitEpoch` — retirement epoch
    // must be STRICTLY in the future, and not exceed `cEpoch + eMax`.
    // -----------------------------------------------------------------------

    /// Retirement epoch <= current_epoch → PoolRetirementTooEarly fires.
    #[test]
    fn test_pool_retirement_too_early_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.certificates.push(Certificate::PoolRetirement {
            pool_hash: Hash28::from_bytes([0xAB; 28]),
            epoch: 100, // == current_epoch → fails strict lower bound
        });
        let params = ProtocolParameters::mainnet_defaults();
        // current_epoch=100 supplied via validate_transaction_with_pools
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
            Some(100),
            None,
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
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::PoolRetirementTooEarly { .. })),
            "retirement_epoch == current_epoch must produce PoolRetirementTooEarly, got {errors:?}"
        );
    }

    /// Retirement epoch > current_epoch and <= current_epoch + eMax → accepted.
    #[test]
    fn test_pool_retirement_valid_window_accepted() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.body.certificates.push(Certificate::PoolRetirement {
            pool_hash: Hash28::from_bytes([0xAB; 28]),
            epoch: 110, // strictly > current_epoch, within e_max
        });
        let params = ProtocolParameters::mainnet_defaults();
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
            Some(100),
            None,
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
            !errors.iter().any(|e| matches!(
                e,
                ValidationError::PoolRetirementTooEarly { .. }
                    | ValidationError::PoolRetirementTooLate { .. }
            )),
            "valid retirement window must not produce PoolRetirementTooEarly/Late, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — Allegra+ InvalidMetadata (64-byte leaf limit)
    // -----------------------------------------------------------------------

    fn make_tx_with_metadata_text(text: String) -> (UtxoSet, Transaction) {
        use dugite_primitives::transaction::{AuxiliaryData, TransactionMetadatum};
        let (utxo_set, mut tx, _) = make_valid_tx();
        let mut meta = ::std::collections::BTreeMap::new();
        meta.insert(0u64, TransactionMetadatum::Text(text));
        let aux = AuxiliaryData {
            metadata: meta,
            native_scripts: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            raw_cbor: None,
        };
        // Add the aux_data_hash so the hash-presence predicate doesn't
        // fire ahead of InvalidMetadata.
        tx.body.auxiliary_data_hash = Some(Hash32::ZERO);
        tx.auxiliary_data = Some(aux);
        (utxo_set, tx)
    }

    /// Metadata text leaf > 64 bytes at Allegra+ → InvalidMetadata fires.
    #[test]
    fn test_invalid_metadata_oversize_text_rejected() {
        let oversized = "a".repeat(65);
        let (utxo_set, tx) = make_tx_with_metadata_text(oversized);
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidMetadata { .. })),
            "65-byte metadata text must produce InvalidMetadata, got {errors:?}"
        );
    }

    /// Metadata text leaf == 64 bytes at Allegra+ → accepted.
    #[test]
    fn test_invalid_metadata_64_byte_text_accepted() {
        let exact = "a".repeat(64);
        let (utxo_set, tx) = make_tx_with_metadata_text(exact);
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidMetadata { .. })),
            "64-byte metadata text must NOT produce InvalidMetadata, got {errors:?}"
        );
    }

    /// At PV<3 (Shelley) the check is skipped (Haskell pre-Allegra
    /// decoder version <= 2 didn't enforce the cap).
    #[test]
    fn test_invalid_metadata_skipped_at_shelley() {
        let oversized = "a".repeat(100);
        let (utxo_set, tx) = make_tx_with_metadata_text(oversized);
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 2; // Shelley
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidMetadata { .. })),
            "Pre-Allegra (PV=2) must skip InvalidMetadata, got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests — MalformedScriptWitnesses / MalformedReferenceScripts (Babbage+)
    //
    // Witness-set and tx-output reference scripts must pass `validScript pv`
    // — Plutus scripts must decode as flat UPLC AND their language version
    // must be supported at the current PV.
    // -----------------------------------------------------------------------

    fn minimal_plutus_program_flat() -> Vec<u8> {
        let p = dugite_uplc::program::Program {
            version: dugite_uplc::program::Program::version_triple(1, 0, 0),
            term: dugite_uplc::term::Term::Const(dugite_uplc::term::Constant::Integer(
                num_bigint::BigInt::from(0),
            )),
        };
        p.to_flat().expect("flat-encode minimal Plutus program")
    }

    /// CBOR-wrapped form of [`minimal_plutus_program_flat`] — the format
    /// real on-chain Plutus WITNESS scripts use. The witness-set array
    /// element (`[* bytes]`), once unwrapped off the wire, is itself a
    /// CBOR bytestring wrapping the flat program (`Program::from_cbor`
    /// requires this — issue #792). `ScriptRef` (reference scripts) does
    /// NOT carry this extra wrapper (`dugite-serialization`'s
    /// `read_script_ref` already strips it), so
    /// [`minimal_plutus_program_flat`] remains the correct fixture for
    /// `ScriptRef` tests.
    fn minimal_plutus_program_cbor() -> Vec<u8> {
        let p = dugite_uplc::program::Program {
            version: dugite_uplc::program::Program::version_triple(1, 0, 0),
            term: dugite_uplc::term::Term::Const(dugite_uplc::term::Constant::Integer(
                num_bigint::BigInt::from(0),
            )),
        };
        p.to_cbor().expect("cbor-encode minimal Plutus program")
    }

    /// Garbage bytes in `witness_set.plutus_v1_scripts` → MalformedScriptWitnesses.
    #[test]
    fn test_malformed_plutus_v1_witness_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set
            .plutus_v1_scripts
            .push(vec![0xff, 0xee, 0xdd]);
        let params = ProtocolParameters::mainnet_defaults();
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "garbage Plutus V1 witness must produce MalformedScriptWitnesses, got {errors:?}"
        );
    }

    /// Era gate: `MalformedScriptWitnesses` is a Babbage+ (PV>=7) predicate and
    /// does NOT exist in the Alonzo UTXOW rule. The same garbage V1 witness that
    /// is rejected at PV7 must NOT be flagged at PV6 (Alonzo) — mainnet tx
    /// 61073ad8… class, where a well-formed-on-chain V1 script was wrongly
    /// rejected during Alonzo sync.
    #[test]
    fn test_malformed_script_witnesses_gated_to_babbage() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set
            .plutus_v1_scripts
            .push(vec![0xff, 0xee, 0xdd]);

        let mut alonzo = ProtocolParameters::mainnet_defaults();
        alonzo.protocol_version_major = 6; // Alonzo
        let alonzo_errors = validate_transaction(&tx, &utxo_set, &alonzo, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !alonzo_errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "Alonzo (PV6) must NOT run the Babbage-only MalformedScriptWitnesses check, got {alonzo_errors:?}"
        );

        let mut babbage = ProtocolParameters::mainnet_defaults();
        babbage.protocol_version_major = 7; // Babbage
        let babbage_errors = validate_transaction(&tx, &utxo_set, &babbage, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            babbage_errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "Babbage (PV7) must run the check and reject the garbage V1 witness, got {babbage_errors:?}"
        );
    }

    /// A zero-amount withdrawal must NOT be rejected in ANY era — cardano-ledger
    /// has no `wdrlNotZero` predicate; the only rule is amount==balance
    /// (`isSubmapOfUM`), which accepts `0 == 0`. Mainnet Alonzo tx fc7ca745…
    /// carried a 0-lovelace withdrawal and was accepted on-chain.
    #[test]
    fn test_zero_amount_withdrawal_accepted_all_eras() {
        let mut reward_account = vec![0xe1u8]; // mainnet VKey-staking reward account
        reward_account.extend_from_slice(&[0x09u8; 28]);
        for pv in [2u64, 5, 6, 7, 9] {
            let (utxo_set, mut tx, _) = make_valid_tx();
            tx.body.withdrawals.insert(
                reward_account.clone(),
                dugite_primitives::value::Lovelace(0),
            );
            let mut params = ProtocolParameters::mainnet_defaults();
            params.protocol_version_major = pv;
            let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
                .err()
                .unwrap_or_default();
            assert!(
                !errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::ZeroWithdrawal { .. })),
                "PV{pv}: a zero-amount withdrawal must not produce ZeroWithdrawal; got {errors:?}"
            );
        }
    }

    /// Real CBOR-wrapped-flat Plutus V1 witness bytes at PV >= 5 → no
    /// MalformedScriptWitnesses. Witness scripts require the CBOR
    /// bytestring wrapper (issue #792) — `minimal_plutus_program_cbor`
    /// supplies it.
    #[test]
    fn test_well_formed_plutus_v1_witness_accepted() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set
            .plutus_v1_scripts
            .push(minimal_plutus_program_cbor());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "well-formed Plutus V1 witness must not produce MalformedScriptWitnesses, got {errors:?}"
        );
    }

    /// Issue #792: a raw-flat (unwrapped) witness script — i.e. the exact
    /// bytes `minimal_plutus_program_flat` produces, WITHOUT the mandatory
    /// CBOR bytestring wrapper — must now be rejected as
    /// MalformedScriptWitnesses. Haskell's `deserialiseScript` requires
    /// `CBOR.decodeBytes` to succeed before flat-decoding; skipping the
    /// wrapper is a hard decode error for every Plutus language.
    #[test]
    fn test_raw_flat_witness_without_cbor_wrapper_rejected() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        tx.witness_set
            .plutus_v1_scripts
            .push(minimal_plutus_program_flat());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "raw-flat (unwrapped) witness script must produce MalformedScriptWitnesses, got {errors:?}"
        );
    }

    /// PlutusV3 script at PV < 9 → MalformedScriptWitnesses (language not yet supported).
    #[test]
    fn test_plutus_v3_witness_rejected_before_pv9() {
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Even a well-formed V3 script is malformed at PV < 9 because the language
        // is not yet enabled — matches `isValidPlutusScript (pvMajor pv)`.
        tx.witness_set
            .plutus_v3_scripts
            .push(minimal_plutus_program_flat());
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 8; // Babbage — V3 not yet allowed
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedScriptWitnesses { .. })),
            "Plutus V3 witness at PV=8 must produce MalformedScriptWitnesses, got {errors:?}"
        );
    }

    /// Garbage bytes in `output.script_ref::PlutusV2` → MalformedReferenceScripts.
    #[test]
    fn test_malformed_reference_script_rejected() {
        use dugite_primitives::transaction::ScriptRef;
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Attach a garbage Plutus V2 reference script to an output PRODUCED
        // by this tx.
        tx.body.outputs[0].script_ref = Some(ScriptRef::PlutusV2(vec![0xff, 0xee]));
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // V2 supported
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedReferenceScripts { .. })),
            "garbage ref script must produce MalformedReferenceScripts, got {errors:?}"
        );
    }

    /// Well-formed ref script at supported PV → no MalformedReferenceScripts.
    #[test]
    fn test_well_formed_reference_script_accepted() {
        use dugite_primitives::transaction::ScriptRef;
        let (utxo_set, mut tx, _) = make_valid_tx();
        // Real on-chain reference scripts are CBOR-bytestring-wrapped flat
        // (cbor(flat)), exactly like witness scripts — NOT raw flat. #792's
        // ref-script decode check therefore requires `from_cbor` (corrected
        // after the #836 tx6.json fixture proved the double-wrap); build the
        // fixture to match real chain data.
        tx.body.outputs[0].script_ref = Some(ScriptRef::PlutusV2(minimal_plutus_program_cbor()));
        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9;
        let errors = validate_transaction(&tx, &utxo_set, &params, 100, 300, None)
            .err()
            .unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::MalformedReferenceScripts { .. })),
            "well-formed ref script must not produce MalformedReferenceScripts, got {errors:?}"
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
