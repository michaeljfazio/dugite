//! In-house decoder for the Alonzo era (era tag 5).
//!
//! # Alonzo block wire format
//!
//! After stripping the HFC envelope `[era_tag, inner]`, the inner CBOR is:
//!
//! ```text
//! block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set, invalid_transactions]
//! ```
//!
//! **5 elements** — Alonzo introduced the `invalid_transactions` field (array of
//! tx indices whose `is_valid` flag is `false`). Shelley/Allegra/Mary have 4.
//!
//! ## Header structure (same as Shelley)
//!
//! ```text
//! header = [header_body, kes_signature]
//! header_body = [
//!   block_number,        ; 0 — u64
//!   slot,                ; 1 — u64
//!   prev_hash,           ; 2 — bytes(32) or null
//!   issuer_vkey,         ; 3 — bytes(32)
//!   vrf_vkey,            ; 4 — bytes(32)
//!   nonce_vrf_cert,      ; 5 — [bytes(64), bytes(80)]
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
//! Fields 9–12 are the operational certificate fields (inline in Shelley/Alonzo).
//!
//! ## Tx body additions over Shelley
//!
//! Allegra added:
//! - key 8: validity_interval_start (slot, u64)
//!
//! Mary added:
//! - key 9: mint ({ policy_id => { asset_name => int } })
//!
//! Alonzo added:
//! - key 11: script_data_hash (bytes(32))
//! - key 13: collateral ([* transaction_input])
//! - key 14: required_signers ([* addr_keyhash(28)]) — padded to Hash32
//! - key 15: network_id (uint 0|1)
//!
//! ## Witness set additions over Shelley
//!
//! - key 3: plutus_v1_scripts ([* bytes])
//! - key 4: plutus_data ([* plutus_data])
//! - key 5: redeemers ([* [tag, index, plutus_data, ex_units]])
//!
//! ## invalid_transactions
//!
//! The 5th element of the block array is `[* tx_index]`. A transaction at
//! `tx_index` in the tx_bodies/witnesses arrays has `is_valid = false`:
//! its collateral inputs are consumed; regular inputs/outputs are skipped.

use crate::decode::era_shelley::read_pre_conway_update_proposal;
use crate::decode::helpers::{read_hash28, read_hash32, read_lovelace, read_network_id};
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
    AuxiliaryData, BootstrapWitness, Certificate, ExUnits, MIRSource, MIRTarget, NativeScript,
    OutputDatum, PlutusData, PoolMetadata, PoolParams, Rational, Redeemer, RedeemerTag,
    Transaction, TransactionBody, TransactionInput, TransactionMetadatum, TransactionOutput,
    TransactionWitnessSet, VKeyWitness,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};
use minicbor::data::Type;
use num_bigint::BigInt;
use std::collections::BTreeMap;

// ============================================================================
// Decode mode
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
// Top-level entry points
// ============================================================================

/// Decode an Alonzo block from the inner CBOR (after HFC envelope stripping).
pub fn decode_alonzo_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_block_mode(inner_cbor, DecodeMode::Full)
}

/// Decode an Alonzo block in minimal mode (witness set skipped).
pub fn decode_alonzo_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_alonzo_block_mode(inner_cbor, DecodeMode::Minimal)
}

/// Decode JUST the block header from the inner header CBOR (issue #654 —
/// eager per-peer header validation in the ChainSync receive loop).
///
/// Covers Allegra, Mary, and Alonzo eras — all three share the same
/// header format (TPraos with split nonce/leader VRF outputs). See
/// `era_conway::decode_conway_block_header` for the contract.
pub fn decode_alonzo_block_header(inner_cbor: &[u8]) -> Result<BlockHeader, SerializationError> {
    let mut r = Reader::new(inner_cbor);
    let raw = KeepRaw::parse_with(&mut r, decode_alonzo_header_inner)?;
    let header_hash = blake2b_256(raw.raw);
    let mut h = raw.value;
    h.header_hash = header_hash;
    Ok(h)
}

// ============================================================================
// Block decoder (shared by Allegra/Mary/Alonzo)
// ============================================================================

/// Decode an Alonzo (or Allegra/Mary) block with a specific era label.
///
/// `era` is the era to stamp on the resulting `Block` and `Transaction` structs.
/// The 4-element vs 5-element block structure is determined by `has_invalid_txs`.
pub(crate) fn decode_alonzo_family_block(
    inner_cbor: &[u8],
    era: Era,
    has_invalid_txs: bool,
    mode: DecodeMode,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    let expected_len = if has_invalid_txs { 5 } else { 4 };
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(n) if n == expected_len) {
        return Err(SerializationError::CborDecode(format!(
            "{era:?} block: expected array({expected_len}), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header
    // -------------------------------------------------------------------------
    let header = {
        let raw = KeepRaw::parse_with(&mut r, decode_alonzo_header_inner)?;
        let header_hash = blake2b_256(raw.raw);
        let mut h = raw.value;
        h.header_hash = header_hash;
        h
    };

    // -------------------------------------------------------------------------
    // 2. tx_bodies — array of KeepRaw<TransactionBody>, DEFINITE OR INDEFINITE.
    //
    // Mainnet blocks encode tx_bodies as an indefinite-length array (`9f … ff`)
    // in some blocks (e.g. Allegra epoch 238, slot 17625607). The previous
    // `read_array_header().unwrap_or(0)` treated indefinite (None) as zero txs,
    // skipped every body, and then misread the first tx body (a map) as the
    // witness array — "expected array, got map at position 1007". Read until the
    // break byte for the indefinite case (matches the Shelley decoder fix).
    let tx_count_hdr = r.read_array_header()?;
    let alloc_cap = r.safe_alloc_capacity(tx_count_hdr.unwrap_or(0));
    let mut raw_bodies: Vec<Vec<u8>> = Vec::with_capacity(alloc_cap);
    let mut parsed_bodies: Vec<TransactionBody> = Vec::with_capacity(alloc_cap);

    let mut bi = 0u64;
    loop {
        match tx_count_hdr {
            Some(n) if bi >= n => break,
            None if r.peek_major()? == minicbor::data::Type::Break => {
                r.skip()?; // consume the indefinite-array break byte
                break;
            }
            _ => {}
        }
        let body = KeepRaw::parse_with(&mut r, |r| decode_alonzo_tx_body(r, era))?;
        raw_bodies.push(body.raw.to_vec());
        parsed_bodies.push(body.value);
        bi += 1;
    }

    // -------------------------------------------------------------------------
    // 3. tx_witness_sets — DEFINITE OR INDEFINITE.
    // -------------------------------------------------------------------------
    let witness_count_hdr = r.read_array_header()?;
    let ws_alloc_cap = r.safe_alloc_capacity(witness_count_hdr.unwrap_or(0));
    let mut raw_witnesses: Vec<Vec<u8>> = Vec::with_capacity(ws_alloc_cap);
    let mut parsed_witnesses: Vec<Option<TransactionWitnessSet>> = Vec::with_capacity(ws_alloc_cap);

    let mut wi = 0u64;
    loop {
        match witness_count_hdr {
            Some(n) if wi >= n => break,
            None if r.peek_major()? == minicbor::data::Type::Break => {
                r.skip()?;
                break;
            }
            _ => {}
        }
        if mode == DecodeMode::Full {
            let ws = KeepRaw::parse_with(&mut r, |r| decode_alonzo_witness_set(r, era))?;
            raw_witnesses.push(ws.raw.to_vec());
            parsed_witnesses.push(Some(ws.value));
        } else {
            let ws_start = r.position();
            r.skip()?;
            raw_witnesses.push(r.slice_from(ws_start).to_vec());
            parsed_witnesses.push(None);
        }
        wi += 1;
    }

    // -------------------------------------------------------------------------
    // 4. auxiliary_data_set
    // -------------------------------------------------------------------------
    let aux_map = decode_alonzo_aux_data_map(&mut r)?;

    // -------------------------------------------------------------------------
    // 5. invalid_transactions (Alonzo+ only)
    // -------------------------------------------------------------------------
    let mut invalid_tx_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if has_invalid_txs {
        // DEFINITE OR INDEFINITE array of tx indices.
        let inv_count_hdr = r.read_array_header()?;
        let mut ii = 0u64;
        loop {
            match inv_count_hdr {
                Some(n) if ii >= n => break,
                None if r.peek_major()? == minicbor::data::Type::Break => {
                    r.skip()?;
                    break;
                }
                _ => {}
            }
            let idx = r.read_uint()? as usize;
            invalid_tx_set.insert(idx);
            ii += 1;
        }
    }

    // -------------------------------------------------------------------------
    // Build transactions
    // -------------------------------------------------------------------------
    let transactions: Result<Vec<Transaction>, SerializationError> = parsed_bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            let raw_body = raw_bodies[i].clone();
            let tx_hash = blake2b_256(&raw_body);

            let is_valid = !invalid_tx_set.contains(&i);

            let witness_set = match parsed_witnesses.get(i).and_then(|w| w.as_ref()) {
                Some(ws) => ws.clone(),
                None => empty_witness_set(),
            };
            let raw_witness = raw_witnesses.get(i).cloned();
            let auxiliary_data = aux_map.get(&(i as u32)).cloned();

            // Reconstruct full wire-format tx CBOR for fee-size calculation.
            // Haskell toCBORForSizeComputation (Alonzo+) = array(3)[body,wits,aux];
            // we build array(4) and fee_tx_size() subtracts the 1-byte is_valid.
            let raw_cbor = if has_invalid_txs {
                // Alonzo+ era: include the is_valid byte.
                Some(
                    crate::decode::era_babbage::reconstruct_alonzo_plus_tx_raw_cbor(
                        &raw_body,
                        raw_witness.as_deref().unwrap_or(&[0xA0]),
                        is_valid,
                        auxiliary_data.as_ref(),
                    ),
                )
            } else {
                // Pre-Alonzo era (Shelley/Allegra/Mary via this path when
                // has_invalid_txs=false): no is_valid byte; wire = array(3).
                Some(
                    crate::decode::era_shelley::reconstruct_pre_alonzo_tx_raw_cbor(
                        &raw_body,
                        raw_witness.as_deref().unwrap_or(&[0xA0]),
                        auxiliary_data.as_ref(),
                    ),
                )
            };

            Ok(Transaction {
                hash: tx_hash,
                era,
                body,
                witness_set,
                is_valid,
                auxiliary_data,
                raw_cbor,
                raw_body_cbor: Some(raw_body),
                raw_witness_cbor: raw_witness,
            })
        })
        .collect();
    let transactions = transactions?;

    Ok(Block {
        header,
        transactions,
        era,
        raw_cbor: None,
    })
}

fn decode_alonzo_block_mode(
    inner_cbor: &[u8],
    mode: DecodeMode,
) -> Result<Block, SerializationError> {
    decode_alonzo_family_block(inner_cbor, Era::Alonzo, true, mode)
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
// Header decoder (same as Shelley — 15-field inline opcert)
// ============================================================================

fn decode_alonzo_header_inner(r: &mut Reader<'_>) -> Result<BlockHeader, SerializationError> {
    // header = [header_body, kes_signature]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "alonzo header: expected array(2), got {hdr_arr:?}"
        )));
    }

    // header_body = array(15) — capture raw bytes (the KES-signed message).
    let body_start = r.position();
    let body_arr = r.read_array_header()?;
    if !matches!(body_arr, Some(15)) {
        return Err(SerializationError::CborDecode(format!(
            "alonzo header_body: expected array(15), got {body_arr:?}"
        )));
    }

    let block_number = r.read_uint()?;
    let slot = r.read_uint()?;
    let prev_hash = read_optional_hash32(r)?;
    let issuer_vkey = r.read_bytes_owned()?;
    let vrf_vkey = r.read_bytes_owned()?;
    let (nonce_output, nonce_proof) = read_vrf_cert(r)?;
    // Alonzo uses `leader_vrf` field (not `nonce_vrf`) as the VRF result.
    // The consensus leader check uses leader_vrf.0 (64-byte output).
    let (leader_output, leader_proof) = read_vrf_cert(r)?;
    let body_size = r.read_uint()?;
    let body_hash = read_hash32(r)?;
    let op_hot_vkey = r.read_bytes_owned()?;
    let op_seq_number = r.read_uint()?;
    let op_kes_period = r.read_uint()?;
    let op_sigma = r.read_bytes_owned()?;
    let protocol_major = r.read_uint()?;
    let protocol_minor = r.read_uint()?;

    let raw_header_body = r.slice_from(body_start).to_vec();

    let kes_signature = r.read_bytes_owned()?;

    Ok(BlockHeader {
        header_hash: Hash32::ZERO,
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
        raw_header_body: Some(raw_header_body),
    })
}

