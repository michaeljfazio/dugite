//! In-house decoder for Byron era blocks (era tags 0 and 1).
//!
//! # Byron block wire format
//!
//! The outer envelope (already consumed by `decode_block_envelope`) wraps the
//! inner block as a 2-element array `[era_tag, inner]`. After stripping the
//! envelope the inner CBOR is:
//!
//! ## Main block (era tag 0)
//!
//! ```text
//! block = [header, body, extra]
//! header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
//! consensus_data = [slot_id, issuer_pubkey, difficulty, block_sig]
//! slot_id = [epoch, rel_slot]
//! difficulty = [uint]  ; always a 1-element array
//! ```
//!
//! ## Epoch Boundary Block (era tag 1)
//!
//! ```text
//! ebb = [header, body, extra]
//! header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
//! consensus_data = [[epoch_id], difficulty]
//! difficulty = [uint]  ; always a 1-element array
//! ```
//!
//! # Header hash computation
//!
//! The header hash is **not** a simple hash over the header bytes. Pallas (and
//! Haskell cardano-node) prefix the header bytes with a type tag:
//!
//! - EBB header: `blake2b_256(cbor_encode([0u16, raw_header_cbor]))`
//! - Main header: `blake2b_256(cbor_encode([1u16, raw_header_cbor]))`
//!
//! This matches `OriginalHash for KeepRaw<'_, byron::BlockHead>` in pallas which
//! uses `Hasher::<256>::hash_cbor(&(1, self))`.
//!
//! # Slot computation
//!
//! When `byron_epoch_length == 0` (mainnet / default), the slot is computed with
//! the mainnet GenesisValues formula:
//!   `absolute_slot = epoch * 21600 + rel_slot`
//!   (derived from `(epoch * 432000) / 20 + rel_slot` where 432000 = epoch length
//!    and 20 = slot length in seconds).
//!
//! When `byron_epoch_length > 0` (preview, preprod, custom), the caller provides
//! the epoch-slot multiplier directly:
//!   `absolute_slot = epoch * byron_epoch_length + rel_slot`
//!
//! This matches the behaviour in `crate::multi_era::decode_block_header`.
//!
//! # Byron transactions
//!
//! Byron transactions have a completely different format from Shelley+. Each tx
//! in the block body is a `TxPayload = [tx, witness]` pair. The tx body itself
//! is `[inputs, outputs, attributes]`.
//!
//! The tx hash is `blake2b_256(raw_tx_cbor)` — i.e. the raw CBOR bytes of the
//! `Tx` struct (NOT the `TxPayload` that includes witnesses).

use crate::decode::helpers::read_hash32;
use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::address::Address;
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet,
};
use dugite_primitives::value::{Lovelace, Value};
use std::collections::BTreeMap;

// ============================================================================
// Mainnet Byron genesis constants (from pallas GenesisValues::mainnet())
// ============================================================================

/// Mainnet Byron epoch length in slots.
const MAINNET_BYRON_EPOCH_LENGTH: u64 = 432_000;

/// Mainnet Byron slot length in seconds.
const MAINNET_BYRON_SLOT_LENGTH: u64 = 20;

/// Compute absolute slot from (epoch, rel_slot) using the mainnet formula.
///
/// Matches pallas `compute_absolute_slot_within_era(epoch, slot, 432000, 20)`:
/// `(epoch * 432000) / 20 + slot = epoch * 21600 + slot`.
#[inline]
fn mainnet_absolute_slot(epoch: u64, rel_slot: u64) -> u64 {
    epoch * (MAINNET_BYRON_EPOCH_LENGTH / MAINNET_BYRON_SLOT_LENGTH) + rel_slot
}

// ============================================================================
// Header hash helpers
// ============================================================================

