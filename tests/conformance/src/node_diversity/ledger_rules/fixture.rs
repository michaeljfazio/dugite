//! Loading the `ledger-rules` fixtures from the prepared corpus tree.
//!
//! `scripts/node-diversity/fetch-corpora.sh` assembles `<corpora>/ledger-rules/` (NDC's fixtures,
//! plus the three protocol-parameter files NDC does not ship).  A fixture refers to shared
//! documents as `common/<dir>/<file>`; NDC drops the `common/` prefix, so a reference resolves to
//! `<root>/<dir>/<file>`.  A `$override` is shallow-merged over the referenced document.
//!
//! The loader is strict: a top-level key it does not know is an error, so a field the scenario
//! adds later cannot be silently ignored.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::node_diversity::pins::corpora_dir;

/// What the fixture says must happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    Pass,
    /// A named predicate failure, with the narrowing `description` some predicates carry.
    Predicate {
        name: String,
        description: Option<String>,
    },
    DecodingFailure,
}

impl Expected {
    /// The string the report carries for this expectation (`pass`, a predicate name,
    /// `DecodingFailure`).
    pub fn result_name(&self) -> &str {
        match self {
            Expected::Pass => "pass",
            Expected::Predicate { name, .. } => name,
            Expected::DecodingFailure => "DecodingFailure",
        }
    }
}

impl Expected {
    /// What the gate compares: the result name, narrowed by the `description` of a predicate that
    /// has one (`ValidationTagMismatch(PassedUnexpectedly)`).
    pub fn gate_name(&self) -> String {
        match self {
            Expected::Predicate {
                name,
                description: Some(d),
            } => format!("{name}({d})"),
            other => other.result_name().to_string(),
        }
    }
}

/// One fixture, with every `$ref` resolved.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// The first five digits of the file name.
    pub id: String,
    pub file_name: String,
    pub title: String,
    pub network: String,
    pub era_history: Value,
    pub protocol_parameters: Value,
    /// The `initial_state` object, section by section.
    pub initial_state: Map<String, Value>,
    pub slot: u64,
    pub transaction_index: u64,
    pub transaction: Vec<u8>,
    pub expected: Expected,
}

/// The prepared `ledger-rules` tree.
pub fn corpus_root() -> PathBuf {
    corpora_dir().join("ledger-rules")
}

const TOP_LEVEL_KEYS: &[&str] = &[
    "title",
    "description",
    "category",
    "network",
    "era_history",
    "protocol_parameters",
    "initial_state",
    "point",
    "transaction",
    "expected",
];

/// Every fixture under `root/scenarios/`, ordered by id.
pub fn load_all(root: &Path) -> Result<Vec<Fixture>, String> {
    let dir = root.join("scenarios");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let fixtures = files
        .iter()
        .map(|f| load(root, f))
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids: Vec<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();
    ids.dedup();
    if ids.len() != fixtures.len() {
        return Err("two fixtures share an id".into());
    }
    Ok(fixtures)
}

fn load(root: &Path, file: &Path) -> Result<Fixture, String> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("non-utf8 file name")?
        .to_string();
    let id = name[..name.find('-').unwrap_or(name.len())].to_string();
    if id.len() != 5 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{name}: the file name must start with five digits"));
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("{name}: {e}"))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))?;
    let obj = doc.as_object().ok_or(format!("{name}: not an object"))?;
    for key in obj.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            return Err(format!("{name}: unknown top-level key `{key}`"));
        }
    }
    let get = |k: &str| obj.get(k).ok_or(format!("{name}: missing `{k}`"));
    let string = |k: &str| -> Result<String, String> {
        get(k)?
            .as_str()
            .map(str::to_string)
            .ok_or(format!("{name}: `{k}` is not a string"))
    };
    let point = get("point")?;
    let uint = |v: &Value, k: &str| -> Result<u64, String> {
        v.get(k)
            .and_then(Value::as_u64)
            .ok_or(format!("{name}: point.{k} is not an unsigned integer"))
    };
    let initial_state = get("initial_state")?
        .as_object()
        .ok_or(format!("{name}: initial_state is not an object"))?
        .clone();
    Ok(Fixture {
        id,
        title: obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        network: string("network")?,
        era_history: resolve(root, get("era_history")?).map_err(|e| format!("{name}: {e}"))?,
        protocol_parameters: resolve(root, get("protocol_parameters")?)
            .map_err(|e| format!("{name}: {e}"))?,
        initial_state,
        slot: uint(point, "slot")?,
        transaction_index: uint(point, "transaction_index")?,
        transaction: hex::decode(string("transaction")?)
            .map_err(|e| format!("{name}: transaction is not hex: {e}"))?,
        expected: parse_expected(get("expected")?).map_err(|e| format!("{name}: {e}"))?,
        file_name: name,
    })
}

fn parse_expected(v: &Value) -> Result<Expected, String> {
    if v.as_str() == Some("Pass") {
        return Ok(Expected::Pass);
    }
    let obj = v
        .as_object()
        .ok_or("`expected` is neither \"Pass\" nor an object")?;
    if obj.get("decoding_failure") == Some(&Value::Bool(true)) && obj.len() == 1 {
        return Ok(Expected::DecodingFailure);
    }
    for key in obj.keys() {
        if key != "predicate" && key != "description" {
            return Err(format!("unknown `expected` key `{key}`"));
        }
    }
    let name = obj
        .get("predicate")
        .and_then(Value::as_str)
        .ok_or("`expected.predicate` missing")?
        .to_string();
    let description = obj
        .get("description")
        .map(|d| {
            d.as_str()
                .map(str::to_string)
                .ok_or("`expected.description` is not a string")
        })
        .transpose()?;
    Ok(Expected::Predicate { name, description })
}

/// Resolve a `{ "$ref": "common/..", "$override": {..} }` document, or return an inline one.
fn resolve(root: &Path, v: &Value) -> Result<Value, String> {
    let Some(obj) = v.as_object() else {
        return Err("a referenced document must be an object".into());
    };
    let Some(reference) = obj.get("$ref") else {
        return Ok(v.clone());
    };
    let reference = reference.as_str().ok_or("`$ref` is not a string")?;
    let rest = reference
        .strip_prefix("common/")
        .ok_or(format!("`$ref` {reference} does not start with common/"))?;
    if rest.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(format!("`$ref` {reference} escapes the corpus"));
    }
    let path = root.join(rest);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut doc: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(over) = obj.get("$override") {
        let over = over.as_object().ok_or("`$override` is not an object")?;
        let target = doc
            .as_object_mut()
            .ok_or("a referenced document must be an object")?;
        for (k, val) in over {
            if !target.contains_key(k) {
                return Err(format!("`$override` key `{k}` is not in {reference}"));
            }
            target.insert(k.clone(), val.clone());
        }
    }
    for key in obj.keys() {
        if key != "$ref" && key != "$override" {
            return Err(format!("unknown key `{key}` next to `$ref`"));
        }
    }
    Ok(doc)
}