fn read_optional_hash32(r: &mut Reader<'_>) -> Result<Hash32, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        Ok(Hash32::ZERO)
    } else {
        read_hash32(r)
    }
}

fn read_vrf_cert(r: &mut Reader<'_>) -> Result<(Vec<u8>, Vec<u8>), SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "vrf_cert: expected array(2), got {arr_len:?}"
        )));
    }
    let output = r.read_bytes_owned()?;
    let proof = r.read_bytes_owned()?;
    Ok((output, proof))
}

// ============================================================================
// Transaction body decoder
// ============================================================================

/// Decode an Alonzo (or Allegra/Mary) transaction body.
///
/// The map key set depends on the era:
/// - Shelley: keys 0–7
/// - Allegra: + key 8 (validity_interval_start)
/// - Mary: + key 9 (mint)
/// - Alonzo: + keys 11, 13, 14, 15
pub(crate) fn decode_alonzo_tx_body(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<TransactionBody, SerializationError> {
    let mut inputs: Vec<TransactionInput> = Vec::new();
    let mut outputs: Vec<TransactionOutput> = Vec::new();
    let mut fee = Lovelace(0);
    let mut ttl: Option<SlotNo> = None;
    let mut certificates: Vec<Certificate> = Vec::new();
    let mut withdrawals: BTreeMap<Vec<u8>, Lovelace> = BTreeMap::new();
    let mut auxiliary_data_hash: Option<Hash32> = None;
    let mut validity_interval_start: Option<SlotNo> = None;
    let mut mint: BTreeMap<Hash28, BTreeMap<AssetName, i64>> = BTreeMap::new();
    let mut script_data_hash: Option<Hash32> = None;
    let mut collateral: Vec<TransactionInput> = Vec::new();
    let mut required_signers: Vec<Hash32> = Vec::new();
    let mut network_id: Option<u8> = None;
    let mut update: Option<dugite_primitives::transaction::UpdateProposal> = None;

    let map_len = r.read_map_header()?;
    let n_entries = match map_len {
        Some(n) => n as i64,
        None => -1,
    };

    let mut i = 0i64;
    loop {
        if n_entries >= 0 && i >= n_entries {
            break;
        }
        if n_entries < 0 {
            let ty = r.peek_major()?;
            if ty == Type::Break {
                r.skip()?;
                break;
            }
        }
        i += 1;

        let key = r.read_uint()?;
        match key {
            0 => {
                inputs = r.read_array(read_tx_input)?;
            }
            1 => {
                outputs = r.read_array(|r| read_alonzo_tx_output(r, era))?;
            }
            2 => {
                fee = read_lovelace(r)?;
            }
            3 => {
                ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                certificates = r.read_array(|r| read_alonzo_certificate(r))?;
            }
            5 => {
                withdrawals = read_withdrawals(r)?;
            }
            6 => {
                // update = [proposed_protocol_parameter_updates, epoch]
                // Decoded so the boundary handler can apply pre-Conway PPUPs.
                // See `era_shelley::read_pre_conway_update_proposal` and #624.
                update = Some(read_pre_conway_update_proposal(r)?);
            }
            7 => {
                auxiliary_data_hash = Some(read_hash32(r)?);
            }
            8 => {
                // validity_interval_start (Allegra+)
                validity_interval_start = Some(SlotNo(r.read_uint()?));
            }
            9 => {
                // mint (Mary+): { policy_id => { asset_name => int } }
                mint = read_mint_map(r)?;
            }
            11 if matches!(era, Era::Alonzo) => {
                // script_data_hash (Alonzo-only; Allegra/Mary reject)
                script_data_hash = Some(read_hash32(r)?);
            }
            13 if matches!(era, Era::Alonzo) => {
                // collateral (Alonzo-only; Allegra/Mary reject)
                collateral = r.read_array(read_tx_input)?;
            }
            14 if matches!(era, Era::Alonzo) => {
                // required_signers (Alonzo-only): [* addr_keyhash(28)] → padded to Hash32
                required_signers = r.read_array(|r| {
                    let h28 = read_hash28(r)?;
                    Ok(h28.to_hash32_padded())
                })?;
            }
            15 if matches!(era, Era::Alonzo) => {
                // network_id (Alonzo-only; Allegra/Mary reject)
                network_id = Some(read_network_id(r)?);
            }
            _ => {
                // Unknown/out-of-era tx-body key — HARD REJECT, per upstream
                // per-era SparseKeyed bodyFields catch-all (invalidField n ->
                // cborError). Allegra/Mary know 0..9; Alonzo adds 11,13,14,15
                // (NOT 10,12). See #31-E.
                return Err(SerializationError::CborDecode(format!(
                    "{era:?} tx body: unknown/invalid key {key}"
                )));
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
        validity_interval_start,
        mint,
        script_data_hash,
        collateral,
        required_signers,
        network_id,
        collateral_return: None,      // Babbage+
        total_collateral: None,       // Babbage+
        reference_inputs: Vec::new(), // Babbage+
        update,
        voting_procedures: BTreeMap::new(), // Conway+
        proposal_procedures: Vec::new(),    // Conway+
        treasury_value: None,
        donation: None,
        sub_transactions: Vec::new(),          // Dijkstra+
        account_balance_intervals: Vec::new(), // Dijkstra+
        direct_deposits: BTreeMap::new(),      // Dijkstra+
        guards: Vec::new(),                    // Dijkstra+
    })
}

fn read_tx_input(r: &mut Reader<'_>) -> Result<TransactionInput, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "tx_in: expected array(2), got {arr_len:?}"
        )));
    }
    let tx_hash = read_hash32(r)?;
    let index = r.read_uint()? as u32;
    Ok(TransactionInput {
        transaction_id: tx_hash,
        index,
    })
}

/// Read an Alonzo-era transaction output: `[address, value]` or `[address, value, datum_hash]`.
///
/// Alonzo outputs are always the legacy array format (not the Babbage map format).
fn read_alonzo_tx_output(
    r: &mut Reader<'_>,
    _era: Era,
) -> Result<TransactionOutput, SerializationError> {
    let arr_len = r.read_array_header()?;
    let n = match arr_len {
        Some(2) | Some(3) => arr_len.unwrap(),
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "alonzo tx_out: expected array(2) or array(3), got {arr_len:?}"
            )));
        }
    };

    let addr_bytes = r.read_bytes_owned()?;
    let address = Address::from_bytes(&addr_bytes)
        .map_err(|e| SerializationError::InvalidData(format!("alonzo output address: {e}")))?;

    let value = read_value(r)?;

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

/// Read a Value: either a plain uint (ADA only) or `[coin, multiasset_map]`.
pub(crate) fn read_value(r: &mut Reader<'_>) -> Result<Value, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let coin = r.read_uint()?;
            Ok(Value::lovelace(coin))
        }
        Type::Array => {
            let arr_len = r.read_array_header()?;
            if !matches!(arr_len, Some(2)) {
                return Err(SerializationError::CborDecode(format!(
                    "value array: expected array(2), got {arr_len:?}"
                )));
            }
            let coin = Lovelace(r.read_uint()?);
            let multi_asset = read_multiasset_map_u64(r)?;
            if multi_asset.is_empty() {
                Ok(Value::lovelace(coin.0))
            } else {
                Ok(Value { coin, multi_asset })
            }
        }
        other => Err(SerializationError::CborDecode(format!(
            "value: expected uint or array, got {other}"
        ))),
    }
}

fn read_multiasset_map_u64(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Hash28, BTreeMap<AssetName, u64>>, SerializationError> {
    // Both the outer policy map and inner asset maps may be definite or
    // indefinite-length. Use read_map() which handles both transparently.
    let entries = r.read_map(
        |r| {
            let policy_bytes = r.read_bytes()?;
            Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                expected: 28,
                got: policy_bytes.len(),
            })
        },
        |r| {
            let asset_entries = r.read_map(
                |r| {
                    let name_bytes = r.read_bytes_owned()?;
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("multiasset: asset name too long".into())
                    })
                },
                |r| r.read_uint(),
            )?;
            let mut assets: BTreeMap<AssetName, u64> = BTreeMap::new();
            for (k, v) in asset_entries {
                assets.insert(k, v);
            }
            Ok(assets)
        },
    )?;
    // Haskell `decodeMultiAsset` (Mary/Value.hs) for decoder version < 9
    // (Alonzo/Babbage era CBOR) uses `decodeWithPrunning`:
    //
    // ```haskell
    // decodeWithPrunning =
    //   pruneZeroMultiAsset . MultiAsset <$> decodeMap decCBOR (decodeMap decCBOR decodeAmount)
    // pruneZeroMultiAsset = filterMultiAsset (\_ _ -> (/= 0))
    // ```
    //
    // Zero-quantity assets are ACCEPTED on the wire and pruned, and a policy
    // whose asset map becomes (or arrives) empty is dropped entirely
    // (`filterMultiAsset` guards `not (null newAssetMap)`). The pruned value
    // is what the ledger stores and what Plutus ScriptContexts see — keeping
    // zeros made every script walking such a Value over-cost vs cardano-node
    // (#730). Conway (v9+) instead REJECTS zeros — see
    // era_conway::read_multiasset_map_u64.
    let mut result = BTreeMap::new();
    for (k, mut v) in entries {
        v.retain(|_, amount| *amount != 0);
        if !v.is_empty() {
            result.insert(k, v);
        }
    }
    Ok(result)
}

/// Read a mint map: `{ policy_id => { asset_name => int } }`.
/// Quantities are signed (minting is positive, burning is negative).
fn read_mint_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Hash28, BTreeMap<AssetName, i64>>, SerializationError> {
    // Both the outer policy map and inner asset maps may be definite or
    // indefinite-length. Use read_map() which handles both transparently.
    let entries = r.read_map(
        |r| {
            // key: policy_id bytes(28)
            let policy_bytes = r.read_bytes()?;
            Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                expected: 28,
                got: policy_bytes.len(),
            })
        },
        |r| {
            // value: { asset_name => signed_int }
            let asset_entries = r.read_map(
                |r| {
                    let name_bytes = r.read_bytes_owned()?;
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("mint: asset name too long".into())
                    })
                },
                |r| Ok(r.read_int()? as i64),
            )?;
            let mut assets: BTreeMap<AssetName, i64> = BTreeMap::new();
            for (k, v) in asset_entries {
                assets.insert(k, v);
            }
            Ok(assets)
        },
    )?;
    // Same pre-v9 `decodeWithPrunning` semantics as `read_multiasset_map_u64`
    // above: the mint field shares Haskell's `decodeMultiAsset` (with a signed
    // amount decoder), so zero quantities are pruned and emptied policies
    // dropped.
    let mut result = BTreeMap::new();
    for (k, mut v) in entries {
        v.retain(|_, amount| *amount != 0);
        if !v.is_empty() {
            result.insert(k, v);
        }
    }
    Ok(result)
}

