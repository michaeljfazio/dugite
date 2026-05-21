//! In-house decoder for the Conway era (era tag 7) and Dijkstra era (era tag 8).
//!
//! # Conway block wire format
//!
//! After stripping the HFC envelope `[era_tag, inner]`, the inner CBOR is:
//!
//! ```text
//! block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set, invalid_txs]
//! ```
//!
//! **5 elements** — same as Alonzo/Babbage. The `invalid_txs` list carries indices
//! of transactions that failed phase-1 (their collateral is consumed instead).
//!
//! ## Header structure (Babbage/Conway Praos style)
//!
//! ```text
//! header = [header_body, kes_signature]
//! header_body = [
//!   block_number,       ; 0 — u64
//!   slot,               ; 1 — u64
//!   prev_hash,          ; 2 — bytes(32) or null
//!   issuer_vkey,        ; 3 — bytes(32)
//!   vrf_vkey,           ; 4 — bytes(32)
//!   vrf_result,         ; 5 — [bytes, bytes]  (single combined cert, no separate nonce)
//!   block_body_size,    ; 6 — u64
//!   block_body_hash,    ; 7 — bytes(32)
//!   operational_cert,   ; 8 — [hot_vkey, seq_num, kes_period, sigma]
//!   protocol_version,   ; 9 — [major, minor]
//! ]
//! ```
//!
//! ## Conway TxBody additions over Babbage
//!
//! | Key | Field | Type |
//! |-----|-------|------|
//! | 19  | voting_procedures | `{ voter => { gov_action_id => voting_procedure } }` |
//! | 20  | proposal_procedures | `[* proposal_procedure]` |
//! | 21  | current_treasury_value | uint (optional) |
//! | 22  | donation | uint (optional) |
//!
//! ## Conway WitnessSet additions over Babbage
//!
//! | Key | Field |
//! |-----|-------|
//! | 5   | redeemers — map form in Conway: `{ [tag, index] => [data, ex_units] }` |
//! | 7   | plutus_v3_scripts |
//!
//! ## Sets (tag 258)
//!
//! Conway+ wraps set fields with tag 258. `Reader::read_set` handles both
//! tagged and untagged forms transparently.

use crate::decode::era_shelley::DecodeMode;
use crate::decode::helpers::{read_hash28, read_hash32, read_lovelace};
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
    Anchor, AuxiliaryData, BootstrapWitness, Certificate, Constitution, CostModels, DRep,
    ExUnitPrices, ExUnits, GovAction, GovActionId, MIRSource, MIRTarget, NativeScript, OutputDatum,
    PlutusData, PoolMetadata, PoolParams, ProposalProcedure, ProtocolParamUpdate, Rational,
    Redeemer, RedeemerTag, Transaction, TransactionBody, TransactionInput, TransactionMetadatum,
    TransactionOutput, TransactionWitnessSet, VKeyWitness, Vote, Voter, VotingProcedure,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};
use minicbor::data::Type;
use std::collections::BTreeMap;

// ============================================================================
// Top-level entry points
// ============================================================================

/// Decode a Conway block from the inner CBOR (after HFC envelope stripping).
pub fn decode_conway_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_conway_block_mode(inner_cbor, DecodeMode::Full, Era::Conway)
}

/// Decode a Conway block in minimal mode (witness set skipped).
pub fn decode_conway_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_conway_block_mode(inner_cbor, DecodeMode::Minimal, Era::Conway)
}

/// Decode a Dijkstra block from the inner CBOR (after HFC envelope stripping).
///
/// Dijkstra uses the same structure as Conway but with era tag 8.
/// Unknown TxBody keys (e.g. 14/23/25/26 per current CIPs) are silently skipped.
pub fn decode_dijkstra_block(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_conway_block_mode(inner_cbor, DecodeMode::Full, Era::Dijkstra)
}

/// Decode a Dijkstra block in minimal mode.
pub fn decode_dijkstra_block_minimal(inner_cbor: &[u8]) -> Result<Block, SerializationError> {
    decode_conway_block_mode(inner_cbor, DecodeMode::Minimal, Era::Dijkstra)
}

// ============================================================================
// Block decoder (Conway + Dijkstra share the same structure)
// ============================================================================

fn decode_conway_block_mode(
    inner_cbor: &[u8],
    mode: DecodeMode,
    era: Era,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    // block = [header, tx_bodies, tx_witness_sets, auxiliary_data_set, invalid_txs]
    // 5 elements (Alonzo+).
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(5)) {
        return Err(SerializationError::CborDecode(format!(
            "conway block: expected array(5), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header
    // -------------------------------------------------------------------------
    let header = {
        let raw = KeepRaw::parse_with(&mut r, decode_conway_header_inner)?;
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
        let body = KeepRaw::parse_with(&mut r, decode_conway_tx_body)?;
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
            let ws = KeepRaw::parse_with(&mut r, decode_conway_witness_set)?;
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
    let aux_map = decode_aux_data_map(&mut r)?;

    // -------------------------------------------------------------------------
    // 5. invalid_txs — set of tx indices that failed phase-1
    // -------------------------------------------------------------------------
    let invalid_tx_indices: Vec<u64> = r.read_set(|r| r.read_uint())?;
    let invalid_set: std::collections::HashSet<usize> =
        invalid_tx_indices.into_iter().map(|i| i as usize).collect();

    // -------------------------------------------------------------------------
    // Build transactions
    // -------------------------------------------------------------------------
    let transactions: Result<Vec<Transaction>, SerializationError> = parsed_bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            let raw_body = raw_bodies[i].clone();
            let tx_hash = blake2b_256(&raw_body);
            let is_valid = !invalid_set.contains(&i);

            let witness_set = match parsed_witnesses.get(i).and_then(|w| w.as_ref()) {
                Some(ws) => ws.clone(),
                None => empty_witness_set(),
            };
            let raw_witness = raw_witnesses.get(i).cloned();
            let auxiliary_data = aux_map.get(&(i as u32)).cloned();

            Ok(Transaction {
                hash: tx_hash,
                era,
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
        era,
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
// Header decoder (Babbage/Conway Praos style)
// ============================================================================

fn decode_conway_header_inner(r: &mut Reader<'_>) -> Result<BlockHeader, SerializationError> {
    // header = [header_body, kes_signature]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "conway header: expected array(2), got {hdr_arr:?}"
        )));
    }

    // header_body = array(10)
    let body_arr = r.read_array_header()?;
    if !matches!(body_arr, Some(10)) {
        return Err(SerializationError::CborDecode(format!(
            "conway header_body: expected array(10), got {body_arr:?}"
        )));
    }

    // 0: block_number
    let block_number = r.read_uint()?;
    // 1: slot
    let slot = r.read_uint()?;
    // 2: prev_hash (32-byte bytes or null)
    let prev_hash = read_optional_hash32(r)?;
    // 3: issuer_vkey (32 bytes)
    let issuer_vkey = r.read_bytes()?.to_vec();
    // 4: vrf_vkey (32 bytes)
    let vrf_vkey = r.read_bytes()?.to_vec();
    // 5: vrf_result = [output_bytes, proof_bytes] (single combined Praos cert)
    let (vrf_output, vrf_proof) = read_vrf_cert(r)?;
    // 6: block_body_size
    let body_size = r.read_uint()?;
    // 7: block_body_hash (32 bytes)
    let body_hash = read_hash32(r)?;
    // 8: operational_cert = [hot_vkey, seq_num, kes_period, sigma]
    let op_cert = read_operational_cert(r)?;
    // 9: protocol_version = [major, minor]
    let protocol_version = read_protocol_version(r)?;

    // KES signature (second element of outer array)
    let kes_signature = r.read_bytes()?.to_vec();

    Ok(BlockHeader {
        header_hash: Hash32::ZERO, // filled in by caller after blake2b_256(raw)
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
        operational_cert: op_cert,
        protocol_version,
        kes_signature,
        // Babbage/Conway Praos: nonce_vrf_output = blake2b_256("N" || vrf_result.output)
        // This matches the `vrfNonceValue` computation in the Haskell Praos era.
        nonce_vrf_output: {
            let mut nonce_input = Vec::with_capacity(1 + vrf_output.len());
            nonce_input.push(b'N');
            nonce_input.extend_from_slice(&vrf_output);
            blake2b_256(&nonce_input).to_vec()
        },
        nonce_vrf_proof: Vec::new(), // Praos has no separate nonce proof
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
    let output = r.read_bytes()?.to_vec();
    let proof = r.read_bytes()?.to_vec();
    Ok((output, proof))
}

fn read_operational_cert(r: &mut Reader<'_>) -> Result<OperationalCert, SerializationError> {
    // operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "operational_cert: expected array(4), got {arr_len:?}"
        )));
    }
    let hot_vkey = r.read_bytes()?.to_vec();
    let sequence_number = r.read_uint()?;
    let kes_period = r.read_uint()?;
    let sigma = r.read_bytes()?.to_vec();
    Ok(OperationalCert {
        hot_vkey,
        sequence_number,
        kes_period,
        sigma,
    })
}

fn read_protocol_version(r: &mut Reader<'_>) -> Result<ProtocolVersion, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "protocol_version: expected array(2), got {arr_len:?}"
        )));
    }
    let major = r.read_uint()?;
    let minor = r.read_uint()?;
    Ok(ProtocolVersion { major, minor })
}

