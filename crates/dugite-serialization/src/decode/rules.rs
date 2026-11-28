//! Rule-level decoders: one entry point per rule of the Cardano CDDL, taking the bytes of ONE
//! item and returning the typed value.
//!
//! These exist for the node-diversity `cbor-decoding` conformance harness
//! (`tests/conformance`), which feeds the dataset's samples to a decoder per CDDL rule.  Every
//! function is a thin call into the decoder the production paths already use for that item
//! (the block, transaction and witness decoders call the same `read_*` functions), plus the
//! "whole input consumed" check that a standalone decode needs and an embedded one gets from the
//! enclosing structure.  Nothing here changes what any decoder accepts.
//!
//! Visibility: the `read_*` functions these call were private to their era module and are now
//! `pub(crate)`.  This module is the only new public surface.

use std::collections::BTreeMap;

use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash, Hash28, Hash32};
use dugite_primitives::transaction::{
    AccountBalanceInterval, Anchor, AuxiliaryData, Certificate, Constitution, CostModels, DRep,
    ExUnitPrices, ExUnits, GovAction, GovActionId, NativeScript, OutputDatum, PlutusData,
    PoolMetadata, ProposalProcedure, ProtocolParamUpdate, Rational, Redeemer, Relay, ScriptRef,
    SubTransaction, Transaction, TransactionBody, TransactionInput, TransactionMetadatum,
    TransactionOutput, TransactionWitnessSet, Vote, Voter, VotingProcedure,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};

use super::era_conway as conway;
use super::helpers;
use super::reader::Reader;
use crate::error::SerializationError;

type Res<T> = Result<T, SerializationError>;