fn read_withdrawals(r: &mut Reader<'_>) -> Result<BTreeMap<Vec<u8>, Lovelace>, SerializationError> {
    // Withdrawals: { reward_account_bytes => coin }
    // Map may be definite or indefinite length.
    let entries = r.read_map(|r| r.read_bytes_owned(), |r| Ok(Lovelace(r.read_uint()?)))?;
    let mut result = BTreeMap::new();
    for (k, v) in entries {
        result.insert(k, v);
    }
    Ok(result)
}

// ============================================================================
// Certificate decoder (Alonzo — same as Shelley certs 0-6)
// ============================================================================

/// Read a single certificate from CBOR. Re-exported as `read_alonzo_cert_inner` for
/// use by the Babbage decoder which shares the same certificate encoding.
pub(crate) fn read_alonzo_cert_inner(
    r: &mut Reader<'_>,
) -> Result<Certificate, SerializationError> {
    read_alonzo_certificate(r)
}

fn read_alonzo_certificate(r: &mut Reader<'_>) -> Result<Certificate, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "certificate: expected definite-length array".into(),
        ));
    }
    let cert_type = r.read_uint()?;
    match cert_type {
        0 => Ok(Certificate::StakeRegistration(read_stake_credential(r)?)),
        1 => Ok(Certificate::StakeDeregistration(read_stake_credential(r)?)),
        2 => {
            let cred = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            Ok(Certificate::StakeDelegation {
                credential: cred,
                pool_hash,
            })
        }
        3 => Ok(Certificate::PoolRegistration(read_pool_params(r)?)),
        4 => {
            let pool_hash = read_hash28_cert(r)?;
            let epoch = r.read_uint()?;
            Ok(Certificate::PoolRetirement { pool_hash, epoch })
        }
        5 => {
            // GenesisKeyDelegation — Shelley CDDL:
            //   (5, genesishash, genesis_delegate_hash, vrf_keyhash)
            // genesishash / genesis_delegate_hash are 28-byte KEY hashes
            // ($hash28); only vrf_keyhash is 32 bytes. Stored zero-padded in
            // the Hash32 enum fields (the ledger consumer truncates back to
            // 28 — see certificates.rs GenesisKeyDelegation). Reading them
            // as Hash32 broke at the first real cert on mainnet (slot
            // 66137371, the pre-Vasil genesis-delegate rotations).
            let genesis_hash = read_hash28_cert(r)?.to_hash32_padded();
            let delegate_hash = read_hash28_cert(r)?.to_hash32_padded();
            let vrf_keyhash = read_hash32(r)?;
            Ok(Certificate::GenesisKeyDelegation {
                genesis_hash,
                genesis_delegate_hash: delegate_hash,
                vrf_keyhash,
            })
        }
        6 => read_mir_cert(r),
        other => Err(SerializationError::CborDecode(format!(
            "certificate: unknown type {other}"
        ))),
    }
}

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

fn read_hash28_cert(r: &mut Reader<'_>) -> Result<Hash28, SerializationError> {
    let bytes = r.read_bytes()?;
    Hash28::try_from(bytes).map_err(|_| SerializationError::InvalidLength {
        expected: 28,
        got: bytes.len(),
    })
}

fn read_pool_params(r: &mut Reader<'_>) -> Result<PoolParams, SerializationError> {
    let operator = read_hash28_cert(r)?;
    let vrf_keyhash = read_hash32(r)?;
    let pledge = read_lovelace(r)?;
    let cost = read_lovelace(r)?;
    let margin = r.read_rational()?;
    let reward_account = r.read_bytes_owned()?;
    let pool_owners: Vec<Hash28> = r.read_set(|r| read_hash28_cert(r))?;
    // relays: definite OR indefinite-length array — see #673 / era_shelley.rs.
    r.for_each_array_item(|r| {
        r.skip()?;
        Ok(())
    })?;
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
        relays: Vec::new(),
        pool_metadata,
    })
}

fn read_pool_metadata(r: &mut Reader<'_>) -> Result<Option<PoolMetadata>, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
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
        let mut buf = [0u8; 32];
        let len = bytes.len().min(32);
        buf[..len].copy_from_slice(&bytes[..len]);
        Hash32::from_bytes(buf)
    };
    Ok(Some(PoolMetadata { url, hash }))
}

fn read_mir_cert(r: &mut Reader<'_>) -> Result<Certificate, SerializationError> {
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
    let ty = r.peek_major()?;
    let target = match ty {
        Type::Map | Type::MapIndef => {
            // Definite OR indefinite `{ stake_credential => delta_coin }` map
            // (see era_shelley::read_mir_cert). read_map handles both.
            let creds = r.read_map(read_stake_credential, |r| r.read_int().map(|d| d as i64))?;
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

/// Decode an Alonzo witness set.
///
/// ```text
/// transaction_witness_set = {
///   ? 0 : [* vkeywitness],       ; vkey_witnesses
///   ? 1 : [* native_script],     ; native_scripts
///   ? 2 : [* bootstrap_witness], ; bootstrap_witnesses
///   ? 3 : [* bytes],             ; plutus_v1_scripts
///   ? 4 : [* plutus_data],       ; plutus_data
///   ? 5 : [* [tag, idx, data, ex_units]], ; redeemers
/// }
/// ```
pub(crate) fn decode_alonzo_witness_set(
    r: &mut Reader<'_>,
    _era: Era,
) -> Result<TransactionWitnessSet, SerializationError> {
    let mut vkey_witnesses: Vec<VKeyWitness> = Vec::new();
    let mut native_scripts = Vec::new();
    let mut bootstrap_witnesses = Vec::new();
    let mut plutus_v1_scripts: Vec<Vec<u8>> = Vec::new();
    let mut raw_plutus_data: Option<Vec<u8>> = None;
    let mut plutus_data: Vec<PlutusData> = Vec::new();
    let mut raw_redeemers: Option<Vec<u8>> = None;
    let mut redeemers: Vec<Redeemer> = Vec::new();

    let map_len = r.read_map_header()?;
    let n_entries = match map_len {
        Some(n) => n as i64,
        None => -1,
    };

    let mut i = 0i64;
    loop {
        if n_entries >= 0 && i >= n_entries {
            break;
        }
        if n_entries < 0 {
            let ty = r.peek_major()?;
            if ty == Type::Break {
                r.skip()?;
                break;
            }
        }
        i += 1;

        let key = r.read_uint()?;
        match key {
            0 => {
                vkey_witnesses = r.read_array(|r| {
                    let arr_len = r.read_array_header()?;
                    if !matches!(arr_len, Some(2)) {
                        return Err(SerializationError::CborDecode(
                            "vkeywitness: expected array(2)".into(),
                        ));
                    }
                    let vkey = r.read_bytes_owned()?;
                    let signature = r.read_bytes_owned()?;
                    Ok(VKeyWitness { vkey, signature })
                })?;
            }
            1 => {
                native_scripts = r.read_array(|r| read_native_script(r))?;
            }
            2 => {
                bootstrap_witnesses = r.read_array(|r| {
                    let arr_len = r.read_array_header()?;
                    if !matches!(arr_len, Some(4)) {
                        return Err(SerializationError::CborDecode(
                            "bootstrap_witness: expected array(4)".into(),
                        ));
                    }
                    let vkey = r.read_bytes_owned()?;
                    let sig = r.read_bytes_owned()?;
                    let chain_code = r.read_bytes_owned()?;
                    let attrs = r.read_bytes_owned()?;
                    Ok(BootstrapWitness {
                        vkey,
                        signature: sig,
                        chain_code,
                        attributes: attrs,
                    })
                })?;
            }
            3 => {
                // plutus_v1_scripts: [* bytes]
                plutus_v1_scripts = r.read_array(|r| r.read_bytes_owned())?;
            }
            4 => {
                // plutus_data: [* plutus_data]
                // Capture raw bytes for script_data_hash computation.
                let pd_start = r.position();
                let items = r.read_array(|r| read_plutus_data(r))?;
                raw_plutus_data = Some(r.slice_from(pd_start).to_vec());
                plutus_data = items;
            }
            5 => {
                // redeemers: [* [tag, index, data, ex_units]]
                let rd_start = r.position();
                let items = r.read_array(|r| read_redeemer(r))?;
                raw_redeemers = Some(r.slice_from(rd_start).to_vec());
                // Haskell Map.fromList semantics: duplicate (tag, index)
                // entries collapse, last wins (#753). Raw bytes above keep
                // the duplicates for script-integrity hashing.
                redeemers = crate::decode::helpers::dedup_redeemers_last_wins(items);
            }
            _ => {
                // Haskell cardano-ledger decodes the witness set via SparseKeyed
                // with field-picker `txWitnessField n = invalidField n`, so an
                // unknown map key hard-fails the decode (invalidField -> Invalid n
                // -> invalidKey -> cborError). Not version-gated — reject in every
                // era (Allegra/Mary/Alonzo all route through this decoder). Mirror
                // that strictness here instead of silently skipping.
                return Err(SerializationError::CborDecode(format!(
                    "witness set: unknown key {key}"
                )));
            }
        }
    }

    Ok(TransactionWitnessSet {
        vkey_witnesses,
        native_scripts,
        bootstrap_witnesses,
        plutus_v1_scripts,
        plutus_v2_scripts: Vec::new(),
        plutus_v3_scripts: Vec::new(),
        plutus_data,
        redeemers,
        raw_redeemers_cbor: raw_redeemers,
        raw_plutus_data_cbor: raw_plutus_data,
        original_script_data_hash: None,
    })
}

/// Read a single native script from CBOR. Re-exported as `read_native_script_from_cbor` for
/// use by the Babbage decoder which shares the same native script encoding.
pub(crate) fn read_native_script_from_cbor(
    r: &mut Reader<'_>,
) -> Result<NativeScript, SerializationError> {
    read_native_script(r)
}

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

/// Read a single Plutus redeemer: `[tag, index, plutus_data, ex_units]`.
fn read_redeemer(r: &mut Reader<'_>) -> Result<Redeemer, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "redeemer: expected array(4), got {arr_len:?}"
        )));
    }
    let tag_u = r.read_uint()?;
    let tag = match tag_u {
        0 => RedeemerTag::Spend,
        1 => RedeemerTag::Mint,
        2 => RedeemerTag::Cert,
        3 => RedeemerTag::Reward,
        4 => RedeemerTag::Vote,
        5 => RedeemerTag::Propose,
        other => {
            return Err(SerializationError::CborDecode(format!(
                "redeemer tag: unknown {other}"
            )));
        }
    };
    let index = r.read_uint()? as u32;
    let data = read_plutus_data(r)?;
    let ex_units = read_ex_units(r)?;
    Ok(Redeemer {
        tag,
        index,
        data,
        ex_units,
    })
}

fn read_ex_units(r: &mut Reader<'_>) -> Result<ExUnits, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "ex_units: expected array(2), got {arr_len:?}"
        )));
    }
    let mem = r.read_uint()?;
    let steps = r.read_uint()?;
    Ok(ExUnits { mem, steps })
}

/// Read a Plutus data value (recursive).
///
/// Plutus data encoding (CDDL):
/// ```text
/// plutus_data
///   = #6.121([* plutus_data])    ; Constr 0
///   / #6.122([* plutus_data])    ; Constr 1
///   ...
///   / #6.127([* plutus_data])    ; Constr 6
///   / #6.1280([* plutus_data])   ; Constr 7
///   ...
///   / #6.1400([* plutus_data])   ; Constr 127
///   / #6.102([int, [* plutus_data]]) ; other alternatives
///   / { * plutus_data => plutus_data } ; map
///   / [* plutus_data]            ; list
///   / big_int                    ; integer
///   / bounded_bytes              ; bytes (possibly indefinite-length)
/// ```
pub(crate) fn read_plutus_data(r: &mut Reader<'_>) -> Result<PlutusData, SerializationError> {
    read_plutus_data_depth(r, 0)
}