// ============================================================================
// Transaction body decoder (Conway)
// ============================================================================

fn decode_conway_tx_body(r: &mut Reader<'_>) -> Result<TransactionBody, SerializationError> {
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
    let mut collateral_return: Option<TransactionOutput> = None;
    let mut total_collateral: Option<Lovelace> = None;
    let mut reference_inputs: Vec<TransactionInput> = Vec::new();
    let mut voting_procedures: BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>> =
        BTreeMap::new();
    let mut proposal_procedures: Vec<ProposalProcedure> = Vec::new();
    let mut treasury_value: Option<Lovelace> = None;
    let mut donation: Option<Lovelace> = None;

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
                // inputs: set<transaction_input>  (tag 258 in Conway)
                inputs = r.read_set(read_tx_input)?;
            }
            1 => {
                // outputs: [* transaction_output]
                outputs = r.read_array(|r| read_babbage_tx_output(r))?;
            }
            2 => {
                fee = read_lovelace(r)?;
            }
            3 => {
                ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                // certificates: set<certificate>
                certificates = r.read_set(|r| read_conway_certificate(r))?;
            }
            5 => {
                // withdrawals: { reward_account => coin }
                withdrawals = read_withdrawals(r)?;
            }
            7 => {
                // auxiliary_data_hash
                auxiliary_data_hash = Some(read_hash32(r)?);
            }
            8 => {
                // validity_interval_start (Allegra+)
                validity_interval_start = Some(SlotNo(r.read_uint()?));
            }
            9 => {
                // mint: { policy_id => { asset_name => i64 } }
                mint = read_mint_map(r)?;
            }
            11 => {
                // script_data_hash
                script_data_hash = Some(read_hash32(r)?);
            }
            13 => {
                // collateral inputs: set<transaction_input>
                collateral = r.read_set(read_tx_input)?;
            }
            14 => {
                // required_signers: set<addr_keyhash> — stored as padded Hash32
                required_signers = r.read_set(|r| {
                    let h28 = read_hash28(r)?;
                    Ok(h28.to_hash32_padded())
                })?;
            }
            15 => {
                // network_id
                let raw = r.read_uint()?;
                network_id = match raw {
                    0 | 1 => Some(raw as u8),
                    _ => None,
                };
            }
            16 => {
                // collateral_return: transaction_output
                collateral_return = Some(read_babbage_tx_output(r)?);
            }
            17 => {
                // total_collateral
                total_collateral = Some(read_lovelace(r)?);
            }
            18 => {
                // reference_inputs: set<transaction_input>
                reference_inputs = r.read_set(read_tx_input)?;
            }
            19 => {
                // voting_procedures: { voter => { gov_action_id => voting_procedure } }
                voting_procedures = read_voting_procedures(r)?;
            }
            20 => {
                // proposal_procedures: set<proposal_procedure>
                proposal_procedures = r.read_set(|r| read_proposal_procedure(r))?;
            }
            21 => {
                // current_treasury_value
                treasury_value = Some(read_lovelace(r)?);
            }
            22 => {
                // donation
                donation = Some(read_lovelace(r)?);
            }
            6 => {
                // update (pre-Conway field) — skip in Conway
                r.skip()?;
            }
            _ => {
                // Unknown key — skip for forward compatibility (Dijkstra may add keys)
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
        update: None, // Conway doesn't use pre-Conway update proposals
        voting_procedures,
        proposal_procedures,
        treasury_value,
        donation,
    })
}

fn read_tx_input(r: &mut Reader<'_>) -> Result<TransactionInput, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "tx_input: expected array(2), got {arr_len:?}"
        )));
    }
    let tx_hash = read_hash32(r)?;
    let index = r.read_uint()? as u32;
    Ok(TransactionInput {
        transaction_id: tx_hash,
        index,
    })
}

/// Read a Babbage/Conway post-Alonzo transaction output.
///
/// Conway outputs can be encoded in two forms:
/// 1. **Legacy array**: `[address, value]` or `[address, value, datum_hash]`
/// 2. **Post-Alonzo map**: `{ 0: address, 1: value, ? 2: datum_option, ? 3: script_ref }`
fn read_babbage_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Array => {
            // Legacy array form
            read_legacy_tx_output(r)
        }
        Type::Map => {
            // Post-Alonzo map form
            read_map_tx_output(r)
        }
        other => Err(SerializationError::CborDecode(format!(
            "tx_output: expected array or map, got {other}"
        ))),
    }
}

fn read_legacy_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let arr_len = r.read_array_header()?;
    let n = match arr_len {
        Some(2) | Some(3) => arr_len.unwrap(),
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "legacy tx_out: expected array(2) or array(3), got {arr_len:?}"
            )));
        }
    };
    let addr_bytes = r.read_bytes()?.to_vec();
    let address = Address::from_bytes(&addr_bytes)
        .map_err(|e| SerializationError::InvalidData(format!("output address: {e}")))?;
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

fn read_map_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let map_len = r.read_map_header()?;
    let n = map_len.unwrap_or(0) as usize;

    let mut address_bytes: Option<Vec<u8>> = None;
    let mut value: Option<Value> = None;
    let mut datum = OutputDatum::None;
    let mut script_ref = None;

    for _ in 0..n {
        let key = r.read_uint()?;
        match key {
            0 => {
                address_bytes = Some(r.read_bytes()?.to_vec());
            }
            1 => {
                value = Some(read_value(r)?);
            }
            2 => {
                datum = read_datum_option(r)?;
            }
            3 => {
                script_ref = Some(read_script_ref(r)?);
            }
            _ => {
                r.skip()?;
            }
        }
    }

    let addr_bytes = address_bytes.ok_or_else(|| {
        SerializationError::CborDecode("map tx_out: missing address (key 0)".into())
    })?;
    let address = Address::from_bytes(&addr_bytes)
        .map_err(|e| SerializationError::InvalidData(format!("map output address: {e}")))?;
    let value = value.unwrap_or_else(|| Value::lovelace(0));

    Ok(TransactionOutput {
        address,
        value,
        datum,
        script_ref,
        is_legacy: false,
        raw_cbor: None,
    })
}

/// Read a value: plain uint or `[coin, multiasset_map]`.
fn read_value(r: &mut Reader<'_>) -> Result<Value, SerializationError> {
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
    // Use read_map to handle both definite- and indefinite-length outer map.
    let policy_pairs = r.read_map(
        |r| {
            let policy_bytes = r.read_bytes()?;
            let policy =
                Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                    expected: 28,
                    got: policy_bytes.len(),
                })?;
            Ok(policy)
        },
        |r| {
            // Inner asset map — also use read_map to handle indefinite lengths.
            let asset_pairs = r.read_map(
                |r| {
                    let name_bytes = r.read_bytes()?.to_vec();
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("multiasset: asset name too long".into())
                    })
                },
                |r| r.read_uint(),
            )?;
            Ok(asset_pairs.into_iter().collect::<BTreeMap<_, _>>())
        },
    )?;
    Ok(policy_pairs.into_iter().collect())
}

fn read_mint_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Hash28, BTreeMap<AssetName, i64>>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length outer map.
    let policy_pairs = r.read_map(
        |r| {
            let policy_bytes = r.read_bytes()?;
            let policy =
                Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                    expected: 28,
                    got: policy_bytes.len(),
                })?;
            Ok(policy)
        },
        |r| {
            // Inner asset map — also use read_map to handle indefinite lengths.
            let asset_pairs = r.read_map(
                |r| {
                    let name_bytes = r.read_bytes()?.to_vec();
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("mint: asset name too long".into())
                    })
                },
                |r| Ok(r.read_int()? as i64),
            )?;
            Ok(asset_pairs.into_iter().collect::<BTreeMap<_, _>>())
        },
    )?;
    Ok(policy_pairs.into_iter().collect())
}

fn read_withdrawals(r: &mut Reader<'_>) -> Result<BTreeMap<Vec<u8>, Lovelace>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(
        |r| Ok(r.read_bytes()?.to_vec()),
        |r| r.read_uint().map(Lovelace),
    )?;
    Ok(pairs.into_iter().collect())
}

/// Read a datum_option: `[0, datum_hash]` or `[1, tag(24) bytes]`.
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
            let dh = read_hash32(r)?;
            Ok(OutputDatum::DatumHash(dh))
        }
        1 => {
            // Inline datum: tag(24) bytes containing serialized PlutusData
            let inner_bytes = r.read_embedded_cbor_bytes()?.to_vec();
            let mut inner_r = Reader::new(&inner_bytes);
            let pd = read_plutus_data(&mut inner_r)?;
            Ok(OutputDatum::InlineDatum {
                data: pd,
                raw_cbor: Some(inner_bytes),
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "datum_option: unknown discriminator {other}"
        ))),
    }
}

