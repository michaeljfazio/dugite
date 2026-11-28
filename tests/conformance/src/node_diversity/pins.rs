//! The pin file, `tests/conformance/node-diversity/corpora.toml`.
//!
//! `scripts/node-diversity/pins.py` reads the same file for the fetch script and applies the same
//! shape checks, so neither reader can accept a pin the other rejects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Every pinned corpus of the node-diversity harness.
#[derive(Debug, Clone, Deserialize)]
pub struct Pins {
    pub ndc: GitPin,
    pub amaru: AmaruPin,
    pub ledger_rules: LedgerRulesPins,
    pub cbor_dataset: GitPin,
    pub uplc_conformance: TarballPin,
    pub uplc_turbo: TarballPin,
}

/// A git repository pinned by commit.
#[derive(Debug, Clone, Deserialize)]
pub struct GitPin {
    pub repository: String,
    pub commit: String,
}

/// The Amaru checkout the ledger-rules fixtures are verified against.
#[derive(Debug, Clone, Deserialize)]
pub struct AmaruPin {
    pub repository: String,
    /// The commit that last changed [`Self::scenarios_dir`]; named by the report as `dataset_commit`.
    pub dataset_commit: String,
    pub scenarios_dir: String,
    pub common_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LedgerRulesPins {
    #[serde(default)]
    pub missing_protocol_parameters: Vec<MissingParameterFile>,
    /// Fixture id -> the top-level keys in which NDC intentionally differs from Amaru.
    #[serde(default)]
    pub known_divergences: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MissingParameterFile {
    pub file: String,
    pub sha256: String,
    pub fixtures: u32,
}

/// A tarball pinned by URL and SHA-256.
#[derive(Debug, Clone, Deserialize)]
pub struct TarballPin {
    pub url: String,
    pub sha256: String,
}

/// The workspace root, from this crate's manifest directory (`tests/conformance`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/conformance sits two levels below the workspace root")
        .to_path_buf()
}

/// The pin file of the repository.
pub fn default_pins_path() -> PathBuf {
    repo_root().join("tests/conformance/node-diversity/corpora.toml")
}

/// Where `fetch-corpora.sh` puts the corpora: `$DUGITE_NDC_DIR`, or `target/node-diversity/corpora`.
pub fn corpora_dir() -> PathBuf {
    std::env::var_os("DUGITE_NDC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/node-diversity/corpora"))
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Pins {
    /// Parse and check a pin file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read pins {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// The repository's own pin file.
    pub fn load_default() -> Result<Self, String> {
        Self::load(&default_pins_path())
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let pins: Pins = toml::from_str(text).map_err(|e| format!("malformed pins: {e}"))?;
        pins.check()?;
        Ok(pins)
    }

    fn check(&self) -> Result<(), String> {
        let commit = |what: &str, value: &str| {
            if is_hex(value, 40) {
                Ok(())
            } else {
                Err(format!(
                    "{what}: {value:?} is not a full lowercase 40-hex commit"
                ))
            }
        };
        let digest = |what: &str, value: &str| {
            if is_hex(value, 64) {
                Ok(())
            } else {
                Err(format!(
                    "{what}: {value:?} is not a lowercase 64-hex SHA-256"
                ))
            }
        };
        commit("ndc.commit", &self.ndc.commit)?;
        commit("amaru.dataset_commit", &self.amaru.dataset_commit)?;
        commit("cbor_dataset.commit", &self.cbor_dataset.commit)?;
        digest("uplc_conformance.sha256", &self.uplc_conformance.sha256)?;
        digest("uplc_turbo.sha256", &self.uplc_turbo.sha256)?;
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.ledger_rules.missing_protocol_parameters {
            digest(
                &format!("missing_protocol_parameters {}", entry.file),
                &entry.sha256,
            )?;
            if entry.file.contains('/') || entry.file.starts_with('.') {
                return Err(format!("{:?} is not a bare file name", entry.file));
            }
            if !seen.insert(&entry.file) {
                return Err(format!("duplicate pinned file {}", entry.file));
            }
        }
        for (id, keys) in &self.ledger_rules.known_divergences {
            if id.len() != 5 || !id.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("known_divergences id {id:?} is not five digits"));
            }
            if keys.is_empty() {
                return Err(format!("known_divergences {id} lists no keys"));
            }
        }
        for (what, url) in [
            ("ndc.repository", &self.ndc.repository),
            ("amaru.repository", &self.amaru.repository),
            ("cbor_dataset.repository", &self.cbor_dataset.repository),
            ("uplc_conformance.url", &self.uplc_conformance.url),
            ("uplc_turbo.url", &self.uplc_turbo.url),
        ] {
            if !url.starts_with("https://") {
                return Err(format!("{what}: {url:?} is not an https URL"));
            }
        }
        Ok(())
    }

    /// The `dataset_commit` a ledger-rules report must name.
    pub fn ledger_rules_dataset_commit(&self) -> &str {
        &self.amaru.dataset_commit
    }

    /// The `dataset_commit` a cbor-decoding report must name.
    pub fn cbor_dataset_commit(&self) -> &str {
        &self.cbor_dataset.commit
    }

    /// The number of fixtures that reference a protocol-parameter file NDC does not ship.
    pub fn fixtures_using_missing_parameters(&self) -> u32 {
        self.ledger_rules
            .missing_protocol_parameters
            .iter()
            .map(|e| e.fixtures)
            .sum()
    }
}
