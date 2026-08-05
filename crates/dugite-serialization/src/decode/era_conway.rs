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
    ExUnitPrices, ExUnits, GovAction, GovActionId, NativeScript, OutputDatum, PlutusData,
    PoolParams, ProposalProcedure, ProtocolParamUpdate, Rational, Redeemer, RedeemerTag,
    Transaction, TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
    VKeyWitness, Vote, Voter, VotingProcedure,
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

/// Decode JUST the block header from the inner header CBOR (issue #654 —
/// eager per-peer header validation in the ChainSync receive loop).
///
/// Unlike [`decode_conway_block_minimal`], the input here is the bytes of
/// the header element ONLY (everything inside `tag24(bytes(...))` of the
/// HFC wrap for a `MsgRollForward` payload), not a full block CBOR. The
/// returned `BlockHeader.header_hash` is `blake2b_256(inner_cbor)` —
/// matching the canonical Cardano block hash.
///
/// Covers both Conway and Dijkstra eras; the only difference at the
/// header layer is Dijkstra's optional 11th header_body element
/// (`prevNonce`), which `decode_conway_header_inner` already handles.
pub fn decode_conway_block_header(inner_cbor: &[u8]) -> Result<BlockHeader, SerializationError> {
    let mut r = Reader::new(inner_cbor);
    let raw = KeepRaw::parse_with(&mut r, |r| decode_conway_header_inner(r, Era::Conway))?;
    let header_hash = blake2b_256(raw.raw);
    let mut h = raw.value;
    h.header_hash = header_hash;
    Ok(h)
}

/// Decode a CBOR-encoded `protocol_param_update` map into a
/// [`ProtocolParamUpdate`].
///
/// Accepts the map bytes directly (without a wrapping block or tx context).
/// Used for testing and for CLI `GetCurrentPParams` / `ParameterChange`
/// decoding where the map is extracted from an HFC-wrapped query result.
///
/// `era` must be `Era::Conway` or `Era::Dijkstra` — it selects which key set
/// is valid (issue #1013). Keys 0-33 are the Conway PParams; keys 34-37 are
/// Dijkstra-only additions:
/// - 34: `maxRefScriptSizePerBlock` (uint / Word32)
/// - 35: `maxRefScriptSizePerTx` (uint / Word32)
/// - 36: `refScriptCostStride` (uint / NonZero Word32)
/// - 37: `refScriptCostMultiplier` (tag-30 rational)
///
/// Dijkstra upstream actually defines SIX new keys (34-39: the above four
/// plus 38 `maxPledgeLeverage` and 39 `minPoolMargin` — oracle-verified,
/// `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/PParams.hs:490-572` @
/// `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`), but `ProtocolParamUpdate` has
/// no fields for 38/39 yet. Keys 38/39 are therefore rejected as unknown even
/// under `Era::Dijkstra` — a documented, fail-closed gap (Dijkstra is
/// unreleased) tracked separately from this decoder's unknown-key fix.
pub fn ppu_from_cbor(cbor: &[u8], era: Era) -> Result<ProtocolParamUpdate, SerializationError> {
    let mut r = Reader::new(cbor);
    read_protocol_param_update(&mut r, era)
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
        let raw = KeepRaw::parse_with(&mut r, |r| decode_conway_header_inner(r, era))?;
        let header_hash = blake2b_256(raw.raw);
        let mut h = raw.value;
        h.header_hash = header_hash;
        h
    };

    // -------------------------------------------------------------------------
    // 2. tx_bodies (definite OR indefinite-length array — mainnet uses both)
    // -------------------------------------------------------------------------
    let mut raw_bodies: Vec<Vec<u8>> = Vec::new();
    let mut parsed_bodies: Vec<TransactionBody> = Vec::new();
    r.for_each_array_item(|r| {
        let body = KeepRaw::parse_with(r, |r| decode_conway_tx_body(r, era))?;
        raw_bodies.push(body.raw.to_vec());
        parsed_bodies.push(body.value);
        Ok(())
    })?;

    // -------------------------------------------------------------------------
    // 3. tx_witness_sets (definite OR indefinite)
    // -------------------------------------------------------------------------
    let mut raw_witnesses: Vec<Vec<u8>> = Vec::new();
    let mut parsed_witnesses: Vec<Option<TransactionWitnessSet>> = Vec::new();
    r.for_each_array_item(|r| {
        if mode == DecodeMode::Full {
            let ws = KeepRaw::parse_with(r, |r| decode_conway_witness_set(r, era))?;
            raw_witnesses.push(ws.raw.to_vec());
            parsed_witnesses.push(Some(ws.value));
        } else {
            let ws_start = r.position();
            r.skip()?;
            raw_witnesses.push(r.slice_from(ws_start).to_vec());
            parsed_witnesses.push(None);
        }
        Ok(())
    })?;

    // -------------------------------------------------------------------------
    // 4. auxiliary_data_set
    // -------------------------------------------------------------------------
    let aux_map = decode_aux_data_map(&mut r, era)?;

    // -------------------------------------------------------------------------
    // 5. invalid_txs — list of tx indices that failed phase-1.
    //
    // Despite the field name, this is a plain CDDL LIST, not a set:
    // conway.cddl declares `invalid_transactions : [* transaction_index]`.
    // Conway reuses `AlonzoBlockBody` (Alonzo BlockBody/Internal.hs), which
    // decodes `isValIdxs :: [Int]` via plain `decodeList` and applies ONLY a
    // range-check (`unless (all inRange isValIdxs) $ fail …`) — there is NO
    // no-duplicate enforcement at any protocol version. Use lenient `read_set`
    // so duplicate indices (e.g. `[0, 0]`) decode the same as Haskell.
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

            // Reconstruct full wire-format tx CBOR for fee-size calculation.
            // Haskell toCBORForSizeComputation (Conway) = array(3)[body,wits,aux];
            // we build array(4) and fee_tx_size() subtracts the 1-byte is_valid.
            let raw_cbor = Some(
                crate::decode::era_babbage::reconstruct_alonzo_plus_tx_raw_cbor(
                    &raw_body,
                    raw_witness.as_deref().unwrap_or(&[0xA0]),
                    is_valid,
                    auxiliary_data.as_ref(),
                ),
            );

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

fn decode_conway_header_inner(
    r: &mut Reader<'_>,
    _era: Era,
) -> Result<BlockHeader, SerializationError> {
    // header = [header_body, kes_signature]
    let hdr_arr = r.read_array_header()?;
    if !matches!(hdr_arr, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "conway header: expected array(2), got {hdr_arr:?}"
        )));
    }

    // Conway header_body = array(10).
    // Dijkstra may add an optional 11th element: prevNonce (bytes(32) or null).
    // We accept array(10) or array(11) to handle both.
    // Capture raw bytes (the KES-signed message) — includes the 11th element.
    let body_start = r.position();
    let body_arr = r.read_array_header()?;
    let body_len = match body_arr {
        Some(n @ (10 | 11)) => n,
        _ => {
            return Err(SerializationError::CborDecode(format!(
                "conway/dijkstra header_body: expected array(10) or array(11), got {body_arr:?}"
            )));
        }
    };

    // 0: block_number
    let block_number = r.read_uint()?;
    // 1: slot
    let slot = r.read_uint()?;
    // 2: prev_hash (32-byte bytes or null)
    let prev_hash = read_optional_hash32(r)?;
    // 3: issuer_vkey (32 bytes)
    let issuer_vkey = r.read_bytes_owned()?;
    // 4: vrf_vkey (32 bytes)
    let vrf_vkey = r.read_bytes_owned()?;
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

    // 10 (optional, Dijkstra only): prevNonce — bytes(32) or null.
    //
    // `prevNonceBlockHeaderL` in `Cardano.Ledger.Dijkstra.Era` is used by the
    // BBODY rule to validate Peras certificates. The nonce is either a
    // 32-byte Blake2b-256 hash or CBOR null when no previous epoch nonce is
    // available (e.g. first block of the epoch or bootstrap).
    //
    // We read the 11th element when present; both absent and CBOR-null map to
    // `None` in our `prev_nonce: Option<Hash32>` field.
    let prev_nonce: Option<Hash32> = if body_len == 11 {
        read_optional_hash32_nullable(r)?
    } else {
        None
    };

    let raw_header_body = r.slice_from(body_start).to_vec();

    // KES signature (second element of outer array)
    let kes_signature = r.read_bytes_owned()?;

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
        // Babbage/Conway/Dijkstra Praos: nonce_vrf_output = blake2b_256("N" || vrf_result.output)
        // This matches the `vrfNonceValue` computation in the Haskell Praos era.
        nonce_vrf_output: {
            let mut nonce_input = Vec::with_capacity(1 + vrf_output.len());
            nonce_input.push(b'N');
            nonce_input.extend_from_slice(&vrf_output);
            blake2b_256(&nonce_input).to_vec()
        },
        nonce_vrf_proof: Vec::new(), // Praos has no separate nonce proof
        prev_nonce,
        raw_header_body: Some(raw_header_body),
    })
}

/// Read an optional 32-byte hash that may be `null` on the wire.
///
/// Returns `None` when the CBOR value is `null`, `Some(hash)` when it is a
/// 32-byte byte string.  Used for Dijkstra `prevNonce` (field 10 of the
/// `header_body` array).
fn read_optional_hash32_nullable(r: &mut Reader<'_>) -> Result<Option<Hash32>, SerializationError> {
    use minicbor::data::Type;
    let ty = r.peek_major()?;
    if ty == Type::Null {
        r.read_null()?;
        Ok(None)
    } else {
        Ok(Some(read_hash32(r)?))
    }
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

fn read_operational_cert(r: &mut Reader<'_>) -> Result<OperationalCert, SerializationError> {
    // operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "operational_cert: expected array(4), got {arr_len:?}"
        )));
    }
    let hot_vkey = r.read_bytes_owned()?;
    let sequence_number = r.read_uint()?;
    let kes_period = r.read_uint()?;
    let sigma = r.read_bytes_owned()?;
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

pub(crate) fn decode_conway_tx_body(
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
    // Dijkstra TxBody key 14 — guards (issue #475 Phase 3.5).
    //
    // Conway:  key 14 = required_signers : set<addr_keyhash>
    // Dijkstra: key 14 = guards          : OSet (Credential Guard)
    //
    // The decoder accepts both wire shapes per upstream's `decodeGuards`:
    // - bare bstr(28)               -> Credential::VerificationKey (legacy)
    // - array(2) [type, hash28]     -> full Credential (Dijkstra)
    //
    // `required_signers` is still populated with the key-hash subset so all
    // Conway-era consumers (CLI tx builder, mempool, witness validator)
    // keep working unchanged. The full credential list is surfaced through
    // `TransactionBody.guards` for Dijkstra+ era validation.
    let mut guards: Vec<Credential> = Vec::new();
    let mut network_id: Option<u8> = None;
    let mut collateral_return: Option<TransactionOutput> = None;
    let mut total_collateral: Option<Lovelace> = None;
    let mut reference_inputs: Vec<TransactionInput> = Vec::new();
    let mut voting_procedures: BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>> =
        BTreeMap::new();
    let mut proposal_procedures: Vec<ProposalProcedure> = Vec::new();
    let mut treasury_value: Option<Lovelace> = None;
    let mut donation: Option<Lovelace> = None;
    // Dijkstra TxBody key 23 — OMap TxId (Tx SubTx era). Populated only
    // when a Dijkstra-shaped body carries the field. Conway bodies never
    // emit key 23 so this stays empty for them.
    let mut sub_transactions: Vec<dugite_primitives::transaction::SubTransaction> = Vec::new();
    // Dijkstra TxBody key 26 — account_balance_intervals (issue #475 Phase 3.3).
    // Map { stake_credential => AccountBalanceInterval }. Same nuance as
    // sub_transactions: only Dijkstra bodies emit it, but the same decoder
    // covers both eras so the field is parsed here and dispatched to the
    // (currently empty for Conway) `account_balance_intervals` field.
    let mut account_balance_intervals: Vec<(
        dugite_primitives::credentials::Credential,
        dugite_primitives::transaction::AccountBalanceInterval,
    )> = Vec::new();
    // Dijkstra TxBody key 25 — direct_deposits (issue #475 Phase 3.4).
    // Map { reward_account_bytes => coin }. Wire shape mirrors withdrawals
    // (key 5) exactly: a CBOR map keyed by 29-byte reward_account bstr.
    // Only Dijkstra+ bodies emit it; the shared Conway+Dijkstra decoder
    // parses it here and surfaces it through TransactionBody.direct_deposits.
    let mut direct_deposits: BTreeMap<Vec<u8>, Lovelace> = BTreeMap::new();

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
                inputs = r.read_set_strict(read_tx_input)?;
            }
            1 => {
                // outputs: [* transaction_output]
                outputs = r.read_array(|r| read_babbage_tx_output_with_raw(r))?;
            }
            2 => {
                fee = read_lovelace(r)?;
            }
            3 => {
                ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                // certificates: set<certificate>
                certificates = r.read_set_strict(|r| read_conway_certificate(r))?;
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
                collateral = r.read_set_strict(read_tx_input)?;
            }
            14 => {
                // Conway:  required_signers : set<addr_keyhash>
                // Dijkstra: guards          : OSet (Credential Guard)
                //
                // Per upstream `decodeGuards` (Cardano.Ledger.Dijkstra.TxBody),
                // each element may be either:
                //   - bare bstr(28)        => Credential::VerificationKey
                //   - array(2)[type, h28]  => full Credential
                // Both shapes coexist in the same set (the type peek is per
                // element, not per set). Required_signers stays populated with
                // the key-hash subset for Conway-shaped callers; the full
                // credential list is surfaced through `guards` for Dijkstra+
                // witness validation. Issue #475 Phase 3.5.
                let parsed: Vec<Credential> = r.read_set_strict(|r| {
                    let ty = r.peek_major()?;
                    match ty {
                        Type::Array | Type::ArrayIndef => read_stake_credential(r),
                        Type::Bytes | Type::BytesIndef => {
                            let h28 = read_hash28(r)?;
                            Ok(Credential::VerificationKey(h28))
                        }
                        other => Err(SerializationError::CborDecode(format!(
                            "guards (TxBody key 14): expected bstr or array, got {other:?}"
                        ))),
                    }
                })?;
                for cred in &parsed {
                    if let Credential::VerificationKey(h28) = cred {
                        required_signers.push(h28.to_hash32_padded());
                    }
                }
                guards = parsed;
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
                collateral_return = Some(read_babbage_tx_output_with_raw(r)?);
            }
            17 => {
                // total_collateral
                total_collateral = Some(read_lovelace(r)?);
            }
            18 => {
                // reference_inputs: set<transaction_input>
                reference_inputs = r.read_set_strict(read_tx_input)?;
            }
            19 => {
                // voting_procedures: { voter => { gov_action_id => voting_procedure } }
                voting_procedures = read_voting_procedures(r)?;
            }
            20 => {
                // proposal_procedures: OSet<proposal_procedure>
                //
                // Conway encodes this as an *ordered* set. The no-duplicate
                // count-check applies to any set/oset, and `read_set_strict`
                // preserves wire order (it only rejects duplicates, never
                // reorders), so the OSet ordering is unchanged.
                proposal_procedures = r.read_set_strict(|r| read_proposal_procedure(r, era))?;
            }
            21 => {
                // current_treasury_value
                treasury_value = Some(read_lovelace(r)?);
            }
            22 => {
                // donation
                donation = Some(read_lovelace(r)?);
            }
            23 if era == Era::Dijkstra => {
                // Dijkstra sub_transactions:
                //   OMap TxId (Tx SubTx era)  ==  CBOR map { tx_id => sub_tx_body }
                // Each sub_tx_body is itself a Dijkstra-SubTx-shaped CBOR map.
                // Conway bodyFields has NO key 23, so a Conway body carrying
                // this key falls through to the rejecting default below. The
                // Dijkstra-routed decoder reuses this function, so we parse
                // it here. See issue #475 Phase 3.1 / cardano-ledger
                // `eras/dijkstra/impl/.../TxBody.hs` (key 23 emitter).
                sub_transactions = decode_sub_transactions(r)?;
            }
            25 if era == Era::Dijkstra => {
                // Dijkstra direct_deposits (issue #475 Phase 3.4):
                //   { reward_account => coin }
                // Wire shape is identical to withdrawals (key 5) — a CBOR
                // map keyed by the 29-byte reward_account bstr. Conway rejects
                // key 25 (falls through to the default). See
                // `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxBody.hs`
                // (key 25 emitter) and `Rules.hs` (UTXOS rule integration).
                direct_deposits = read_withdrawals(r)?;
            }
            26 if era == Era::Dijkstra => {
                // Dijkstra account_balance_intervals (issue #475 Phase 3.3):
                //   { stake_credential => AccountBalanceInterval }
                // Wire shape per
                // `Cardano.Ledger.Dijkstra.Scripts.AccountBalanceInterval`:
                // each value is a CBOR `array(2)` of two `coin / null`
                // entries — `[lower, upper]` — with at least one non-null.
                // Conway rejects key 26 (falls through to the default).
                account_balance_intervals = decode_account_balance_intervals(r)?;
            }
            _ => {
                // Unknown/invalid tx-body key — HARD REJECT, per upstream
                // SparseKeyed bodyFields catch-all (Conway: invalidField ->
                // cborError; Dijkstra v12+: decoderByKey _ -> Nothing ->
                // failMsg). Each era only knows its OWN keys. Key 6 (pre-Conway
                // `update`) is NOT in Conway OR Dijkstra bodyFields and is hard-
                // rejected here. The 23/25/26 arms above are Dijkstra-guarded,
                // so for Conway those keys also reach this reject.
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
        collateral_return,
        total_collateral,
        reference_inputs,
        update: None, // Conway doesn't use pre-Conway update proposals
        voting_procedures,
        proposal_procedures,
        treasury_value,
        donation,
        sub_transactions,          // Dijkstra+ only; empty for Conway-shaped bodies
        account_balance_intervals, // Dijkstra+ only; empty for Conway-shaped bodies
        direct_deposits,           // Dijkstra+ only; empty for Conway-shaped bodies
        guards,                    // Dijkstra+ guards; for Conway-shaped bodies this
                                   // contains the key-hash-only subset (every entry
                                   // is a Credential::VerificationKey) so callers
                                   // can iterate uniformly.
    })
}

/// Decode the Dijkstra `sub_transactions` field (TxBody key 23).
///
/// Wire shape is the CBOR encoding of `OMap TxId (Tx SubTx era)` — a bare
/// ARRAY of values (Haskell `OMap` never encodes its keys; they are
/// reconstructed via `HasOKey.toOKey`, #936). Each VALUE is a
/// `DijkstraSubTx` — a plain 3-element record `[body, wits, auxData]`
/// (oracle-verified #1010, `Tx.hs` lines 88-115 + the general
/// `DecCBOR (Annotator (Tx l DijkstraEra))` instance, `decodeRecordNamed
/// "DijkstraSubTx" (const 3)`), **not** a bare body map — a sub-tx has its
/// own independent witness set (`dstWits`) and auxiliary data
/// (`dstAuxData`), just like a top-level `Tx`. This mirrors exactly how
/// the standalone top-level `[body, witness_set, (is_valid,) aux_data]`
/// array is decoded a few hundred lines above
/// (`decode_dijkstra_transaction`/`decode_conway_transaction`) — same
/// per-element shape (`KeepRaw` body, `decode_conway_witness_set`,
/// null-or-value aux data), just without the `is_valid` slot (sub-txs
/// never had one; CIP-0167 only affects the top-level envelope).
///
/// The sub-tx's TxId (`toOKey`) is the hash of its BODY bytes only —
/// never the whole 3-element record — matching Cardano's universal
/// TxId-is-body-hash-only convention (confirmed: sub-txs are "keyed in
/// the OMap by its own TxId, derived the normal way, from its own body
/// hash"). `libs/cardano-data/.../OMap/Strict.hs`'s
/// `decodeListLikeEnforceNoDuplicates` rejects a repeated reconstructed
/// key outright, mirrored here via `seen`.
fn decode_sub_transactions(
    r: &mut Reader<'_>,
) -> Result<Vec<dugite_primitives::transaction::SubTransaction>, SerializationError> {
    use dugite_primitives::transaction::SubTransaction;
    use std::collections::HashSet;

    let mut out: Vec<SubTransaction> = Vec::new();
    let mut seen: HashSet<Hash32> = HashSet::new();

    r.for_each_array_item(|r| {
        // DijkstraSubTx = [body, wits, auxData] — array(3), verbatim shape
        // of decodeRecordNamed "DijkstraSubTx" (const 3).
        let arr_len = r.read_array_header()?;
        if !matches!(arr_len, Some(3)) {
            return Err(SerializationError::CborDecode(format!(
                "Dijkstra sub-tx: expected array(3) [body, wits, auxData], got {arr_len:?}"
            )));
        }

        // 1. Body — capture raw bytes for TxId computation. This span is
        // exactly the body's own bytes (KeepRaw scopes to the inner
        // parser's consumption), never the wits/auxData that follow.
        let body_raw = KeepRaw::parse_with(r, decode_sub_tx_body)?;
        let mut sub = body_raw.value;
        sub.tx_id = blake2b_256(body_raw.raw);
        sub.raw_body_cbor = Some(body_raw.raw.to_vec());

        if !seen.insert(sub.tx_id) {
            return Err(SerializationError::CborDecode(format!(
                "sub_transactions: duplicate sub-transaction id {} (OMap forbids duplicates)",
                sub.tx_id.to_hex()
            )));
        }

        // 2. Witness set — the sub-tx's OWN, independent of the parent's.
        sub.witness_set = decode_conway_witness_set(r, Era::Dijkstra)?;

        // 3. Auxiliary data — StrictMaybe: null, or a value.
        sub.auxiliary_data = {
            let ty = r.peek_major()?;
            if ty == Type::Null {
                r.read_null()?;
                None
            } else {
                Some(decode_auxiliary_data(r, Era::Dijkstra)?)
            }
        };

        out.push(sub);
        Ok(())
    })?;

    Ok(out)
}