/// Compute the Byron main-block header hash from the raw header CBOR bytes.
///
/// The hash is `blake2b_256(cbor([1u16, raw_header_bytes]))`.
///
/// CBOR encoding:
/// - `0x82` — array(2)
/// - `0x19 0x00 0x01` — uint 1 (pallas uses u16 encoding: major 1, 2-byte extra)
///
/// Wait: pallas uses Rust's minicbor where `(1u16, data)` encodes `1` as
/// u16 → CBOR major type 0 (unsigned int) with value 1. The minimal CBOR for
/// uint 1 is just `0x01`. Let's use that.
///
/// Actually pallas calls `hash_cbor(&(1, self))` where `1` is a Rust integer
/// literal — minicbor will encode it as the smallest uint, which is `0x01`.
/// So the encoding is `[0x82, 0x01, <raw_header_bytes>]`.
fn byron_main_header_hash(raw_header_cbor: &[u8]) -> Hash32 {
    // Build: array(2) [uint(1), bstr(raw_header_cbor)]
    // But wait — pallas KeepRaw<BlockHead> encodes itself as its raw bytes
    // WITHOUT a bstr wrapper. When you encode (1, keep_raw_value), you get:
    //   array(2)[uint(1), <keep_raw_cbor_as_is>]
    // So the input to blake2b is:
    //   [0x82, 0x01, ...raw_header_bytes...]
    let mut buf = Vec::with_capacity(2 + raw_header_cbor.len());
    buf.push(0x82); // array(2)
    buf.push(0x01); // uint(1)
    buf.extend_from_slice(raw_header_cbor);
    blake2b_256(&buf)
}

/// Compute the Byron EBB header hash from the raw header CBOR bytes.
///
/// The hash is `blake2b_256(cbor([0u16, raw_header_bytes]))`.
fn byron_ebb_header_hash(raw_header_cbor: &[u8]) -> Hash32 {
    let mut buf = Vec::with_capacity(2 + raw_header_cbor.len());
    buf.push(0x82); // array(2)
    buf.push(0x00); // uint(0)
    buf.extend_from_slice(raw_header_cbor);
    blake2b_256(&buf)
}

// ============================================================================
// Byron Tx decode
// ============================================================================

/// Read a single Byron TxInput.
///
/// Byron TxIn = `[0, #6.24(bytes .cbor ([txhash, txix]))]`
/// The outer tag is the input type discriminator (0 = PubKey input).
fn read_byron_tx_input(r: &mut Reader<'_>) -> Result<TransactionInput, SerializationError> {
    // Byron inputs are tagged with a discriminator at position 0.
    // CDDL: tx_in = [0, #6.24(bytes .cbor ([txhash, txix]))]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(2) => {}
        _ => {
            return Err(SerializationError::CborDecode(
                "byron tx_in: expected array(2)".into(),
            ));
        }
    }
    // Discriminator (must be 0 for PubKey inputs)
    let disc = r.read_uint()?;
    if disc != 0 {
        return Err(SerializationError::CborDecode(format!(
            "byron tx_in: unknown discriminator {disc}"
        )));
    }
    // tag(24) wrapped CBOR containing [txhash, txix].
    // Collect to owned Vec to avoid lifetime entanglement with the outer reader.
    let inner_bytes: Vec<u8> = r.read_embedded_cbor_bytes()?.to_vec();
    // Decode the inner [txhash, txix]
    let mut inner_r = Reader::new(&inner_bytes);
    let inner_len = inner_r.read_array_header()?;
    match inner_len {
        Some(2) => {}
        _ => {
            return Err(SerializationError::CborDecode(
                "byron tx_in inner: expected array(2)".into(),
            ));
        }
    }
    let tx_hash = read_hash32(&mut inner_r)?;
    let index = inner_r.read_uint()? as u32;
    Ok(TransactionInput {
        transaction_id: tx_hash,
        index,
    })
}

