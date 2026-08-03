//! Guards that the fuzz harness actually runs what it declares.
//!
//! Issue #971: 11 of 53 declared fuzz targets were never in the nightly CI
//! matrix. They compiled, they were documented, they had thoughtful headers
//! explaining what security property each one protected — and none of them had
//! executed once in 2.5 months, including all four mini-protocol state
//! machines and the regression protection produced by security audit
//! #541-#547.
//!
//! Nothing detected it because nothing was watching the seam between
//! `fuzz/Cargo.toml` (which declares targets) and `.github/workflows/fuzz.yml`
//! (which runs them). Two files, no link. This test is the link.
//!
//! Issue #972 added a second seam of the same kind: seeds committed under a
//! path the fuzzer does not read, and seeds larger than `-max_len`, which
//! libFuzzer silently TRUNCATES rather than skipping. A 29 KB real block read
//! under a 4 KB cap is a fragment that can only reach the decoder's error
//! path — it looks like a seeded corpus and behaves like noise.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Targets deliberately absent from the CI matrix, with the reason.
///
/// Adding an entry here is the documented way to exclude a target. Removing a
/// target from the matrix without adding it here fails this test.
/// Empty, and that is the intended steady state.
///
/// The one entry that lived here was `plutus_script_decode`, excluded because
/// upstream Aiken's `uplc` panicked on malformed input. #970 deleted the
/// target outright: it fuzzed a third-party library dugite does not ship, so
/// it could never have found a dugite defect. `dugite_uplc_program_decode`
/// covers the decoder that actually runs in production.
const DOCUMENTED_EXCLUSIONS: &[(&str, &str)] = &[];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

/// Target names declared as `[[bin]]` entries in `fuzz/Cargo.toml`.
///
/// Returned without the `fuzz_` prefix, matching how the CI matrix names them.
fn declared_targets(root: &Path) -> BTreeSet<String> {
    let manifest =
        std::fs::read_to_string(root.join("fuzz/Cargo.toml")).expect("fuzz/Cargo.toml is readable");
    manifest
        .lines()
        .filter_map(|line| {
            let name = line.trim().strip_prefix("name = \"")?.strip_suffix('"')?;
            name.strip_prefix("fuzz_").map(str::to_owned)
        })
        .collect()
}

struct Workflow {
    targets: BTreeSet<String>,
    /// `matrix.include` overrides: target -> max_len.
    max_len_overrides: BTreeMap<String, usize>,
    /// The fallback in `-max_len=${{ matrix.max_len || N }}`.
    default_max_len: usize,
}

/// Line-oriented parse of the bits of `fuzz.yml` this test asserts on.
///
/// Deliberately not a YAML dependency: the surface is three known constructs in
/// a file this repo owns, and every extraction below asserts it found something
/// plausible, so a structural change fails loudly rather than silently matching
/// nothing — which is the exact failure mode this test exists to prevent.
fn parse_workflow(root: &Path) -> Workflow {
    let text = std::fs::read_to_string(root.join(".github/workflows/fuzz.yml"))
        .expect(".github/workflows/fuzz.yml is readable");

    let mut targets = BTreeSet::new();
    let mut max_len_overrides = BTreeMap::new();
    let mut default_max_len = None;

    #[derive(PartialEq)]
    enum Section {
        None,
        Targets,
        Include,
    }
    let mut section = Section::None;
    let mut pending_include: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed == "target:" {
            section = Section::Targets;
            continue;
        }
        if trimmed == "include:" {
            section = Section::Include;
            continue;
        }
        // `steps:` ends the matrix block.
        if trimmed == "steps:" {
            section = Section::None;
        }

        match section {
            Section::Targets => {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    // Bare scalar list items only; anything with a colon is a
                    // different construct and means the matrix shape changed.
                    if !item.contains(':') && !item.is_empty() {
                        targets.insert(item.to_owned());
                    }
                }
            }
            Section::Include => {
                if let Some(t) = trimmed.strip_prefix("- target: ") {
                    pending_include = Some(t.to_owned());
                } else if let Some(v) = trimmed.strip_prefix("max_len: ") {
                    let target = pending_include
                        .take()
                        .expect("max_len appears under a `- target:` entry");
                    let len = v.parse().expect("max_len is a number");
                    max_len_overrides.insert(target, len);
                }
            }
            Section::None => {}
        }

        if let Some(idx) = trimmed.find("matrix.max_len || ") {
            let rest = &trimmed[idx + "matrix.max_len || ".len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            default_max_len = Some(digits.parse().expect("default max_len is a number"));
        }
    }

    // Every extraction asserts it found something. A parser that silently
    // matches nothing would make this whole test vacuously pass — the "reports
    // success while measuring nothing" shape the fuzz audit kept finding.
    assert!(
        targets.len() > 40,
        "parsed only {} matrix targets from fuzz.yml — the matrix shape changed \
         and this test is no longer reading it",
        targets.len()
    );

    Workflow {
        targets,
        max_len_overrides,
        default_max_len: default_max_len.expect(
            "fuzz.yml passes -max_len=${{ matrix.max_len || N }}; this test reads N from it",
        ),
    }
}

