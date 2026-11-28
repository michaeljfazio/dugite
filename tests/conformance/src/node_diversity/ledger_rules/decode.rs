//! Decoding the `ApplyTxErr` bytes `encode_apply_tx_err` emits.
//!
//! The harness never reads dugite's own error type to learn what was reported: it reads the BYTES
//! a cardano-cli would be sent, walks them through [`tag_table`] (transcribed from cardano-ledger)
//! and names every failure from its tag path.  A failure whose tag is not in the table is
//! reported as unknown, never guessed.  The generic `ConwayMempoolFailure` fallback is decoded
//! like any other constructor; [`Failure::is_generic_fallback`] lets callers refuse to count it.

use minicbor::Decoder;

use super::tag_table::{Rule, ARITY, PAYLOAD_ARITY, PAYLOAD_RULES, ROOT_RULE, RULES};

thread_local! {
    /// A replacement for [`RULES`] on this thread, for the red-proof that a wrong table entry
    /// turns a fixture red.  Never set outside tests.
    static RULES_OVERRIDE: std::cell::Cell<Option<&'static [Rule]>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with `rules` standing in for the transcribed [`RULES`] on this thread.
pub fn with_rules_override<R>(rules: &'static [Rule], f: impl FnOnce() -> R) -> R {
    RULES_OVERRIDE.with(|c| c.set(Some(rules)));
    let out = f();
    RULES_OVERRIDE.with(|c| c.set(None));
    out
}

/// A rule by Haskell type name, from the failure rules or the payload rules.
pub fn lookup(name: &str) -> Option<&'static Rule> {
    let rules = RULES_OVERRIDE.with(|c| c.get()).unwrap_or(RULES);
    rules
        .iter()
        .chain(PAYLOAD_RULES.iter())
        .find(|r| r.name == name)
}

/// The expected array length of a constructor (tag element included).
fn arity(constructor: &str) -> Option<usize> {
    ARITY
        .iter()
        .chain(PAYLOAD_ARITY.iter())
        .find(|(n, _)| *n == constructor)
        .map(|(_, a)| *a)
}

/// The HFC era index of Conway (`[[era, [failures]]]`).
pub const CONWAY_ERA_INDEX: u64 = 6;

/// One predicate failure, named from its tag path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The constructor names from the root (`ConwayLedgerPredFailure`) down to the leaf, e.g.
    /// `["ConwayUtxowFailure", "UtxoFailure", "BadInputsUTxO"]`.
    pub path: Vec<String>,
    /// The Haskell type the leaf constructor belongs to.
    pub rule: String,
    /// The leaf constructor, or `<unknown tag N in Rule>` when the table has no such tag.
    pub leaf: String,
    /// The tags walked, root to leaf.
    pub tags: Vec<u64>,
    /// The CBOR of the leaf failure array, for payload-dependent naming.
    pub raw: Vec<u8>,
    /// The tag was not in the table.
    pub unknown: bool,
}

impl Failure {
    /// `encode_apply_tx_err`'s catch-all for a failure it has no typed encoding for.  It carries
    /// free text and names nothing a Haskell node would send.
    pub fn is_generic_fallback(&self) -> bool {
        self.leaf == "ConwayMempoolFailure"
    }
}

/// What the bytes say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    /// `[[6, [failure, ...]]]`.
    Failures(Vec<Failure>),
    /// The HFC `Left` branch: the transaction's era is not the ledger's.
    WrongEra,
    /// The bytes do not have the `ApplyTxErr` shape (or an unknown era), with the reason.
    Malformed(String),
}

fn err<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Decode one failure of `rule_name` at the decoder's position (payload types inside a list).
pub fn decode_failure_in(d: &mut Decoder<'_>, rule_name: &str) -> Result<Failure, String> {
    let rule_ = lookup(rule_name).ok_or(format!("no rule {rule_name}"))?;
    decode_failure(d, rule_, &[], &[])
}

/// Decode a failure rooted at `rule_name` from `bytes` (used for payload types).
pub fn decode_failure_at(bytes: &[u8], rule_name: &str) -> Result<Failure, String> {
    let rule_ = lookup(rule_name).ok_or(format!("no rule {rule_name}"))?;
    let mut d = Decoder::new(bytes);
    let f = decode_failure(&mut d, rule_, &[], &[])?;
    ensure_end(&d, bytes)?;
    Ok(f)
}

/// Decode an `ApplyTxErr` payload (the bytes after `MsgRejectTx`'s `2`).
pub fn decode_apply_tx_err(bytes: &[u8]) -> Wire {
    match decode(bytes) {
        Ok(w) => w,
        Err(e) => Wire::Malformed(e),
    }
}

