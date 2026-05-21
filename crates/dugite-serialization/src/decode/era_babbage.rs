//! In-house decoder for the Babbage era (era tag 6).
//!
//! # Babbage block wire format
//!
//! After stripping the HFC envelope `[era_tag, inner]`, the inner CBOR is:
//!
//! ```text
//! block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set, invalid_transactions]
//! ```
//!
//! **5 elements** — same as Alonzo.
//!
//! ## Header differences from Alonzo
//!
//! The operational certificate is **promoted** to a nested struct:
//!
//! ```text
//! header_body = [
//!   block_number,            ; 0
//!   slot,                    ; 1
//!   prev_hash,               ; 2 bytes(32) or null
//!   issuer_vkey,             ; 3 bytes(32)
//!   vrf_vkey,                ; 4 bytes(32)
//!   vrf_result,              ; 5 [output_bytes(64), proof_bytes(80)]
//!   block_body_size,         ; 6 u64
//!   block_body_hash,         ; 7 bytes(32)
//!   operational_cert,        ; 8 [hot_vkey(32), seq_num, kes_period, sigma(64)]
//!   protocol_version,        ; 9 [major, minor]
//! ]
//! ```
//!
//! - Single `vrf_result` replaces `nonce_vrf_cert` + `leader_vrf_cert`.
//! - `operational_cert` is a 4-element array (was inline fields 9–12 in Alonzo).
//! - No nonce VRF proof field — Babbage/Conway Praos uses single VRF output.
//!
//! ## Tx body additions over Alonzo
//!
//! - key 16: collateral_return (post-Alonzo output format or legacy)
//! - key 17: total_collateral (coin)
//! - key 18: reference_inputs ([* transaction_input])
//!
//! ## TransactionOutput changes
//!
//! Babbage introduces a map-based output format (`{0: address, 1: value, ...}`)
//! alongside the legacy 2-element array format. Both are valid within the same block.
//!
//! The map format also supports inline datums (key 2, DatumOption) and script
//! references (key 3, ScriptRef encoded as tag(24) bytes).
//!
//! ## Witness set additions over Alonzo
//!
//! - key 6: plutus_v2_scripts ([* bytes])

use crate::decode::era_alonzo::{
    decode_alonzo_aux_data_map, read_plutus_data, read_value, DecodeMode,
};
use crate::decode::helpers::{read_hash28, read_hash32};
use crate::decode::raw::KeepRaw;
use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::address::Address;
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    BootstrapWitness, ExUnits, OutputDatum, PlutusData, Redeemer, RedeemerTag, ScriptRef,
    Transaction, TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
    VKeyWitness,
};
use dugite_primitives::value::Lovelace;
use minicbor::data::Type;
use std::collections::BTreeMap;

// ============================================================================
// Top-level entry points
// ============================================================================

/// Decode a Babbage block from the inner CBOR (after HFC envelope stripping).
pub fn decode_babbage_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_babbage_block_mode(inner_cbor, DecodeMode::Full)
}

/// Decode a Babbage block in minimal mode (witness set skipped).
pub fn decode_babbage_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_babbage_block_mode(inner_cbor, DecodeMode::Minimal)
}