/// Every declared target runs in CI, unless explicitly excluded here.
#[test]
fn every_declared_fuzz_target_is_in_the_ci_matrix() {
    let root = repo_root();
    let declared = declared_targets(&root);
    let wf = parse_workflow(&root);

    assert!(
        declared.len() >= 53,
        "expected at least 53 declared fuzz targets, found {} — did the \
         [[bin]] parse break?",
        declared.len()
    );

    let excluded: BTreeSet<&str> = DOCUMENTED_EXCLUSIONS.iter().map(|(t, _)| *t).collect();

    let missing: Vec<&String> = declared
        .iter()
        .filter(|t| !wf.targets.contains(*t) && !excluded.contains(t.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "these fuzz targets are declared in fuzz/Cargo.toml but never run in \
         .github/workflows/fuzz.yml: {missing:?}\n\n\
         A declared-but-unwired target is dead code that looks like coverage. \
         Either add it to matrix.target, or add it to DOCUMENTED_EXCLUSIONS in \
         this file with the reason."
    );
}

/// The matrix does not name targets that do not exist.
#[test]
fn every_ci_matrix_target_is_declared() {
    let root = repo_root();
    let declared = declared_targets(&root);
    let wf = parse_workflow(&root);

    let unknown: Vec<&String> = wf.targets.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "fuzz.yml runs targets with no [[bin]] in fuzz/Cargo.toml: {unknown:?} \
         — the nightly job would fail to build"
    );
}

/// Documented exclusions must be real targets, so the list cannot rot.
#[test]
fn documented_exclusions_still_exist() {
    let root = repo_root();
    let declared = declared_targets(&root);

    for (target, reason) in DOCUMENTED_EXCLUSIONS {
        assert!(
            declared.contains(*target),
            "DOCUMENTED_EXCLUSIONS names `{target}` ({reason}) but no such \
             fuzz target is declared — stale exclusion, remove it"
        );
    }
}

/// `matrix.include` entries must attach to a target the matrix actually runs.
#[test]
fn max_len_overrides_attach_to_real_targets() {
    let root = repo_root();
    let wf = parse_workflow(&root);

    for target in wf.max_len_overrides.keys() {
        assert!(
            wf.targets.contains(target),
            "fuzz.yml sets max_len for `{target}`, which is not in \
             matrix.target — the override applies to nothing"
        );
    }
}

/// Seed directories must name a real target, or the seeds are never loaded.
///
/// This is #972's original defect generalised: `fuzz/corpus/decode_block/` held
/// three real on-chain blocks that nothing read, because cargo-fuzz derives the
/// corpus path from the BIN name (`fuzz_decode_block`). A typo'd or renamed
/// seed directory fails silently and looks exactly like a seeded corpus.
#[test]
fn every_seed_directory_names_a_real_target() {
    let root = repo_root();
    let declared = declared_targets(&root);
    let seeds_dir = root.join("fuzz/seeds");

    let entries = std::fs::read_dir(&seeds_dir).expect("fuzz/seeds/ exists");
    let mut seeded = 0usize;

    for entry in entries {
        let entry = entry.expect("readable dir entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            declared.contains(&name),
            "fuzz/seeds/{name}/ does not match any declared fuzz target. \
             cargo-fuzz reads corpus/fuzz_<target>/, so these seeds would \
             never be loaded. Regenerate with scripts/dev/regen-fuzz-seeds.sh"
        );
        seeded += 1;
    }

    assert!(
        seeded > 0,
        "fuzz/seeds/ has no target directories — run scripts/dev/regen-fuzz-seeds.sh"
    );
}

/// No committed seed may exceed its target's `-max_len`.
///
/// libFuzzer TRUNCATES an oversized seed to `-max_len` rather than skipping it
/// (`FileToVector(Path, MaxSize)`), so an oversized real block silently
/// degrades into a prefix fragment that can only reach the decoder's error
/// path. The seed still shows up in the corpus count, which is what makes this
/// worth a test rather than a comment.
#[test]
fn no_committed_seed_exceeds_its_targets_max_len() {
    let root = repo_root();
    let wf = parse_workflow(&root);
    let seeds_dir = root.join("fuzz/seeds");

    let mut violations = Vec::new();

    for entry in std::fs::read_dir(&seeds_dir).expect("fuzz/seeds/ exists") {
        let entry = entry.expect("readable dir entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        let target = entry.file_name().to_string_lossy().into_owned();
        let cap = wf
            .max_len_overrides
            .get(&target)
            .copied()
            .unwrap_or(wf.default_max_len);

        for seed in std::fs::read_dir(entry.path()).expect("readable seed dir") {
            let seed = seed.expect("readable seed entry");
            let len = seed.metadata().expect("seed metadata").len() as usize;
            if len > cap {
                violations.push(format!(
                    "  fuzz/seeds/{target}/{} is {len} bytes but -max_len is {cap}",
                    seed.file_name().to_string_lossy()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "committed seeds exceed their target's -max_len and would be silently \
         truncated by libFuzzer:\n{}\n\nRaise max_len for the target via \
         matrix.include in .github/workflows/fuzz.yml, or shrink the seed.",
        violations.join("\n")
    );
}
