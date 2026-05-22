//! In-house decoder for the Shelley era (era tag 2).
//!
//! # Shelley block wire format
//!
//! After stripping the HFC envelope `[era_tag, inner]`, the inner CBOR is:
//!
//! ```text
//! block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set]
//! ```
//!
//! **4 elements** — Shelley/Allegra/Mary do **not** have an `invalid_transactions`
//! field (that was added in Alonzo). The presence of 4 vs 5 elements distinguishes
//! pre-Alonzo from Alonzo+ blocks.
//!
//! ## Header structure
//!
//! ```text
//! header = [header_body, kes_signature]
//! header_body = [
//!   block_number,        ; 0 — u64
//!   slot,                ; 1 — u64
//!   prev_hash,           ; 2 — bytes(32) or null
//!   issuer_vkey,         ; 3 — bytes(32)
//!   vrf_vkey,            ; 4 — bytes(32)
//!   nonce_vrf_cert,      ; 5 — [bytes(64), bytes(80)]   (VRF output + proof)
//!   leader_vrf_cert,     ; 6 — [bytes(64), bytes(80)]
//!   block_body_size,     ; 7 — u64
//!   block_body_hash,     ; 8 — bytes(32)
//!   op_cert_hot_vkey,    ; 9 — bytes(32)
//!   op_cert_seq_number,  ; 10 — u64
//!   op_cert_kes_period,  ; 11 — u64
//!   op_cert_sigma,       ; 12 — bytes(64)
//!   protocol_major,      ; 13 — u64
//!   protocol_minor,      ; 14 — u64
//! ]
//! ```
//!
//! Fields 9–12 are the operational certificate fields (inline in Shelley/Alonzo,
//! promoted to a separate `operational_cert` struct in Babbage).
//!
//! ## Header hash
//!
//! `blake2b_256(raw_header_cbor)` — the hash of the exact bytes of the `header`
//! struct (the 2-element array containing `[header_body, kes_signature]`).
//!
//! This matches the cardano-ledger spec `OriginalHash<32> for KeepRaw<'_, alonzo::Header>`:
//! `Hasher::<256>::hash(self.raw_cbor())`.
//!
//! ## Transaction hash
//!
//! `blake2b_256(raw_tx_body_cbor)` — the hash of the exact bytes of the transaction
//! body map (the `KeepRaw<TransactionBody>` CBOR).
//!
//! This matches the cardano-ledger spec `OriginalHash<32> for KeepRaw<'_, alonzo::TransactionBody>`:
//! `Hasher::<256>::hash(self.raw_cbor())`.
//!
//! ## Tx body structure (Shelley CDDL)
//!
//! The Shelley transaction body is a map with keys 0..10:
//! ```text
//! transaction_body = {
//!   0 : [* transaction_input],      ; inputs
//!   1 : [* transaction_output],     ; outputs
//!   2 : coin,                       ; fee
//!   ? 3 : uint,                     ; TTL (slot)
//!   ? 4 : [* certificate],          ; certificates
//!   ? 5 : withdrawals,              ; withdrawals
//!   ? 6 : update,                   ; update proposals
//!   ? 7 : auxiliary_data_hash,      ; aux data hash (32 bytes)
//! }
//! ```
//!
//! The Shelley transaction body is decodeed from the `KeepRaw<TransactionBody>` CBOR,
//! with the hash computed over those raw bytes. For this decoder, we capture the
//! body raw bytes and parse only the fields needed to populate `TransactionBody`.
//!
//! ## DecodeMode
//!
//! Both Full and Minimal modes are supported. In Minimal mode, the witness set is
//! skipped entirely (empty `TransactionWitnessSet`). Auxiliary data is always parsed.

use crate::decode::helpers::{read_hash32, read_lovelace};
use crate::decode::raw::KeepRaw;
use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::address::Address;
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash28, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    AuxiliaryData, BootstrapWitness, Certificate, MIRSource, MIRTarget, NativeScript, OutputDatum,
    PoolMetadata, PoolParams, Rational, Transaction, TransactionBody, TransactionInput,
    TransactionMetadatum, TransactionOutput, TransactionWitnessSet, VKeyWitness,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};
use minicbor::data::Type;
use std::collections::BTreeMap;

// ============================================================================
// Decode mode (mirrors multi_era::DecodeMode)
// ============================================================================

/// Controls whether the witness set is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeMode {
    /// Decode body, witnesses, aux data.
    Full,
    /// Decode body and aux data only; witness set is left empty.
    Minimal,
}

// ============================================================================
// Top-level entry point
// ============================================================================

/// Decode a Shelley block from the inner CBOR (after HFC envelope stripping).
///
/// `inner_cbor` is the CBOR starting at the `block` structure:
/// `[header, tx_bodies, tx_witness_sets, auxiliary_data_set]`
pub fn decode_shelley_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_shelley_block_mode(inner_cbor, DecodeMode::Full)
}

/// Decode a Shelley block in minimal mode (witness set skipped).
pub fn decode_shelley_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_shelley_block_mode(inner_cbor, DecodeMode::Minimal)
}

// ============================================================================
// Block decoder
// ============================================================================

fn decode_shelley_block_mode(
    inner_cbor: &[u8],
    mode: DecodeMode,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    // block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set]
    // Note: Shelley has 4 elements (no invalid_transactions field).
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "shelley block: expected array(4), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header — capture raw bytes for hash computation
    // -------------------------------------------------------------------------
    let header = {
        let raw = KeepRaw::parse_with(&mut r, decode_shelley_header_inner)?;
        let header_hash = blake2b_256(raw.raw);
        let mut h = raw.value;
        h.header_hash = header_hash;
        h
    };

    // -------------------------------------------------------------------------
    // 2. tx_bodies — definite-length array of KeepRaw<TransactionBody>
    // -------------------------------------------------------------------------
    let tx_count = r.read_array_header()?.unwrap_or(0) as usize;
    let alloc_cap = r.safe_alloc_capacity(tx_count as u64);
    let mut raw_bodies: Vec<Vec<u8>> = Vec::with_capacity(alloc_cap);
    let mut parsed_bodies: Vec<TransactionBody> = Vec::with_capacity(alloc_cap);

    for _ in 0..tx_count {
        let body = KeepRaw::parse_with(&mut r, |r| decode_shelley_tx_body(r))?;
        raw_bodies.push(body.raw.to_vec());
        parsed_bodies.push(body.value);
    }

    // -------------------------------------------------------------------------
    // 3. tx_witness_sets — definite-length array of witness sets
    // -------------------------------------------------------------------------
    let witness_count = r.read_array_header()?.unwrap_or(0) as usize;
    let ws_alloc_cap = r.safe_alloc_capacity(witness_count as u64);
    let mut raw_witnesses: Vec<Vec<u8>> = Vec::with_capacity(ws_alloc_cap);
    let mut parsed_witnesses: Vec<Option<TransactionWitnessSet>> = Vec::with_capacity(ws_alloc_cap);

    for _ in 0..witness_count {
        if mode == DecodeMode::Full {
            let ws = KeepRaw::parse_with(&mut r, |r| decode_shelley_witness_set(r))?;
            raw_witnesses.push(ws.raw.to_vec());
            parsed_witnesses.push(Some(ws.value));
        } else {
            let ws_start = r.position();
            r.skip()?;
            raw_witnesses.push(r.slice_from(ws_start).to_vec());
            parsed_witnesses.push(None);
        }
    }

    // -------------------------------------------------------------------------
    // 4. auxiliary_data_set — map of tx_index → AuxiliaryData
    // -------------------------------------------------------------------------
    let aux_map = decode_aux_data_map(&mut r)?;

    // -------------------------------------------------------------------------
    // Build transactions
    // -------------------------------------------------------------------------
    let transactions: Result<Vec<Transaction>, SerializationError> = parsed_bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            let raw_body = raw_bodies[i].clone();
            let tx_hash = blake2b_256(&raw_body);

            let witness_set = match parsed_witnesses.get(i).and_then(|w| w.as_ref()) {
                Some(ws) => ws.clone(),
                None => empty_witness_set(),
            };
            let raw_witness = raw_witnesses.get(i).cloned();
            let auxiliary_data = aux_map.get(&(i as u32)).cloned();

            Ok(Transaction {
                hash: tx_hash,
                era: Era::Shelley,
                body,
                witness_set,
                is_valid: true,
                auxiliary_data,
                raw_cbor: None, // full tx CBOR not available without re-serialization
                raw_body_cbor: Some(raw_body),
                raw_witness_cbor: raw_witness,
            })
        })
        .collect();
    let transactions = transactions?;

    Ok(Block {
        header,
        transactions,
        era: Era::Shelley,
        raw_cbor: None,
    })
}

fn empty_witness_set() -> TransactionWitnessSet {
    TransactionWitnessSet {
        vkey_witnesses: Vec::new(),
        native_scripts: Vec::new(),
        bootstrap_witnesses: Vec::new(),
        plutus_v1_scripts: Vec::new(),
        plutus_v2_scripts: Vec::new(),
        plutus_v3_scripts: Vec::new(),
        plutus_data: Vec::new(),
        redeemers: Vec::new(),
        raw_redeemers_cbor: None,
        raw_plutus_data_cbor: None,
        original_script_data_hash: None,
    }
}

// ============================================================================
// Header decoder
// ============================================================================