fn decode_babbage_block_mode(
    inner_cbor: &[u8],
    mode: DecodeMode,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    // block = array(5)
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(5)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage block: expected array(5), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header
    // -------------------------------------------------------------------------
    let header = {
        let raw = KeepRaw::parse_with(&mut r, decode_babbage_header_inner)?;
        let header_hash = blake2b_256(raw.raw);
        let mut h = raw.value;
        h.header_hash = header_hash;
        h
    };

    // -------------------------------------------------------------------------
    // 2. tx_bodies
    // -------------------------------------------------------------------------
    let tx_count = r.read_array_header()?.unwrap_or(0) as usize;
    let mut raw_bodies: Vec<Vec<u8>> = Vec::with_capacity(tx_count);
    let mut parsed_bodies: Vec<TransactionBody> = Vec::with_capacity(tx_count);

    for _ in 0..tx_count {
        let body = KeepRaw::parse_with(&mut r, |r| decode_babbage_tx_body(r))?;
        raw_bodies.push(body.raw.to_vec());
        parsed_bodies.push(body.value);
    }

    // -------------------------------------------------------------------------
    // 3. tx_witness_sets
    // -------------------------------------------------------------------------
    let witness_count = r.read_array_header()?.unwrap_or(0) as usize;
    let mut raw_witnesses: Vec<Vec<u8>> = Vec::with_capacity(witness_count);
    let mut parsed_witnesses: Vec<Option<TransactionWitnessSet>> =
        Vec::with_capacity(witness_count);

    for _ in 0..witness_count {
        if mode == DecodeMode::Full {
            let ws = KeepRaw::parse_with(&mut r, |r| decode_babbage_witness_set(r))?;
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
    // 4. auxiliary_data_set
    // -------------------------------------------------------------------------
    let aux_map = decode_alonzo_aux_data_map(&mut r)?;

    // -------------------------------------------------------------------------
    // 5. invalid_transactions
    // -------------------------------------------------------------------------
    let mut invalid_tx_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let inv_count = r.read_array_header()?.unwrap_or(0) as usize;
    for _ in 0..inv_count {
        let idx = r.read_uint()? as usize;
        invalid_tx_set.insert(idx);
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

            Ok(Transaction {
                hash: tx_hash,
                era: Era::Babbage,
                body,
                witness_set,
                is_valid,
                auxiliary_data,
                raw_cbor: None,
                raw_body_cbor: Some(raw_body),
                raw_witness_cbor: raw_witness,
            })
        })
        .collect();
    let transactions = transactions?;

    Ok(Block {
        header,
        transactions,
        era: Era::Babbage,
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

fn decode_babbage_header_inner(r: &mut Reader<'_>) -> Result<BlockHeader, SerializationError> {
    // header = [header_body, kes_signature]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage header: expected array(2), got {hdr_arr:?}"
        )));
    }

    // header_body = array(10) — note: 10 elements in Babbage (not 15 like Shelley/Alonzo)
    let body_arr = r.read_array_header()?;
    if !matches!(body_arr, Some(10)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage header_body: expected array(10), got {body_arr:?}"
        )));
    }

    // 0: block_number
    let block_number = r.read_uint()?;
    // 1: slot
    let slot = r.read_uint()?;
    // 2: prev_hash
    let prev_hash = read_optional_hash32(r)?;
    // 3: issuer_vkey (32 bytes)
    let issuer_vkey = r.read_bytes()?.to_vec();
    // 4: vrf_vkey (32 bytes)
    let vrf_vkey = r.read_bytes()?.to_vec();
    // 5: vrf_result = [output_bytes(64), proof_bytes(80)]
    //    Babbage has a SINGLE vrf_result (no separate nonce/leader split).
    let (vrf_output, vrf_proof) = read_vrf_result(r)?;
    // 6: block_body_size
    let body_size = r.read_uint()?;
    // 7: block_body_hash
    let body_hash = read_hash32(r)?;
    // 8: operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]
    let (op_hot_vkey, op_seq_number, op_kes_period, op_sigma) = read_operational_cert(r)?;
    // 9: protocol_version = [major, minor]
    let (protocol_major, protocol_minor) = read_protocol_version(r)?;

    // KES signature (second element of outer array)
    let kes_signature = r.read_bytes()?.to_vec();

    Ok(BlockHeader {
        header_hash: Hash32::ZERO,
        prev_hash,
        issuer_vkey,
        vrf_vkey,
        vrf_result: VrfOutput {
            output: vrf_output.clone(),
            proof: vrf_proof,
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
        // Babbage/Conway Praos: nonce_vrf_output = blake2b_256("N" || vrf_result.output).
        // The "N" prefix tag and blake2b-256 hash match pallas's HeaderBody::nonce_vrf_output()
        // and Haskell's vrfNonceValue in the Praos era.
        nonce_vrf_output: {
            let mut tagged = b"N".to_vec();
            tagged.extend_from_slice(&vrf_output);
            blake2b_256(&tagged).as_bytes().to_vec()
        },
        // Babbage has no separate nonce VRF proof (single VRF certificate).
        nonce_vrf_proof: Vec::new(),
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

/// Read the Babbage `vrf_result = [output_bytes, proof_bytes]`.
fn read_vrf_result(r: &mut Reader<'_>) -> Result<(Vec<u8>, Vec<u8>), SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage vrf_result: expected array(2), got {arr_len:?}"
        )));
    }
    let output = r.read_bytes()?.to_vec();
    let proof = r.read_bytes()?.to_vec();
    Ok((output, proof))
}