/// Decode a Dijkstra SubTx body (`DijkstraSubTxBodyRaw`).
///
/// Oracle-verified key table against `IntersectMBO/cardano-ledger` pinned
/// SHA `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (#1010) — see the doc
/// comment on `dugite_primitives::transaction::SubTransaction` for the
/// full table with upstream field names. Every key here reuses the EXACT
/// same decode helper the top-level Dijkstra/Conway tx body uses for the
/// identically-numbered field a few hundred lines above
/// (`decode_conway_tx_body`) — same wire shape, same key number, per the
/// oracle's direct comparison of both `encodeTxBodyRaw` clauses.
///
/// Key 24 (`required_top_level_guards` /
/// `dstbrRequiredTopLevelGuards`) is a Dijkstra-only concept with no
/// Conway analog and a wire shape (`encodeMap encCBOR
/// (encodeNullStrictMaybe encCBOR)`) not yet modelled on
/// `SubTransaction` — deliberately left unmodelled rather than guessed
/// at. It, and every other unknown key (including the GADT-excluded
/// TopTx-only keys 2/13/16/17/23/27 — a sub-tx has no fee/collateral/
/// nested-sub-tx fields at all, confirmed by the decoder's own required-key
/// list `SSubTx -> [(0,"inputs"),(1,"outputs")]`), HARD REJECTS, matching
/// this file's own established pattern for the top-level tx body's
/// unknown-key catch-all a few lines above (`{era:?} tx body:
/// unknown/invalid key {key}`) and upstream's `SparseKeyed` `decoderByKey
/// _ -> Nothing -> failMsg`. Before #1010 this arm silently `r.skip()`-ped
/// ANY key it didn't model — a sub-tx carrying certs/mint/withdrawals/
/// votes decoded SUCCESSFULLY with all of that discarded, then applied
/// with none of those effects: the parent OMap key's hash-of-bytes
/// invariant keeps the TxId correct while the APPLIED LEDGER STATE
/// silently diverges from what the same bytes mean upstream. That is
/// strictly worse than a decode failure: both nodes agree on the id and
/// disagree on the ledger.
pub(crate) fn decode_sub_tx_body(
    r: &mut Reader<'_>,
) -> Result<dugite_primitives::transaction::SubTransaction, SerializationError> {
    use dugite_primitives::transaction::SubTransaction;

    let mut sub = SubTransaction::default();

    let len = r.read_map_header()?;
    let total: i64 = len.map(|n| n as i64).unwrap_or(-1);
    let mut i: i64 = 0;
    loop {
        if total >= 0 && i >= total {
            break;
        }
        if total < 0 {
            let ty = r.peek_major()?;
            if ty == minicbor::data::Type::Break {
                r.skip()?;
                break;
            }
        }
        i += 1;
        let key = r.read_uint()?;
        match key {
            0 => {
                // spend inputs: set<transaction_input>  (tag 258 in Dijkstra)
                sub.inputs = r.read_set_strict(read_tx_input)?;
            }
            1 => {
                // outputs
                sub.outputs = r.read_array(|r| read_babbage_tx_output_with_raw(r))?;
            }
            3 => {
                sub.ttl = Some(SlotNo(r.read_uint()?));
            }
            4 => {
                // certificates: set<certificate> — same shape/key as the
                // top-level body's own key 4.
                sub.certificates = r.read_set_strict(|r| read_conway_certificate(r))?;
            }
            5 => {
                // withdrawals: { reward_account => coin }
                sub.withdrawals = read_withdrawals(r)?;
            }
            7 => {
                sub.auxiliary_data_hash = Some(read_hash32(r)?);
            }
            8 => {
                sub.validity_interval_start = Some(SlotNo(r.read_uint()?));
            }
            9 => {
                // mint: { policy_id => { asset_name => i64 } }
                sub.mint = read_mint_map(r)?;
            }
            11 => {
                // script_integrity_hash
                sub.script_data_hash = Some(read_hash32(r)?);
            }
            14 => {
                // guards: OSet (Credential Guard) — identical wire shape
                // to the top-level body's own key 14 (bare bstr(28) =>
                // VerificationKey, or array(2)[type,h28] => full
                // Credential). Dijkstra removed the classic
                // required-signers field entirely, so — unlike the
                // top-level decoder — there is no required_signers subset
                // to also populate here.
                sub.guards = r.read_set_strict(|r| {
                    let ty = r.peek_major()?;
                    match ty {
                        Type::Array | Type::ArrayIndef => read_stake_credential(r),
                        Type::Bytes | Type::BytesIndef => {
                            let h28 = read_hash28(r)?;
                            Ok(Credential::VerificationKey(h28))
                        }
                        other => Err(SerializationError::CborDecode(format!(
                            "sub-tx guards (key 14): expected bstr or array, got {other:?}"
                        ))),
                    }
                })?;
            }
            15 => {
                // network_id
                let raw = r.read_uint()?;
                sub.network_id = match raw {
                    0 | 1 => Some(raw as u8),
                    _ => None,
                };
            }
            18 => {
                sub.reference_inputs = r.read_set_strict(read_tx_input)?;
            }
            19 => {
                // voting_procedures: { voter => { gov_action_id => voting_procedure } }
                sub.voting_procedures = read_voting_procedures(r)?;
            }
            20 => {
                // proposal_procedures: OSet<proposal_procedure>
                //
                // `decode_sub_tx_body` only ever decodes a Dijkstra SubTx (the
                // `SubTransaction` concept does not exist pre-Dijkstra), so the
                // era passed to `read_proposal_procedure`'s PPU key-set gate
                // (issue #1013) is unconditionally `Era::Dijkstra` here — not a
                // behavior change, just naming what was already implicit.
                sub.proposal_procedures =
                    r.read_set_strict(|r| read_proposal_procedure(r, Era::Dijkstra))?;
            }
            21 => {
                // current_treasury_value
                sub.treasury_value = Some(read_lovelace(r)?);
            }
            22 => {
                // treasury_donation
                sub.donation = Some(read_lovelace(r)?);
            }
            25 => {
                // direct_deposits — wire-symmetric with withdrawals (key 5).
                sub.direct_deposits = read_withdrawals(r)?;
            }
            26 => {
                // account_balance_intervals
                sub.account_balance_intervals = decode_account_balance_intervals(r)?;
            }
            // Key 24 (required_top_level_guards) and every GADT-excluded
            // TopTx-only key (2 fee, 13 collateral, 16 collateral_return,
            // 17 total_collateral, 23 sub_transactions, 27
            // starting_account_balance_intervals) — see the doc comment
            // above for why these hard-reject rather than skip.
            _ => {
                return Err(SerializationError::CborDecode(format!(
                    "Dijkstra sub-tx body: unmodelled or structurally-invalid key {key} \
                     (#1010 — either unmodelled on SubTransaction, or a TopTx-only field \
                     that cannot appear on a sub-tx at all; refusing to silently discard \
                     its effects rather than accept a sub-tx whose applied state would \
                     diverge from upstream)"
                )));
            }
        }
    }
    Ok(sub)
}

/// Decode Dijkstra TxBody key 26 — `account_balance_intervals`.
///
/// Wire shape (mirrors `Cardano.Ledger.Dijkstra.Scripts.AccountBalanceIntervals`):
///
/// ```text
/// map { stake_credential => account_balance_interval }
/// account_balance_interval = [ coin / null, coin / null ]
/// ```
///
/// The Haskell decoder rejects intervals where **both** bounds are `null`
/// (`AccountBalanceInterval "Both interval bounds cannot be nil."`); we
/// surface the same rejection here.
///
/// See issue #475 Phase 3.3 — UTXO predicate `AccountBalanceOutOfRange`.
fn decode_account_balance_intervals(
    r: &mut Reader<'_>,
) -> Result<
    Vec<(
        dugite_primitives::credentials::Credential,
        dugite_primitives::transaction::AccountBalanceInterval,
    )>,
    SerializationError,
> {
    use dugite_primitives::transaction::AccountBalanceInterval;

    let len = r.read_map_header()?;
    let mut out: Vec<(_, AccountBalanceInterval)> = match len {
        Some(n) => Vec::with_capacity(n.min(1024) as usize),
        None => Vec::new(),
    };
    let total: i64 = len.map(|n| n as i64).unwrap_or(-1);
    let mut i: i64 = 0;
    loop {
        if total >= 0 && i >= total {
            break;
        }
        if total < 0 {
            let ty = r.peek_major()?;
            if ty == minicbor::data::Type::Break {
                r.skip()?;
                break;
            }
        }
        i += 1;

        let cred = read_stake_credential(r)?;
        let interval = decode_account_balance_interval(r)?;
        out.push((cred, interval));
    }
    Ok(out)
}

/// Decode a single `AccountBalanceInterval` — a 2-element CBOR array
/// `[lower, upper]` of `coin / null`.
fn decode_account_balance_interval(
    r: &mut Reader<'_>,
) -> Result<dugite_primitives::transaction::AccountBalanceInterval, SerializationError> {
    use dugite_primitives::transaction::AccountBalanceInterval;
    use dugite_primitives::value::Lovelace;

    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "AccountBalanceInterval: expected array(2), got {arr_len:?}"
        )));
    }

    // Helper: read an optional Coin — either null (skipped) or uint.
    let read_opt_coin = |r: &mut Reader<'_>| -> Result<Option<Lovelace>, SerializationError> {
        let ty = r.peek_major()?;
        if ty == minicbor::data::Type::Null {
            r.read_null()?;
            Ok(None)
        } else {
            Ok(Some(Lovelace(r.read_uint()?)))
        }
    };

    let lower = read_opt_coin(r)?;
    let upper = read_opt_coin(r)?;

    if lower.is_none() && upper.is_none() {
        // Mirror Haskell `Cardano.Ledger.Dijkstra.Scripts`:
        //   "Both interval bounds cannot be nil."
        return Err(SerializationError::CborDecode(
            "AccountBalanceInterval: both bounds cannot be nil".to_string(),
        ));
    }

    Ok(AccountBalanceInterval { lower, upper })
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

/// Read a transaction output and capture its raw CBOR bytes in `raw_cbor`.
///
/// This is a wrapper around [`read_babbage_tx_output`] that uses [`KeepRaw::parse_with`]
/// to preserve the original byte encoding. Required for byte-exact re-encoding of
/// inline datums, indefinite-length arrays, and collateral returns.
fn read_babbage_tx_output_with_raw(
    r: &mut Reader<'_>,
) -> Result<TransactionOutput, SerializationError> {
    let raw = KeepRaw::parse_with(r, read_babbage_tx_output)?;
    let mut output = raw.value;
    output.raw_cbor = Some(raw.raw.to_vec());
    Ok(output)
}

/// Read a Babbage/Conway post-Alonzo transaction output.
///
/// Conway outputs can be encoded in two forms:
///
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
    let addr_bytes = r.read_bytes_owned()?;
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

/// **Unknown keys are HARD-REJECTED** (issue #1013's sweep — see
/// `read_babbage_map_output` in `era_babbage.rs`, which carries the full
/// oracle citation this shares: `Conway`/`Dijkstra` both reuse
/// `BabbageTxOut`'s `DecCBOR` instance verbatim, `SparseKeyed` with
/// `bodyFields n = invalidField n` for keys outside 0-3, no era gating
/// needed since the key set is identical across Babbage/Conway/Dijkstra).
fn read_map_tx_output(r: &mut Reader<'_>) -> Result<TransactionOutput, SerializationError> {
    let mut address_bytes: Option<Vec<u8>> = None;
    let mut value: Option<Value> = None;
    let mut datum = OutputDatum::None;
    let mut script_ref = None;

    r.for_each_field_entry(|r, key| {
        match key {
            0 => {
                address_bytes = Some(r.read_bytes_owned()?);
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
                return Err(SerializationError::CborDecode(format!(
                    "map tx_out: unknown/invalid key {key}"
                )));
            }
        }
        Ok(())
    })?;

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
    // Haskell `decodeMultiAsset` (Mary/Value.hs) for decoder version >= 9
    // (`decodeConway`) REJECTS what pre-Conway eras pruned:
    //
    // ```haskell
    // decodeConway = MultiAsset <$> decodeMap decCBOR (decodeNonEmptyMap decodeNonZeroAmount)
    // decodeNonZeroAmount = do
    //   amount <- decodeAmount
    //   amount <$ when (amount == 0) (fail "MultiAsset cannot contain zeros")
    // decodeNonEmptyMap valueDecoder = do
    //   m <- decodeMap decCBOR valueDecoder
    //   m <$ when (Map.null m) (fail "Empty Assets are not allowed")
    // ```
    //
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
                    let name_bytes = r.read_bytes_owned()?;
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("multiasset: asset name too long".into())
                    })
                },
                |r| {
                    let amount = r.read_uint()?;
                    if amount == 0 {
                        return Err(SerializationError::CborDecode(
                            "multiasset: MultiAsset cannot contain zeros".into(),
                        ));
                    }
                    Ok(amount)
                },
            )?;
            if asset_pairs.is_empty() {
                return Err(SerializationError::CborDecode(
                    "multiasset: Empty Assets are not allowed".into(),
                ));
            }
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
            // Same Conway (decoder version >= 9) strictness as
            // `read_multiasset_map_u64` above: Haskell's `decodeMultiAsset` is
            // shared by the mint field (with a signed amount decoder), so zero
            // quantities and empty asset maps are decode FAILURES.
            let asset_pairs = r.read_map(
                |r| {
                    let name_bytes = r.read_bytes_owned()?;
                    AssetName::new(name_bytes).map_err(|_| {
                        SerializationError::CborDecode("mint: asset name too long".into())
                    })
                },
                |r| {
                    let amount = r.read_int()? as i64;
                    if amount == 0 {
                        return Err(SerializationError::CborDecode(
                            "mint: MultiAsset cannot contain zeros".into(),
                        ));
                    }
                    Ok(amount)
                },
            )?;
            if asset_pairs.is_empty() {
                return Err(SerializationError::CborDecode(
                    "mint: Empty Assets are not allowed".into(),
                ));
            }
            Ok(asset_pairs.into_iter().collect::<BTreeMap<_, _>>())
        },
    )?;
    Ok(policy_pairs.into_iter().collect())
}

fn read_withdrawals(r: &mut Reader<'_>) -> Result<BTreeMap<Vec<u8>, Lovelace>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(|r| r.read_bytes_owned(), |r| r.read_uint().map(Lovelace))?;
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

/// Read a script reference: `#6.24(bytes .cbor script)` where the embedded
/// CBOR is `[script_type, script_value]`.
///
/// **Script-type-dependent shape of `script_value`** (Conway CDDL):
/// - `script_type == 0` (native_script): the value is the native_script CBOR
///   itself — an array, NOT wrapped in a bytes string. Reading `read_bytes`
///   here trips the "expected bytes, got array" decode failure that orphaned
///   preprod block 4734057 at slot 123,678,510.
/// - `script_type == 1/2/3` (plutus_v1/v2/v3): the value is a bytes string
///   containing the serialized Plutus script.
///
/// This mirrors the Babbage `read_script_ref` shape (see `era_babbage.rs`).
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
    match script_type {
        0 => {
            // Native script — value is an array, read in place.
            let ns = read_native_script(&mut sr)?;
            Ok(ScriptRef::NativeScript(ns))
        }
        1 => {
            let script_bytes = sr.read_bytes_owned()?;
            Ok(ScriptRef::PlutusV1(script_bytes))
        }
        2 => {
            let script_bytes = sr.read_bytes_owned()?;
            Ok(ScriptRef::PlutusV2(script_bytes))
        }
        3 => {
            let script_bytes = sr.read_bytes_owned()?;
            Ok(ScriptRef::PlutusV3(script_bytes))
        }
        4 => {
            // PlutusV4: Dijkstra language tag 4. Wire shape identical to V3 —
            // `bstr(flat_program)`. Cost-model slot 3 (issue #475 Phase 5).
            let script_bytes = sr.read_bytes_owned()?;
            Ok(ScriptRef::PlutusV4(script_bytes))
        }
        other => Err(SerializationError::CborDecode(format!(
            "script_ref: unknown script type {other}"
        ))),
    }
}

/// Capture the ORIGINAL wire byte span of each native script in a `native_scripts`
/// set/array, in order. Reuses [`read_set`](Reader::read_set) (which transparently
/// handles the pre-Conway plain array and the Conway tag-258 set form) with a
/// [`KeepRaw`] element wrapper so each element's exact original bytes are captured.
fn read_native_script_spans(r: &mut Reader<'_>) -> Result<Vec<Vec<u8>>, SerializationError> {
    r.read_set(|r| {
        let kr = KeepRaw::parse_with(r, read_native_script)?;
        Ok(kr.raw.to_vec())
    })
}

/// Extract the ORIGINAL wire bytes of every native script in a transaction witness
/// set (key 1), in order, from the witness set's raw CBOR.
///
/// Cardano hashes a native script as `blake2b_224(0x00 || originalBytes)` over the
/// exact decoded bytes (Haskell `hashScript` over the Timelock `MemoBytes`), NEVER a
/// canonical re-encode. A non-canonically-but-validly-encoded native script (e.g. an
/// indefinite-length outer array, or a non-minimal integer field) therefore hashes
/// differently from `encode_native_script(decoded)`. This lets the ledger hash over
/// the original bytes with a re-encode fallback only when the raw CBOR is absent
/// (locally-constructed transactions). Era-agnostic. See issue #862.
///
/// Returns `None` if the witness set CBOR is unavailable/malformed or carries no
/// `native_scripts` key.
pub fn witness_native_script_original_bytes(witness_set_cbor: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut r = Reader::new(witness_set_cbor);
    let len = r.read_map_header().ok()?;
    let mut spans: Option<Vec<Vec<u8>>> = None;
    let mut read_one_entry = |r: &mut Reader<'_>| -> Result<(), SerializationError> {
        let key = r.read_uint()?;
        if key == 1 {
            spans = Some(read_native_script_spans(r)?);
        } else {
            r.skip()?;
        }
        Ok(())
    };
    match len {
        Some(n) => {
            for _ in 0..n {
                read_one_entry(&mut r).ok()?;
            }
        }
        None => loop {
            if r.peek_major().ok()? == Type::Break {
                r.expect_break().ok()?;
                break;
            }
            read_one_entry(&mut r).ok()?;
        },
    }
    spans
}

/// Extract the ORIGINAL wire bytes of a reference NATIVE script from a transaction
/// output's raw CBOR (map key 3, `#6.24(bytes .cbor [0, native_script])`).
///
/// The reference-script hash is `blake2b_224(0x00 || originalBytes)` over the inner
/// native script's original bytes (excluding the tag-24/bstr framing and the leading
/// `0` type tag), same as the witness-set path. Returns `None` for legacy-array
/// outputs, non-native reference scripts, missing script_ref, or malformed CBOR — in
/// which case the ledger falls back to a re-encode. See issue #862.
pub fn reference_native_script_original_bytes(output_cbor: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(output_cbor);
    // Only post-Alonzo map-form outputs carry a script_ref; a legacy array output
    // fails read_map_header and yields None.
    let len = r.read_map_header().ok()?;
    let mut found: Option<Vec<u8>> = None;
    let mut read_one_entry = |r: &mut Reader<'_>| -> Result<(), SerializationError> {
        let key = r.read_uint()?;
        if key == 3 && found.is_none() {
            // script_ref = #6.24(bytes .cbor [type, script])
            let inner = r.read_embedded_cbor_bytes()?.to_vec();
            let mut sr = Reader::new(&inner);
            if matches!(sr.read_array_header()?, Some(2)) && sr.read_uint()? == 0 {
                let kr = KeepRaw::parse_with(&mut sr, read_native_script)?;
                found = Some(kr.raw.to_vec());
            }
        } else {
            r.skip()?;
        }
        Ok(())
    };
    match len {
        Some(n) => {
            for _ in 0..n {
                read_one_entry(&mut r).ok()?;
            }
        }
        None => loop {
            if r.peek_major().ok()? == Type::Break {
                r.expect_break().ok()?;
                break;
            }
            read_one_entry(&mut r).ok()?;
        },
    }
    found
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
/// 5 = genesis_key_delegation    REMOVED — hard decode failure (see below)
/// 6 = move_instantaneous_reward REMOVED — hard decode failure (see below)
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
///
/// ## Issue #1023 — tags 5 and 6 are hard decode failures, not accepted legacy
///
/// Oracle-verified verbatim against `IntersectMBO/cardano-ledger` at pinned
/// SHA `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`
/// (`eras/conway/impl/src/Cardano/Ledger/Conway/TxCert.hs:719-726`):
///
/// ```haskell
/// decCBOR = decodeRecordSum "ConwayTxCert" $ \case
///   t
///     | 0 <= t && t < 3 -> shelleyTxCertDelegDecoder t
///     | 3 <= t && t < 5 -> poolTxCertDecoder t
///     | t == 5 -> fail "Genesis delegation certificates are no longer supported"
///     | t == 6 -> fail "MIR certificates are no longer supported"
///     | 7 <= t -> conwayTxCertDelegDecoder t
///   t -> invalidKey t
/// ```
///
/// This is a clean, unconditional, era-scoped `fail` — NOT protocol-version
/// gated (no `ifDecoderVersionAtLeast`/`natVersion` anywhere near this
/// match). `ConwayTxCert` is also a disjoint Haskell TYPE from
/// `ShelleyTxCert` (`type TxCert ConwayEra = ConwayTxCert ConwayEra`, with
/// only 3 constructors: `ConwayTxCertDeleg | ConwayTxCertPool |
/// ConwayTxCertGov` — no MIR/GenesisDeleg constructor exists at all), so the
/// removal is structural, not merely a decoder-side check. Every pre-Conway
/// era (Shelley/Allegra/Mary/Alonzo/Babbage) instead aliases
/// `type TxCert <Era> = ShelleyTxCert <Era>`
/// (`eras/babbage/impl/.../Babbage/TxCert.hs:12`, and identically for the
/// other four), whose decoder (`Shelley/TxCert.hs:475-487`) accepts both
/// tags unconditionally. Tags 0-4 and 7-18 are UNCHANGED — Conway's decoder
/// literally imports and calls the same `shelleyTxCertDelegDecoder` /
/// `poolTxCertDecoder` functions Shelley uses for 0-4.
///
/// dugite mirrors this via era-scoped decoder DISPATCH (this function is
/// Conway/Dijkstra-only; `era_shelley`/`era_alonzo` keep accepting 5/6
/// unchanged for Shelley through Babbage) rather than a runtime PV check —
/// matching upstream's own mechanism (GHC instance resolution on the era
/// type, not a version branch inside one decoder).
///
/// Test-only re-export of [`read_conway_certificate`] so the encoder's
/// round-trip test can prove `encode_certificate` produces bytes THIS decoder
/// accepts. #948 was exactly this asymmetry going unnoticed.
#[cfg(test)]
pub(crate) fn read_conway_certificate_for_test(
    r: &mut Reader<'_>,
) -> Result<Certificate, SerializationError> {
    read_conway_certificate(r)
}

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
            // #1023: Genesis delegation certificates were REMOVED from
            // ConwayTxCert. Oracle-verified `ConwayTxCert.hs:723`:
            //   | t == 5 -> fail "Genesis delegation certificates are no
            //                     longer supported"
            // This is unconditional (not PV-gated) and structural (no
            // `ConwayTxCert` constructor exists for it). A real
            // cardano-node peer rejects this at CBOR decode, before Phase-1
            // — mirror that exactly, with the same message, rather than
            // falling through to the generic "unknown type" arm below.
            Err(SerializationError::CborDecode(
                "certificate: Genesis delegation certificates are no longer supported".into(),
            ))
        }
        6 => {
            // #1023: MIR certificates were REMOVED from ConwayTxCert.
            // Oracle-verified `ConwayTxCert.hs:724`:
            //   | t == 6 -> fail "MIR certificates are no longer supported"
            // Same unconditional/structural removal as tag 5 above.
            Err(SerializationError::CborDecode(
                "certificate: MIR certificates are no longer supported".into(),
            ))
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
/// Test-only re-export of [`read_drep`] so the encoder's round-trip test can
/// prove `encode_drep` produces bytes THIS decoder accepts (#948).
#[cfg(test)]
pub(crate) fn read_drep_for_test(r: &mut Reader<'_>) -> Result<DRep, SerializationError> {
    read_drep(r)
}

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

pub(crate) fn read_anchor(r: &mut Reader<'_>) -> Result<Anchor, SerializationError> {
    // anchor = [url : text, anchor_data_hash : $hash32]
    // Conway CDDL: url = text (CBOR major type 3).
    // Some implementations may encode the URL as bytes (major type 2); handle both
    // to stay robust against non-canonical encodings.
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(2)) {
        return Err(SerializationError::CborDecode(format!(
            "anchor: expected array(2), got {arr_len:?}"
        )));
    }
    let ty = r.peek_major()?;
    let url = match ty {
        minicbor::data::Type::String => r.read_str()?.to_string(),
        _ => {
            // Fallback: treat as bytes and convert UTF-8.
            let url_bytes = r.read_bytes()?;
            String::from_utf8(url_bytes.to_vec()).map_err(|_| {
                SerializationError::CborDecode("anchor: URL bytes are not valid UTF-8".into())
            })?
        }
    };
    let data_hash = read_hash32(r)?;
    Ok(Anchor { url, data_hash })
}

