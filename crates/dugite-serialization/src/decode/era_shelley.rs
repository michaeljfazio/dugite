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

use crate::decode::era_conway::{read_cost_models, read_ex_unit_prices, read_ex_units};
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
    AuxiliaryData, BootstrapWitness, Certificate, MIRSource, MIRTarget, NativeScript, OutputDatum,
    PoolMetadata, PoolParams, ProtocolParamUpdate, Rational, Relay, Transaction, TransactionBody,
    TransactionInput, TransactionOutput, TransactionWitnessSet, UpdateProposal, VKeyWitness,
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

/// Decode JUST the block header from the inner header CBOR (issue #654 —
/// eager per-peer header validation in the ChainSync receive loop). See
/// `era_conway::decode_conway_block_header` for the contract.
pub fn decode_shelley_block_header(inner_cbor: &[u8]) -> Result<BlockHeader, SerializationError> {
    let mut r = Reader::new(inner_cbor);
    let raw = KeepRaw::parse_with(&mut r, decode_shelley_header_inner)?;
    let header_hash = blake2b_256(raw.raw);
    let mut h = raw.value;
    h.header_hash = header_hash;
    Ok(h)
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
    // 2. tx_bodies — array of KeepRaw<TransactionBody>, DEFINITE OR INDEFINITE.
    //
    // Mainnet Shelley blocks encode tx_bodies as an indefinite-length array
    // (`9f … ff`) in some blocks (e.g. epoch 223, block 4_813_942). The previous
    // `read_array_header().unwrap_or(0)` treated indefinite (None) as zero txs,
    // skipped every body, and then misread the first tx body (a map) as the
    // witness array — "expected array, got map". Read until the break byte for
    // the indefinite case.
    let tx_count_hdr = r.read_array_header()?;
    let alloc_cap = r.safe_alloc_capacity(tx_count_hdr.unwrap_or(0));
    let mut raw_bodies: Vec<Vec<u8>> = Vec::with_capacity(alloc_cap);
    let mut parsed_bodies: Vec<TransactionBody> = Vec::with_capacity(alloc_cap);

    let mut i = 0u64;
    loop {
        match tx_count_hdr {
            Some(n) if i >= n => break,
            None if r.peek_major()? == Type::Break => {
                r.skip()?; // consume the indefinite-array break byte
                break;
            }
            _ => {}
        }
        let body = KeepRaw::parse_with(&mut r, |r| decode_shelley_tx_body(r))?;
        raw_bodies.push(body.raw.to_vec());
        parsed_bodies.push(body.value);
        i += 1;
    }

    // -------------------------------------------------------------------------
    // 3. tx_witness_sets — array of witness sets, DEFINITE OR INDEFINITE.
    // -------------------------------------------------------------------------
    let witness_count_hdr = r.read_array_header()?;
    let ws_alloc_cap = r.safe_alloc_capacity(witness_count_hdr.unwrap_or(0));
    let mut raw_witnesses: Vec<Vec<u8>> = Vec::with_capacity(ws_alloc_cap);
    let mut parsed_witnesses: Vec<Option<TransactionWitnessSet>> = Vec::with_capacity(ws_alloc_cap);

    let mut j = 0u64;
    loop {
        match witness_count_hdr {
            Some(n) if j >= n => break,
            None if r.peek_major()? == Type::Break => {
                r.skip()?;
                break;
            }
            _ => {}
        }
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
        j += 1;
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

            // Reconstruct full wire-format tx CBOR (pre-Alonzo: 3-element array).
            // Haskell fee size = full wire size of [body, wits, aux] (no subtraction).
            let raw_cbor = Some(reconstruct_pre_alonzo_tx_raw_cbor(
                &raw_body,
                raw_witness.as_deref().unwrap_or(&[0xA0]),
                auxiliary_data.as_ref(),
            ));

            Ok(Transaction {
                hash: tx_hash,
                era: Era::Shelley,
                body,
                witness_set,
                is_valid: true,
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

    // header_body = array(15) — capture its raw bytes; this is exactly the
    // message the KES signature signs (Haskell `serialize'(pvMajor, BHBody)`,
    // byte-identical to the on-wire body for canonically-encoded blocks).
    let body_start = r.position();
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
    let issuer_vkey = r.read_bytes_owned()?;
    // 4: vrf_vkey (32 bytes)
    let vrf_vkey = r.read_bytes_owned()?;
    // 5: nonce_vrf_cert = [output_bytes(64), proof_bytes(80)]
    let (nonce_output, nonce_proof) = read_vrf_cert(r)?;
    // 6: leader_vrf_cert = [output_bytes(64), proof_bytes(80)]
    let (leader_output, leader_proof) = read_vrf_cert(r)?;
    // 7: block_body_size
    let body_size = r.read_uint()?;
    // 8: block_body_hash (32 bytes)
    let body_hash = read_hash32(r)?;
    // 9: op_cert_hot_vkey (32 bytes)
    let op_hot_vkey = r.read_bytes_owned()?;
    // 10: op_cert_sequence_number
    let op_seq_number = r.read_uint()?;
    // 11: op_cert_kes_period
    let op_kes_period = r.read_uint()?;
    // 12: op_cert_sigma (64 bytes)
    let op_sigma = r.read_bytes_owned()?;
    // 13: protocol_major
    let protocol_major = r.read_uint()?;
    // 14: protocol_minor
    let protocol_minor = r.read_uint()?;

    // The header body spans from `body_start` to here (before the KES signature).
    let raw_header_body = r.slice_from(body_start).to_vec();

    // KES signature (second element of outer array)
    let kes_signature = r.read_bytes_owned()?;

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
        raw_header_body: Some(raw_header_body),
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
    let output = r.read_bytes_owned()?;
    let proof = r.read_bytes_owned()?;
    Ok((output, proof))
}

// ============================================================================
// Transaction body decoder
// ============================================================================

/// Decode a Shelley transaction body from a map.
///
/// The Shelley tx body is a CBOR map with keys 0..7. Unknown/out-of-era keys
/// are HARD-REJECTED, matching Haskell cardano-ledger's per-era SparseKeyed
/// `bodyFields` catch-all (`invalidField n -> invalidKey -> cborError`). The
/// reject is un-gated (no `ifDecoderVersionAtLeast`). See #31-E.
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
                // update = [ proposed_protocol_parameter_updates, epoch ]
                // Decoded so the boundary handler can apply pre-Conway PPUPs
                // (Shelley-Babbage). Without this, the d=0 PPUP that takes
                // preview's d from 1→0 at boundary 1→2 is silently dropped,
                // and every subsequent reward calculation is wrong because
                // `isOverlaySlot` returns true for every slot under d=1
                // → no blocks ever attribute to pools → bprev=0 at every
                // boundary. Tracked as issue #624 (root cause of #621).
                update = Some(read_pre_conway_update_proposal(r)?);
            }
            7 => {
                // auxiliary_data_hash
                auxiliary_data_hash = Some(read_hash32(r)?);
            }
            _ => {
                // Unknown/out-of-era tx-body key — HARD REJECT, per upstream
                // Shelley SparseKeyed bodyFields catch-all (invalidField n ->
                // cborError). Shelley knows ONLY keys 0..7. See #31-E.
                return Err(SerializationError::CborDecode(format!(
                    "Shelley tx body: unknown/invalid key {key}"
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
        guards: Vec::new(),                    // Dijkstra+
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
    let addr_bytes = r.read_bytes_owned()?;
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
///
/// Both map levels use `read_map` (handles definite AND indefinite CBOR map
/// forms) rather than `read_map_header()?.unwrap_or(0)`, which silently
/// decoded an indefinite-length map (`encode_map_open`'s form above 23
/// entries, #932/#938) as zero entries and desynced the reader for
/// everything after — the same shape as `read_mir_cert`'s historical
/// mainnet-Shelley-MIR-cert bug and `read_withdrawals` just above.
fn read_multiasset_map_u64(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<Hash28, BTreeMap<AssetName, u64>>, SerializationError> {
    let mut result = BTreeMap::new();
    let policy_pairs = r.read_map(
        |r| {
            let policy_bytes = r.read_bytes()?;
            Hash28::try_from(policy_bytes).map_err(|_| SerializationError::InvalidLength {
                expected: 28,
                got: policy_bytes.len(),
            })
        },
        |r| {
            r.read_map(
                |r| {
                    let name_bytes = r.read_bytes_owned()?;
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("multiasset: asset name too long".into())
                    })
                },
                |r| r.read_uint(),
            )
        },
    )?;
    for (policy, asset_pairs) in policy_pairs {
        let mut assets: BTreeMap<AssetName, u64> = asset_pairs.into_iter().collect();
        // Haskell `decodeMultiAsset` for decoder version < 9 (Mary/Allegra era
        // CBOR) prunes zero-quantity assets and drops policies whose asset map
        // becomes (or arrives) empty — `pruneZeroMultiAsset`/`filterMultiAsset`
        // (Mary/Value.hs). See era_alonzo::read_multiasset_map_u64 for the
        // quoted Haskell (#730).
        assets.retain(|_, qty| *qty != 0);
        if !assets.is_empty() {
            result.insert(policy, assets);
        }
    }
    Ok(result)
}

/// Read `withdrawals = { reward_account_bytes => coin }`.
///
/// `read_map` handles both the definite and indefinite CBOR map forms — the
/// same fix already applied to `read_mir_cert`'s target map after mainnet's
/// first Shelley MIR certs (an indefinite-length map) were found to silently
/// decode as zero entries under the previous `read_map_header().unwrap_or(0)`
/// pattern, desyncing the reader for everything after. `encode_map_open`
/// switches to indefinite form above 23 entries (`ENCODE_MAP_DEFINITE_MAX`,
/// #932/#938's `encodeMap` semantics), so a >23-entry withdrawals map is
/// exactly the shape dugite's own encoder can produce and this decoder
/// previously could not read back — found live by `fuzz_structured_tx_encode`
/// once the generator started reaching >23-entry maps.
fn read_withdrawals(r: &mut Reader<'_>) -> Result<BTreeMap<Vec<u8>, Lovelace>, SerializationError> {
    let pairs = r.read_map(|r| r.read_bytes_owned(), |r| r.read_uint().map(Lovelace))?;
    Ok(pairs.into_iter().collect())
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
    read_pool_params_inner(r, false)
}

/// The single `pool_params` decoder, shared by every era.
///
/// `strict_owners` selects `read_set_strict` (Conway PV9+ duplicate rejection)
/// over the lenient `read_set`; that is the ONLY per-era difference in this
/// structure, since the `pool_params` CDDL is unchanged from Shelley on.
///
/// It is shared because it was not, and that cost a real bug: #670 required
/// relays to be decoded into `Relay` values rather than skipped, and the fix
/// landed in this file and in the Conway copy but NOT in the Alonzo-family
/// copy — which serves Allegra, Mary, Alonzo AND Babbage, since
/// `era_babbage` calls `era_alonzo::read_alonzo_cert_inner`. Every
/// `PoolRegistration` certificate in those four eras decoded with
/// `relays: []`, which is exactly the symptom #670 was filed for
/// (`pool_params: value_mismatches=605`) and also starves ledger-based peer
/// discovery, which reads relay addresses out of `pool_params`.
///
/// Three copies of a decoder is the same mechanism behind #937 (three drifted
/// `read_metadatum` copies) and #932/#938 (triplicated body encoders).
pub(crate) fn read_pool_params_inner(
    r: &mut Reader<'_>,
    strict_owners: bool,
) -> Result<PoolParams, SerializationError> {
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
    let reward_account = r.read_bytes_owned()?;
    // pool_owners: set of addr_keyhash (28 bytes each)
    let pool_owners: Vec<Hash28> = if strict_owners {
        r.read_set_strict(|r| read_hash28_cert(r))?
    } else {
        r.read_set(|r| read_hash28_cert(r))?
    };
    // relays: array of relay structs (definite OR indefinite-length).
    //
    // #673: cn 11.0.1 emits indefinite-length relay arrays on preview /
    // preprod for some pool registration certificates. The pre-fix
    // `read_array_header()?.unwrap_or(0)` returned 0 for indef-length,
    // silently leaving every relay element in the stream; the next
    // decoder (read_pool_metadata) then read the first relay
    // `single_host_addr = (0, port, ipv4, ipv6)` (array(4)) and failed
    // with "pool_metadata: expected array(2) or null, got Some(4)".
    //
    // Mirrors Haskell `instance DecCBOR (StrictSeq StakePoolRelay)` →
    // `decodeSeq decCBOR` → `decodeListLenOrIndef` (handles both).
    //
    // Issue #670: the relays MUST be decoded into `Relay` values (not
    // just skipped) so that `pool_params` in the from-genesis ledger
    // state matches the Mithril-ancillary import byte-exact. The
    // ancillary path populates relays from `HaskellStakePoolState`
    // (`PoolRegistration.relays`); skipping the relay decode here left
    // the from-genesis path with `relays: []` for every pool and the
    // verify-ledger-snapshot harness reported `pool_params:
    // value_mismatches=605` on preview epoch 1308.
    let mut relays: Vec<Relay> = Vec::new();
    r.for_each_array_item(|r| {
        relays.push(read_relay(r)?);
        Ok(())
    })?;
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
        relays,
        pool_metadata,
    })
}

/// Decode a single CBOR relay structure per the Shelley+ CDDL:
///
/// ```cddl
/// relay =
///   [  single_host_addr   // 0
///   // single_host_name   // 1
///   // multi_host_name    // 2
///   ]
///
/// single_host_addr = [0, port / null, ipv4 / null, ipv6 / null]
/// single_host_name = [1, port / null, dns_name]
/// multi_host_name  = [2, dns_name]
/// ```
///
/// Used by both Shelley/Babbage and Conway pool-registration decoders.
pub(crate) fn read_relay(r: &mut Reader<'_>) -> Result<Relay, SerializationError> {
    let arr_len = r.read_array_header()?;
    let tag = r.read_uint()?;
    match tag {
        0 => {
            // single_host_addr: [0, port?, ipv4?, ipv6?]
            let port = read_opt_port(r)?;
            let ipv4 = read_opt_ipv4(r)?;
            let ipv6 = read_opt_ipv6(r)?;
            if matches!(arr_len, Some(n) if n != 4) {
                return Err(SerializationError::CborDecode(format!(
                    "single_host_addr: expected array(4), got array({:?})",
                    arr_len
                )));
            }
            Ok(Relay::SingleHostAddr { port, ipv4, ipv6 })
        }
        1 => {
            // single_host_name: [1, port?, dns_name]
            let port = read_opt_port(r)?;
            let dns_name = r.read_str()?.to_string();
            if matches!(arr_len, Some(n) if n != 3) {
                return Err(SerializationError::CborDecode(format!(
                    "single_host_name: expected array(3), got array({:?})",
                    arr_len
                )));
            }
            Ok(Relay::SingleHostName { port, dns_name })
        }
        2 => {
            // multi_host_name: [2, dns_name]
            let dns_name = r.read_str()?.to_string();
            if matches!(arr_len, Some(n) if n != 2) {
                return Err(SerializationError::CborDecode(format!(
                    "multi_host_name: expected array(2), got array({:?})",
                    arr_len
                )));
            }
            Ok(Relay::MultiHostName { dns_name })
        }
        other => Err(SerializationError::CborDecode(format!(
            "relay: unknown tag {other}"
        ))),
    }
}

fn read_opt_port(r: &mut Reader<'_>) -> Result<Option<u16>, SerializationError> {
    if r.peek_major()? == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    let v = r.read_uint()?;
    Ok(Some(v as u16))
}

fn read_opt_ipv4(r: &mut Reader<'_>) -> Result<Option<[u8; 4]>, SerializationError> {
    if r.peek_major()? == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    let bytes = r.read_bytes()?;
    if bytes.len() != 4 {
        return Err(SerializationError::CborDecode(format!(
            "ipv4: expected 4 bytes, got {}",
            bytes.len()
        )));
    }
    let mut a = [0u8; 4];
    a.copy_from_slice(bytes);
    Ok(Some(a))
}

fn read_opt_ipv6(r: &mut Reader<'_>) -> Result<Option<[u8; 16]>, SerializationError> {
    if r.peek_major()? == Type::Null {
        r.read_null()?;
        return Ok(None);
    }
    let bytes = r.read_bytes()?;
    if bytes.len() != 16 {
        return Err(SerializationError::CborDecode(format!(
            "ipv6: expected 16 bytes, got {}",
            bytes.len()
        )));
    }
    let mut a = [0u8; 16];
    a.copy_from_slice(bytes);
    Ok(Some(a))
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
        Type::Map | Type::MapIndef => {
            // `{ stake_credential => delta_coin }`, definite OR indefinite map.
            // Mainnet's first Shelley MIR certs use an indefinite-length map;
            // the previous `read_map_header().unwrap_or(0)` silently read zero
            // entries for those and the indefinite type fell through to the
            // error arm — blocking every Shelley block carrying a MIR cert.
            // `read_map` handles both encodings.
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

    // `for_each_field_entry` handles BOTH definite- and indefinite-length CBOR
    // maps (issue #1012) and hard-rejects a duplicate field key, matching
    // Haskell's `decodeSparseKeyed`/`applyField` (the same helper already used
    // for Conway's witness set at `decode_conway_witness_set`, map-form TxOut,
    // and the aux-data tag-259 inner map). The previous
    // `read_map_header()?.unwrap_or(0)` silently decoded an indefinite-length
    // map as ZERO entries and desynced the reader for everything after it —
    // fewer honest encoders reach >23 witness-set keys than the 30+-key PPU
    // sibling (#1012's `read_protocol_param_update`), but dugite is
    // adversarial-deployment software and a hostile peer chooses its own
    // encoding.
    r.for_each_field_entry(|r, key| {
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
                    let vkey = r.read_bytes_owned()?;
                    let signature = r.read_bytes_owned()?;
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
            _ => {
                // Haskell cardano-ledger decodes the witness set via SparseKeyed
                // with field-picker `txWitnessField n = invalidField n`, so an
                // unknown map key hard-fails the decode (invalidField -> Invalid n
                // -> invalidKey -> cborError). Not version-gated — reject in every
                // era. Mirror that strictness here instead of silently skipping.
                return Err(SerializationError::CborDecode(format!(
                    "witness set: unknown key {key}"
                )));
            }
        }
        Ok(())
    })?;

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
    // OUTER ARRAY: accept BOTH definite- AND indefinite-length encodings, matching
    // cardano-ledger's Timelock decoder (`decodeRecordSum` -> `decodeListLenOrIndef`).
    // The previous `is_none() => Err` HARD-REJECTED indefinite-length native scripts
    // that Haskell accepts — over-rejecting valid blocks. Mirrors the already-fixed
    // Conway copy (`era_conway::read_native_script`). See issue #862.
    let arr_len = r.read_array_header()?;
    let disc = r.read_uint()?;
    let script = match disc {
        0 => {
            // ScriptPubkey: hash28 → padded to Hash32
            let h28 = read_hash28_cert(r)?;
            NativeScript::ScriptPubkey(h28.to_hash32_padded())
        }
        1 => {
            let scripts = r.read_array(read_native_script)?;
            NativeScript::ScriptAll(scripts)
        }
        2 => {
            let scripts = r.read_array(read_native_script)?;
            NativeScript::ScriptAny(scripts)
        }
        3 => {
            let n = r.read_uint()? as u32;
            let scripts = r.read_array(read_native_script)?;
            NativeScript::ScriptNOfK(n, scripts)
        }
        4 => {
            let slot = r.read_uint()?;
            NativeScript::InvalidBefore(SlotNo(slot))
        }
        5 => {
            let slot = r.read_uint()?;
            NativeScript::InvalidHereafter(SlotNo(slot))
        }
        other => {
            return Err(SerializationError::CborDecode(format!(
                "native_script: unknown type {other}"
            )))
        }
    };
    // Consume the trailing CBOR break byte for an indefinite-length outer array.
    if arr_len.is_none() {
        r.expect_break()?;
    }
    Ok(script)
}

// ============================================================================
// Auxiliary data decoder
// ============================================================================

/// Decode the auxiliary_data_set map: `{ tx_index => auxiliary_data }`.
///
/// Returns a `BTreeMap<u32, AuxiliaryData>` keyed by transaction index.
///
/// `read_map` handles both the definite and indefinite CBOR map forms.
/// This map is keyed by EVERY transaction index in the block that carries
/// auxiliary data, so a block with more than 23 such transactions is
/// exactly the >23-entry case `encode_map_open` switches to indefinite
/// form for (#932/#938) — the previous `read_map_header()?.unwrap_or(0)`
/// silently decoded that as zero entries and desynced the reader for
/// everything after, the same shape as `read_withdrawals`/
/// `read_multiasset_map_u64` just above.
fn decode_aux_data_map(
    r: &mut Reader<'_>,
) -> Result<BTreeMap<u32, AuxiliaryData>, SerializationError> {
    let pairs = r.read_map(|r| r.read_uint().map(|n| n as u32), decode_auxiliary_data)?;
    Ok(pairs.into_iter().collect())
}

/// Decode a Shelley/Mary auxiliary data value.
///
/// Shelley auxiliary data is just a metadata map: `{ label => metadatum }`.
/// Mary (ShelleyMa) aux data is `[metadata, native_scripts]`.
/// Alonzo+ aux data is a tag(259) map.
///
/// For Shelley, the aux data is always the plain metadata map form.
/// Shelley-era auxiliary data.
///
/// Delegates to the shared decoder. This copy previously had NO `tag(259)`
/// arm, so any PostAlonzo-shaped auxiliary data reaching it decoded to
/// entirely empty auxiliary data — metadata included — and its ShelleyMa arm
/// skipped the native scripts. Issue #984.
fn decode_auxiliary_data(r: &mut Reader<'_>) -> Result<AuxiliaryData, SerializationError> {
    super::era_alonzo::decode_alonzo_auxiliary_data(r)
}

// ============================================================================
// Standalone tx decoder (Shelley era)
// ============================================================================

/// Reconstruct the full wire-format CBOR for a pre-Alonzo tx decoded from a block.
///
/// Pre-Alonzo blocks (Shelley/Allegra/Mary) embed transactions as separate
/// `[body, wits, aux_data]` arrays without an `is_valid` flag.  The block
/// decoders do not materialise the full per-tx CBOR slice.  This function
/// reassembles it so that `Transaction.raw_cbor` has the correct byte-length.
///
/// Haskell fee size for Shelley/Allegra/Mary = full wire size of
/// `[body, wits, aux_data]` (3-element, no subtraction).  `fee_tx_size()` in
/// scripts.rs does not subtract for `0x83` (pre-Alonzo), so returning a
/// 3-element array is byte-exact.
///
/// Reconstruction:
/// ```text
/// [0x83]  raw_body  raw_witness  (aux_raw | 0xF6)
/// ```
pub(crate) fn reconstruct_pre_alonzo_tx_raw_cbor(
    raw_body: &[u8],
    raw_witness: &[u8],
    auxiliary_data: Option<&dugite_primitives::transaction::AuxiliaryData>,
) -> Vec<u8> {
    let aux_bytes: &[u8] = match auxiliary_data {
        Some(aux) => aux.raw_cbor.as_deref().unwrap_or(&[0xF6]),
        None => &[0xF6],
    };
    let mut out = Vec::with_capacity(1 + raw_body.len() + raw_witness.len() + aux_bytes.len());
    out.push(0x83); // array(3)
    out.extend_from_slice(raw_body);
    out.extend_from_slice(raw_witness);
    out.extend_from_slice(aux_bytes);
    out
}

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
// Pre-Conway PPUP decoder (shared with Alonzo, Babbage; Allegra/Mary inherit
// via decode_alonzo_family_block which now also wires it up)
// ============================================================================

/// Decode a pre-Conway `update` field from a tx body:
///
/// ```text
/// update = [ proposed_protocol_parameter_updates, epoch ]
/// proposed_protocol_parameter_updates = { * genesishash => protocol_param_update }
/// ```
///
/// `genesishash` is a 28-byte blake2b_224 hash of a genesis delegate's vkey.
/// We pad to 32 bytes via [`Hash28::to_hash32_padded`] for storage as
/// `Hash32` in `UpdateProposal::proposed_updates`.
pub(crate) fn read_pre_conway_update_proposal(
    r: &mut Reader<'_>,
) -> Result<UpdateProposal, SerializationError> {
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "pre-conway update: expected array(2), got {arr_len:?}"
        )));
    }
    let proposed_updates = r.read_map(
        |r| {
            let h28 = read_hash28(r)?;
            Ok(h28.to_hash32_padded())
        },
        read_pre_conway_protocol_param_update,
    )?;
    let epoch = r.read_uint()?;
    Ok(UpdateProposal {
        proposed_updates,
        epoch,
    })
}

