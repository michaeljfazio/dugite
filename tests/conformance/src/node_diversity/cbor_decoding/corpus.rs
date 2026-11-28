//! The cardano-cbor-dataset on disk: `<era>/<rule>/<category>/<name>.input.cbor`, and for every
//! sample that must decode a `<name>.expected.cbor` beside it.
//!
//! Loading is strict.  A directory that is not a known category, a valid sample without an
//! expected file, an expected file without a sample, or any other file in a category directory is
//! a HARNESS failure, never a skipped sample: the site stops with an error on an unknown category,
//! and a silently dropped sample would raise the score.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::node_diversity::report::CborEra;

/// Whether a sample must decode or must be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// `valid`, `manual-valid`: must decode and re-encode to `.expected.cbor`.
    Decodes,
    /// `invalid-zap-<n>`, `manual-invalid`: must be refused.
    Refused,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub rule: String,
    pub category: String,
    /// `<name>.input.cbor`
    pub file: String,
    pub expectation: Expectation,
    pub input: Vec<u8>,
    /// Only for samples that must decode.
    pub expected: Option<Vec<u8>>,
}

impl Sample {
    /// The key of the sample in the report.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.rule, self.category, self.file)
    }
}

/// Every sample of one era, grouped by rule.
#[derive(Debug)]
pub struct Corpus {
    pub era: CborEra,
    pub root: PathBuf,
    pub rules: BTreeMap<String, Vec<Sample>>,
}

/// The expectation a category directory name carries, or None for an unknown category.
pub fn expectation_of(category: &str) -> Option<Expectation> {
    match category {
        "valid" | "manual-valid" => Some(Expectation::Decodes),
        "manual-invalid" => Some(Expectation::Refused),
        c if c
            .strip_prefix("invalid-zap-")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            Some(Expectation::Refused)
        }
        _ => None,
    }
}

/// Where the pinned dataset's samples of `era` are, under the fetched corpora directory.
pub fn dataset_dir(corpora: &Path, era: CborEra) -> PathBuf {
    corpora.join("cbor-dataset/dataset").join(era.id())
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

impl Corpus {
    pub fn load(corpora: &Path, era: CborEra) -> Result<Corpus, String> {
        let root = dataset_dir(corpora, era);
        let mut rules = BTreeMap::new();
        for rule_entry in read_dir_sorted(&root)? {
            let rule_path = rule_entry.path();
            let rule = rule_entry.file_name().to_string_lossy().into_owned();
            if rule_path.is_file() {
                // corpus.json (generation parameters) and coverage.json belong to the generator.
                if rule == "corpus.json" || rule == "coverage.json" {
                    continue;
                }
                return Err(format!("unexpected file {}", rule_path.display()));
            }
            let mut samples = Vec::new();
            for category_entry in read_dir_sorted(&rule_path)? {
                let category = category_entry.file_name().to_string_lossy().into_owned();
                let expectation = expectation_of(&category).ok_or_else(|| {
                    format!(
                        "{}: unknown category directory '{category}' (the site stops on it)",
                        rule_path.display()
                    )
                })?;
                samples.extend(load_category(
                    &rule,
                    &category,
                    expectation,
                    &category_entry.path(),
                )?);
            }
            rules.insert(rule, samples);
        }
        if rules.is_empty() {
            return Err(format!("no rules under {}", root.display()));
        }
        Ok(Corpus { era, root, rules })
    }

    pub fn sample_count(&self) -> usize {
        self.rules.values().map(Vec::len).sum()
    }
}

fn load_category(
    rule: &str,
    category: &str,
    expectation: Expectation,
    dir: &Path,
) -> Result<Vec<Sample>, String> {
    let mut samples = Vec::new();
    let mut expected_files = Vec::new();
    for entry in read_dir_sorted(dir)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".input.cbor") {
            let input = std::fs::read(entry.path())
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
            let expected = match expectation {
                Expectation::Decodes => {
                    let path = dir.join(format!("{stem}.expected.cbor"));
                    Some(std::fs::read(&path).map_err(|e| {
                        format!(
                            "valid sample {} has no readable expected file: {e}",
                            entry.path().display()
                        )
                    })?)
                }
                Expectation::Refused => None,
            };
            samples.push(Sample {
                rule: rule.to_string(),
                category: category.to_string(),
                file: name,
                expectation,
                input,
                expected,
            });
        } else if name.ends_with(".expected.cbor") {
            expected_files.push(name);
        } else {
            return Err(format!("unexpected file {}", entry.path().display()));
        }
    }
    for name in expected_files {
        let stem = name.strip_suffix(".expected.cbor").unwrap();
        let has_input = samples
            .iter()
            .any(|s| s.file.strip_suffix(".input.cbor") == Some(stem));
        if expectation == Expectation::Refused || !has_input {
            return Err(format!(
                "{}/{name}: an expected file without a sample that decodes",
                dir.display()
            ));
        }
    }
    Ok(samples)
}