/// Read the Babbage `operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]`.
fn read_operational_cert(
    r: &mut Reader<'_>,
) -> Result<(Vec<u8>, u64, u64, Vec<u8>), SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage operational_cert: expected array(4), got {arr_len:?}"
        )));
    }
    let hot_vkey = r.read_bytes()?.to_vec();
    let seq_number = r.read_uint()?;
    let kes_period = r.read_uint()?;
    let sigma = r.read_bytes()?.to_vec();
    Ok((hot_vkey, seq_number, kes_period, sigma))
}

/// Read `protocol_version = [major, minor]`.
fn read_protocol_version(r: &mut Reader<'_>) -> Result<(u64, u64), SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage protocol_version: expected array(2), got {arr_len:?}"
        )));
    }
    let major = r.read_uint()?;
    let minor = r.read_uint()?;
    Ok((major, minor))
}

// ============================================================================
// Transaction body decoder
// ============================================================================

/// Decode a Babbage transaction body.
///
/// Extends Alonzo with keys 16, 17, 18.
fn decode_babbage_tx_body(r: &mut Reader<'_>) -> Result<TransactionBody, SerializationError> {
    // Parse the map header first; then for Babbage-specific keys, handle them.
    // Strategy: decode using the Alonzo body decoder for keys 0-15, and then
    // handle 16-18 in an extended post-processing pass.
    //
    // Since we need to handle all keys in a single pass, we duplicate the map
    // parsing loop but add cases for keys 16, 17, 18.

    let mut inputs: Vec<TransactionInput> = Vec::new();
    let mut outputs: Vec<TransactionOutput> = Vec::new();
    let mut fee = Lovelace(0);
    let mut ttl: Option<SlotNo> = None;
    let mut certificates = Vec::new();
    let mut withdrawals = BTreeMap::new();
    let mut auxiliary_data_hash: Option<Hash32> = None;
    let mut validity_interval_start: Option<SlotNo> = None;
    let mut mint = BTreeMap::new();
    let mut script_data_hash: Option<Hash32> = None;
    let mut collateral: Vec<TransactionInput> = Vec::new();
    let mut required_signers = Vec::new();
    let mut network_id: Option<u8> = None;
    let mut collateral_return: Option<TransactionOutput> = None;
    let mut total_collateral: Option<Lovelace> = None;
    let mut reference_inputs: Vec<TransactionInput> = Vec::new();

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
                outputs = r.read_array(|r| read_babbage_tx_output(r))?;
            }
            2 => {
                fee = Lovelace(r.read_uint()?);
            }
            3 => {
                ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                // Re-use the Alonzo certificate decoder — same encoding in Babbage.
                certificates =
                    r.read_array(|r| crate::decode::era_alonzo::read_alonzo_cert_inner(r))?;
            }
            5 => {
                // withdrawals: { reward_account_bytes => coin }
                // Map may be definite or indefinite length.
                let entries = r.read_map(
                    |r| Ok(r.read_bytes()?.to_vec()),
                    |r| Ok(Lovelace(r.read_uint()?)),
                )?;
                for (account, coin) in entries {
                    withdrawals.insert(account, coin);
                }
            }
            6 => {
                r.skip()?; // update proposals
            }
            7 => {
                auxiliary_data_hash = Some(read_hash32(r)?);
            }
            8 => {
                validity_interval_start = Some(SlotNo(r.read_uint()?));
            }
            9 => {
                // mint: { policy_id => { asset_name => int } }
                mint = read_babbage_mint_map(r)?;
            }
            11 => {
                script_data_hash = Some(read_hash32(r)?);
            }
            13 => {
                collateral = r.read_array(read_tx_input)?;
            }
            14 => {
                required_signers = r.read_array(|r| {
                    let h28 = read_hash28(r)?;
                    Ok(h28.to_hash32_padded())
                })?;
            }
            15 => {
                let id = r.read_uint()?;
                network_id = Some(id as u8);
            }
            16 => {
                // collateral_return: a post-Alonzo output (map or legacy array)
                collateral_return = Some(read_babbage_tx_output(r)?);
            }
            17 => {
                // total_collateral: coin
                total_collateral = Some(Lovelace(r.read_uint()?));
            }
            18 => {
                // reference_inputs: [* transaction_input]
                reference_inputs = r.read_array(read_tx_input)?;
            }
            _ => {
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
        validity_interval_start,
        mint,
        script_data_hash,
        collateral,
        required_signers,
        network_id,
        collateral_return,
        total_collateral,
        reference_inputs,
        update: None,
        voting_procedures: BTreeMap::new(),
        proposal_procedures: Vec::new(),
        treasury_value: None,
        donation: None,
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

fn read_babbage_mint_map(
    r: &mut Reader<'_>,
) -> Result<
    BTreeMap<dugite_primitives::hash::Hash28, BTreeMap<dugite_primitives::value::AssetName, i64>>,
    SerializationError,
> {
    use dugite_primitives::hash::Hash28;
    use dugite_primitives::value::AssetName;
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
                    let name_bytes = r.read_bytes()?.to_vec();
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("mint: asset name too long".into())
                    })
                },
                |r| Ok(r.read_int()? as i64),
            )?;
            let mut assets = BTreeMap::new();
            for (k, v) in asset_entries {
                assets.insert(k, v);
            }
            Ok(assets)
        },
    )?;
    let mut result = BTreeMap::new();
    for (k, v) in entries {
        result.insert(k, v);
    }
    Ok(result)
}