/// Decode a pre-Conway `protocol_param_update` map covering the union of
/// Shelley/Allegra/Mary/Alonzo/Babbage key sets (keys 0–24).
///
/// Cross-referenced against Haskell:
///   `eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs`
///   `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/PParams.hs`
///   `eras/babbage/impl/src/Cardano/Ledger/Babbage/PParams.hs`
///
/// Keys:
/// - 0: minfee A (uint)
/// - 1: minfee B (uint)
/// - 2: max block body size (uint)
/// - 3: max tx size (uint)
/// - 4: max block header size (uint)
/// - 5: key deposit (coin)
/// - 6: pool deposit (coin)
/// - 7: e_max (uint)
/// - 8: n_opt (uint)
/// - 9: a0 (nonnegative_interval = tag 30 rational)
/// - 10: rho (unit_interval = tag 30 rational)
/// - 11: tau (unit_interval = tag 30 rational)
/// - 12: d (unit_interval = tag 30 rational) — Shelley-Alonzo
/// - 13: extra_entropy (nonce) — currently skipped (deprecated in Babbage+)
/// - 14: [protocol_version_major, protocol_version_minor]
/// - 15: min_utxo_value (coin) — Shelley-Mary only; decoded into
///   `ProtocolParamUpdate.min_utxo_value` (issue #919; previously skipped)
/// - 16: min pool cost (coin) — Alonzo+
/// - 17: ada per utxo word (Alonzo) / coins per utxo byte (Babbage) — same wire
///   shape, disambiguated by protocol version IN FORCE at apply time (see
///   `ProtocolParameters::apply_key17_update`, issue #919)
/// - 18: cost models
/// - 19: ex unit prices
/// - 20: max tx ex units
/// - 21: max block ex units
/// - 22: max value size (uint)
/// - 23: collateral percentage (uint)
/// - 24: max collateral inputs (uint)
///
/// Unknown keys are skipped (forward compatibility).
/// Decode a standalone pre-Conway `protocol_param_update` CBOR map.
///
/// The Shelley..Babbage counterpart of [`crate::decode::ppu_from_cbor`], and
/// the decode half that `encode_pre_conway_protocol_param_update` must
/// round-trip against — the two key sets genuinely differ (keys 12-15 exist
/// only here; keys 25-37 only in Conway).
pub fn pre_conway_ppu_from_cbor(cbor: &[u8]) -> Result<ProtocolParamUpdate, SerializationError> {
    let mut r = Reader::new(cbor);
    read_pre_conway_protocol_param_update(&mut r)
}

