//! From a decoded failure to the scenario's vocabulary.
//!
//! The decoder yields Haskell constructor names read from the wire.  The scenario has its own
//! vocabulary ([`Vocabulary`], the 81 `result` names in its `schema.json`), mostly equal to the
//! constructor name.  The exceptions are in [`ndc_names::HASKELL_TO_NDC`]; three kinds of failure
//! need more than the constructor to be named, and read it from the same bytes (or the
//! transaction that was rejected):
//!
//! * `StakeKeyNotRegisteredDELEG` carries only a credential; the scenario names it by the kind of
//!   certificate that referred to the credential (unregistration, pool delegation, vote
//!   delegation), so the transaction's own certificates decide among the candidates.
//! * `CollectErrors` carries a list of `CollectError`s; each is a failure of its own and is named
//!   by its payload (`NoCostModel`, `TimeTranslationPastHorizon`, a context error, ...).
//! * `ConwayMempoolFailure` is dugite's generic fallback.  It has NO name, by design: a failure the
//!   encoder could not type must never be scored as the failure it stood in for.

use std::collections::BTreeSet;
use std::path::Path;

use dugite_primitives::credentials::Credential;
use dugite_primitives::transaction::{Certificate, Transaction};
use minicbor::Decoder;
use serde_json::Value;

use super::decode::{decode_failure_in, Failure};
use super::ndc_names::HASKELL_TO_NDC;

/// The 81 `result` names of the scenario schema.
#[derive(Debug, Clone)]
pub struct Vocabulary(BTreeSet<String>);

impl Vocabulary {
    /// Read the `oneOf` constants of `scenarios.items.properties.result` from the scenario's own
    /// `schema.json`, so the vocabulary is the scenario's, not a copy of it.
    pub fn from_schema(schema: &Value) -> Result<Self, String> {
        let one_of = schema
            .pointer("/properties/scenarios/items/properties/result/oneOf")
            .and_then(Value::as_array)
            .ok_or("schema.json: no result oneOf")?;
        let names: BTreeSet<String> = one_of
            .iter()
            .map(|o| {
                o.get("const")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or("schema.json: a result alternative is not a const string")
            })
            .collect::<Result<_, _>>()?;
        if names.len() != one_of.len() {
            return Err("schema.json: duplicate result names".into());
        }
        Ok(Vocabulary(names))
    }

    pub fn load(schema_path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(schema_path)
            .map_err(|e| format!("cannot read {}: {e}", schema_path.display()))?;
        let schema: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", schema_path.display()))?;
        Self::from_schema(&schema)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// One raised failure, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// `ConwayUtxowFailure/UtxoFailure/BadInputsUTxO` and the tags walked.
    pub path: String,
    pub tags: Vec<u64>,
    /// The Haskell constructor at the leaf.
    pub haskell: String,
    /// The scenario names this failure can be reported as.  Empty means it has none.
    pub ndc: Vec<String>,
    /// dugite's generic fallback (`ConwayMempoolFailure`).
    pub generic: bool,
    /// A narrowing detail read from the payload, e.g. `PassedUnexpectedly`.
    pub detail: Option<String>,
}

fn named(f: &Failure, ndc: Vec<String>, detail: Option<String>) -> Named {
    Named {
        path: f.path.join("/"),
        tags: f.tags.clone(),
        haskell: f.leaf.clone(),
        ndc,
        generic: f.is_generic_fallback(),
        detail,
    }
}

/// The scenario names for a constructor from the transcribed table, else the constructor's own
/// name if the scenario has it.
fn by_constructor(leaf: &str, vocab: &Vocabulary) -> Vec<String> {
    let rows: Vec<String> = HASKELL_TO_NDC
        .iter()
        .filter(|(h, _, _)| *h == leaf)
        .map(|(_, n, _)| (*n).to_string())
        .collect();
    if !rows.is_empty() {
        return rows;
    }
    if vocab.contains(leaf) {
        vec![leaf.to_string()]
    } else {
        Vec::new()
    }
}

fn credential_of(d: &mut Decoder<'_>) -> Result<Credential, String> {
    let n = d.array().map_err(|e| e.to_string())?;
    if n != Some(2) {
        return Err("credential is not a 2-element array".into());
    }
    let kind = d.u8().map_err(|e| e.to_string())?;
    let bytes: [u8; 28] = d
        .bytes()
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "credential hash is not 28 bytes".to_string())?;
    let hash = dugite_primitives::hash::Hash28::from_bytes(bytes);
    match kind {
        0 => Ok(Credential::VerificationKey(hash)),
        1 => Ok(Credential::Script(hash)),
        n => Err(format!("credential kind {n}")),
    }
}