// ============================================================================
// Transaction output decoder (Babbage: legacy array OR post-Alonzo map)
// ============================================================================

/// Detect whether the current CBOR value is a Babbage map-form output or legacy array.
///
/// A Babbage output is either:
/// - Legacy array: `[address_bytes, value]` or `[address_bytes, value, datum_hash]`
/// - Map form: `{ 0 => address_bytes, 1 => value, ? 2 => datum_option, ? 3 => script_ref }`
///
/// The discriminator is the CBOR major type: Array = legacy, Map = post-Alonzo.
fn read_babbage_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Array => {
            // Legacy Shelley-style: [address, value] or [address, value, datum_hash]
            read_babbage_legacy_output(r)
        }
        Type::Map => {
            // Post-Alonzo map: {0: address, 1: value, 2: datum_option, 3: script_ref}
            read_babbage_map_output(r)
        }
        other => Err(SerializationError::CborDecode(format!(
            "babbage tx_out: expected array or map, got {other}"
        ))),
    }
}

fn read_babbage_legacy_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let arr_len = r.read_array_header()?;
    let n = match arr_len {
        Some(2) | Some(3) => arr_len.unwrap(),
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "babbage legacy tx_out: expected array(2) or array(3), got {arr_len:?}"
            )));
        }
    };

    let addr_bytes = r.read_bytes()?.to_vec();
    let address = Address::from_bytes(&addr_bytes)
        .map_err(|e| SerializationError::InvalidData(format!("babbage output address: {e}")))?;

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

/// Read the Babbage/Conway post-Alonzo map-form output.
///
/// ```text
/// post_alonzo_transaction_output = {
///   0 : address,
///   1 : value,
///   ? 2 : datum_option,   ; DatumHash or InlineDatum
///   ? 3 : script_ref,     ; tag(24) bytes(script_cbor)
/// }
/// ```
fn read_babbage_map_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let map_len = r.read_map_header()?;
    let n_entries = match map_len {
        Some(n) => n as i64,
        None => -1,
    };

    let mut address: Option<Address> = None;
    let mut value = dugite_primitives::value::Value::lovelace(0);
    let mut datum = OutputDatum::None;
    let mut script_ref: Option<ScriptRef> = None;

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
                let addr_bytes = r.read_bytes()?.to_vec();
                address = Some(Address::from_bytes(&addr_bytes).map_err(|e| {
                    SerializationError::InvalidData(format!("babbage map output address: {e}"))
                })?);
            }
            1 => {
                value = read_value(r)?;
            }
            2 => {
                // datum_option = [0, bytes(32)]  ; DatumHash
                //              / [1, tag(24) bytes] ; InlineDatum
                datum = read_datum_option(r)?;
            }
            3 => {
                // script_ref: tag(24) bytes(script_cbor)
                // script_cbor = [type, script_bytes]
                script_ref = Some(read_script_ref(r)?);
            }
            _ => {
                r.skip()?;
            }
        }
    }

    let address = address.ok_or_else(|| {
        SerializationError::CborDecode("babbage map output: missing address (key 0)".into())
    })?;

    Ok(TransactionOutput {
        address,
        value,
        datum,
        script_ref,
        is_legacy: false,
        raw_cbor: None,
    })
}