/// Read a single Byron TxOutput.
///
/// Byron outputs use the format:
/// `tx_out = [address, amount]`
/// where `address = [addr_payload, crc32]` (a 2-element array).
fn read_byron_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    // [address, coin]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(2) => {}
        _ => {
            return Err(SerializationError::CborDecode(
                "byron tx_out: expected array(2)".into(),
            ));
        }
    }
    // Skip address — read raw bytes of the address CBOR
    // Byron address is a 2-element array [addr_payload, crc32]
    // We capture the raw bytes for the Address type.
    let addr_start = r.position();
    r.skip()?;
    let addr_raw = r.slice_from(addr_start).to_vec();

    let amount = r.read_uint()?;

    // Parse address from raw CBOR bytes.
    let address = Address::from_bytes(&addr_raw)
        .map_err(|e| SerializationError::InvalidData(format!("byron output address: {e}")))?;

    Ok(TransactionOutput {
        address,
        value: Value::lovelace(amount),
        datum: OutputDatum::None,
        script_ref: None,
        is_legacy: true,
        raw_cbor: None,
    })
}

/// Decode a single Byron transaction from its raw CBOR bytes.
///
/// The raw bytes are from the `KeepRaw<Tx>` captured via `TxPayload.transaction`.
/// The tx hash is `blake2b_256(raw_tx_cbor)`.
///
/// Byron Tx format: `[inputs, outputs, attributes]`
fn decode_byron_tx(
    raw_tx_cbor: &[u8],
    _raw_witness_cbor: &[u8],
) -> Result<Transaction, SerializationError> {
    let tx_hash = blake2b_256(raw_tx_cbor);
    let mut r = Reader::new(raw_tx_cbor);

    // [inputs, outputs, attributes]
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(3) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "byron tx: expected array(3), got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(
                "byron tx: expected definite-length array(3)".into(),
            ));
        }
    }

    // Inputs: definite or indefinite array of tx_in
    let inputs = r.read_array(read_byron_tx_input)?;

    // Outputs: definite or indefinite array of tx_out
    let outputs: Vec<TransactionOutput> = r.read_array(read_byron_tx_output)?;

    // Attributes: map (usually empty)
    r.skip()?;

    let body = TransactionBody {
        inputs,
        outputs,
        fee: Lovelace(0), // Byron has no explicit fee field in the tx body
        ttl: None,
        certificates: Vec::new(),
        withdrawals: BTreeMap::new(),
        auxiliary_data_hash: None,
        validity_interval_start: None,
        mint: BTreeMap::new(),
        script_data_hash: None,
        collateral: Vec::new(),
        required_signers: Vec::new(),
        network_id: None,
        collateral_return: None,
        total_collateral: None,
        reference_inputs: Vec::new(),
        update: None,
        voting_procedures: BTreeMap::new(),
        proposal_procedures: Vec::new(),
        treasury_value: None,
        donation: None,
    };

    Ok(Transaction {
        hash: tx_hash,
        era: Era::Byron,
        body,
        witness_set: TransactionWitnessSet {
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
        },
        is_valid: true,
        auxiliary_data: None,
        raw_cbor: Some(raw_tx_cbor.to_vec()),
        raw_body_cbor: Some(raw_tx_cbor.to_vec()),
        raw_witness_cbor: None,
    })
}

// ============================================================================
// Byron main block decoder
// ============================================================================