fn decode_shelley_header_inner(r: &mut Reader<'_>) -> Result<BlockHeader, SerializationError> {
    // header = [header_body, kes_signature]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "shelley header: expected array(2), got {hdr_arr:?}"
        )));
    }

    // header_body = array(15)
    let body_arr = r.read_array_header()?;
    if !matches!(body_arr, Some(15)) {
        return Err(SerializationError::CborDecode(format!(
            "shelley header_body: expected array(15), got {body_arr:?}"
        )));
    }

    // 0: block_number
    let block_number = r.read_uint()?;
    // 1: slot
    let slot = r.read_uint()?;
    // 2: prev_hash (32-byte bytestring or null)
    let prev_hash = read_optional_hash32(r)?;
    // 3: issuer_vkey (32 bytes)
    let issuer_vkey = r.read_bytes()?.to_vec();
    // 4: vrf_vkey (32 bytes)
    let vrf_vkey = r.read_bytes()?.to_vec();
    // 5: nonce_vrf_cert = [output_bytes(64), proof_bytes(80)]
    let (nonce_output, nonce_proof) = read_vrf_cert(r)?;
    // 6: leader_vrf_cert = [output_bytes(64), proof_bytes(80)]
    let (leader_output, leader_proof) = read_vrf_cert(r)?;
    // 7: block_body_size
    let body_size = r.read_uint()?;
    // 8: block_body_hash (32 bytes)
    let body_hash = read_hash32(r)?;
    // 9: op_cert_hot_vkey (32 bytes)
    let op_hot_vkey = r.read_bytes()?.to_vec();
    // 10: op_cert_sequence_number
    let op_seq_number = r.read_uint()?;
    // 11: op_cert_kes_period
    let op_kes_period = r.read_uint()?;
    // 12: op_cert_sigma (64 bytes)
    let op_sigma = r.read_bytes()?.to_vec();
    // 13: protocol_major
    let protocol_major = r.read_uint()?;
    // 14: protocol_minor
    let protocol_minor = r.read_uint()?;

    // KES signature (second element of outer array)
    let kes_signature = r.read_bytes()?.to_vec();

    // header_hash will be filled in by the caller after computing blake2b_256(raw)
    Ok(BlockHeader {
        header_hash: Hash32::ZERO, // placeholder — filled in by decode_shelley_block_mode
        prev_hash,
        issuer_vkey,
        vrf_vkey,
        vrf_result: VrfOutput {
            output: leader_output,
            proof: leader_proof,
        },
        block_number: BlockNo(block_number),
        slot: SlotNo(slot),
        epoch_nonce: Hash32::ZERO,
        body_size,
        body_hash,
        operational_cert: OperationalCert {
            hot_vkey: op_hot_vkey,
            sequence_number: op_seq_number,
            kes_period: op_kes_period,
            sigma: op_sigma,
        },
        protocol_version: ProtocolVersion {
            major: protocol_major,
            minor: protocol_minor,
        },
        kes_signature,
        nonce_vrf_output: nonce_output,
        nonce_vrf_proof: nonce_proof,
        prev_nonce: None,
    })
}

/// Read an optional 32-byte hash (null or bytes).
fn read_optional_hash32(r: &mut Reader<'_>) -> Result<Hash32, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        Ok(Hash32::ZERO)
    } else {
        read_hash32(r)
    }
}

/// Read a VRF certificate: `[output_bytes, proof_bytes]`.
fn read_vrf_cert(r: &mut Reader<'_>) -> Result<(Vec<u8>, Vec<u8>), SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "vrf_cert: expected array(2), got {arr_len:?}"
        )));
    }
    let output = r.read_bytes()?.to_vec();
    let proof = r.read_bytes()?.to_vec();
    Ok((output, proof))
}

// ============================================================================
// Transaction body decoder
// ============================================================================

/// Decode a Shelley transaction body from a map.
///
/// The Shelley tx body is a CBOR map with keys 0..7. Unknown keys are silently
/// ignored (forward-compatibility behaviour matching Haskell).
fn decode_shelley_tx_body(r: &mut Reader<'_>) -> Result<TransactionBody, SerializationError> {
    let mut inputs: Vec<TransactionInput> = Vec::new();
    let mut outputs: Vec<TransactionOutput> = Vec::new();
    let mut fee = Lovelace(0);
    let mut ttl: Option<SlotNo> = None;
    let mut certificates: Vec<Certificate> = Vec::new();
    let mut withdrawals: BTreeMap<Vec<u8>, Lovelace> = BTreeMap::new();
    let mut auxiliary_data_hash: Option<Hash32> = None;
    let mut update = None;

    let map_len = r.read_map_header()?;
    let n_entries = match map_len {
        Some(n) => n as i64,
        None => -1, // indefinite map
    };

    let mut i = 0i64;
    loop {
        if n_entries >= 0 && i >= n_entries {
            break;
        }
        if n_entries < 0 {
            // Check for break byte
            let ty = r.peek_major()?;
            if ty == Type::Break {
                // Indefinite-length map: break byte signals end.
                // Skip the break byte and stop.
                r.skip()?;
                break;
            }
        }
        i += 1;

        let key = r.read_uint()?;
        match key {
            0 => {
                // inputs: [* transaction_input]
                inputs = r.read_array(read_shelley_tx_input)?;
            }
            1 => {
                // outputs: [* transaction_output]
                outputs = r.read_array(|r| read_shelley_tx_output(r))?;
            }
            2 => {
                // fee
                fee = read_lovelace(r)?;
            }
            3 => {
                // TTL
                ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                // certificates
                certificates = r.read_array(|r| read_shelley_certificate(r))?;
            }
            5 => {
                // withdrawals: { reward_account => coin }
                withdrawals = read_withdrawals(r)?;
            }
            6 => {
                // update proposals — skip for Shelley compatibility
                r.skip()?;
                update = None; // TODO(M4a-2): decode Shelley update proposals if needed
            }
            7 => {
                // auxiliary_data_hash
                auxiliary_data_hash = Some(read_hash32(r)?);
            }
            _ => {
                // Unknown key — skip value (forward compatibility)
                r.skip()?;
            }
        }
    }

    Ok(TransactionBody {
        inputs,
        outputs,
        fee,
        ttl,
        certificates,
        withdrawals,
        auxiliary_data_hash,
        validity_interval_start: None, // added in Allegra (key 8)
        mint: BTreeMap::new(),         // added in Mary (key 9)
        script_data_hash: None,        // added in Alonzo
        collateral: Vec::new(),        // added in Alonzo
        required_signers: Vec::new(),  // added in Alonzo
        network_id: None,              // added in Alonzo
        collateral_return: None,       // added in Babbage
        total_collateral: None,        // added in Babbage
        reference_inputs: Vec::new(),  // added in Babbage
        update,
        voting_procedures: BTreeMap::new(), // Conway+
        proposal_procedures: Vec::new(),    // Conway+
        treasury_value: None,
        donation: None,
        sub_transactions: Vec::new(),          // Dijkstra+
        account_balance_intervals: Vec::new(), // Dijkstra+
        direct_deposits: BTreeMap::new(),      // Dijkstra+
    })
}

/// Read a Shelley `transaction_input = [txhash, index]`.
fn read_shelley_tx_input(r: &mut Reader<'_>) -> Result<TransactionInput, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "shelley tx_in: expected array(2), got {arr_len:?}"
        )));
    }
    let tx_hash = read_hash32(r)?;
    let index = r.read_uint()? as u32;
    Ok(TransactionInput {
        transaction_id: tx_hash,
        index,
    })
}

/// Read a Shelley/Allegra/Mary `transaction_output = [address, amount]`.
///
/// Shelley outputs are 2-element arrays: `[address_bytes, coin_or_value]`.
/// The value is either a plain uint (ADA only) or `[coin, multiasset_map]` (Mary+).
fn read_shelley_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(2) | Some(3) => {}
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "shelley tx_out: expected array(2) or array(3), got {arr_len:?}"
            )));
        }
    }
    let n = arr_len.unwrap();

    // Address bytes
    let addr_bytes = r.read_bytes()?.to_vec();
    let address = Address::from_bytes(&addr_bytes)
        .map_err(|e| SerializationError::InvalidData(format!("shelley output address: {e}")))?;

    // Value: uint (ADA-only) or [coin, multiasset_map] (Mary+)
    let value = read_shelley_value(r)?;

    // Optional datum hash (pre-Alonzo extension — Shelley CDDL allows it)
    let datum = if n == 3 {
        let dh = read_hash32(r)?;
        OutputDatum::DatumHash(dh)
    } else {
        OutputDatum::None
    };

    Ok(TransactionOutput {
        address,
        value,
        datum,
        script_ref: None,
        is_legacy: true,
        raw_cbor: None,
    })
}

/// Read a Shelley/Mary value: either a plain uint or `[coin, multiasset_map]`.
fn read_shelley_value(r: &mut Reader<'_>) -> Result<Value, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let coin = r.read_uint()?;
            Ok(Value::lovelace(coin))
        }
        Type::Array => {
            // [coin, multiasset_map]
            let arr_len = r.read_array_header()?;
            if !matches!(arr_len, Some(2)) {
                return Err(SerializationError::CborDecode(format!(
                    "shelley value array: expected array(2), got {arr_len:?}"
                )));
            }
            let coin = Lovelace(r.read_uint()?);
            // multiasset_map: { policy_id => { asset_name => quantity } }
            let multi_asset = read_multiasset_map_u64(r)?;
            if multi_asset.is_empty() {
                Ok(Value::lovelace(coin.0))
            } else {
                Ok(Value { coin, multi_asset })
            }
        }
        other => Err(SerializationError::CborDecode(format!(
            "shelley value: expected uint or array, got {other}"
        ))),
    }
}

