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
//! This matches `OriginalHash for KeepRaw<'_, byron::BlockHead>` in the in-house decoder which
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
use dugite_primitives::block::{
    Block, BlockHeader, ByronBlockAux, ByronDlgCert, ByronParamsUpdate, ByronUpdProposal,
    ByronUpdVote, OperationalCert, ProtocolVersion, VrfOutput,
};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet,
};
use dugite_primitives::value::{Lovelace, Value};
use num_traits::ToPrimitive;
use std::collections::BTreeMap;

// ============================================================================
// Mainnet Byron genesis constants (from the in-house decoder GenesisValues::mainnet())
// ============================================================================

/// Mainnet Byron epoch length in slots.
const MAINNET_BYRON_EPOCH_LENGTH: u64 = 432_000;

/// Mainnet Byron slot length in seconds.
const MAINNET_BYRON_SLOT_LENGTH: u64 = 20;

/// Compute absolute slot from (epoch, rel_slot) using the mainnet formula.
///
/// Matches `compute_absolute_slot_within_era(epoch, slot, 432000, 20)`:
/// `(epoch * 432000) / 20 + slot = epoch * 21600 + slot`.
///
/// Returns `u64::MAX` on overflow rather than panicking under debug overflow
/// checks — an adversarial Byron block can encode `epoch = u64::MAX` and
/// would otherwise abort the process. Real-world Byron epoch numbers are
/// bounded by a few hundred so saturation cannot affect production decoding.
#[inline]
fn mainnet_absolute_slot(epoch: u64, rel_slot: u64) -> u64 {
    epoch
        .saturating_mul(MAINNET_BYRON_EPOCH_LENGTH / MAINNET_BYRON_SLOT_LENGTH)
        .saturating_add(rel_slot)
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
/// - `0x19 0x00 0x01` — uint 1 (legacy decoder uses u16 encoding: major 1, 2-byte extra)
///
/// Wait: minicbor encodes where `(1u16, data)` encodes `1` as
/// u16 → CBOR major type 0 (unsigned int) with value 1. The minimal CBOR for
/// uint 1 is just `0x01`. Let's use that.
///
/// Actually `hash_cbor(&(1, self))` where `1` is a Rust integer
/// literal — minicbor will encode it as the smallest uint, which is `0x01`.
/// So the encoding is `[0x82, 0x01, <raw_header_bytes>]`.
fn byron_main_header_hash(raw_header_cbor: &[u8]) -> Hash32 {
    // Build: array(2) [uint(1), bstr(raw_header_cbor)]
    // But wait — the in-house KeepRaw<BlockHead> encodes itself as its raw bytes
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
        sub_transactions: Vec::new(),
        account_balance_intervals: Vec::new(),
        direct_deposits: BTreeMap::new(),
        guards: Vec::new(), // Dijkstra+
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
// Byron delegation + update payload decode (#1084)
// ============================================================================
//
// Grounded in `cddl-spec/byron.cddl` (`cardano-ledger-byron` 1.2.0.0) — see
// `docs/superpowers/specs/2026-08-20-byron-delegation-update-state-design.md`
// §2.6 for the exact wire shapes reproduced below.

/// Read a Byron `[? x]` — a 0- or 1-element array encoding `Maybe x`.
///
/// Haskell's `Bi` instances for `Maybe` use `encodeListLen`, which is always
/// DEFINITE-length; this reader is therefore definite-only, matching every
/// other Byron structural reader in this file (`array(N)`, never
/// `array(indef)`, for a fixed-arity record).
fn read_byron_maybe<T>(
    r: &mut Reader<'_>,
    f: impl FnOnce(&mut Reader<'_>) -> Result<T, SerializationError>,
) -> Result<Option<T>, SerializationError> {
    match r.read_array_header()? {
        Some(0) => Ok(None),
        Some(1) => Ok(Some(f(r)?)),
        other => Err(SerializationError::CborDecode(format!(
            "byron maybe: expected array(0) or array(1), got {other:?}"
        ))),
    }
}

/// Read a `bigint` CDDL field (plain uint or CBOR bignum tag) as a `u64`,
/// hard-erroring above `u64::MAX` rather than truncating — a silent
/// truncation on a consensus-adjacent size limit is #952's shape.
fn read_bigint_u64(r: &mut Reader<'_>) -> Result<u64, SerializationError> {
    r.read_bigint()?.to_u64().ok_or_else(|| {
        SerializationError::CborDecode("byron bvermod: value exceeds u64::MAX".into())
    })
}

/// Read `bver = [u16, u16, u8]` (major, minor, alt protocol version).
fn read_bver(r: &mut Reader<'_>) -> Result<(u16, u16, u8), SerializationError> {
    let n = r.read_array_header()?;
    if !matches!(n, Some(3)) {
        return Err(SerializationError::CborDecode(format!(
            "byron bver: expected array(3), got {n:?}"
        )));
    }
    let major = r.read_uint()?;
    let minor = r.read_uint()?;
    let alt = r.read_uint()?;
    let major = u16::try_from(major)
        .map_err(|_| SerializationError::CborDecode("byron bver: major exceeds u16".into()))?;
    let minor = u16::try_from(minor)
        .map_err(|_| SerializationError::CborDecode("byron bver: minor exceeds u16".into()))?;
    let alt = u8::try_from(alt)
        .map_err(|_| SerializationError::CborDecode("byron bver: alt exceeds u8".into()))?;
    Ok((major, minor, alt))
}

/// Read one `dlg` element:
/// `[epoch: u64, issuer: pubkey, delegate: pubkey, certificate: signature]`.
fn read_byron_dlg_cert(r: &mut Reader<'_>) -> Result<ByronDlgCert, SerializationError> {
    let n = r.read_array_header()?;
    if !matches!(n, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "byron dlg cert: expected array(4), got {n:?}"
        )));
    }
    let epoch = r.read_uint()?;
    let issuer_vk = r.read_bytes_owned()?;
    let delegate_vk = r.read_bytes_owned()?;
    let signature = r.read_bytes_owned()?;
    Ok(ByronDlgCert {
        epoch,
        issuer_vk,
        delegate_vk,
        signature,
    })
}