/// Decode a Byron main-chain block from the inner CBOR (after envelope stripping).
///
/// `inner_cbor` is the raw bytes of the inner value returned by
/// `decode_block_envelope`, i.e. the CBOR starting at the `block` structure:
///   `block = [header, body, extra]`
///
/// `byron_epoch_length` controls slot computation — see module doc.
pub fn decode_byron_main_block(
    inner_cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    // block = [header, body, extra]
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(3)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block: expected array(3), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header (KeepRaw — we need its raw bytes for the hash)
    // -------------------------------------------------------------------------
    let header_start = r.position();
    // header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(5)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block header: expected array(5), got {hdr_arr:?}"
        )));
    }

    // field 0: protocol_magic (uint)
    let _protocol_magic = r.read_uint()?;

    // field 1: prev_hash (32 bytes)
    let prev_hash = read_hash32(&mut r)?;

    // field 2: body_proof (skip)
    r.skip()?;

    // field 3: consensus_data = [slot_id, issuer_pubkey, difficulty, block_sig]
    //   slot_id = [epoch, rel_slot]
    //   difficulty = [uint]
    let cons_arr = r.read_array_header()?;
    if !matches!(cons_arr, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block consensus_data: expected array(4), got {cons_arr:?}"
        )));
    }
    // slot_id = [epoch, rel_slot]
    let slot_id_arr = r.read_array_header()?;
    if !matches!(slot_id_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block slot_id: expected array(2), got {slot_id_arr:?}"
        )));
    }
    let epoch = r.read_uint()?;
    let rel_slot = r.read_uint()?;

    // issuer_pubkey (bytes — skip)
    r.skip()?;

    // difficulty = [uint]
    let diff_arr = r.read_array_header()?;
    if !matches!(diff_arr, Some(1)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block difficulty: expected array(1), got {diff_arr:?}"
        )));
    }
    let block_number = r.read_uint()?;

    // block_sig (skip — large structure)
    r.skip()?;

    // field 4: extra_data (skip)
    r.skip()?;

    let header_raw = r.slice_from(header_start);
    let header_hash = byron_main_header_hash(header_raw);

    // -------------------------------------------------------------------------
    // 2. Body — decode transactions
    // -------------------------------------------------------------------------
    // body = [tx_payload, ssc, dlg_payload, upd_payload]
    let body_arr = r.read_array_header()?;
    if !matches!(body_arr, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block body: expected array(4), got {body_arr:?}"
        )));
    }

    // tx_payload: array of TxPayload
    // TxPayload = [tx_KeepRaw, witness_KeepRaw]
    let transactions = r.read_array(|r| {
        // TxPayload = [tx, witness]
        let tp_arr = r.read_array_header()?;
        if !matches!(tp_arr, Some(2)) {
            return Err(SerializationError::CborDecode(format!(
                "byron tx_payload: expected array(2), got {tp_arr:?}"
            )));
        }
        // tx: KeepRaw<Tx> — raw CBOR bytes of the Tx struct
        let tx_start = r.position();
        r.skip()?;
        let raw_tx = r.slice_from(tx_start);
        // witness: KeepRaw<Witnesses> — skip for now
        let witness_start = r.position();
        r.skip()?;
        let raw_witness = r.slice_from(witness_start);
        decode_byron_tx(raw_tx, raw_witness)
    })?;

    // ssc, dlg_payload, upd_payload — skip
    r.skip()?;
    r.skip()?;
    r.skip()?;

    // -------------------------------------------------------------------------
    // 3. Extra (skip)
    // -------------------------------------------------------------------------
    r.skip()?;

    // -------------------------------------------------------------------------
    // Compute slot
    // -------------------------------------------------------------------------
    let slot = if byron_epoch_length > 0 {
        SlotNo(epoch * byron_epoch_length + rel_slot)
    } else {
        SlotNo(mainnet_absolute_slot(epoch, rel_slot))
    };

    let header = BlockHeader {
        header_hash,
        prev_hash,
        issuer_vkey: Vec::new(),
        vrf_vkey: Vec::new(),
        vrf_result: VrfOutput {
            output: Vec::new(),
            proof: Vec::new(),
        },
        block_number: BlockNo(block_number),
        slot,
        epoch_nonce: Hash32::ZERO,
        body_size: 0,
        body_hash: Hash32::ZERO,
        operational_cert: OperationalCert {
            hot_vkey: Vec::new(),
            sequence_number: 0,
            kes_period: 0,
            sigma: Vec::new(),
        },
        protocol_version: ProtocolVersion { major: 1, minor: 0 },
        kes_signature: Vec::new(),
        nonce_vrf_output: Vec::new(),
        nonce_vrf_proof: Vec::new(),
    };

    Ok(Block {
        header,
        transactions,
        era: Era::Byron,
        raw_cbor: None, // set by caller from full block CBOR
    })
}

// ============================================================================
// Byron EBB decoder
// ============================================================================