/// Read a script reference: tag(24) bytes wrapping `[script_type, script_bytes]`.
fn read_script_ref(
    r: &mut Reader<'_>,
) -> Result<dugite_primitives::transaction::ScriptRef, SerializationError> {
    use dugite_primitives::transaction::ScriptRef;
    // script_ref = #6.24(bytes .cbor script)
    let inner = r.read_embedded_cbor_bytes()?.to_vec();
    let mut sr = Reader::new(&inner);
    let arr_len = sr.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "script_ref: expected array(2), got {arr_len:?}"
        )));
    }
    let script_type = sr.read_uint()?;
    let script_bytes = sr.read_bytes()?.to_vec();
    match script_type {
        0 => {
            // Native script — decode it
            let mut ns_r = Reader::new(&script_bytes);
            let ns = read_native_script(&mut ns_r)?;
            Ok(ScriptRef::NativeScript(ns))
        }
        1 => Ok(ScriptRef::PlutusV1(script_bytes)),
        2 => Ok(ScriptRef::PlutusV2(script_bytes)),
        3 => Ok(ScriptRef::PlutusV3(script_bytes)),
        other => Err(SerializationError::CborDecode(format!(
            "script_ref: unknown script type {other}"
        ))),
    }
}

// ============================================================================
// Certificate decoder (Conway)
// ============================================================================

/// Read a single Conway-era certificate.
///
/// Conway certificate types (extending Shelley):
/// ```text
/// 0 = stake_registration        (Shelley legacy)
/// 1 = stake_deregistration      (Shelley legacy)
/// 2 = stake_delegation          (Shelley legacy)
/// 3 = pool_registration         (Shelley legacy)
/// 4 = pool_retirement           (Shelley legacy)
/// 5 = genesis_key_delegation    (Shelley legacy, deprecated)
/// 6 = move_instantaneous_reward (Shelley legacy, deprecated)
/// 7 = reg (Conway: stake reg + deposit)
/// 8 = unreg (Conway: stake unreg + refund)
/// 9 = vote_deleg (Conway: delegate vote to DRep)
/// 10 = stake_vote_deleg (Conway: delegate both pool + DRep)
/// 11 = stake_reg_deleg (Conway: reg + pool deleg)
/// 12 = vote_reg_deleg (Conway: reg + DRep deleg)
/// 13 = stake_vote_reg_deleg (Conway: reg + pool + DRep)
/// 14 = auth_committee_hot (Conway: committee hot auth)
/// 15 = resign_committee_cold (Conway: committee cold resign)
/// 16 = reg_drep (Conway: DRep registration)
/// 17 = unreg_drep (Conway: DRep deregistration)
/// 18 = update_drep (Conway: DRep update)
/// ```
fn read_conway_certificate(r: &mut Reader<'_>) -> Result<Certificate, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "certificate: expected definite-length array".into(),
        ));
    }
    let cert_type = r.read_uint()?;
    match cert_type {
        // ── Shelley legacy certificates ──────────────────────────────────────
        0 => {
            let cred = read_stake_credential(r)?;
            Ok(Certificate::StakeRegistration(cred))
        }
        1 => {
            let cred = read_stake_credential(r)?;
            Ok(Certificate::StakeDeregistration(cred))
        }
        2 => {
            let cred = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            Ok(Certificate::StakeDelegation {
                credential: cred,
                pool_hash,
            })
        }
        3 => {
            let params = read_pool_params(r)?;
            Ok(Certificate::PoolRegistration(params))
        }
        4 => {
            let pool_hash = read_hash28_cert(r)?;
            let epoch = r.read_uint()?;
            Ok(Certificate::PoolRetirement { pool_hash, epoch })
        }
        5 => {
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
            let cert = read_mir_cert(r)?;
            Ok(cert)
        }
        // ── Conway certificates ───────────────────────────────────────────────
        7 => {
            // reg: stake registration with explicit deposit
            let credential = read_stake_credential(r)?;
            let deposit = read_lovelace(r)?;
            Ok(Certificate::ConwayStakeRegistration {
                credential,
                deposit,
            })
        }
        8 => {
            // unreg: stake deregistration with refund
            let credential = read_stake_credential(r)?;
            let refund = read_lovelace(r)?;
            Ok(Certificate::ConwayStakeDeregistration { credential, refund })
        }
        9 => {
            // vote_deleg: delegate vote to DRep
            let credential = read_stake_credential(r)?;
            let drep = read_drep(r)?;
            Ok(Certificate::VoteDelegation { credential, drep })
        }
        10 => {
            // stake_vote_deleg: delegate to pool + DRep
            let credential = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            let drep = read_drep(r)?;
            Ok(Certificate::StakeVoteDelegation {
                credential,
                pool_hash,
                drep,
            })
        }
        11 => {
            // stake_reg_deleg: register + delegate to pool
            let credential = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            let deposit = read_lovelace(r)?;
            Ok(Certificate::RegStakeDeleg {
                credential,
                pool_hash,
                deposit,
            })
        }
        12 => {
            // vote_reg_deleg: register + delegate to DRep
            let credential = read_stake_credential(r)?;
            let drep = read_drep(r)?;
            let deposit = read_lovelace(r)?;
            Ok(Certificate::VoteRegDeleg {
                credential,
                drep,
                deposit,
            })
        }
        13 => {
            // stake_vote_reg_deleg: register + pool + DRep
            let credential = read_stake_credential(r)?;
            let pool_hash = read_hash28_cert(r)?;
            let drep = read_drep(r)?;
            let deposit = read_lovelace(r)?;
            Ok(Certificate::RegStakeVoteDeleg {
                credential,
                pool_hash,
                drep,
                deposit,
            })
        }
        14 => {
            // auth_committee_hot
            let cold_credential = read_stake_credential(r)?;
            let hot_credential = read_stake_credential(r)?;
            Ok(Certificate::CommitteeHotAuth {
                cold_credential,
                hot_credential,
            })
        }
        15 => {
            // resign_committee_cold
            let cold_credential = read_stake_credential(r)?;
            let anchor = read_optional_anchor(r)?;
            Ok(Certificate::CommitteeColdResign {
                cold_credential,
                anchor,
            })
        }
        16 => {
            // reg_drep
            let credential = read_stake_credential(r)?;
            let deposit = read_lovelace(r)?;
            let anchor = read_optional_anchor(r)?;
            Ok(Certificate::RegDRep {
                credential,
                deposit,
                anchor,
            })
        }
        17 => {
            // unreg_drep
            let credential = read_stake_credential(r)?;
            let refund = read_lovelace(r)?;
            Ok(Certificate::UnregDRep { credential, refund })
        }
        18 => {
            // update_drep
            let credential = read_stake_credential(r)?;
            let anchor = read_optional_anchor(r)?;
            Ok(Certificate::UpdateDRep { credential, anchor })
        }
        other => Err(SerializationError::CborDecode(format!(
            "conway certificate: unknown type {other}"
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

/// Read a DRep:
/// ```text
/// drep = [0, addr_keyhash]   ; KeyHash
///      / [1, scripthash]     ; ScriptHash
///      / [2]                 ; Abstain
///      / [3]                 ; NoConfidence
/// ```
fn read_drep(r: &mut Reader<'_>) -> Result<DRep, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "drep: expected definite-length array".into(),
        ));
    }
    let disc = r.read_uint()?;
    match disc {
        0 => {
            let h28 = read_hash28_cert(r)?;
            Ok(DRep::KeyHash(h28.to_hash32_padded()))
        }
        1 => {
            let h28 = read_hash28_cert(r)?;
            Ok(DRep::ScriptHash(h28))
        }
        2 => Ok(DRep::Abstain),
        3 => Ok(DRep::NoConfidence),
        other => Err(SerializationError::CborDecode(format!(
            "drep: unknown discriminator {other}"
        ))),
    }
}

fn read_optional_anchor(r: &mut Reader<'_>) -> Result<Option<Anchor>, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    Ok(Some(read_anchor(r)?))
}

fn read_anchor(r: &mut Reader<'_>) -> Result<Anchor, SerializationError> {
    // anchor = [url, data_hash(32)]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "anchor: expected array(2), got {arr_len:?}"
        )));
    }
    let url_bytes = r.read_bytes()?;
    let url = String::from_utf8(url_bytes.to_vec())
        .map_err(|_| SerializationError::CborDecode("anchor: URL is not valid UTF-8".into()))?;
    let data_hash = read_hash32(r)?;
    Ok(Anchor { url, data_hash })
}

