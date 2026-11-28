//! Rule-level encoders: the counterpart of [`crate::decode::rules`].
//!
//! One entry point per CDDL rule, re-encoding a typed value to the bytes of that ONE item (the
//! node-diversity `cbor-decoding` harness compares them with the dataset's expected files).
//! Each is a call into the encoder the production paths already use for that item; items that the
//! production encoders only emit inline (a datum option, pool metadata) were split out of their
//! parent encoder unchanged.  Production encoders are unchanged.  Not every entry is one:
//! `vote`, `ex_units`, `ex_unit_prices`, `constitution`, `account_balance_interval(s)`, `data`,
//! `bytes` and `network_id` are written here from the CBOR primitives, because production only
//! emits those items inside a parent encoding.

use std::collections::BTreeMap;

use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::transaction::{
    AccountBalanceInterval, Anchor, AuxiliaryData, Certificate, Constitution, CostModels, DRep,
    ExUnitPrices, ExUnits, GovAction, GovActionId, NativeScript, OutputDatum, PlutusData,
    PoolMetadata, ProposalProcedure, ProtocolParamUpdate, Rational, Redeemer, Relay, ScriptRef,
    SubTransaction, Transaction, TransactionBody, TransactionInput, TransactionMetadatum,
    TransactionOutput, TransactionWitnessSet, Vote, Voter, VotingProcedure,
};
use dugite_primitives::value::{AssetName, Lovelace, Value};

use super::{block as blk, certificate, governance, script, transaction, value as val};
use crate::cbor::*;

pub fn uint(v: u64) -> Vec<u8> {
    encode_uint(v)
}

pub fn bytes(b: &[u8]) -> Vec<u8> {
    encode_bytes(b)
}

/// `network_id`: written back as the `uint` it was read from; a value the decoder turned into
/// `None` has no encoding, so `null` stands in.
pub fn network_id(n: &Option<u8>) -> Vec<u8> {
    match n {
        Some(v) => encode_uint(u64::from(*v)),
        None => encode_null(),
    }
}

pub fn lovelace(v: &Lovelace) -> Vec<u8> {
    encode_uint(v.0)
}

pub fn hash28(h: &Hash28) -> Vec<u8> {
    encode_hash28(h)
}

pub fn hash32(h: &Hash32) -> Vec<u8> {
    encode_hash32(h)
}

pub fn rational(r: &Rational) -> Vec<u8> {
    certificate::encode_rational(r)
}

pub fn vote(v: &Vote) -> Vec<u8> {
    encode_uint(match v {
        Vote::No => 0,
        Vote::Yes => 1,
        Vote::Abstain => 2,
    })
}

pub fn ex_units(e: &ExUnits) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_uint(e.mem));
    buf.extend(encode_uint(e.steps));
    buf
}

pub fn ex_unit_prices(p: &ExUnitPrices) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(certificate::encode_rational(&p.mem_price));
    buf.extend(certificate::encode_rational(&p.step_price));
    buf
}

pub fn protocol_version(pv: &ProtocolVersion) -> Vec<u8> {
    blk::encode_protocol_version(pv)
}

pub fn operational_cert(c: &OperationalCert) -> Vec<u8> {
    blk::encode_operational_cert(c)
}

/// A block as `[header, tx_bodies, tx_witness_sets, aux_data, invalid_txs]`, with no era
/// envelope (the dataset's `block` rule).  `encode_block` writes `[era_tag, block]`; the era tag
/// of every era the dataset covers is one byte, so the block is everything after the two-byte
/// envelope head.
pub fn block(b: &Block) -> Vec<u8> {
    let full = blk::encode_block(b, &b.header.kes_signature);
    debug_assert!(full.len() > 2 && full[0] == 0x82 && full[1] < 24);
    full[2..].to_vec()
}

/// A block header `[header_body, kes_signature]`.
pub fn block_header(h: &BlockHeader) -> Vec<u8> {
    blk::encode_block_header(h, &h.kes_signature)
}

pub fn transaction(tx: &Transaction) -> Vec<u8> {
    transaction::encode_transaction(tx)
}

pub fn transaction_body(body: &TransactionBody, era: Era) -> Vec<u8> {
    transaction::encode_transaction_body_for_era(body, era)
}

pub fn sub_transaction_body(sub: &SubTransaction) -> Vec<u8> {
    transaction::encode_sub_tx_body(sub)
}

pub fn transaction_witness_set(ws: &TransactionWitnessSet, era: Era) -> Vec<u8> {
    transaction::encode_witness_set_for_era(ws, era)
}

pub fn transaction_input(i: &TransactionInput) -> Vec<u8> {
    encode_tx_input(i)
}

pub fn transaction_output(o: &TransactionOutput) -> Vec<u8> {
    transaction::encode_transaction_output(o)
}

pub fn value(v: &Value) -> Vec<u8> {
    val::encode_value(v)
}

pub fn withdrawals(w: &BTreeMap<Vec<u8>, Lovelace>) -> Vec<u8> {
    transaction::encode_withdrawals(w)
}