pub(crate) fn read_pre_conway_protocol_param_update(
    r: &mut Reader<'_>,
) -> Result<ProtocolParamUpdate, SerializationError> {
    let mut ppu = ProtocolParamUpdate::default();
    // `for_each_map_entry` handles BOTH definite- and indefinite-length CBOR
    // maps (last-wins on a repeated key, matching Haskell's `PParamsUpdate`
    // decode — see #1012's oracle finding on `read_protocol_param_update`
    // just below in `era_conway.rs`, which shares this exact shape). The
    // original fix here (faaaed42d8) hand-rolled the same definite/indefinite
    // sentinel loop that `for_each_map_entry` already implements and that
    // `decode_alonzo_aux_data_map` already uses for an identical purpose;
    // routing through the shared helper removes that second copy rather than
    // leaving it to drift — #1012 swept for exactly this pattern. Behavior is
    // unchanged: this refactor is covered by the same four tests
    // faaaed42d8 added, including the two indefinite-length-map regressions.
    //
    // Previously (pre-faaaed42d8): `read_map_header()?.unwrap_or(0)` silently
    // decoded an indefinite-length map — `encode_map_open`'s >23-entry form,
    // #932/#938 — as zero entries and desynced the reader for everything
    // after; the same bug shape as `read_withdrawals`/`read_multiasset_map_u64`/
    // `decode_aux_data_map` above. A pre-Conway PPU has ~30 possible keys, so
    // a proposal setting more than 23 of them at once is exactly the case
    // this was blind to.
    r.for_each_map_entry(|r| {
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
            12 => {
                let rat = r.read_rational()?;
                ppu.d = Some(Rational {
                    numerator: rat.numerator,
                    denominator: rat.denominator,
                });
            }
            13 => {
                // extra_entropy (Shelley Nonce). CBOR: [0] = NeutralNonce, or
                // [1, bytes32] = Nonce(h). Folded into the epoch nonce at the
                // TICKN rule (η0 = ηc ⭒ ηh ⭒ extraEntropy). Mainnet injected a
                // one-time non-neutral value effective epoch 259 — dropping it
                // here desynchronises every epoch nonce from that point on.
                let arr = r.read_array_header()?;
                let tag = r.read_uint()?;
                ppu.extra_entropy = Some(match tag {
                    0 => Hash32::ZERO,
                    1 => read_hash32(r)?,
                    other => {
                        return Err(SerializationError::CborDecode(format!(
                            "extra_entropy: invalid nonce tag {other} (expected 0 or 1)"
                        )));
                    }
                });
                let _ = arr;
            }
            14 => {
                // [protocol_version_major, protocol_version_minor]
                let arr_len = r.read_array_header()?;
                if !matches!(arr_len, Some(2)) {
                    return Err(SerializationError::CborDecode(format!(
                        "protocol_version: expected array(2), got {arr_len:?}"
                    )));
                }
                ppu.protocol_version_major = Some(r.read_uint()?);
                ppu.protocol_version_minor = Some(r.read_uint()?);
            }
            15 => {
                // min_utxo_value (Shelley-Mary only): the flat `minUTxOValue`
                // PParam. Alonzo removes it from the wire PParamUpdate type
                // entirely (replaced by `coinsPerUTxOWord` at key 17), so this
                // key is never present in an Alonzo+ proposal in practice.
                // Previously dropped on the floor — Shelley/Allegra/Mary's
                // `getMinCoinTxOut` uses this value directly (never the
                // Babbage/Conway serialized-size formula), so silently
                // discarding a genuine on-chain minUTxOValue update
                // desynchronised the minimum-UTxO check from Haskell for any
                // chain whose genesis or on-chain PPU changed it (issue #919).
                ppu.min_utxo_value = Some(read_lovelace(r)?);
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
            _ => {
                r.skip()?;
            }
        }
        Ok(())
    })?;
    Ok(ppu)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::transaction::TransactionMetadatum;

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

    #[test]
    fn pre_conway_pp_update_decodes_extra_entropy_key13() {
        // map { 13: [1, <32 bytes>] } — a concrete (non-neutral) nonce.
        let entropy = [0xABu8; 32];
        let one = cbor_uint(1);
        let bytes = cbor_bytes(&entropy);
        let nonce_arr = cbor_arr(&[&one, &bytes]);
        let mut cbor = vec![0xa1]; // map(1)
        cbor.extend(cbor_uint(13));
        cbor.extend(nonce_arr);
        let mut r = Reader::new(&cbor);
        let ppu = read_pre_conway_protocol_param_update(&mut r).unwrap();
        assert_eq!(ppu.extra_entropy, Some(Hash32::from_bytes(entropy)));

        // map { 13: [0] } — NeutralNonce decodes to ZERO.
        let zero = cbor_uint(0);
        let neutral = cbor_arr(&[&zero]);
        let mut cbor2 = vec![0xa1];
        cbor2.extend(cbor_uint(13));
        cbor2.extend(neutral);
        let mut r2 = Reader::new(&cbor2);
        let ppu2 = read_pre_conway_protocol_param_update(&mut r2).unwrap();
        assert_eq!(ppu2.extra_entropy, Some(Hash32::ZERO));

        // Absent key 13 → None (not all updates carry it).
        let empty = cbor_map0();
        let mut r3 = Reader::new(&empty);
        let ppu3 = read_pre_conway_protocol_param_update(&mut r3).unwrap();
        assert_eq!(ppu3.extra_entropy, None);
    }

    // -------------------------------------------------------------------
    // Indefinite-length map decoding (found by `fuzz_structured_tx_encode`
    // once its generator started reaching >23-entry maps; same bug shape
    // as `read_mir_cert`'s historical mainnet-Shelley-MIR-cert fix).
    //
    // `encode_map_open` (#932/#938, mirroring Haskell's `encodeMap`) emits
    // an INDEFINITE-length map header for maps with more than 23 entries.
    // `read_map_header()?.unwrap_or(0)` treats ANY indefinite map as ZERO
    // entries regardless of its actual size, so a 2-entry indefinite map
    // is sufficient to reproduce the bug without needing 24 real entries —
    // these tests hand-build the smallest reproducer rather than the
    // large fixture that would be needed to force the encoder's own
    // threshold. `read_map`/the indefinite-aware loop must decode the
    // SAME logical content whether the map arrived definite or indefinite.
    // -------------------------------------------------------------------

    fn cbor_bytes_indef_map2(entries: [(&[u8], &[u8]); 2]) -> Vec<u8> {
        let mut v = vec![0xbf]; // indefinite map open
        for (k, val) in entries {
            v.extend(cbor_bytes(k));
            v.extend_from_slice(val);
        }
        v.push(0xff); // break
        v
    }

    #[test]
    fn read_withdrawals_accepts_indefinite_length_map() {
        let acct_a = [0xe0u8; 29];
        let acct_b = [0xe1u8; 29];
        let cbor = cbor_bytes_indef_map2([(&acct_a, &cbor_uint(5)), (&acct_b, &cbor_uint(7))]);
        let mut r = Reader::new(&cbor);
        let withdrawals = read_withdrawals(&mut r).expect("indefinite withdrawals map must decode");
        assert_eq!(withdrawals.len(), 2, "PRE-FIX this decoded as 0 entries");
        assert_eq!(withdrawals.get(acct_a.as_slice()), Some(&Lovelace(5)));
        assert_eq!(withdrawals.get(acct_b.as_slice()), Some(&Lovelace(7)));
    }

    #[test]
    fn read_multiasset_map_u64_accepts_indefinite_length_outer_and_inner_maps() {
        let policy_a = [0x11u8; 28];
        let policy_b = [0x22u8; 28];
        let asset_name = b"tok";
        // Inner asset map is ALSO indefinite, exercising both levels at once.
        let mut inner_a = vec![0xbf];
        inner_a.extend(cbor_bytes(asset_name));
        inner_a.extend(cbor_uint(9));
        inner_a.push(0xff);
        let mut inner_b = vec![0xbf];
        inner_b.extend(cbor_bytes(asset_name));
        inner_b.extend(cbor_uint(3));
        inner_b.push(0xff);

        let mut cbor = vec![0xbf]; // outer indefinite map
        cbor.extend(cbor_bytes(&policy_a));
        cbor.extend(inner_a);
        cbor.extend(cbor_bytes(&policy_b));
        cbor.extend(inner_b);
        cbor.push(0xff);

        let mut r = Reader::new(&cbor);
        let assets =
            read_multiasset_map_u64(&mut r).expect("indefinite multiasset map must decode");
        assert_eq!(assets.len(), 2, "PRE-FIX this decoded as 0 policies");
        let name = AssetName::new(asset_name.to_vec()).unwrap();
        assert_eq!(
            assets
                .get(&Hash28::from_bytes(policy_a))
                .and_then(|m| m.get(&name)),
            Some(&9)
        );
        assert_eq!(
            assets
                .get(&Hash28::from_bytes(policy_b))
                .and_then(|m| m.get(&name)),
            Some(&3)
        );
    }

    #[test]
    fn decode_aux_data_map_accepts_indefinite_length_map() {
        // { 0 : <empty aux data>, 2 : <empty aux data> } as an indefinite map.
        let empty_aux = cbor_arr(&[&cbor_arr(&[]), &cbor_arr(&[])]); // metadata{}, [native_scripts]
        let mut cbor = vec![0xbf];
        cbor.extend(cbor_uint(0));
        cbor.extend(cbor_map0()); // aux-data shorthand: bare metadata map form
        cbor.extend(cbor_uint(2));
        cbor.extend(cbor_map0());
        cbor.push(0xff);
        let _ = empty_aux; // (kept for documentation of the array-form alternative; unused here)

        let mut r = Reader::new(&cbor);
        let aux_map = decode_aux_data_map(&mut r).expect("indefinite aux-data-set map must decode");
        assert_eq!(aux_map.len(), 2, "PRE-FIX this decoded as 0 entries");
        assert!(aux_map.contains_key(&0));
        assert!(aux_map.contains_key(&2));
    }

    #[test]
    fn pre_conway_ppu_accepts_indefinite_length_map() {
        // { 7: e_max, 8: n_opt } as an indefinite map.
        let mut cbor = vec![0xbf];
        cbor.extend(cbor_uint(7));
        cbor.extend(cbor_uint(100));
        cbor.extend(cbor_uint(8));
        cbor.extend(cbor_uint(150));
        cbor.push(0xff);

        let mut r = Reader::new(&cbor);
        let ppu =
            read_pre_conway_protocol_param_update(&mut r).expect("indefinite PPU map must decode");
        assert_eq!(
            ppu.e_max,
            Some(100),
            "PRE-FIX this decoded as None (0 entries read)"
        );
        assert_eq!(ppu.n_opt, Some(150));
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
        // CDDL: (5, genesishash: $hash28, genesis_delegate_hash: $hash28,
        //        vrf_keyhash: $hash32). The previous fixture used 32-byte
        //        key hashes, masking a decoder that read all three fields
        //        as Hash32 and broke at the FIRST real cert on mainnet
        //        (slot 66137371, pre-Vasil genesis-delegate rotations).
        let mut cert = vec![0x84]; // array(4)
        cert.extend(cbor_uint(5));
        cert.extend(cbor_bytes(&[0x01; 28])); // genesis KEY hash (28!)
        cert.extend(cbor_bytes(&[0x02; 28])); // delegate KEY hash (28!)
        cert.extend(cbor_bytes(&[0x03; 32])); // vrf keyhash (32)
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        match &block.transactions[0].body.certificates[0] {
            Certificate::GenesisKeyDelegation {
                genesis_hash,
                genesis_delegate_hash,
                vrf_keyhash,
            } => {
                // 28-byte hashes are stored zero-PADDED in the Hash32 enum
                // fields (the ledger truncates back to 28 on apply).
                assert_eq!(&genesis_hash.as_bytes()[..28], &[0x01; 28]);
                assert_eq!(&genesis_hash.as_bytes()[28..], &[0u8; 4]);
                assert_eq!(&genesis_delegate_hash.as_bytes()[..28], &[0x02; 28]);
                assert_eq!(vrf_keyhash.as_bytes(), &[0x03; 32]);
            }
            other => panic!("expected GenesisKeyDelegation, got {other:?}"),
        }
    }

    #[test]
    fn shelley_genesis_key_delegation_real_mainnet_cert() {
        // The exact certificate from mainnet block 7492516 (slot 66137371)
        // that broke the from-genesis sync at first contact — the genesis
        // delegate key rotation ahead of the Vasil HF.
        let genesis_hash =
            hex::decode("2075a095b3c844a29c24317a94a643ab8e22d54a3a3a72a420260af6").unwrap();
        let delegate_hash =
            hex::decode("98599cbfede2ff9471797f7115ce2f745d83026936759fcf95092cc1").unwrap();
        let vrf = hex::decode("5549bba78a65e5160a8421b9ad7cf0db017dc8aa84e2f2cb957c490a2f699aca")
            .unwrap();
        let mut cert = vec![0x84];
        cert.extend(cbor_uint(5));
        cert.extend(cbor_bytes(&genesis_hash));
        cert.extend(cbor_bytes(&delegate_hash));
        cert.extend(cbor_bytes(&vrf));
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        match &block.transactions[0].body.certificates[0] {
            Certificate::GenesisKeyDelegation {
                genesis_hash: g,
                genesis_delegate_hash: d,
                vrf_keyhash: v,
            } => {
                assert_eq!(&g.as_bytes()[..28], genesis_hash.as_slice());
                assert_eq!(&d.as_bytes()[..28], delegate_hash.as_slice());
                assert_eq!(v.as_bytes(), vrf.as_slice());
            }
            other => panic!("expected GenesisKeyDelegation, got {other:?}"),
        }
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
    fn shelley_mir_certificate_indefinite_map_target() {
        // Mainnet's first Shelley MIR certs (epoch 208) encode the target as an
        // INDEFINITE-length map `{ stake_credential => delta_coin }` (0xbf … 0xff).
        // Regression for "mir target: expected map or uint, got indefinite map",
        // which blocked the entire Byron→Shelley boundary on a from-genesis sync.
        let mut cert = vec![0x82];
        cert.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(0)); // source = Reserves
        mir.push(0xbf); // target = INDEFINITE map
        mir.push(0x82); // key: stake credential [0, hash28]
        mir.extend(cbor_uint(0));
        mir.extend(cbor_bytes(&[0x09; 28]));
        mir.push(0x20); // value: delta = -1
        mir.push(0xff); // break (end of indefinite map)
        cert.extend(&mir);
        let mut extra = Vec::new();
        extra.extend(cbor_uint(4));
        extra.push(0x81);
        extra.extend(&cert);
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        match &block.transactions[0].body.certificates[0] {
            Certificate::MoveInstantaneousRewards {
                source: MIRSource::Reserves,
                target: MIRTarget::StakeCredentials(creds),
            } => {
                assert_eq!(creds.len(), 1, "indefinite map must decode its 1 entry");
                assert_eq!(creds[0].1, -1, "delta should be -1");
            }
            other => panic!("unexpected cert: {other:?}"),
        }
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
    fn shelley_body_key_6_update_proposal_empty() {
        // key 6 = update = [ { } , 0 ] — empty proposal map targeting epoch 0.
        // After #624 we decode this rather than skip; the result is a
        // Some(UpdateProposal{ proposed_updates: [], epoch: 0 }).
        let mut extra = Vec::new();
        extra.extend(cbor_uint(6));
        extra.push(0x82); // array(2)
        extra.push(0xa0); // empty map (proposed_updates)
        extra.push(0x00); // uint 0 (target epoch)
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        let up = block.transactions[0]
            .body
            .update
            .as_ref()
            .expect("update set");
        assert_eq!(up.proposed_updates.len(), 0);
        assert_eq!(up.epoch, 0);
    }

    #[test]
    fn shelley_body_key_6_update_proposal_with_d_zero() {
        // Real-shape PPUP: one genesis delegate proposes { d: 0/1 } for epoch 2.
        //   update = [ { delegate_hash(28) => { 12: tag30 [0, 1] } }, 2 ]
        let mut extra = Vec::new();
        extra.extend(cbor_uint(6));
        extra.push(0x82); // array(2)
                          // proposed_updates: map of 1 entry
        extra.push(0xa1); // map(1)
                          // key: 28-byte hash (genesis delegate cold key hash)
        extra.push(0x58); // bytes(uint8 length)
        extra.push(28);
        extra.extend([0xaau8; 28]);
        // value: param_update map with key 12 → tag30 [0,1]
        extra.push(0xa1); // map(1)
        extra.push(0x0c); // uint 12
        extra.push(0xd8); // tag prefix (2-byte form)
        extra.push(30); // tag 30
        extra.push(0x82); // array(2)
        extra.push(0x00); // 0
        extra.push(0x01); // 1
                          // target epoch
        extra.push(0x02);

        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        let up = block.transactions[0]
            .body
            .update
            .as_ref()
            .expect("update set");
        assert_eq!(up.epoch, 2);
        assert_eq!(up.proposed_updates.len(), 1);
        let (_hash, ppu) = &up.proposed_updates[0];
        let d = ppu.d.as_ref().expect("d set");
        assert_eq!(d.numerator, 0);
        assert_eq!(d.denominator, 1);
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
    fn shelley_body_unknown_key_rejected() {
        // Unknown key 42 — must be HARD-REJECTED. Haskell cardano-ledger's
        // Shelley SparseKeyed bodyFields catch-all (invalidField n -> cborError)
        // fails on any out-of-domain tx-body key. See #31-E.
        let mut extra = Vec::new();
        extra.extend(cbor_uint(42));
        extra.extend(cbor_uint(0));
        let result = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1));
        assert!(
            result.is_err(),
            "unknown Shelley tx-body key must be rejected, got {result:?}"
        );
    }

    #[test]
    fn shelley_body_key_8_rejected() {
        // Key 8 (validity_interval_start) is Allegra+ — Shelley must reject it.
        let mut extra = Vec::new();
        extra.extend(cbor_uint(8));
        extra.extend(cbor_uint(0));
        let result = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1));
        assert!(
            result.is_err(),
            "Shelley tx-body key 8 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn shelley_body_key_9_rejected() {
        // Key 9 (mint) is Mary+ — Shelley must reject it.
        let mut extra = Vec::new();
        extra.extend(cbor_uint(9));
        extra.extend(cbor_uint(0));
        let result = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1));
        assert!(
            result.is_err(),
            "Shelley tx-body key 9 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn shelley_body_key_7_accepted() {
        // Key 7 (auxiliary_data_hash) is the highest valid Shelley key.
        let mut extra = Vec::new();
        extra.extend(cbor_uint(7));
        extra.extend(cbor_bytes(&[0x42; 32]));
        let block = decode_shelley_block(&shelley_block_with_tx_body(&extra, 1)).unwrap();
        assert_eq!(
            block.transactions[0].body.auxiliary_data_hash.unwrap(),
            Hash32::from_bytes([0x42; 32])
        );
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
    fn shelley_witness_set_unknown_key_rejected() {
        // ws = {99: 0} — unknown key, value uint(0). Haskell cardano-ledger
        // SparseKeyed (txWitnessField n = invalidField n) hard-fails an unknown
        // witness-set map key, so dugite must reject the block decode too.
        let mut ws = vec![0xa1];
        ws.extend(cbor_uint(99));
        ws.extend(cbor_uint(0));
        let result = decode_shelley_block(&shelley_block_with_witness_set(&ws));
        assert!(
            result.is_err(),
            "unknown witness-set key must be rejected, got {result:?}"
        );
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

    // ── #1012: decode_shelley_witness_set must accept indefinite-length maps ──
    //
    // The previous `read_map_header()?.unwrap_or(0)` silently decoded an
    // indefinite-length witness-set map as ZERO entries and desynced the
    // reader for everything after it — the same bug shape as the
    // `read_protocol_param_update` fix in `era_conway.rs` (#1012). Now routed
    // through `for_each_field_entry`, the same strict (definite- and
    // indefinite-aware, duplicate-key-rejecting) helper Conway's witness set
    // already used (`decode_conway_witness_set`).

    #[test]
    fn shelley_witness_set_accepts_indefinite_length_map() {
        // Same logical content as `shelley_witness_set_vkey_decoded` +
        // `shelley_witness_set_bootstrap_decoded`, combined into one map and
        // encoded BOTH ways: {0: [[vkey,sig]], 2: [[vkey,sig,cc,attrs]]}.
        let vkey_entry = {
            let mut v = vec![0x81]; // array(1)
            v.push(0x82); // [vkey, sig]
            v.extend(cbor_bytes(&[0x77; 32]));
            v.extend(cbor_bytes(&[0x88; 64]));
            v
        };
        let bootstrap_entry = {
            let mut v = vec![0x81]; // array(1)
            v.push(0x84); // [vkey, sig, chain_code, attrs]
            v.extend(cbor_bytes(&[0x01; 32]));
            v.extend(cbor_bytes(&[0x02; 64]));
            v.extend(cbor_bytes(&[0x03; 32]));
            v.extend(cbor_bytes(&[]));
            v
        };

        let mut ws_definite = vec![0xa2]; // map(2)
        ws_definite.extend(cbor_uint(0));
        ws_definite.extend(&vkey_entry);
        ws_definite.extend(cbor_uint(2));
        ws_definite.extend(&bootstrap_entry);

        let mut ws_indefinite = vec![0xbf]; // indefinite map open
        ws_indefinite.extend(cbor_uint(0));
        ws_indefinite.extend(&vkey_entry);
        ws_indefinite.extend(cbor_uint(2));
        ws_indefinite.extend(&bootstrap_entry);
        ws_indefinite.push(0xff); // break

        let definite_block = decode_shelley_block(&shelley_block_with_witness_set(&ws_definite))
            .expect("definite-form witness set must decode");
        let indefinite_block =
            decode_shelley_block(&shelley_block_with_witness_set(&ws_indefinite))
                .expect("indefinite-form witness set must decode");

        let definite_ws = &definite_block.transactions[0].witness_set;
        let indefinite_ws = &indefinite_block.transactions[0].witness_set;

        assert_eq!(
            indefinite_ws.vkey_witnesses, definite_ws.vkey_witnesses,
            "PRE-FIX the indefinite form decoded as 0 witnesses"
        );
        assert_eq!(
            indefinite_ws.bootstrap_witnesses,
            definite_ws.bootstrap_witnesses
        );
        assert_eq!(definite_ws.vkey_witnesses.len(), 1);
        assert_eq!(definite_ws.bootstrap_witnesses.len(), 1);
    }

    #[test]
    fn shelley_witness_set_indefinite_unknown_key_rejected() {
        // Indefinite-map counterpart of `shelley_witness_set_unknown_key_rejected`:
        // the strictness must survive on the indefinite path too, now that both
        // go through the same `for_each_field_entry` helper.
        let mut ws = vec![0xbf]; // indefinite map open
        ws.extend(cbor_uint(99));
        ws.extend(cbor_uint(0));
        ws.push(0xff); // break
        let result = decode_shelley_block(&shelley_block_with_witness_set(&ws));
        assert!(
            result.is_err(),
            "unknown witness-set key must be rejected on the indefinite path too, got {result:?}"
        );
    }

    #[test]
    fn shelley_witness_set_indefinite_duplicate_key_rejected() {
        // {0: [], 0: []} as an indefinite map: `for_each_field_entry` rejects a
        // repeated field key (Haskell `decodeSparseKeyed`/`applyField`), so this
        // must fail rather than silently keep the last value.
        let mut ws = vec![0xbf]; // indefinite map open
        ws.extend(cbor_uint(0));
        ws.push(0x80); // empty array of vkey witnesses
        ws.extend(cbor_uint(0));
        ws.push(0x80);
        ws.push(0xff); // break
        let result = decode_shelley_block(&shelley_block_with_witness_set(&ws));
        assert!(
            result.is_err(),
            "duplicate witness-set key must be rejected, got {result:?}"
        );
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