fn read_pool_params(r: &mut Reader<'_>) -> Result<PoolParams, SerializationError> {
    let operator = read_hash28_cert(r)?;
    let vrf_keyhash = read_hash32(r)?;
    let pledge = read_lovelace(r)?;
    let cost = read_lovelace(r)?;
    let margin = r.read_rational()?;
    let reward_account = r.read_bytes()?.to_vec();
    let pool_owners: Vec<Hash28> = r.read_set(|r| read_hash28_cert(r))?;
    let relays_count = r.read_array_header()?.unwrap_or(0) as usize;
    for _ in 0..relays_count {
        r.skip()?;
    }
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
    let url_bytes = r.read_bytes()?;
    let url = String::from_utf8(url_bytes.to_vec())
        .map_err(|_| SerializationError::CborDecode("pool_metadata url: invalid UTF-8".into()))?;
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
        Type::Map => {
            // Use read_map to handle both definite- and indefinite-length maps.
            let pairs = r.read_map(|r| read_stake_credential(r), |r| Ok(r.read_int()? as i64))?;
            MIRTarget::StakeCredentials(pairs)
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
// Governance decoders
// ============================================================================

/// Read voting_procedures: `{ voter => { gov_action_id => voting_procedure } }`.
fn read_voting_procedures(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(
        |r| read_voter(r),
        |r| {
            let inner_pairs =
                r.read_map(|r| read_gov_action_id(r), |r| read_voting_procedure(r))?;
            Ok(inner_pairs.into_iter().collect::<BTreeMap<_, _>>())
        },
    )?;
    Ok(pairs.into_iter().collect())
}

/// Read a voter:
/// ```text
/// voter = [0, committee_cold_credential]  ; ConstitutionalCommitteeKey
///       / [1, committee_cold_credential]  ; ConstitutionalCommitteeScript
///       / [2, drep_credential]            ; DRepKey
///       / [3, drep_credential]            ; DRepScript
///       / [4, pool_keyhash]               ; StakePoolKey
/// ```
fn read_voter(r: &mut Reader<'_>) -> Result<Voter, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "voter: expected array(2), got {arr_len:?}"
        )));
    }
    let disc = r.read_uint()?;
    match disc {
        0 => {
            let h = read_hash28_cert(r)?;
            Ok(Voter::ConstitutionalCommittee(Credential::VerificationKey(
                h,
            )))
        }
        1 => {
            let h = read_hash28_cert(r)?;
            Ok(Voter::ConstitutionalCommittee(Credential::Script(h)))
        }
        2 => {
            let h = read_hash28_cert(r)?;
            Ok(Voter::DRep(Credential::VerificationKey(h)))
        }
        3 => {
            let h = read_hash28_cert(r)?;
            Ok(Voter::DRep(Credential::Script(h)))
        }
        4 => {
            // Pool key hash (28 bytes) padded to Hash32
            let h28 = read_hash28_cert(r)?;
            Ok(Voter::StakePool(h28.to_hash32_padded()))
        }
        other => Err(SerializationError::CborDecode(format!(
            "voter: unknown discriminator {other}"
        ))),
    }
}

fn read_gov_action_id(r: &mut Reader<'_>) -> Result<GovActionId, SerializationError> {
    // gov_action_id = [transaction_id, gov_action_index]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "gov_action_id: expected array(2), got {arr_len:?}"
        )));
    }
    let transaction_id = read_hash32(r)?;
    let action_index = r.read_uint()? as u32;
    Ok(GovActionId {
        transaction_id,
        action_index,
    })
}

fn read_voting_procedure(r: &mut Reader<'_>) -> Result<VotingProcedure, SerializationError> {
    // voting_procedure = [vote, anchor / null]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "voting_procedure: expected array(2), got {arr_len:?}"
        )));
    }
    let vote = read_vote(r)?;
    let anchor = read_optional_anchor(r)?;
    Ok(VotingProcedure { vote, anchor })
}

fn read_vote(r: &mut Reader<'_>) -> Result<Vote, SerializationError> {
    let v = r.read_uint()?;
    match v {
        0 => Ok(Vote::No),
        1 => Ok(Vote::Yes),
        2 => Ok(Vote::Abstain),
        other => Err(SerializationError::CborDecode(format!(
            "vote: unknown value {other}"
        ))),
    }
}

fn read_proposal_procedure(r: &mut Reader<'_>) -> Result<ProposalProcedure, SerializationError> {
    // proposal_procedure = [deposit, reward_account, gov_action, anchor]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "proposal_procedure: expected array(4), got {arr_len:?}"
        )));
    }
    let deposit = read_lovelace(r)?;
    let return_addr = r.read_bytes()?.to_vec();
    let gov_action = read_gov_action(r)?;
    let anchor = read_anchor(r)?;
    Ok(ProposalProcedure {
        deposit,
        return_addr,
        gov_action,
        anchor,
    })
}

/// Read a governance action.
///
/// ```text
/// gov_action =
///   [0, gov_action_id / null, protocol_param_update, policy_hash / null]   ; ParameterChange
/// / [1, gov_action_id / null, [major, minor]]                               ; HardForkInitiation
/// / [2, { reward_account => coin }, policy_hash / null]                     ; TreasuryWithdrawals
/// / [3, gov_action_id / null]                                               ; NoConfidence
/// / [4, gov_action_id / null, set<credential>, { credential => epoch }, unit_interval] ; UpdateCommittee
/// / [5, gov_action_id / null, constitution]                                 ; NewConstitution
/// / [6]                                                                     ; InfoAction
/// ```
fn read_gov_action(r: &mut Reader<'_>) -> Result<GovAction, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len.is_none() {
        return Err(SerializationError::CborDecode(
            "gov_action: expected definite-length array".into(),
        ));
    }
    let disc = r.read_uint()?;
    match disc {
        0 => {
            // ParameterChange
            let prev_action_id = read_optional_gov_action_id(r)?;
            let protocol_param_update = Box::new(read_protocol_param_update(r)?);
            let policy_hash = read_optional_hash28_gov(r)?;
            Ok(GovAction::ParameterChange {
                prev_action_id,
                protocol_param_update,
                policy_hash,
            })
        }
        1 => {
            // HardForkInitiation
            let prev_action_id = read_optional_gov_action_id(r)?;
            let ver_arr = r.read_array_header()?;
            if !matches!(ver_arr, Some(2)) {
                return Err(SerializationError::CborDecode(format!(
                    "HardForkInitiation: protocol version expected array(2), got {ver_arr:?}"
                )));
            }
            let major = r.read_uint()?;
            let minor = r.read_uint()?;
            Ok(GovAction::HardForkInitiation {
                prev_action_id,
                protocol_version: (major, minor),
            })
        }
        2 => {
            // TreasuryWithdrawals
            let pairs = r.read_map(|r| Ok(r.read_bytes()?.to_vec()), |r| read_lovelace(r))?;
            let withdrawals: BTreeMap<Vec<u8>, Lovelace> = pairs.into_iter().collect();
            let policy_hash = read_optional_hash28_gov(r)?;
            Ok(GovAction::TreasuryWithdrawals {
                withdrawals,
                policy_hash,
            })
        }
        3 => {
            // NoConfidence
            let prev_action_id = read_optional_gov_action_id(r)?;
            Ok(GovAction::NoConfidence { prev_action_id })
        }
        4 => {
            // UpdateCommittee
            let prev_action_id = read_optional_gov_action_id(r)?;
            // members_to_remove: set<credential>
            let remove_list: Vec<Credential> = r.read_set(|r| read_stake_credential(r))?;
            // members_to_add: { credential => epoch }
            let add_pairs = r.read_map(|r| read_stake_credential(r), |r| r.read_uint())?;
            let members_to_add: BTreeMap<Credential, u64> = add_pairs.into_iter().collect();
            // threshold: unit_interval (rational)
            let threshold_rat = r.read_rational()?;
            let threshold = Rational {
                numerator: threshold_rat.numerator,
                denominator: threshold_rat.denominator,
            };
            Ok(GovAction::UpdateCommittee {
                prev_action_id,
                members_to_remove: remove_list,
                members_to_add,
                threshold,
            })
        }
        5 => {
            // NewConstitution
            let prev_action_id = read_optional_gov_action_id(r)?;
            let constitution = read_constitution(r)?;
            Ok(GovAction::NewConstitution {
                prev_action_id,
                constitution,
            })
        }
        6 => Ok(GovAction::InfoAction),
        other => Err(SerializationError::CborDecode(format!(
            "gov_action: unknown discriminator {other}"
        ))),
    }
}

fn read_optional_gov_action_id(
    r: &mut Reader<'_>,
) -> Result<Option<GovActionId>, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    Ok(Some(read_gov_action_id(r)?))
}

fn read_optional_hash28_gov(r: &mut Reader<'_>) -> Result<Option<Hash28>, SerializationError> {
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    Ok(Some(read_hash28(r)?))
}

