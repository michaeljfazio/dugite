//! No input-byte reuse.
//!
//! Dugite's decoders keep the original bytes of several items next to the typed value (so that a
//! transaction hash, a script integrity hash or a relayed block never depends on a re-encoding),
//! and several encoders PREFER those bytes over encoding the value.  A harness that decoded and
//! re-encoded without clearing them would be comparing the input with itself.
//!
//! Between decode and encode every value goes through [`Scrub::scrub`], which clears each such
//! field and returns the names of the ones that were populated.  Afterwards [`Scrub::populated`]
//! must be empty; if it is not, that is a HARNESS defect and [`checked_scrub`] panics with a
//! [`HarnessDefect`] (which the runner never turns into a sample failure).
//!
//! The list of fields is audited by `every_preserved_bytes_field_is_covered`, which scans the
//! source of `dugite-primitives` for any field that looks like preserved bytes and fails when one
//! is not on the audited list below.

use dugite_primitives::block::{Block, BlockHeader};
use dugite_primitives::transaction::{
    AuxiliaryData, OutputDatum, SubTransaction, Transaction, TransactionBody, TransactionOutput,
    TransactionWitnessSet,
};

use super::HarnessDefect;

/// Every field of `dugite-primitives` that holds the original bytes of an item, as
/// `(type, field)`.  The audit test scans both crates for every field whose type carries bytes
/// (`Vec<u8>`, `Bytes`, `Arc<[u8]>`, `Box<[u8]>`, `KeepRaw<..>`, also inside `Option`) and for any
/// field named like `raw_*` / `original_*`, and requires each to be here or to be listed in
/// [`NOT_PRESERVED_BYTES`] with the reason it is not.
pub const PRESERVED_BYTES_FIELDS: &[(&str, &str)] = &[
    ("TransactionOutput", "raw_cbor"),
    ("OutputDatum::InlineDatum", "raw_cbor"),
    ("Transaction", "raw_cbor"),
    ("Transaction", "raw_body_cbor"),
    ("Transaction", "raw_witness_cbor"),
    ("SubTransaction", "raw_body_cbor"),
    ("TransactionWitnessSet", "raw_redeemers_cbor"),
    ("TransactionWitnessSet", "raw_plutus_data_cbor"),
    ("TransactionWitnessSet", "original_script_data_hash"),
    ("AuxiliaryData", "raw_cbor"),
    ("BlockHeader", "raw_header_body"),
    ("Block", "raw_cbor"),
];