/// Read `dlgPayload = [* dlg]`.
fn read_byron_dlg_payload(r: &mut Reader<'_>) -> Result<Vec<ByronDlgCert>, SerializationError> {
    r.read_array(read_byron_dlg_cert)
}

/// Read `blockSig = [2, [dlg, signature]]` (`Header.hs:648-701`,
/// `ABlockSignature`/`DecCBOR (ABlockSignature ByteSpan)`).
///
/// Tag 2 (heavyweight-delegation-backed signature) is the ONLY variant the
/// pinned decoder accepts: `EncCBOR BlockSignature` unconditionally emits
/// `[2, ...]` ("Tag 0 was previously used for BlockSignature (no
/// delegation)... Tag 1 was previously used for BlockPSignatureLight" — both
/// dead), and `DecCBOR` hard-rejects any other tag with
/// `DecoderErrorUnknownTag`. No conforming implementation can have ever
/// produced anything else, so this decoder does the same: an unknown tag is
/// a decode error, not a lenient skip.
///
/// Returns the embedded certificate's DELEGATE key
/// (`Delegation.delegateVK cert`) — this is upstream's
/// `headerIssuer`/`blockIssuer` (`Header.hs:274-276`), NOT the raw
/// `issuer_pubkey` read from consensus-data field 1. See
/// [`dugite_primitives::block::ByronBlockAux::delegate_pubkey`]'s doc for why
/// the two are different fields.
fn read_byron_block_sig(r: &mut Reader<'_>) -> Result<Vec<u8>, SerializationError> {
    let outer = r.read_array_header()?;
    if !matches!(outer, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron block_sig: expected array(2), got {outer:?}"
        )));
    }
    let tag = r.read_uint()?;
    if tag != 2 {
        return Err(SerializationError::CborDecode(format!(
            "byron block_sig: unknown tag {tag} (only the heavyweight-delegation \
             tag 2 has ever been emitted by a conforming encoder)"
        )));
    }
    let inner = r.read_array_header()?;
    if !matches!(inner, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron block_sig: expected inner array(2), got {inner:?}"
        )));
    }
    let cert = read_byron_dlg_cert(r)?;
    // signature :: Signature ToSign — the block's own signature, raw bytes.
    // Verification is a separate, deliberately out-of-scope gap (design doc
    // §3.6), matching the posture already taken for `dlg_certs[].signature`.
    let _signature = r.read_bytes_owned()?;
    Ok(cert.delegate_vk)
}

/// Read `bvermod` — the 14-field sparse protocol-parameter update record.
/// Every field is `[? x]`. See [`ByronParamsUpdate`]'s doc for why
/// `txFeePolicy` is captured raw rather than parsed.
fn read_byron_bvermod(r: &mut Reader<'_>) -> Result<ByronParamsUpdate, SerializationError> {
    let n = r.read_array_header()?;
    if !matches!(n, Some(14)) {
        return Err(SerializationError::CborDecode(format!(
            "byron bvermod: expected array(14), got {n:?}"
        )));
    }
    let script_version = read_byron_maybe(r, |r| {
        let v = r.read_uint()?;
        u16::try_from(v)
            .map_err(|_| SerializationError::CborDecode("byron scriptVersion exceeds u16".into()))
    })?;
    let slot_duration = read_byron_maybe(r, read_bigint_u64)?;
    let max_block_size = read_byron_maybe(r, read_bigint_u64)?;
    let max_header_size = read_byron_maybe(r, read_bigint_u64)?;
    let max_tx_size = read_byron_maybe(r, read_bigint_u64)?;
    let max_proposal_size = read_byron_maybe(r, read_bigint_u64)?;
    let mpc_thd = read_byron_maybe(r, |r| r.read_uint())?;
    let heavy_del_thd = read_byron_maybe(r, |r| r.read_uint())?;
    let update_vote_thd = read_byron_maybe(r, |r| r.read_uint())?;
    let update_proposal_thd = read_byron_maybe(r, |r| r.read_uint())?;
    let update_implicit = read_byron_maybe(r, |r| r.read_uint())?;
    let soft_fork_rule = read_byron_maybe(r, |r| {
        let n = r.read_array_header()?;
        if !matches!(n, Some(3)) {
            return Err(SerializationError::CborDecode(format!(
                "byron softForkRule: expected array(3), got {n:?}"
            )));
        }
        Ok((r.read_uint()?, r.read_uint()?, r.read_uint()?))
    })?;
    let tx_fee_policy = read_byron_maybe(r, |r| {
        let start = r.position();
        r.skip()?;
        Ok(r.slice_from(start).to_vec())
    })?;
    let unlock_stake_epoch = read_byron_maybe(r, |r| r.read_uint())?;
    Ok(ByronParamsUpdate {
        script_version,
        slot_duration,
        max_block_size,
        max_header_size,
        max_tx_size,
        max_proposal_size,
        mpc_thd,
        heavy_del_thd,
        update_vote_thd,
        update_proposal_thd,
        update_implicit,
        soft_fork_rule,
        tx_fee_policy,
        unlock_stake_epoch,
    })
}