pub fn mint(m: &BTreeMap<Hash28, BTreeMap<AssetName, i64>>) -> Vec<u8> {
    val::encode_mint(m)
}

pub fn multiasset(m: &BTreeMap<Hash28, BTreeMap<AssetName, u64>>) -> Vec<u8> {
    val::encode_multi_asset(m)
}

/// `datum_option = [0, hash] / [1, #6.24(bytes .cbor data)]`.
pub fn datum_option(d: &OutputDatum) -> Vec<u8> {
    transaction::encode_datum_option(d)
}

pub fn address(a: &dugite_primitives::address::Address) -> Vec<u8> {
    encode_bytes(&a.to_bytes())
}

/// A `script` as `[script_type, script]`.
pub fn script(s: &ScriptRef) -> Vec<u8> {
    script::encode_script_ref(s)
}

pub fn certificate(c: &Certificate) -> Vec<u8> {
    certificate::encode_certificate(c)
}

/// An ordered set of certificates, as the transaction body encodes them (Conway and later).
pub fn certificates(cs: &[Certificate]) -> Vec<u8> {
    transaction::encode_ordered_set(cs, certificate::encode_certificate)
}

pub fn credential(c: &Credential) -> Vec<u8> {
    certificate::encode_credential(c)
}

pub fn drep(d: &DRep) -> Vec<u8> {
    governance::encode_drep(d)
}

pub fn anchor(a: &Anchor) -> Vec<u8> {
    certificate::encode_anchor(a)
}

pub fn gov_action_id(id: &GovActionId) -> Vec<u8> {
    governance::encode_gov_action_id(id)
}

pub fn voter(v: &Voter) -> Vec<u8> {
    governance::encode_voter(v)
}

pub fn voting_procedure(p: &VotingProcedure) -> Vec<u8> {
    governance::encode_voting_procedure(p)
}

pub fn voting_procedures(p: &BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>>) -> Vec<u8> {
    governance::encode_voting_procedures(p)
}

pub fn proposal_procedure(p: &ProposalProcedure) -> Vec<u8> {
    governance::encode_proposal_procedure(p)
}

/// An ordered set of proposal procedures, as the transaction body encodes them.
pub fn proposal_procedures(ps: &[ProposalProcedure]) -> Vec<u8> {
    transaction::encode_ordered_set(ps, governance::encode_proposal_procedure)
}

pub fn gov_action(a: &GovAction) -> Vec<u8> {
    governance::encode_gov_action(a)
}

pub fn constitution(c: &Constitution) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(certificate::encode_anchor(&c.anchor));
    match &c.script_hash {
        Some(h) => buf.extend(encode_hash28(h)),
        None => buf.extend(encode_null()),
    }
    buf
}

pub fn protocol_param_update(p: &ProtocolParamUpdate) -> Vec<u8> {
    super::protocol_params::encode_protocol_param_update(p)
}

pub fn cost_models(c: &CostModels) -> Vec<u8> {
    super::protocol_params::encode_cost_models(c)
}

pub fn account_balance_interval(i: &AccountBalanceInterval) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    for bound in [&i.lower, &i.upper] {
        match bound {
            Some(l) => buf.extend(encode_uint(l.0)),
            None => buf.extend(encode_null()),
        }
    }
    buf
}

pub fn account_balance_intervals(m: &[(Credential, AccountBalanceInterval)]) -> Vec<u8> {
    let mut buf = encode_map_header(m.len());
    for (c, i) in m {
        buf.extend(certificate::encode_credential(c));
        buf.extend(account_balance_interval(i));
    }
    buf
}

pub fn native_script(s: &NativeScript) -> Vec<u8> {
    script::encode_native_script(s)
}

pub fn plutus_data(d: &PlutusData) -> Vec<u8> {
    encode_plutus_data(d)
}

/// `data = #6.24(bytes .cbor plutus_data)`.
pub fn data(d: &PlutusData) -> Vec<u8> {
    let mut buf = encode_tag(24);
    buf.extend(encode_bytes(&encode_plutus_data(d)));
    buf
}

pub fn redeemer(r: &Redeemer) -> Vec<u8> {
    script::encode_redeemer(r)
}

pub fn redeemers(rs: &[Redeemer]) -> Vec<u8> {
    transaction::encode_redeemers(rs, true)
}

pub fn auxiliary_data(a: &AuxiliaryData) -> Vec<u8> {
    transaction::encode_auxiliary_data(a)
}

pub fn metadatum(m: &TransactionMetadatum) -> Vec<u8> {
    encode_metadatum(m)
}

pub fn metadata(m: &BTreeMap<u64, TransactionMetadatum>) -> Vec<u8> {
    script::encode_metadata_map(m)
}

pub fn relay(r: &Relay) -> Vec<u8> {
    certificate::encode_relay(r)
}

pub fn pool_metadata(m: &PoolMetadata) -> Vec<u8> {
    certificate::encode_pool_metadata(m)
}