fn read_constitution(r: &mut Reader<'_>) -> Result<Constitution, SerializationError> {
    // constitution = [anchor, scripthash / null]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "constitution: expected array(2), got {arr_len:?}"
        )));
    }
    let anchor = read_anchor(r)?;
    let script_hash = read_optional_hash28_gov(r)?;
    Ok(Constitution {
        anchor,
        script_hash,
    })
}

/// Read a Conway protocol parameter update (map form, keys 0-31).
fn read_protocol_param_update(
    r: &mut Reader<'_>,
) -> Result<ProtocolParamUpdate, SerializationError> {
    let mut ppu = ProtocolParamUpdate::default();
    let map_len = r.read_map_header()?;
    let n = map_len.unwrap_or(0) as usize;
    for _ in 0..n {
        let key = r.read_uint()?;
        match key {
            0 => ppu.min_fee_a = Some(r.read_uint()?),
            1 => ppu.min_fee_b = Some(r.read_uint()?),
            2 => ppu.max_block_body_size = Some(r.read_uint()?),
            3 => ppu.max_tx_size = Some(r.read_uint()?),
            4 => ppu.max_block_header_size = Some(r.read_uint()?),
            5 => ppu.key_deposit = Some(read_lovelace(r)?),
            6 => ppu.pool_deposit = Some(read_lovelace(r)?),
            7 => ppu.e_max = Some(r.read_uint()?),
            8 => ppu.n_opt = Some(r.read_uint()?),
            9 => {
                let rat = r.read_rational()?;
                ppu.a0 = Some(Rational {
                    numerator: rat.numerator,
                    denominator: rat.denominator,
                });
            }
            10 => {
                let rat = r.read_rational()?;
                ppu.rho = Some(Rational {
                    numerator: rat.numerator,
                    denominator: rat.denominator,
                });
            }
            11 => {
                let rat = r.read_rational()?;
                ppu.tau = Some(Rational {
                    numerator: rat.numerator,
                    denominator: rat.denominator,
                });
            }
            16 => ppu.min_pool_cost = Some(read_lovelace(r)?),
            17 => ppu.ada_per_utxo_byte = Some(read_lovelace(r)?),
            18 => ppu.cost_models = Some(read_cost_models(r)?),
            19 => ppu.execution_costs = Some(read_ex_unit_prices(r)?),
            20 => ppu.max_tx_ex_units = Some(read_ex_units(r)?),
            21 => ppu.max_block_ex_units = Some(read_ex_units(r)?),
            22 => ppu.max_val_size = Some(r.read_uint()?),
            23 => ppu.collateral_percentage = Some(r.read_uint()?),
            24 => ppu.max_collateral_inputs = Some(r.read_uint()?),
            25 => {
                // pool_voting_thresholds
                ppu = read_pool_voting_thresholds(r, ppu)?;
            }
            26 => {
                // drep_voting_thresholds
                ppu = read_drep_voting_thresholds(r, ppu)?;
            }
            27 => ppu.min_committee_size = Some(r.read_uint()?),
            28 => ppu.committee_term_limit = Some(r.read_uint()?),
            29 => ppu.gov_action_lifetime = Some(r.read_uint()?),
            30 => ppu.gov_action_deposit = Some(read_lovelace(r)?),
            31 => ppu.drep_deposit = Some(read_lovelace(r)?),
            32 => ppu.drep_activity = Some(r.read_uint()?),
            33 => {
                // min_fee_ref_script_cost_per_byte: rational, store as numerator/denominator
                let rat = r.read_rational()?;
                ppu.min_fee_ref_script_cost_per_byte =
                    Some(rat.numerator.checked_div(rat.denominator).unwrap_or(15));
            }
            _ => {
                r.skip()?;
            }
        }
    }
    Ok(ppu)
}

fn read_cost_models(r: &mut Reader<'_>) -> Result<CostModels, SerializationError> {
    let mut plutus_v1 = None;
    let mut plutus_v2 = None;
    let mut plutus_v3 = None;
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(
        |r| r.read_uint(),
        |r| r.read_array(|r| Ok(r.read_int()? as i64)),
    )?;
    for (key, costs) in pairs {
        match key {
            0 => plutus_v1 = Some(costs),
            1 => plutus_v2 = Some(costs),
            2 => plutus_v3 = Some(costs),
            _ => {}
        }
    }
    Ok(CostModels {
        plutus_v1,
        plutus_v2,
        plutus_v3,
    })
}

fn read_ex_unit_prices(r: &mut Reader<'_>) -> Result<ExUnitPrices, SerializationError> {
    // ex_unit_prices = [mem_price, step_price] — both are rationals
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "ex_unit_prices: expected array(2), got {arr_len:?}"
        )));
    }
    let mem = r.read_rational()?;
    let step = r.read_rational()?;
    Ok(ExUnitPrices {
        mem_price: Rational {
            numerator: mem.numerator,
            denominator: mem.denominator,
        },
        step_price: Rational {
            numerator: step.numerator,
            denominator: step.denominator,
        },
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

fn read_pool_voting_thresholds(
    r: &mut Reader<'_>,
    mut ppu: ProtocolParamUpdate,
) -> Result<ProtocolParamUpdate, SerializationError> {
    // pool_voting_thresholds = [5 rationals]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(5)) {
        return Err(SerializationError::CborDecode(format!(
            "pool_voting_thresholds: expected array(5), got {arr_len:?}"
        )));
    }
    let motion_no_confidence = r.read_rational()?;
    let committee_normal = r.read_rational()?;
    let committee_no_confidence = r.read_rational()?;
    let hard_fork = r.read_rational()?;
    let security = r.read_rational()?;
    let cvt = |rat: Rational| Rational {
        numerator: rat.numerator,
        denominator: rat.denominator,
    };
    ppu.pvt_motion_no_confidence = Some(cvt(motion_no_confidence));
    ppu.pvt_committee_normal = Some(cvt(committee_normal));
    ppu.pvt_committee_no_confidence = Some(cvt(committee_no_confidence));
    ppu.pvt_hard_fork = Some(cvt(hard_fork));
    ppu.pvt_pp_security_group = Some(cvt(security));
    Ok(ppu)
}

fn read_drep_voting_thresholds(
    r: &mut Reader<'_>,
    mut ppu: ProtocolParamUpdate,
) -> Result<ProtocolParamUpdate, SerializationError> {
    // drep_voting_thresholds = [10 rationals]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(10)) {
        return Err(SerializationError::CborDecode(format!(
            "drep_voting_thresholds: expected array(10), got {arr_len:?}"
        )));
    }
    let motion_no_confidence = r.read_rational()?;
    let committee_normal = r.read_rational()?;
    let committee_no_confidence = r.read_rational()?;
    let update_constitution = r.read_rational()?;
    let hard_fork = r.read_rational()?;
    let pp_network = r.read_rational()?;
    let pp_economic = r.read_rational()?;
    let pp_technical = r.read_rational()?;
    let pp_governance = r.read_rational()?;
    let treasury = r.read_rational()?;
    let cvt = |rat: Rational| Rational {
        numerator: rat.numerator,
        denominator: rat.denominator,
    };
    ppu.dvt_no_confidence = Some(cvt(motion_no_confidence));
    ppu.dvt_committee_normal = Some(cvt(committee_normal));
    ppu.dvt_committee_no_confidence = Some(cvt(committee_no_confidence));
    ppu.dvt_constitution = Some(cvt(update_constitution));
    ppu.dvt_hard_fork = Some(cvt(hard_fork));
    ppu.dvt_pp_network_group = Some(cvt(pp_network));
    ppu.dvt_pp_economic_group = Some(cvt(pp_economic));
    ppu.dvt_pp_technical_group = Some(cvt(pp_technical));
    ppu.dvt_pp_gov_group = Some(cvt(pp_governance));
    ppu.dvt_treasury_withdrawal = Some(cvt(treasury));
    Ok(ppu)
}

// ============================================================================
// Witness set decoder (Conway)
// ============================================================================