/// Read `upprop = [bver, bvermod, softwareVersion, data, attributes, from, signature]`.
///
/// `up_id` is `blake2b_256` over this proposal's OWN raw CBOR span, captured
/// via the same start/end-position discipline `KeepRaw` uses elsewhere in
/// this decoder (`recoverUpId = hashDecoded`).
///
/// **Deviation from the design doc's `ByronUpdProposal` field list**: the
/// doc's §3.1 sketch omits the proposer key (`from`), but §2.4's own
/// registration rule requires it — `registerProposal` rejects a proposal
/// whose `from` key does not resolve (via `Delegation.memberR`) to a genesis
/// key. That check is unimplementable without capturing the field, so it is
/// captured here as `proposer_vk` even though the doc's struct sketch did
/// not list it.
fn read_byron_upprop(r: &mut Reader<'_>) -> Result<ByronUpdProposal, SerializationError> {
    let start = r.position();
    let n = r.read_array_header()?;
    if !matches!(n, Some(7)) {
        return Err(SerializationError::CborDecode(format!(
            "byron upprop: expected array(7), got {n:?}"
        )));
    }
    let protocol_version = read_bver(r)?;
    let params_update = read_byron_bvermod(r)?;
    // softwareVersion = [text, u32]
    let sv_arr = r.read_array_header()?;
    if !matches!(sv_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron upprop softwareVersion: expected array(2), got {sv_arr:?}"
        )));
    }
    let sv_name = r.read_str_owned()?;
    let sv_number = r.read_uint()?;
    let sv_number = u32::try_from(sv_number).map_err(|_| {
        SerializationError::CborDecode("byron softwareVersion number exceeds u32".into())
    })?;
    // data: { * text => [hash,hash,hash,hash] } — opaque, feeds only the raw
    // span the up_id hash is computed over.
    r.skip()?;
    // attributes — opaque, same reason.
    r.skip()?;
    // from: pubkey — the proposer's key (see doc comment above).
    let proposer_vk = r.read_bytes_owned()?;
    // signature — unverified (Byron signature verification is out of scope;
    // see the design doc §3.6).
    r.skip()?;

    let raw = r.slice_from(start);
    let up_id = blake2b_256(raw);
    let encoded_len = raw.len() as u64;

    Ok(ByronUpdProposal {
        up_id,
        encoded_len,
        protocol_version,
        params_update,
        software_version: (sv_name, sv_number),
        proposer_vk,
    })
}

/// Read one `upvote = [voter: pubkey, proposalId: updid, vote: bool, signature]`.
///
/// The `vote: bool` element is decoded and discarded — matching upstream,
/// whose pinned decoder does `void $ decCBOR @Bool` (negative voting does
/// not exist in Byron; see the design doc §2.6).
fn read_byron_upvote(r: &mut Reader<'_>) -> Result<ByronUpdVote, SerializationError> {
    let n = r.read_array_header()?;
    if !matches!(n, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "byron upvote: expected array(4), got {n:?}"
        )));
    }
    let voter_vk = r.read_bytes_owned()?;
    let proposal_id = read_hash32(r)?;
    let _vote = r.read_bool()?;
    let signature = r.read_bytes_owned()?;
    Ok(ByronUpdVote {
        voter_vk,
        proposal_id,
        signature,
    })
}