/// Recover the original CBOR byte span of each element in a preserved
/// `plutus_data` witness array (`witness_set.raw_plutus_data_cbor`).
///
/// **Datum hashes are `blake2b256` over the *original* datum bytes** — Haskell
/// memoises the raw CBOR (`MemoBytes`/`Data`) and hashes that, never a
/// re-encoding. On-chain datums are frequently encoded in non-canonical forms
/// that a structural re-encoder cannot reproduce: the general `Constr` form
/// (CBOR tag 102) for small constructor indices, definite-length field arrays
/// where the canonical form is indefinite, non-minimal integers, etc. Decoding
/// such a datum to `PlutusData` and re-encoding yields a *different* hash, which
/// breaks the `MissingDatumWitness` / `ExtraDatumWitness` Phase-1 checks (the
/// recomputed hash matches neither the required input-datum hash nor the
/// allowed set). This helper re-splits the preserved raw array into the exact
/// per-element byte spans so the caller can hash each verbatim.
///
/// Returns `None` if `raw` does not parse as a `plutus_data` array (the caller
/// then falls back to canonical re-encoding, which is correct for datums the
/// node *constructs* itself — those have no original wire bytes). The spans are
/// returned in array order, 1:1 with the decoded `witness_set.plutus_data`.
pub fn plutus_data_element_spans(raw: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut r = Reader::new(raw);
    let mut spans: Vec<Vec<u8>> = Vec::new();
    // The witness `plutus_data` field is a plain array pre-Conway and a
    // tag-258 set in Conway; `read_set` consumes either form transparently.
    let result: Result<Vec<()>, SerializationError> = r.read_set(|r| {
        let start = r.position();
        read_plutus_data(r)?;
        spans.push(r.slice_from(start).to_vec());
        Ok(())
    });
    result.ok().map(|_| spans)
}

/// Maximum nesting depth for `read_plutus_data`.
///
/// Adversarial transactions can encode arbitrarily deep PlutusData via nested
/// constructors, arrays, and maps. Each level consumes a frame of native stack
/// (~3 KiB at -C opt-level=3, much larger under AddressSanitizer); without a
/// cap a fuzz-generated input of a few KiB of repeated `0x9f` (indef array)
/// bytes overflows the stack and SIGABRTs the decoder.
///
/// The cap matches `haskell_snapshot::cbor_utils::CBOR_SKIP_MAX_DEPTH` (1024)
/// so any structure the skip helper accepts is also accepted here. Haskell's
/// `cardano-ledger` PlutusData decoder relies on GHC's native stack and
/// imposes no explicit limit, but in practice never approaches anywhere near
/// this depth on real-world data. See #673 for the equivalent rationale.
const MAX_PLUTUS_DATA_DEPTH: usize = 1024;

fn read_plutus_data_depth(
    r: &mut Reader<'_>,
    depth: usize,
) -> Result<PlutusData, SerializationError> {
    if depth > MAX_PLUTUS_DATA_DEPTH {
        return Err(SerializationError::CborDecode(format!(
            "plutus_data nesting depth exceeds limit ({MAX_PLUTUS_DATA_DEPTH})"
        )));
    }
    let ty = r.peek_major()?;
    match ty {
        Type::Tag => {
            // #831 finding 2: `cborg`'s `peekTokenType` (which upstream
            // `cardano-ledger`'s `PlutusData` `Serialise` instance is
            // built on) maps ONLY the 1-byte inline tag header (`0xc2`
            // positive / `0xc3` negative) to `TypeInteger` -> the bignum
            // decode path. Any wider, non-minimal encoding of the same
            // tag value (e.g. `d8 02`, `d9 00 02`) is `TypeTag` ->
            // `decodeConstr`, which fails since 2/3 aren't valid
            // constructor tags. Peek the raw header byte BEFORE
            // consuming the tag so a wide-form 2/3 can be routed away
            // from the bignum arms and into the "unknown tag" rejection,
            // matching Haskell's acceptance boundary exactly.
            let is_inline_bignum_header = matches!(r.peek_byte(), Some(0xc2) | Some(0xc3));
            // Peek the tag value without consuming via skip-and-restore trick.
            // We use the raw underlying bytes to read the tag value.
            let tag_n = read_tag_value(r)?;
            match (is_inline_bignum_header, tag_n) {
                (true, 2) => {
                    // Positive bignum: tag was already consumed, just read bytes.
                    // CBOR §3.4.3 + Cardano `bounded_bytes`: the mantissa may
                    // be encoded as indefinite-length chunks. Use the
                    // chunked-aware reader (#673). The mantissa is a PlutusData
                    // ByteString leaf, so enforce the 64-byte-per-chunk bound
                    // (Note [The 64-byte limit], #28).
                    let bytes = r.read_bounded_plutus_bytes()?;
                    let val = BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes);
                    Ok(PlutusData::Integer(val))
                }
                (true, 3) => {
                    // Negative bignum: tag was already consumed, just read bytes.
                    // Mantissa is a PlutusData ByteString leaf — bounded (#28).
                    let bytes = r.read_bounded_plutus_bytes()?;
                    let n = BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes);
                    Ok(PlutusData::Integer(-BigInt::from(1) - n))
                }
                (_, tag_n @ 121..=127) => {
                    // Constr alternative 0..6: tag = 121 + N
                    let constructor = tag_n - 121;
                    let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
                    Ok(PlutusData::Constr(constructor, fields))
                }
                (_, tag_n @ 1280..=1400) => {
                    // Constr alternative 7..127: tag = 1280 + (N - 7)
                    let constructor = tag_n - 1280 + 7;
                    let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
                    Ok(PlutusData::Constr(constructor, fields))
                }
                (_, 102) => {
                    // Alternative encoding: [constructor_index, [* plutus_data]]
                    //
                    // #831 finding 1: Haskell's `decodeConstrExtended` uses
                    // `decodeListLenOrIndef`, which accepts BOTH a
                    // definite-length array(2) and an indefinite-length
                    // array closed by an explicit break — it does not
                    // require definite-length here. Accept both forms,
                    // consuming the trailing break for the indefinite case.
                    let arr_len = r.read_array_header()?;
                    match arr_len {
                        Some(2) => {
                            let constructor = r.read_uint()?;
                            let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
                            Ok(PlutusData::Constr(constructor, fields))
                        }
                        Some(n) => Err(SerializationError::CborDecode(format!(
                            "plutus_data constr(102): expected array(2), got Some({n})"
                        ))),
                        None => {
                            let constructor = r.read_uint()?;
                            let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
                            r.expect_break()?;
                            Ok(PlutusData::Constr(constructor, fields))
                        }
                    }
                }
                (_, other) => Err(SerializationError::CborDecode(format!(
                    "plutus_data: unknown tag {other}"
                ))),
            }
        }
        Type::Map | Type::MapIndef => {
            let pairs = r.read_map(
                |r| read_plutus_data_depth(r, depth + 1),
                |r| read_plutus_data_depth(r, depth + 1),
            )?;
            Ok(PlutusData::Map(pairs))
        }
        Type::Array | Type::ArrayIndef => {
            let items = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
            Ok(PlutusData::List(items))
        }
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let v = r.read_uint()?;
            Ok(PlutusData::Integer(BigInt::from(v)))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
            let v = r.read_int()?;
            Ok(PlutusData::Integer(BigInt::from(v)))
        }
        Type::Bytes | Type::BytesIndef => {
            // PlutusData ByteString leaf: enforce the plutus 64-byte-per-chunk
            // bound (Note [The 64-byte limit]). Definite > 64 => Err; each
            // indefinite chunk must be <= 64 (total unbounded). See #28.
            let bytes = r.read_bounded_plutus_bytes()?;
            Ok(PlutusData::Bytes(bytes))
        }
        other => Err(SerializationError::CborDecode(format!(
            "plutus_data: unexpected type {other}"
        ))),
    }
}

/// Peek and consume a CBOR tag value (major type 6) without consuming the
/// tagged value that follows.
///
/// CBOR tag encoding:
/// - 0xc0..0xd7: tag value 0..23 inline in the first byte (major=6, info=val)
/// - 0xd8 NN: 1-byte tag (info=24, then 1 byte for value)
/// - 0xd9 NN NN: 2-byte tag (info=25, then 2 bytes BE)
/// - 0xda NN NN NN NN: 4-byte tag (info=26, then 4 bytes BE)
/// - 0xdb NN×8: 8-byte tag (info=27, then 8 bytes BE)
///
/// Advances the reader past the tag header only; the tagged value is left
/// for the caller to consume.
fn read_tag_value(r: &mut Reader<'_>) -> Result<u64, SerializationError> {
    r.read_tag()
}

// ============================================================================
// Auxiliary data decoder
// ============================================================================

/// Decode the auxiliary_data_set map: `{ tx_index => auxiliary_data }`.
///
/// Handles both definite- and indefinite-length CBOR maps. cn 11.0.1 emits
/// indefinite-length aux_data_set on preview / preprod for Babbage blocks
/// containing CIP-20 transaction messages (issue #673 — sibling of #615e
/// which fixed tx_bodies/tx_witness_sets/invalid_transactions but missed
/// the aux_data_set map). Using `read_map_header()?.unwrap_or(0)` silently
/// treated the indef variant as empty and left the `0xff` break byte for
/// the next decoder, which then failed with "expected array, got u8".
pub(crate) fn decode_alonzo_aux_data_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u32, AuxiliaryData>, SerializationError> {
    let mut result = BTreeMap::new();
    r.for_each_map_entry(|r| {
        let tx_idx = r.read_uint()? as u32;
        let aux = decode_alonzo_auxiliary_data(r)?;
        result.insert(tx_idx, aux);
        Ok(())
    })?;
    Ok(result)
}

/// Decode an Alonzo auxiliary data value.
///
/// Wire formats:
/// - Plain map (Shelley): `{ label => metadatum }`
/// - ShelleyMa (Allegra/Mary): `[metadata_map, native_scripts]`
/// - PostAlonzo: `tag(259) { 0 => metadata, 1 => [native_scripts], 2 => [plutus_v1] }`
pub(crate) fn decode_alonzo_auxiliary_data(
    r: &mut Reader<'_>,
) -> Result<AuxiliaryData, SerializationError> {
    let raw_start = r.position();
    r.skip()?;
    let raw_bytes = r.slice_from(raw_start).to_vec();

    let mut aux_r = Reader::new(&raw_bytes);
    let ty = aux_r.peek_major()?;

    let mut metadata = BTreeMap::new();
    let native_scripts: Vec<NativeScript> = Vec::new();
    let mut plutus_v1_scripts: Vec<Vec<u8>> = Vec::new();

    match ty {
        Type::Map => {
            metadata = decode_metadata_map(&mut aux_r)?;
        }
        Type::Array => {
            // ShelleyMa: [metadata_map, native_scripts]
            let arr_len = aux_r.read_array_header()?;
            if matches!(arr_len, Some(2)) {
                metadata = decode_metadata_map(&mut aux_r)?;
                // Skip native scripts (we don't decode them from aux data)
                aux_r.skip()?;
            }
        }
        Type::Tag => {
            // PostAlonzo: tag(259) { ... }
            // Consume the tag (any value accepted; cardano uses 259)
            let _ = aux_r.read_tag()?;
            // Now we have a map — handle BOTH definite and indefinite
            // length (sibling of #673 — cn 11.0.1 emits indef-length on
            // preview for some Babbage blocks).
            aux_r.for_each_field_entry(|r, k| {
                match k {
                    0 => {
                        metadata = decode_metadata_map(r)?;
                    }
                    1 => {
                        // native scripts — skip for now
                        r.skip()?;
                    }
                    2 => {
                        // plutus_v1_scripts (array of byte strings).
                        // Use for_each_array_item so indefinite-length
                        // arrays don't silently truncate.
                        let mut local = Vec::new();
                        r.for_each_array_item(|r| {
                            local.push(r.read_bytes_owned()?);
                            Ok(())
                        })?;
                        plutus_v1_scripts.extend(local);
                    }
                    _ => {
                        r.skip()?;
                    }
                }
                Ok(())
            })?;
        }
        _ => {
            // Unknown format — return raw bytes only
        }
    }

    Ok(AuxiliaryData {
        metadata,
        native_scripts,
        plutus_v1_scripts,
        plutus_v2_scripts: Vec::new(),
        plutus_v3_scripts: Vec::new(),
        raw_cbor: Some(raw_bytes),
    })
}