/// Read a multiasset map: `{ policy_id(28) => { asset_name => uint } }`.
fn read_multiasset_map_u64(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Hash28, BTreeMap<AssetName, u64>>, SerializationError> {
    let mut result = BTreeMap::new();
    let n = r.read_map_header()?;
    let count = n.unwrap_or(0) as usize;
    for _ in 0..count {
        // policy_id: 28 bytes
        let policy_bytes = r.read_bytes()?;
        let policy =
            Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                expected: 28,
                got: policy_bytes.len(),
            })?;
        // { asset_name => quantity }
        let mut assets: BTreeMap<AssetName, u64> = BTreeMap::new();
        let an = r.read_map_header()?;
        let asset_count = an.unwrap_or(0) as usize;
        for _ in 0..asset_count {
            let name_bytes = r.read_bytes()?.to_vec();
            let qty = r.read_uint()?;
            let asset_name = AssetName::new(name_bytes).map_err(|_| {
                SerializationError::CborDecode("multiasset: asset name too long".into())
            })?;
            assets.insert(asset_name, qty);
        }
        result.insert(policy, assets);
    }
    Ok(result)
}

/// Read `withdrawals = { reward_account_bytes => coin }`.
fn read_withdrawals(r: &mut Reader<'_>) -> Result<BTreeMap<Vec<u8>, Lovelace>, SerializationError> {
    let mut result = BTreeMap::new();
    let n = r.read_map_header()?;
    let count = n.unwrap_or(0) as usize;
    for _ in 0..count {
        let account = r.read_bytes()?.to_vec();
        let coin = Lovelace(r.read_uint()?);
        result.insert(account, coin);
    }
    Ok(result)
}

// ============================================================================
// Certificate decoder (Shelley/Alonzo era certificates)
// ============================================================================

/// Read a single Shelley-era certificate.
///
/// Shelley CDDL:
/// ```text
/// certificate = [0, stake_credential]             ; StakeRegistration
///             / [1, stake_credential]             ; StakeDeregistration
///             / [2, stake_credential, pool_hash]  ; StakeDelegation
///             / [3, pool_params]                  ; PoolRegistration
///             / [4, pool_hash, epoch]             ; PoolRetirement
///             / [5, genesis_hash, delegate_hash, vrf_keyhash]  ; GenesisKeyDelegation
///             / [6, move_instantaneous_reward]    ; MIR
/// ```
fn read_shelley_certificate(r: &mut Reader<'_>) -> Result<Certificate, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "certificate: expected definite-length array".into(),
        ));
    }
    let cert_type = r.read_uint()?;
    match cert_type {
        0 => {
            // StakeRegistration
            let cred = read_stake_credential(r)?;
            Ok(Certificate::StakeRegistration(cred))
        }
        1 => {
            // StakeDeregistration
            let cred = read_stake_credential(r)?;
            Ok(Certificate::StakeDeregistration(cred))
        }
        2 => {
            // StakeDelegation
            let cred = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            Ok(Certificate::StakeDelegation {
                credential: cred,
                pool_hash,
            })
        }
        3 => {
            // PoolRegistration — pool_params
            let params = read_pool_params(r)?;
            Ok(Certificate::PoolRegistration(params))
        }
        4 => {
            // PoolRetirement
            let pool_hash = read_hash28_cert(r)?;
            let epoch = r.read_uint()?;
            Ok(Certificate::PoolRetirement { pool_hash, epoch })
        }
        5 => {
            // GenesisKeyDelegation
            let genesis_hash = read_hash32(r)?;
            let delegate_hash = read_hash32(r)?;
            let vrf_keyhash = read_hash32(r)?;
            Ok(Certificate::GenesisKeyDelegation {
                genesis_hash,
                genesis_delegate_hash: delegate_hash,
                vrf_keyhash,
            })
        }
        6 => {
            // MoveInstantaneousRewardsCert
            let cert = read_mir_cert(r)?;
            Ok(cert)
        }
        other => {
            // Unknown certificate type — skip remaining fields
            Err(SerializationError::CborDecode(format!(
                "certificate: unknown type {other}"
            )))
        }
    }
}

/// Read a stake_credential = [0, keyhash] | [1, script_hash].
fn read_stake_credential(r: &mut Reader<'_>) -> Result<Credential, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "stake_credential: expected array(2), got {arr_len:?}"
        )));
    }
    let disc = r.read_uint()?;
    let hash = read_hash28_cert(r)?;
    match disc {
        0 => Ok(Credential::VerificationKey(hash)),
        1 => Ok(Credential::Script(hash)),
        other => Err(SerializationError::CborDecode(format!(
            "stake_credential: unknown discriminator {other}"
        ))),
    }
}

/// Read a 28-byte hash used in certificates (pool hash, credential hash).
fn read_hash28_cert(r: &mut Reader<'_>) -> Result<Hash28, SerializationError> {
    let bytes = r.read_bytes()?;
    Hash28::try_from(bytes).map_err(|_| SerializationError::InvalidLength {
        expected: 28,
        got: bytes.len(),
    })
}

/// Read pool_params from the register pool certificate.
fn read_pool_params(r: &mut Reader<'_>) -> Result<PoolParams, SerializationError> {
    // pool_params = (
    //   operator: pool_keyhash,
    //   vrf_keyhash: vrf_keyhash,
    //   pledge: coin,
    //   cost: coin,
    //   margin: unit_interval,
    //   reward_account: reward_account_bytes,
    //   pool_owners: [* addr_keyhash],
    //   relays: [* relay],
    //   pool_metadata: pool_metadata / null,
    // )
    // Note: pool_params is NOT wrapped in an array — they are inline elements
    // following the cert type discriminator in the [3, ...] array.
    let operator = read_hash28_cert(r)?;
    let vrf_keyhash = read_hash32(r)?;
    let pledge = read_lovelace(r)?;
    let cost = read_lovelace(r)?;
    // margin: unit_interval = tag(30)[numerator, denominator]
    let margin = r.read_rational()?;
    let reward_account = r.read_bytes()?.to_vec();
    // pool_owners: set of addr_keyhash (28 bytes each)
    let pool_owners: Vec<Hash28> = r.read_set(|r| read_hash28_cert(r))?;
    // relays: array of relay structs
    let relays_count = r.read_array_header()?.unwrap_or(0) as usize;
    for _ in 0..relays_count {
        r.skip()?; // TODO(M4a-2): decode relay structs
    }
    // pool_metadata: null or [url, hash]
    let pool_metadata = read_pool_metadata(r)?;

    Ok(PoolParams {
        operator,
        vrf_keyhash,
        pledge,
        cost,
        margin: Rational {
            numerator: margin.numerator,
            denominator: margin.denominator,
        },
        reward_account,
        pool_owners,
        relays: Vec::new(), // TODO(M4a-2): decode relay structs
        pool_metadata,
    })
}

fn read_pool_metadata(r: &mut Reader<'_>) -> Result<Option<PoolMetadata>, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    // [url, hash(32)]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "pool_metadata: expected array(2) or null, got {arr_len:?}"
        )));
    }
    // pool_metadata_url = text (CDDL). Mainnet uses CBOR major type 3 (text string).
    // Handle both text and bytes for robustness against non-canonical encodings.
    let ty = r.peek_major()?;
    let url = match ty {
        Type::String => r.read_str()?.to_string(),
        _ => {
            let url_bytes = r.read_bytes()?;
            String::from_utf8(url_bytes.to_vec()).map_err(|_| {
                SerializationError::CborDecode("pool_metadata url: invalid UTF-8".into())
            })?
        }
    };
    let hash = {
        let bytes = r.read_bytes()?;
        // Pool metadata hash can be 32 bytes
        let mut buf = [0u8; 32];
        let len = bytes.len().min(32);
        buf[..len].copy_from_slice(&bytes[..len]);
        Hash32::from_bytes(buf)
    };
    Ok(Some(PoolMetadata { url, hash }))
}

/// Read a MoveInstantaneousRewards certificate.
fn read_mir_cert(r: &mut Reader<'_>) -> Result<Certificate, SerializationError> {
    // move_instantaneous_reward = [0/1, { stake_credential => delta_coin } / coin]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "mir: expected array(2), got {arr_len:?}"
        )));
    }
    let source_disc = r.read_uint()?;
    let source = match source_disc {
        0 => MIRSource::Reserves,
        1 => MIRSource::Treasury,
        other => {
            return Err(SerializationError::CborDecode(format!(
                "mir: unknown source {other}"
            )));
        }
    };
    // target: map or coin
    let ty = r.peek_major()?;
    let target = match ty {
        Type::Map => {
            let mut creds = Vec::new();
            let n = r.read_map_header()?.unwrap_or(0) as usize;
            for _ in 0..n {
                let cred = read_stake_credential(r)?;
                let delta = r.read_int()? as i64;
                creds.push((cred, delta));
            }
            MIRTarget::StakeCredentials(creds)
        }
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let coin = r.read_uint()?;
            MIRTarget::OtherAccountingPot(coin)
        }
        other => {
            return Err(SerializationError::CborDecode(format!(
                "mir target: expected map or uint, got {other}"
            )));
        }
    };
    Ok(Certificate::MoveInstantaneousRewards { source, target })
}

// ============================================================================
// Witness set decoder
// ============================================================================