/// Read `updPayload = [proposal: [? upprop], votes: [* upvote]]` — arity 2,
/// proposal FIRST (the wire order the design doc's §2.6 CDDL pins; a
/// synthetic test fixture in this file's own test module had this backwards
/// before #1084).
fn read_byron_upd_payload(
    r: &mut Reader<'_>,
) -> Result<(Option<ByronUpdProposal>, Vec<ByronUpdVote>), SerializationError> {
    let n = r.read_array_header()?;
    if !matches!(n, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron updPayload: expected array(2), got {n:?}"
        )));
    }
    let proposal = read_byron_maybe(r, read_byron_upprop)?;
    let votes = r.read_array(read_byron_upvote)?;
    Ok((proposal, votes))
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

    // issuer_pubkey — the block issuer's 64-byte extended verification key.
    // Captured for `ByronBlockAux` (#1084), which keys the per-block update
    // endorsement by it. NOT wired into `BlockHeader.issuer_vkey` — see the
    // design doc §3.1's hazard note: that field feeds the Shelley `bprev`
    // overlay-schedule read at the era seam and must stay empty for Byron.
    let issuer_pubkey = r.read_bytes_owned()?;

    // difficulty = [uint]
    let diff_arr = r.read_array_header()?;
    if !matches!(diff_arr, Some(1)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block difficulty: expected array(1), got {diff_arr:?}"
        )));
    }
    let block_number = r.read_uint()?;

    // block_sig = [2, [dlg_cert, signature]] — decoded (not skipped) as of
    // this fix. The embedded certificate's DELEGATE key is `headerIssuer`
    // (`Header.hs:274-276`), which `apply_update_payload` must hash for this
    // block's per-block update endorsement — NOT `issuer_pubkey` above.
    // Certificate/block signature verification itself remains a separate,
    // deliberately out-of-scope gap (design doc §3.6).
    let delegate_pubkey = read_byron_block_sig(&mut r)?;

    // field 4: extra_data = [blockVersion: bver, softwareVersion, attributes, extraProof]
    let ed_arr = r.read_array_header()?;
    if !matches!(ed_arr, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "byron main block extra_data: expected array(4), got {ed_arr:?}"
        )));
    }
    // blockVersion — the protocol version this block's issuer ENDORSES.
    let block_version = read_bver(&mut r)?;
    r.skip()?; // softwareVersion
    r.skip()?; // attributes
    r.skip()?; // extraProof

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

    // ssc — dead subsystem, no oracle output; stays skipped (design doc §3.6).
    r.skip()?;
    // dlgPayload / updPayload (#1084) — decoded into `ByronBlockAux`.
    let dlg_certs = read_byron_dlg_payload(&mut r)?;
    let (upd_proposal, upd_votes) = read_byron_upd_payload(&mut r)?;

    // -------------------------------------------------------------------------
    // 3. Extra (skip)
    // -------------------------------------------------------------------------
    r.skip()?;

    // -------------------------------------------------------------------------
    // Compute slot
    // -------------------------------------------------------------------------
    let slot = if byron_epoch_length > 0 {
        // Saturating arithmetic: an adversarial Byron block can encode
        // arbitrary u64 values for `epoch`/`rel_slot`, and the fuzz target
        // is built with debug overflow checks; plain `*` / `+` would abort
        // the process. Production Byron epoch numbers are bounded by a few
        // hundred so saturation never affects valid blocks.
        SlotNo(
            epoch
                .saturating_mul(byron_epoch_length)
                .saturating_add(rel_slot),
        )
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
        prev_nonce: None,
        raw_header_body: None, // Byron (BFT) — no Praos KES
    };

    let byron_aux = ByronBlockAux {
        protocol_version: block_version,
        issuer_pubkey,
        delegate_pubkey,
        dlg_certs,
        upd_proposal,
        upd_votes,
    };

    Ok(Block {
        header,
        transactions,
        era: Era::Byron,
        raw_cbor: None, // set by caller from full block CBOR
        byron: Some(Box::new(byron_aux)),
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

    // field 3: consensus_data = [epoch :: uint, difficulty :: [uint]]
    //
    // Matches Haskell `ABoundaryConsensusData` in
    // `cardano-ledger-byron/Cardano.Chain.Block.Boundary`:
    //   ABoundaryConsensusData { boundaryEpoch :: EpochNumber,
    //                            boundaryDifficulty :: ChainDifficulty }
    // `ChainDifficulty` is itself encoded as `array(1) [uint]`, so the on-wire
    // shape is `array(2) [uint(epoch), array(1)[uint(difficulty)]]`.
    //
    // Issue #613: this was previously decoded as `[array(1)[epoch], array(1)[difficulty]]`
    // which doesn't match the real wire and broke every preprod EBB.
    let cons_arr = r.read_array_header()?;
    if !matches!(cons_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "byron ebb consensus_data: expected array(2), got {cons_arr:?}"
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
        // Saturating: see the matching note in `decode_byron_main_block` —
        // adversarial inputs may encode huge `epoch` values that overflow
        // `u64::MAX` under debug arithmetic.
        SlotNo(epoch.saturating_mul(byron_epoch_length))
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
        prev_nonce: None,
        raw_header_body: None, // Byron (BFT) — no Praos KES
    };

    Ok(Block {
        header,
        transactions: Vec::new(),
        era: Era::Byron,
        raw_cbor: None,
        // EBBs have no body — no `dlgPayload`/`updPayload` to carry.
        byron: None,
    })
}

// ============================================================================
// Standalone tx decoder (Byron era)
// ============================================================================

/// Decode a standalone Byron transaction from raw CBOR bytes.
///
/// The standalone Byron tx format is `tag(30, bstr(cbor([tx, witnesses])))`.
/// The outer tag(30) is the CBSE (CBOR Simple Encoding) wrapper. The inner CBOR is a
/// 2-element array `[tx_body, witnesses]` where `tx_body = [inputs, outputs, attributes]`.
///
/// The transaction hash is `blake2b_256(raw_tx_cbor)` — over the Tx struct bytes only,
/// not the full TxPayload.
pub(crate) fn decode_byron_tx_standalone(cbor: &[u8]) -> Result<Transaction, SerializationError> {
    let mut r = Reader::new(cbor);

    // The Byron standalone format is tag(30, bstr(cbor([tx, witnesses]))).
    // Read the tag (expect 30).
    let tag = r
        .read_tag()
        .map_err(|e| SerializationError::CborDecode(format!("byron tx: {e}")))?;
    if tag != 30 {
        return Err(SerializationError::CborDecode(format!(
            "byron tx: expected tag(30), got tag({tag})"
        )));
    }

    // Read the embedded bytes.
    let inner_bytes: Vec<u8> = r
        .read_bytes()
        .map_err(|e| SerializationError::CborDecode(format!("byron tx bstr: {e}")))?
        .to_vec();

    // Decode the inner [tx, witnesses] 2-element array.
    let mut inner_r = Reader::new(&inner_bytes);
    let payload_arr = inner_r.read_array_header()?;
    match payload_arr {
        Some(2) => {}
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "byron tx payload: expected array(2), got {payload_arr:?}"
            )));
        }
    }

    // tx: capture raw bytes (this is the KeepRaw<Tx> in the in-house decoder)
    let tx_start = inner_r.position();
    inner_r.skip()?;
    let raw_tx = inner_r.slice_from(tx_start).to_vec();

    // witnesses: skip (we don't use them for now)
    let witness_start = inner_r.position();
    inner_r.skip()?;
    let raw_witness = inner_r.slice_from(witness_start).to_vec();

    decode_byron_tx(&raw_tx, &raw_witness)
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

    fn cbor_arr0() -> Vec<u8> {
        vec![0x80u8] // definite-length array(0)
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

    /// CBOR text string (major type 3), definite length.
    fn cbor_text(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        assert!(
            b.len() <= 23,
            "cbor_text helper only supports short strings"
        );
        let mut v = vec![0x60 | b.len() as u8];
        v.extend_from_slice(b);
        v
    }

    fn cbor_map0() -> Vec<u8> {
        vec![0xa0] // map(0) — empty map
    }

    fn cbor_indef_arr0() -> Vec<u8> {
        vec![0x9f, 0xff] // indefinite array []
    }

    /// Build a minimal Byron EBB block CBOR (inner, without the outer envelope).
    ///
    /// Structure (post-issue #613 fix — matches Haskell wire):
    /// ```text
    /// [header, body, extra]
    /// header = [protocol_magic, prev_hash, body_proof, consensus_data, extra_data]
    /// consensus_data = [epoch :: uint, difficulty=[block_no]]
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
        // consensus_data = [epoch, [block_no]]
        let epoch_arr = cbor_uint(epoch);
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
        // block_sig = [2, [dlg_cert, signature]] (`ABlockSignature`,
        // Header.hs:648-701) — tag 2 is the only variant a conforming
        // encoder ever emits; see `read_byron_block_sig`'s doc.
        let block_sig_cert = cbor_dlg_cert(0, &[0u8; 64], &[0u8; 64], &[0u8; 64]);
        let block_sig = cbor_arr2(
            &cbor_uint(2),
            &cbor_arr2(&block_sig_cert, &cbor_bytes(&[0u8; 64])),
        );
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
        // upd_payload = [proposal: [? upprop], votes: [* upvote]] — proposal
        // FIRST (design doc §2.6's CDDL). This fixture had the element order
        // reversed and the wrong shapes (`[votes, map0]`) before #1084; both
        // elements are now the CORRECT empty forms: `array(0)` for "no
        // proposal" (`[? upprop]` is definite-length, never indefinite — see
        // `read_byron_maybe`) and an empty array for "no votes".
        let upd_payload = cbor_arr2(&cbor_arr0(), &cbor_indef_arr0());
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

    // ── decode_byron_tx error paths ───────────────────────────────────────

    #[test]
    fn decode_byron_tx_rejects_wrong_arity() {
        // array(2) instead of array(3)
        let tx_cbor = cbor_arr2(&cbor_indef_arr0(), &cbor_indef_arr0());
        let err = decode_byron_tx(&tx_cbor, &[]).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode");
        };
        assert!(msg.contains("array(2)"));
    }

    #[test]
    fn decode_byron_tx_rejects_indefinite_outer() {
        // array(indef) for the tx body — must be definite array(3).
        let tx_cbor = vec![0x9f, 0xff];
        assert!(decode_byron_tx(&tx_cbor, &[]).is_err());
    }

    #[test]
    fn decode_byron_tx_empty_inputs_outputs_decodes() {
        // [[], [], {}]
        let tx_cbor = cbor_arr3(&cbor_indef_arr0(), &cbor_indef_arr0(), &cbor_map0());
        let tx = decode_byron_tx(&tx_cbor, &[]).unwrap();
        assert_eq!(tx.era, Era::Byron);
        assert_eq!(tx.body.inputs.len(), 0);
        assert_eq!(tx.body.outputs.len(), 0);
        assert!(tx.is_valid);
    }

    // ── read_byron_tx_input error paths ───────────────────────────────────

    #[test]
    fn read_byron_tx_input_rejects_unknown_disc() {
        // [5, ...] — disc must be 0
        let mut data = vec![0x82];
        data.extend(cbor_uint(5));
        data.extend(cbor_bytes(&[0; 4])); // some bytes
        let mut r = Reader::new(&data);
        let err = read_byron_tx_input(&mut r).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode");
        };
        assert!(msg.contains("unknown discriminator"));
    }

    #[test]
    fn read_byron_tx_input_rejects_wrong_outer_arity() {
        // array(3) instead of array(2)
        let mut data = vec![0x83];
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(0));
        let mut r = Reader::new(&data);
        assert!(read_byron_tx_input(&mut r).is_err());
    }

    #[test]
    fn read_byron_tx_input_decodes_pubkey_form() {
        // [0, tag(24) bstr( [txhash(32), txix] )]
        let mut inner = vec![0x82];
        inner.extend(cbor_bytes(&[0xAB; 32]));
        inner.extend(cbor_uint(7));
        let mut data = vec![0x82];
        data.extend(cbor_uint(0)); // PubKey disc
        data.push(0xd8);
        data.push(0x18); // tag(24)
        data.extend(cbor_bytes(&inner));
        let mut r = Reader::new(&data);
        let input = read_byron_tx_input(&mut r).unwrap();
        assert_eq!(input.transaction_id.as_bytes(), &[0xAB; 32]);
        assert_eq!(input.index, 7);
    }

    #[test]
    fn read_byron_tx_input_inner_wrong_arity() {
        // [0, tag(24) bstr( array(3) [...] )] — inner must be array(2)
        let mut inner = vec![0x83];
        inner.extend(cbor_uint(0));
        inner.extend(cbor_uint(0));
        inner.extend(cbor_uint(0));
        let mut data = vec![0x82];
        data.extend(cbor_uint(0));
        data.push(0xd8);
        data.push(0x18);
        data.extend(cbor_bytes(&inner));
        let mut r = Reader::new(&data);
        assert!(read_byron_tx_input(&mut r).is_err());
    }

    // ── read_byron_tx_output error paths ──────────────────────────────────

    #[test]
    fn read_byron_tx_output_rejects_wrong_arity() {
        // array(3) instead of array(2)
        let mut data = vec![0x83];
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(0));
        let mut r = Reader::new(&data);
        assert!(read_byron_tx_output(&mut r).is_err());
    }

    // ── decode_byron_tx_standalone ────────────────────────────────────────

    #[test]
    fn decode_byron_tx_standalone_rejects_non_tag() {
        // Plain array, no tag
        let data = [0x82u8, 0xa0, 0xa0];
        assert!(decode_byron_tx_standalone(&data).is_err());
    }

    #[test]
    fn decode_byron_tx_standalone_rejects_wrong_tag() {
        // tag(99) bstr — must be tag(30)
        let mut data = vec![0xd8, 0x63]; // tag(99)
        data.extend(cbor_bytes(&[0; 4]));
        let err = decode_byron_tx_standalone(&data).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode");
        };
        assert!(msg.contains("tag(30)"));
    }

    #[test]
    fn decode_byron_tx_standalone_rejects_inner_wrong_arity() {
        // tag(30) bstr( array(3) [...] ) — must be array(2)
        let inner = cbor_arr3(&cbor_uint(0), &cbor_uint(0), &cbor_uint(0));
        let mut data = vec![0xd8, 0x1e]; // tag(30)
        data.extend(cbor_bytes(&inner));
        let err = decode_byron_tx_standalone(&data).unwrap_err();
        let SerializationError::CborDecode(msg) = err else {
            panic!("expected CborDecode");
        };
        assert!(msg.contains("array(2)"));
    }

    #[test]
    fn decode_byron_tx_standalone_minimal_tx_decodes() {
        // tag(30) bstr( [ tx=[[],[],{}], witnesses=[] ] )
        let tx_body = cbor_arr3(&cbor_indef_arr0(), &cbor_indef_arr0(), &cbor_map0());
        let inner = cbor_arr2(&tx_body, &cbor_indef_arr0());
        let mut data = vec![0xd8, 0x1e];
        data.extend(cbor_bytes(&inner));
        let tx = decode_byron_tx_standalone(&data).unwrap();
        assert_eq!(tx.era, Era::Byron);
    }

    // -----------------------------------------------------------------------
    // dlgPayload / updPayload decode (#1084)
    // -----------------------------------------------------------------------

    /// `dlg = [epoch, issuer, delegate, certificate]`.
    fn cbor_dlg_cert(epoch: u64, issuer: &[u8], delegate: &[u8], sig: &[u8]) -> Vec<u8> {
        let mut v = vec![0x84u8];
        v.extend(cbor_uint(epoch));
        v.extend(cbor_bytes(issuer));
        v.extend(cbor_bytes(delegate));
        v.extend(cbor_bytes(sig));
        v
    }

    /// `bvermod` — all 14 fields `[]` (Nothing).
    fn cbor_empty_bvermod() -> Vec<u8> {
        let mut v = vec![0x8eu8]; // array(14)
        for _ in 0..14 {
            v.extend(cbor_arr0());
        }
        v
    }

    /// `bvermod` with ONLY `maxTxSize` set — the exact shape of the real
    /// mainnet epoch-16 proposal this design validated against.
    fn cbor_bvermod_max_tx_size(value: u64) -> Vec<u8> {
        let mut v = vec![0x8eu8]; // array(14)
        v.extend(cbor_arr0()); // scriptVersion
        v.extend(cbor_arr0()); // slotDuration
        v.extend(cbor_arr0()); // maxBlockSize
        v.extend(cbor_arr0()); // maxHeaderSize
        v.extend(cbor_arr1(&cbor_uint(value))); // maxTxSize = Just value
        for _ in 0..9 {
            v.extend(cbor_arr0());
        }
        v
    }

    /// `upprop = [bver, bvermod, softwareVersion, data, attributes, from, signature]`.
    fn cbor_upprop(bver: (u16, u16, u8), bvermod: &[u8], from: &[u8]) -> Vec<u8> {
        let mut v = vec![0x87u8]; // array(7)
        v.extend(cbor_arr3(
            &cbor_uint(bver.0 as u64),
            &cbor_uint(bver.1 as u64),
            &cbor_uint(bver.2 as u64),
        ));
        v.extend(bvermod);
        v.extend(cbor_arr2(&cbor_text("cardano-sl"), &cbor_uint(1))); // softwareVersion
        v.extend(cbor_map0()); // data
        v.extend(cbor_map0()); // attributes
        v.extend(cbor_bytes(from)); // proposer pubkey
        v.extend(cbor_bytes(&[0u8; 64])); // signature (unverified)
        v
    }

    /// `upvote = [voter, proposalId, vote, signature]`.
    fn cbor_upvote(voter: &[u8], proposal_id: &[u8; 32]) -> Vec<u8> {
        let mut v = vec![0x84u8];
        v.extend(cbor_bytes(voter));
        v.extend(cbor_bytes(proposal_id));
        v.push(0xf5); // true
        v.extend(cbor_bytes(&[0u8; 64])); // signature
        v
    }

    /// A main block whose `dlgPayload` carries one certificate and whose
    /// `updPayload` carries one proposal (`maxTxSize -> 65536`, matching the
    /// real mainnet epoch-16 event's shape) plus two votes.
    fn make_main_inner_with_byron_aux(prev_hash: &[u8; 32]) -> Vec<u8> {
        let pm = cbor_uint(764824073);
        let prev = cbor_bytes(prev_hash);
        let body_proof = cbor_bytes(&[0u8; 32]);
        let slot_id = cbor_arr2(&cbor_uint(20), &cbor_uint(0));
        let issuer_pubkey = [0x11u8; 64];
        let issuer = cbor_bytes(&issuer_pubkey);
        let difficulty = cbor_arr1(&cbor_uint(50_000));
        // block_sig's embedded certificate's delegate key is DELIBERATELY
        // distinct from both `issuer_pubkey` above and `dlg_certs[0]`'s
        // delegate below, so a test reading `aux.delegate_pubkey` can only
        // pass if the decoder actually threads this specific field through
        // rather than any other key already in scope.
        let sig_delegate = [0x77u8; 64];
        let block_sig_cert = cbor_dlg_cert(0, &issuer_pubkey, &sig_delegate, &[0u8; 64]);
        let block_sig = cbor_arr2(
            &cbor_uint(2),
            &cbor_arr2(&block_sig_cert, &cbor_bytes(&[0u8; 64])),
        );
        let cons_data = cbor_arr4(&slot_id, &issuer, &difficulty, &block_sig);

        let extra_data = cbor_arr4(
            &cbor_arr3(&cbor_uint(0), &cbor_uint(1), &cbor_uint(0)), // blockVersion = 0.1.0
            &cbor_arr2(&cbor_text("cardano-sl"), &cbor_uint(1)),
            &cbor_map0(),
            &cbor_bytes(&[0u8; 32]),
        );
        let header = cbor_arr5(&pm, &prev, &body_proof, &cons_data, &extra_data);

        let tx_payload = cbor_indef_arr0();
        let ssc = cbor_uint(0);

        let dlg_issuer = [0x22u8; 64];
        let dlg_delegate = [0x33u8; 64];
        let dlg_payload = {
            let mut v = vec![0x81u8]; // array(1)
            v.extend(cbor_dlg_cert(0, &dlg_issuer, &dlg_delegate, &[0u8; 64]));
            v
        };

        let proposer = [0x44u8; 64];
        let bvermod = cbor_bvermod_max_tx_size(65_536);
        let upprop = cbor_upprop((0, 1, 0), &bvermod, &proposer);
        let voter_a = [0x55u8; 64];
        let voter_b = [0x66u8; 64];
        let fake_proposal_id = [0xEEu8; 32];
        let votes = {
            let mut v = vec![0x82u8]; // array(2)
            v.extend(cbor_upvote(&voter_a, &fake_proposal_id));
            v.extend(cbor_upvote(&voter_b, &fake_proposal_id));
            v
        };
        let upd_payload = cbor_arr2(&cbor_arr1(&upprop), &votes);

        let body = cbor_arr4(&tx_payload, &ssc, &dlg_payload, &upd_payload);
        let extra = cbor_indef_arr0();
        cbor_arr3(&header, &body, &extra)
    }

    #[test]
    fn main_block_decodes_dlg_and_upd_payload() {
        let prev = [0x77; 32];
        let inner = make_main_inner_with_byron_aux(&prev);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        let aux = block.byron.expect("main block must carry ByronBlockAux");

        assert_eq!(aux.protocol_version, (0, 1, 0));
        assert_eq!(aux.issuer_pubkey, vec![0x11u8; 64]);
        // `delegate_pubkey` comes from `block_sig`'s embedded certificate,
        // NOT from `issuer_pubkey` (0x11) and NOT from `dlgPayload`'s
        // certificate (0x33) — three different keys in this fixture, so
        // this can only pass if the decoder reads the right one.
        assert_eq!(aux.delegate_pubkey, vec![0x77u8; 64]);

        assert_eq!(aux.dlg_certs.len(), 1);
        assert_eq!(aux.dlg_certs[0].epoch, 0);
        assert_eq!(aux.dlg_certs[0].issuer_vk, vec![0x22u8; 64]);
        assert_eq!(aux.dlg_certs[0].delegate_vk, vec![0x33u8; 64]);

        let proposal = aux.upd_proposal.expect("proposal must decode");
        assert_eq!(proposal.protocol_version, (0, 1, 0));
        assert_eq!(proposal.params_update.max_tx_size, Some(65_536));
        assert_eq!(proposal.params_update.max_block_size, None);
        assert_eq!(proposal.proposer_vk, vec![0x44u8; 64]);
        assert_eq!(proposal.software_version, ("cardano-sl".to_string(), 1));

        assert_eq!(aux.upd_votes.len(), 2);
        assert_eq!(aux.upd_votes[0].voter_vk, vec![0x55u8; 64]);
        assert_eq!(aux.upd_votes[1].voter_vk, vec![0x66u8; 64]);
        assert_eq!(aux.upd_votes[0].proposal_id.as_bytes(), &[0xEEu8; 32]);
    }

    #[test]
    fn up_id_is_blake2b256_of_the_proposals_own_raw_cbor_span() {
        let prev = [0x88; 32];
        let inner = make_main_inner_with_byron_aux(&prev);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        let aux = block.byron.unwrap();
        let proposal = aux.upd_proposal.unwrap();

        // Recompute the raw upprop span independently and hash it — must
        // match `up_id` exactly (`recoverUpId = hashDecoded`).
        let bvermod = cbor_bvermod_max_tx_size(65_536);
        let upprop_raw = cbor_upprop((0, 1, 0), &bvermod, &[0x44u8; 64]);
        let expected = blake2b_256(&upprop_raw);
        assert_eq!(proposal.up_id, expected);
        assert_eq!(proposal.encoded_len, upprop_raw.len() as u64);
    }

    #[test]
    fn ebb_never_carries_byron_aux() {
        let prev = [0x99; 32];
        let inner = make_ebb_inner(764824073, &prev, 5, 12345);
        let block = decode_byron_ebb_block(&inner, 0).unwrap();
        assert!(block.byron.is_none());
    }

    #[test]
    fn main_block_with_empty_payloads_has_some_aux_but_no_proposal() {
        // `make_main_inner` (used by the other main-block tests) has empty
        // dlg/upd payloads; #1084 must still populate `Some(ByronBlockAux)`
        // for every main block (only EBBs are `None`), just with empty
        // vecs/None fields.
        let prev = [0x00; 32];
        let inner = make_main_inner(764824073, &prev, 2, 150, 42000);
        let block = decode_byron_main_block(&inner, 0).unwrap();
        let aux = block.byron.expect("main block always carries Some(aux)");
        assert!(aux.dlg_certs.is_empty());
        assert!(aux.upd_proposal.is_none());
        assert!(aux.upd_votes.is_empty());
    }

    #[test]
    fn bvermod_wrong_arity_rejected() {
        // array(13) instead of array(14).
        let mut bad = vec![0x8du8];
        for _ in 0..13 {
            bad.extend(cbor_arr0());
        }
        let mut r = Reader::new(&bad);
        assert!(read_byron_bvermod(&mut r).is_err());
    }

    #[test]
    fn upprop_with_all_empty_bvermod_decodes_to_default_params_update() {
        let bvermod = cbor_empty_bvermod();
        let upprop = cbor_upprop((1, 0, 0), &bvermod, &[0x00u8; 64]);
        let mut r = Reader::new(&upprop);
        let proposal = read_byron_upprop(&mut r).expect("must decode");
        assert!(proposal.params_update.is_empty());
    }

    #[test]
    fn byron_maybe_rejects_multi_element_array() {
        // array(2) is neither "Nothing" (0) nor "Just x" (1).
        let bad = cbor_arr2(&cbor_uint(1), &cbor_uint(2));
        let mut r = Reader::new(&bad);
        let result = read_byron_maybe(&mut r, |r| r.read_uint());
        assert!(result.is_err());
    }
}