/// Decode a Conway witness set.
///
/// Conway adds:
/// - Key 5: redeemers in MAP form `{ [tag, index] => [data, ex_units] }` OR array form
/// - Key 7: plutus_v3_scripts
fn decode_conway_witness_set(
    r: &mut Reader<'_>,
) -> Result<TransactionWitnessSet, SerializationError> {
    let mut vkey_witnesses: Vec<VKeyWitness> = Vec::new();
    let mut native_scripts = Vec::new();
    let mut bootstrap_witnesses = Vec::new();
    let mut plutus_v1_scripts: Vec<Vec<u8>> = Vec::new();
    let mut plutus_v2_scripts: Vec<Vec<u8>> = Vec::new();
    let mut plutus_v3_scripts: Vec<Vec<u8>> = Vec::new();
    let mut plutus_data: Vec<PlutusData> = Vec::new();
    let mut redeemers: Vec<Redeemer> = Vec::new();
    let mut raw_redeemers_cbor: Option<Vec<u8>> = None;
    let mut raw_plutus_data_cbor: Option<Vec<u8>> = None;

    let map_len = r.read_map_header()?;
    let n_entries = map_len.unwrap_or(0) as usize;

    for _ in 0..n_entries {
        let key = r.read_uint()?;
        match key {
            0 => {
                // vkey_witnesses: nonempty_set<vkeywitness> — tag(258) in Conway
                vkey_witnesses = r.read_set(|r| {
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
                // native_scripts: nonempty_set<native_script> — tag(258) in Conway
                native_scripts = r.read_set(|r| read_native_script(r))?;
            }
            2 => {
                // bootstrap_witnesses: nonempty_set<bootstrap_witness> — tag(258) in Conway
                bootstrap_witnesses = r.read_set(|r| {
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
                // plutus_v1_scripts: nonempty_set<plutus_v1_script> — tag(258) in Conway
                plutus_v1_scripts = r.read_set(|r| Ok(r.read_bytes()?.to_vec()))?;
            }
            4 => {
                // plutus_data: [* plutus_data] — plain list, NOT a set
                let pd_start = r.position();
                plutus_data = r.read_array(|r| read_plutus_data(r))?;
                raw_plutus_data_cbor = Some(r.slice_from(pd_start).to_vec());
            }
            5 => {
                // redeemers: map form (Conway) or array form (pre-Conway) — NOT a set
                let rd_start = r.position();
                redeemers = read_redeemers(r)?;
                raw_redeemers_cbor = Some(r.slice_from(rd_start).to_vec());
            }
            6 => {
                // plutus_v2_scripts: nonempty_set<plutus_v2_script> — tag(258) in Conway
                plutus_v2_scripts = r.read_set(|r| Ok(r.read_bytes()?.to_vec()))?;
            }
            7 => {
                // plutus_v3_scripts: nonempty_set<plutus_v3_script> — tag(258) in Conway
                plutus_v3_scripts = r.read_set(|r| Ok(r.read_bytes()?.to_vec()))?;
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
        plutus_v3_scripts,
        plutus_data,
        redeemers,
        raw_redeemers_cbor,
        raw_plutus_data_cbor,
        original_script_data_hash: None,
    })
}

/// Read redeemers — supports both Conway map form and pre-Conway array form.
fn read_redeemers(r: &mut Reader<'_>) -> Result<Vec<Redeemer>, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Map => {
            // Conway map form: { [tag, index] => [data, ex_units] }
            // Use read_map to handle both definite- and indefinite-length maps.
            let pairs = r.read_map(
                |r| {
                    let key_arr = r.read_array_header()?;
                    if !matches!(key_arr, Some(2)) {
                        return Err(SerializationError::CborDecode(format!(
                            "redeemer key: expected array(2), got {key_arr:?}"
                        )));
                    }
                    let tag = read_redeemer_tag(r)?;
                    let index = r.read_uint()? as u32;
                    Ok((tag, index))
                },
                |r| {
                    let val_arr = r.read_array_header()?;
                    if !matches!(val_arr, Some(2)) {
                        return Err(SerializationError::CborDecode(format!(
                            "redeemer value: expected array(2), got {val_arr:?}"
                        )));
                    }
                    let data = read_plutus_data(r)?;
                    let ex_units = read_ex_units(r)?;
                    Ok((data, ex_units))
                },
            )?;
            let out = pairs
                .into_iter()
                .map(|((tag, index), (data, ex_units))| Redeemer {
                    tag,
                    index,
                    data,
                    ex_units,
                })
                .collect();
            Ok(out)
        }
        Type::Array => {
            // Pre-Conway array form: [* [tag, index, data, ex_units]]
            r.read_array(|r| {
                let arr_len = r.read_array_header()?;
                if !matches!(arr_len, Some(4)) {
                    return Err(SerializationError::CborDecode(format!(
                        "redeemer: expected array(4), got {arr_len:?}"
                    )));
                }
                let tag = read_redeemer_tag(r)?;
                let index = r.read_uint()? as u32;
                let data = read_plutus_data(r)?;
                let ex_units = read_ex_units(r)?;
                Ok(Redeemer {
                    tag,
                    index,
                    data,
                    ex_units,
                })
            })
        }
        other => Err(SerializationError::CborDecode(format!(
            "redeemers: expected map or array, got {other}"
        ))),
    }
}

fn read_redeemer_tag(r: &mut Reader<'_>) -> Result<RedeemerTag, SerializationError> {
    let v = r.read_uint()?;
    match v {
        0 => Ok(RedeemerTag::Spend),
        1 => Ok(RedeemerTag::Mint),
        2 => Ok(RedeemerTag::Cert),
        3 => Ok(RedeemerTag::Reward),
        4 => Ok(RedeemerTag::Vote),
        5 => Ok(RedeemerTag::Propose),
        other => Err(SerializationError::CborDecode(format!(
            "redeemer_tag: unknown value {other}"
        ))),
    }
}

// ============================================================================
// Native script decoder
// ============================================================================

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

// ============================================================================
// Plutus data decoder
// ============================================================================

/// Read a PlutusData value.
///
/// PlutusData can be:
/// - Constructor: `#6.121(items)` or `#6.1280(items)` etc.
/// - Map: `{ data => data, ... }`
/// - List: `[* data]`
/// - Integer: uint or negative int or tag(2/3) bignum
/// - Bytes: bytestring (possibly indefinite-length)
pub(crate) fn read_plutus_data(r: &mut Reader<'_>) -> Result<PlutusData, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Tag => {
            // Constructor: tag(121..=127 or 1280..=1400) + array, or tag(2/3) bignum
            // Peek at the tag value without consuming it, then dispatch.
            let tag_val = r.probe_tag()?;
            match tag_val {
                2 | 3 => {
                    // bignum — delegate to read_bigint which consumes the tag
                    let big = r.read_bigint()?;
                    Ok(PlutusData::Integer(big))
                }
                121..=127 => {
                    // Alternative 0..=6: tag(121+n) [* plutus_data]
                    r.read_tag()?; // consume tag
                    let fields = r.read_array(read_plutus_data)?;
                    Ok(PlutusData::Constr(tag_val - 121, fields))
                }
                1280..=1400 => {
                    // Alternative 7+: tag(1280+n) [* plutus_data]
                    r.read_tag()?; // consume tag
                    let fields = r.read_array(read_plutus_data)?;
                    Ok(PlutusData::Constr(tag_val - 1280 + 7, fields))
                }
                102 => {
                    // Explicit alternative: tag(102) [alternative_int, [* plutus_data]]
                    r.read_tag()?; // consume tag
                    let inner_arr = r.read_array_header()?;
                    if !matches!(inner_arr, Some(2)) {
                        return Err(SerializationError::CborDecode(format!(
                            "plutus_data Constr102: expected array(2), got {inner_arr:?}"
                        )));
                    }
                    let alt = r.read_uint()?;
                    let fields = r.read_array(read_plutus_data)?;
                    Ok(PlutusData::Constr(alt, fields))
                }
                _ => {
                    // Unknown tag — skip and return error
                    r.skip()?;
                    Err(SerializationError::CborDecode(format!(
                        "plutus_data: unknown tag {tag_val}"
                    )))
                }
            }
        }
        Type::Map => {
            let entries = r.read_map(read_plutus_data, read_plutus_data)?;
            Ok(PlutusData::Map(entries))
        }
        Type::Array => {
            let items = r.read_array(read_plutus_data)?;
            Ok(PlutusData::List(items))
        }
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::Int => {
            let big = r.read_bigint()?;
            Ok(PlutusData::Integer(big))
        }
        Type::Bytes | Type::BytesIndef => {
            let bytes = r.read_bytes_owned()?;
            Ok(PlutusData::Bytes(bytes))
        }
        other => Err(SerializationError::CborDecode(format!(
            "plutus_data: unexpected type {other}"
        ))),
    }
}

// ============================================================================
// Auxiliary data decoder
// ============================================================================

fn decode_aux_data_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u32, AuxiliaryData>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(|r| Ok(r.read_uint()? as u32), |r| decode_auxiliary_data(r))?;
    Ok(pairs.into_iter().collect())
}