/// Decode a Byron epoch boundary block from the inner CBOR (after envelope stripping).
///
/// EBBs have no transactions and no Praos fields. They carry only a boundary
/// header with the epoch/difficulty and the previous hash.
///
/// `inner_cbor` is the CBOR of the `ebb = [header, body, extra]` structure.
pub fn decode_byron_ebb_block(
    inner_cbor: &[u8],
    byron_epoch_length: u64,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(inner_cbor);

    // ebb = [header, body, extra]
    let block_arr = r.read_array_header()?;
    if !matches!(block_arr, Some(3)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb: expected array(3), got {block_arr:?}"
        )));
    }

    // -------------------------------------------------------------------------
    // 1. Header
    // -------------------------------------------------------------------------
    let header_start = r.position();
    // header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(5)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb header: expected array(5), got {hdr_arr:?}"
        )));
    }

    // field 0: protocol_magic
    let _protocol_magic = r.read_uint()?;

    // field 1: prev_hash (32 bytes)
    let prev_hash = read_hash32(&mut r)?;

    // field 2: body_proof (32-byte hash — skip)
    r.skip()?;

    // field 3: consensus_data = [[epoch_id], difficulty]
    //   epoch_id is a 1-element array [uint]
    //   difficulty = [uint]
    let cons_arr = r.read_array_header()?;
    if !matches!(cons_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb consensus_data: expected array(2), got {cons_arr:?}"
        )));
    }
    // epoch_id = [uint]
    let epoch_arr = r.read_array_header()?;
    if !matches!(epoch_arr, Some(1)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb epoch_id: expected array(1), got {epoch_arr:?}"
        )));
    }
    let epoch = r.read_uint()?;

    // difficulty = [uint]
    let diff_arr = r.read_array_header()?;
    if !matches!(diff_arr, Some(1)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb difficulty: expected array(1), got {diff_arr:?}"
        )));
    }
    let block_number = r.read_uint()?;

    // field 4: extra_data (skip)
    r.skip()?;

    let header_raw = r.slice_from(header_start);
    let header_hash = byron_ebb_header_hash(header_raw);

    // -------------------------------------------------------------------------
    // 2. Body — EBBs have stakeholder IDs only, no transactions
    // -------------------------------------------------------------------------
    r.skip()?;

    // -------------------------------------------------------------------------
    // 3. Extra (skip)
    // -------------------------------------------------------------------------
    r.skip()?;

    // EBBs use slot 0 of their epoch.
    let slot = if byron_epoch_length > 0 {
        SlotNo(epoch * byron_epoch_length)
    } else {
        SlotNo(mainnet_absolute_slot(epoch, 0))
    };

    let header = BlockHeader {
        header_hash,
        prev_hash,
        issuer_vkey: Vec::new(),
        vrf_vkey: Vec::new(),
        vrf_result: VrfOutput {
            output: Vec::new(),
            proof: Vec::new(),
        },
        block_number: BlockNo(block_number),
        slot,
        epoch_nonce: Hash32::ZERO,
        body_size: 0,
        body_hash: Hash32::ZERO,
        operational_cert: OperationalCert {
            hot_vkey: Vec::new(),
            sequence_number: 0,
            kes_period: 0,
            sigma: Vec::new(),
        },
        protocol_version: ProtocolVersion { major: 1, minor: 0 },
        kes_signature: Vec::new(),
        nonce_vrf_output: Vec::new(),
        nonce_vrf_proof: Vec::new(),
    };

    Ok(Block {
        header,
        transactions: Vec::new(),
        era: Era::Byron,
        raw_cbor: None,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: encode a 2-element CBOR array  [a, b]
    // -----------------------------------------------------------------------
    fn cbor_arr2(a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut v = vec![0x82u8];
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v
    }

    fn cbor_arr3(a: &[u8], b: &[u8], c: &[u8]) -> Vec<u8> {
        let mut v = vec![0x83u8];
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v.extend_from_slice(c);
        v
    }

    fn cbor_arr4(a: &[u8], b: &[u8], c: &[u8], d: &[u8]) -> Vec<u8> {
        let mut v = vec![0x84u8];
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v.extend_from_slice(c);
        v.extend_from_slice(d);
        v
    }

    fn cbor_arr5(a: &[u8], b: &[u8], c: &[u8], d: &[u8], e: &[u8]) -> Vec<u8> {
        let mut v = vec![0x85u8];
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v.extend_from_slice(c);
        v.extend_from_slice(d);
        v.extend_from_slice(e);
        v
    }

    fn cbor_arr1(a: &[u8]) -> Vec<u8> {
        let mut v = vec![0x81u8];
        v.extend_from_slice(a);
        v
    }

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

    fn cbor_map0() -> Vec<u8> {
        vec![0xa0] // map(0) — empty map
    }

    fn cbor_indef_arr0() -> Vec<u8> {
        vec![0x9f, 0xff] // indefinite array []
    }

    /// Build a minimal Byron EBB block CBOR (inner, without the outer envelope).
    ///
    /// Structure:
    /// ```text
    /// [header, body, extra]
    /// header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
    /// consensus_data = [[epoch], difficulty=[block_no]]
    /// body = [] (empty stakeholder ids)
    /// extra = [] (empty attributes)
    /// ```
    fn make_ebb_inner(
        protocol_magic: u64,
        prev_hash: &[u8; 32],
        epoch: u64,
        block_no: u64,
    ) -> Vec<u8> {
        let pm = cbor_uint(protocol_magic);
        let prev = cbor_bytes(prev_hash);
        let body_proof = cbor_bytes(&[0u8; 32]);
        // consensus_data = [[epoch], [block_no]]
        let epoch_arr = cbor_arr1(&cbor_uint(epoch));
        let difficulty = cbor_arr1(&cbor_uint(block_no));
        let cons_data = cbor_arr2(&epoch_arr, &difficulty);
        // extra_data = ([],) — a 1-tuple of empty attributes
        // Simplify: use empty map as the attributes
        let extra_data = cbor_arr1(&cbor_map0());

        let header = cbor_arr5(&pm, &prev, &body_proof, &cons_data, &extra_data);

        // body = indef array of stakeholder IDs (empty)
        let body = cbor_indef_arr0();

        // extra = indef array of attributes (empty)
        let extra = cbor_indef_arr0();

        cbor_arr3(&header, &body, &extra)
    }

    /// Build a minimal Byron main block CBOR (inner, without the outer envelope).
    fn make_main_inner(
        protocol_magic: u64,
        prev_hash: &[u8; 32],
        epoch: u64,
        rel_slot: u64,
        block_no: u64,
    ) -> Vec<u8> {
        let pm = cbor_uint(protocol_magic);
        let prev = cbor_bytes(prev_hash);
        let body_proof = cbor_bytes(&[0u8; 32]); // just a placeholder 32-byte hash
                                                 // consensus_data = [slot_id, issuer_pubkey, difficulty, block_sig]
                                                 // slot_id = [epoch, rel_slot]
        let slot_id = cbor_arr2(&cbor_uint(epoch), &cbor_uint(rel_slot));
        let issuer = cbor_bytes(&[0u8; 32]);
        let difficulty = cbor_arr1(&cbor_uint(block_no));
        // block_sig — use a minimal placeholder: [0, [delegator, signature]]
        // where 0 = ProxySKLight discriminator
        // For our decoder we only read this to skip it
        let dlg_cert = cbor_arr5(
            &cbor_uint(0),           // epoch range start
            &cbor_uint(0),           // epoch range end
            &cbor_bytes(&[0u8; 32]), // issuer
            &cbor_bytes(&[0u8; 32]), // delegate
            &cbor_bytes(&[0u8; 64]), // certificate signature placeholder
        );
        // Use block_sig type 0 (GenesisSignature) — simplest: just skip
        // Actually we need: [0, [delegator, signature]] or [1, ...] etc.
        // Use a stub: [0, #6.24(bytes .cbor (...))] — too complex. Use bytes placeholder.
        // The decoder calls r.skip() on block_sig so any valid CBOR works.
        let block_sig = cbor_arr2(&cbor_uint(0), &cbor_bytes(&[0u8; 64]));
        let cons_data = cbor_arr4(&slot_id, &issuer, &difficulty, &block_sig);

        // extra_data = [block_version, software_version, attributes, extra_proof]
        let extra_data = cbor_arr4(
            &cbor_arr3(&cbor_uint(1), &cbor_uint(0), &cbor_uint(0)), // BVer
            &cbor_arr2(&cbor_bytes(b"cardano"), &cbor_uint(1)),      // SoftVer
            &cbor_map0(),                                            // attributes
            &cbor_bytes(&[0u8; 32]),                                 // extra_proof
        );

        let header = cbor_arr5(&pm, &prev, &body_proof, &cons_data, &extra_data);

        // body = [tx_payload, ssc, dlg_payload, upd_payload]
        // all empty:
        let tx_payload = cbor_indef_arr0();
        let ssc = cbor_uint(0); // placeholder — simplify to a uint
        let dlg_payload = cbor_indef_arr0();
        // upd_payload = [vote_list, maybe_proposal] — complex; use an empty 2-arr
        let upd_payload = cbor_arr2(&cbor_indef_arr0(), &cbor_map0());
        let body = cbor_arr4(&tx_payload, &ssc, &dlg_payload, &upd_payload);

        // extra = []
        let extra = cbor_indef_arr0();

        cbor_arr3(&header, &body, &extra)
    }

    // -----------------------------------------------------------------------
    // EBB tests
    // -----------------------------------------------------------------------

    #[test]
    fn ebb_empty_block_decodes() {
        let prev = [0xab; 32];
        let inner = make_ebb_inner(764824073, &prev, 5, 12345);
        let block = decode_byron_ebb_block(&inner, 0).unwrap();
        assert_eq!(block.era, Era::Byron);
        assert_eq!(block.transactions.len(), 0);
        assert_eq!(block.header.block_number.0, 12345);
    }

    #[test]
    fn ebb_slot_mainnet_formula() {
        // epoch=5, rel_slot=0 → absolute_slot = 5 * 21600 + 0 = 108000
        let prev = [0x00; 32];
        let inner = make_ebb_inner(764824073, &prev, 5, 0);
        let block = decode_byron_ebb_block(&inner, 0).unwrap();
        assert_eq!(block.header.slot.0, 5 * 21600);
    }

    #[test]
    fn ebb_slot_custom_epoch_length() {
        // epoch=3, byron_epoch_length=86400 → slot = 3 * 86400 = 259200
        let prev = [0x00; 32];
        let inner = make_ebb_inner(2, &prev, 3, 0);
        let block = decode_byron_ebb_block(&inner, 86400).unwrap();
        assert_eq!(block.header.slot.0, 3 * 86400);
    }

    #[test]
    fn ebb_prev_hash_preserved() {
        let prev = [0xde; 32];
        let inner = make_ebb_inner(764824073, &prev, 0, 0);
        let block = decode_byron_ebb_block(&inner, 0).unwrap();
        assert_eq!(block.header.prev_hash.as_bytes(), &prev);
    }

    #[test]
    fn ebb_header_hash_is_deterministic() {
        let prev = [0x12; 32];
        let inner = make_ebb_inner(764824073, &prev, 7, 99);
        let block1 = decode_byron_ebb_block(&inner, 0).unwrap();
        let block2 = decode_byron_ebb_block(&inner, 0).unwrap();
        assert_eq!(block1.header.header_hash, block2.header.header_hash);
    }

    #[test]
    fn ebb_malformed_header_rejected() {
        // array(2) instead of array(5) for header
        let data: Vec<u8> = vec![
            0x83, // outer array(3)
            0x82, // header array(2) — wrong length
            0x00, 0x58, 0x20,
        ];
        let result = decode_byron_ebb_block(&data, 0);
        assert!(
            result.is_err(),
            "expected error for malformed header, got {:?}",
            result.ok()
        );
    }

    #[test]
    fn ebb_malformed_body_not_panic() {
        // Give an outer array(2) instead of array(3) — should fail
        let data = vec![0x82, 0x00, 0x00];
        let result = decode_byron_ebb_block(&data, 0);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Main block tests
    // -----------------------------------------------------------------------

    #[test]
    fn main_block_empty_txs_decodes() {
        let prev = [0xcd; 32];
        let inner = make_main_inner(764824073, &prev, 2, 150, 42000);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        assert_eq!(block.era, Era::Byron);
        assert_eq!(block.transactions.len(), 0);
        assert_eq!(block.header.block_number.0, 42000);
    }

    #[test]
    fn main_block_slot_mainnet_formula() {
        // epoch=2, rel_slot=150 → 2*21600 + 150 = 43350
        let prev = [0x00; 32];
        let inner = make_main_inner(764824073, &prev, 2, 150, 0);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        assert_eq!(block.header.slot.0, 2 * 21600 + 150);
    }

    #[test]
    fn main_block_slot_custom_epoch_length() {
        // epoch=1, rel_slot=100, byron_epoch_length=86400 → 86500
        let prev = [0x00; 32];
        let inner = make_main_inner(2, &prev, 1, 100, 0);
        let block = decode_byron_main_block(&inner, 86400).unwrap();
        assert_eq!(block.header.slot.0, 86400 + 100);
    }

    #[test]
    fn main_block_prev_hash() {
        let prev = [0xff; 32];
        let inner = make_main_inner(764824073, &prev, 0, 0, 0);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        assert_eq!(block.header.prev_hash.as_bytes(), &prev);
    }

    #[test]
    fn main_block_header_hash_is_deterministic() {
        let prev = [0x55; 32];
        let inner = make_main_inner(764824073, &prev, 10, 50, 9999);
        let b1 = decode_byron_main_block(&inner, 0).unwrap();
        let b2 = decode_byron_main_block(&inner, 0).unwrap();
        assert_eq!(b1.header.header_hash, b2.header.header_hash);
    }

    #[test]
    fn main_block_malformed_outer_rejected() {
        // array(2) outer instead of array(3)
        let data = vec![0x82, 0x00, 0x00];
        let result = decode_byron_main_block(&data, 0);
        assert!(result.is_err());
    }

    #[test]
    fn main_block_vrf_fields_empty() {
        let prev = [0x00; 32];
        let inner = make_main_inner(764824073, &prev, 0, 0, 0);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        assert!(block.header.vrf_result.output.is_empty());
        assert!(block.header.vrf_result.proof.is_empty());
        assert!(block.header.nonce_vrf_output.is_empty());
        assert!(block.header.kes_signature.is_empty());
    }

    #[test]
    fn main_block_protocol_version_is_byron() {
        let prev = [0x00; 32];
        let inner = make_main_inner(764824073, &prev, 0, 0, 0);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        assert_eq!(block.header.protocol_version.major, 1);
        assert_eq!(block.header.protocol_version.minor, 0);
    }

    // -----------------------------------------------------------------------
    // Header hash prefix tests
    // -----------------------------------------------------------------------

    #[test]
    fn main_and_ebb_header_hashes_differ() {
        // Same raw header bytes → different hashes due to type prefix
        let raw = [0xaa; 32];
        let main_hash = byron_main_header_hash(&raw);
        let ebb_hash = byron_ebb_header_hash(&raw);
        assert_ne!(
            main_hash, ebb_hash,
            "main and EBB hash prefixes must differ"
        );
    }

    #[test]
    fn main_header_hash_prefix_is_1() {
        // The first two bytes of the hashed data must be [0x82, 0x01]
        let raw = b"test";
        // We know the prefix is [0x82, 0x01, ...raw...]
        let mut expected_input = vec![0x82u8, 0x01];
        expected_input.extend_from_slice(raw);
        let expected = blake2b_256(&expected_input);
        assert_eq!(byron_main_header_hash(raw), expected);
    }

    #[test]
    fn ebb_header_hash_prefix_is_0() {
        let raw = b"test";
        let mut expected_input = vec![0x82u8, 0x00];
        expected_input.extend_from_slice(raw);
        let expected = blake2b_256(&expected_input);
        assert_eq!(byron_ebb_header_hash(raw), expected);
    }
}