fn decode_metadata_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u64, TransactionMetadatum>, SerializationError> {
    // Handle both definite- and indefinite-length CBOR maps — see #673.
    let mut result = BTreeMap::new();
    r.for_each_map_entry(|r| {
        let label = r.read_uint()?;
        let value = read_metadatum(r)?;
        result.insert(label, value);
        Ok(())
    })?;
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
            let bytes = r.read_bytes_owned()?;
            Ok(TransactionMetadatum::Bytes(bytes))
        }
        Type::BytesIndef => {
            let bytes = r.read_indef_bytes()?;
            Ok(TransactionMetadatum::Bytes(bytes))
        }
        Type::String => {
            let s = r.read_str()?.to_string();
            Ok(TransactionMetadatum::Text(s))
        }
        other => Err(SerializationError::CborDecode(format!(
            "metadatum: unexpected type {other}"
        ))),
    }
}

// ============================================================================
// Standalone tx decoder (Allegra / Mary / Alonzo era family)
// ============================================================================

/// Decode a standalone Alonzo-family transaction from raw CBOR bytes.
///
/// Covers Allegra (era_id=2), Mary (era_id=3), and Alonzo (era_id=4).
/// The standalone tx format is `[body_map, witness_set_map, is_valid_bool, aux_data]`.
///
/// The `era` argument is stamped onto the returned [`Transaction`] and controls
/// which body fields are decoded (Allegra adds validity_start, Mary adds mint,
/// Alonzo adds script_data_hash/collateral/required_signers).
///
/// The transaction hash is `blake2b_256(raw_body_cbor)`.
pub(crate) fn decode_alonzo_family_tx_standalone(
    cbor: &[u8],
    era: Era,
) -> Result<Transaction, SerializationError> {
    let mut r = Reader::new(cbor);

    // tx = [body, witness_set, is_valid, aux_data]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(4) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "{era:?} tx: expected array(4), got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(format!(
                "{era:?} tx: expected definite-length array"
            )));
        }
    }

    // 1. Body — capture raw bytes for hash computation
    let body_raw = KeepRaw::parse_with(&mut r, |r| decode_alonzo_tx_body(r, era))?;
    let raw_body_cbor = body_raw.raw.to_vec();
    let tx_hash = blake2b_256(&raw_body_cbor);
    let body = body_raw.value;

    // 2. Witness set
    let ws_raw = KeepRaw::parse_with(&mut r, |r| decode_alonzo_witness_set(r, era))?;
    let raw_witness_cbor = ws_raw.raw.to_vec();
    let witness_set = ws_raw.value;

    // 3. is_valid bool
    let is_valid = {
        let ty = r.peek_major()?;
        if ty == Type::Bool {
            r.read_bool()?
        } else {
            r.skip()?;
            true
        }
    };

    // 4. Auxiliary data (null or a value)
    let auxiliary_data = {
        let ty = r.peek_major()?;
        if ty == Type::Null {
            r.read_null()?;
            None
        } else {
            Some(decode_alonzo_auxiliary_data(&mut r)?)
        }
    };

    Ok(Transaction {
        hash: tx_hash,
        era,
        body,
        witness_set,
        is_valid,
        auxiliary_data,
        raw_cbor: Some(cbor.to_vec()),
        raw_body_cbor: Some(raw_body_cbor),
        raw_witness_cbor: Some(raw_witness_cbor),
    })
}