/// Decode a Shelley witness set.
///
/// ```text
/// transaction_witness_set = {
///   ? 0 : [* vkeywitness],       ; vkey_witnesses
///   ? 1 : [* native_script],     ; native_scripts
///   ? 2 : [* bootstrap_witness], ; bootstrap_witnesses (Byron-era compatibility)
/// }
/// ```
fn decode_shelley_witness_set(
    r: &mut Reader<'_>,
) -> Result<TransactionWitnessSet, SerializationError> {
    let mut vkey_witnesses: Vec<VKeyWitness> = Vec::new();
    let mut native_scripts = Vec::new();
    let mut bootstrap_witnesses = Vec::new();

    let map_len = r.read_map_header()?;
    let n_entries = map_len.unwrap_or(0) as usize;

    for _ in 0..n_entries {
        let key = r.read_uint()?;
        match key {
            0 => {
                // vkey_witnesses: [* [vkey, signature]]
                vkey_witnesses = r.read_array(|r| {
                    let arr_len = r.read_array_header()?;
                    if !matches!(arr_len, Some(2)) {
                        return Err(SerializationError::CborDecode(
                            "vkeywitness: expected array(2)".into(),
                        ));
                    }
                    let vkey = r.read_bytes()?.to_vec();
                    let signature = r.read_bytes()?.to_vec();
                    Ok(VKeyWitness { vkey, signature })
                })?;
            }
            1 => {
                // native_scripts
                native_scripts = r.read_array(|r| read_native_script(r))?;
            }
            2 => {
                // bootstrap_witnesses: [* [vkey, sig, chain_code, attributes]]
                bootstrap_witnesses = r.read_array(|r| {
                    let arr_len = r.read_array_header()?;
                    if !matches!(arr_len, Some(4)) {
                        return Err(SerializationError::CborDecode(
                            "bootstrap_witness: expected array(4)".into(),
                        ));
                    }
                    let vkey = r.read_bytes()?.to_vec();
                    let sig = r.read_bytes()?.to_vec();
                    let chain_code = r.read_bytes()?.to_vec();
                    let attrs = r.read_bytes()?.to_vec();
                    Ok(BootstrapWitness {
                        vkey,
                        signature: sig,
                        chain_code,
                        attributes: attrs,
                    })
                })?;
            }
            _ => {
                r.skip()?;
            }
        }
    }

    Ok(TransactionWitnessSet {
        vkey_witnesses,
        native_scripts,
        bootstrap_witnesses,
        plutus_v1_scripts: Vec::new(),
        plutus_v2_scripts: Vec::new(),
        plutus_v3_scripts: Vec::new(),
        plutus_data: Vec::new(),
        redeemers: Vec::new(),
        raw_redeemers_cbor: None,
        raw_plutus_data_cbor: None,
        original_script_data_hash: None,
    })
}

/// Decode a native script.
///
/// ```text
/// native_script = [0, addr_keyhash]   ; ScriptPubkey
///               / [1, [* native_script]] ; ScriptAll
///               / [2, [* native_script]] ; ScriptAny
///               / [3, n, [* native_script]] ; ScriptNOfK
///               / [4, slot]           ; InvalidBefore
///               / [5, slot]           ; InvalidHereafter
/// ```
fn read_native_script(r: &mut Reader<'_>) -> Result<NativeScript, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "native_script: expected definite-length array".into(),
        ));
    }
    let disc = r.read_uint()?;
    match disc {
        0 => {
            // ScriptPubkey: hash28 → padded to Hash32
            let h28 = read_hash28_cert(r)?;
            Ok(NativeScript::ScriptPubkey(h28.to_hash32_padded()))
        }
        1 => {
            let scripts = r.read_array(read_native_script)?;
            Ok(NativeScript::ScriptAll(scripts))
        }
        2 => {
            let scripts = r.read_array(read_native_script)?;
            Ok(NativeScript::ScriptAny(scripts))
        }
        3 => {
            let n = r.read_uint()? as u32;
            let scripts = r.read_array(read_native_script)?;
            Ok(NativeScript::ScriptNOfK(n, scripts))
        }
        4 => {
            let slot = r.read_uint()?;
            Ok(NativeScript::InvalidBefore(SlotNo(slot)))
        }
        5 => {
            let slot = r.read_uint()?;
            Ok(NativeScript::InvalidHereafter(SlotNo(slot)))
        }
        other => Err(SerializationError::CborDecode(format!(
            "native_script: unknown type {other}"
        ))),
    }
}

// ============================================================================
// Auxiliary data decoder
// ============================================================================

/// Decode the auxiliary_data_set map: `{ tx_index => auxiliary_data }`.
///
/// Returns a `BTreeMap<u32, AuxiliaryData>` keyed by transaction index.
fn decode_aux_data_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u32, AuxiliaryData>, SerializationError> {
    let mut result = BTreeMap::new();
    let n = r.read_map_header()?;
    let count = n.unwrap_or(0) as usize;
    for _ in 0..count {
        let tx_idx = r.read_uint()? as u32;
        let aux = decode_auxiliary_data(r)?;
        result.insert(tx_idx, aux);
    }
    Ok(result)
}

/// Decode a Shelley/Mary auxiliary data value.
///
/// Shelley auxiliary data is just a metadata map: `{ label => metadatum }`.
/// Mary (ShelleyMa) aux data is `[metadata, native_scripts]`.
/// Alonzo+ aux data is a tag(259) map.
///
/// For Shelley, the aux data is always the plain metadata map form.
fn decode_auxiliary_data(r: &mut Reader<'_>) -> Result<AuxiliaryData, SerializationError> {
    // Capture raw CBOR bytes
    let raw_start = r.position();
    r.skip()?;
    let raw_bytes = r.slice_from(raw_start).to_vec();

    // Re-parse the aux data to extract metadata
    let mut aux_r = Reader::new(&raw_bytes);
    let ty = aux_r.peek_major()?;
    let metadata = match ty {
        Type::Map => {
            // Plain Shelley metadata map
            decode_metadata_map(&mut aux_r)?
        }
        Type::Array => {
            // ShelleyMa: [metadata_map, native_scripts]
            let arr_len = aux_r.read_array_header()?;
            if !matches!(arr_len, Some(2)) {
                // Unexpected format — return empty
                BTreeMap::new()
            } else {
                let meta = decode_metadata_map(&mut aux_r)?;
                // skip native scripts
                aux_r.skip()?;
                meta
            }
        }
        _ => BTreeMap::new(),
    };

    Ok(AuxiliaryData {
        metadata,
        native_scripts: Vec::new(),
        plutus_v1_scripts: Vec::new(),
        plutus_v2_scripts: Vec::new(),
        plutus_v3_scripts: Vec::new(),
        raw_cbor: Some(raw_bytes),
    })
}

fn decode_metadata_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u64, TransactionMetadatum>, SerializationError> {
    let mut result = BTreeMap::new();
    let n = r.read_map_header()?;
    let count = n.unwrap_or(0) as usize;
    for _ in 0..count {
        let label = r.read_uint()?;
        let value = read_metadatum(r)?;
        result.insert(label, value);
    }
    Ok(result)
}

fn read_metadatum(r: &mut Reader<'_>) -> Result<TransactionMetadatum, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Map => {
            let entries = r.read_map(read_metadatum, read_metadatum)?;
            Ok(TransactionMetadatum::Map(entries))
        }
        Type::Array => {
            let items = r.read_array(read_metadatum)?;
            Ok(TransactionMetadatum::List(items))
        }
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let v = r.read_uint()?;
            Ok(TransactionMetadatum::Int(v as i128))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
            let v = r.read_int()?;
            Ok(TransactionMetadatum::Int(v))
        }
        Type::Bytes => {
            let bytes = r.read_bytes()?.to_vec();
            Ok(TransactionMetadatum::Bytes(bytes))
        }
        Type::String => {
            let s = r.read_str()?.to_string();
            Ok(TransactionMetadatum::Text(s))
        }
        other => {
            // Unexpected type — skip
            Err(SerializationError::CborDecode(format!(
                "metadatum: unexpected type {other}"
            )))
        }
    }
}

// ============================================================================
// Standalone tx decoder (Shelley era)
// ============================================================================