/// Read a datum option: `[0, hash32] | [1, tag(24) bytes]`.
fn read_datum_option(r: &mut Reader<'_>) -> Result<OutputDatum, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "datum_option: expected array(2), got {arr_len:?}"
        )));
    }
    let disc = r.read_uint()?;
    match disc {
        0 => {
            // DatumHash: bytes(32)
            let hash = read_hash32(r)?;
            Ok(OutputDatum::DatumHash(hash))
        }
        1 => {
            // InlineDatum: tag(24) bytes containing PlutusData CBOR
            let inner_bytes = r.read_embedded_cbor_bytes()?.to_vec();
            // Parse the PlutusData from the inner bytes
            let mut inner_r = Reader::new(&inner_bytes);
            let data = read_plutus_data(&mut inner_r)?;
            Ok(OutputDatum::InlineDatum {
                data,
                raw_cbor: Some(inner_bytes),
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "datum_option: unknown discriminator {other}"
        ))),
    }
}

/// Read a script reference: `tag(24) bytes([type, script_bytes])`.
///
/// The script_cbor inside tag(24) is `[type, payload_bytes]` where:
/// - type 0 = NativeScript
/// - type 1 = PlutusV1
/// - type 2 = PlutusV2
/// - type 3 = PlutusV3
fn read_script_ref(r: &mut Reader<'_>) -> Result<ScriptRef, SerializationError> {
    let inner_bytes = r.read_embedded_cbor_bytes()?.to_vec();
    let mut inner_r = Reader::new(&inner_bytes);
    let arr_len = inner_r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "script_ref: expected array(2), got {arr_len:?}"
        )));
    }
    let script_type = inner_r.read_uint()?;
    match script_type {
        0 => {
            // NativeScript — the value is the native script CBOR bytes.
            // We already consumed the script_type (0). Re-parse from inner_bytes
            // with a fresh reader since we can't "un-read" the already-consumed bytes.
            let _ = inner_r; // drop to avoid unused-variable warning
            let mut fresh = Reader::new(&inner_bytes);
            let _arr = fresh.read_array_header()?; // [2]
            let _type = fresh.read_uint()?; // 0 — script type discriminator
                                            // The remaining value is the native script CBOR.
            let ns = crate::decode::era_alonzo::read_native_script_from_cbor(&mut fresh)?;
            Ok(ScriptRef::NativeScript(ns))
        }
        1 => {
            let script_bytes = inner_r.read_bytes()?.to_vec();
            Ok(ScriptRef::PlutusV1(script_bytes))
        }
        2 => {
            let script_bytes = inner_r.read_bytes()?.to_vec();
            Ok(ScriptRef::PlutusV2(script_bytes))
        }
        3 => {
            let script_bytes = inner_r.read_bytes()?.to_vec();
            Ok(ScriptRef::PlutusV3(script_bytes))
        }
        other => Err(SerializationError::CborDecode(format!(
            "script_ref: unknown type {other}"
        ))),
    }
}

// ============================================================================
// Witness set decoder
// ============================================================================

/// Decode a Babbage witness set.
///
/// Extends Alonzo with key 6 (plutus_v2_scripts).
fn decode_babbage_witness_set(
    r: &mut Reader<'_>,
) -> Result<TransactionWitnessSet, SerializationError> {
    let mut vkey_witnesses: Vec<VKeyWitness> = Vec::new();
    let mut native_scripts = Vec::new();
    let mut bootstrap_witnesses = Vec::new();
    let mut plutus_v1_scripts: Vec<Vec<u8>> = Vec::new();
    let mut plutus_v2_scripts: Vec<Vec<u8>> = Vec::new();
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
                    let vkey = r.read_bytes()?.to_vec();
                    let signature = r.read_bytes()?.to_vec();
                    Ok(VKeyWitness { vkey, signature })
                })?;
            }
            1 => {
                native_scripts =
                    r.read_array(|r| crate::decode::era_alonzo::read_native_script_from_cbor(r))?;
            }
            2 => {
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
            3 => {
                plutus_v1_scripts = r.read_array(|r| Ok(r.read_bytes()?.to_vec()))?;
            }
            4 => {
                let pd_start = r.position();
                let items = r.read_array(|r| read_plutus_data(r))?;
                raw_plutus_data = Some(r.slice_from(pd_start).to_vec());
                plutus_data = items;
            }
            5 => {
                let rd_start = r.position();
                let items = r.read_array(|r| read_babbage_redeemer(r))?;
                raw_redeemers = Some(r.slice_from(rd_start).to_vec());
                redeemers = items;
            }
            6 => {
                // plutus_v2_scripts (Babbage+)
                plutus_v2_scripts = r.read_array(|r| Ok(r.read_bytes()?.to_vec()))?;
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
        plutus_v1_scripts,
        plutus_v2_scripts,
        plutus_v3_scripts: Vec::new(),
        plutus_data,
        redeemers,
        raw_redeemers_cbor: raw_redeemers,
        raw_plutus_data_cbor: raw_plutus_data,
        original_script_data_hash: None,
    })
}