/// `StakeKeyNotRegisteredDELEG` is `[3, Credential]`.  The scenario names it by the certificate
/// that referred to the credential.
fn stake_key_not_registered(f: &Failure, tx: &Transaction) -> Result<Vec<String>, String> {
    let mut d = Decoder::new(&f.raw);
    d.array().map_err(|e| e.to_string())?;
    d.u64().map_err(|e| e.to_string())?;
    let cred = credential_of(&mut d)?;
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    };
    for cert in &tx.body.certificates {
        match cert {
            Certificate::StakeDeregistration(c)
            | Certificate::ConwayStakeDeregistration { credential: c, .. }
                if *c == cred =>
            {
                push("StakeKeyNotRegistered")
            }
            Certificate::StakeDelegation { credential: c, .. } if *c == cred => {
                push("StakeCredentialInvalidPoolDelegation")
            }
            // A stake-and-vote delegation counts as the vote's (see ndc_names.rs).
            Certificate::VoteDelegation { credential: c, .. }
            | Certificate::StakeVoteDelegation { credential: c, .. }
                if *c == cred =>
            {
                push("StakeCredentialInvalidVoteDelegation")
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `CollectErrors` is `[1, [CollectError, ...]]`.  Each element is a failure of its own.
fn collect_errors(f: &Failure, vocab: &Vocabulary) -> Result<Vec<Named>, String> {
    let mut d = Decoder::new(&f.raw);
    if d.array().map_err(|e| e.to_string())? != Some(2) {
        return Err("CollectErrors is not a 2-element array".into());
    }
    d.u64().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut each = |d: &mut Decoder<'_>| -> Result<(), String> {
        let item = decode_failure_in(d, "CollectError")?;
        let leaf = item.leaf.clone();
        // The leaf of a CollectError is a context-error constructor, or NoCostModel.
        let ndc = match leaf.as_str() {
            "NoCostModel" => vec!["MissingCostModel".to_string()],
            "TimeTranslationPastHorizon" => vec!["OutsideForecast".to_string()],
            other if vocab.contains(other) => vec![other.to_string()],
            _ => Vec::new(),
        };
        let mut n = named(&item, ndc, None);
        n.path = format!("{}/CollectErrors/{}", f.path.join("/"), item.path.join("/"));
        out.push(n);
        Ok(())
    };
    match d.array().map_err(|e| e.to_string())? {
        Some(n) => {
            for _ in 0..n {
                each(&mut d)?;
            }
        }
        None => {
            while d.datatype().map_err(|e| e.to_string())? != minicbor::data::Type::Break {
                each(&mut d)?;
            }
            d.skip().map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

/// `ValidationTagMismatch` is `[0, isValid, TagMismatchDescription]`.
fn tag_mismatch_detail(f: &Failure) -> Result<String, String> {
    let mut d = Decoder::new(&f.raw);
    d.array().map_err(|e| e.to_string())?;
    d.u64().map_err(|e| e.to_string())?;
    d.bool().map_err(|e| e.to_string())?;
    let description = decode_failure_in(&mut d, "TagMismatchDescription")?;
    Ok(description.leaf)
}

/// Name every failure the wire carries, in order.  A failure with no scenario name has an empty
/// `ndc`.
pub fn name_failure(
    f: &Failure,
    tx: &Transaction,
    vocab: &Vocabulary,
) -> Result<Vec<Named>, String> {
    if f.unknown || f.is_generic_fallback() {
        return Ok(vec![named(f, Vec::new(), None)]);
    }
    match f.leaf.as_str() {
        "StakeKeyNotRegisteredDELEG" => {
            let ndc = stake_key_not_registered(f, tx)?;
            Ok(vec![named(f, ndc, None)])
        }
        "CollectErrors" => collect_errors(f, vocab),
        "ValidationTagMismatch" => {
            let detail = tag_mismatch_detail(f)?;
            Ok(vec![named(f, by_constructor(&f.leaf, vocab), Some(detail))])
        }
        leaf => Ok(vec![named(f, by_constructor(leaf, vocab), None)]),
    }
}