fn decode(bytes: &[u8]) -> Result<Wire, String> {
    let mut d = Decoder::new(bytes);
    let top = d.array().map_err(err("ApplyTxErr"))?;
    match top {
        // Left (MismatchEraInfo): array(2) of two `[index, name]` arrays.
        Some(2) => {
            for _ in 0..2 {
                let inner = d.array().map_err(err("MismatchEraInfo"))?;
                if inner != Some(2) {
                    return Err("MismatchEraInfo: era entry is not a 2-element array".into());
                }
                d.u64().map_err(err("MismatchEraInfo era index"))?;
                d.str().map_err(err("MismatchEraInfo era name"))?;
            }
            ensure_end(&d, bytes)?;
            Ok(Wire::WrongEra)
        }
        // Right (OneEraApplyTxErr): array(1)[ [era, [failures]] ].
        Some(1) => {
            if d.array().map_err(err("era entry"))? != Some(2) {
                return Err("era entry is not a 2-element array".into());
            }
            let era = d.u64().map_err(err("era index"))?;
            if era != CONWAY_ERA_INDEX {
                return Err(format!(
                    "era index {era}, expected Conway ({CONWAY_ERA_INDEX})"
                ));
            }
            let count = d
                .array()
                .map_err(err("failure list"))?
                .ok_or("failure list is indefinite-length")?;
            let root = lookup(ROOT_RULE).ok_or("tag table has no root rule")?;
            let mut failures = Vec::new();
            for i in 0..count {
                failures.push(
                    decode_failure(&mut d, root, &[], &[])
                        .map_err(|e| format!("failure {i}: {e}"))?,
                );
            }
            ensure_end(&d, bytes)?;
            Ok(Wire::Failures(failures))
        }
        other => Err(format!("top-level array of {other:?} elements")),
    }
}

fn ensure_end(d: &Decoder<'_>, bytes: &[u8]) -> Result<(), String> {
    if d.position() == bytes.len() {
        Ok(())
    } else {
        Err(format!("{} trailing bytes", bytes.len() - d.position()))
    }
}

/// Decode one failure of `rule`, positioned at its array header.
fn decode_failure(
    d: &mut Decoder<'_>,
    rule_: &'static Rule,
    path: &[String],
    tags: &[u64],
) -> Result<Failure, String> {
    let start = d.position();
    let len = d
        .array()
        .map_err(err(rule_.name))?
        .ok_or(format!("{}: indefinite-length failure", rule_.name))?;
    if len == 0 {
        return Err(format!("{}: empty failure array", rule_.name));
    }
    let tag = d.u64().map_err(err(rule_.name))?;
    let mut tags = tags.to_vec();
    tags.push(tag);
    let Some(variant) = rule_.variants.iter().find(|v| v.tag == tag) else {
        for _ in 1..len {
            d.skip().map_err(err(rule_.name))?;
        }
        let leaf = format!("<unknown tag {tag} in {}>", rule_.name);
        let mut path = path.to_vec();
        path.push(leaf.clone());
        return Ok(Failure {
            path,
            rule: rule_.name.to_string(),
            leaf,
            tags,
            raw: d.input()[start..d.position()].to_vec(),
            unknown: true,
        });
    };
    match arity(variant.name) {
        Some(a) if a as u64 == len => {}
        Some(a) => {
            return Err(format!(
                "{}: array of {len} elements, the transcribed encoding has {a}",
                variant.name
            ))
        }
        None => return Err(format!("{}: no arity in the tag table", variant.name)),
    }
    let mut path = path.to_vec();
    path.push(variant.name.to_string());
    let Some(nested) = variant.nested else {
        for _ in 1..len {
            d.skip().map_err(err(variant.name))?;
        }
        return Ok(Failure {
            path,
            rule: rule_.name.to_string(),
            leaf: variant.name.to_string(),
            tags,
            raw: d.input()[start..d.position()].to_vec(),
            unknown: false,
        });
    };
    let nested_rule = lookup(nested).ok_or(format!(
        "tag table: {} wraps `{nested}`, which has no rule",
        variant.name
    ))?;
    if variant.nested_field == 0 || variant.nested_field as u64 >= len {
        return Err(format!(
            "{}: array of {len} has no element {}",
            variant.name, variant.nested_field
        ));
    }
    let mut found = None;
    for i in 1..len {
        if i == variant.nested_field as u64 {
            found = Some(decode_failure(d, nested_rule, &path, &tags)?);
        } else {
            d.skip().map_err(err(variant.name))?;
        }
    }
    found.ok_or(format!("{}: nested failure missing", variant.name))
}

/// A short human form: `Ledger/Utxow/Utxo/BadInputsUTxO [1,0,0,1]`.
pub fn describe(f: &Failure) -> String {
    format!("{} {:?}", f.path.join("/"), f.tags)
}