/// Decode a standalone Shelley-era transaction from raw CBOR bytes.
///
/// The standalone tx format is `[body_map, witness_set_map, is_valid_bool, aux_data]`.
/// Pallas encodes Shelley txs in a 4-element array (with `is_valid` as the 3rd
/// element, always `true` for Shelley since invalid txs weren't introduced until Alonzo).
///
/// The transaction hash is `blake2b_256(raw_body_cbor)`.
pub(crate) fn decode_shelley_tx_standalone(cbor: &[u8]) -> Result<Transaction, SerializationError> {
    let mut r = Reader::new(cbor);

    // tx = [body, witness_set, is_valid, aux_data]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(4) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "shelley tx: expected array(4), got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(
                "shelley tx: expected definite-length array".into(),
            ));
        }
    }

    // 1. Body — capture raw bytes for hash computation
    let body_raw = KeepRaw::parse_with(&mut r, |r| decode_shelley_tx_body(r))?;
    let raw_body_cbor = body_raw.raw.to_vec();
    let tx_hash = blake2b_256(&raw_body_cbor);
    let body = body_raw.value;

    // 2. Witness set
    let ws_raw = KeepRaw::parse_with(&mut r, |r| decode_shelley_witness_set(r))?;
    let raw_witness_cbor = ws_raw.raw.to_vec();
    let witness_set = ws_raw.value;

    // 3. is_valid — Shelley always true; read but ignore
    let ty = r.peek_major()?;
    if ty == minicbor::data::Type::Bool {
        let _is_valid = r.read_bool()?;
    } else {
        r.skip()?;
    }

    // 4. Auxiliary data (null or a map)
    let auxiliary_data = {
        let ty = r.peek_major()?;
        if ty == minicbor::data::Type::Null {
            r.read_null()?;
            None
        } else {
            Some(decode_auxiliary_data(&mut r)?)
        }
    };

    Ok(Transaction {
        hash: tx_hash,
        era: Era::Shelley,
        body,
        witness_set,
        is_valid: true,
        auxiliary_data,
        raw_cbor: Some(cbor.to_vec()),
        raw_body_cbor: Some(raw_body_cbor),
        raw_witness_cbor: Some(raw_witness_cbor),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CBOR encoding helpers
    // -----------------------------------------------------------------------

    fn cbor_uint(n: u64) -> Vec<u8> {
        if n == 0 {
            vec![0x00]
        } else if n <= 23 {
            vec![n as u8]
        } else if n <= 0xff {
            vec![0x18, n as u8]
        } else if n <= 0xffff {
            let b = (n as u16).to_be_bytes();
            vec![0x19, b[0], b[1]]
        } else {
            let b = n.to_be_bytes();
            vec![0x1b, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
        }
    }

    fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        if b.len() <= 23 {
            let mut v = vec![0x40 | b.len() as u8];
            v.extend_from_slice(b);
            v
        } else if b.len() <= 0xff {
            let mut v = vec![0x58, b.len() as u8];
            v.extend_from_slice(b);
            v
        } else {
            let l = b.len() as u16;
            let lb = l.to_be_bytes();
            let mut v = vec![0x59, lb[0], lb[1]];
            v.extend_from_slice(b);
            v
        }
    }

    fn cbor_arr(items: &[&[u8]]) -> Vec<u8> {
        assert!(items.len() <= 23);
        let mut v = vec![0x80 | items.len() as u8];
        for item in items {
            v.extend_from_slice(item);
        }
        v
    }

    fn cbor_map0() -> Vec<u8> {
        vec![0xa0]
    }

    fn cbor_null() -> Vec<u8> {
        vec![0xf6]
    }

    /// Build a minimal Shelley block CBOR for testing (inner, after envelope stripping).
    ///
    /// Uses a minimal but structurally correct block with:
    /// - 15-element header_body
    /// - Empty tx arrays
    /// - Empty aux data map
    fn make_shelley_block(n_txs: usize) -> Vec<u8> {
        // VRF cert = [[output_bytes(64)], [proof_bytes(80)]] — but as array(2) of 2 bytestrings
        let vrf_output = cbor_bytes(&[0u8; 64]);
        let vrf_proof = cbor_bytes(&[0u8; 80]);
        let vrf_cert = cbor_arr(&[&vrf_output, &vrf_proof]);

        // tag(30)[0, 1] — rational 0/1
        let rational = {
            let mut v = vec![0xd8, 0x1e]; // tag(30)
            v.extend(cbor_arr(&[&cbor_uint(0), &cbor_uint(1)]));
            v
        };
        let _ = rational; // unused in this minimal block

        // Header body: 15 fields
        let block_number_bytes = cbor_uint(99);
        let slot_bytes = cbor_uint(123456);
        let prev_hash_bytes = cbor_bytes(&[0xab; 32]);
        let issuer_vkey_bytes = cbor_bytes(&[0x01; 32]);
        let vrf_vkey_bytes = cbor_bytes(&[0x02; 32]);
        let nonce_vrf_bytes = vrf_cert.clone();
        let leader_vrf_bytes = vrf_cert;
        let body_size_bytes = cbor_uint(0);
        let body_hash_bytes = cbor_bytes(&[0x00; 32]);
        let op_hot_vkey_bytes = cbor_bytes(&[0x03; 32]);
        let op_seq_bytes = cbor_uint(0);
        let op_kes_bytes = cbor_uint(0);
        let op_sigma_bytes = cbor_bytes(&[0x04; 64]);
        let proto_major_bytes = cbor_uint(2);
        let proto_minor_bytes = cbor_uint(0);

        // header_body = array(15) of the above
        let mut hb = vec![0x8f]; // array(15)
        hb.extend(&block_number_bytes);
        hb.extend(&slot_bytes);
        hb.extend(&prev_hash_bytes);
        hb.extend(&issuer_vkey_bytes);
        hb.extend(&vrf_vkey_bytes);
        hb.extend(&nonce_vrf_bytes);
        hb.extend(&leader_vrf_bytes);
        hb.extend(&body_size_bytes);
        hb.extend(&body_hash_bytes);
        hb.extend(&op_hot_vkey_bytes);
        hb.extend(&op_seq_bytes);
        hb.extend(&op_kes_bytes);
        hb.extend(&op_sigma_bytes);
        hb.extend(&proto_major_bytes);
        hb.extend(&proto_minor_bytes);

        let kes_sig = cbor_bytes(&[0x05; 448]);
        // header = [header_body, kes_sig]
        let mut header = vec![0x82];
        header.extend(&hb);
        header.extend(&kes_sig);

        // tx_bodies = array(n_txs) of minimal tx bodies
        let mut tx_bodies_header = Vec::new();
        let mut tx_witness_header = Vec::new();
        if n_txs <= 23 {
            tx_bodies_header.push(0x80 | n_txs as u8);
            tx_witness_header.push(0x80 | n_txs as u8);
        }
        let mut tx_bodies = tx_bodies_header;
        let mut tx_witnesses = tx_witness_header;
        for _ in 0..n_txs {
            // Minimal tx body: map with fee(key=2) and inputs(key=0) and outputs(key=1)
            // {0: [], 1: [], 2: 1000000}
            let mut tb = vec![0xa3]; // map(3)
            tb.extend(&cbor_uint(0)); // key 0
            tb.push(0x80); // array(0) inputs
            tb.extend(&cbor_uint(1)); // key 1
            tb.push(0x80); // array(0) outputs
            tb.extend(&cbor_uint(2)); // key 2
            tb.extend(&cbor_uint(1_000_000)); // fee
            tx_bodies.extend(&tb);

            // Empty witness set: {}
            tx_witnesses.push(0xa0); // map(0)
        }

        // aux_data_set = {} (empty map)
        let aux_data = cbor_map0();

        // block = [header, tx_bodies, tx_witnesses, aux_data]
        let mut block = vec![0x84]; // array(4)
        block.extend(&header);
        block.extend(&tx_bodies);
        block.extend(&tx_witnesses);
        block.extend(&aux_data);
        block
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn shelley_empty_block_decodes() {
        let cbor = make_shelley_block(0);
        let block = decode_shelley_block(&cbor).unwrap();
        assert_eq!(block.era, Era::Shelley);
        assert_eq!(block.transactions.len(), 0);
        assert_eq!(block.header.slot.0, 123456);
        assert_eq!(block.header.block_number.0, 99);
    }

    #[test]
    fn shelley_single_tx_block_decodes() {
        let cbor = make_shelley_block(1);
        let block = decode_shelley_block(&cbor).unwrap();
        assert_eq!(block.era, Era::Shelley);
        assert_eq!(block.transactions.len(), 1);
        assert_eq!(block.transactions[0].era, Era::Shelley);
        assert_eq!(block.transactions[0].body.fee, Lovelace(1_000_000));
    }

    #[test]
    fn shelley_multi_tx_block_decodes() {
        let cbor = make_shelley_block(3);
        let block = decode_shelley_block(&cbor).unwrap();
        assert_eq!(block.transactions.len(), 3);
    }

    #[test]
    fn shelley_header_hash_is_blake2b256_of_header_cbor() {
        let cbor = make_shelley_block(0);
        let block = decode_shelley_block(&cbor).unwrap();

        // Verify: hash should be blake2b_256 of the header portion of the CBOR.
        // The header starts after the array(4) header byte. Parse to get it.
        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        let header_start = r.position();
        r.skip().unwrap(); // skip the header value
        let header_bytes = r.slice_from(header_start);
        let expected_hash = blake2b_256(header_bytes);
        assert_eq!(block.header.header_hash, expected_hash);
    }

    #[test]
    fn shelley_tx_hash_is_blake2b256_of_body_cbor() {
        let cbor = make_shelley_block(1);
        let block = decode_shelley_block(&cbor).unwrap();
        assert_eq!(block.transactions.len(), 1);

        // Extract the tx body bytes from the CBOR
        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        r.skip().unwrap(); // skip header
                           // tx_bodies = array(1)[tx_body]
        r.read_array_header().unwrap();
        let body_start = r.position();
        r.skip().unwrap();
        let body_bytes = r.slice_from(body_start);
        let expected_hash = blake2b_256(body_bytes);
        assert_eq!(block.transactions[0].hash, expected_hash);
    }

    #[test]
    fn shelley_minimal_mode_skips_witnesses() {
        let cbor = make_shelley_block(1);
        let block = decode_shelley_block_minimal(&cbor).unwrap();
        // Witness set should be empty in minimal mode
        assert!(block.transactions[0].witness_set.vkey_witnesses.is_empty());
        assert!(block.transactions[0].witness_set.native_scripts.is_empty());
    }

    #[test]
    fn shelley_malformed_outer_array_rejected() {
        // array(3) instead of array(4)
        let data = vec![0x83, 0x00, 0x00, 0x00];
        let result = decode_shelley_block(&data);
        assert!(
            result.is_err(),
            "decode_shelley_block(array(3)) should fail but returned {result:?}"
        );
    }

    #[test]
    fn shelley_malformed_header_body_rejected() {
        // block array(4) with header that has array(2) body instead of array(15)
        let mut data = vec![0x84]; // array(4)
                                   // header = [header_body, sig]
        data.push(0x82); // array(2)
                         // header_body = array(2) — wrong length
        data.extend_from_slice(&[0x82, 0x00, 0x00]);
        // rest doesn't matter
        data.extend_from_slice(&[0x80, 0x80, 0xa0]);
        let result = decode_shelley_block(&data);
        assert!(result.is_err());
    }

    #[test]
    fn shelley_prev_hash_zero_for_genesis() {
        // A block with null prev_hash (genesis-adjacent)
        // Build the header body with null at position 2 (prev_hash)
        let vrf_output = cbor_bytes(&[0u8; 64]);
        let vrf_proof = cbor_bytes(&[0u8; 80]);
        let vrf_cert: Vec<u8> = {
            let mut v = vec![0x82];
            v.extend(&vrf_output);
            v.extend(&vrf_proof);
            v
        };
        let mut hb = vec![0x8f]; // array(15)
        hb.extend(&cbor_uint(0)); // block_number
        hb.extend(&cbor_uint(0)); // slot
        hb.extend(&cbor_null()); // prev_hash = null
        hb.extend(&cbor_bytes(&[0u8; 32])); // issuer_vkey
        hb.extend(&cbor_bytes(&[0u8; 32])); // vrf_vkey
        hb.extend(&vrf_cert); // nonce_vrf
        hb.extend(&vrf_cert); // leader_vrf
        hb.extend(&cbor_uint(0)); // body_size
        hb.extend(&cbor_bytes(&[0u8; 32])); // body_hash
        hb.extend(&cbor_bytes(&[0u8; 32])); // op_hot_vkey
        hb.extend(&cbor_uint(0)); // op_seq
        hb.extend(&cbor_uint(0)); // op_kes
        hb.extend(&cbor_bytes(&[0u8; 64])); // op_sigma
        hb.extend(&cbor_uint(2)); // proto_major
        hb.extend(&cbor_uint(0)); // proto_minor
        let kes_sig = cbor_bytes(&[0u8; 448]);
        let mut header = vec![0x82];
        header.extend(&hb);
        header.extend(&kes_sig);

        let mut block = vec![0x84]; // array(4)
        block.extend(&header);
        block.push(0x80); // empty tx_bodies
        block.push(0x80); // empty tx_witnesses
        block.push(0xa0); // empty aux_data

        let decoded = decode_shelley_block(&block).unwrap();
        assert_eq!(decoded.header.prev_hash, Hash32::ZERO);
    }

    #[test]
    fn shelley_is_valid_always_true() {
        let cbor = make_shelley_block(1);
        let block = decode_shelley_block(&cbor).unwrap();
        assert!(block.transactions[0].is_valid);
    }

    #[test]
    fn shelley_vrf_fields_populated() {
        let cbor = make_shelley_block(0);
        let block = decode_shelley_block(&cbor).unwrap();
        // leader_vrf output = [0u8; 64]
        assert_eq!(block.header.vrf_result.output, vec![0u8; 64]);
        assert_eq!(block.header.vrf_result.proof, vec![0u8; 80]);
        // nonce_vrf output = [0u8; 64]
        assert_eq!(block.header.nonce_vrf_output, vec![0u8; 64]);
        assert_eq!(block.header.nonce_vrf_proof, vec![0u8; 80]);
    }

    #[test]
    fn shelley_op_cert_fields_populated() {
        let cbor = make_shelley_block(0);
        let block = decode_shelley_block(&cbor).unwrap();
        assert_eq!(block.header.operational_cert.hot_vkey, vec![0x03u8; 32]);
        assert_eq!(block.header.operational_cert.sequence_number, 0);
        assert_eq!(block.header.operational_cert.kes_period, 0);
        assert_eq!(block.header.operational_cert.sigma, vec![0x04u8; 64]);
    }

    // ── tx_body optional keys 3-7 ──────────────────────────────────────────

    /// Build a Shelley block carrying one tx with an arbitrary body-map content
    /// inserted after the mandatory keys 0/1/2. `extra_entries` is appended
    /// directly to the body map and the map header is updated to match.
    fn shelley_block_with_tx_body(extra_entries: &[u8], extra_count: usize) -> Vec<u8> {
        let _ = make_shelley_block(1); // sanity-check helper signature
                                       // Rebuild the block manually instead of patching offsets.

        // Re-use header from make_shelley_block(0) by stripping the empty txs.
        let template = make_shelley_block(0);
        // Drop the trailing aux_data + tx_witnesses + tx_bodies (3 bytes 0xa0, 0x80, 0x80).
        let header_only_end = template.len() - 3;
        let mut block = vec![0x84]; // array(4)
        block.extend_from_slice(&template[1..header_only_end]);

        // tx_bodies = array(1)[body]
        let base_keys = 3; // keys 0, 1, 2
        let total_keys = base_keys + extra_count;
        assert!(total_keys <= 23, "too many keys");
        block.push(0x81); // array(1)
        block.push(0xa0 | total_keys as u8); // map(N)
        block.extend(cbor_uint(0));
        block.push(0x80); // inputs = []
        block.extend(cbor_uint(1));
        block.push(0x80); // outputs = []
        block.extend(cbor_uint(2));
        block.extend(cbor_uint(1_000_000)); // fee
        block.extend_from_slice(extra_entries);

        block.push(0x81); // tx_witnesses = array(1)
        block.push(0xa0); // {}

        block.push(0xa0); // aux_data = {}

        block
    }

    #[test]
    fn shelley_body_key_3_ttl_decoded() {
        let mut extra = Vec::new();
        extra.extend(cbor_uint(3));
        extra.extend(cbor_uint(42));
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(block.transactions[0].body.ttl, Some(SlotNo(42)));
    }

    #[test]
    fn shelley_body_key_4_certificates_stake_registration() {
        // cert array(1) [ array(2) [0, [0, hash28]] ] — StakeRegistration KeyHash
        let mut cert = vec![0x82]; // array(2)
        cert.extend(cbor_uint(0)); // disc 0
        cert.push(0x82); // stake_credential array(2)
        cert.extend(cbor_uint(0)); // KeyHash
        cert.extend(cbor_bytes(&[0xAA; 28]));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81); // certs array(1)
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(block.transactions[0].body.certificates.len(), 1);
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::StakeRegistration(Credential::VerificationKey(_))
        ));
    }

    #[test]
    fn shelley_body_key_4_certificate_stake_deregistration_script_cred() {
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(1)); // disc 1 = StakeDeregistration
        cert.push(0x82);
        cert.extend(cbor_uint(1)); // Script
        cert.extend(cbor_bytes(&[0xBB; 28]));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::StakeDeregistration(Credential::Script(_))
        ));
    }

    #[test]
    fn shelley_body_key_4_certificate_stake_delegation() {
        let mut cert = vec![0x83]; // array(3) for delegation
        cert.extend(cbor_uint(2));
        cert.push(0x82);
        cert.extend(cbor_uint(0));
        cert.extend(cbor_bytes(&[0xCC; 28]));
        cert.extend(cbor_bytes(&[0xDD; 28])); // pool hash
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::StakeDelegation { .. }
        ));
    }

    #[test]
    fn shelley_body_key_4_certificate_pool_retirement() {
        let mut cert = vec![0x83]; // array(3)
        cert.extend(cbor_uint(4));
        cert.extend(cbor_bytes(&[0xEE; 28]));
        cert.extend(cbor_uint(123));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::PoolRetirement { epoch: 123, .. }
        ));
    }

    #[test]
    fn shelley_body_key_4_certificate_genesis_key_delegation() {
        let mut cert = vec![0x84]; // array(4)
        cert.extend(cbor_uint(5));
        cert.extend(cbor_bytes(&[0x01; 32])); // genesis hash
        cert.extend(cbor_bytes(&[0x02; 32])); // delegate hash
        cert.extend(cbor_bytes(&[0x03; 32])); // vrf keyhash
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::GenesisKeyDelegation { .. }
        ));
    }

    #[test]
    fn shelley_body_key_4_mir_certificate_reserves_to_credentials() {
        // [6, [0, {cred => delta}]]
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(6));
        // mir array(2) [source=0, target_map(1)]
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(0)); // source = Reserves
                                  // target = map(1)
        mir.push(0xa1);
        // key: stake credential [0, hash28]
        mir.push(0x82);
        mir.extend(cbor_uint(0));
        mir.extend(cbor_bytes(&[0x09; 28]));
        // value: delta = -1 (negative int)
        mir.push(0x20); // major 1, value 0 → -1
        cert.extend(&mir);
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(matches!(
            block.transactions[0].body.certificates[0],
            Certificate::MoveInstantaneousRewards {
                source: MIRSource::Reserves,
                target: MIRTarget::StakeCredentials(_),
            }
        ));
    }

    #[test]
    fn shelley_body_key_4_mir_certificate_treasury_to_other_pot() {
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(1)); // source = Treasury
        mir.extend(cbor_uint(9999)); // target = coin (uint)
        cert.extend(&mir);
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        match &block.transactions[0].body.certificates[0] {
            Certificate::MoveInstantaneousRewards {
                source: MIRSource::Treasury,
                target: MIRTarget::OtherAccountingPot(c),
            } => {
                assert_eq!(*c, 9999);
            }
            other => panic!("unexpected cert: {other:?}"),
        }
    }

    #[test]
    fn shelley_body_key_4_unknown_cert_type_rejected() {
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(99));
        cert.extend(cbor_uint(0));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        assert!(decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).is_err());
    }

    #[test]
    fn shelley_body_key_4_mir_unknown_source_rejected() {
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(2)); // unknown source
        mir.extend(cbor_uint(0));
        cert.extend(&mir);
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        assert!(decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).is_err());
    }

    #[test]
    fn shelley_body_key_5_withdrawals() {
        let mut wd = vec![0xa1]; // map(1)
        wd.extend(cbor_bytes(&[0xE0; 29])); // reward account
        wd.extend(cbor_uint(500_000));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(5));
        extra.extend(&wd);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(block.transactions[0].body.withdrawals.len(), 1);
        let &v = block.transactions[0]
            .body
            .withdrawals
            .values()
            .next()
            .unwrap();
        assert_eq!(v.0, 500_000);
    }

    #[test]
    fn shelley_body_key_6_update_proposal_skipped() {
        // key 6 = update proposals (just skip)
        let mut extra = Vec::new();
        extra.extend(cbor_uint(6));
        extra.push(0xa0); // empty map placeholder for update
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert!(block.transactions[0].body.update.is_none());
    }

    #[test]
    fn shelley_body_key_7_aux_data_hash() {
        let mut extra = Vec::new();
        extra.extend(cbor_uint(7));
        extra.extend(cbor_bytes(&[0x42; 32]));
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(
            block.transactions[0].body.auxiliary_data_hash.unwrap(),
            Hash32::from_bytes([0x42; 32])
        );
    }

    #[test]
    fn shelley_body_unknown_key_skipped() {
        // Unknown key 42 — must be skipped without error.
        let mut extra = Vec::new();
        extra.extend(cbor_uint(42));
        extra.extend(cbor_uint(0));
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(block.transactions[0].body.fee.0, 1_000_000);
    }

    // ── pool_params + pool_metadata + relays ────────────────────────────────

    #[test]
    fn shelley_pool_registration_with_metadata() {
        // PoolRegistration cert: array(2) [3, ...inline pool_params]
        // pool_params: operator, vrf, pledge, cost, margin, reward_acct, owners, relays, metadata
        let mut params = Vec::new();
        params.extend(cbor_bytes(&[0xA1; 28])); // operator
        params.extend(cbor_bytes(&[0xB2; 32])); // vrf_keyhash
        params.extend(cbor_uint(1_000_000)); // pledge
        params.extend(cbor_uint(340_000_000)); // cost
                                               // margin = tag(30) [1, 100]
        let mut margin = vec![0xd8, 0x1e, 0x82];
        margin.extend(cbor_uint(1));
        margin.extend(cbor_uint(100));
        params.extend(&margin);
        params.extend(cbor_bytes(&[0xE0; 29])); // reward_account
                                                // owners = tag(258) array(1)[hash28] OR plain array(1)[hash28]
        let mut owners = vec![0x81];
        owners.extend(cbor_bytes(&[0xC3; 28]));
        params.extend(&owners);
        // relays = array(0)
        params.push(0x80);
        // pool_metadata = [url, hash32]
        let mut pm = vec![0x82];
        pm.push(0x63); // text(3)
        pm.extend_from_slice(b"foo");
        pm.extend(cbor_bytes(&[0xD4; 32]));
        params.extend(&pm);

        // cert outer: array(10) [3, ...the 9 params elements]
        let mut cert = vec![0x8a]; // array(10) = 1 disc + 9 fields
        cert.extend(cbor_uint(3));
        cert.extend(&params);

        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81); // array(1) certs
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        let pp = match &block.transactions[0].body.certificates[0] {
            Certificate::PoolRegistration(p) => p,
            _ => panic!("expected PoolRegistration"),
        };
        assert_eq!(pp.pledge.0, 1_000_000);
        assert_eq!(pp.cost.0, 340_000_000);
        assert_eq!(pp.margin.numerator, 1);
        assert_eq!(pp.margin.denominator, 100);
        assert!(pp.pool_metadata.is_some());
        let md = pp.pool_metadata.as_ref().unwrap();
        assert_eq!(md.url, "foo");
    }

    #[test]
    fn shelley_pool_registration_with_null_metadata() {
        let mut params = Vec::new();
        params.extend(cbor_bytes(&[0xA1; 28]));
        params.extend(cbor_bytes(&[0xB2; 32]));
        params.extend(cbor_uint(0));
        params.extend(cbor_uint(0));
        let mut margin = vec![0xd8, 0x1e, 0x82];
        margin.extend(cbor_uint(0));
        margin.extend(cbor_uint(1));
        params.extend(&margin);
        params.extend(cbor_bytes(&[0xE0; 29]));
        params.push(0x80); // empty owners
        params.push(0x80); // empty relays
        params.push(0xf6); // null metadata
        let mut cert = vec![0x8a];
        cert.extend(cbor_uint(3));
        cert.extend(&params);
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        let pp = match &block.transactions[0].body.certificates[0] {
            Certificate::PoolRegistration(p) => p,
            _ => panic!("expected PoolRegistration"),
        };
        assert!(pp.pool_metadata.is_none());
    }

    // ── Witness sets: vkey, native_script, bootstrap ────────────────────────

    /// Replace the witness set in a 1-tx Shelley block. `ws_cbor` is the full
    /// witness-set map CBOR; the function wraps it in `array(1)[..]`.
    fn shelley_block_with_witness_set(ws_cbor: &[u8]) -> Vec<u8> {
        let _ = make_shelley_block(1);

        let template = make_shelley_block(0);
        // strip trailing 3 bytes (aux_data + empty witness + empty bodies).
        let header_only_end = template.len() - 3;
        let mut out = vec![0x84];
        out.extend_from_slice(&template[1..header_only_end]);
        // tx_bodies = array(1)[ minimal body ]
        out.push(0x81); // array(1)
                        // {0: [], 1: [], 2: 1_000_000}
        out.push(0xa3);
        out.extend(cbor_uint(0));
        out.push(0x80);
        out.extend(cbor_uint(1));
        out.push(0x80);
        out.extend(cbor_uint(2));
        out.extend(cbor_uint(1_000_000));
        // tx_witnesses = array(1)[ ws_cbor ]
        out.push(0x81);
        out.extend_from_slice(ws_cbor);
        // aux_data = {}
        out.push(0xa0);
        out
    }

    #[test]
    fn shelley_witness_set_vkey_decoded() {
        // ws = {0: [[vkey, sig]]}
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(0));
        // array(1) of vkey witnesses
        ws.push(0x81);
        // [vkey(32), sig(64)]
        ws.push(0x82);
        ws.extend(cbor_bytes(&[0x77; 32]));
        ws.extend(cbor_bytes(&[0x88; 64]));
        let block = decode_shelley_block(&shelley_block_with_witness_set(&ws)).unwrap();
        let w = &block.transactions[0].witness_set.vkey_witnesses;
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].vkey, vec![0x77; 32]);
        assert_eq!(w[0].signature, vec![0x88; 64]);
    }

    #[test]
    fn shelley_witness_set_bootstrap_decoded() {
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(2));
        ws.push(0x81); // array(1)
        ws.push(0x84); // [vkey, sig, chain_code, attrs]
        ws.extend(cbor_bytes(&[0x01; 32]));
        ws.extend(cbor_bytes(&[0x02; 64]));
        ws.extend(cbor_bytes(&[0x03; 32]));
        ws.extend(cbor_bytes(&[])); // empty attributes
        let block = decode_shelley_block(&shelley_block_with_witness_set(&ws)).unwrap();
        let b = &block.transactions[0].witness_set.bootstrap_witnesses;
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].vkey.len(), 32);
        assert_eq!(b[0].signature.len(), 64);
    }

    #[test]
    fn shelley_witness_set_all_native_script_variants() {
        // Build native_scripts containing one of each variant (0..=5).
        let mut scripts_arr = vec![0x86]; // array(6)
                                          // [0, hash28]
        scripts_arr.push(0x82);
        scripts_arr.extend(cbor_uint(0));
        scripts_arr.extend(cbor_bytes(&[0xAA; 28]));
        // [1, []]
        scripts_arr.push(0x82);
        scripts_arr.extend(cbor_uint(1));
        scripts_arr.push(0x80);
        // [2, []]
        scripts_arr.push(0x82);
        scripts_arr.extend(cbor_uint(2));
        scripts_arr.push(0x80);
        // [3, n=2, []]
        scripts_arr.push(0x83);
        scripts_arr.extend(cbor_uint(3));
        scripts_arr.extend(cbor_uint(2));
        scripts_arr.push(0x80);
        // [4, slot]
        scripts_arr.push(0x82);
        scripts_arr.extend(cbor_uint(4));
        scripts_arr.extend(cbor_uint(100));
        // [5, slot]
        scripts_arr.push(0x82);
        scripts_arr.extend(cbor_uint(5));
        scripts_arr.extend(cbor_uint(200));

        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(1));
        ws.extend(&scripts_arr);
        let block = decode_shelley_block(&shelley_block_with_witness_set(&ws)).unwrap();
        let ns = &block.transactions[0].witness_set.native_scripts;
        assert_eq!(ns.len(), 6);
        assert!(matches!(ns[0], NativeScript::ScriptPubkey(_)));
        assert!(matches!(ns[1], NativeScript::ScriptAll(_)));
        assert!(matches!(ns[2], NativeScript::ScriptAny(_)));
        assert!(matches!(ns[3], NativeScript::ScriptNOfK(2, _)));
        assert!(matches!(ns[4], NativeScript::InvalidBefore(SlotNo(100))));
        assert!(matches!(ns[5], NativeScript::InvalidHereafter(SlotNo(200))));
    }

    #[test]
    fn shelley_witness_set_unknown_key_skipped() {
        // ws = {99: 0} — unknown key, value uint(0). Decoder must skip and return empty ws.
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(99));
        ws.extend(cbor_uint(0));
        let block = decode_shelley_block(&shelley_block_with_witness_set(&ws)).unwrap();
        assert!(block.transactions[0].witness_set.vkey_witnesses.is_empty());
    }

    #[test]
    fn shelley_native_script_unknown_variant_rejected() {
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(1));
        ws.push(0x81); // array(1) of native_scripts
        ws.push(0x82); // [99, anything]
        ws.extend(cbor_uint(99));
        ws.extend(cbor_uint(0));
        assert!(decode_shelley_block(&shelley_block_with_witness_set(&ws)).is_err());
    }

    // ── Outputs with datum hash + multi-asset ──────────────────────────────

    #[test]
    fn shelley_output_with_datum_hash() {
        // Use a 3-element output: [address, value, datum_hash]
        let mut out = vec![0x83];
        let mut addr = vec![0x60u8]; // enterprise testnet header
        addr.extend_from_slice(&[0xCA; 28]);
        out.extend(cbor_bytes(&addr));
        out.extend(cbor_uint(5_000_000));
        out.extend(cbor_bytes(&[0xDA; 32]));

        // Build body with outputs replaced.
        let template = make_shelley_block(0);
        let header_only_end = template.len() - 3;
        let mut block = vec![0x84];
        block.extend_from_slice(&template[1..header_only_end]);
        block.push(0x81); // tx_bodies array(1)
                          // body: {0: [], 1: [out], 2: fee}
        block.push(0xa3);
        block.extend(cbor_uint(0));
        block.push(0x80);
        block.extend(cbor_uint(1));
        block.push(0x81); // array(1) of outputs
        block.extend(&out);
        block.extend(cbor_uint(2));
        block.extend(cbor_uint(1));
        // ws and aux
        block.push(0x81);
        block.push(0xa0);
        block.push(0xa0);
        let res = decode_shelley_block(&block).unwrap();
        let tx = &res.transactions[0];
        assert_eq!(tx.body.outputs.len(), 1);
        match tx.body.outputs[0].datum {
            OutputDatum::DatumHash(_) => {}
            _ => panic!("expected DatumHash"),
        }
    }

    #[test]
    fn shelley_output_with_multi_asset_value() {
        // Output value = [coin, {policy => {asset => qty}}]
        let mut value = vec![0x82];
        value.extend(cbor_uint(2_000_000));
        // multiasset map(1)
        let mut ma = vec![0xa1];
        ma.extend(cbor_bytes(&[0xAA; 28])); // policy
                                            // assets map(1)
        ma.push(0xa1);
        ma.extend(cbor_bytes(b"TKN")); // asset name
        ma.extend(cbor_uint(42));
        value.extend(&ma);

        let mut addr = vec![0x60u8];
        addr.extend_from_slice(&[0xAB; 28]);
        let mut out = vec![0x82];
        out.extend(cbor_bytes(&addr));
        out.extend(&value);

        let template = make_shelley_block(0);
        let header_only_end = template.len() - 3;
        let mut block = vec![0x84];
        block.extend_from_slice(&template[1..header_only_end]);
        block.push(0x81); // array(1) tx_bodies
        block.push(0xa3);
        block.extend(cbor_uint(0));
        block.push(0x80);
        block.extend(cbor_uint(1));
        block.push(0x81);
        block.extend(&out);
        block.extend(cbor_uint(2));
        block.extend(cbor_uint(0));
        block.push(0x81);
        block.push(0xa0);
        block.push(0xa0);
        let decoded = decode_shelley_block(&block).unwrap();
        let v = &decoded.transactions[0].body.outputs[0].value;
        assert_eq!(v.coin.0, 2_000_000);
        assert_eq!(v.multi_asset.len(), 1);
    }

    #[test]
    fn shelley_output_with_empty_multi_asset_value_yields_pure_ada() {
        // [coin, {}] → still treated as pure ADA
        let mut value = vec![0x82];
        value.extend(cbor_uint(7));
        value.push(0xa0);
        let mut addr = vec![0x60u8];
        addr.extend_from_slice(&[0xAB; 28]);
        let mut out = vec![0x82];
        out.extend(cbor_bytes(&addr));
        out.extend(&value);
        let template = make_shelley_block(0);
        let header_only_end = template.len() - 3;
        let mut block = vec![0x84];
        block.extend_from_slice(&template[1..header_only_end]);
        block.push(0x81);
        block.push(0xa3);
        block.extend(cbor_uint(0));
        block.push(0x80);
        block.extend(cbor_uint(1));
        block.push(0x81);
        block.extend(&out);
        block.extend(cbor_uint(2));
        block.extend(cbor_uint(0));
        block.push(0x81);
        block.push(0xa0);
        block.push(0xa0);
        let decoded = decode_shelley_block(&block).unwrap();
        assert!(decoded.transactions[0].body.outputs[0]
            .value
            .multi_asset
            .is_empty());
    }

    // ── Auxiliary data: Mary-form + metadata variants ──────────────────────

    /// Build a block whose aux_data_set contains one entry for tx_idx=0.
    fn shelley_block_with_aux_for_tx0(aux_cbor: &[u8]) -> Vec<u8> {
        let template = make_shelley_block(0);
        let header_only_end = template.len() - 3;
        let mut block = vec![0x84];
        block.extend_from_slice(&template[1..header_only_end]);
        block.push(0x81); // 1 tx
        block.push(0xa3);
        block.extend(cbor_uint(0));
        block.push(0x80);
        block.extend(cbor_uint(1));
        block.push(0x80);
        block.extend(cbor_uint(2));
        block.extend(cbor_uint(0));
        block.push(0x81); // ws array(1)
        block.push(0xa0);
        // aux_data_set = map(1) {0 => aux}
        block.push(0xa1);
        block.extend(cbor_uint(0));
        block.extend_from_slice(aux_cbor);
        block
    }

    #[test]
    fn shelley_aux_data_metadata_map_form() {
        // aux = { 5 => "hello" }
        let mut aux = vec![0xa1];
        aux.extend(cbor_uint(5));
        // text "hello"
        aux.push(0x65);
        aux.extend_from_slice(b"hello");
        let block = decode_shelley_block(&shelley_block_with_aux_for_tx0(&aux)).unwrap();
        let md = &block.transactions[0]
            .auxiliary_data
            .as_ref()
            .unwrap()
            .metadata;
        assert_eq!(md.len(), 1);
        match md.get(&5).unwrap() {
            TransactionMetadatum::Text(s) => assert_eq!(s, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn shelley_aux_data_mary_form_array() {
        // aux = [metadata_map, native_scripts]
        let mut aux = vec![0x82];
        aux.push(0xa1);
        aux.extend(cbor_uint(0));
        aux.extend(cbor_uint(7)); // metadatum = Int
        aux.push(0x80); // empty native_scripts
        let block = decode_shelley_block(&shelley_block_with_aux_for_tx0(&aux)).unwrap();
        let md = &block.transactions[0]
            .auxiliary_data
            .as_ref()
            .unwrap()
            .metadata;
        match md.get(&0).unwrap() {
            TransactionMetadatum::Int(7) => {}
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn shelley_aux_data_mary_form_wrong_length_returns_empty() {
        // [single] — wrong arity, parser should swallow it gracefully.
        let mut aux = vec![0x81];
        aux.push(0xa0);
        let block = decode_shelley_block(&shelley_block_with_aux_for_tx0(&aux)).unwrap();
        assert!(block.transactions[0]
            .auxiliary_data
            .as_ref()
            .unwrap()
            .metadata
            .is_empty());
    }

    #[test]
    fn shelley_metadatum_variants_all_decode() {
        // aux map covers Map / List / Int(+/-) / Bytes / Text.
        let mut aux = vec![0xa5]; // map(5)
                                  // 0 => map(0)
        aux.extend(cbor_uint(0));
        aux.push(0xa0);
        // 1 => list[uint(1)]
        aux.extend(cbor_uint(1));
        aux.push(0x81);
        aux.extend(cbor_uint(1));
        // 2 => int(-3)
        aux.extend(cbor_uint(2));
        aux.push(0x22); // major 1, val 2 → -3
                        // 3 => bytes
        aux.extend(cbor_uint(3));
        aux.extend(cbor_bytes(&[0xAB, 0xCD]));
        // 4 => text "ok"
        aux.extend(cbor_uint(4));
        aux.push(0x62);
        aux.extend_from_slice(b"ok");
        let block = decode_shelley_block(&shelley_block_with_aux_for_tx0(&aux)).unwrap();
        let md = &block.transactions[0]
            .auxiliary_data
            .as_ref()
            .unwrap()
            .metadata;
        assert!(matches!(md.get(&0).unwrap(), TransactionMetadatum::Map(_)));
        assert!(matches!(md.get(&1).unwrap(), TransactionMetadatum::List(_)));
        assert!(matches!(md.get(&2).unwrap(), TransactionMetadatum::Int(-3)));
        assert!(matches!(
            md.get(&3).unwrap(),
            TransactionMetadatum::Bytes(_)
        ));
        assert!(matches!(md.get(&4).unwrap(), TransactionMetadatum::Text(_)));
    }

    // ── Standalone tx decoder ──────────────────────────────────────────────

    fn build_shelley_standalone_tx(aux: Option<Vec<u8>>) -> Vec<u8> {
        // [body, ws, is_valid_bool, aux_or_null]
        let mut tx = vec![0x84];
        // body = {0: [], 1: [], 2: 1_000_000}
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(1_000_000));
        // ws = {}
        tx.push(0xa0);
        // is_valid = true
        tx.push(0xf5);
        // aux
        match aux {
            Some(a) => tx.extend(&a),
            None => tx.push(0xf6),
        }
        tx
    }

    #[test]
    fn shelley_standalone_tx_decodes_with_null_aux() {
        let tx_cbor = build_shelley_standalone_tx(None);
        let tx = decode_shelley_tx_standalone(&tx_cbor).unwrap();
        assert_eq!(tx.era, Era::Shelley);
        assert_eq!(tx.body.fee.0, 1_000_000);
        assert!(tx.is_valid);
        assert!(tx.auxiliary_data.is_none());
        assert!(tx.raw_body_cbor.is_some());
        assert!(tx.raw_witness_cbor.is_some());
    }

    #[test]
    fn shelley_standalone_tx_decodes_with_aux() {
        let mut aux = vec![0xa1];
        aux.extend(cbor_uint(0));
        aux.extend(cbor_uint(123));
        let tx_cbor = build_shelley_standalone_tx(Some(aux));
        let tx = decode_shelley_tx_standalone(&tx_cbor).unwrap();
        assert!(tx.auxiliary_data.is_some());
    }

    #[test]
    fn shelley_standalone_tx_rejects_wrong_arity() {
        // array(3) instead of array(4)
        let cbor = [0x83, 0xa0, 0xa0, 0xf6];
        assert!(decode_shelley_tx_standalone(&cbor).is_err());
    }

    #[test]
    fn shelley_standalone_tx_rejects_indefinite_outer() {
        // array(indef)
        assert!(decode_shelley_tx_standalone(&[0x9f, 0xff]).is_err());
    }

    #[test]
    fn shelley_standalone_tx_is_valid_non_bool_skipped() {
        // Replace is_valid with uint(0) — decoder must skip and treat as valid.
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(0));
        tx.push(0xa0); // ws
        tx.extend(cbor_uint(0)); // is_valid as uint instead of bool
        tx.push(0xf6); // null aux
        let tx = decode_shelley_tx_standalone(&tx).unwrap();
        assert!(tx.is_valid);
    }
}
