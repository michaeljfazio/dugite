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
/// `(type, field)`.  The audit test scans for `Option<Vec<u8>>` fields and for any field named
/// like `raw_*` / `original_*`, and requires each to be here (or to be listed in
/// [`NOT_PRESERVED_BYTES`] with the reason it is not).
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

/// Fields the audit scan finds that are NOT preserved input bytes, with the reason.
pub const NOT_PRESERVED_BYTES: &[(&str, &str, &str)] = &[
    (
        "Byron",
        "tx_fee_policy",
        "Byron-only update parameter, captured raw and never applied; no Conway or Dijkstra item holds it",
    ),
    (
        "ByronDlgCert",
        "epoch_raw",
        "Byron delegation certificate field; Byron blocks are outside both datasets",
    ),
    (
        "ByronUpdVote",
        "proposal_id_raw",
        "Byron update vote field; Byron blocks are outside both datasets",
    ),
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
    dugite_primitives::address::Address,
    dugite_primitives::block::ProtocolVersion,
    dugite_primitives::block::OperationalCert,
    dugite_primitives::value::AssetName,
);

impl<T: NoPreservedBytes> NoPreservedBytes for Vec<T> {}
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

/// Clear every preserved-bytes field of `value`, then verify none remains.  A remaining field is
/// a harness defect: the encoder could reuse input bytes, so the result would prove nothing.
pub fn checked_scrub<T: Scrub>(value: &mut T) -> Vec<&'static str> {
    let cleared = value.scrub();
    let left = value.populated();
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

    /// Every `(owner, field, type)` of a field in `dugite-primitives` (outside `#[cfg(test)]`
    /// modules) that could hold preserved input bytes: an `Option<Vec<u8>>`, or a field named
    /// like `raw_*`, `*_raw`, `original_*`.
    fn candidate_fields() -> Vec<(String, String, String)> {
        let dir = repo_root().join("crates/dugite-primitives/src");
        let mut files = Vec::new();
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        let mut found = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let mut owner = String::new();
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with("#[cfg(test)]") {
                    break;
                }
                for kw in ["struct ", "enum "] {
                    if let Some(rest) = t
                        .strip_prefix(&format!("pub {kw}"))
                        .or_else(|| t.strip_prefix(kw))
                    {
                        owner = rest
                            .split(|c: char| !c.is_alphanumeric() && c != '_')
                            .next()
                            .unwrap_or("")
                            .to_string();
                    }
                }
                if t.starts_with("//") {
                    continue;
                }
                let t = t.strip_prefix("pub ").unwrap_or(t);
                let Some((name, ty)) = t.split_once(':') else {
                    continue;
                };
                let name = name.trim();
                let ty = ty.trim().trim_end_matches(',').trim();
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let by_name = name.starts_with("raw_")
                    || name.ends_with("_raw")
                    || name.starts_with("original_");
                // `raw_cbor: None,` in a struct literal is an initialiser, not a field.
                if ty == "None" || ty.starts_with("Some(") {
                    continue;
                }
                if ty.contains("Option<Vec<u8>>") || by_name {
                    found.push((owner.clone(), name.to_string(), ty.to_string()));
                }
            }
        }
        found
    }

    /// Every field of `dugite-primitives` that could hold preserved input bytes is either on the
    /// audited list (and scrubbed) or on the list of fields that are not (with the reason).  A
    /// field added tomorrow fails here until someone decides which it is.
    #[test]
    fn every_preserved_bytes_field_is_covered() {
        let candidates = candidate_fields();
        let owner_matches = |audited: &str, owner: &str| {
            audited == owner || audited.split("::").next() == Some(owner)
        };
        let mut unaccounted = Vec::new();
        for (owner, field, ty) in &candidates {
            let preserved = PRESERVED_BYTES_FIELDS
                .iter()
                .any(|(o, f)| f == field && owner_matches(o, owner));
            let not_preserved = NOT_PRESERVED_BYTES.iter().any(|(_, f, _)| f == field);
            if !preserved && !not_preserved {
                unaccounted.push(format!("{owner}.{field}: {ty}"));
            }
        }
        assert!(
            unaccounted.is_empty(),
            "fields that may hold preserved input bytes are neither scrubbed nor audited as \
             harmless: {unaccounted:?}\nadd them to PRESERVED_BYTES_FIELDS (and implement Scrub) or \
             to NOT_PRESERVED_BYTES (with the reason)"
        );
        // And the other direction: every audited field still exists, so the list cannot rot.
        for (owner, field) in PRESERVED_BYTES_FIELDS {
            assert!(
                candidates
                    .iter()
                    .any(|(o, f, _)| f == field && owner_matches(owner, o)),
                "audited field {owner}.{field} no longer exists: update PRESERVED_BYTES_FIELDS"
            );
        }
    }
}