fn read_babbage_redeemer(r: &mut Reader<'_>) -> Result<Redeemer, SerializationError> {
    // Babbage redeemers are same as Alonzo: [tag, index, data, ex_units]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage redeemer: expected array(4), got {arr_len:?}"
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
                "babbage redeemer tag: unknown {other}"
            )));
        }
    };
    let index = r.read_uint()? as u32;
    let data = read_plutus_data(r)?;
    let arr_len2 = r.read_array_header()?;
    if !matches!(arr_len2, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "babbage ex_units: expected array(2), got {arr_len2:?}"
        )));
    }
    let mem = r.read_uint()?;
    let steps = r.read_uint()?;
    Ok(Redeemer {
        tag,
        index,
        data,
        ex_units: ExUnits { mem, steps },
    })
}

// ============================================================================
// Standalone tx decoder (Babbage era)
// ============================================================================

/// Decode a standalone Babbage-era transaction from raw CBOR bytes.
///
/// The standalone tx format is `[body_map, witness_set_map, is_valid_bool, aux_data]`.
///
/// The transaction hash is `blake2b_256(raw_body_cbor)`.
pub(crate) fn decode_babbage_tx_standalone(cbor: &[u8]) -> Result<Transaction, SerializationError> {
    use crate::decode::era_alonzo::decode_alonzo_auxiliary_data;

    let mut r = Reader::new(cbor);

    // tx = [body, witness_set, is_valid, aux_data]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(4) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "babbage tx: expected array(4), got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(
                "babbage tx: expected definite-length array".into(),
            ));
        }
    }

    // 1. Body — capture raw bytes for hash computation
    let body_raw = KeepRaw::parse_with(&mut r, |r| decode_babbage_tx_body(r))?;
    let raw_body_cbor = body_raw.raw.to_vec();
    let tx_hash = blake2b_256(&raw_body_cbor);
    let body = body_raw.value;

    // 2. Witness set
    let ws_raw = KeepRaw::parse_with(&mut r, |r| decode_babbage_witness_set(r))?;
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
        era: Era::Babbage,
        body,
        witness_set,
        is_valid,
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
    use crate::decode::reader::Reader;
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::blake2b_256;

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

    #[allow(dead_code)]
    fn cbor_null() -> Vec<u8> {
        vec![0xf6]
    }

    /// Build a minimal Babbage block inner CBOR.
    fn make_babbage_block(n_txs: usize) -> Vec<u8> {
        // vrf_result = [output(64), proof(80)]
        let vrf_out = cbor_bytes(&[0u8; 64]);
        let vrf_proof = cbor_bytes(&[0u8; 80]);
        let vrf_result = cbor_arr(&[&vrf_out, &vrf_proof]);

        // operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]
        let op_cert = cbor_arr(&[
            &cbor_bytes(&[0x10u8; 32]), // hot_vkey
            &cbor_uint(0),              // seq_num
            &cbor_uint(0),              // kes_period
            &cbor_bytes(&[0x11u8; 64]), // sigma
        ]);

        // protocol_version = [6, 0]
        let proto_ver = cbor_arr(&[&cbor_uint(6), &cbor_uint(0)]);

        // header_body = array(10)
        let mut hb = vec![0x8a]; // array(10)
        hb.extend(cbor_uint(100)); // block_number
        hb.extend(cbor_uint(777777)); // slot
        hb.extend(cbor_bytes(&[0xcc; 32])); // prev_hash
        hb.extend(cbor_bytes(&[0x01; 32])); // issuer_vkey
        hb.extend(cbor_bytes(&[0x02; 32])); // vrf_vkey
        hb.extend(&vrf_result); // vrf_result
        hb.extend(cbor_uint(0)); // body_size
        hb.extend(cbor_bytes(&[0x00; 32])); // body_hash
        hb.extend(&op_cert); // operational_cert
        hb.extend(&proto_ver); // protocol_version

        let kes_sig = cbor_bytes(&[0x05; 448]);
        let mut header = vec![0x82];
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
            tb.push(0x80);
            tb.extend(cbor_uint(1));
            tb.push(0x80);
            tb.extend(cbor_uint(2));
            tb.extend(cbor_uint(1_000_000));
            tx_bodies_v.extend(&tb);
            tx_witnesses_v.push(0xa0); // empty witness set {}
        }

        let aux_data = vec![0xa0]; // {}
        let invalid_txs = vec![0x80]; // []

        // block = array(5)
        let mut block = vec![0x85];
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
    fn babbage_empty_block_decodes() {
        let cbor = make_babbage_block(0);
        let block = decode_babbage_block(&cbor).unwrap();
        assert_eq!(block.era, Era::Babbage);
        assert_eq!(block.transactions.len(), 0);
        assert_eq!(block.header.slot.0, 777777);
        assert_eq!(block.header.block_number.0, 100);
    }

    #[test]
    fn babbage_header_has_promoted_opcert() {
        let cbor = make_babbage_block(0);
        let block = decode_babbage_block(&cbor).unwrap();
        assert_eq!(block.header.operational_cert.hot_vkey, vec![0x10u8; 32]);
        assert_eq!(block.header.operational_cert.sequence_number, 0);
    }

    #[test]
    fn babbage_header_hash_blake2b256() {
        let cbor = make_babbage_block(0);
        let block = decode_babbage_block(&cbor).unwrap();
        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        let header_start = r.position();
        r.skip().unwrap();
        let header_bytes = r.slice_from(header_start);
        assert_eq!(block.header.header_hash, blake2b_256(header_bytes));
    }

    #[test]
    fn babbage_tx_hash_blake2b256() {
        let cbor = make_babbage_block(1);
        let block = decode_babbage_block(&cbor).unwrap();
        let mut r = Reader::new(&cbor);
        r.read_array_header().unwrap();
        r.skip().unwrap();
        r.read_array_header().unwrap();
        let body_start = r.position();
        r.skip().unwrap();
        let body_bytes = r.slice_from(body_start);
        assert_eq!(block.transactions[0].hash, blake2b_256(body_bytes));
    }

    #[test]
    fn babbage_invalid_tx_marked() {
        let mut cbor = make_babbage_block(2);
        // Replace last byte (0x80 = empty invalid_txs) with array(1)[0]
        let pos = cbor.len() - 1;
        assert_eq!(cbor[pos], 0x80);
        cbor.pop();
        cbor.push(0x81); // array(1)
        cbor.push(0x00); // uint(0)
        let block = decode_babbage_block(&cbor).unwrap();
        assert!(!block.transactions[0].is_valid);
        assert!(block.transactions[1].is_valid);
    }

    #[test]
    fn babbage_vrf_result_stored_in_both_fields() {
        let cbor = make_babbage_block(0);
        let block = decode_babbage_block(&cbor).unwrap();
        // In Babbage, vrf_result.output holds the raw 64-byte output.
        assert_eq!(block.header.vrf_result.output, vec![0u8; 64]);
        // nonce_vrf_output = blake2b_256("N" || vrf_result.output), matching pallas.
        let expected_nonce = {
            let mut tagged = b"N".to_vec();
            tagged.extend_from_slice(&[0u8; 64]);
            blake2b_256(&tagged).as_bytes().to_vec()
        };
        assert_eq!(block.header.nonce_vrf_output, expected_nonce);
        assert_eq!(
            block.header.nonce_vrf_output.len(),
            32,
            "nonce_vrf_output is 32 bytes (blake2b-256)"
        );
        // Babbage has no separate nonce VRF proof
        assert!(block.header.nonce_vrf_proof.is_empty());
    }

    #[test]
    fn babbage_reference_inputs_decoded() {
        // Build a tx body with reference_inputs (key 18)
        let tx_id = [0xbb; 32];
        let mut inp_cbor = vec![0x82]; // array(2)
        inp_cbor.extend(cbor_bytes(&tx_id));
        inp_cbor.extend(cbor_uint(0));
        let mut ref_arr = vec![0x81]; // array(1)
        ref_arr.extend(&inp_cbor);

        let mut tb = vec![0xa3]; // map(3)
        tb.extend(cbor_uint(0));
        tb.push(0x80);
        tb.extend(cbor_uint(2));
        tb.extend(cbor_uint(500_000));
        tb.extend(cbor_uint(18)); // reference_inputs key
        tb.extend(&ref_arr);

        let raw =
            KeepRaw::parse_with(&mut Reader::new(&tb), |r| decode_babbage_tx_body(r)).unwrap();
        assert_eq!(raw.value.reference_inputs.len(), 1);
        assert_eq!(
            raw.value.reference_inputs[0].transaction_id.as_bytes(),
            &tx_id
        );
    }

    #[test]
    fn babbage_post_alonzo_map_output_decodes() {
        // Build a minimal post-Alonzo map output: {0: addr_bytes, 1: coin}
        // Use a testnet base address: 0x60 + 28 zero bytes for stake, 28 zero bytes for payment
        // Actually a simple enterprise address: 0x60 + 28 bytes (Shelley enterprise testnet)
        let addr_bytes: Vec<u8> = {
            let mut v = vec![0x60]; // enterprise testnet header
            v.extend_from_slice(&[0u8; 28]); // payment key hash
            v
        };

        let mut out_map = vec![0xa2]; // map(2)
        out_map.extend(cbor_uint(0));
        out_map.extend(cbor_bytes(&addr_bytes));
        out_map.extend(cbor_uint(1));
        out_map.extend(cbor_uint(2_000_000)); // 2 ADA

        let mut r = Reader::new(&out_map);
        let out = read_babbage_tx_output(&mut r).unwrap();
        assert!(
            !out.is_legacy,
            "map-form output should have is_legacy=false"
        );
        assert_eq!(out.value.coin.0, 2_000_000);
    }

    #[test]
    fn babbage_legacy_array_output_decodes() {
        let addr_bytes: Vec<u8> = {
            let mut v = vec![0x60]; // enterprise testnet header
            v.extend_from_slice(&[0u8; 28]);
            v
        };

        // Legacy: [addr_bytes, coin]
        let mut out_arr = vec![0x82]; // array(2)
        out_arr.extend(cbor_bytes(&addr_bytes));
        out_arr.extend(cbor_uint(3_000_000));

        let mut r = Reader::new(&out_arr);
        let out = read_babbage_tx_output(&mut r).unwrap();
        assert!(
            out.is_legacy,
            "array-form output should have is_legacy=true"
        );
        assert_eq!(out.value.coin.0, 3_000_000);
    }

    #[test]
    fn babbage_inline_datum_in_map_output() {
        // Build a map output with inline datum: {0: addr, 1: coin, 2: [1, tag(24) bytes]}
        let addr_bytes: Vec<u8> = {
            let mut v = vec![0x60];
            v.extend_from_slice(&[0u8; 28]);
            v
        };

        // InlineDatum = [1, tag(24) bytes(datum_cbor)]
        // datum_cbor = uint(42) = 0x18 0x2a
        let datum_cbor = vec![0x18u8, 0x2a]; // uint(42)
        let mut inline_datum = vec![0x82]; // array(2)
        inline_datum.push(0x01); // discriminator 1 = InlineDatum
                                 // tag(24) bytes(datum_cbor)
        inline_datum.push(0xd8); // tag major=6, info=24
        inline_datum.push(0x18); // tag value = 24
        inline_datum.extend(cbor_bytes(&datum_cbor));

        let mut out_map = vec![0xa3]; // map(3)
        out_map.extend(cbor_uint(0));
        out_map.extend(cbor_bytes(&addr_bytes));
        out_map.extend(cbor_uint(1));
        out_map.extend(cbor_uint(1_000_000));
        out_map.extend(cbor_uint(2));
        out_map.extend(&inline_datum);

        let mut r = Reader::new(&out_map);
        let out = read_babbage_tx_output(&mut r).unwrap();
        match &out.datum {
            OutputDatum::InlineDatum { data, raw_cbor } => {
                assert_eq!(data, &PlutusData::Integer(num_bigint::BigInt::from(42u64)));
                assert!(raw_cbor.is_some());
            }
            other => panic!("expected InlineDatum, got {other:?}"),
        }
    }

    #[test]
    fn babbage_minimal_mode_skips_witnesses() {
        let cbor = make_babbage_block(1);
        let block = decode_babbage_block_minimal(&cbor).unwrap();
        assert!(block.transactions[0].witness_set.vkey_witnesses.is_empty());
    }
}