/// Conway/Dijkstra `pool_params`.
///
/// Delegates to the shared implementation with `strict_owners`, since Conway
/// PV9+ rejects duplicate set elements.
fn read_pool_params(r: &mut Reader<'_>) -> Result<PoolParams, SerializationError> {
    super::era_shelley::read_pool_params_inner(r, true)
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
/// Test-only re-export of [`read_voter`] so the encoder's round-trip test can
/// prove its output is accepted by THIS decoder.
#[cfg(test)]
pub(crate) fn read_voter_for_test(r: &mut Reader<'_>) -> Result<Voter, SerializationError> {
    read_voter(r)
}

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

pub(crate) fn read_gov_action_id(r: &mut Reader<'_>) -> Result<GovActionId, SerializationError> {
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

/// Test-only re-export of [`read_voting_procedure`] so the encoder's round-trip test can
/// prove its output is accepted by THIS decoder.
#[cfg(test)]
pub(crate) fn read_voting_procedure_for_test(
    r: &mut Reader<'_>,
) -> Result<VotingProcedure, SerializationError> {
    read_voting_procedure(r)
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

pub(crate) fn read_proposal_procedure(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<ProposalProcedure, SerializationError> {
    // proposal_procedure = [deposit, reward_account, gov_action, anchor]
    let arr_len = r.read_array_header()?;
    if !matches!(arr_len, Some(4)) {
        return Err(SerializationError::CborDecode(format!(
            "proposal_procedure: expected array(4), got {arr_len:?}"
        )));
    }
    let deposit = read_lovelace(r)?;
    let return_addr = r.read_bytes_owned()?;
    let gov_action = read_gov_action(r, era)?;
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
/// Test-only re-export of [`read_gov_action`] so the encoder's round-trip test can
/// prove its output is accepted by THIS decoder.
#[cfg(test)]
pub(crate) fn read_gov_action_for_test(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<GovAction, SerializationError> {
    read_gov_action(r, era)
}

fn read_gov_action(r: &mut Reader<'_>, era: Era) -> Result<GovAction, SerializationError> {
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
            let protocol_param_update = Box::new(read_protocol_param_update(r, era)?);
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
            let pairs = r.read_map(|r| r.read_bytes_owned(), |r| read_lovelace(r))?;
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
            let remove_list: Vec<Credential> = r.read_set_strict(|r| read_stake_credential(r))?;
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

pub(crate) fn read_optional_hash28_gov(
    r: &mut Reader<'_>,
) -> Result<Option<Hash28>, SerializationError> {
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

/// Read a Conway/Dijkstra protocol parameter update (map form).
/// Test-only re-export of [`read_protocol_param_update`] so the encoder's
/// round-trip test can prove every populated field survives the wire. #919
/// found key 15 being decoded and then DROPPED — a full round-trip is what
/// catches that class.
#[cfg(test)]
pub(crate) fn read_protocol_param_update_for_test(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<ProtocolParamUpdate, SerializationError> {
    read_protocol_param_update(r, era)
}

/// Read a Conway (PV9-11) or Dijkstra (PV12+) `protocol_param_update` map.
///
/// Both the definite- and indefinite-length CBOR forms are accepted
/// (`for_each_field_entry` — issue #1012; the previous
/// `read_map_header()?.unwrap_or(0)` silently decoded an indefinite-length
/// map as ZERO entries and desynced the reader for everything after it — a
/// `ParameterChange` spanning more than 23 keys installed NO parameters at
/// all while cardano-node installed the real set: a consensus divergence).
/// `for_each_field_entry`
/// also hard-rejects a **duplicate** key on the same map, matching Haskell's
/// `Set Word`-tracked `decodeSparseKeyed`/`SparseKeyed` (both branches below).
///
/// **Unknown/out-of-era keys are HARD-REJECTED** (issue #1013), not silently
/// skipped. Oracle-verified against `IntersectMBO/cardano-ledger` at pinned
/// commit `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (SHA confirmed resolving
/// via `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`):
///
/// `libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs:262-297` —
/// `DecCBOR (PParamsUpdate era)` splits on `ifDecoderVersionAtLeast (natVersion
/// @12)` into `decodeSparseKeyed TypeName [] emptyPParamsUpdate decoderByKey`
/// (PV12+/Dijkstra) vs `decode $ SparseKeyed name emptyPParamsUpdate
/// updateField []` (PV<12/Conway) — but **both branches consult the exact same
/// `updateFieldMap`**, built once from `eraPParams @era`:
/// ```haskell
/// updateFieldMap :: IntMap (Field (PParamsUpdate era))
/// updateFieldMap =
///   IntMap.fromList
///     [ (fromIntegral ppuTag, mkField ppEraDecoder ppuLens)
///     | PParam {ppEraDecoder, ppUpdate = Just PParamUpdate {ppuTag, ppuLens}} <- eraPParams @era
///     ]
/// ```
/// A key absent from that map hard-fails in EITHER branch: the PV12+
/// `decodeSparseKeyed`'s `step` returns `Nothing` -> `failMsg "Unknown field
/// key ..."` (`Decoder.hs:1277-1287`); the pre-PV12 `updateField` falls
/// through `IntMap.findWithDefault (invalidField k) ...` -> `invalidField` ->
/// `cborError` (`Coders.hs`). The `natVersion @12` split changes only which
/// decode *combinator* runs, never which keys are legal — so a single,
/// era-gated (not PV-gated) key table is correct for both.
///
/// Per-era valid key sets, independently confirmed by fetching each file at
/// the pinned SHA (not inferred from dugite's prior match arms, several of
/// which were already too permissive before this fix):
/// - **Conway** (`eras/conway/impl/.../Conway/PParams.hs:862-894`): 0-11,
///   16-33. Gaps 12/13/14/15 — `ppD`/`hkdExtraEntropyL`/`hkdMinUTxOValueCompactL`
///   are `notSupportedInThisEraL`, and `ppGovProtocolVersion` (in the array(31)
///   positional list, at the same array slot key 14 would occupy) carries
///   `ppUpdate = Nothing` — present for the POSITIONAL `GetCurrentPParams`
///   encoding, absent from the sparse PPU update map.
/// - **Dijkstra** (`eras/dijkstra/impl/.../Dijkstra/PParams.hs:450-572`): 0-11,
///   16-39 — same four gaps as Conway (imports `ppGovProtocolVersion` directly
///   from Conway; `hkdMinUTxOValueCompactL = notSupportedInThisEraL`). Adds SIX
///   keys, not four: 34 `maxRefScriptSizePerBlock`, 35 `maxRefScriptSizePerTx`,
///   36 `refScriptCostStride`, 37 `refScriptCostMultiplier`, 38
///   `maxPledgeLeverage`, 39 `minPoolMargin`. `ProtocolParamUpdate` has no
///   fields for 38/39 yet (dugite's prior "34-37" doc comment undercounted
///   Dijkstra by two keys) — those two are rejected as unknown even under
///   `Era::Dijkstra`, a documented fail-closed gap (Dijkstra is unreleased)
///   for whoever adds the missing fields, tracked separately from this fix.
fn read_protocol_param_update(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<ProtocolParamUpdate, SerializationError> {
    let mut ppu = ProtocolParamUpdate::default();
    r.for_each_field_entry(|r, key| {
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
                read_pool_voting_thresholds(r, &mut ppu)?;
            }
            26 => {
                // drep_voting_thresholds
                read_drep_voting_thresholds(r, &mut ppu)?;
            }
            27 => ppu.min_committee_size = Some(r.read_uint()?),
            28 => ppu.committee_term_limit = Some(r.read_uint()?),
            29 => ppu.gov_action_lifetime = Some(r.read_uint()?),
            30 => ppu.gov_action_deposit = Some(read_lovelace(r)?),
            31 => ppu.drep_deposit = Some(read_lovelace(r)?),
            32 => ppu.drep_activity = Some(r.read_uint()?),
            33 => {
                // min_fee_ref_script_cost_per_byte: NonNegativeInterval (rational).
                // Preserve the full num/den — Haskell carries this rational through
                // the tiered ref-script fee and the ChangedParameters Plutus Data,
                // so truncating the denominator would diverge for a fractional value.
                ppu.min_fee_ref_script_cost_per_byte = Some(r.read_rational()?);
            }
            // Dijkstra-era PParams (keys 34-37). Conway's `eraPParams` list has
            // NO entries for these tags (verified — see the function doc
            // comment), so they must hard-reject when `era == Era::Conway`;
            // only Dijkstra's `eraPParams` list includes them.
            34 if era == Era::Dijkstra => {
                // maxRefScriptSizePerBlock: Word32 encoded as uint
                ppu.max_ref_script_size_per_block = Some(r.read_uint()? as u32);
            }
            35 if era == Era::Dijkstra => {
                // maxRefScriptSizePerTx: Word32 encoded as uint
                ppu.max_ref_script_size_per_tx = Some(r.read_uint()? as u32);
            }
            36 if era == Era::Dijkstra => {
                // refScriptCostStride: NonZero Word32 encoded as uint
                // Must be >= 1 on the wire; we store as u32 (caller enforces > 0).
                ppu.ref_script_cost_stride = Some(r.read_uint()? as u32);
            }
            37 if era == Era::Dijkstra => {
                // refScriptCostMultiplier: PositiveInterval — CBOR rational (tag 30
                // wrapping [numerator, denominator]) or bare [uint, uint].
                let rat = r.read_rational()?;
                ppu.ref_script_cost_multiplier = Some(Rational {
                    numerator: rat.numerator,
                    denominator: rat.denominator,
                });
            }
            _ => {
                // Unknown/out-of-era PPU key — HARD REJECT (issue #1013), per
                // upstream `updateFieldMap`/`invalidField`/`decodeSparseKeyed`
                // (see the function doc comment for the full oracle citation).
                // This also catches keys 12/13/14/15 (never valid in Conway OR
                // Dijkstra — no match arm exists for them above, by design),
                // keys 34-37 reached under `Era::Conway` (guard above did not
                // match), and keys 38/39 (Dijkstra-valid upstream, but
                // `ProtocolParamUpdate` has no fields for them yet — a
                // documented fail-closed gap, not this fix's scope).
                return Err(SerializationError::CborDecode(format!(
                    "{era:?} protocol_param_update: unknown/invalid key {key}"
                )));
            }
        }
        Ok(())
    })?;
    Ok(ppu)
}

pub(crate) fn read_cost_models(r: &mut Reader<'_>) -> Result<CostModels, SerializationError> {
    let mut plutus_v1 = None;
    let mut plutus_v2 = None;
    let mut plutus_v3 = None;
    let mut plutus_v4 = None;
    let mut unknown_cost_models: std::collections::BTreeMap<u8, Vec<i64>> =
        std::collections::BTreeMap::new();
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
            // Dijkstra cost-model slot 3 = PlutusV4 (issue #475 Phase 5).
            3 => plutus_v4 = Some(costs),
            // #770: preserve unknown-language entries (Haskell keeps these in
            // `_costModelsUnknown :: Map Word8 [Int64]` and re-emits them via
            // `flattenCostModels`). A key > 255 is not a valid `Word8` — Haskell
            // decodes the language key as `Word8`, so such a key is malformed.
            other => {
                let lang = u8::try_from(other).map_err(|_| {
                    SerializationError::CborDecode(format!(
                        "cost-model language key {other} exceeds Word8 (0..=255)"
                    ))
                })?;
                unknown_cost_models.insert(lang, costs);
            }
        }
    }
    Ok(CostModels {
        plutus_v1,
        plutus_v2,
        plutus_v3,
        plutus_v4,
        unknown_cost_models,
    })
}

pub(crate) fn read_ex_unit_prices(r: &mut Reader<'_>) -> Result<ExUnitPrices, SerializationError> {
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

pub(crate) fn read_ex_units(r: &mut Reader<'_>) -> Result<ExUnits, SerializationError> {
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
    ppu: &mut ProtocolParamUpdate,
) -> Result<(), SerializationError> {
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
    Ok(())
}

fn read_drep_voting_thresholds(
    r: &mut Reader<'_>,
    ppu: &mut ProtocolParamUpdate,
) -> Result<(), SerializationError> {
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
    Ok(())
}

// ============================================================================
// Witness set decoder (Conway)
// ============================================================================

/// Decode a Conway witness set.
///
/// Conway adds:
/// - Key 5: redeemers in MAP form `{ [tag, index] => [data, ex_units] }` OR array form
/// - Key 7: plutus_v3_scripts
pub(crate) fn decode_conway_witness_set(
    r: &mut Reader<'_>,
    era: Era,
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

    r.for_each_field_entry(|r, key| {
        match key {
            0 => {
                // vkey_witnesses: nonempty_set<vkeywitness> — tag(258) in Conway.
                //
                // Conway reuses the Alonzo TxWits decoder (Conway/TxWits.hs:
                // `type TxWits ConwayEra = AlonzoTxWits ConwayEra`). In
                // Alonzo/TxWits.hs the no-duplicate enforcement for vkey
                // witnesses (`addrWitsSetDecoder`) is gated at `natVersion @12`:
                // `ifDecoderVersionAtLeast (natVersion @12)
                //    nonEmptyNoDuplicatesDecoder nonEmptyDecoder`. At PV9-11
                // Haskell silently dedups (`nonEmptyDecoder` = `Set.fromList`)
                // and ACCEPTS a duplicate. Use lenient `read_set` so a dup-vkey
                // tx that Haskell accepts at live-mainnet PV11 does not hard-fail.
                vkey_witnesses = r.read_set(|r| {
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
                // native_scripts: nonempty_set<native_script> — tag(258) in Conway.
                //
                // Alonzo/TxWits.hs `nativeScriptsDecoder` gates no-duplicate
                // enforcement at `natVersion @12` (`noDuplicateNativeScriptsDecoder`);
                // at PV9-11 it uses `decodeNonEmptyList` + `Map.fromList`, which
                // silently dedups and ACCEPTS a duplicate. Use lenient `read_set`.
                native_scripts = r.read_set(|r| read_native_script(r))?;
            }
            2 => {
                // bootstrap_witnesses: nonempty_set<bootstrap_witness> — tag(258) in Conway.
                //
                // Bootstrap witnesses share `addrWitsSetDecoder` with vkey
                // witnesses (Alonzo/TxWits.hs), so the no-duplicate rule is also
                // gated at `natVersion @12`. At PV9-11 Haskell silently dedups
                // and ACCEPTS a duplicate. Use lenient `read_set`.
                bootstrap_witnesses = r.read_set(|r| {
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
                // plutus_v1_scripts: nonempty_set<plutus_v1_script> — tag(258) in Conway
                plutus_v1_scripts = r.read_set_strict(|r| r.read_bytes_owned())?;
            }
            4 => {
                // plutus_data: nonempty_set<plutus_data> — may be tag(258) on mainnet.
                // Conway CDDL allows both plain-array and tag-258 set encoding.
                //
                // `TxDatsRaw` DecCBOR (Alonzo/TxWits.hs) gates its no-duplicate
                // enforcement (`noDuplicatesDatsDecoder`) at `natVersion @12`;
                // at PV9-11 it uses `decodeNonEmptyList` + `Map.fromElems hashData`,
                // which silently dedups by hash and ACCEPTS a duplicate datum.
                // Use lenient `read_set` (still strips the optional tag-258 prefix).
                let pd_start = r.position();
                plutus_data = r.read_set(|r| read_plutus_data(r))?;
                raw_plutus_data_cbor = Some(r.slice_from(pd_start).to_vec());
            }
            5 => {
                // redeemers: map form (Conway) or array form (pre-Conway) — NOT a set
                let rd_start = r.position();
                redeemers = read_redeemers(r, era)?;
                raw_redeemers_cbor = Some(r.slice_from(rd_start).to_vec());
            }
            6 => {
                // plutus_v2_scripts: nonempty_set<plutus_v2_script> — tag(258) in Conway
                plutus_v2_scripts = r.read_set_strict(|r| r.read_bytes_owned())?;
            }
            7 => {
                // plutus_v3_scripts: nonempty_set<plutus_v3_script> — tag(258) in Conway
                plutus_v3_scripts = r.read_set_strict(|r| r.read_bytes_owned())?;
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
///
/// `era` selects the decoder version: from Dijkstra (PV12) on, the legacy
/// list/array encoding is a hard decode failure (see [`read_redeemers_raw`]).
fn read_redeemers(r: &mut Reader<'_>, era: Era) -> Result<Vec<Redeemer>, SerializationError> {
    read_redeemers_raw(r, era).map(crate::decode::helpers::dedup_redeemers_last_wins)
}

/// Wire-form reader without the Haskell `Map.fromList` dedup — every caller
/// must go through [`read_redeemers`]. Duplicate (tag, index) entries occur
/// on-chain (mainnet block 8,826,011, #753); Haskell collapses them
/// (last-wins) in BOTH the map and array wire forms.
///
/// From PV12 (Dijkstra) the list/array wire form is rejected outright, matching
/// cardano-ledger `DecCBOR (Annotator RedeemersRaw)` in `Alonzo/TxWits.hs`:
///
/// ```haskell
/// ifDecoderVersionAtLeast (natVersion @12)
///   (fail "List encoding of redeemers not supported starting with PV 12")
///   decodeListRedeemers
/// ```
///
/// The decoder version equals the protocol major version, and PV12 is the
/// Dijkstra hard fork, so `era == Dijkstra` is the exact proxy for the gate.
/// The whole list branch fails (empty `0x80` included) — only the Conway map
/// form is accepted at PV12+.
fn read_redeemers_raw(r: &mut Reader<'_>, era: Era) -> Result<Vec<Redeemer>, SerializationError> {
    let ty = r.peek_major()?;
    match ty {
        Type::Map | Type::MapIndef => {
            // Conway map form: { [tag, index] => [data, ex_units] }
            // Use read_map to handle both definite- and indefinite-length maps.
            // Both the definite (0xA_) and indefinite (0xBF) map headers occur
            // on-chain (e.g. preprod block 4149070, epoch 253, encodes the
            // redeemers map indefinitely); read_map handles both internally,
            // so the outer type check must accept MapIndef too.
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
        Type::Array | Type::ArrayIndef if era == Era::Dijkstra => {
            // PV12 (Dijkstra) hard-rejects the legacy list encoding — matches
            // Haskell's `fail` branch. Reject before reading any element so an
            // empty `0x80` list is refused too.
            Err(SerializationError::CborDecode(
                "List encoding of redeemers not supported starting with PV 12".to_string(),
            ))
        }
        Type::Array | Type::ArrayIndef => {
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
        // Dijkstra (PV12+): tag 6 = `Guarding` per
        // `Cardano.Ledger.Dijkstra.Scripts.DijkstraPlutusPurpose`
        // (`DijkstraGuarding`, Sum 6). Issue #475 Phase 3.5.
        6 => Ok(RedeemerTag::Guarding),
        other => Err(SerializationError::CborDecode(format!(
            "redeemer_tag: unknown value {other}"
        ))),
    }
}

// ============================================================================
// Native script decoder
// ============================================================================

/// Decode a native (timelock) script from its raw CBOR encoding.
///
/// The input is the bare native-script CBOR array (NOT tag-24-wrapped). This is
/// the body stored by a MemPack `AlonzoScript`'s `NativeScript` (timelock)
/// variant, whose MemoBytes hold exactly this CBOR. Exposed so the Mithril /
/// Haskell-import UTxO loader can reconstruct a typed `ScriptRef::NativeScript`
/// from a MemPack reference-script blob and hash it via the ledger's
/// `compute_script_ref_hash`.
pub fn decode_native_script_cbor(cbor: &[u8]) -> Result<NativeScript, SerializationError> {
    let mut r = Reader::new(cbor);
    read_native_script(&mut r)
}

/// Decode an inline-datum `PlutusData` from its bare CBOR encoding.
///
/// The input is the raw CBOR-encoded Plutus `Data` (NOT tag-24-wrapped). This is
/// exactly the body carried by a MemPack `Datum era` inline (`Datum binaryData`)
/// variant: `BinaryData` is a newtype over `ShortByteString` holding the
/// *original on-chain CBOR* of the datum, so the bytes are identical to the
/// `inner_bytes` extracted from a tag-24 datum_option during normal block decode
/// (see [`read_datum_option`]).
///
/// Exposed so the Mithril / Haskell-import UTxO loader can reconstruct a typed
/// `OutputDatum::InlineDatum` from a MemPack TxOut (tags 4/5). Dropping the datum
/// (importing `OutputDatum::None`) builds a *wrong* `ScriptContext` for any
/// resolved script that spends the output, producing spurious "script returned
/// Error term" phase-2 failures at the live tip.
pub fn decode_plutus_data_cbor(cbor: &[u8]) -> Result<PlutusData, SerializationError> {
    let mut r = Reader::new(cbor);
    read_plutus_data(&mut r)
}

fn read_native_script(r: &mut Reader<'_>) -> Result<NativeScript, SerializationError> {
    // OUTER ARRAY: accept BOTH definite- AND indefinite-length encodings.
    //
    // cardano-ledger's Timelock decoder reads this outer "sum" array via
    // `Summands "TimelockRaw" decRaw` => `decodeRecordSum` => `decodeListLike`
    // => `decodeListLikeT`, which decodes the length with `decodeListLenOrIndef`
    // and TOLERATES the indefinite case (consuming the trailing break byte):
    //
    // ```haskell
    // -- Cardano.Ledger.Allegra.Scripts
    // instance Era era => DecCBOR (Annotator (TimelockRaw era)) where
    //   decCBOR = decode (Summands "TimelockRaw" decRaw)
    //
    // -- Cardano.Ledger.Binary.Decoding.Decoder
    // decodeRecordSum name decoder =
    //   snd <$> decodeListLike name (decodeWord >>= decoder) $ \(size, _) n ->
    //     matchSize ("Sum " <> name) size n
    //
    // decodeListLikeT name decoder actOnLength = do
    //   lenOrIndef <- lift decodeListLenOrIndef
    //   result <- decoder
    //   case lenOrIndef of
    //     Just n  -> actOnLength result n
    //     Nothing -> lift $ do
    //       isBreak <- decodeBreakOr
    //       unless isBreak $ cborError $ DecoderErrorCustom name "Excess terms in array"
    //   pure result
    // ```
    //
    // So an indefinite-length outer array is VALID upstream. Our previous
    // `read_array_header` + `is_none => Err` HARD-REJECTED it, over-rejecting
    // potentially-real on-chain native scripts (ledger leniency means some may be
    // indefinite-encoded) and aborting an entire snapshot import on the first such
    // tag-5 reference script. Every NESTED list below already accepts indefinite
    // arrays transparently via `read_array`; we now mirror that AND the upstream
    // `decodeListLenOrIndef` tolerance for the outer array too (same bug class as
    // commit 4b42125fbb, "accept indefinite-length CBOR map/array in Conway
    // redeemers decode"). #10 round-4 F2.
    let arr_len = r.read_array_header()?;
    let disc = r.read_uint()?;
    let script = match disc {
        0 => {
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
        6 => {
            // Dijkstra (PV12+) only: `DijkstraRequireGuard credential`
            // wire shape per
            // `Cardano.Ledger.Dijkstra.Scripts.DijkstraRequireGuard`:
            //   array(2) [uint 6, credential]
            // where `credential = [type, hash28]` (0 = key, 1 = script).
            // Issue #475 Phase 3.5.
            let cred = read_stake_credential(r)?;
            NativeScript::RequireGuard(cred)
        }
        other => {
            return Err(SerializationError::CborDecode(format!(
                "native_script: unknown type {other}"
            )))
        }
    };
    // Indefinite-length outer array: consume the trailing CBOR break byte
    // (0xff), exactly as upstream `decodeListLikeT` does for the `Nothing`
    // (indefinite) case. For a definite-length array nothing further is needed.
    if arr_len.is_none() {
        r.expect_break()?;
    }
    Ok(script)
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
    read_plutus_data_depth(r, 0)
}

/// Maximum nesting depth for `read_plutus_data` (Conway era).
///
/// See the `MAX_PLUTUS_DATA_DEPTH` constant in `era_alonzo.rs` for the
/// rationale; the same limit applies to the Conway decoder because the
/// recursive grammar is identical.
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
            // Constructor: tag(121..=127 or 1280..=1400) + array, or tag(2/3) bignum
            // Peek at the tag value without consuming it, then dispatch.
            let tag_val = r.probe_tag()?;
            match tag_val {
                2 | 3 => {
                    // bignum: the mantissa is a PlutusData ByteString leaf, so
                    // it must obey the plutus 64-byte-per-chunk bound (Note
                    // [The 64-byte limit], #28). Route through the bounded
                    // PlutusData bigint reader rather than the generic
                    // `read_bigint` (which uses the unbounded `read_bytes_owned`
                    // and is shared with non-PlutusData decode paths).
                    let big = r.read_bounded_plutus_bigint()?;
                    Ok(PlutusData::Integer(big))
                }
                121..=127 => {
                    // Alternative 0..=6: tag(121+n) [* plutus_data]
                    r.read_tag()?; // consume tag
                    let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
                    Ok(PlutusData::Constr(tag_val - 121, fields))
                }
                1280..=1400 => {
                    // Alternative 7+: tag(1280+n) [* plutus_data]
                    r.read_tag()?; // consume tag
                    let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
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
                    let fields = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
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
        Type::Map | Type::MapIndef => {
            // Both definite-length and indefinite-length maps are valid PlutusData.
            // read_map() handles both via the None => loop {} branch.
            let entries = r.read_map(
                |r| read_plutus_data_depth(r, depth + 1),
                |r| read_plutus_data_depth(r, depth + 1),
            )?;
            Ok(PlutusData::Map(entries))
        }
        Type::Array | Type::ArrayIndef => {
            let items = r.read_array(|r| read_plutus_data_depth(r, depth + 1))?;
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

// ============================================================================
// Auxiliary data decoder
// ============================================================================

fn decode_aux_data_map(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<BTreeMap<u32, AuxiliaryData>, SerializationError> {
    // Use read_map to handle both definite- and indefinite-length maps.
    let pairs = r.read_map(
        |r| Ok(r.read_uint()? as u32),
        |r| decode_auxiliary_data(r, era),
    )?;
    Ok(pairs.into_iter().collect())
}

/// Conway/Dijkstra auxiliary data.
///
/// Delegates to the shared decoder. This copy handled all three shapes but
/// still skipped the ShelleyMa array's native scripts. Issue #984.
fn decode_auxiliary_data(
    r: &mut Reader<'_>,
    era: Era,
) -> Result<AuxiliaryData, SerializationError> {
    super::era_alonzo::decode_alonzo_auxiliary_data(r, era)
}

// ============================================================================
// Standalone tx decoder (Conway / Dijkstra era)
// ============================================================================

/// Decode a standalone Conway or Dijkstra transaction from raw CBOR bytes.
///
/// The standalone tx format is `[body_map, witness_set_map, is_valid_bool, aux_data]`.
///
/// The `era` argument is stamped onto the returned [`Transaction`] (Conway or
/// Dijkstra — both use the same CBOR structure).
///
/// The transaction hash is `blake2b_256(raw_body_cbor)`.
pub(crate) fn decode_conway_tx_standalone(
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
    let body_raw = KeepRaw::parse_with(&mut r, |r| decode_conway_tx_body(r, era))?;
    let raw_body_cbor = body_raw.raw.to_vec();
    let tx_hash = blake2b_256(&raw_body_cbor);
    let body = body_raw.value;

    // 2. Witness set
    let ws_raw = KeepRaw::parse_with(&mut r, |r| decode_conway_witness_set(r, era))?;
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
            Some(decode_auxiliary_data(&mut r, era)?)
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

/// Decode a standalone Dijkstra transaction from raw CBOR bytes (CIP-0167).
///
/// Dijkstra removes the top-level `isValid` flag from the mempool / standalone
/// transaction wire format. The wire shape is therefore:
///
/// ```text
/// tx = [body, witness_set, auxiliary_data]    -- 3 elements
/// ```
///
/// instead of the pre-Dijkstra `[body, witness_set, is_valid, aux_data]`.
///
/// This matches Haskell `Cardano.Ledger.Dijkstra.Tx`:
///
/// ```haskell
/// toCBORForMempoolSubmission DijkstraTx{..} =
///   encode $ Rec DijkstraTx
///     !> To dtBody
///     !> To dtWits
///     !> OmitC dtIsValid          -- *** removed from wire ***
///     !> E (encodeNullStrictMaybe encCBOR) dtAuxData
/// ```
///
/// Because the wire no longer carries an explicit validity flag, the returned
/// [`Transaction`] has `is_valid` defaulted to `true`. The ledger determines
/// the actual outcome dynamically: a Phase-2 script failure routes the
/// transaction through the collateral-consumption path
/// (`apply_invalid_tx`), exactly as in Conway, regardless of any
/// author-supplied flag.
///
/// The transaction hash is `blake2b_256(raw_body_cbor)` — identical
/// computation to Conway.
pub(crate) fn decode_dijkstra_tx_standalone(
    cbor: &[u8],
) -> Result<Transaction, SerializationError> {
    let mut r = Reader::new(cbor);

    // tx = [body, witness_set, aux_data]  -- CIP-0167: NO is_valid bool.
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(3) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "Dijkstra tx: expected array(3) per CIP-0167, got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(
                "Dijkstra tx: expected definite-length array".to_string(),
            ));
        }
    }

    // 1. Body — capture raw bytes for hash computation.
    let body_raw = KeepRaw::parse_with(&mut r, |r| decode_conway_tx_body(r, Era::Dijkstra))?;
    let raw_body_cbor = body_raw.raw.to_vec();
    let tx_hash = blake2b_256(&raw_body_cbor);
    let body = body_raw.value;

    // 2. Witness set.
    let ws_raw = KeepRaw::parse_with(&mut r, |r| decode_conway_witness_set(r, Era::Dijkstra))?;
    let raw_witness_cbor = ws_raw.raw.to_vec();
    let witness_set = ws_raw.value;

    // 3. Auxiliary data (null or a value). No is_valid bool in between.
    let auxiliary_data = {
        let ty = r.peek_major()?;
        if ty == Type::Null {
            r.read_null()?;
            None
        } else {
            Some(decode_auxiliary_data(&mut r, Era::Dijkstra)?)
        }
    };

    Ok(Transaction {
        hash: tx_hash,
        era: Era::Dijkstra,
        body,
        witness_set,
        // CIP-0167: validity is determined dynamically by Phase-2 evaluation,
        // not signaled on the wire. Default to `true` here; the ledger's
        // `apply_invalid_tx` path runs when Phase-2 actually fails.
        is_valid: true,
        auxiliary_data,
        raw_cbor: Some(cbor.to_vec()),
        raw_body_cbor: Some(raw_body_cbor),
        raw_witness_cbor: Some(raw_witness_cbor),
    })
}

/// Decode a single Conway/Dijkstra `transaction_output` CBOR value.
///
/// Conway outputs share the post-Alonzo "Babbage" wire shape — either a
/// 2-or-3-element legacy array or a `{ 0: addr, 1: value, ? 2: datum_option,
/// ? 3: script_ref }` map. The decoder accepts either form.
///
/// Used by [`crate::decode::decode_transaction_output`] (Conway / Dijkstra
/// dispatch) and by `dugite-uplc`'s phase-2 evaluator to decode the
/// resolved-UTxO CBOR pairs the ledger passes in.
pub(crate) fn decode_conway_tx_output_standalone(
    cbor: &[u8],
) -> Result<TransactionOutput, SerializationError> {
    let mut r = Reader::new(cbor);
    let raw = KeepRaw::parse_with(&mut r, read_babbage_tx_output)?;
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

    /// #862: a native script encoded with a non-canonical INDEFINITE-length outer
    /// array is (a) accepted by the decoder and (b) has its EXACT original bytes
    /// recovered by `witness_native_script_original_bytes`, which differ from a
    /// canonical re-encode. This is the byte-exact property the hashScript fix needs.
    #[test]
    fn witness_native_original_bytes_captures_indefinite_form_862() {
        // ScriptPubkey = [0, h28]; indefinite outer array: 0x9f 0x00 (0x581c<28>) 0xff
        let mut ns = vec![0x9f, 0x00, 0x58, 0x1c];
        ns.extend_from_slice(&[0xAB; 28]);
        ns.push(0xff);
        // Witness set map { 1 => array(1) [ ns ] } : 0xa1 0x01 0x81 <ns>
        let mut ws = vec![0xa1, 0x01, 0x81];
        ws.extend_from_slice(&ns);

        let spans = witness_native_script_original_bytes(&ws).expect("extract native spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0], ns,
            "must capture the exact original (indefinite) bytes"
        );

        // The decoded script re-encodes to a DIFFERENT (canonical, definite) form.
        let decoded = read_native_script(&mut Reader::new(&ns)).unwrap();
        let reencoded = crate::encode::encode_native_script(&decoded);
        assert_ne!(
            reencoded, ns,
            "canonical re-encode must differ from the indefinite original"
        );
    }

    /// #862: a reference NATIVE script's original inner bytes are recovered from an
    /// output's CBOR (map key 3 = #6.24(bstr .cbor [0, native_script])).
    #[test]
    fn reference_native_original_bytes_recovers_inner_script_862() {
        // native script (definite): [0, h28] = 0x82 0x00 0x581c<28>
        let mut ns = vec![0x82, 0x00, 0x58, 0x1c];
        ns.extend_from_slice(&[0xCD; 28]);
        // inner = [0, native_script] = 0x82 0x00 <ns>
        let mut inner = vec![0x82, 0x00];
        inner.extend_from_slice(&ns);
        // script_ref = #6.24(bstr(inner)) = 0xd8 0x18 0x58 <len> <inner>
        let mut sref = vec![0xd8, 0x18, 0x58, inner.len() as u8];
        sref.extend_from_slice(&inner);
        // output map { 0 => addr, 3 => script_ref }
        let addr = {
            let mut v = vec![0x60];
            v.extend_from_slice(&[0u8; 28]);
            v
        };
        let mut out = vec![0xa2, 0x00, 0x58, addr.len() as u8];
        out.extend_from_slice(&addr);
        out.push(0x03);
        out.extend_from_slice(&sref);

        let recovered =
            reference_native_script_original_bytes(&out).expect("recover inner native bytes");
        assert_eq!(
            recovered, ns,
            "must recover the exact inner native-script bytes"
        );
    }

    /// #730: Haskell `decodeMultiAsset` at decoder version >= 9
    /// (`decodeConway`) REJECTS zero-quantity assets:
    /// `fail "MultiAsset cannot contain zeros"`.
    #[test]
    fn conway_value_zero_quantity_asset_rejected() {
        // [10, {policy(0x11) -> {"" -> 0}}]
        let mut bytes = vec![0x82, 0x0a, 0xa1, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.extend([0xa1, 0x40, 0x00]);
        let mut r = Reader::new(&bytes);
        let err = read_value(&mut r).expect_err("Conway must reject zero amounts");
        assert!(
            err.to_string().contains("cannot contain zeros"),
            "unexpected error: {err}"
        );
    }

    /// Haskell `decodeNonEmptyMap`: `fail "Empty Assets are not allowed"`.
    #[test]
    fn conway_value_empty_asset_map_rejected() {
        // [10, {policy(0x11) -> {}}]
        let mut bytes = vec![0x82, 0x0a, 0xa1, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.push(0xa0);
        let mut r = Reader::new(&bytes);
        let err = read_value(&mut r).expect_err("Conway must reject empty asset maps");
        assert!(
            err.to_string().contains("Empty Assets"),
            "unexpected error: {err}"
        );
    }

    /// Conway mint shares the same strictness: zero quantities are a decode
    /// failure.
    #[test]
    fn conway_mint_zero_quantity_rejected() {
        // {policy(0x11) -> {"" -> 0}}
        let mut bytes = vec![0xa1, 0x58, 0x1c];
        bytes.extend([0x11; 28]);
        bytes.extend([0xa1, 0x40, 0x00]);
        let mut r = Reader::new(&bytes);
        let err = read_mint_map(&mut r).expect_err("Conway must reject zero mint amounts");
        assert!(
            err.to_string().contains("cannot contain zeros"),
            "unexpected error: {err}"
        );
    }

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
        let redeemers = read_redeemers(&mut r, Era::Conway).unwrap();
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
        let redeemers = read_redeemers(&mut r, Era::Conway).unwrap();
        assert_eq!(redeemers.len(), 1);
        assert_eq!(redeemers[0].tag, RedeemerTag::Spend);
        assert_eq!(redeemers[0].ex_units.mem, 50);
    }

    /// #885: the legacy list/array redeemers encoding is accepted through
    /// Conway (PV9/10/11) but hard-rejected from Dijkstra (PV12) on, mirroring
    /// cardano-ledger's `ifDecoderVersionAtLeast (natVersion @12) (fail …)`.
    #[test]
    fn test_redeemers_list_form_rejected_from_pv12() {
        // Same array-form bytes as `test_read_redeemers_array_form`:
        // [[0, 0, #6.121([]), [50, 100]]] — Alonzo/Babbage list form.
        let constr_data = {
            let mut v = cbor_tag(121);
            v.extend(vec![0x80]);
            v
        };
        let ex_units = cbor_arr(&[&cbor_uint(50), &cbor_uint(100)]);
        let redeemer = cbor_arr(&[&cbor_uint(0), &cbor_uint(0), &constr_data, &ex_units]);
        let mut list_bytes = vec![0x81]; // array(1)
        list_bytes.extend(&redeemer);

        // Conway (PV11): accepted.
        let mut r = Reader::new(&list_bytes);
        let redeemers = read_redeemers(&mut r, Era::Conway).expect("Conway accepts list form");
        assert_eq!(redeemers.len(), 1);

        // Dijkstra (PV12): the identical bytes are a decode failure.
        let mut r = Reader::new(&list_bytes);
        let err = read_redeemers(&mut r, Era::Dijkstra)
            .expect_err("Dijkstra must reject list-encoded redeemers");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("PV 12")),
            "unexpected error for PV12 list redeemers: {err:?}"
        );

        // The EMPTY list `0x80` is rejected too (whole list branch fails).
        let empty_list = vec![0x80];
        let mut r = Reader::new(&empty_list);
        assert!(read_redeemers(&mut r, Era::Dijkstra).is_err());
        // …but Conway still accepts an empty list as zero redeemers.
        let mut r = Reader::new(&empty_list);
        assert_eq!(read_redeemers(&mut r, Era::Conway).unwrap().len(), 0);

        // The Conway MAP form is accepted in BOTH eras (only the list form is
        // gated). { [0,0] => [#6.121([]), [50,100]] }
        let value_arr = cbor_arr(&[&constr_data, &ex_units]);
        let key_arr = cbor_arr(&[&cbor_uint(0), &cbor_uint(0)]);
        let mut map_bytes = vec![0xa1]; // map(1)
        map_bytes.extend(&key_arr);
        map_bytes.extend(&value_arr);
        for era in [Era::Conway, Era::Dijkstra] {
            let mut r = Reader::new(&map_bytes);
            let rs = read_redeemers(&mut r, era).expect("map form accepted in every era");
            assert_eq!(rs.len(), 1);
            assert_eq!(rs[0].ex_units.mem, 50);
        }
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
        let ws = decode_conway_witness_set(&mut r, Era::Conway).unwrap();
        assert_eq!(ws.plutus_v3_scripts.len(), 1);
        assert_eq!(ws.plutus_v3_scripts[0], vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn conway_witness_set_unknown_key_rejected() {
        // A valid Conway witness set ({7: [[script]]}) with an unknown key 8
        // appended (one past the highest known Conway witness key). Haskell
        // cardano-ledger SparseKeyed (txWitnessField n = invalidField n)
        // hard-fails the unknown key; dugite must reject the decode too.
        let script_bytes = cbor_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let scripts_arr = {
            let mut v = vec![0x81]; // array(1)
            v.extend(&script_bytes);
            v
        };
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(7)); // key 7 = plutus_v3_scripts (valid)
        data.extend(&scripts_arr);
        data.extend(cbor_uint(8)); // key 8 = unknown
        data.extend(cbor_uint(0));

        let mut r = Reader::new(&data);
        let result = decode_conway_witness_set(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "unknown witness-set key must be rejected, got {result:?}"
        );
    }

    /// Issue #1000 (PlutusV4/Dijkstra): the SAME key-8 probe rejected for
    /// Conway above, but decoded under `Era::Dijkstra`. Oracle-verified
    /// (`IntersectMBO/cardano-ledger` @
    /// `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`): Dijkstra's `TxWits`
    /// is literally `type TxWits DijkstraEra = AlonzoTxWits DijkstraEra` and
    /// its `natVersion @12`-gated `decoderByKey` still enumerates ONLY keys
    /// 0-7 — there is no `plutus_v4_scripts` witness-set field upstream (the
    /// Dijkstra CDDL spec generator has a literal
    /// `-- TODO: Add plutus_v4_script at index 8 …` marking it unimplemented,
    /// and the era's own test fixtures document that including one "would
    /// cause a roundtrip failure as they get silently dropped during
    /// serialization"). A real cardano-node peer can neither emit nor decode
    /// a V4 witness-set script, so dugite must not either — key 8 stays
    /// rejected under Dijkstra exactly as it is under Conway. See
    /// `dugite_uplc::redeemer_resolve::ScriptLanguage`'s doc comment and
    /// `find_script_bytes`'s doc comment for the full citation and the
    /// evaluator-side consequence (V4 only resolves via `script_ref`).
    #[test]
    fn dijkstra_witness_set_hypothetical_plutus_v4_key_still_rejected() {
        let script_bytes = cbor_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let scripts_arr = {
            let mut v = vec![0x81]; // array(1)
            v.extend(&script_bytes);
            v
        };
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(7)); // key 7 = plutus_v3_scripts (valid)
        data.extend(&scripts_arr);
        data.extend(cbor_uint(8)); // key 8 = hypothetical plutus_v4_scripts
        data.extend(&scripts_arr);

        let mut r = Reader::new(&data);
        let result = decode_conway_witness_set(&mut r, Era::Dijkstra);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "Dijkstra witness-set key 8 (no plutus_v4_scripts slot upstream) \
             must be rejected exactly like Conway, got {result:?}"
        );
    }

    // ── SparseKeyed duplicate field-key rejection (backlog #31-D) ──────────────

    #[test]
    fn conway_witness_set_duplicate_field_key_rejected() {
        // map(2) { 0: [], 0: [] } — field key 0 (vkey_witnesses) twice.
        // Haskell decodeSparseKeyed tracks Set Word seen keys and hard-fails
        // the second occurrence (un-gated at every PV).
        let empty_set = vec![0x80]; // array(0)
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(0));
        data.extend(&empty_set);
        data.extend(cbor_uint(0));
        data.extend(&empty_set);

        let mut r = Reader::new(&data);
        let result = decode_conway_witness_set(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate witness-set field key must be rejected, got {result:?}"
        );
    }

    #[test]
    fn map_tx_output_duplicate_field_key_rejected() {
        // map(3) { 0: addr, 1: 5, 1: 7 } — field key 1 (value) twice.
        let addr = {
            let mut a = vec![0x60u8]; // enterprise, mainnet
            a.extend([0x11u8; 28]);
            a
        };
        let mut data = vec![0xa3]; // map(3)
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&addr));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(5));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(7));

        let mut r = Reader::new(&data);
        let result = read_map_tx_output(&mut r);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate map-TxOut field key must be rejected, got {result:?}"
        );
    }

    #[test]
    fn map_tx_output_unique_field_keys_ok() {
        // map(2) { 0: addr, 1: 5 } — sanity that the strict path still decodes.
        let addr = {
            let mut a = vec![0x60u8];
            a.extend([0x11u8; 28]);
            a
        };
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&addr));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(5));

        let mut r = Reader::new(&data);
        let out = read_map_tx_output(&mut r).expect("unique map-TxOut keys decode");
        assert_eq!(out.value.coin.0, 5);
    }

    /// Issue #1013's sweep finding: `post_alonzo_transaction_output` is
    /// SparseKeyed 0-3 (`invalidField` catch-all — same as PPU), so an
    /// unrecognized key must hard-reject, not silently skip.
    #[test]
    fn map_tx_output_unknown_key_rejected() {
        // map(2) { 0: addr, 4: 0 } — key 4 does not exist in BabbageTxOut.
        let addr = {
            let mut a = vec![0x60u8];
            a.extend([0x11u8; 28]);
            a
        };
        let mut data = vec![0xa2]; // map(2)
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&addr));
        data.extend(cbor_uint(4));
        data.extend(cbor_uint(0));

        let mut r = Reader::new(&data);
        let result = read_map_tx_output(&mut r);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "unknown map-TxOut key 4 must be rejected, got {result:?}"
        );
    }

    // ── Conway PV9+ Set duplicate rejection (backlog #31-C) ────────────────────

    /// Build a Conway transaction_input `[hash32, index]`.
    fn conway_input(hash_byte: u8, index: u64) -> Vec<u8> {
        cbor_arr(&[&cbor_bytes(&[hash_byte; 32]), &cbor_uint(index)])
    }

    #[test]
    fn conway_body_duplicate_input_rejected() {
        // map(1) { 0: tag(258) [in, in] } — the SAME input twice.
        // At PV9+ `inputs` is a Set and a duplicate must hard-fail the decode.
        let input = conway_input(0xaa, 0);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&input);
        set.extend(&input);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(0)); // key 0 = inputs
        data.extend(&set);

        let mut r = Reader::new(&data);
        let result = decode_conway_tx_body(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate input in Conway set must be rejected, got {result:?}"
        );
    }

    #[test]
    fn conway_body_unique_inputs_accepted() {
        // map(1) { 0: tag(258) [inA, inB] } — distinct inputs decode Ok and
        // preserve wire order.
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&conway_input(0xaa, 0));
        set.extend(&conway_input(0xbb, 1));

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(0));
        data.extend(&set);

        let mut r = Reader::new(&data);
        let body = decode_conway_tx_body(&mut r, Era::Conway).expect("unique inputs decode");
        assert_eq!(body.inputs.len(), 2);
        assert_eq!(body.inputs[0].index, 0);
        assert_eq!(body.inputs[1].index, 1);
    }

    #[test]
    fn conway_body_duplicate_certificate_rejected() {
        // map(1) { 4: tag(258) [cert, cert] } — the SAME stake-reg cert twice.
        // certificates is a Set in Conway; a duplicate must hard-fail.
        let cert = {
            // [7, [0, keyhash(28)], deposit] = Conway stake registration
            let mut c = vec![0x83];
            c.extend(cbor_uint(7));
            let mut cred = vec![0x82];
            cred.extend(cbor_uint(0));
            cred.extend(cbor_bytes(&[0x11; 28]));
            c.extend(&cred);
            c.extend(cbor_uint(2_000_000));
            c
        };
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&cert);
        set.extend(&cert);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(4)); // key 4 = certificates
        data.extend(&set);

        let mut r = Reader::new(&data);
        let result = decode_conway_tx_body(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate certificate in Conway set must be rejected, got {result:?}"
        );
    }

    #[test]
    fn conway_body_duplicate_required_signer_rejected() {
        // map(1) { 14: tag(258) [h28, h28] } — the SAME required signer twice.
        // required_signers (key 14) is a Set in Conway; duplicate must fail.
        let signer = cbor_bytes(&[0x33; 28]);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&signer);
        set.extend(&signer);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(14)); // key 14 = required_signers
        data.extend(&set);

        let mut r = Reader::new(&data);
        let result = decode_conway_tx_body(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate required_signer in Conway set must be rejected, got {result:?}"
        );
    }

    #[test]
    fn conway_witness_set_duplicate_vkey_accepted_lenient() {
        // map(1) { 0: tag(258) [vkw, vkw] } — the SAME vkeywitness twice.
        //
        // Conway reuses the Alonzo TxWits decoder; `addrWitsSetDecoder`
        // (Alonzo/TxWits.hs) gates no-duplicate enforcement at `natVersion @12`.
        // At PV9-11 Haskell silently dedups (`nonEmptyDecoder` = `Set.fromList`)
        // and ACCEPTS the tx. Live mainnet is PV11, so this MUST decode Ok — a
        // hard-fail here would split the chain. dugite retains both physical
        // elements (len == 2) rather than deduping; the decode succeeds either
        // way, which is the consensus-relevant property.
        let vkw = cbor_arr(&[&cbor_bytes(&[0x44; 32]), &cbor_bytes(&[0x55; 64])]);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&vkw);
        set.extend(&vkw);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(0)); // key 0 = vkey_witnesses
        data.extend(&set);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r, Era::Conway)
            .expect("duplicate vkey witness must decode Ok at PV9-11 (Haskell accepts)");
        assert_eq!(
            ws.vkey_witnesses.len(),
            2,
            "lenient read_set retains both physical vkey witnesses"
        );
    }

    #[test]
    fn conway_witness_set_duplicate_native_script_accepted_lenient() {
        // map(1) { 1: tag(258) [ns, ns] } — the SAME native script twice.
        //
        // `nativeScriptsDecoder` (Alonzo/TxWits.hs) gates no-duplicate
        // enforcement at `natVersion @12`; at PV9-11 it uses `decodeNonEmptyList`
        // + `Map.fromList` which silently dedups and ACCEPTS. Must decode Ok.
        // native_script [0, keyhash(28)] = ScriptPubkey.
        let ns = cbor_arr(&[&cbor_uint(0), &cbor_bytes(&[0x88; 28])]);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&ns);
        set.extend(&ns);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(1)); // key 1 = native_scripts
        data.extend(&set);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r, Era::Conway)
            .expect("duplicate native script must decode Ok at PV9-11 (Haskell accepts)");
        assert_eq!(
            ws.native_scripts.len(),
            2,
            "lenient read_set retains both physical native scripts"
        );
    }

    #[test]
    fn conway_witness_set_duplicate_bootstrap_accepted_lenient() {
        // map(1) { 2: tag(258) [bw, bw] } — the SAME bootstrap witness twice.
        //
        // Bootstrap witnesses share `addrWitsSetDecoder` with vkey witnesses, so
        // the no-duplicate rule is also gated at `natVersion @12`. At PV9-11
        // Haskell silently dedups and ACCEPTS. Must decode Ok.
        // bootstrap_witness = [vkey(32), sig(64), chain_code(32), attrs(bytes)].
        let bw = cbor_arr(&[
            &cbor_bytes(&[0x44; 32]),
            &cbor_bytes(&[0x55; 64]),
            &cbor_bytes(&[0x66; 32]),
            &cbor_bytes(&[0x77; 8]),
        ]);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&bw);
        set.extend(&bw);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(2)); // key 2 = bootstrap_witnesses
        data.extend(&set);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r, Era::Conway)
            .expect("duplicate bootstrap witness must decode Ok at PV9-11 (Haskell accepts)");
        assert_eq!(
            ws.bootstrap_witnesses.len(),
            2,
            "lenient read_set retains both physical bootstrap witnesses"
        );
    }

    #[test]
    fn conway_witness_set_duplicate_plutus_data_accepted_lenient() {
        // map(1) { 4: tag(258) [pd, pd] } — the SAME plutus datum twice.
        //
        // `TxDatsRaw` DecCBOR (Alonzo/TxWits.hs) gates `noDuplicatesDatsDecoder`
        // at `natVersion @12`; at PV9-11 it uses `decodeNonEmptyList` +
        // `Map.fromElems hashData` which silently dedups by hash and ACCEPTS.
        // Must decode Ok. plutus_data = a small int constant here.
        let pd = cbor_uint(42);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&pd);
        set.extend(&pd);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(4)); // key 4 = plutus_data
        data.extend(&set);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r, Era::Conway)
            .expect("duplicate plutus datum must decode Ok at PV9-11 (Haskell accepts)");
        assert_eq!(
            ws.plutus_data.len(),
            2,
            "lenient read_set retains both physical plutus data"
        );
    }

    #[test]
    fn conway_block_invalid_tx_indices_duplicate_accepted_lenient() {
        // The block `invalid_transactions` field is a plain CDDL list
        // (`invalid_transactions : [* transaction_index]`), decoded by the
        // reused `AlonzoBlockBody` via plain `decodeList` + a range-check ONLY —
        // there is NO no-duplicate enforcement at any protocol version. So
        // duplicate indices `[0, 0]` must decode Ok (the same expression the
        // block decoder uses: `r.read_set(|r| r.read_uint())`).
        //
        // We exercise the exact decode expression directly rather than building
        // a full Conway header, which is not constructible by hand in a unit
        // test.
        let data = cbor_arr(&[&cbor_uint(0), &cbor_uint(0)]); // [0, 0]
        let mut r = Reader::new(&data);
        let indices: Vec<u64> = r
            .read_set(|r| r.read_uint())
            .expect("duplicate invalid-tx indices must decode Ok (plain list, no no-dup)");
        assert_eq!(
            indices,
            vec![0, 0],
            "lenient read_set retains both duplicate indices"
        );
    }

    #[test]
    fn conway_witness_set_unique_vkeys_accepted() {
        // Two distinct vkeywitnesses decode Ok and preserve wire order.
        let vkw_a = cbor_arr(&[&cbor_bytes(&[0x44; 32]), &cbor_bytes(&[0x55; 64])]);
        let vkw_b = cbor_arr(&[&cbor_bytes(&[0x66; 32]), &cbor_bytes(&[0x77; 64])]);
        let mut set = tag_258();
        set.extend(vec![0x82]); // array(2)
        set.extend(&vkw_a);
        set.extend(&vkw_b);

        let mut data = vec![0xa1]; // map(1)
        data.extend(cbor_uint(0));
        data.extend(&set);

        let mut r = Reader::new(&data);
        let ws = decode_conway_witness_set(&mut r, Era::Conway).expect("unique vkeys decode");
        assert_eq!(ws.vkey_witnesses.len(), 2);
        assert_eq!(ws.vkey_witnesses[0].vkey, vec![0x44; 32]);
        assert_eq!(ws.vkey_witnesses[1].vkey, vec![0x66; 32]);
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
        let body = decode_conway_tx_body(&mut r, Era::Conway).unwrap();
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
        let pp = read_proposal_procedure(&mut r, Era::Conway).unwrap();
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

    // ── #1023: Conway hard-rejects MIR (tag 6) and GenesisKeyDelegation
    //    (tag 5) at decode — oracle-verified `ConwayTxCert.hs:719-726`
    //    (`IntersectMBO/cardano-ledger@4849c13d6f70e5ab46add9af6e0ec5c537b61f69`).
    //    Pre-Conway eras keep accepting both unchanged (see
    //    `era_shelley.rs`/`era_alonzo.rs`/`era_babbage.rs` tests) — this is a
    //    hard era boundary, not a blanket removal.

    #[test]
    fn conway_certificate_tag5_genesis_key_delegation_rejected() {
        // Same wire shape as the real mainnet Shelley cert (block 7492516,
        // slot 66137371) exercised by
        // `era_shelley::shelley_genesis_key_delegation_real_mainnet_cert` —
        // proves this is a REAL cert type Haskell no longer decodes in
        // Conway, not a synthetic edge case.
        let mut data = vec![0x84]; // array(4)
        data.extend(cbor_uint(5));
        data.extend(cbor_bytes(&[0x01; 28])); // genesis key hash
        data.extend(cbor_bytes(&[0x02; 28])); // delegate key hash
        data.extend(cbor_bytes(&[0x03; 32])); // vrf keyhash

        let mut r = Reader::new(&data);
        let err = read_conway_certificate(&mut r)
            .expect_err("Conway must hard-reject GenesisKeyDelegation (tag 5)");
        let msg = err.to_string();
        assert!(
            msg.contains("Genesis delegation certificates are no longer supported"),
            "error message must mirror Haskell's ConwayTxCert.hs fail text, got: {msg}"
        );
    }

    #[test]
    fn conway_certificate_tag6_mir_rejected() {
        // [6, [0, 9999]] — MIR OtherAccountingPot, Reserves -> Treasury.
        let mut data = vec![0x82]; // outer array(2)
        data.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(0)); // source = Reserves
        mir.extend(cbor_uint(9999)); // target = coin
        data.extend(&mir);

        let mut r = Reader::new(&data);
        let err = read_conway_certificate(&mut r).expect_err("Conway must hard-reject MIR (tag 6)");
        let msg = err.to_string();
        assert!(
            msg.contains("MIR certificates are no longer supported"),
            "error message must mirror Haskell's ConwayTxCert.hs fail text, got: {msg}"
        );
    }

    #[test]
    fn conway_certificate_tag6_mir_stake_credentials_variant_also_rejected() {
        // Confirm rejection happens before any target-shape parsing (map
        // form), not just the simpler uint-coin form above.
        let mut data = vec![0x82];
        data.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(0));
        mir.push(0xa1); // map(1)
        mir.push(0x82);
        mir.extend(cbor_uint(0));
        mir.extend(cbor_bytes(&[0x09; 28]));
        mir.push(0x20); // delta = -1
        data.extend(&mir);

        let mut r = Reader::new(&data);
        assert!(read_conway_certificate(&mut r).is_err());
    }

    /// Every OTHER Conway certificate tag (the full valid set, 0-4 and
    /// 7-18) must still decode — this is the companion guard to the two
    /// reject tests above: the fix must be scoped to exactly tags 5/6, not
    /// accidentally widen into a stricter unknown-tag rejection or leave a
    /// sibling tag behind (#1012/#1013's "fixed one instance, left the rest"
    /// trap). Complements `every_conway_certificate_round_trips_through_our_own_decoder`
    /// in `encode/certificate.rs` (encoder round-trip); this one exercises
    /// every tag directly against hand-built wire bytes.
    #[test]
    fn conway_certificate_every_other_tag_still_decodes() {
        let cred_bytes = |b: u8| {
            let mut c = vec![0x82];
            c.extend(cbor_uint(0));
            c.extend(cbor_bytes(&[b; 28]));
            c
        };
        let drep_abstain = {
            let mut d = vec![0x81];
            d.extend(cbor_uint(2));
            d
        };

        let cases: Vec<(u64, Vec<u8>)> = vec![
            // 0: StakeRegistration [0, cred]
            (0, {
                let mut d = vec![0x82];
                d.extend(cbor_uint(0));
                d.extend(cred_bytes(0x01));
                d
            }),
            // 1: StakeDeregistration [1, cred]
            (1, {
                let mut d = vec![0x82];
                d.extend(cbor_uint(1));
                d.extend(cred_bytes(0x02));
                d
            }),
            // 2: StakeDelegation [2, cred, pool_hash]
            (2, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(2));
                d.extend(cred_bytes(0x03));
                d.extend(cbor_bytes(&[0x04; 28]));
                d
            }),
            // 4: PoolRetirement [4, pool_hash, epoch]
            (4, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(4));
                d.extend(cbor_bytes(&[0x05; 28]));
                d.extend(cbor_uint(500));
                d
            }),
            // 7: ConwayStakeRegistration [7, cred, deposit]
            (7, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(7));
                d.extend(cred_bytes(0x06));
                d.extend(cbor_uint(2_000_000));
                d
            }),
            // 8: ConwayStakeDeregistration [8, cred, refund]
            (8, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(8));
                d.extend(cred_bytes(0x07));
                d.extend(cbor_uint(2_000_000));
                d
            }),
            // 9: VoteDelegation [9, cred, drep]
            (9, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(9));
                d.extend(cred_bytes(0x08));
                d.extend(&drep_abstain);
                d
            }),
            // 10: StakeVoteDelegation [10, cred, pool_hash, drep]
            (10, {
                let mut d = vec![0x84];
                d.extend(cbor_uint(10));
                d.extend(cred_bytes(0x09));
                d.extend(cbor_bytes(&[0x0a; 28]));
                d.extend(&drep_abstain);
                d
            }),
            // 11: RegStakeDeleg [11, cred, pool_hash, deposit]
            (11, {
                let mut d = vec![0x84];
                d.extend(cbor_uint(11));
                d.extend(cred_bytes(0x0b));
                d.extend(cbor_bytes(&[0x0c; 28]));
                d.extend(cbor_uint(2_000_000));
                d
            }),
            // 12: VoteRegDeleg [12, cred, drep, deposit]
            (12, {
                let mut d = vec![0x84];
                d.extend(cbor_uint(12));
                d.extend(cred_bytes(0x0d));
                d.extend(&drep_abstain);
                d.extend(cbor_uint(2_000_000));
                d
            }),
            // 13: RegStakeVoteDeleg [13, cred, pool_hash, drep, deposit]
            (13, {
                let mut d = vec![0x85];
                d.extend(cbor_uint(13));
                d.extend(cred_bytes(0x0e));
                d.extend(cbor_bytes(&[0x0f; 28]));
                d.extend(&drep_abstain);
                d.extend(cbor_uint(2_000_000));
                d
            }),
            // 14: CommitteeHotAuth [14, cold_cred, hot_cred]
            (14, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(14));
                d.extend(cred_bytes(0x10));
                d.extend(cred_bytes(0x11));
                d
            }),
            // 15: CommitteeColdResign [15, cold_cred, null]
            (15, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(15));
                d.extend(cred_bytes(0x12));
                d.push(0xf6); // null anchor
                d
            }),
            // 16: RegDRep [16, cred, deposit, null]
            (16, {
                let mut d = vec![0x84];
                d.extend(cbor_uint(16));
                d.extend(cred_bytes(0x13));
                d.extend(cbor_uint(500_000_000));
                d.push(0xf6);
                d
            }),
            // 17: UnregDRep [17, cred, refund]
            (17, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(17));
                d.extend(cred_bytes(0x14));
                d.extend(cbor_uint(500_000_000));
                d
            }),
            // 18: UpdateDRep [18, cred, null]
            (18, {
                let mut d = vec![0x83];
                d.extend(cbor_uint(18));
                d.extend(cred_bytes(0x15));
                d.push(0xf6);
                d
            }),
        ];

        let mut failures = Vec::new();
        for (tag, bytes) in &cases {
            let mut r = Reader::new(bytes);
            if let Err(e) = read_conway_certificate(&mut r) {
                failures.push(format!("tag {tag} FAILED to decode: {e}"));
            }
        }
        assert!(
            failures.is_empty(),
            "every non-removed Conway cert tag must still decode:\n  {}",
            failures.join("\n  ")
        );

        // Tag 3 (PoolRegistration) has a large variable-length body
        // (pool_params) that's awkward to hand-encode inline above; cover it
        // via the real encoder instead, matching the pattern used by
        // `every_conway_certificate_round_trips_through_our_own_decoder`.
        let pool_cert = Certificate::PoolRegistration(dugite_primitives::transaction::PoolParams {
            operator: Hash28::from_bytes([0x20; 28]),
            vrf_keyhash: Hash32::from_bytes([0x21; 32]),
            pledge: Lovelace(1_000_000),
            cost: Lovelace(340_000_000),
            margin: Rational {
                numerator: 1,
                denominator: 20,
            },
            reward_account: vec![0xe0; 29],
            pool_owners: vec![Hash28::from_bytes([0x22; 28])],
            relays: vec![],
            pool_metadata: None,
        });
        let enc = crate::encode_certificate(&pool_cert);
        let mut r = Reader::new(&enc);
        read_conway_certificate(&mut r).expect("tag 3 PoolRegistration must still decode");
    }

    /// End-to-end guard at the actual mempool-submission entry point
    /// (`decode_transaction`), not just the isolated cert reader — proves a
    /// Conway tx carrying a MIR cert is rejected exactly where N2C tx
    /// submission would reject it.
    #[test]
    fn conway_standalone_tx_with_mir_cert_rejected_at_decode() {
        // Minimal Conway tx body: just certificates (key 4) = [MIR cert].
        let mut mir_cert = vec![0x82];
        mir_cert.extend(cbor_uint(6));
        let mut mir = vec![0x82];
        mir.extend(cbor_uint(0));
        mir.extend(cbor_uint(9999));
        mir_cert.extend(&mir);

        let mut body = vec![0xa1]; // map(1)
        body.extend(cbor_uint(4));
        body.push(0x81); // plain array(1) of certs — read_set_strict accepts untagged
        body.extend(&mir_cert);

        let mut tx = vec![0x84]; // [body, witness_set, is_valid, aux]
        tx.extend(&body);
        tx.push(0xa0); // empty witness set
        tx.push(0xf5); // is_valid = true
        tx.push(0xf6); // no aux data

        let result = crate::decode::decode_transaction(6, &tx);
        assert!(
            result.is_err(),
            "Conway standalone tx with a MIR cert must be rejected at decode"
        );
        assert!(result.unwrap_err().to_string().contains("MIR"));
    }

    /// Same end-to-end guard for Dijkstra (era_id 7) — `read_conway_certificate`
    /// is shared between Conway and Dijkstra tx-body decode (both top-level
    /// `certificates` and sub-tx `certificates`), and Dijkstra's own
    /// `DijkstraTxCert` decoder independently hard-rejects tags 5/6 with the
    /// identical messages (oracle-verified
    /// `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxCert.hs`), so the
    /// shared decoder is correct for both eras.
    #[test]
    fn dijkstra_standalone_tx_with_genesis_key_delegation_cert_rejected_at_decode() {
        let mut cert = vec![0x84];
        cert.extend(cbor_uint(5));
        cert.extend(cbor_bytes(&[0x01; 28]));
        cert.extend(cbor_bytes(&[0x02; 28]));
        cert.extend(cbor_bytes(&[0x03; 32]));

        let mut body = vec![0xa1];
        body.extend(cbor_uint(4));
        body.push(0x81);
        body.extend(&cert);

        // Dijkstra standalone tx (CIP-0167): [body, witness_set, aux] — 3 elements.
        let mut tx = vec![0x83];
        tx.extend(&body);
        tx.push(0xa0);
        tx.push(0xf6);

        let result = crate::decode::decode_transaction(7, &tx);
        assert!(
            result.is_err(),
            "Dijkstra standalone tx with a GenesisKeyDelegation cert must be rejected at decode"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Genesis delegation"));
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
        let ppu = read_protocol_param_update(&mut r, Era::Conway).unwrap();
        assert_eq!(ppu.min_fee_a, Some(44));
        assert_eq!(ppu.min_fee_b, Some(155381));
        assert_eq!(ppu.min_pool_cost, None);
    }

    // ── #1012: indefinite-length ProtocolParamUpdate maps ──────────────────────
    //
    // `read_protocol_param_update` used to drive its key loop from
    // `read_map_header()?.unwrap_or(0)`, so an indefinite-length map (`None`)
    // decoded as ZERO entries and left the reader desynced for everything
    // after it. This is the tx-body key 6 / governance `ParameterChange`
    // decoder, so a `ParameterChange` proposal spanning more than 23 keys
    // (crossing `cardano-ledger-binary`'s `encodeMap` indefinite threshold,
    // #932/#938) would install NO parameters at all — a consensus divergence.
    //
    // Oracle-verified against `IntersectMBO/cardano-ledger` at pinned SHA
    // `4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (confirmed resolving via
    // `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`):
    // `DecCBOR (PParamsUpdate era)` (`libs/cardano-ledger-core/src/Cardano/
    // Ledger/Core/PParams.hs`) decodes via `decodeSparse`, which calls
    // `decodeMapLenOrIndef` and, on `Nothing`, loops via `getSparseBlockIndef`
    // until the CBOR break byte — both forms are honored on the Haskell side.
    //
    // These fixtures are HAND-BUILT bytes, not `encode(decode(x))` round
    // trips: dugite's own PPU encoder (`encode_ppu_map` in
    // `encode/protocol_params.rs`) is, correctly per the same oracle lookup,
    // ALWAYS definite-length (`encodeMapLen count` on the Haskell side has no
    // size-dependent branching, unlike the general `encodeMap` #932/#938
    // covers) — so a same-process round trip through dugite's own encoder can
    // NEVER produce an indefinite-length PPU map and could not have caught
    // this. Only a hand-built or Haskell-derived fixture reaches it, per
    // CLAUDE.md's standing round-trip caveat.

    /// Every key valid under `Era::Conway` (issue #1013 — keys 34-37 are
    /// Dijkstra-only and must NOT appear here; see [`dijkstra_only_ppu_entries`]
    /// for those). 30 entries — past the 23-entry indefinite threshold so
    /// this fixture crosses it with real (not padding) keys.
    fn conway_ppu_entries() -> Vec<(u64, Vec<u8>)> {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0, cbor_uint(44)),
            (1, cbor_uint(155381)),
            (2, cbor_uint(90112)),
            (3, cbor_uint(16384)),
            (4, cbor_uint(1100)),
            (5, cbor_uint(2_000_000)),
            (6, cbor_uint(500_000_000)),
            (7, cbor_uint(18)),
            (8, cbor_uint(500)),
            (9, cbor_rational(3, 10)),
            (10, cbor_rational(3, 1000)),
            (11, cbor_rational(2, 10)),
            (16, cbor_uint(340_000_000)),
            (17, cbor_uint(4310)),
            (18, cbor_map0()),
            (
                19,
                cbor_arr(&[&cbor_rational(577, 10_000), &cbor_rational(721, 10_000_000)]),
            ),
            (
                20,
                cbor_arr(&[&cbor_uint(14_000_000), &cbor_uint(10_000_000_000)]),
            ),
            (
                21,
                cbor_arr(&[&cbor_uint(62_000_000), &cbor_uint(20_000_000_000)]),
            ),
            (22, cbor_uint(5000)),
            (23, cbor_uint(150)),
            (24, cbor_uint(3)),
            (
                25,
                cbor_arr(&[
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                ]),
            ),
            (
                26,
                cbor_arr(&[
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                    &cbor_rational(1, 2),
                ]),
            ),
            (27, cbor_uint(7)),
            (28, cbor_uint(146)),
            (29, cbor_uint(6)),
            (30, cbor_uint(100_000_000_000)),
            (31, cbor_uint(500_000_000)),
            (32, cbor_uint(20)),
            (33, cbor_rational(15, 100)),
        ];
        assert!(
            entries.len() > 23,
            "fixture must cross the encodeMap indefinite threshold"
        );
        entries
    }

    /// `conway_ppu_entries()` plus the 4 Dijkstra-only additions (34-37) —
    /// valid under `Era::Dijkstra` only (issue #1013).
    fn dijkstra_ppu_entries() -> Vec<(u64, Vec<u8>)> {
        let mut entries = conway_ppu_entries();
        entries.push((34, cbor_uint(200_000)));
        entries.push((35, cbor_uint(1_000_000)));
        entries.push((36, cbor_uint(25_600)));
        entries.push((37, cbor_rational(12, 10)));
        entries
    }

    fn ppu_map_definite(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
        // Mirrors `cbor.rs::encode_map_header` for n in [24, 256).
        let n = entries.len();
        assert!(n < 256, "fixture helper only covers small map headers");
        let mut v = if n < 24 {
            vec![0xa0 | n as u8]
        } else {
            vec![0xb8, n as u8]
        };
        for (key, value) in entries {
            v.extend(cbor_uint(*key));
            v.extend(value.clone());
        }
        v
    }

    fn ppu_map_indefinite(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut v = vec![0xbf]; // indefinite map open
        for (key, value) in entries {
            v.extend(cbor_uint(*key));
            v.extend(value.clone());
        }
        v.push(0xff); // break
        v
    }

    #[test]
    fn read_protocol_param_update_accepts_indefinite_length_map_minimal() {
        // map { 7: 100, 8: 150 } as an indefinite map — parity with the
        // sibling fix's minimal reproducer (faaaed42d8 /
        // `pre_conway_ppu_accepts_indefinite_length_map`).
        let entries = vec![(7u64, cbor_uint(100)), (8u64, cbor_uint(150))];
        let cbor = ppu_map_indefinite(&entries);
        let mut r = Reader::new(&cbor);
        let ppu = read_protocol_param_update(&mut r, Era::Conway)
            .expect("indefinite ProtocolParamUpdate map must decode");
        assert_eq!(
            ppu.e_max,
            Some(100),
            "PRE-FIX this decoded as None (0 entries read)"
        );
        assert_eq!(ppu.n_opt, Some(150));
    }

    #[test]
    fn read_protocol_param_update_accepts_indefinite_length_map_over_23_keys() {
        // The consensus-relevant case: a `ParameterChange` spanning more keys
        // than the 23-entry `encodeMap` indefinite-form threshold. Assert the
        // indefinite-form decode equals the definite-form decode of the exact
        // same logical entries — not merely "decodes without error".
        let entries = conway_ppu_entries();

        let definite_cbor = ppu_map_definite(&entries);
        let mut dr = Reader::new(&definite_cbor);
        let definite_ppu = read_protocol_param_update(&mut dr, Era::Conway)
            .expect("definite-form >23-key PPU map must decode");

        let indefinite_cbor = ppu_map_indefinite(&entries);
        let mut ir = Reader::new(&indefinite_cbor);
        let indefinite_ppu = read_protocol_param_update(&mut ir, Era::Conway)
            .expect("indefinite-form >23-key PPU map must decode");

        assert_eq!(
            indefinite_ppu,
            definite_ppu,
            "PRE-FIX the indefinite form decoded as a near-empty default ProtocolParamUpdate \
             (every key silently dropped) while the definite form carried all {} entries",
            entries.len()
        );
        // Sanity: this is not a vacuous equality between two empty defaults —
        // confirm the fixture actually populated the fields both sides agree on.
        assert_eq!(definite_ppu.min_fee_a, Some(44));
        assert!(definite_ppu.min_fee_ref_script_cost_per_byte.is_some());
        assert_ne!(definite_ppu, ProtocolParamUpdate::default());
    }

    /// Issue #1013: the Dijkstra counterpart of the test above — the same
    /// past-23-key indefinite-map coverage, but including keys 34-37 and
    /// decoded under `Era::Dijkstra`. Proves the era-gating added for #1013
    /// does not regress #1012's indefinite-map fix for the Dijkstra-only keys.
    #[test]
    fn read_protocol_param_update_accepts_indefinite_length_map_dijkstra_over_23_keys() {
        let entries = dijkstra_ppu_entries();

        let definite_cbor = ppu_map_definite(&entries);
        let mut dr = Reader::new(&definite_cbor);
        let definite_ppu = read_protocol_param_update(&mut dr, Era::Dijkstra)
            .expect("definite-form >23-key Dijkstra PPU map must decode");

        let indefinite_cbor = ppu_map_indefinite(&entries);
        let mut ir = Reader::new(&indefinite_cbor);
        let indefinite_ppu = read_protocol_param_update(&mut ir, Era::Dijkstra)
            .expect("indefinite-form >23-key Dijkstra PPU map must decode");

        assert_eq!(indefinite_ppu, definite_ppu);
        assert!(definite_ppu.ref_script_cost_multiplier.is_some());
        assert!(definite_ppu.max_ref_script_size_per_block.is_some());
        assert_ne!(definite_ppu, ProtocolParamUpdate::default());
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

    // ── Era-aware unknown / invalid tx-body key rejection (#31-B) ──────────────

    #[test]
    fn test_dijkstra_unknown_tx_body_key_rejected() {
        // map(2) { 2: fee, 99: some_unknown_value }
        // Key 99 is NOT in Dijkstra bodyFields — must hard-reject (upstream
        // SparseKeyed catch-all: decoderByKey _ -> Nothing -> failMsg).
        let mut data = vec![0xa2];
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(42)); // fee
        data.extend(cbor_uint(99)); // unknown key
        data.extend(cbor_uint(0)); // unknown value (just a uint)

        let mut r = Reader::new(&data);
        let result = decode_conway_tx_body(&mut r, Era::Dijkstra);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "Dijkstra unknown tx-body key 99 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn conway_tx_body_rejects_dijkstra_only_keys() {
        // Conway bodyFields has NO key 23/25/26 — each must hard-reject when the
        // body is decoded in the Conway era (the Dijkstra-only arms are guarded).
        for key in [23u64, 25, 26] {
            // map(2) { 2: fee, <key>: {} }  (value is an empty map — never read
            // because Conway rejects on the key before parsing the value).
            let mut data = vec![0xa2];
            data.extend(cbor_uint(2));
            data.extend(cbor_uint(1)); // fee
            data.extend(cbor_uint(key));
            data.extend(vec![0xa0]); // empty map value

            let mut r = Reader::new(&data);
            let result = decode_conway_tx_body(&mut r, Era::Conway);
            assert!(
                matches!(result, Err(SerializationError::CborDecode(_))),
                "Conway must reject Dijkstra-only tx-body key {key}, got {result:?}"
            );
        }
    }

    #[test]
    fn dijkstra_tx_body_accepts_23_25_26() {
        // A Dijkstra body carrying keys 23 (sub_transactions), 25
        // (direct_deposits) and 26 (account_balance_intervals) — each value is
        // the minimal valid wire form — must decode cleanly.
        // map(4) { 2: fee, 23: [], 25: {}, 26: {} }
        //
        // Key 23 is an OMap, which Haskell encodes as a bare ARRAY of values
        // (`encodeStrictSeq encCBOR (toStrictSeq omap)`) — not a map (#936).
        let mut data = vec![0xa4];
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(7)); // fee
        data.extend(cbor_uint(23));
        data.extend(vec![0x80]); // sub_transactions: empty OMap == empty array
        data.extend(cbor_uint(25));
        data.extend(vec![0xa0]); // direct_deposits: empty map
        data.extend(cbor_uint(26));
        data.extend(vec![0xa0]); // account_balance_intervals: empty map

        let mut r = Reader::new(&data);
        let body = decode_conway_tx_body(&mut r, Era::Dijkstra)
            .expect("Dijkstra body with keys 23/25/26 must decode");
        assert_eq!(body.fee, Lovelace(7));
        assert!(body.sub_transactions.is_empty());
        assert!(body.direct_deposits.is_empty());
        assert!(body.account_balance_intervals.is_empty());
    }

    // ── #1010: unmodelled Dijkstra SubTx body keys must hard-reject ────────────
    //
    // Before #1010 part 1, `decode_sub_tx_body`'s catch-all silently
    // `r.skip()`-ped any key it didn't model. Part 2 then added the full
    // field set EXCEPT key 24 (`required_top_level_guards`, a Dijkstra-only
    // concept with no Conway analog and a wire shape not yet modelled — see
    // the doc comment on `SubTransaction`), which remains a genuine,
    // documented, fail-closed gap. certs/withdrawals/mint/guards/
    // network_id/voting+proposal procedures/treasury_value/donation/
    // direct_deposits/account_balance_intervals/script_integrity_hash are
    // now all modelled — see `dijkstra_sub_tx_body_round_trips_every_field`
    // below.

    /// Build a minimal valid Dijkstra SubTx body CBOR map carrying only keys
    /// 0 (inputs) and 1 (outputs), both empty, plus one EXTRA key/value pair.
    fn sub_tx_body_with_extra_key(extra_key: u64, extra_value: &[u8]) -> Vec<u8> {
        let mut data = vec![0xa3]; // map(3)
        data.extend(cbor_uint(0));
        data.extend(vec![0x80]); // inputs: empty array
        data.extend(cbor_uint(1));
        data.extend(vec![0x80]); // outputs: empty array
        data.extend(cbor_uint(extra_key));
        data.extend_from_slice(extra_value);
        data
    }

    #[test]
    fn dijkstra_sub_tx_body_rejects_unmodelled_key_24_required_top_level_guards() {
        // Key 24 (`required_top_level_guards`) — the one field #1010 part 2
        // deliberately left unmodelled (no Conway analog, non-obvious wire
        // shape). An empty map is a syntactically plausible value; the
        // decoder must still reject purely because key 24 is unmodelled.
        let data = sub_tx_body_with_extra_key(24, &[0xa0]);
        let mut r = Reader::new(&data);
        let result = decode_sub_tx_body(&mut r);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "sub-tx body with unmodelled key 24 must be rejected, got {result:?}"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("24") && msg.contains("#1010"),
            "error must name the offending key and the tracking issue, got: {msg}"
        );
    }

    #[test]
    fn dijkstra_sub_tx_body_rejects_topx_only_keys() {
        // Keys 2 (fee), 13 (collateral), 16 (collateral_return), 17
        // (total_collateral), 23 (sub_transactions — no nested sub-txs), 27
        // (starting_account_balance_intervals) are GADT-excluded from
        // DijkstraSubTxBodyRaw entirely — a sub-tx cannot carry them at all,
        // not even in principle. Each must hard-reject.
        for key in [2u64, 13, 16, 17, 23, 27] {
            let data = sub_tx_body_with_extra_key(key, &[0x00]);
            let mut r = Reader::new(&data);
            let result = decode_sub_tx_body(&mut r);
            assert!(
                matches!(result, Err(SerializationError::CborDecode(_))),
                "sub-tx body with TopTx-only key {key} must be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn dijkstra_sub_tx_body_round_trips_flat_fields() {
        // The flat/scalar #1010-part-2 keys (9 mint, 11 script_integrity_hash,
        // 15 network_id, 21 treasury_value, 22 donation, 5 withdrawals, 25
        // direct_deposits), hand-built per the oracle-verified key table
        // (NOT a same-process round trip) so a shared wrong shape on both
        // halves cannot hide behind agreement (#951 caveat). The nested/
        // compound fields (4 certs, 14 guards, 19/20 voting+proposal
        // procedures, 26 account_balance_intervals) are covered by
        // `dijkstra_sub_tx_body_round_trips_compound_fields_via_typed_encoder`
        // below instead — those reuse the SAME already-independently-tested
        // sub-encoders the top-level tx body uses (`encode_certificate`,
        // `encode_voting_procedures`, …), so hand-rolling their nested CBOR
        // byte-for-byte here would just be re-deriving already-covered
        // ground with more room to get a bracket wrong.
        let mut data = vec![0xaa]; // map(10): 0,1,3,5,9,11,15,21,22,25
        data.extend(cbor_uint(0));
        data.extend(vec![0x80]); // inputs
        data.extend(cbor_uint(1));
        data.extend(vec![0x80]); // outputs
        data.extend(cbor_uint(3));
        data.extend(cbor_uint(999)); // ttl
                                     // 5: withdrawals — { 29-byte reward_account => 5 }
        data.extend(cbor_uint(5));
        data.extend(vec![0xa1]);
        data.extend(cbor_bytes(&[0xe0; 29]));
        data.extend(cbor_uint(5));
        // 9: mint — { policy(28B) => { assetname => -3 } }
        data.extend(cbor_uint(9));
        data.extend(vec![0xa1]);
        data.extend(cbor_bytes(&[0x33; 28]));
        data.extend(vec![0xa1]);
        data.extend(cbor_bytes(b"tok"));
        data.extend(vec![0x22]); // -3
                                 // 11: script_integrity_hash (32B)
        data.extend(cbor_uint(11));
        data.extend(cbor_bytes(&[0x44; 32]));
        // 15: network_id
        data.extend(cbor_uint(15));
        data.extend(cbor_uint(1));
        // 21: treasury_value
        data.extend(cbor_uint(21));
        data.extend(cbor_uint(123_456));
        // 22: donation
        data.extend(cbor_uint(22));
        data.extend(cbor_uint(789));
        // 25: direct_deposits — { 29-byte reward_account => 7 }
        data.extend(cbor_uint(25));
        data.extend(vec![0xa1]);
        data.extend(cbor_bytes(&[0xe1; 29]));
        data.extend(cbor_uint(7));

        let mut r = Reader::new(&data);
        let sub = decode_sub_tx_body(&mut r).expect("flat-field sub-tx body must decode");

        assert_eq!(sub.ttl.map(|s| s.0), Some(999));
        assert_eq!(sub.withdrawals.len(), 1, "withdrawals (key 5)");
        assert_eq!(sub.mint.len(), 1, "mint (key 9)");
        assert_eq!(
            sub.script_data_hash,
            Some(Hash32::from_bytes([0x44; 32])),
            "script_integrity_hash (key 11)"
        );
        assert_eq!(sub.network_id, Some(1), "network_id (key 15)");
        assert_eq!(sub.treasury_value.map(|c| c.0), Some(123_456), "key 21");
        assert_eq!(sub.donation.map(|c| c.0), Some(789), "key 22");
        assert_eq!(sub.direct_deposits.len(), 1, "direct_deposits (key 25)");
    }

    #[test]
    fn dijkstra_sub_tx_body_round_trips_compound_fields_via_typed_encoder() {
        // Certs (4), guards (14), voting_procedures (19), proposal_procedures
        // (20), account_balance_intervals (26) — built as typed values and
        // pushed through the REAL `encode_sub_tx_body` (oracle-verified key
        // numbers, and the exact same `encode_certificate`/
        // `encode_voting_procedures`/`encode_ordered_set` calls the
        // top-level tx body already uses and is independently fixture-tested
        // against, elsewhere in this crate) then the real `decode_sub_tx_body`.
        // This is a same-process round trip for the COMPOUND SHAPES (the
        // #951 caveat applies to those specifically), but the key NUMBERS
        // are the oracle-verified ones already asserted individually in
        // `dijkstra_sub_tx_body_round_trips_flat_fields` and the
        // `dijkstra_sub_tx_body_rejects_*` tests, and the sub-encoders
        // themselves are shared, previously-verified code, not new guesses.
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;
        use dugite_primitives::transaction::{
            AccountBalanceInterval, GovActionId, ProposalProcedure, SubTransaction, Vote, Voter,
            VotingProcedure,
        };
        use dugite_primitives::value::Lovelace;
        use std::collections::BTreeMap;

        let cred = Credential::VerificationKey(Hash28::from_bytes([0x21; 28]));
        let mut voting_procedures = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: Hash32::from_bytes([0x77; 32]),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::No,
                anchor: None,
            },
        );
        voting_procedures.insert(Voter::DRep(cred.clone()), inner);

        let sub = SubTransaction {
            inputs: vec![],
            outputs: vec![],
            certificates: vec![Certificate::StakeRegistration(cred.clone())],
            guards: vec![cred.clone()],
            voting_procedures,
            proposal_procedures: vec![ProposalProcedure {
                deposit: Lovelace(500_000_000),
                return_addr: vec![0xe0; 29],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                anchor: dugite_primitives::transaction::Anchor {
                    url: "https://example.test".to_string(),
                    data_hash: Hash32::from_bytes([0x99; 32]),
                },
            }],
            account_balance_intervals: vec![(cred, AccountBalanceInterval::at_least(Lovelace(1)))],
            ..Default::default()
        };

        let encoded = crate::encode::transaction::encode_sub_tx_body(&sub);
        let mut r = Reader::new(&encoded);
        let decoded =
            decode_sub_tx_body(&mut r).expect("typed-encoded compound sub-tx body must decode");

        assert_eq!(decoded.certificates, sub.certificates, "certs (key 4)");
        assert_eq!(decoded.guards, sub.guards, "guards (key 14)");
        assert_eq!(
            decoded.voting_procedures, sub.voting_procedures,
            "voting_procedures (key 19)"
        );
        assert_eq!(
            decoded.proposal_procedures, sub.proposal_procedures,
            "proposal_procedures (key 20)"
        );
        assert_eq!(
            decoded.account_balance_intervals, sub.account_balance_intervals,
            "account_balance_intervals (key 26)"
        );
    }

    #[test]
    fn conway_tx_body_rejects_key6() {
        // Key 6 (pre-Conway `update`) is NOT in Conway OR Dijkstra bodyFields —
        // hard-rejected by BOTH eras (the old lenient `6 => r.skip()` arm is
        // deleted; key 6 falls through to the rejecting default).
        for era in [Era::Conway, Era::Dijkstra] {
            // map(2) { 2: fee, 6: 0 }
            let mut data = vec![0xa2];
            data.extend(cbor_uint(2));
            data.extend(cbor_uint(1)); // fee
            data.extend(cbor_uint(6)); // pre-Conway update key
            data.extend(cbor_uint(0)); // value (uint)

            let mut r = Reader::new(&data);
            let result = decode_conway_tx_body(&mut r, era);
            assert!(
                matches!(result, Err(SerializationError::CborDecode(_))),
                "{era:?} must reject tx-body key 6, got {result:?}"
            );
        }
    }

    // ── Voter (all discriminators 0-4 + error) ────────────────────────────

    #[test]
    fn voter_committee_key() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&[0xAA; 28]));
        let mut r = Reader::new(&data);
        let v = read_voter(&mut r).unwrap();
        assert!(matches!(
            v,
            Voter::ConstitutionalCommittee(Credential::VerificationKey(_))
        ));
    }

    #[test]
    fn voter_committee_script() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(1));
        data.extend(cbor_bytes(&[0xBB; 28]));
        let mut r = Reader::new(&data);
        let v = read_voter(&mut r).unwrap();
        assert!(matches!(
            v,
            Voter::ConstitutionalCommittee(Credential::Script(_))
        ));
    }

    #[test]
    fn voter_drep_key() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(2));
        data.extend(cbor_bytes(&[0xCC; 28]));
        let mut r = Reader::new(&data);
        let v = read_voter(&mut r).unwrap();
        assert!(matches!(v, Voter::DRep(Credential::VerificationKey(_))));
    }

    #[test]
    fn voter_drep_script() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(3));
        data.extend(cbor_bytes(&[0xDD; 28]));
        let mut r = Reader::new(&data);
        let v = read_voter(&mut r).unwrap();
        assert!(matches!(v, Voter::DRep(Credential::Script(_))));
    }

    #[test]
    fn voter_pool_padded_to_hash32() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(4));
        data.extend(cbor_bytes(&[0xEE; 28]));
        let mut r = Reader::new(&data);
        let v = read_voter(&mut r).unwrap();
        match v {
            Voter::StakePool(h32) => {
                assert_eq!(&h32.as_bytes()[..28], &[0xEE; 28]);
                assert_eq!(&h32.as_bytes()[28..], &[0u8; 4]);
            }
            _ => panic!("expected StakePool"),
        }
    }

    #[test]
    fn voter_unknown_disc_rejected() {
        let mut data = vec![0x82];
        data.extend(cbor_uint(5));
        data.extend(cbor_bytes(&[0; 28]));
        let mut r = Reader::new(&data);
        assert!(read_voter(&mut r).is_err());
    }

    #[test]
    fn voter_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_voter(&mut r).is_err());
    }

    // ── Anchor ────────────────────────────────────────────────────────────

    #[test]
    fn anchor_with_text_url() {
        let mut data = vec![0x82];
        data.push(0x63); // text(3)
        data.extend_from_slice(b"foo");
        data.extend(cbor_bytes(&[0x42; 32]));
        let mut r = Reader::new(&data);
        let a = read_anchor(&mut r).unwrap();
        assert_eq!(a.url, "foo");
        assert_eq!(a.data_hash.as_bytes(), &[0x42; 32]);
    }

    #[test]
    fn anchor_with_bytes_url_fallback() {
        let mut data = vec![0x82];
        data.extend(cbor_bytes(b"bar"));
        data.extend(cbor_bytes(&[0x42; 32]));
        let mut r = Reader::new(&data);
        let a = read_anchor(&mut r).unwrap();
        assert_eq!(a.url, "bar");
    }

    #[test]
    fn anchor_invalid_utf8_bytes_url_rejected() {
        let mut data = vec![0x82];
        data.extend(cbor_bytes(&[0xff, 0xff]));
        data.extend(cbor_bytes(&[0; 32]));
        let mut r = Reader::new(&data);
        assert!(read_anchor(&mut r).is_err());
    }

    #[test]
    fn anchor_wrong_arity_rejected() {
        let data = [0x83, 0x60, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_anchor(&mut r).is_err());
    }

    #[test]
    fn optional_anchor_null_yields_none() {
        let data = [0xf6u8];
        let mut r = Reader::new(&data);
        let a = read_optional_anchor(&mut r).unwrap();
        assert!(a.is_none());
    }

    #[test]
    fn optional_anchor_some_decodes_inner() {
        let mut data = vec![0x82];
        data.push(0x63);
        data.extend_from_slice(b"foo");
        data.extend(cbor_bytes(&[0; 32]));
        let mut r = Reader::new(&data);
        let a = read_optional_anchor(&mut r).unwrap();
        assert!(a.is_some());
    }

    // ── Vote / VotingProcedure / GovActionId ──────────────────────────────

    #[test]
    fn read_vote_all_values() {
        for (v, expected) in [(0u64, Vote::No), (1, Vote::Yes), (2, Vote::Abstain)] {
            let data = cbor_uint(v);
            let mut r = Reader::new(&data);
            assert_eq!(read_vote(&mut r).unwrap(), expected);
        }
    }

    #[test]
    fn read_vote_unknown_rejected() {
        let data = cbor_uint(99);
        let mut r = Reader::new(&data);
        assert!(read_vote(&mut r).is_err());
    }

    #[test]
    fn voting_procedure_decodes_with_null_anchor() {
        // [vote=1, null]
        let mut data = vec![0x82];
        data.extend(cbor_uint(1));
        data.push(0xf6);
        let mut r = Reader::new(&data);
        let vp = read_voting_procedure(&mut r).unwrap();
        assert_eq!(vp.vote, Vote::Yes);
        assert!(vp.anchor.is_none());
    }

    #[test]
    fn voting_procedure_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0xf6, 0xf6];
        let mut r = Reader::new(&data);
        assert!(read_voting_procedure(&mut r).is_err());
    }

    #[test]
    fn gov_action_id_decodes() {
        let mut data = vec![0x82];
        data.extend(cbor_bytes(&[0xAB; 32]));
        data.extend(cbor_uint(3));
        let mut r = Reader::new(&data);
        let id = read_gov_action_id(&mut r).unwrap();
        assert_eq!(id.action_index, 3);
        assert_eq!(id.transaction_id.as_bytes(), &[0xAB; 32]);
    }

    #[test]
    fn gov_action_id_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_gov_action_id(&mut r).is_err());
    }

    // ── Constitution + Anchor + Optional hash ─────────────────────────────

    #[test]
    fn constitution_with_script_hash() {
        // [anchor, hash28]
        let mut data = vec![0x82];
        let mut anchor = vec![0x82];
        anchor.push(0x63);
        anchor.extend_from_slice(b"foo");
        anchor.extend(cbor_bytes(&[0; 32]));
        data.extend(&anchor);
        data.extend(cbor_bytes(&[0x11; 28]));
        let mut r = Reader::new(&data);
        let c = read_constitution(&mut r).unwrap();
        assert!(c.script_hash.is_some());
    }

    #[test]
    fn constitution_with_null_script_hash() {
        let mut data = vec![0x82];
        let mut anchor = vec![0x82];
        anchor.push(0x63);
        anchor.extend_from_slice(b"foo");
        anchor.extend(cbor_bytes(&[0; 32]));
        data.extend(&anchor);
        data.push(0xf6);
        let mut r = Reader::new(&data);
        let c = read_constitution(&mut r).unwrap();
        assert!(c.script_hash.is_none());
    }

    #[test]
    fn constitution_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_constitution(&mut r).is_err());
    }

    // ── Cost models + ex_unit_prices + ex_units ───────────────────────────

    #[test]
    fn cost_models_all_three_versions() {
        // {0: [...], 1: [...], 2: [...]}
        let mut data = vec![0xa3];
        data.extend(cbor_uint(0));
        data.push(0x82); // array(2)
        data.extend(cbor_uint(100));
        data.extend(cbor_uint(200));
        data.extend(cbor_uint(1));
        data.push(0x81);
        data.extend(cbor_uint(50));
        data.extend(cbor_uint(2));
        data.push(0x81);
        data.extend(cbor_uint(75));
        let mut r = Reader::new(&data);
        let cm = read_cost_models(&mut r).unwrap();
        assert_eq!(cm.plutus_v1.as_ref().unwrap(), &vec![100, 200]);
        assert_eq!(cm.plutus_v2.as_ref().unwrap(), &vec![50]);
        assert_eq!(cm.plutus_v3.as_ref().unwrap(), &vec![75]);
    }

    /// #770: unknown-language entries (keys ≥ 4) are PRESERVED, not dropped —
    /// Haskell keeps them in `_costModelsUnknown` and re-emits via
    /// `flattenCostModels`. Known typed fields decode alongside.
    #[test]
    fn cost_models_unknown_keys_preserved() {
        // {0: [1, 2], 99: [42, 43]}
        let mut data = vec![0xa2];
        data.extend(cbor_uint(0));
        data.push(0x82); // array(2)
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(99));
        data.push(0x82); // array(2)
        data.extend(cbor_uint(42));
        data.extend(cbor_uint(43));
        let mut r = Reader::new(&data);
        let cm = read_cost_models(&mut r).unwrap();
        assert_eq!(cm.plutus_v1.as_ref().unwrap(), &vec![1, 2]);
        assert!(cm.plutus_v2.is_none());
        assert!(cm.plutus_v3.is_none());
        assert!(cm.plutus_v4.is_none());
        assert_eq!(cm.unknown_cost_models.get(&99), Some(&vec![42, 43]));
        assert_eq!(cm.unknown_cost_models.len(), 1);
    }

    /// #770: a language key > 255 is not a valid `Word8` (Haskell decodes the
    /// key as `Word8`) → malformed, hard reject rather than silent truncation.
    #[test]
    fn cost_models_key_over_255_rejected() {
        // {256: []}
        let mut data = vec![0xa1];
        data.extend(cbor_uint(256));
        data.push(0x80); // array(0)
        let mut r = Reader::new(&data);
        let err = read_cost_models(&mut r).unwrap_err();
        assert!(
            err.to_string().contains("exceeds Word8"),
            "expected Word8-overflow error, got: {err}"
        );
    }

    #[test]
    fn ex_unit_prices_decodes() {
        let mut data = vec![0x82];
        // mem = tag(30) [1, 100]
        let mut rat = vec![0xd8, 0x1e, 0x82];
        rat.extend(cbor_uint(1));
        rat.extend(cbor_uint(100));
        data.extend(&rat);
        // step = tag(30) [2, 1000]
        let mut rat2 = vec![0xd8, 0x1e, 0x82];
        rat2.extend(cbor_uint(2));
        rat2.extend(cbor_uint(1000));
        data.extend(&rat2);
        let mut r = Reader::new(&data);
        let p = read_ex_unit_prices(&mut r).unwrap();
        assert_eq!(p.mem_price.numerator, 1);
        assert_eq!(p.mem_price.denominator, 100);
        // #860.4: rationals are reduced to lowest terms at decode (Haskell `%`), so
        // the non-reduced wire pair 2/1000 becomes 1/500.
        assert_eq!(p.step_price.numerator, 1);
        assert_eq!(p.step_price.denominator, 500);
    }

    #[test]
    fn ex_unit_prices_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_ex_unit_prices(&mut r).is_err());
    }

    #[test]
    fn ex_units_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x00, 0x00];
        let mut r = Reader::new(&data);
        assert!(read_ex_units(&mut r).is_err());
    }

    // ── gov_action variants ───────────────────────────────────────────────

    fn cbor_anchor(url: &str) -> Vec<u8> {
        let mut v = vec![0x82];
        let bytes = url.as_bytes();
        v.push(0x60 | bytes.len() as u8); // text(len)
        v.extend_from_slice(bytes);
        v.extend(cbor_bytes(&[0; 32]));
        v
    }

    #[test]
    fn gov_action_info_action() {
        // [6]
        let data = [0x81, 0x06];
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        assert!(matches!(g, GovAction::InfoAction));
    }

    #[test]
    fn gov_action_no_confidence_with_null_prev() {
        // [3, null]
        let data = [0x82, 0x03, 0xf6];
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        assert!(matches!(
            g,
            GovAction::NoConfidence {
                prev_action_id: None
            }
        ));
    }

    #[test]
    fn gov_action_hard_fork_initiation() {
        // [1, null, [11, 0]]
        let mut data = vec![0x83];
        data.extend(cbor_uint(1));
        data.push(0xf6);
        data.push(0x82);
        data.extend(cbor_uint(11));
        data.extend(cbor_uint(0));
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        match g {
            GovAction::HardForkInitiation {
                protocol_version, ..
            } => assert_eq!(protocol_version, (11, 0)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn gov_action_hard_fork_invalid_version_arity() {
        // [1, null, [11]]
        let mut data = vec![0x83];
        data.extend(cbor_uint(1));
        data.push(0xf6);
        data.push(0x81);
        data.extend(cbor_uint(11));
        let mut r = Reader::new(&data);
        assert!(read_gov_action(&mut r, Era::Conway).is_err());
    }

    #[test]
    fn gov_action_treasury_withdrawals() {
        // [2, {reward_acct => coin}, null]
        let mut data = vec![0x83];
        data.extend(cbor_uint(2));
        data.push(0xa1);
        data.extend(cbor_bytes(&[0xE0; 29]));
        data.extend(cbor_uint(1_000_000));
        data.push(0xf6); // null policy
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        match g {
            GovAction::TreasuryWithdrawals { withdrawals, .. } => {
                assert_eq!(withdrawals.len(), 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn gov_action_new_constitution() {
        // [5, null, [anchor, null]]
        let mut data = vec![0x83];
        data.extend(cbor_uint(5));
        data.push(0xf6);
        data.push(0x82);
        data.extend(cbor_anchor("foo"));
        data.push(0xf6);
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        assert!(matches!(g, GovAction::NewConstitution { .. }));
    }

    #[test]
    fn gov_action_update_committee() {
        // [4, null, set<cred>, {cred => epoch}, threshold]
        let mut data = vec![0x85];
        data.extend(cbor_uint(4));
        data.push(0xf6); // prev_id = null
                         // members_to_remove = []
        data.push(0x80);
        // members_to_add = {cred(0, hash28) => 10}
        data.push(0xa1);
        data.push(0x82);
        data.extend(cbor_uint(0));
        data.extend(cbor_bytes(&[0xAA; 28]));
        data.extend(cbor_uint(10));
        // threshold = tag(30) [1, 2]
        let mut rat = vec![0xd8, 0x1e, 0x82];
        rat.extend(cbor_uint(1));
        rat.extend(cbor_uint(2));
        data.extend(&rat);
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        assert!(matches!(g, GovAction::UpdateCommittee { .. }));
    }

    #[test]
    fn gov_action_parameter_change_with_empty_update() {
        // [0, null, {}, null]
        let mut data = vec![0x84];
        data.extend(cbor_uint(0));
        data.push(0xf6); // prev_id
        data.push(0xa0); // empty pparam update
        data.push(0xf6); // null policy
        let mut r = Reader::new(&data);
        let g = read_gov_action(&mut r, Era::Conway).unwrap();
        assert!(matches!(g, GovAction::ParameterChange { .. }));
    }

    #[test]
    fn gov_action_unknown_disc_rejected() {
        let data = [0x82, 0x18, 0x63, 0xf6]; // [99, null]
        let mut r = Reader::new(&data);
        assert!(read_gov_action(&mut r, Era::Conway).is_err());
    }

    #[test]
    fn gov_action_indefinite_array_rejected() {
        let data = [0x9f, 0x06, 0xff];
        let mut r = Reader::new(&data);
        assert!(read_gov_action(&mut r, Era::Conway).is_err());
    }

    // ── Protocol param update ─────────────────────────────────────────────

    #[test]
    fn pparam_update_min_fee_a_and_b() {
        // {0: 100, 1: 200}
        let mut data = vec![0xa2];
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(100));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(200));
        let mut r = Reader::new(&data);
        let ppu = read_protocol_param_update(&mut r, Era::Conway).unwrap();
        assert_eq!(ppu.min_fee_a, Some(100));
        assert_eq!(ppu.min_fee_b, Some(200));
    }

    /// Issue #1013. `{99: 0, 2: 4096}` — key 99 is not in ANY era's
    /// `updateFieldMap`. Haskell hard-rejects the whole decode via
    /// `invalidField`/`decodeSparseKeyed`'s `Unknown field key` (see the
    /// `read_protocol_param_update` doc comment for the full oracle
    /// citation), so dugite must reject too rather than silently skip key 99
    /// and install key 2. This test previously asserted the OLD (buggy)
    /// behavior under the name `pparam_update_unknown_key_skipped` — renamed
    /// and inverted, not duplicated, so the buggy assertion cannot coexist
    /// with the fix.
    #[test]
    fn pparam_update_unknown_key_rejected() {
        // {99: 0, 2: 4096}
        let mut data = vec![0xa2];
        data.extend(cbor_uint(99));
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(4096));
        let mut r = Reader::new(&data);
        let result = read_protocol_param_update(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "unknown PPU key 99 must be rejected under Era::Conway, got {result:?}"
        );

        // Same bytes, indefinite-length map form — `for_each_field_entry`
        // must reject on both the definite and indefinite paths (#1012's
        // lesson: a fix that only covers one framing leaves the other silent).
        let mut data_indef = vec![0xbf]; // indefinite map open
        data_indef.extend(cbor_uint(99));
        data_indef.extend(cbor_uint(0));
        data_indef.extend(cbor_uint(2));
        data_indef.extend(cbor_uint(4096));
        data_indef.push(0xff); // break
        let mut r_indef = Reader::new(&data_indef);
        let result_indef = read_protocol_param_update(&mut r_indef, Era::Conway);
        assert!(
            matches!(result_indef, Err(SerializationError::CborDecode(_))),
            "unknown PPU key 99 must be rejected on the indefinite-map path too, got {result_indef:?}"
        );
    }

    /// Issue #1013: a totally out-of-range key (never valid in any era) must
    /// reject regardless of era.
    #[test]
    fn pparam_update_far_out_of_range_key_rejected_both_eras() {
        for era in [Era::Conway, Era::Dijkstra] {
            let mut data = vec![0xa1];
            data.extend(cbor_uint(1000));
            data.extend(cbor_uint(0));
            let mut r = Reader::new(&data);
            let result = read_protocol_param_update(&mut r, era);
            assert!(
                matches!(result, Err(SerializationError::CborDecode(_))),
                "key 1000 must be rejected under {era:?}, got {result:?}"
            );
        }
    }

    /// Issue #1013: keys 12/13/14/15 are gaps in BOTH Conway and Dijkstra
    /// (`ppD`/`hkdExtraEntropyL`/`hkdMinUTxOValueCompactL` = `notSupportedInThisEraL`,
    /// `ppGovProtocolVersion { ppUpdate = Nothing }` — oracle-verified, see the
    /// function doc comment). Must reject under both eras, not just one.
    #[test]
    fn pparam_update_gap_keys_12_13_14_15_rejected_both_eras() {
        for era in [Era::Conway, Era::Dijkstra] {
            for key in [12u64, 13, 14, 15] {
                let mut data = vec![0xa1];
                data.extend(cbor_uint(key));
                data.extend(cbor_uint(0));
                let mut r = Reader::new(&data);
                let result = read_protocol_param_update(&mut r, era);
                assert!(
                    matches!(result, Err(SerializationError::CborDecode(_))),
                    "gap key {key} must be rejected under {era:?}, got {result:?}"
                );
            }
        }
    }

    /// Issue #1013: keys 34-37 are Dijkstra-only. Conway's `eraPParams` has
    /// no entries for them (verified against
    /// `eras/conway/impl/.../Conway/PParams.hs:862-894`), so a `ParameterChange`
    /// carrying one of these keys inside a CONWAY (PV9-11) transaction must be
    /// rejected — accepting it would be accept-where-Haskell-rejects, live on
    /// mainnet today, not merely an unreleased-era edge case.
    #[test]
    fn pparam_update_keys_34_37_rejected_under_conway_accepted_under_dijkstra() {
        for key in [34u64, 35, 36, 37] {
            let mut data = vec![0xa1];
            data.extend(cbor_uint(key));
            // A value shape valid for all four keys' decoders: uint(1) works
            // for 34/35/36 (Word32); 37 needs a rational, handled below.
            if key == 37 {
                data.extend(cbor_rational(1, 1));
            } else {
                data.extend(cbor_uint(1));
            }

            let mut r_conway = Reader::new(&data);
            let conway_result = read_protocol_param_update(&mut r_conway, Era::Conway);
            assert!(
                matches!(conway_result, Err(SerializationError::CborDecode(_))),
                "key {key} must be rejected under Era::Conway (Dijkstra-only), got {conway_result:?}"
            );

            let mut r_dijkstra = Reader::new(&data);
            let dijkstra_result = read_protocol_param_update(&mut r_dijkstra, Era::Dijkstra);
            assert!(
                dijkstra_result.is_ok(),
                "key {key} must be ACCEPTED under Era::Dijkstra, got {dijkstra_result:?}"
            );
        }
    }

    /// Issue #1013: Dijkstra upstream defines keys 34-39 (six new keys, not
    /// four — oracle-verified against
    /// `eras/dijkstra/impl/.../Dijkstra/PParams.hs:490-572`), but
    /// `ProtocolParamUpdate` has no fields for 38 (`maxPledgeLeverage`) or 39
    /// (`minPoolMargin`) yet. Document the resulting fail-closed gap: dugite
    /// rejects what Haskell would accept for these two keys, even under
    /// `Era::Dijkstra`. Safe direction (never accept-where-Haskell-rejects),
    /// and Dijkstra is unreleased, but this is NOT full Dijkstra PPU coverage
    /// — a follow-up needs `dugite-primitives::ProtocolParamUpdate` fields for
    /// 38/39 before this test's expectation should flip to `is_ok()`.
    #[test]
    fn pparam_update_keys_38_39_rejected_under_dijkstra_known_gap() {
        for key in [38u64, 39] {
            let mut data = vec![0xa1];
            data.extend(cbor_uint(key));
            data.extend(cbor_uint(1));
            let mut r = Reader::new(&data);
            let result = read_protocol_param_update(&mut r, Era::Dijkstra);
            assert!(
                matches!(result, Err(SerializationError::CborDecode(_))),
                "key {key} is rejected under Era::Dijkstra today (no ProtocolParamUpdate \
                 field yet) — if this now passes, dugite gained 38/39 support and this \
                 test's expectation should flip, got {result:?}"
            );
        }
    }

    /// Issue #1013 boundary: the highest key valid under Conway (33) decodes;
    /// the next key (34) is rejected under Conway but accepted under Dijkstra.
    #[test]
    fn pparam_update_boundary_key_33_34() {
        let mut data33 = vec![0xa1];
        data33.extend(cbor_uint(33));
        data33.extend(cbor_rational(1, 1));
        let mut r33 = Reader::new(&data33);
        assert!(
            read_protocol_param_update(&mut r33, Era::Conway).is_ok(),
            "key 33 (min_fee_ref_script_cost_per_byte) is the highest Conway key and must decode"
        );

        let mut data34 = vec![0xa1];
        data34.extend(cbor_uint(34));
        data34.extend(cbor_uint(1));
        let mut r34_conway = Reader::new(&data34);
        assert!(
            read_protocol_param_update(&mut r34_conway, Era::Conway).is_err(),
            "key 34 must be the first REJECTED key under Era::Conway"
        );
        let mut r34_dijkstra = Reader::new(&data34);
        assert!(
            read_protocol_param_update(&mut r34_dijkstra, Era::Dijkstra).is_ok(),
            "key 34 must decode under Era::Dijkstra"
        );
    }

    /// Issue #1013 boundary: the highest key dugite currently supports under
    /// Dijkstra (37 — see the 38/39 gap test above) decodes; the next key
    /// (38) is rejected even under Dijkstra (known gap, not full coverage).
    #[test]
    fn pparam_update_boundary_key_37_38() {
        let mut data37 = vec![0xa1];
        data37.extend(cbor_uint(37));
        data37.extend(cbor_rational(1, 1));
        let mut r37 = Reader::new(&data37);
        assert!(
            read_protocol_param_update(&mut r37, Era::Dijkstra).is_ok(),
            "key 37 (ref_script_cost_multiplier) is the highest dugite-supported \
             Dijkstra key and must decode"
        );

        let mut data38 = vec![0xa1];
        data38.extend(cbor_uint(38));
        data38.extend(cbor_uint(1));
        let mut r38 = Reader::new(&data38);
        assert!(
            read_protocol_param_update(&mut r38, Era::Dijkstra).is_err(),
            "key 38 is rejected even under Era::Dijkstra today (known gap — see \
             pparam_update_keys_38_39_rejected_under_dijkstra_known_gap)"
        );
    }

    /// Issue #1013: duplicate PPU key must be rejected (`for_each_field_entry`
    /// — Haskell's `decodeSparseKeyed`/`SparseKeyed` both track `Set Word` and
    /// hard-fail on a repeat, un-gated by protocol version — oracle-verified,
    /// `Decoder.hs:1281` / `Coders.hs`'s `applyField`). This is the "bonus
    /// finding" from the same oracle lookup that established the unknown-key
    /// behavior: a fix that only rejects unknown keys and not duplicate keys
    /// is incomplete relative to Haskell.
    #[test]
    fn pparam_update_duplicate_key_rejected() {
        // {0: 44, 0: 99} — key 0 twice.
        let mut data = vec![0xa2];
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(44));
        data.extend(cbor_uint(0));
        data.extend(cbor_uint(99));
        let mut r = Reader::new(&data);
        let result = read_protocol_param_update(&mut r, Era::Conway);
        assert!(
            matches!(result, Err(SerializationError::CborDecode(_))),
            "duplicate PPU key 0 must be rejected, got {result:?}"
        );
    }

    // ── Proposal procedure ────────────────────────────────────────────────

    #[test]
    fn proposal_procedure_decodes() {
        // [deposit, reward_acct, gov_action=[6], anchor]
        let mut data = vec![0x84];
        data.extend(cbor_uint(500_000));
        data.extend(cbor_bytes(&[0xE0; 29]));
        data.push(0x81);
        data.extend(cbor_uint(6)); // InfoAction
        data.extend(cbor_anchor("a"));
        let mut r = Reader::new(&data);
        let pp = read_proposal_procedure(&mut r, Era::Conway).unwrap();
        assert_eq!(pp.deposit.0, 500_000);
        assert!(matches!(pp.gov_action, GovAction::InfoAction));
    }

    #[test]
    fn proposal_procedure_wrong_arity_rejected() {
        let data = [0x83, 0x00, 0x40, 0x81];
        let mut r = Reader::new(&data);
        assert!(read_proposal_procedure(&mut r, Era::Conway).is_err());
    }

    // ── Standalone tx (Conway + Dijkstra) ─────────────────────────────────

    fn build_conway_standalone_tx() -> Vec<u8> {
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(123_456));
        tx.push(0xa0);
        tx.push(0xf5);
        tx.push(0xf6);
        tx
    }

    #[test]
    fn conway_standalone_tx_decodes() {
        let cbor = build_conway_standalone_tx();
        let tx = decode_conway_tx_standalone(&cbor, Era::Conway).unwrap();
        assert_eq!(tx.era, Era::Conway);
        assert_eq!(tx.body.fee.0, 123_456);
    }

    #[test]
    fn dijkstra_standalone_tx_decodes() {
        let cbor = build_conway_standalone_tx();
        let tx = decode_conway_tx_standalone(&cbor, Era::Dijkstra).unwrap();
        assert_eq!(tx.era, Era::Dijkstra);
    }

    #[test]
    fn conway_standalone_tx_invalid_flag() {
        let mut tx = vec![0x84];
        tx.push(0xa3);
        tx.extend(cbor_uint(0));
        tx.push(0x80);
        tx.extend(cbor_uint(1));
        tx.push(0x80);
        tx.extend(cbor_uint(2));
        tx.extend(cbor_uint(0));
        tx.push(0xa0);
        tx.push(0xf4);
        tx.push(0xf6);
        let result = decode_conway_tx_standalone(&tx, Era::Conway).unwrap();
        assert!(!result.is_valid);
    }

    #[test]
    fn conway_standalone_tx_rejects_wrong_arity() {
        let cbor = [0x83, 0xa0, 0xa0, 0xf6];
        assert!(decode_conway_tx_standalone(&cbor, Era::Conway).is_err());
    }

    #[test]
    fn conway_standalone_tx_rejects_indefinite() {
        assert!(decode_conway_tx_standalone(&[0x9f, 0xff], Era::Conway).is_err());
    }

    // ── pool_voting_thresholds + drep_voting_thresholds ────────────────────

    #[test]
    fn pool_voting_thresholds_5_rationals() {
        // {25: [5 rationals]}
        let mut data = vec![0xa1];
        data.extend(cbor_uint(25));
        data.push(0x85);
        for _ in 0..5 {
            let mut rat = vec![0xd8, 0x1e, 0x82];
            rat.extend(cbor_uint(1));
            rat.extend(cbor_uint(2));
            data.extend(&rat);
        }
        let mut r = Reader::new(&data);
        let ppu = read_protocol_param_update(&mut r, Era::Conway).unwrap();
        assert!(ppu.pvt_motion_no_confidence.is_some());
        assert!(ppu.pvt_pp_security_group.is_some());
    }

    #[test]
    fn drep_voting_thresholds_10_rationals() {
        let mut data = vec![0xa1];
        data.extend(cbor_uint(26));
        data.push(0x8a); // array(10)
        for _ in 0..10 {
            let mut rat = vec![0xd8, 0x1e, 0x82];
            rat.extend(cbor_uint(1));
            rat.extend(cbor_uint(3));
            data.extend(&rat);
        }
        let mut r = Reader::new(&data);
        let ppu = read_protocol_param_update(&mut r, Era::Conway).unwrap();
        assert!(ppu.dvt_no_confidence.is_some());
    }

    // ── voting_procedures (whole map) ─────────────────────────────────────

    #[test]
    fn voting_procedures_decode() {
        // {voter => {gov_action_id => voting_procedure}}
        // Build one voter (DRep key 28), one gov_action_id, one procedure (Yes, null).
        let mut data = vec![0xa1];
        // voter [2, hash28]
        data.push(0x82);
        data.extend(cbor_uint(2));
        data.extend(cbor_bytes(&[0x44; 28]));
        // inner map(1)
        data.push(0xa1);
        // gov_action_id [tx, idx]
        data.push(0x82);
        data.extend(cbor_bytes(&[0; 32]));
        data.extend(cbor_uint(0));
        // voting_procedure [Yes, null]
        data.push(0x82);
        data.extend(cbor_uint(1));
        data.push(0xf6);
        let mut r = Reader::new(&data);
        let vp = read_voting_procedures(&mut r).unwrap();
        assert_eq!(vp.len(), 1);
        let inner = vp.values().next().unwrap();
        assert_eq!(inner.len(), 1);
    }

    // ── script_ref native-script shape regression ──────────────────────────────
    //
    // The Conway CDDL has a type-dependent shape inside the embedded script_ref:
    //   script = [ 0, native_script ]      ← native_script is an array, NOT bytes
    //          / [ 1, plutus_v1_script ]   ← plutus_v* are bytes
    //          / [ 2, plutus_v2_script ]
    //          / [ 3, plutus_v3_script ]
    //
    // The pre-fix code unconditionally called read_bytes() after reading
    // script_type, which decode-failed at "position 2" for native scripts
    // (the array header byte for the inline native_script body).
    //
    // Repro: preprod block 4,734,057 at slot 123,678,510 contained an output
    // with a script_ref carrying a native multi-sig script. The decoder
    // refused the block, the BlockFetch path could not store it, and
    // dugite's chain stuck at the previous block while the network advanced.

    /// Wrap inner CBOR into the `#6.24(bytes .cbor X)` tag-24-bytes envelope.
    fn embed_cbor_tag24(inner: &[u8]) -> Vec<u8> {
        let mut out = vec![0xd8, 0x18]; // tag 24
        out.extend(cbor_bytes(inner));
        out
    }

    /// Inline native script: `[3, 1, [[0, addr_keyhash(28 bytes)]]]` = 1-of-1
    /// signature requirement. Mirrors the exact byte pattern from the failing
    /// preprod block.
    fn synthetic_native_script_cbor() -> Vec<u8> {
        // [3, 1, [[0, addr_keyhash(28)]]] = script_n_of_k(n=1, [script_pubkey])
        let mut v = vec![
            0x83, // array(3) — script_n_of_k
            0x03, //   uint 3 — discriminator
            0x01, //   uint 1 — n
            0x81, //   array(1) — children
            0x82, //     array(2) — script_pubkey [0, keyhash]
            0x00, //       uint 0 — script_pubkey discriminator
        ];
        v.extend(cbor_bytes(&[0xaau8; 28])); //  bytes(28) — addr_keyhash
        v
    }

    #[test]
    fn script_ref_native_script_decodes_as_array_not_bytes() {
        use dugite_primitives::transaction::ScriptRef;

        let ns = synthetic_native_script_cbor();
        // inner = [0, native_script]
        let mut inner = vec![0x82, 0x00];
        inner.extend(&ns);
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let parsed = read_script_ref(&mut r)
            .expect("native script_ref must decode (regression: preprod 4734057)");
        match parsed {
            ScriptRef::NativeScript(_) => {}
            other => panic!("expected NativeScript, got {other:?}"),
        }
    }

    #[test]
    fn script_ref_plutus_v1_decodes_as_bytes() {
        use dugite_primitives::transaction::ScriptRef;
        let script = vec![0xde, 0xad, 0xbe, 0xef];
        let mut inner = vec![0x82, 0x01];
        inner.extend(cbor_bytes(&script));
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let parsed = read_script_ref(&mut r).expect("plutus v1 script_ref must decode");
        match parsed {
            ScriptRef::PlutusV1(bytes) => assert_eq!(bytes, script),
            other => panic!("expected PlutusV1, got {other:?}"),
        }
    }

    #[test]
    fn script_ref_plutus_v2_decodes_as_bytes() {
        use dugite_primitives::transaction::ScriptRef;
        let script = vec![0x12, 0x34, 0x56];
        let mut inner = vec![0x82, 0x02];
        inner.extend(cbor_bytes(&script));
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let parsed = read_script_ref(&mut r).expect("plutus v2 script_ref must decode");
        match parsed {
            ScriptRef::PlutusV2(bytes) => assert_eq!(bytes, script),
            other => panic!("expected PlutusV2, got {other:?}"),
        }
    }

    #[test]
    fn script_ref_plutus_v3_decodes_as_bytes() {
        use dugite_primitives::transaction::ScriptRef;
        let script = vec![0xab, 0xcd];
        let mut inner = vec![0x82, 0x03];
        inner.extend(cbor_bytes(&script));
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let parsed = read_script_ref(&mut r).expect("plutus v3 script_ref must decode");
        match parsed {
            ScriptRef::PlutusV3(bytes) => assert_eq!(bytes, script),
            other => panic!("expected PlutusV3, got {other:?}"),
        }
    }

    #[test]
    fn script_ref_unknown_script_type_errors_cleanly() {
        // Type 7 is not defined for Conway. Must surface as a CborDecode
        // error, not a panic or silent skip.
        let mut inner = vec![0x82, 0x07];
        inner.extend(cbor_bytes(&[0x00]));
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let err = read_script_ref(&mut r).expect_err("unknown type must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown script type"),
            "error must surface the rejection reason, got: {msg}"
        );
    }

    #[test]
    fn redeemers_map_decodes_both_definite_and_indefinite() {
        // One redeemer: key [tag=0(Spend), index=0], value [data=int 0, exunits [0,0]].
        // value = array(2)[ 0x00 (int 0), array(2)[0,0] ] = 82 00 82 00 00
        // key   = array(2)[ 0x00, 0x00 ]                  = 82 00 00
        let body = [0x82, 0x00, 0x00, 0x82, 0x00, 0x82, 0x00, 0x00];

        // Definite map (0xA1 = map(1)).
        let mut def = vec![0xA1];
        def.extend_from_slice(&body);
        let mut rd = Reader::new(&def);
        let rs = read_redeemers(&mut rd, Era::Conway).expect("definite map redeemers must decode");
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].tag, RedeemerTag::Spend);
        assert_eq!(rs[0].index, 0);

        // Indefinite map (0xBF … 0xFF) — the on-chain form that wedged the
        // preprod sync at block 4149070 / epoch 253 before this fix.
        let mut indef = vec![0xBF];
        indef.extend_from_slice(&body);
        indef.push(0xFF);
        let mut ri = Reader::new(&indef);
        let rs2 =
            read_redeemers(&mut ri, Era::Conway).expect("indefinite map redeemers must decode");
        assert_eq!(rs2.len(), 1);
        assert_eq!(rs2[0].tag, RedeemerTag::Spend);
        assert_eq!(rs2[0].index, 0);
        assert_eq!(rs2[0], rs[0]);
    }

    // ── F2: indefinite-length OUTER array in native (timelock) script ─────────
    //
    // cardano-ledger's Timelock decoder (`Summands "TimelockRaw" decRaw` =>
    // `decodeRecordSum` => `decodeListLike` => `decodeListLikeT`) reads the outer
    // sum array with `decodeListLenOrIndef` and TOLERATES the indefinite case
    // (consuming the trailing break via `decodeBreakOr`). dugite previously
    // HARD-REJECTED an indefinite outer array, over-rejecting potentially-real
    // on-chain native scripts and aborting an entire snapshot import. These tests
    // pin the relaxation and confirm the indefinite encoding decodes IDENTICALLY
    // to its definite-encoded equivalent.

    /// `[5, 1234]` = InvalidHereafter(1234), DEFINITE outer array.
    fn native_invalid_hereafter_definite() -> Vec<u8> {
        let mut v = vec![0x82, 0x05]; // array(2), uint 5
        v.extend(cbor_uint(1234));
        v
    }

    /// `[_ 5, 1234, break]` = InvalidHereafter(1234), INDEFINITE outer array.
    fn native_invalid_hereafter_indefinite() -> Vec<u8> {
        let mut v = vec![0x9f, 0x05]; // array(*) indefinite, uint 5
        v.extend(cbor_uint(1234));
        v.push(0xff); // break
        v
    }

    #[test]
    fn native_script_indefinite_outer_array_decodes_like_definite() {
        let def = native_invalid_hereafter_definite();
        let indef = native_invalid_hereafter_indefinite();

        let parsed_def =
            decode_native_script_cbor(&def).expect("definite-length native script must decode");
        let parsed_indef = decode_native_script_cbor(&indef)
            .expect("indefinite-length native script must decode (F2: ledger leniency)");

        assert_eq!(parsed_def, NativeScript::InvalidHereafter(SlotNo(1234)));
        // Byte-different encoding, identical decoded value.
        assert_eq!(parsed_def, parsed_indef);
    }

    #[test]
    fn native_script_indefinite_outer_array_missing_break_errors() {
        // Indefinite header but the body is NOT closed with a break byte: upstream
        // `decodeListLikeT` raises "Excess terms in array". We must error, not
        // silently accept a half-open array.
        let mut bad = vec![0x9f, 0x05]; // array(*) indefinite, uint 5
        bad.extend(cbor_uint(1234));
        // (no 0xff break, and a stray trailing byte that is not a break)
        bad.push(0x00);
        assert!(
            decode_native_script_cbor(&bad).is_err(),
            "indefinite outer array without a closing break must be rejected"
        );
    }

    #[test]
    fn native_script_nested_indefinite_outer_array_decodes() {
        // ScriptAll with a child whose OUTER array is indefinite-encoded:
        //   [1, [ [_ 5, 1234, break] ]]
        // Exercises the relaxation through the recursive `read_array` path.
        let mut v = vec![0x82, 0x01]; // array(2), uint 1 (ScriptAll)
        v.push(0x81); // array(1) children
        v.extend(native_invalid_hereafter_indefinite());
        let parsed =
            decode_native_script_cbor(&v).expect("nested indefinite native script must decode");
        assert_eq!(
            parsed,
            NativeScript::ScriptAll(vec![NativeScript::InvalidHereafter(SlotNo(1234))])
        );
    }

    #[test]
    fn script_ref_native_script_indefinite_outer_array_imports() {
        // A `#6.24`-wrapped reference script (script_type 0 = native) whose native
        // timelock outer array is INDEFINITE-encoded must IMPORT rather than abort
        // the decode — the snapshot-import failure mode this fix closes.
        use dugite_primitives::transaction::ScriptRef;

        let ns = native_invalid_hereafter_indefinite();
        let mut inner = vec![0x82, 0x00]; // [0, native_script]
        inner.extend(&ns);
        let outer = embed_cbor_tag24(&inner);

        let mut r = Reader::new(&outer);
        let parsed = read_script_ref(&mut r)
            .expect("indefinite native script_ref must decode (F2 import path)");
        match parsed {
            ScriptRef::NativeScript(NativeScript::InvalidHereafter(SlotNo(s))) => {
                assert_eq!(s, 1234)
            }
            other => panic!("expected NativeScript(InvalidHereafter), got {other:?}"),
        }
    }

    // ── PlutusData 64-byte ByteString-leaf bound (#28, Note [The 64-byte limit]) ──
    //
    // Mirrors Haskell `plutus` PlutusCore.Data.decodeData
    // `decodeBoundedBytes` / `decodeBoundedBytesIndefLen`:
    //   * definite-length leaf  > 64 bytes  => Err
    //   * indefinite form: EACH single chunk must be <= 64 bytes (any chunk
    //     > 64 => Err); the concatenated TOTAL may exceed 64 (unbounded).
    //   * a 0-length chunk is allowed.
    //   * the tag-2 / tag-3 bignum mantissa is also a leaf — bounded the same way.
    //
    // The same rule must NOT touch generic (non-PlutusData) bytestrings.

    /// Encode a definite-length CBOR byte string header + payload.
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

    /// Encode an indefinite-length CBOR byte string (`0x5f <chunks> 0xff`),
    /// each chunk being a definite-length byte string.
    fn indef_bytes(chunks: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![0x5fu8];
        for c in chunks {
            v.extend_from_slice(&def_bytes(c));
        }
        v.push(0xff);
        v
    }

    /// Wrap a (definite or indefinite) byte-string encoding in a `tag(2)`
    /// positive-bignum header.
    fn tag2(mantissa: Vec<u8>) -> Vec<u8> {
        let mut v = vec![0xc2u8]; // tag(2)
        v.extend_from_slice(&mantissa);
        v
    }

    /// Wrap a (definite or indefinite) byte-string encoding in a `tag(3)`
    /// negative-bignum header.
    fn tag3(mantissa: Vec<u8>) -> Vec<u8> {
        let mut v = vec![0xc3u8]; // tag(3)
        v.extend_from_slice(&mantissa);
        v
    }

    fn decode_pd(cbor: &[u8]) -> Result<PlutusData, SerializationError> {
        let mut r = Reader::new(cbor);
        read_plutus_data(&mut r)
    }

    #[test]
    fn plutus_bytes_definite_64_ok() {
        let payload = vec![0xABu8; 64];
        let pd = decode_pd(&def_bytes(&payload)).expect("64-byte definite leaf must decode");
        assert_eq!(pd, PlutusData::Bytes(payload));
    }

    #[test]
    fn plutus_bytes_definite_65_err() {
        let payload = vec![0xABu8; 65];
        let err = decode_pd(&def_bytes(&payload)).expect_err("65-byte definite leaf must reject");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn plutus_bytes_indef_single_chunk_64_ok() {
        let chunk = vec![0x11u8; 64];
        let pd = decode_pd(&indef_bytes(&[&chunk])).expect("64-byte indef chunk must decode");
        assert_eq!(pd, PlutusData::Bytes(chunk));
    }

    #[test]
    fn plutus_bytes_indef_single_chunk_65_err() {
        let chunk = vec![0x11u8; 65];
        let err = decode_pd(&indef_bytes(&[&chunk])).expect_err("65-byte indef chunk must reject");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn plutus_bytes_indef_two_64_chunks_total_128_ok() {
        // Two <=64 chunks: concatenated total (128) MAY exceed 64 — unbounded.
        let a = vec![0x22u8; 64];
        let b = vec![0x33u8; 64];
        let pd =
            decode_pd(&indef_bytes(&[&a, &b])).expect("two 64-byte chunks (total 128) must decode");
        let mut expected = a.clone();
        expected.extend_from_slice(&b);
        assert_eq!(pd, PlutusData::Bytes(expected));
    }

    #[test]
    fn plutus_bytes_indef_zero_length_chunk_ok() {
        // A 0-length chunk is permitted (alongside a non-empty one).
        let a: &[u8] = &[];
        let b = vec![0x44u8; 10];
        let pd = decode_pd(&indef_bytes(&[a, &b])).expect("0-length chunk must decode");
        assert_eq!(pd, PlutusData::Bytes(b));
    }

    #[test]
    fn plutus_bytes_indef_second_chunk_65_err() {
        // First chunk fine; the SECOND chunk (65) must trip the per-chunk bound.
        let a = vec![0x55u8; 64];
        let b = vec![0x66u8; 65];
        let err = decode_pd(&indef_bytes(&[&a, &b]))
            .expect_err("a >64 chunk anywhere in the stream must reject");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn plutus_bignum_mantissa_definite_64_ok() {
        let mantissa = vec![0x01u8; 64];
        // tag(2): positive bignum with a 64-byte mantissa.
        let pd = decode_pd(&tag2(def_bytes(&mantissa)))
            .expect("64-byte bignum mantissa (tag2 definite) must decode");
        assert!(matches!(pd, PlutusData::Integer(_)));
        // tag(3): negative bignum, same mantissa bound.
        let pd3 = decode_pd(&tag3(def_bytes(&mantissa)))
            .expect("64-byte bignum mantissa (tag3 definite) must decode");
        assert!(matches!(pd3, PlutusData::Integer(_)));
    }

    #[test]
    fn plutus_bignum_mantissa_definite_65_err() {
        let mantissa = vec![0x01u8; 65];
        let err = decode_pd(&tag2(def_bytes(&mantissa)))
            .expect_err("65-byte bignum mantissa (tag2 definite) must reject");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err:?}"
        );
        let err3 = decode_pd(&tag3(def_bytes(&mantissa)))
            .expect_err("65-byte bignum mantissa (tag3 definite) must reject");
        assert!(
            matches!(&err3, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err3:?}"
        );
    }

    #[test]
    fn plutus_bignum_mantissa_indef_chunk_65_err() {
        let chunk = vec![0x01u8; 65];
        let err = decode_pd(&tag2(indef_bytes(&[&chunk])))
            .expect_err("65-byte bignum mantissa (tag2 indef chunk) must reject");
        assert!(
            matches!(&err, SerializationError::CborDecode(m) if m.contains("64 bytes")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn plutus_bignum_mantissa_indef_two_64_chunks_ok() {
        // tag(2) mantissa as two 64-byte chunks: per-chunk bound respected,
        // total (128) unbounded.
        let a = vec![0x01u8; 64];
        let b = vec![0x02u8; 64];
        let pd = decode_pd(&tag2(indef_bytes(&[&a, &b])))
            .expect("two 64-byte mantissa chunks must decode");
        assert!(matches!(pd, PlutusData::Integer(_)));
    }

    /// OVER-STRICTNESS GUARD: a >64-byte NON-PlutusData bytestring (here a
    /// Plutus script blob in a Conway witness set) is STILL accepted. The
    /// 64-byte rule applies ONLY to PlutusData leaves, never to generic
    /// `read_bytes_owned` / `read_indef_bytes` callers (vkeys, scripts,
    /// addresses, asset names, metadata, ...).
    #[test]
    fn over_strictness_guard_non_plutus_script_blob_over_64_ok() {
        // A 200-byte definite bytestring read via the generic owned reader.
        let blob = vec![0x7Eu8; 200];
        let cbor = def_bytes(&blob);
        let mut r = Reader::new(&cbor);
        let read = r
            .read_bytes_owned()
            .expect(">64-byte non-PlutusData bytestring must still decode");
        assert_eq!(read, blob);

        // And via the generic indefinite reader: two 200-byte chunks (each
        // individually >64) must concatenate fine — the bound does not apply.
        let big = vec![0x7Du8; 200];
        let indef = indef_bytes(&[&big, &big]);
        let mut r2 = Reader::new(&indef);
        let read2 = r2
            .read_indef_bytes()
            .expect(">64-byte non-PlutusData indef chunks must still decode");
        assert_eq!(read2.len(), 400);
    }

    proptest::proptest! {
        /// Length-lattice property: for a single definite-length PlutusData
        /// byte-string leaf, decode succeeds iff len <= 64.
        #[test]
        fn prop_plutus_definite_leaf_bound(len in 0usize..=160) {
            let payload = vec![0x5Au8; len];
            let res = decode_pd(&def_bytes(&payload));
            if len <= 64 {
                proptest::prop_assert!(res.is_ok(), "len {} <= 64 must decode", len);
                proptest::prop_assert_eq!(res.unwrap(), PlutusData::Bytes(payload));
            } else {
                proptest::prop_assert!(res.is_err(), "len {} > 64 must reject", len);
            }
        }

        /// Length-lattice property: a single indefinite chunk is bounded the
        /// same way as a definite leaf (per-chunk <= 64).
        #[test]
        fn prop_plutus_indef_single_chunk_bound(len in 0usize..=160) {
            let chunk = vec![0x6Bu8; len];
            let res = decode_pd(&indef_bytes(&[&chunk]));
            if len <= 64 {
                proptest::prop_assert!(res.is_ok(), "chunk len {} <= 64 must decode", len);
            } else {
                proptest::prop_assert!(res.is_err(), "chunk len {} > 64 must reject", len);
            }
        }

        /// Length-lattice property: TWO chunks each <= 64 always decode (total
        /// unbounded); if EITHER chunk > 64 the whole leaf rejects.
        #[test]
        fn prop_plutus_indef_two_chunk_per_chunk_bound(
            a in 0usize..=160,
            b in 0usize..=160,
        ) {
            let ca = vec![0x01u8; a];
            let cb = vec![0x02u8; b];
            let res = decode_pd(&indef_bytes(&[&ca, &cb]));
            if a <= 64 && b <= 64 {
                proptest::prop_assert!(
                    res.is_ok(),
                    "both chunks <=64 (a={}, b={}, total={}) must decode",
                    a, b, a + b
                );
            } else {
                proptest::prop_assert!(
                    res.is_err(),
                    "a chunk >64 (a={}, b={}) must reject",
                    a, b
                );
            }
        }

        /// Length-lattice property: the tag-2 bignum mantissa leaf obeys the
        /// same definite-length bound.
        #[test]
        fn prop_plutus_bignum_mantissa_bound(len in 1usize..=160) {
            let mantissa = vec![0x01u8; len];
            let res = decode_pd(&tag2(def_bytes(&mantissa)));
            if len <= 64 {
                proptest::prop_assert!(res.is_ok(), "mantissa len {} <= 64 must decode", len);
            } else {
                proptest::prop_assert!(res.is_err(), "mantissa len {} > 64 must reject", len);
            }
        }

        /// OVER-STRICTNESS GUARD (property form): a generic non-PlutusData
        /// bytestring of ANY length (incl. > 64) read via `read_bytes_owned`
        /// always decodes — the 64-byte rule must not leak into generic readers.
        #[test]
        fn prop_generic_bytes_unbounded(len in 0usize..=300) {
            let blob = vec![0x9Cu8; len];
            let cbor = def_bytes(&blob);
            let mut r = Reader::new(&cbor);
            let read = r.read_bytes_owned();
            proptest::prop_assert!(
                read.is_ok(),
                "generic non-PlutusData bytestring len {} must always decode",
                len
            );
            proptest::prop_assert_eq!(read.unwrap(), blob);
        }
    }

    // ── Fix #744: block-decoded Conway tx must have raw_cbor populated ────────

    /// Build a minimal Conway block inner CBOR (same 5-element structure as Babbage).
    fn make_conway_block_inner(n_txs: usize) -> Vec<u8> {
        // VRF result = [output(64), proof(80)]
        let vrf_out = cbor_bytes(&[0u8; 64]);
        let vrf_proof = cbor_bytes(&[0u8; 80]);
        let vrf_result = cbor_arr(&[&vrf_out, &vrf_proof]);

        // operational_cert = [hot_vkey(32), seq_num, kes_period, sigma(64)]
        let op_cert = cbor_arr(&[
            &cbor_bytes(&[0x10u8; 32]),
            &cbor_uint(0),
            &cbor_uint(0),
            &cbor_bytes(&[0x11u8; 64]),
        ]);
        // protocol_version = [9, 0]  (Conway)
        let proto_ver = cbor_arr(&[&cbor_uint(9), &cbor_uint(0)]);

        // header_body = array(10)
        let mut hb = vec![0x8au8]; // array(10)
        hb.extend(cbor_uint(200)); // block_number
        hb.extend(cbor_uint(88888888)); // slot
        hb.extend(cbor_bytes(&[0xcc; 32])); // prev_hash
        hb.extend(cbor_bytes(&[0x01; 32])); // issuer_vkey
        hb.extend(cbor_bytes(&[0x02; 32])); // vrf_vkey
        hb.extend(&vrf_result);
        hb.extend(cbor_uint(0)); // body_size
        hb.extend(cbor_bytes(&[0x00; 32])); // body_hash
        hb.extend(&op_cert);
        hb.extend(&proto_ver);

        let kes_sig = cbor_bytes(&[0x05u8; 448]);
        let mut header = vec![0x82u8]; // array(2)
        header.extend(&hb);
        header.extend(&kes_sig);

        let mut tx_bodies_v = Vec::new();
        let mut tx_witnesses_v = Vec::new();
        if n_txs <= 23 {
            tx_bodies_v.push(0x80 | n_txs as u8);
            tx_witnesses_v.push(0x80 | n_txs as u8);
        }
        for _ in 0..n_txs {
            // {0: [], 1: [], 2: 1000000}
            let mut tb = vec![0xa3u8];
            tb.extend(cbor_uint(0));
            tb.push(0x80);
            tb.extend(cbor_uint(1));
            tb.push(0x80);
            tb.extend(cbor_uint(2));
            tb.extend(cbor_uint(1_000_000));
            tx_bodies_v.extend(&tb);
            tx_witnesses_v.push(0xa0u8); // empty witness set
        }

        let aux_data = vec![0xa0u8]; // {}
        let invalid_txs = vec![0x80u8]; // []

        let mut block = vec![0x85u8]; // array(5)
        block.extend(&header);
        block.extend(&tx_bodies_v);
        block.extend(&tx_witnesses_v);
        block.extend(&aux_data);
        block.extend(&invalid_txs);
        block
    }

    /// Fix #744 — block-decoded Conway tx must have raw_cbor populated so that
    /// compute_min_fee can use the correct wire size for the fee formula.
    #[test]
    fn block_decoded_conway_tx_has_raw_cbor() {
        let cbor = make_conway_block_inner(1);
        let block = decode_conway_block(&cbor).unwrap();
        let tx = &block.transactions[0];

        assert!(
            tx.raw_cbor.is_some(),
            "block-decoded Conway tx must have raw_cbor populated for fee calculation"
        );
        let raw = tx.raw_cbor.as_ref().unwrap();
        assert_eq!(
            raw[0], 0x84,
            "Conway tx raw_cbor must start with 0x84 (array-4)"
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