fn decode_auxiliary_data(r: &mut Reader<'_>) -> Result<AuxiliaryData, SerializationError> {
    let raw_start = r.position();
    r.skip()?;
    let raw_bytes = r.slice_from(raw_start).to_vec();

    let mut aux_r = Reader::new(&raw_bytes);
    let ty = aux_r.peek_major()?;

    let (metadata, native_scripts, plutus_v1_scripts, plutus_v2_scripts, plutus_v3_scripts) =
        match ty {
            Type::Map => {
                let meta = decode_metadata_map(&mut aux_r)?;
                (meta, Vec::new(), Vec::new(), Vec::new(), Vec::new())
            }
            Type::Array => {
                let arr_len = aux_r.read_array_header()?;
                if !matches!(arr_len, Some(2)) {
                    (
                        BTreeMap::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )
                } else {
                    let meta = decode_metadata_map(&mut aux_r)?;
                    aux_r.skip()?;
                    (meta, Vec::new(), Vec::new(), Vec::new(), Vec::new())
                }
            }
            Type::Tag => {
                // Alonzo+ PostAlonzoAuxiliaryData: tag(259) { ... }
                // or tag(11311) in some representations; skip the tag
                let _tag = aux_r.read_tag()?;
                // Now read map
                let mut meta = BTreeMap::new();
                let mut ns = Vec::new();
                let mut v1 = Vec::new();
                let mut v2 = Vec::new();
                let mut v3 = Vec::new();
                if let Ok(Some(n)) = aux_r.read_map_header() {
                    for _ in 0..n {
                        if let Ok(key) = aux_r.read_uint() {
                            match key {
                                0 => {
                                    if let Ok(m) = decode_metadata_map(&mut aux_r) {
                                        meta = m;
                                    } else {
                                        let _ = aux_r.skip();
                                    }
                                }
                                1 => {
                                    let _ = aux_r.read_array(|r| {
                                        let s = read_native_script(r)?;
                                        ns.push(s);
                                        Ok(())
                                    });
                                }
                                2 => {
                                    let _ = aux_r.read_array(|r| {
                                        v1.push(r.read_bytes()?.to_vec());
                                        Ok(())
                                    });
                                }
                                3 => {
                                    let _ = aux_r.read_array(|r| {
                                        v2.push(r.read_bytes()?.to_vec());
                                        Ok(())
                                    });
                                }
                                4 => {
                                    let _ = aux_r.read_array(|r| {
                                        v3.push(r.read_bytes()?.to_vec());
                                        Ok(())
                                    });
                                }
                                _ => {
                                    let _ = aux_r.skip();
                                }
                            }
                        }
                    }
                }
                (meta, ns, v1, v2, v3)
            }
            _ => (
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
        };

    Ok(AuxiliaryData {
        metadata,
        native_scripts,
        plutus_v1_scripts,
        plutus_v2_scripts,
        plutus_v3_scripts,
        raw_cbor: Some(raw_bytes),
    })
}

fn decode_metadata_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u64, TransactionMetadatum>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(|r| r.read_uint(), read_metadatum)?;
    Ok(pairs.into_iter().collect())
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
        _ => Err(SerializationError::CborDecode(
            "metadatum: unexpected type".into(),
        )),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── CBOR helpers ──────────────────────────────────────────────────────────

    fn cbor_uint(n: u64) -> Vec<u8> {
        if n <= 23 {
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
    fn cbor_map0() -> Vec<u8> {
        vec![0xa0]
    }

    fn cbor_null() -> Vec<u8> {
        vec![0xf6]
    }

    fn cbor_tag(n: u64) -> Vec<u8> {
        if n <= 23 {
            vec![0xc0 | n as u8]
        } else if n <= 0xff {
            vec![0xd8, n as u8]
        } else if n <= 0xffff {
            let b = (n as u16).to_be_bytes();
            vec![0xd9, b[0], b[1]]
        } else {
            let b = (n as u32).to_be_bytes();
            vec![0xda, b[0], b[1], b[2], b[3]]
        }
    }

    // tag 258 header: [0xd9, 0x01, 0x02]
    fn tag_258() -> Vec<u8> {
        vec![0xd9, 0x01, 0x02]
    }

    #[allow(dead_code)]
    fn cbor_rational(num: u64, den: u64) -> Vec<u8> {
        // tag(30) [num, den]
        let mut v = cbor_tag(30);
        v.extend(cbor_arr(&[&cbor_uint(num), &cbor_uint(den)]));
        v
    }

    // ── Tag 258 set encoding ──────────────────────────────────────────────────

    #[test]
    fn test_tag_258_set_inputs_decoded() {
        // Build set<transaction_input> with tag(258)
        let input1 = cbor_arr(&[&cbor_bytes(&[0xaa; 32]), &cbor_uint(0)]);
        let input2 = cbor_arr(&[&cbor_bytes(&[0xbb; 32]), &cbor_uint(1)]);

        let mut data = tag_258();
        data.extend(vec![0x82]); // array(2)
        data.extend(&input1);
        data.extend(&input2);

        let mut r = Reader::new(&data);
        let inputs = r.read_set(read_tx_input).unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].index, 0);
        assert_eq!(inputs[1].index, 1);
    }

    #[test]
    fn test_untagged_set_inputs_decoded() {
        // Without tag(258) — plain array
        let input = cbor_arr(&[&cbor_bytes(&[0xcc; 32]), &cbor_uint(5)]);
        let mut data = vec![0x81]; // array(1)
        data.extend(&input);

        let mut r = Reader::new(&data);
        let inputs = r.read_set(read_tx_input).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].index, 5);
    }

    // ── Vote ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_read_vote_no() {
        let data = cbor_uint(0);
        let mut r = Reader::new(&data);
        assert_eq!(read_vote(&mut r).unwrap(), Vote::No);
    }

    #[test]
    fn test_read_vote_yes() {
        let data = cbor_uint(1);
        let mut r = Reader::new(&data);
        assert_eq!(read_vote(&mut r).unwrap(), Vote::Yes);
    }

    #[test]
    fn test_read_vote_abstain() {
        let data = cbor_uint(2);
        let mut r = Reader::new(&data);
        assert_eq!(read_vote(&mut r).unwrap(), Vote::Abstain);
    }

    #[test]
    fn test_read_vote_unknown_rejected() {
        let data = cbor_uint(3);
        let mut r = Reader::new(&data);
        assert!(read_vote(&mut r).is_err());
    }

    // ── GovActionId ──────────────────────────────────────────────────────────

    #[test]
    fn test_read_gov_action_id() {
        let tx_hash = [0xde; 32];
        let mut data = vec![0x82]; // array(2)
        data.extend(cbor_bytes(&tx_hash));
        data.extend(cbor_uint(3));

        let mut r = Reader::new(&data);
        let id = read_gov_action_id(&mut r).unwrap();
        assert_eq!(id.action_index, 3);
        assert_eq!(id.transaction_id.as_bytes(), &tx_hash);
    }

    // ── Voter ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_read_voter_drep_key() {
        // [2, keyhash(28)]
        let mut data = vec![0x82];
        data.extend(cbor_uint(2));
        data.extend(cbor_bytes(&[0xab; 28]));

        let mut r = Reader::new(&data);
        let voter = read_voter(&mut r).unwrap();
        assert!(matches!(voter, Voter::DRep(Credential::VerificationKey(_))));
    }

    #[test]
    fn test_read_voter_stake_pool() {
        // [4, pool_keyhash(28)]
        let mut data = vec![0x82];
        data.extend(cbor_uint(4));
        data.extend(cbor_bytes(&[0x01; 28]));

        let mut r = Reader::new(&data);
        let voter = read_voter(&mut r).unwrap();
        assert!(matches!(voter, Voter::StakePool(_)));
    }

    // ── Anchor ────────────────────────────────────────────────────────────────

    #[test]
    fn test_read_anchor() {
        let url = b"https://example.com";
        let hash = [0xfe; 32];
        let mut data = vec![0x82];
        data.extend(cbor_bytes(url));
        data.extend(cbor_bytes(&hash));

        let mut r = Reader::new(&data);
        let anchor = read_anchor(&mut r).unwrap();
        assert_eq!(anchor.url, "https://example.com");
        assert_eq!(anchor.data_hash.as_bytes(), &hash);
    }

    #[test]
    fn test_read_optional_anchor_null() {
        let data = cbor_null();
        let mut r = Reader::new(&data);
        let anchor = read_optional_anchor(&mut r).unwrap();
        assert!(anchor.is_none());
    }

    // ── DRep ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_read_drep_key_hash() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&[0x11; 28]));
        let mut r = Reader::new(&data);
        let drep = read_drep(&mut r).unwrap();
        assert!(matches!(drep, DRep::KeyHash(_)));
    }

    #[test]
    fn test_read_drep_abstain() {
        let mut data = vec![0x81]; // array(1)
        data.extend(cbor_uint(2));
        let mut r = Reader::new(&data);
        let drep = read_drep(&mut r).unwrap();
        assert_eq!(drep, DRep::Abstain);
    }

    #[test]
    fn test_read_drep_no_confidence() {
        let mut data = vec![0x81];
        data.extend(cbor_uint(3));
        let mut r = Reader::new(&data);
        let drep = read_drep(&mut r).unwrap();
        assert_eq!(drep, DRep::NoConfidence);
    }

    // ── Map-form redeemers ────────────────────────────────────────────────────

    #[test]
    fn test_read_redeemers_map_form() {
        // { [0, 0] => [#6.121([]), [100, 200]] }
        // Spend redeemer, index 0, empty constr data, ex_units mem=100 steps=200
        let constr_data = {
            let mut v = cbor_tag(121);
            v.extend(vec![0x80]); // empty array
            v
        };
        let ex_units = cbor_arr(&[&cbor_uint(100), &cbor_uint(200)]);
        let value_arr = cbor_arr(&[&constr_data, &ex_units]);
        let key_arr = cbor_arr(&[&cbor_uint(0), &cbor_uint(0)]); // [spend=0, index=0]

        // map(1) { key_arr => value_arr }
        let mut data = vec![0xa1];
        data.extend(&key_arr);
        data.extend(&value_arr);

        let mut r = Reader::new(&data);
        let redeemers = read_redeemers(&mut r).unwrap();
        assert_eq!(redeemers.len(), 1);
        assert_eq!(redeemers[0].tag, RedeemerTag::Spend);
        assert_eq!(redeemers[0].index, 0);
        assert_eq!(redeemers[0].ex_units.mem, 100);
        assert_eq!(redeemers[0].ex_units.steps, 200);
    }

    #[test]
    fn test_read_redeemers_array_form() {
        // [[0, 0, #6.121([]), [50, 100]]] — Alonzo/Babbage form
        let constr_data = {
            let mut v = cbor_tag(121);
            v.extend(vec![0x80]);
            v
        };
        let ex_units = cbor_arr(&[&cbor_uint(50), &cbor_uint(100)]);
        let redeemer = cbor_arr(&[&cbor_uint(0), &cbor_uint(0), &constr_data, &ex_units]);

        let mut data = vec![0x81]; // array(1)
        data.extend(&redeemer);

        let mut r = Reader::new(&data);
        let redeemers = read_redeemers(&mut r).unwrap();
        assert_eq!(redeemers.len(), 1);
        assert_eq!(redeemers[0].tag, RedeemerTag::Spend);
        assert_eq!(redeemers[0].ex_units.mem, 50);
    }

    // ── Plutus V3 script ──────────────────────────────────────────────────────

    #[test]
    fn test_witness_set_plutus_v3_script() {
        // map(1) { 7: [[script_bytes]] }
        let script_bytes = cbor_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let scripts_arr = {
            let mut v = vec![0x81]; // array(1)
            v.extend(&script_bytes);
            v
        };
        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(7)); // key 7 = plutus_v3_scripts
        data.extend(&scripts_arr);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r).unwrap();
        assert_eq!(ws.plutus_v3_scripts.len(), 1);
        assert_eq!(ws.plutus_v3_scripts[0], vec![0xde, 0xad, 0xbe, 0xef]);
    }

    // ── treasury_value + donation ─────────────────────────────────────────────

    #[test]
    fn test_tx_body_treasury_value_and_donation() {
        // Minimal tx body map with keys 2 (fee), 21 (treasury), 22 (donation)
        // Build: map(3){ 2: fee, 21: treasury, 22: donation }
        // key 2 = fee (coin), key 21 = current_treasury_value, key 22 = donation
        let mut data = vec![0xa3]; // map(3)
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(1000)); // fee
        data.extend(cbor_uint(21));
        data.extend(cbor_uint(5_000_000)); // treasury_value
        data.extend(cbor_uint(22));
        data.extend(cbor_uint(1_000_000)); // donation

        let mut r = Reader::new(&data);
        let body = decode_conway_tx_body(&mut r).unwrap();
        assert_eq!(body.fee, Lovelace(1000));
        assert_eq!(body.treasury_value, Some(Lovelace(5_000_000)));
        assert_eq!(body.donation, Some(Lovelace(1_000_000)));
    }

    // ── Voting procedure round-trip ───────────────────────────────────────────

    #[test]
    fn test_voting_procedure_with_anchor() {
        // voting_procedure = [vote=1, [url_bytes, hash(32)]]
        let url = b"https://example.com/vote.json";
        let hash = [0xcc; 32];
        let anchor = cbor_arr(&[&cbor_bytes(url), &cbor_bytes(&hash)]);
        let data = cbor_arr(&[&cbor_uint(1), &anchor]); // vote=Yes

        let mut r = Reader::new(&data);
        let vp = read_voting_procedure(&mut r).unwrap();
        assert_eq!(vp.vote, Vote::Yes);
        assert!(vp.anchor.is_some());
        assert_eq!(vp.anchor.unwrap().url, "https://example.com/vote.json");
    }

    #[test]
    fn test_voting_procedure_null_anchor() {
        let data = cbor_arr(&[&cbor_uint(0), &cbor_null()]); // vote=No, no anchor
        let mut r = Reader::new(&data);
        let vp = read_voting_procedure(&mut r).unwrap();
        assert_eq!(vp.vote, Vote::No);
        assert!(vp.anchor.is_none());
    }

    // ── Proposal procedure round-trip ─────────────────────────────────────────

    #[test]
    fn test_proposal_procedure_info_action() {
        // [deposit, reward_account(29 bytes), [6], [url, hash]]
        let deposit = cbor_uint(500_000_000);
        let reward_acct = cbor_bytes(&[0x01u8; 29]);
        let info_action = cbor_arr(&[&cbor_uint(6)]); // InfoAction
        let url = b"https://info.example.com";
        let hash = [0xab; 32];
        let anchor = cbor_arr(&[&cbor_bytes(url), &cbor_bytes(&hash)]);

        let data = cbor_arr(&[&deposit, &reward_acct, &info_action, &anchor]);
        let mut r = Reader::new(&data);
        let pp = read_proposal_procedure(&mut r).unwrap();
        assert_eq!(pp.deposit, Lovelace(500_000_000));
        assert!(matches!(pp.gov_action, GovAction::InfoAction));
        assert_eq!(pp.anchor.url, "https://info.example.com");
    }

    // ── Conway certificate types ──────────────────────────────────────────────

    #[test]
    fn test_conway_stake_reg_cert() {
        // [7, [0, keyhash(28)], deposit]
        let mut data = vec![0x83]; // array(3)
        data.extend(cbor_uint(7));
        // stake_credential: [0, 28-byte hash]
        let mut cred = vec![0x82];
        cred.extend(cbor_uint(0));
        cred.extend(cbor_bytes(&[0x11; 28]));
        data.extend(&cred);
        data.extend(cbor_uint(2_000_000)); // deposit

        let mut r = Reader::new(&data);
        let cert = read_conway_certificate(&mut r).unwrap();
        assert!(matches!(cert, Certificate::ConwayStakeRegistration { .. }));
    }

    #[test]
    fn test_conway_vote_deleg_cert() {
        // [9, credential, drep]
        // drep = [2] (Abstain)
        let mut data = vec![0x83]; // array(3)
        data.extend(cbor_uint(9));
        let mut cred = vec![0x82];
        cred.extend(cbor_uint(0));
        cred.extend(cbor_bytes(&[0x22; 28]));
        data.extend(&cred);
        // drep Abstain = array(1)[2]
        data.extend(vec![0x81]);
        data.extend(cbor_uint(2));

        let mut r = Reader::new(&data);
        let cert = read_conway_certificate(&mut r).unwrap();
        assert!(matches!(
            cert,
            Certificate::VoteDelegation {
                drep: DRep::Abstain,
                ..
            }
        ));
    }

    // ── Conway protocol_param_update ──────────────────────────────────────────

    #[test]
    fn test_read_protocol_param_update_partial() {
        // map(2) { 0: 44, 1: 155381 }
        let mut data = vec![0xa2];
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(44));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(155381));

        let mut r = Reader::new(&data);
        let ppu = read_protocol_param_update(&mut r).unwrap();
        assert_eq!(ppu.min_fee_a, Some(44));
        assert_eq!(ppu.min_fee_b, Some(155381));
        assert_eq!(ppu.min_pool_cost, None);
    }

    // ── Operational cert ──────────────────────────────────────────────────────

    #[test]
    fn test_read_operational_cert() {
        let hot_vkey = cbor_bytes(&[0x01; 32]);
        let seq = cbor_uint(5);
        let kes_period = cbor_uint(42);
        let sigma = cbor_bytes(&[0x02; 64]);
        let data = cbor_arr(&[&hot_vkey, &seq, &kes_period, &sigma]);

        let mut r = Reader::new(&data);
        let cert = read_operational_cert(&mut r).unwrap();
        assert_eq!(cert.sequence_number, 5);
        assert_eq!(cert.kes_period, 42);
        assert_eq!(cert.hot_vkey.len(), 32);
        assert_eq!(cert.sigma.len(), 64);
    }

    // ── Dijkstra unknown key skipping ─────────────────────────────────────────

    #[test]
    fn test_dijkstra_unknown_tx_body_key_skipped() {
        // map(2) { 2: fee, 99: some_unknown_value }
        let mut data = vec![0xa2];
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(42)); // fee
        data.extend(cbor_uint(99)); // unknown key
        data.extend(cbor_uint(0)); // unknown value (just a uint)

        let mut r = Reader::new(&data);
        let body = decode_conway_tx_body(&mut r).unwrap();
        assert_eq!(body.fee, Lovelace(42));
    }
}