/// Byte-carrying fields (of `dugite-primitives` and `dugite-serialization`) that are NOT preserved
/// input, keyed by owner type AND field, each with the reason.  The audit scan requires every
/// byte-carrying field to be on one of the two lists.
pub const NOT_PRESERVED_BYTES: &[(&str, &str, &str)] = &[
    ("Addr28Decoded", "address", "an address or reward account: the item's own value"),
    ("AssetName", "0", "the asset name: the item's own value"),
    ("AuxiliaryData", "plutus_v1_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("AuxiliaryData", "plutus_v2_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("AuxiliaryData", "plutus_v3_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("TransactionWitnessSet", "plutus_v1_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("TransactionWitnessSet", "plutus_v2_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("TransactionWitnessSet", "plutus_v3_scripts", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("BlockHeader", "issuer_vkey", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BlockHeader", "kes_signature", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BlockHeader", "nonce_vrf_output", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding (derived)"),
    ("BlockHeader", "nonce_vrf_proof", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BlockHeader", "vrf_vkey", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BootstrapWitness", "attributes", "the Byron address attributes of a bootstrap witness: the witness's own value"),
    ("BootstrapWitness", "chain_code", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BootstrapWitness", "signature", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("BootstrapWitness", "vkey", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("VKeyWitness", "signature", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("VKeyWitness", "vkey", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("OperationalCert", "hot_vkey", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("OperationalCert", "sigma", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("VrfOutput", "output", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("VrfOutput", "proof", "the item's own value (a key, signature, hash, VRF output): decoded content, not a copy of its encoding"),
    ("ByronAddress", "payload", "opaque input re-emitted verbatim by the address encoder: `Address` is NOT `NoPreservedBytes`; Byron address samples are counted apart (`samples_with_opaque_reemitted_input`)"),
    ("ByronAddressPayload", "attributes", "opaque input re-emitted verbatim by the address encoder: `Address` is NOT `NoPreservedBytes`; Byron address samples are counted apart (`samples_with_opaque_reemitted_input`)"),
    ("ByronBlockAux", "block_signature", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronBlockAux", "block_signed_bytes", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronBlockAux", "delegate_pubkey", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronBlockAux", "issuer_pubkey", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronDlgCert", "delegate_vk", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronDlgCert", "issuer_vk", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronDlgCert", "signature", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronDlgCert", "epoch_raw", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronParamsUpdate", "tx_fee_policy", "Byron-only; Byron blocks are outside both datasets (captured raw and never applied)"),
    ("ByronUpdProposal", "body_span", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronUpdProposal", "proposer_vk", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronUpdProposal", "signature", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronUpdVote", "signature", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronUpdVote", "voter_vk", "Byron-only; Byron blocks are outside both datasets"),
    ("ByronUpdVote", "proposal_id_raw", "Byron-only; Byron blocks are outside both datasets"),
    ("CompactValueDecoded", "multi_asset_raw", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("DatumOption", "datum", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellUtxoEntry", "txout_mempack", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellGovState", "committee_raw", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellGovState", "drep_pulsing_raw", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellGovState", "proposals_raw", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellSnapShotPool", "reward_account", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellStakePoolState", "reward_account", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "address", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "datum", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "multi_asset", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "opaque_tail", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "script_ref", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("KeepRaw", "raw", "decode helper, never part of a decoded value"),
    ("KeepRawOwned", "raw", "decode helper, never part of a decoded value"),
    ("Reader", "origin", "decode helper, never part of a decoded value"),
    ("PoolParams", "reward_account", "an address or reward account: the item's own value"),
    ("ProposalProcedure", "return_addr", "an address or reward account: the item's own value"),
    ("Withdrawal", "reward_account", "an address or reward account: the item's own value"),
    ("GovAction::TreasuryWithdrawals", "withdrawals", "reward-account keys of a map: the item's own value"),
    ("SubTransaction", "direct_deposits", "reward-account keys of a map: the item's own value"),
    ("SubTransaction", "withdrawals", "reward-account keys of a map: the item's own value"),
    ("TransactionBody", "direct_deposits", "reward-account keys of a map: the item's own value"),
    ("TransactionBody", "withdrawals", "reward-account keys of a map: the item's own value"),
    ("ByronAddressPayload", "root", "opaque input re-emitted verbatim by the address encoder (see the `attributes` entry)"),
    ("DatumOption", "datum_hash", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("MemPackTxOut", "datum_hash", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("Hash", "0", "the hash's own bytes: the item's own value"),
    ("HaskellRelay::SingleHostAddr", "1", "ledger-state snapshot import type: not decoded by any rule of the dataset"),
    ("HaskellRelay::SingleHostAddr", "2", "ledger-state snapshot import type: not decoded by any rule of the dataset"),
    ("PlutusData::Bytes", "0", "a PlutusData byte string: the item's own value"),
    ("RawKeyWalker", "data", "decode helper, never part of a decoded value"),
    ("TvarIterator", "data", "decode helper, never part of a decoded value"),
    ("Relay::SingleHostAddr", "ipv4", "the relay's own address bytes: the item's own value"),
    ("Relay::SingleHostAddr", "ipv6", "the relay's own address bytes: the item's own value"),
    ("ScriptRef::PlutusV1", "0", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("ScriptRef::PlutusV2", "0", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("ScriptRef::PlutusV3", "0", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("ScriptRef::PlutusV4", "0", "a Plutus script: a byte string that is the item's value; the CBOR wrapper around it is not kept"),
    ("ScriptRefKind::Native", "0", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("ScriptRefKind::Plutus", "body", "ledger-state / MemPack snapshot import type: not decoded by any rule of the dataset"),
    ("TransactionMetadatum::Bytes", "0", "a metadatum byte string: the item's own value"),
];

/// A decoded type that is KNOWN to hold no preserved input bytes, anywhere inside it.
///
/// A codec for such a type needs no [`Scrub`]; the registry accepts one only through this trait,
/// so a rule's decoded type is either scrubbed or explicitly stated clean.  The statement is
/// backed by `every_preserved_bytes_field_is_covered`: the scan finds every field of
/// `dugite-primitives` that holds preserved bytes, and none is in a type implemented here.
pub trait NoPreservedBytes {}

macro_rules! no_preserved_bytes {
    ($($t:ty),* $(,)?) => { $(impl NoPreservedBytes for $t {})* };
}

no_preserved_bytes!(
    u8,
    u64,
    i64,
    dugite_primitives::value::Lovelace,
    dugite_primitives::value::Value,
    dugite_primitives::transaction::Rational,
    dugite_primitives::transaction::Vote,
    dugite_primitives::transaction::ExUnits,
    dugite_primitives::transaction::ExUnitPrices,
    dugite_primitives::transaction::TransactionInput,
    dugite_primitives::transaction::Certificate,
    dugite_primitives::transaction::DRep,
    dugite_primitives::transaction::Anchor,
    dugite_primitives::transaction::GovActionId,
    dugite_primitives::transaction::Voter,
    dugite_primitives::transaction::VotingProcedure,
    dugite_primitives::transaction::ProposalProcedure,
    dugite_primitives::transaction::GovAction,
    dugite_primitives::transaction::Constitution,
    dugite_primitives::transaction::ProtocolParamUpdate,
    dugite_primitives::transaction::CostModels,
    dugite_primitives::transaction::NativeScript,
    dugite_primitives::transaction::PlutusData,
    dugite_primitives::transaction::Redeemer,
    dugite_primitives::transaction::TransactionMetadatum,
    dugite_primitives::transaction::Relay,
    dugite_primitives::transaction::PoolMetadata,
    dugite_primitives::transaction::ScriptRef,
    dugite_primitives::transaction::AccountBalanceInterval,
    dugite_primitives::credentials::Credential,
    dugite_primitives::block::ProtocolVersion,
    dugite_primitives::block::OperationalCert,
    dugite_primitives::value::AssetName,
);

impl<T: NoPreservedBytes> NoPreservedBytes for Vec<T> {}
impl<T: NoPreservedBytes> NoPreservedBytes for Option<T> {}
impl<A: NoPreservedBytes, B: NoPreservedBytes> NoPreservedBytes for (A, B) {}
impl<K: NoPreservedBytes, V: NoPreservedBytes> NoPreservedBytes
    for std::collections::BTreeMap<K, V>
{
}
impl<const N: usize> NoPreservedBytes for dugite_primitives::hash::Hash<N> {}

/// A value that may carry preserved input bytes.
pub trait Scrub {
    /// Clear every preserved-bytes field; return `type.field` of each one that was populated.
    fn scrub(&mut self) -> Vec<&'static str>;
    /// The `type.field` of every preserved-bytes field that is populated.
    fn populated(&self) -> Vec<&'static str>;
}

impl Scrub for OutputDatum {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if let OutputDatum::InlineDatum { raw_cbor, .. } = self {
            if raw_cbor.take().is_some() {
                found.push("InlineDatum.raw_cbor");
            }
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        match self {
            OutputDatum::InlineDatum {
                raw_cbor: Some(_), ..
            } => vec!["InlineDatum.raw_cbor"],
            _ => Vec::new(),
        }
    }
}

impl Scrub for TransactionOutput {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = self.datum.scrub();
        if self.raw_cbor.take().is_some() {
            found.push("TransactionOutput.raw_cbor");
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = self.datum.populated();
        if self.raw_cbor.is_some() {
            found.push("TransactionOutput.raw_cbor");
        }
        found
    }
}

impl Scrub for TransactionWitnessSet {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_redeemers_cbor.take().is_some() {
            found.push("TransactionWitnessSet.raw_redeemers_cbor");
        }
        if self.raw_plutus_data_cbor.take().is_some() {
            found.push("TransactionWitnessSet.raw_plutus_data_cbor");
        }
        if self.original_script_data_hash.take().is_some() {
            found.push("TransactionWitnessSet.original_script_data_hash");
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_redeemers_cbor.is_some() {
            found.push("TransactionWitnessSet.raw_redeemers_cbor");
        }
        if self.raw_plutus_data_cbor.is_some() {
            found.push("TransactionWitnessSet.raw_plutus_data_cbor");
        }
        if self.original_script_data_hash.is_some() {
            found.push("TransactionWitnessSet.original_script_data_hash");
        }
        found
    }
}

impl Scrub for AuxiliaryData {
    fn scrub(&mut self) -> Vec<&'static str> {
        if self.raw_cbor.take().is_some() {
            vec!["AuxiliaryData.raw_cbor"]
        } else {
            Vec::new()
        }
    }
    fn populated(&self) -> Vec<&'static str> {
        if self.raw_cbor.is_some() {
            vec!["AuxiliaryData.raw_cbor"]
        } else {
            Vec::new()
        }
    }
}

impl Scrub for SubTransaction {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_body_cbor.take().is_some() {
            found.push("SubTransaction.raw_body_cbor");
        }
        for o in &mut self.outputs {
            found.extend(o.scrub());
        }
        found.extend(self.witness_set.scrub());
        if let Some(a) = self.auxiliary_data.as_mut() {
            found.extend(a.scrub());
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_body_cbor.is_some() {
            found.push("SubTransaction.raw_body_cbor");
        }
        for o in &self.outputs {
            found.extend(o.populated());
        }
        found.extend(self.witness_set.populated());
        if let Some(a) = self.auxiliary_data.as_ref() {
            found.extend(a.populated());
        }
        found
    }
}

impl Scrub for TransactionBody {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        for o in &mut self.outputs {
            found.extend(o.scrub());
        }
        if let Some(o) = self.collateral_return.as_mut() {
            found.extend(o.scrub());
        }
        for s in &mut self.sub_transactions {
            found.extend(s.scrub());
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        for o in &self.outputs {
            found.extend(o.populated());
        }
        if let Some(o) = self.collateral_return.as_ref() {
            found.extend(o.populated());
        }
        for s in &self.sub_transactions {
            found.extend(s.populated());
        }
        found
    }
}

impl Scrub for Transaction {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_cbor.take().is_some() {
            found.push("Transaction.raw_cbor");
        }
        if self.raw_body_cbor.take().is_some() {
            found.push("Transaction.raw_body_cbor");
        }
        if self.raw_witness_cbor.take().is_some() {
            found.push("Transaction.raw_witness_cbor");
        }
        found.extend(self.body.scrub());
        found.extend(self.witness_set.scrub());
        if let Some(a) = self.auxiliary_data.as_mut() {
            found.extend(a.scrub());
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_cbor.is_some() {
            found.push("Transaction.raw_cbor");
        }
        if self.raw_body_cbor.is_some() {
            found.push("Transaction.raw_body_cbor");
        }
        if self.raw_witness_cbor.is_some() {
            found.push("Transaction.raw_witness_cbor");
        }
        found.extend(self.body.populated());
        found.extend(self.witness_set.populated());
        if let Some(a) = self.auxiliary_data.as_ref() {
            found.extend(a.populated());
        }
        found
    }
}

impl Scrub for BlockHeader {
    fn scrub(&mut self) -> Vec<&'static str> {
        if self.raw_header_body.take().is_some() {
            vec!["BlockHeader.raw_header_body"]
        } else {
            Vec::new()
        }
    }
    fn populated(&self) -> Vec<&'static str> {
        if self.raw_header_body.is_some() {
            vec!["BlockHeader.raw_header_body"]
        } else {
            Vec::new()
        }
    }
}

impl Scrub for Block {
    fn scrub(&mut self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_cbor.take().is_some() {
            found.push("Block.raw_cbor");
        }
        found.extend(self.header.scrub());
        for tx in &mut self.transactions {
            found.extend(tx.scrub());
        }
        found
    }
    fn populated(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.raw_cbor.is_some() {
            found.push("Block.raw_cbor");
        }
        found.extend(self.header.populated());
        for tx in &self.transactions {
            found.extend(tx.populated());
        }
        found
    }
}

/// Field names that hold byte content, not preserved input, and may be populated in a decoded
/// value (an empty `Option<Vec<u8>>` is never a hit; these are populated ones).
const CONTENT_OPTION_BYTES: &[&str] = &[
    "tx_fee_policy",
    "ipv4",
    "ipv6",
    "plutus_v1",
    "plutus_v2",
    "plutus_v3",
    "plutus_v4",
];

/// Look at the `Debug` rendering of a value for a populated preserved-bytes field, without
/// consulting [`Scrub`]: any field named `raw_*` / `*_raw` / `original_*` holding `Some(..)`, and
/// any `Option` of bytes (`Some([..` rendering) that is not a known content field.  `Debug` prints
/// every field, including ones a serde `skip` hides, so a traversal gap in the hand-written
/// scrub is caught here.
pub fn independent_preserved_bytes(debug: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let bytes = debug.as_bytes();
    let mut i = 0;
    while let Some(pos) = debug[i..].find(": Some(") {
        let at = i + pos;
        let name_start = debug[..at]
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |p| p + 1);
        let name = &debug[name_start..at];
        let after = at + ": Some(".len();
        let looks_like_bytes = bytes.get(after) == Some(&b'[');
        let named =
            name.starts_with("raw_") || name.ends_with("_raw") || name.starts_with("original_");
        if (named || looks_like_bytes) && !CONTENT_OPTION_BYTES.contains(&name) {
            hits.push(name.to_string());
        }
        i = after;
    }
    hits.sort();
    hits.dedup();
    hits
}

/// Clear every preserved-bytes field of `value`, then verify none remains.  A remaining field is
/// a harness defect: the encoder could reuse input bytes, so the result would prove nothing.
pub fn checked_scrub<T: Scrub + std::fmt::Debug>(value: &mut T) -> Vec<&'static str> {
    let cleared = value.scrub();
    let mut left: Vec<String> = value.populated().iter().map(|s| (*s).to_string()).collect();
    left.extend(independent_preserved_bytes(&format!("{value:?}")));
    if !left.is_empty() {
        std::panic::panic_any(HarnessDefect(format!(
            "preserved input bytes remain after scrubbing: {}",
            left.join(", ")
        )));
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_diversity::pins::repo_root;

    /// Type spellings that carry bytes, matched on the type with whitespace removed, after
    /// resolving type aliases: a byte vector, `Bytes`, any byte slice (`Arc<[u8]>`, `Box<[u8]>`,
    /// `Cow<'_, [u8]>`, `&[u8]`), a fixed byte array, and the raw-capturing wrappers.  Also inside
    /// `Option`, tuples and maps, because the match is on the rendered type.
    const BYTE_TYPES: [&str; 5] = ["Vec<u8>", "Bytes", "[u8]", "[u8;", "KeepRaw"];

    fn render(t: &impl quote::ToTokens) -> String {
        t.to_token_stream().to_string().replace(' ', "")
    }

    fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs
            .iter()
            .any(|a| a.path().is_ident("cfg") && render(a).contains("test"))
    }

    fn rs_files(dir: std::path::PathBuf, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rs_files(path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[derive(Default)]
    struct Found {
        aliases: Vec<(String, String)>,
        /// (owner, field, rendered type)
        fields: Vec<(String, String, String)>,
    }

    fn walk(items: &[syn::Item], found: &mut Found) {
        use syn::{Fields, Item};
        fn record(owner: &str, fields: &Fields, found: &mut Found) {
            for (i, f) in fields.iter().enumerate() {
                let name = f
                    .ident
                    .as_ref()
                    .map_or_else(|| i.to_string(), ToString::to_string);
                found.fields.push((owner.to_string(), name, render(&f.ty)));
            }
        }
        for item in items {
            match item {
                Item::Struct(s) if !is_cfg_test(&s.attrs) => {
                    record(&s.ident.to_string(), &s.fields, found)
                }
                Item::Enum(e) if !is_cfg_test(&e.attrs) => {
                    for v in &e.variants {
                        record(&format!("{}::{}", e.ident, v.ident), &v.fields, found);
                    }
                }
                Item::Type(t) if !is_cfg_test(&t.attrs) => {
                    found.aliases.push((t.ident.to_string(), render(&t.ty)));
                }
                Item::Mod(m) if !is_cfg_test(&m.attrs) => {
                    if let Some((_, inner)) = &m.content {
                        walk(inner, found);
                    }
                }
                _ => {}
            }
        }
    }

    /// `ty` followed by the target of every alias it mentions (repeatedly), so `Option<Wire>` with
    /// `type Wire = Vec<u8>` is seen as carrying bytes.
    fn with_aliases(ty: &str, aliases: &[(String, String)]) -> String {
        let mut out = ty.to_string();
        for _ in 0..4 {
            let mut grew = false;
            for (name, target) in aliases {
                let mentioned = out.match_indices(name.as_str()).any(|(i, _)| {
                    let before = out[..i].chars().next_back();
                    let after = out[i + name.len()..].chars().next();
                    !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                        && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                });
                if mentioned && !out.contains(target.as_str()) {
                    out.push('|');
                    out.push_str(target);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        out
    }

    /// Every `(owner, field, type)` of a struct field, tuple field or enum-variant field (named or
    /// positional, the type wherever the source puts it) in `dugite-primitives` and
    /// `dugite-serialization`, excluding `#[cfg(test)]` items, whose type carries bytes
    /// ([`BYTE_TYPES`], through aliases) or whose name looks like preserved bytes (`raw_*`,
    /// `*_raw`, `original_*`).  Parsed with `syn`, so layout does not matter.  Enum variants are
    /// owned as `Enum::Variant`; positional fields are named `0`, `1`, ...
    fn candidate_fields() -> Vec<(String, String, String)> {
        let mut files = Vec::new();
        rs_files(repo_root().join("crates/dugite-primitives/src"), &mut files);
        rs_files(
            repo_root().join("crates/dugite-serialization/src"),
            &mut files,
        );
        files.sort();
        let mut found = Found::default();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let ast = syn::parse_file(&text)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", file.display()));
            walk(&ast.items, &mut found);
        }
        candidates_of(&found)
    }

    fn candidates_of(found: &Found) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (owner, field, ty) in &found.fields {
            let by_name = field.starts_with("raw_")
                || field.ends_with("_raw")
                || field.starts_with("original_");
            let resolved = with_aliases(ty, &found.aliases);
            if BYTE_TYPES.iter().any(|b| resolved.contains(b)) || by_name {
                out.push((owner.clone(), field.clone(), ty.clone()));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The scan, on a source snippet: the shapes a line-based scan missed.
    #[test]
    fn the_scan_sees_every_field_shape() {
        let src = r#"
            type Wire = Vec<u8>;
            pub enum E { Kept(Vec<u8>), Named { x: std::borrow::Cow<'static, [u8]> } }
            pub struct Next {
                pub field:
                    Option<Wire>,
            }
            pub struct Multi(
                pub u32,
                pub Box<[u8]>,
            );
            pub struct Arc1 { a: std::sync::Arc<[u8]>, b: bytes::Bytes, c: Wrapper }
            pub struct Named2 { pub raw_thing: u64 }
            #[cfg(test)]
            mod tests { pub struct Hidden { pub v: Vec<u8> } }
            pub struct AfterTests { pub v: Vec<u8> }
            pub struct Clean { pub n: u64 }
        "#;
        let mut found = Found::default();
        walk(&syn::parse_file(src).unwrap().items, &mut found);
        let got: Vec<String> = candidates_of(&found)
            .into_iter()
            .map(|(o, f, _)| format!("{o}.{f}"))
            .collect();
        for want in [
            "E::Kept.0",
            "E::Named.x",
            "Next.field",
            "Multi.1",
            "Arc1.a",
            "Arc1.b",
            "Named2.raw_thing",
            "AfterTests.v",
        ] {
            assert!(got.iter().any(|g| g == want), "missed {want}: {got:?}");
        }
        for not in ["Hidden.v", "Clean.n", "Multi.0", "Arc1.c"] {
            assert!(
                !got.iter().any(|g| g == not),
                "wrongly found {not}: {got:?}"
            );
        }
    }

    fn owner_matches(audited: &str, owner: &str) -> bool {
        audited == owner || audited.split("::").next() == Some(owner)
    }

    /// Every byte-carrying field of the two crates is either preserved input (scrubbed) or on the
    /// exemption list keyed by owner AND field with a reason.  A field added tomorrow fails here
    /// until someone decides which it is.
    #[test]
    fn every_preserved_bytes_field_is_covered() {
        let candidates = candidate_fields();
        let mut unaccounted = Vec::new();
        for (owner, field, ty) in &candidates {
            let preserved = PRESERVED_BYTES_FIELDS
                .iter()
                .any(|(o, f)| f == field && owner_matches(o, owner));
            let exempt = NOT_PRESERVED_BYTES
                .iter()
                .any(|(o, f, _)| f == field && o == owner);
            if !preserved && !exempt {
                unaccounted.push(format!("(\"{owner}\", \"{field}\"): {ty}"));
            }
        }
        assert!(
            unaccounted.is_empty(),
            "byte-carrying fields neither scrubbed nor exempted with a reason:\n{}\nadd them to \
             PRESERVED_BYTES_FIELDS (and implement Scrub) or to NOT_PRESERVED_BYTES (owner, field, reason)",
            unaccounted.join("\n")
        );
        // The other direction: neither list may rot.
        for (owner, field) in PRESERVED_BYTES_FIELDS {
            assert!(
                candidates
                    .iter()
                    .any(|(o, f, _)| f == field && owner_matches(owner, o)),
                "audited field {owner}.{field} no longer exists: update PRESERVED_BYTES_FIELDS"
            );
        }
        for (owner, field, reason) in NOT_PRESERVED_BYTES {
            assert!(!reason.is_empty());
            assert!(
                candidates.iter().any(|(o, f, _)| f == field && o == owner),
                "exempted field {owner}.{field} no longer exists: update NOT_PRESERVED_BYTES"
            );
        }
    }

    /// The verification after a scrub does not trust the scrub's own enumeration: it formats the
    /// whole value with `Debug` (which prints every field, `serde(skip)` or not) and looks for a
    /// populated `Option` of bytes or a populated field named like preserved bytes.
    #[test]
    fn the_independent_check_sees_a_field_the_scrub_does_not_know() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Unknown {
            raw_extra: Option<Vec<u8>>,
        }
        let leaky = Unknown {
            raw_extra: Some(vec![1, 2]),
        };
        assert_eq!(
            super::independent_preserved_bytes(&format!("{leaky:?}")),
            vec!["raw_extra".to_string()]
        );
        let clean = Unknown { raw_extra: None };
        assert!(super::independent_preserved_bytes(&format!("{clean:?}")).is_empty());
    }
}