/// Decode a single Alonzo-family (Allegra / Mary / Alonzo) `transaction_output`
/// CBOR value.
///
/// Alonzo outputs are always the legacy array form: `[address, value]` or
/// `[address, value, datum_hash]`. The `era` is forwarded to
/// [`read_alonzo_tx_output`] for diagnostic context.
///
/// Used by [`crate::decode::decode_transaction_output`] (Allegra/Mary/Alonzo
/// dispatch) and by `dugite-uplc`'s phase-2 evaluator to decode resolved-UTxO
/// CBOR pairs.
pub(crate) fn decode_alonzo_tx_output_standalone(
    cbor: &[u8],
    era: Era,
) -> Result<TransactionOutput, SerializationError> {
    let mut r = Reader::new(cbor);
    let raw = KeepRaw::parse_with(&mut r, |r| read_alonzo_tx_output(r, era))?;
    let mut output = raw.value;
    output.raw_cbor = Some(raw.raw.to_vec());
    Ok(output)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::era::Era;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// CBOR for `[coin, {policy(28×fill) -> {"" -> amount}}]`.
    fn value_one_asset_cbor(coin: u8, fill: u8, amount: u8) -> Vec<u8> {
        let mut v = vec![0x82, coin, 0xa1, 0x58, 0x1c];
        v.extend([fill; 28]);
        v.extend([0xa1, 0x40, amount]);
        v
    }

    /// #730: Haskell `decodeMultiAsset` at decoder version < 9 PRUNES
    /// zero-quantity assets (`pruneZeroMultiAsset`) and drops policies whose
    /// asset map becomes empty. A value whose only asset is zero collapses to
    /// an ada-only Value (the ledger / Plutus ScriptContext never see the
    /// zero entry).
    #[test]
    fn pre_conway_value_zero_quantity_asset_pruned() {
        let bytes = value_one_asset_cbor(0x0a, 0x11, 0x00);
        let mut r = Reader::new(&bytes);
        let v = read_value(&mut r).expect("zero amount accepted pre-Conway");
        assert_eq!(v.coin.0, 10);
        assert!(
            v.multi_asset.is_empty(),
            "zero-quantity asset must be pruned: {:?}",
            v.multi_asset
        );
    }

    /// Mixed case: only the zero entry is pruned; non-zero assets survive.
    #[test]
    fn pre_conway_value_partial_zero_prune() {
        // [10, {policy(0x11) -> {"" -> 0}, policy(0x22) -> {"" -> 5}}]
        let mut bytes = vec![0x82, 0x0a, 0xa2, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.extend([0xa1, 0x40, 0x00, 0x58, 0x1c]);
        bytes.extend([0x22; 28]);
        bytes.extend([0xa1, 0x40, 0x05]);
        let mut r = Reader::new(&bytes);
        let v = read_value(&mut r).expect("decode");
        assert_eq!(v.multi_asset.len(), 1);
        let (policy, assets) = v.multi_asset.iter().next().unwrap();
        assert_eq!(policy.as_bytes(), &[0x22; 28]);
        assert_eq!(assets.values().copied().collect::<Vec<u64>>(), vec![5]);
    }

    /// A wire-empty inner asset map is dropped pre-Conway (`filterMultiAsset`
    /// guards `not (null newAssetMap)` over the as-decoded map).
    #[test]
    fn pre_conway_value_empty_asset_map_dropped() {
        // [10, {policy(0x11) -> {}}]
        let mut bytes = vec![0x82, 0x0a, 0xa1, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.push(0xa0);
        let mut r = Reader::new(&bytes);
        let v = read_value(&mut r).expect("decode");
        assert!(v.multi_asset.is_empty());
    }

    /// The mint field shares Haskell's `decodeMultiAsset`: zero quantities are
    /// pruned pre-Conway.
    #[test]
    fn pre_conway_mint_zero_quantity_pruned() {
        // {policy(0x11) -> {"" -> 0}}
        let mut bytes = vec![0xa1, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.extend([0xa1, 0x40, 0x00]);
        let mut r = Reader::new(&bytes);
        let mint = read_mint_map(&mut r).expect("decode");
        assert!(mint.is_empty(), "zero mint quantity must be pruned");
    }

    /// `plutus_data_element_spans` must return each datum's *original* bytes
    /// verbatim, so the hash matches Haskell's `MemoBytes` hash even when the
    /// datum is encoded non-canonically.
    ///
    /// Vector: mainnet datum `5ff23baed5…` (Koios `datum_info`), encoded with
    /// the **general `Constr` form** (CBOR tag 102, `d866`) and a definite
    /// 6-field array `86` for constructor index 0 — a form the canonical Plutus
    /// encoder (tag 121, indefinite) would never produce. A definite outer
    /// array of one element wraps it (`0x81`).
    #[test]
    fn test_plutus_data_element_spans_preserves_noncanonical_bytes() {
        let datum = "d866820086581ca3250750af6227b5a7dc689de94c83728a9d1d4029cc232d4a46f81e1a041cdb40581c023cec350597bdf2a2b6945e62e0111d9808caf7a9353a2ab91e8beb50534f434945545932354c4d4239323332581c63a3bc3807c6a51f85570ad9a82ed46bdb96feeabae6c4aa0526d4ed181e";
        let datum_bytes = hex(datum);
        let mut array = vec![0x81u8]; // array(1)
        array.extend_from_slice(&datum_bytes);

        let spans = plutus_data_element_spans(&array).expect("spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0], datum_bytes,
            "span must be the verbatim datum bytes"
        );

        let h = dugite_primitives::hash::blake2b_256(&spans[0]).to_hex();
        assert_eq!(
            h, "5ff23baed51ec22e9342ace92e6dd9976be5ded109575f58a8a2419f064818d0",
            "hash of the original bytes must equal the on-chain datum hash"
        );
    }

    /// Indefinite-array form and multiple elements split correctly.
    #[test]
    fn test_plutus_data_element_spans_indefinite_multi() {
        // 9f 182a 43aabbcc ff  — indefinite array of [ I(42), B(aabbcc) ]
        let raw = hex("9f182a43aabbccff");
        let spans = plutus_data_element_spans(&raw).expect("spans");
        assert_eq!(spans, vec![hex("182a"), hex("43aabbcc")]);
    }

    /// Pre-Conway leniency guard (backlog #31-C over-strictness inverse).
    ///
    /// Alonzo/Babbage are protocol-version < 9, so their `set` fields must stay
    /// LENIENT: `cardano-ledger-binary`'s `decodeSet` only enforces
    /// no-duplicates at PV9+; pre-PV9 it silently dedups via `Set.fromList`.
    /// `read_pool_params` here decodes `pool_owners` via the lenient
    /// [`Reader::read_set`], which must accept a duplicate `addr_keyhash`
    /// without error. This pins that leniency so the Conway-only switch to
    /// `read_set_strict` cannot accidentally tighten pre-Conway decoders.
    #[test]
    fn alonzo_pool_owners_set_accepts_duplicate_lenient() {
        // tag(258) [h28, h28] — the SAME 28-byte key twice.
        let key = [0x11u8; 28];
        let mut data = vec![0xd9, 0x01, 0x02]; // tag 258
        data.extend(vec![0x82]); // array(2)
        data.extend(cbor_bytes(&key));
        data.extend(cbor_bytes(&key));

        let mut r = Reader::new(&data);
        // This is exactly the call Alonzo `read_pool_params` makes for
        // `pool_owners`. It must NOT reject the duplicate at PV < 9.
        let owners: Vec<Hash28> = r
            .read_set(read_hash28_cert)
            .expect("pre-Conway set must accept duplicate elements (lenient)");
        // The lenient reader keeps both physical elements (no dedup, no fail).
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0], owners[1]);
    }

    // ── SparseKeyed duplicate field-key rejection (backlog #31-D) ──────────────

    /// tag(259) aux-data field map rejects a duplicate field key.
    ///
    /// `decodeSparseKeyed` tracks the seen field keys (`Set Word`) and
    /// hard-fails the second occurrence; this rejection is un-gated.
    #[test]
    fn alonzo_tag259_aux_duplicate_field_key_rejected() {
        // tag(259) { 0: {}, 0: {} } — field key 0 (metadata) twice.
        let mut data = vec![0xd9, 0x01, 0x03]; // tag 259
        data.extend(vec![0xa2]); // map(2)
        data.extend(cbor_uint(0));
        data.extend(cbor_map0()); // empty metadata map
        data.extend(cbor_uint(0));
        data.extend(cbor_map0());

        let mut r = Reader::new(&data);
        let result = decode_alonzo_auxiliary_data(&mut r);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate tag-259 aux field key must be rejected, got {result:?}"
        );
    }

    #[test]
    fn alonzo_tag259_aux_unique_field_keys_ok() {
        // tag(259) { 0: { 5 => 7 } } — sanity the strict path still decodes.
        let mut metadata_map = vec![0xa1]; // map(1)
        metadata_map.extend(cbor_uint(5));
        metadata_map.extend(cbor_uint(7));

        let mut data = vec![0xd9, 0x01, 0x03]; // tag 259
        data.extend(vec![0xa1]); // map(1)
        data.extend(cbor_uint(0));
        data.extend(&metadata_map);

        let mut r = Reader::new(&data);
        let aux = decode_alonzo_auxiliary_data(&mut r).expect("unique aux field keys decode");
        assert_eq!(aux.metadata.len(), 1);
    }

    /// Metadata LABEL maps stay LENIENT (last-wins) — metadata is the canonical
    /// lenient class. A duplicate label key must NOT error here, mirroring the
    /// Haskell `Map.fromList`/last-wins behaviour. This pins that the #31-D
    /// switch to strict touched ONLY the SparseKeyed field maps.
    #[test]
    fn alonzo_metadata_label_map_accepts_duplicate_lenient() {
        // map(2) { 5 => 7, 5 => 9 } — the SAME label twice.
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(5));
        data.extend(cbor_uint(7));
        data.extend(cbor_uint(5));
        data.extend(cbor_uint(9));

        let mut r = Reader::new(&data);
        let md = decode_metadata_map(&mut r)
            .expect("metadata label map must accept duplicate label (lenient last-wins)");
        // Last-wins: BTreeMap keeps the final value, no fail.
        assert_eq!(md.get(&5), Some(&TransactionMetadatum::Int(9i128)));
    }

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

    #[allow(dead_code)]
    fn cbor_null() -> Vec<u8> {
        vec![0xf6]
    }

    /// Build a minimal Alonzo block CBOR (inner, after envelope stripping).
    fn make_alonzo_block(n_txs: usize) -> Vec<u8> {
        // VRF cert = array(2)[output(64), proof(80)]
        let vrf_output = cbor_bytes(&[0u8; 64]);
        let vrf_proof = cbor_bytes(&[0u8; 80]);
        let vrf_cert = cbor_arr(&[&vrf_output, &vrf_proof]);

        // Header body: 15 fields (same as Shelley)
        let mut hb = vec![0x8f]; // array(15)
        hb.extend(cbor_uint(42)); // block_number
        hb.extend(cbor_uint(654321)); // slot
        hb.extend(cbor_bytes(&[0xba; 32])); // prev_hash
        hb.extend(cbor_bytes(&[0x01; 32])); // issuer_vkey
        hb.extend(cbor_bytes(&[0x02; 32])); // vrf_vkey
        hb.extend(&vrf_cert); // nonce_vrf_cert
        hb.extend(&vrf_cert); // leader_vrf_cert
        hb.extend(cbor_uint(0)); // body_size
        hb.extend(cbor_bytes(&[0x00; 32])); // body_hash
        hb.extend(cbor_bytes(&[0x03; 32])); // op_hot_vkey
        hb.extend(cbor_uint(0)); // op_seq
        hb.extend(cbor_uint(0)); // op_kes
        hb.extend(cbor_bytes(&[0x04; 64])); // op_sigma
        hb.extend(cbor_uint(5)); // proto_major
        hb.extend(cbor_uint(0)); // proto_minor

        let kes_sig = cbor_bytes(&[0x05; 448]);
        let mut header = vec![0x82]; // array(2)[header_body, kes_sig]
        header.extend(&hb);
        header.extend(&kes_sig);

        // tx_bodies = array(n_txs) of minimal bodies
        let mut tx_bodies_v = Vec::new();
        let mut tx_witnesses_v = Vec::new();
        if n_txs <= 23 {
            tx_bodies_v.push(0x80 | n_txs as u8);
            tx_witnesses_v.push(0x80 | n_txs as u8);
        }
        for _ in 0..n_txs {
            // {0: [], 1: [], 2: 1000000}
            let mut tb = vec![0xa3];
            tb.extend(cbor_uint(0));
            tb.push(0x80); // empty inputs
            tb.extend(cbor_uint(1));
            tb.push(0x80); // empty outputs
            tb.extend(cbor_uint(2));
            tb.extend(cbor_uint(1_000_000));
            tx_bodies_v.extend(&tb);
            tx_witnesses_v.push(0xa0); // empty witness set
        }

        // aux_data_set = {}
        let aux_data = cbor_map0();

        // invalid_transactions = []
        let invalid_txs = vec![0x80]; // array(0)

        // block = array(5)[header, tx_bodies, tx_witnesses, aux_data, invalid_txs]
        let mut block = vec![0x85]; // array(5)
        block.extend(&header);
        block.extend(&tx_bodies_v);
        block.extend(&tx_witnesses_v);
        block.extend(&aux_data);
        block.extend(&invalid_txs);
        block
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn alonzo_empty_block_decodes() {
        let cbor = make_alonzo_block(0);
        let block = decode_alonzo_block(&cbor).unwrap();
        assert_eq!(block.era, Era::Alonzo);
        assert_eq!(block.transactions.len(), 0);
        assert_eq!(block.header.slot.0, 654321);
        assert_eq!(block.header.block_number.0, 42);
    }

    #[test]
    fn alonzo_single_tx_valid_flag() {
        let cbor = make_alonzo_block(1);
        let block = decode_alonzo_block(&cbor).unwrap();
        assert_eq!(block.transactions.len(), 1);
        assert!(block.transactions[0].is_valid);
    }

    #[test]
    fn alonzo_invalid_tx_is_marked() {
        // Build a block with 2 txs, tx index 1 is invalid.
        let mut cbor = make_alonzo_block(2);
        // The invalid_txs array is `[0x80]` = empty array(0).
        // Replace the last byte sequence with `[0x81, 0x01]` = array(1)[uint(1)].
        // Find and replace the trailing empty invalid_txs array.
        let pos = cbor.len() - 1; // last byte is 0x80
        assert_eq!(cbor[pos], 0x80, "expected empty invalid_txs array at end");
        cbor.pop();
        cbor.push(0x81); // array(1)
        cbor.push(0x01); // uint(1)

        let block = decode_alonzo_block(&cbor).unwrap();
        assert!(block.transactions[0].is_valid, "tx 0 should be valid");
        assert!(!block.transactions[1].is_valid, "tx 1 should be invalid");
    }

    #[test]
    fn alonzo_block_5_elements_required() {
        // array(4) instead of array(5) should fail
        let data = vec![0x84, 0x00, 0x00, 0x00, 0x00];
        let result = decode_alonzo_block(&data);
        assert!(result.is_err());
    }

    #[test]
    fn alonzo_header_hash_is_blake2b256_of_header_cbor() {
        let cbor = make_alonzo_block(0);
        let block = decode_alonzo_block(&cbor).unwrap();

        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        let header_start = r.position();
        r.skip().unwrap();
        let header_bytes = r.slice_from(header_start);
        let expected_hash = blake2b_256(header_bytes);
        assert_eq!(block.header.header_hash, expected_hash);
    }

    #[test]
    fn alonzo_tx_hash_is_blake2b256_of_body_cbor() {
        let cbor = make_alonzo_block(1);
        let block = decode_alonzo_block(&cbor).unwrap();
        assert_eq!(block.transactions.len(), 1);

        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        r.skip().unwrap(); // skip header
        r.read_array_header().unwrap();
        let body_start = r.position();
        r.skip().unwrap();
        let body_bytes = r.slice_from(body_start);
        let expected_hash = blake2b_256(body_bytes);
        assert_eq!(block.transactions[0].hash, expected_hash);
    }

    #[test]
    fn alonzo_script_data_hash_decoded() {
        // Build a tx body with script_data_hash (key 11)
        let sdh = [0xde; 32];
        let mut tb = vec![0xa4]; // map(4)
        tb.extend(cbor_uint(0));
        tb.push(0x80); // inputs []
        tb.extend(cbor_uint(1));
        tb.push(0x80); // outputs []
        tb.extend(cbor_uint(2));
        tb.extend(cbor_uint(500_000)); // fee
        tb.extend(cbor_uint(11));
        tb.extend(cbor_bytes(&sdh)); // script_data_hash

        let raw_sdh = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        })
        .unwrap();
        assert_eq!(
            raw_sdh.value.script_data_hash,
            Some(Hash32::from_bytes(sdh))
        );
    }

    // ── #31-E: per-era unknown/out-of-era tx-body key rejection ─────────────
    // decode_alonzo_tx_body is SHARED across Allegra/Mary/Alonzo (era threaded).
    // Allegra/Mary know {0..9}; Alonzo adds {11,13,14,15} (NOT 10,12,16+).

    /// Helper: minimal map(2) tx body {0:[], <key>:0} for unknown-key probes.
    fn alonzo_body_with_extra_key(key: u64) -> Vec<u8> {
        let mut tb = vec![0xa2]; // map(2)
        tb.extend(cbor_uint(0));
        tb.push(0x80); // inputs []
        tb.extend(cbor_uint(key));
        tb.extend(cbor_uint(0)); // arbitrary value
        tb
    }

    #[test]
    fn allegra_body_key_11_rejected() {
        // Key 11 (script_data_hash) is Alonzo-only — Allegra must reject it.
        let tb = alonzo_body_with_extra_key(11);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Allegra)
        });
        assert!(
            result.is_err(),
            "Allegra tx-body key 11 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn allegra_body_key_13_rejected() {
        // Key 13 (collateral) is Alonzo-only — Allegra must reject it.
        let tb = alonzo_body_with_extra_key(13);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Allegra)
        });
        assert!(
            result.is_err(),
            "Allegra tx-body key 13 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn mary_body_key_11_rejected() {
        // Key 11 (script_data_hash) is Alonzo-only — Mary must reject it.
        let tb = alonzo_body_with_extra_key(11);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Mary)
        });
        assert!(
            result.is_err(),
            "Mary tx-body key 11 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn alonzo_body_key_10_rejected() {
        // Key 10 is NOT in the Alonzo bodyFields domain — must be rejected.
        let tb = alonzo_body_with_extra_key(10);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        });
        assert!(
            result.is_err(),
            "Alonzo tx-body key 10 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn alonzo_body_key_12_rejected() {
        // Key 12 is NOT in the Alonzo bodyFields domain — must be rejected.
        let tb = alonzo_body_with_extra_key(12);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        });
        assert!(
            result.is_err(),
            "Alonzo tx-body key 12 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn alonzo_body_key_16_rejected() {
        // Key 16 (collateral_return) is Babbage+ — Alonzo must reject it.
        let tb = alonzo_body_with_extra_key(16);
        let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        });
        assert!(
            result.is_err(),
            "Alonzo tx-body key 16 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn alonzo_body_key_15_accepted() {
        // Key 15 (network_id) is the highest valid Alonzo key.
        let mut tb = vec![0xa2]; // map(2)
        tb.extend(cbor_uint(0));
        tb.push(0x80); // inputs []
        tb.extend(cbor_uint(15));
        tb.extend(cbor_uint(1)); // network_id = mainnet
        let raw = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        })
        .unwrap();
        assert_eq!(raw.value.network_id, Some(1));
    }

    #[test]
    fn alonzo_rejects_babbage_keys_16_17_18() {
        // Cross-era discriminator: keys 16/17/18 (Babbage collateral_return /
        // total_collateral / reference_inputs) MUST be rejected by Alonzo even
        // though Babbage accepts them. See babbage_accepts_keys_16_17_18.
        for key in [16u64, 17, 18] {
            let tb = alonzo_body_with_extra_key(key);
            let result = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
                decode_alonzo_tx_body(r, Era::Alonzo)
            });
            assert!(
                result.is_err(),
                "Alonzo tx-body key {key} must be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn alonzo_collateral_decoded() {
        // Build a tx body with collateral (key 13)
        let tx_id = [0xaa; 32];
        let mut inp = cbor_arr(&[&cbor_bytes(&tx_id), &cbor_uint(0)]);
        let mut collateral_arr = vec![0x81]; // array(1)
        collateral_arr.append(&mut inp);

        let mut tb = vec![0xa3]; // map(3)
        tb.extend(cbor_uint(0));
        tb.push(0x80); // inputs []
        tb.extend(cbor_uint(2));
        tb.extend(cbor_uint(200_000)); // fee
        tb.extend(cbor_uint(13));
        tb.extend(&collateral_arr);

        let raw = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        })
        .unwrap();
        assert_eq!(raw.value.collateral.len(), 1);
        assert_eq!(raw.value.collateral[0].transaction_id.as_bytes(), &tx_id);
    }

    #[test]
    fn alonzo_required_signers_padded_to_hash32() {
        // required_signers (key 14): [* addr_keyhash(28)] → padded to Hash32
        let signer = [0xcc; 28];
        let mut arr = vec![0x81]; // array(1)
        arr.extend(cbor_bytes(&signer));

        let mut tb = vec![0xa3]; // map(3)
        tb.extend(cbor_uint(0));
        tb.push(0x80);
        tb.extend(cbor_uint(2));
        tb.extend(cbor_uint(100_000));
        tb.extend(cbor_uint(14));
        tb.extend(&arr);

        let raw = KeepRaw::parse_with(&mut Reader::new(&tb), |r| {
            decode_alonzo_tx_body(r, Era::Alonzo)
        })
        .unwrap();
        assert_eq!(raw.value.required_signers.len(), 1);
        // First 28 bytes match the signer; last 4 are zero-padding
        let h32 = raw.value.required_signers[0];
        assert_eq!(&h32.as_bytes()[..28], &signer);
        assert_eq!(&h32.as_bytes()[28..], &[0u8; 4]);
    }

    #[test]
    fn plutus_data_integer() {
        let data = [0x05u8]; // uint(5)
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Integer(BigInt::from(5u64)));
    }

    #[test]
    fn plutus_data_bytes() {
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let data = cbor_bytes(&payload);
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Bytes(payload.to_vec()));
    }

    #[test]
    fn plutus_data_constr_121() {
        // tag(121) array(0) — Constr 0 with no fields
        let mut data = vec![0xd8, 0x79]; // tag(121) — 0xc0 | 0x18 (1-byte), value 121 = 0x79
        data.push(0x80); // array(0)
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Constr(0, vec![]));
    }

    #[test]
    fn plutus_data_constr_1280() {
        // tag(1280) array(0) — Constr 7 with no fields
        // 1280 = 0x500; tag encoding: 0xd9 0x05 0x00
        let mut data = vec![0xd9, 0x05, 0x00]; // tag(1280)
        data.push(0x80); // array(0)
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Constr(7, vec![]));
    }

    #[test]
    fn plutus_data_map() {
        // {0 => 1}
        let mut data = vec![0xa1]; // map(1)
        data.push(0x00); // uint(0)
        data.push(0x01); // uint(1)
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(
            pd,
            PlutusData::Map(vec![(
                PlutusData::Integer(BigInt::from(0u64)),
                PlutusData::Integer(BigInt::from(1u64))
            )])
        );
    }

    #[test]
    fn plutus_data_list() {
        // [1, 2]
        let mut data = vec![0x82]; // array(2)
        data.push(0x01);
        data.push(0x02);
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(
            pd,
            PlutusData::List(vec![
                PlutusData::Integer(BigInt::from(1u64)),
                PlutusData::Integer(BigInt::from(2u64))
            ])
        );
    }

    #[test]
    fn alonzo_minimal_mode_skips_witnesses() {
        let cbor = make_alonzo_block(1);
        let block = decode_alonzo_block_minimal(&cbor).unwrap();
        assert!(block.transactions[0].witness_set.vkey_witnesses.is_empty());
        assert!(block.transactions[0].witness_set.redeemers.is_empty());
    }

    // ── Plutus data edge cases ────────────────────────────────────────────

    #[test]
    fn plutus_data_constr_102_alternative_encoding() {
        // tag(102) [constructor, [fields]] — alternative for constr 128+
        let mut data = vec![0xd8, 0x66]; // tag(102)
        data.push(0x82); // array(2)
        data.extend(cbor_uint(200)); // constructor
        data.push(0x80); // empty fields
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert!(matches!(pd, PlutusData::Constr(200, _)));
    }

    #[test]
    fn plutus_data_constr_102_wrong_arity_rejected() {
        let data = vec![0xd8, 0x66, 0x81, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn plutus_data_constr_102_indefinite_outer_array_accepted() {
        // #831 finding 1: `d8 66 9f 00 80 ff` = tag 102, indefinite
        // `[0, []]` => Constr 0 []. Haskell's `decodeConstrExtended`
        // uses `decodeListLenOrIndef`, which accepts an indefinite
        // outer array closed by an explicit break; `read_plutus_data`
        // previously required `Some(2)` and rejected this.
        let data = vec![0xd8, 0x66, 0x9f, 0x00, 0x80, 0xff];
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Constr(0, vec![]));
    }

    #[test]
    fn plutus_data_constr_102_indefinite_missing_break_rejected() {
        // Same as above with the trailing break omitted.
        let data = vec![0xd8, 0x66, 0x9f, 0x00, 0x80];
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn plutus_data_wide_header_bignum_tag2_rejected() {
        // #831 finding 2: `d8 02 41 05` is tag 2 via the non-minimal
        // 1-byte-argument header (`0xd8 0x02`) rather than the minimal
        // inline form (`0xc2`). `cborg`'s `peekTokenType` maps ONLY the
        // inline `0xc2`/`0xc3` headers to the bignum path; a wide-header
        // encoding of tag 2/3 must be rejected as an unknown
        // constructor tag, not silently accepted as a bignum.
        let mut data = vec![0xd8, 0x02];
        data.extend(cbor_bytes(&[0x05]));
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn plutus_data_wide_header_bignum_tag3_rejected() {
        let mut data = vec![0xd9, 0x00, 0x03];
        data.extend(cbor_bytes(&[0x05]));
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn plutus_data_positive_bignum_tag2() {
        // tag(2) bstr — positive bignum
        let mut data = vec![0xc2];
        data.extend(cbor_bytes(&[0x01, 0x00])); // 256
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Integer(BigInt::from(256u64)));
    }

    #[test]
    fn plutus_data_negative_bignum_tag3() {
        let mut data = vec![0xc3];
        data.extend(cbor_bytes(&[0x00])); // -(0)-1 = -1
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Integer(BigInt::from(-1i32)));
    }

    #[test]
    fn plutus_data_negative_int() {
        // -5 = major 1, info 4 = 0x24
        let data = [0x24u8];
        let mut r = Reader::new(&data);
        let pd = read_plutus_data(&mut r).unwrap();
        assert_eq!(pd, PlutusData::Integer(BigInt::from(-5i32)));
    }

    #[test]
    fn plutus_data_unknown_tag_rejected() {
        let mut data = vec![0xd9, 0x10, 0x00]; // tag(4096) — unknown
        data.push(0x80);
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn plutus_data_unexpected_type_rejected() {
        // null (major 7) is not valid plutus_data
        let data = [0xf6u8];
        let mut r = Reader::new(&data);
        assert!(read_plutus_data(&mut r).is_err());
    }

    #[test]
    fn read_ex_units_decodes() {
        // [mem, steps]
        let mut data = vec![0x82];
        data.extend(cbor_uint(100));
        data.extend(cbor_uint(2_000));
        let mut r = Reader::new(&data);
        let eu = read_ex_units(&mut r).unwrap();
        assert_eq!(eu.mem, 100);
        assert_eq!(eu.steps, 2_000);
    }

    #[test]
    fn read_ex_units_rejects_wrong_arity() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_ex_units(&mut r).is_err());
    }

    // ── Redeemer ───────────────────────────────────────────────────────────

    #[test]
    fn read_redeemer_all_tags() {
        for (tag_u, expected_tag) in [
            (0u64, RedeemerTag::Spend),
            (1, RedeemerTag::Mint),
            (2, RedeemerTag::Cert),
            (3, RedeemerTag::Reward),
            (4, RedeemerTag::Vote),
            (5, RedeemerTag::Propose),
        ] {
            let mut data = vec![0x84]; // array(4)
            data.extend(cbor_uint(tag_u));
            data.extend(cbor_uint(0));
            data.push(0x00); // plutus_data = uint(0)
            data.push(0x82); // ex_units array(2)
            data.extend(cbor_uint(1));
            data.extend(cbor_uint(2));
            let mut r = Reader::new(&data);
            let red = read_redeemer(&mut r).unwrap();
            assert_eq!(red.tag, expected_tag);
            assert_eq!(red.ex_units.mem, 1);
            assert_eq!(red.ex_units.steps, 2);
        }
    }

    #[test]
    fn read_redeemer_unknown_tag_rejected() {
        let mut data = vec![0x84];
        data.extend(cbor_uint(99));
        data.extend(cbor_uint(0));
        data.push(0x00);
        data.push(0x82);
        data.push(0x00);
        data.push(0x00);
        let mut r = Reader::new(&data);
        assert!(read_redeemer(&mut r).is_err());
    }

    #[test]
    fn read_redeemer_wrong_arity_rejected() {
        // array(3) instead of array(4)
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_redeemer(&mut r).is_err());
    }

    // ── Witness set: covers vkey, native, bootstrap, plutus_v1, data, redeemers ──

    /// Build a witness-set CBOR with a mix of keys.
    fn alonzo_ws_all_keys() -> Vec<u8> {
        let mut ws = vec![0xa6]; // map(6)

        // 0: vkey witnesses [[vkey, sig]]
        ws.extend(cbor_uint(0));
        ws.push(0x81);
        ws.push(0x82);
        ws.extend(cbor_bytes(&[0xAA; 32]));
        ws.extend(cbor_bytes(&[0xBB; 64]));

        // 1: native_scripts [[5, 100]]
        ws.extend(cbor_uint(1));
        ws.push(0x81);
        ws.push(0x82);
        ws.extend(cbor_uint(5));
        ws.extend(cbor_uint(100));

        // 2: bootstrap_witnesses [[vkey, sig, cc, attrs]]
        ws.extend(cbor_uint(2));
        ws.push(0x81);
        ws.push(0x84);
        ws.extend(cbor_bytes(&[0x01; 32]));
        ws.extend(cbor_bytes(&[0x02; 64]));
        ws.extend(cbor_bytes(&[0x03; 32]));
        ws.extend(cbor_bytes(&[]));

        // 3: plutus_v1_scripts [bytes]
        ws.extend(cbor_uint(3));
        ws.push(0x81);
        ws.extend(cbor_bytes(&[0xCA, 0xFE]));

        // 4: plutus_data [uint(7)]
        ws.extend(cbor_uint(4));
        ws.push(0x81);
        ws.extend(cbor_uint(7));

        // 5: redeemers [[0, 0, uint(0), [1, 1]]]
        ws.extend(cbor_uint(5));
        ws.push(0x81);
        ws.push(0x84);
        ws.extend(cbor_uint(0));
        ws.extend(cbor_uint(0));
        ws.push(0x00);
        ws.push(0x82);
        ws.extend(cbor_uint(1));
        ws.extend(cbor_uint(1));

        ws
    }

    #[test]
    fn alonzo_witness_set_all_keys_decode() {
        let ws_cbor = alonzo_ws_all_keys();
        let mut r = Reader::new(&ws_cbor);
        let ws = decode_alonzo_witness_set(&mut r, Era::Alonzo).unwrap();
        assert_eq!(ws.vkey_witnesses.len(), 1);
        assert_eq!(ws.native_scripts.len(), 1);
        assert_eq!(ws.bootstrap_witnesses.len(), 1);
        assert_eq!(ws.plutus_v1_scripts.len(), 1);
        assert_eq!(ws.plutus_data.len(), 1);
        assert_eq!(ws.redeemers.len(), 1);
        assert!(ws.raw_redeemers_cbor.is_some());
        assert!(ws.raw_plutus_data_cbor.is_some());
    }

    #[test]
    fn alonzo_witness_set_unknown_key_rejected() {
        // Haskell cardano-ledger SparseKeyed (txWitnessField n = invalidField n)
        // hard-fails an unknown witness-set map key. dugite must reject too.
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(99));
        ws.extend(cbor_uint(0));
        let mut r = Reader::new(&ws);
        let result = decode_alonzo_witness_set(&mut r, Era::Alonzo);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "unknown witness-set key must be rejected, got {result:?}"
        );
    }

    // ── Standalone tx decoder (Alonzo family) ──────────────────────────────

    fn build_alonzo_standalone_tx(era: Era) -> Vec<u8> {
        let mut tx = vec![0x84];
        // body
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(500_000));
        // ws
        tx.push(0xa0);
        // is_valid
        tx.push(0xf5);
        // aux
        tx.push(0xf6);
        let _ = era;
        tx
    }

    #[test]
    fn alonzo_standalone_tx_decodes() {
        let tx_cbor = build_alonzo_standalone_tx(Era::Alonzo);
        let tx = decode_alonzo_family_tx_standalone(&tx_cbor, Era::Alonzo).unwrap();
        assert_eq!(tx.era, Era::Alonzo);
        assert_eq!(tx.body.fee.0, 500_000);
        assert!(tx.is_valid);
    }

    #[test]
    fn allegra_standalone_tx_decodes() {
        let tx_cbor = build_alonzo_standalone_tx(Era::Allegra);
        let tx = decode_alonzo_family_tx_standalone(&tx_cbor, Era::Allegra).unwrap();
        assert_eq!(tx.era, Era::Allegra);
    }

    #[test]
    fn mary_standalone_tx_decodes() {
        let tx_cbor = build_alonzo_standalone_tx(Era::Mary);
        let tx = decode_alonzo_family_tx_standalone(&tx_cbor, Era::Mary).unwrap();
        assert_eq!(tx.era, Era::Mary);
    }

    #[test]
    fn alonzo_standalone_tx_rejects_wrong_arity() {
        let cbor = [0x83, 0xa0, 0xa0, 0xf6];
        assert!(decode_alonzo_family_tx_standalone(&cbor, Era::Alonzo).is_err());
    }

    #[test]
    fn alonzo_standalone_tx_rejects_indefinite() {
        assert!(decode_alonzo_family_tx_standalone(&[0x9f, 0xff], Era::Alonzo).is_err());
    }

    #[test]
    fn alonzo_standalone_tx_with_aux_data() {
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(0));
        tx.push(0xa0);
        tx.push(0xf5);
        // aux = tag(259) {}
        tx.push(0xd9);
        tx.push(0x01);
        tx.push(0x03);
        tx.push(0xa0);
        let tx_decoded = decode_alonzo_family_tx_standalone(&tx, Era::Alonzo).unwrap();
        assert!(tx_decoded.auxiliary_data.is_some());
    }

    #[test]
    fn alonzo_standalone_tx_invalid_flag() {
        // is_valid = false (0xf4)
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(0));
        tx.push(0xa0);
        tx.push(0xf4); // false
        tx.push(0xf6);
        let tx_decoded = decode_alonzo_family_tx_standalone(&tx, Era::Alonzo).unwrap();
        assert!(!tx_decoded.is_valid);
    }

    // ── Native script wrapper ─────────────────────────────────────────────

    #[test]
    fn native_script_from_cbor_wraps_read_native_script() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&[0x77; 28]));
        let mut r = Reader::new(&data);
        let ns = read_native_script_from_cbor(&mut r).unwrap();
        assert!(matches!(ns, NativeScript::ScriptPubkey(_)));
    }

    // ── Alonzo PlutusData 64-byte ByteString-leaf bound (#28) ─────────────────
    //
    // The Alonzo PlutusData decoder is a SEPARATE code path from the Conway one
    // (with inline tag-2/tag-3 bignum arms), so it needs its own coverage of
    // Note [The 64-byte limit] / decodeBoundedBytes(IndefLen).

    /// Definite-length CBOR byte string header + payload (Alonzo test helper).
    fn def_bytes(payload: &[u8]) -> Vec<u8> {
        let n = payload.len();
        let mut v = if n <= 23 {
            vec![0x40 | n as u8]
        } else if n <= 0xff {
            vec![0x58, n as u8]
        } else {
            let b = (n as u16).to_be_bytes();
            vec![0x59, b[0], b[1]]
        };
        v.extend_from_slice(payload);
        v
    }

    /// Indefinite-length CBOR byte string (`0x5f <chunks> 0xff`).
    fn indef_bytes(chunks: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![0x5fu8];
        for c in chunks {
            v.extend_from_slice(&def_bytes(c));
        }
        v.push(0xff);
        v
    }

    fn decode_alonzo_pd(cbor: &[u8]) -> Result<PlutusData, SerializationError> {
        let mut r = Reader::new(cbor);
        read_plutus_data(&mut r)
    }

    #[test]
    fn alonzo_plutus_bytes_definite_64_ok_65_err() {
        let ok = vec![0xABu8; 64];
        assert_eq!(
            decode_alonzo_pd(&def_bytes(&ok)).expect("64 ok"),
            PlutusData::Bytes(ok)
        );
        let bad = vec![0xABu8; 65];
        let err = decode_alonzo_pd(&def_bytes(&bad)).expect_err("65 must reject");
        assert!(matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")));
    }

    #[test]
    fn alonzo_plutus_bytes_indef_chunk_64_ok_65_err() {
        let ok = vec![0x11u8; 64];
        assert_eq!(
            decode_alonzo_pd(&indef_bytes(&[&ok])).expect("64 chunk ok"),
            PlutusData::Bytes(ok)
        );
        let bad = vec![0x11u8; 65];
        let err = decode_alonzo_pd(&indef_bytes(&[&bad])).expect_err("65 chunk must reject");
        assert!(matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")));
    }

    #[test]
    fn alonzo_plutus_bytes_indef_two_64_chunks_total_128_ok() {
        let a = vec![0x22u8; 64];
        let b = vec![0x33u8; 64];
        let pd = decode_alonzo_pd(&indef_bytes(&[&a, &b])).expect("128 total ok");
        let mut expected = a.clone();
        expected.extend_from_slice(&b);
        assert_eq!(pd, PlutusData::Bytes(expected));
    }

    #[test]
    fn alonzo_plutus_bytes_indef_zero_length_chunk_ok() {
        let empty: &[u8] = &[];
        let b = vec![0x44u8; 8];
        let pd = decode_alonzo_pd(&indef_bytes(&[empty, &b])).expect("0-length chunk ok");
        assert_eq!(pd, PlutusData::Bytes(b));
    }

    #[test]
    fn alonzo_plutus_bignum_mantissa_64_ok_65_err() {
        // tag(2) positive bignum mantissa: 0xc2 then bytes.
        let ok = vec![0x01u8; 64];
        let mut cbor_ok = vec![0xc2u8];
        cbor_ok.extend_from_slice(&def_bytes(&ok));
        assert!(matches!(
            decode_alonzo_pd(&cbor_ok).expect("64 mantissa ok"),
            PlutusData::Integer(_)
        ));

        let bad = vec![0x01u8; 65];
        let mut cbor_bad = vec![0xc2u8];
        cbor_bad.extend_from_slice(&def_bytes(&bad));
        let err = decode_alonzo_pd(&cbor_bad).expect_err("65 mantissa must reject");
        assert!(matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")));

        // tag(3) negative bignum: 65-byte indef chunk mantissa must also reject.
        let mut cbor_neg = vec![0xc3u8];
        cbor_neg.extend_from_slice(&indef_bytes(&[&bad]));
        let err3 = decode_alonzo_pd(&cbor_neg).expect_err("tag3 65 indef chunk must reject");
        assert!(matches!(&err3, SerializationError::CborDecode(m) if m.contains("64 bytes")));
    }

    /// OVER-STRICTNESS GUARD (Alonzo): a >64-byte non-PlutusData bytestring
    /// (e.g. an Alonzo PlutusV1 script blob) still decodes via the generic
    /// owned reader. The bound is PlutusData-leaf only.
    #[test]
    fn alonzo_over_strictness_guard_non_plutus_blob_over_64_ok() {
        let blob = vec![0x7Eu8; 128];
        let cbor = def_bytes(&blob);
        let mut r = Reader::new(&cbor);
        assert_eq!(
            r.read_bytes_owned()
                .expect(">64 non-plutus blob must decode"),
            blob
        );
    }

    /// Fix #744 — block-decoded Alonzo tx must have raw_cbor populated.
    /// Same invariant as Babbage: raw_cbor.len() = 1 + body + witness + 1 + 1.
    #[test]
    fn block_decoded_alonzo_tx_has_raw_cbor() {
        let cbor = make_alonzo_block(1);
        let block = decode_alonzo_block(&cbor).unwrap();
        let tx = &block.transactions[0];

        assert!(
            tx.raw_cbor.is_some(),
            "block-decoded Alonzo tx must have raw_cbor populated for fee calculation"
        );
        let raw = tx.raw_cbor.as_ref().unwrap();
        assert_eq!(
            raw[0], 0x84,
            "Alonzo tx raw_cbor must start with 0x84 (array-4)"
        );

        let body_len = tx.raw_body_cbor.as_ref().map_or(0, |b| b.len());
        let witness_len = tx.raw_witness_cbor.as_ref().map_or(0, |b| b.len());
        let expected_len = 1 + body_len + witness_len + 1 + 1;
        assert_eq!(
            raw.len(),
            expected_len,
            "raw_cbor.len()={} expected={}",
            raw.len(),
            expected_len
        );
    }
}