/// Run `f` on a reader over `bytes` and require that it consumes every byte.
pub fn whole<T>(bytes: &[u8], f: impl FnOnce(&mut Reader<'_>) -> Res<T>) -> Res<T> {
    let mut r = Reader::new(bytes);
    let value = f(&mut r)?;
    if r.position() != bytes.len() {
        return Err(SerializationError::CborDecode(format!(
            "{} trailing bytes after the item",
            bytes.len() - r.position()
        )));
    }
    Ok(value)
}

// ── primitives ──────────────────────────────────────────────────────────────────────────────

pub fn uint(bytes: &[u8]) -> Res<u64> {
    whole(bytes, |r| r.read_uint())
}

pub fn lovelace(bytes: &[u8]) -> Res<Lovelace> {
    whole(bytes, helpers::read_lovelace)
}

pub fn network_id(bytes: &[u8]) -> Res<u8> {
    whole(bytes, helpers::read_network_id)
}

pub fn hash28(bytes: &[u8]) -> Res<Hash28> {
    whole(bytes, helpers::read_hash28)
}

pub fn hash32(bytes: &[u8]) -> Res<Hash32> {
    whole(bytes, helpers::read_hash32)
}

/// A fixed-size byte string of `N` bytes (`vkey`, `signature`, ...).
pub fn fixed_bytes<const N: usize>(bytes: &[u8]) -> Res<Hash<N>> {
    whole(bytes, helpers::read_hash::<N>)
}

pub fn rational(bytes: &[u8]) -> Res<Rational> {
    whole(bytes, |r| r.read_rational())
}

pub fn vote(bytes: &[u8]) -> Res<Vote> {
    whole(bytes, conway::read_vote)
}

pub fn ex_units(bytes: &[u8]) -> Res<ExUnits> {
    whole(bytes, conway::read_ex_units)
}

pub fn ex_unit_prices(bytes: &[u8]) -> Res<ExUnitPrices> {
    whole(bytes, conway::read_ex_unit_prices)
}

pub fn protocol_version(bytes: &[u8]) -> Res<ProtocolVersion> {
    whole(bytes, conway::read_protocol_version)
}

pub fn operational_cert(bytes: &[u8]) -> Res<OperationalCert> {
    whole(bytes, conway::read_operational_cert)
}

// ── blocks, headers, transactions ───────────────────────────────────────────────────────────

/// A block as `[header, tx_bodies, tx_witness_sets, aux_data, invalid_txs]` (no era envelope).
pub fn block(bytes: &[u8], era: Era) -> Res<Block> {
    match era {
        Era::Dijkstra => conway::decode_dijkstra_block(bytes),
        _ => conway::decode_conway_block(bytes),
    }
}

/// A block header `[header_body, kes_signature]`.
pub fn block_header(bytes: &[u8]) -> Res<BlockHeader> {
    conway::decode_conway_block_header(bytes)
}

pub fn transaction(bytes: &[u8], era: Era) -> Res<Transaction> {
    match era {
        Era::Dijkstra => conway::decode_dijkstra_tx_standalone(bytes),
        _ => conway::decode_conway_tx_standalone(bytes, era),
    }
}

pub fn transaction_body(bytes: &[u8], era: Era) -> Res<TransactionBody> {
    whole(bytes, |r| conway::decode_conway_tx_body(r, era))
}

pub fn sub_transaction_body(bytes: &[u8]) -> Res<SubTransaction> {
    whole(bytes, conway::decode_sub_tx_body)
}

pub fn transaction_witness_set(bytes: &[u8], era: Era) -> Res<TransactionWitnessSet> {
    whole(bytes, |r| conway::decode_conway_witness_set(r, era))
}

pub fn transaction_input(bytes: &[u8]) -> Res<TransactionInput> {
    whole(bytes, conway::read_tx_input)
}

/// The Babbage-and-later output decoder (array or map form), which the ledger reads every
/// output with.
pub fn transaction_output(bytes: &[u8]) -> Res<TransactionOutput> {
    whole(bytes, conway::read_babbage_tx_output)
}

/// The array (pre-Babbage) form of an output.
pub fn legacy_transaction_output(bytes: &[u8]) -> Res<TransactionOutput> {
    whole(bytes, conway::read_legacy_tx_output)
}

pub fn value(bytes: &[u8]) -> Res<Value> {
    whole(bytes, conway::read_value)
}

pub fn mint(bytes: &[u8]) -> Res<BTreeMap<Hash28, BTreeMap<AssetName, i64>>> {
    whole(bytes, conway::read_mint_map)
}

pub fn multiasset(bytes: &[u8]) -> Res<BTreeMap<Hash28, BTreeMap<AssetName, u64>>> {
    whole(bytes, conway::read_multiasset_map_u64)
}

pub fn withdrawals(bytes: &[u8]) -> Res<BTreeMap<Vec<u8>, Lovelace>> {
    whole(bytes, conway::read_withdrawals)
}

pub fn datum_option(bytes: &[u8]) -> Res<OutputDatum> {
    whole(bytes, conway::read_datum_option)
}

/// `script_ref = #6.24(bytes .cbor script)`.
pub fn script_ref(bytes: &[u8]) -> Res<ScriptRef> {
    whole(bytes, conway::read_script_ref)
}

/// A `script` (`[script_type, script]`).  The production decoder reads it inside the
/// `#6.24(bytes .cbor script)` wrapper of a script reference, so the item is wrapped the same
/// way and read by that decoder.
pub fn script(bytes: &[u8]) -> Res<ScriptRef> {
    let mut wrapped = vec![0xd8, 0x18];
    wrapped.extend(crate::cbor::encode_bytes(bytes));
    script_ref(&wrapped)
}

/// `address`: a byte string holding the binary address.
pub fn address(bytes: &[u8]) -> Res<dugite_primitives::address::Address> {
    whole(bytes, |r| {
        let raw = r.read_bytes()?;
        dugite_primitives::address::Address::from_bytes(raw)
            .map_err(|e| SerializationError::CborDecode(format!("address: {e}")))
    })
}

// ── certificates and governance ─────────────────────────────────────────────────────────────

pub fn certificate(bytes: &[u8]) -> Res<Certificate> {
    whole(bytes, conway::read_conway_certificate)
}

/// An ordered set of certificates, as the transaction body reads them.
pub fn certificates(bytes: &[u8]) -> Res<Vec<Certificate>> {
    whole(bytes, |r| {
        r.read_set_strict(|r| conway::read_conway_certificate(r))
    })
}

pub fn credential(bytes: &[u8]) -> Res<Credential> {
    whole(bytes, conway::read_stake_credential)
}

pub fn drep(bytes: &[u8]) -> Res<DRep> {
    whole(bytes, conway::read_drep)
}

pub fn anchor(bytes: &[u8]) -> Res<Anchor> {
    whole(bytes, conway::read_anchor)
}

pub fn gov_action_id(bytes: &[u8]) -> Res<GovActionId> {
    whole(bytes, conway::read_gov_action_id)
}

pub fn voter(bytes: &[u8]) -> Res<Voter> {
    whole(bytes, conway::read_voter)
}

pub fn voting_procedure(bytes: &[u8]) -> Res<VotingProcedure> {
    whole(bytes, conway::read_voting_procedure)
}

pub fn voting_procedures(
    bytes: &[u8],
) -> Res<BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>> {
    whole(bytes, conway::read_voting_procedures)
}

pub fn proposal_procedure(bytes: &[u8], era: Era) -> Res<ProposalProcedure> {
    whole(bytes, |r| conway::read_proposal_procedure(r, era))
}

pub fn proposal_procedures(bytes: &[u8], era: Era) -> Res<Vec<ProposalProcedure>> {
    whole(bytes, |r| {
        r.read_set_strict(|r| conway::read_proposal_procedure(r, era))
    })
}

pub fn gov_action(bytes: &[u8], era: Era) -> Res<GovAction> {
    whole(bytes, |r| conway::read_gov_action(r, era))
}

pub fn constitution(bytes: &[u8]) -> Res<Constitution> {
    whole(bytes, conway::read_constitution)
}

pub fn protocol_param_update(bytes: &[u8], era: Era) -> Res<ProtocolParamUpdate> {
    whole(bytes, |r| conway::read_protocol_param_update(r, era))
}

pub fn cost_models(bytes: &[u8]) -> Res<CostModels> {
    whole(bytes, conway::read_cost_models)
}

pub fn account_balance_interval(bytes: &[u8]) -> Res<AccountBalanceInterval> {
    whole(bytes, conway::decode_account_balance_interval)
}

pub fn account_balance_intervals(bytes: &[u8]) -> Res<Vec<(Credential, AccountBalanceInterval)>> {
    whole(bytes, conway::decode_account_balance_intervals)
}

// ── scripts, data, redeemers, auxiliary data ────────────────────────────────────────────────

pub fn native_script(bytes: &[u8]) -> Res<NativeScript> {
    conway::decode_native_script_cbor(bytes)
}

pub fn plutus_data(bytes: &[u8]) -> Res<PlutusData> {
    conway::decode_plutus_data_cbor(bytes)
}

/// One `[tag, index, data, ex_units]` redeemer.
pub fn redeemer(bytes: &[u8]) -> Res<Redeemer> {
    whole(bytes, super::era_alonzo::read_redeemer)
}

/// The `redeemers` witness term, in either wire form the era accepts.
pub fn redeemers(bytes: &[u8], era: Era) -> Res<Vec<Redeemer>> {
    whole(bytes, |r| conway::read_redeemers(r, era))
}

/// `data = #6.24(bytes .cbor plutus_data)`, as the inline-datum arm of the datum option reads it.
pub fn data(bytes: &[u8]) -> Res<PlutusData> {
    whole(bytes, |r| {
        let inner = r.read_embedded_cbor_bytes()?.to_vec();
        whole(&inner, conway::read_plutus_data)
    })
}

pub fn auxiliary_data(bytes: &[u8], era: Era) -> Res<AuxiliaryData> {
    whole(bytes, |r| conway::decode_auxiliary_data(r, era))
}

pub fn metadatum(bytes: &[u8]) -> Res<TransactionMetadatum> {
    whole(bytes, helpers::read_metadatum)
}

pub fn metadata(bytes: &[u8]) -> Res<BTreeMap<u64, TransactionMetadatum>> {
    whole(bytes, helpers::read_metadata_map)
}

// ── pool parameters ─────────────────────────────────────────────────────────────────────────

pub fn relay(bytes: &[u8]) -> Res<Relay> {
    whole(bytes, super::era_shelley::read_relay)
}

/// `pool_metadata = [url, hash]`.  The production reader also accepts null (the pool
/// parameters' optional metadata); the rule does not, so null is refused here.
pub fn pool_metadata(bytes: &[u8]) -> Res<PoolMetadata> {
    whole(bytes, super::era_shelley::read_pool_metadata)?.ok_or_else(|| {
        SerializationError::CborDecode("pool_metadata: null where pool_metadata is required".into())
    })
}
