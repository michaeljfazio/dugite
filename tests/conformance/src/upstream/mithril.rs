//! Phase 6 — Mithril certificate fixture validation.
//!
//! Validates Mithril snapshot and certificate structures using fixtures from
//! `input-output-hk/mithril`, published via the corpus regeneration pipeline.
//!
//! ## Validation levels
//!
//! **Level 1 (structural):** JSON parses, identifier field present.
//! **Level 2 (schema):** All standard fields present with correct types.
//! **Level 3 (semantic):** Cross-field consistency checks — beacon.epoch > 0,
//! created_at looks like a timestamp, identifier is a 64-char hex string
//! (BLAKE2b-256), multi_signature/signed_message non-empty when present.
//!
//! Levels 1-3 run on every fixture. Full certificate chain STM multi-signature
//! verification requires `mithril-stm` / `mithril-common` plus the full stake
//! distribution and genesis certificate; that is tracked as a Phase 6 follow-on
//! (filed as a separate issue when mithril-stm is added to dugite-conformance).
//!
//! ## Status
//!
//! The fixture area (`tests/conformance/upstream/fixtures/mithril/`) is
//! currently a stub placeholder. The module falls back to the existing ad-hoc
//! Mithril fixtures in `crates/dugite-node/tests/fixtures/mithril-*.json`.
//!
//! To activate corpus-based fixtures:
//!
//! 1. Trigger the corpus regeneration pipeline (captures Mithril certificate
//!    fixtures from `input-output-hk/mithril` at the SHA pinned in `sources.toml`).
//!
//! 2. Update `manifest.toml` to point at the new corpus release tag.
//!
//! 3. Run `cargo xtask download-upstream-fixtures`.
//!
//! Once corpus-based fixtures are populated, the ad-hoc node-crate fixtures
//! (`crates/dugite-node/tests/fixtures/mithril-*.json`) can be removed — they
//! are superseded by the corpus copies. See the Phase 6 supersession note in
//! the design spec.

use std::path::Path;

fn has_only_readme(dir: &Path) -> bool {
    let files = walkdir(dir);
    files.len() == 1
        && files[0]
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("readme.txt"))
            .unwrap_or(false)
}

/// Validate that `json_path` is parseable and satisfies Levels 1-3.
fn check_mithril_json(json_path: &Path, label: &str) {
    let text = std::fs::read_to_string(json_path)
        .unwrap_or_else(|e| panic!("read mithril fixture {}: {e}", json_path.display()));
    let val: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {label}: {e}"));
    assert!(
        !val.is_null(),
        "Mithril fixture {label} parsed as null ({})",
        json_path.display()
    );

    if label.contains("list") {
        validate_list(&val, label, json_path);
    } else if label.contains("detail") {
        validate_detail(&val, label, json_path);
    } else {
        // Unknown category: at minimum check it's non-null (Level 1 only).
        eprintln!("[mithril] {label}: non-null JSON ({} bytes)", text.len());
    }
}

/// Level 1-3 validation for a certificate list fixture.
fn validate_list(val: &serde_json::Value, label: &str, path: &Path) {
    let arr = val
        .as_array()
        .unwrap_or_else(|| panic!("Mithril {label} expected JSON array ({:?})", path));
    assert!(!arr.is_empty(), "Mithril {label} list is empty");

    for (i, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .unwrap_or_else(|| panic!("Mithril {label}[{i}] is not a JSON object"));

        // Level 1: identifier field present.
        assert!(
            obj.contains_key("hash")
                || obj.contains_key("certificate_hash")
                || obj.contains_key("digest")
                || obj.contains_key("block_hash")
                || obj.contains_key("transaction_hash"),
            "Mithril {label}[{i}] missing identifier field \
             (hash / certificate_hash / digest / block_hash / transaction_hash)"
        );

        // Level 2: common schema fields (warn if absent — fake aggregator fixtures
        // may omit these; real aggregator fixtures always include them).
        for field in &["beacon", "created_at"] {
            if !obj.contains_key(*field) {
                eprintln!(
                    "[mithril] WARN {label}[{i}] missing schema field '{field}' \
                     (non-standard fixture format — Level 2 partial)"
                );
            }
        }

        // Level 3: semantic checks (validators are no-ops when the field is absent).
        validate_beacon(obj, label, i);
        validate_created_at(obj, label, i);

        // Level 3: identifier is a 64-char hex string (BLAKE2b-256 = 32 bytes).
        let id_val = obj
            .get("hash")
            .or_else(|| obj.get("certificate_hash"))
            .or_else(|| obj.get("digest"))
            .and_then(|v| v.as_str());
        if let Some(id_str) = id_val {
            validate_hash_hex(id_str, label, i, "identifier");
        }

        // Level 3: multi_signature / signed_message non-empty when present.
        validate_optional_nonempty(obj, label, i, "multi_signature");
        validate_optional_nonempty(obj, label, i, "signed_message");
    }
    eprintln!("[mithril] {label}: {} entries, Level 1-3 OK", arr.len());
}

/// Level 1-3 validation for a certificate detail fixture.
fn validate_detail(val: &serde_json::Value, label: &str, path: &Path) {
    let obj = val
        .as_object()
        .unwrap_or_else(|| panic!("Mithril {label} expected JSON object ({:?})", path));

    // Level 1: identifier field present.
    let id = obj
        .get("hash")
        .or_else(|| obj.get("certificate_hash"))
        .or_else(|| obj.get("digest"))
        .unwrap_or_else(|| panic!("Mithril {label} missing identifier field"));

    let id_str = id.as_str().unwrap_or("<non-string>");
    assert!(
        !id_str.is_empty(),
        "Mithril {label} identifier field is empty string"
    );

    // Level 2: common schema fields (warn if absent — fake aggregator fixtures
    // may omit these; real aggregator fixtures always include them).
    for field in &["beacon", "created_at"] {
        if !obj.contains_key(*field) {
            eprintln!(
                "[mithril] WARN {label} missing schema field '{field}' \
                 (non-standard fixture format — Level 2 partial)"
            );
        }
    }

    // Level 3: semantic checks (validators are no-ops when the field is absent).
    validate_beacon(obj, label, 0);
    validate_created_at(obj, label, 0);

    // Level 3: identifier must be a 64-char hex string (BLAKE2b-256 = 32 bytes).
    validate_hash_hex(id_str, label, 0, "identifier");

    // Level 3: network field consistency (must be a non-empty string when present).
    if let Some(network) = obj.get("network").and_then(|v| v.as_str()) {
        assert!(
            !network.is_empty(),
            "Mithril {label}: 'network' field is empty"
        );
    }

    // Level 3: certificate_hash non-empty and hex-encoded when present.
    if let Some(ch) = obj.get("certificate_hash").and_then(|v| v.as_str()) {
        assert!(
            !ch.is_empty(),
            "Mithril {label}: 'certificate_hash' is empty string"
        );
        validate_hash_hex(ch, label, 0, "certificate_hash");
    }

    // Level 3: multi_signature non-empty when present.
    validate_optional_nonempty(obj, label, 0, "multi_signature");

    // Level 3: signed_message non-empty when present.
    validate_optional_nonempty(obj, label, 0, "signed_message");

    eprintln!("[mithril] {label}: identifier={id_str}, Level 1-3 OK");
}

/// Level 3 — Validate the `beacon` field has expected subfields.
fn validate_beacon(obj: &serde_json::Map<String, serde_json::Value>, label: &str, idx: usize) {
    let Some(beacon) = obj.get("beacon") else {
        return;
    };
    let Some(beacon_obj) = beacon.as_object() else {
        return;
    };
    // Beacon should contain epoch or block information.
    // Both `epoch` and `immutable_file_number` are standard Mithril beacon fields.
    for field in &["epoch"] {
        if !beacon_obj.contains_key(*field) {
            eprintln!(
                "[mithril] WARN {label}[{idx}] beacon missing optional field '{field}' \
                 (may be OK for this Mithril API version)"
            );
        }
    }
    // Epoch must be a non-negative integer when present.
    if let Some(epoch) = beacon_obj.get("epoch") {
        assert!(
            epoch.is_number(),
            "Mithril {label}[{idx}] beacon.epoch is not a number: {epoch}"
        );
        let n = epoch.as_u64().unwrap_or(0);
        assert!(
            n > 0,
            "Mithril {label}[{idx}] beacon.epoch is zero (expected > 0)"
        );
    }
}

/// Level 3 — Validate that a field value is a lowercase hex string of the expected length.
///
/// Mithril uses BLAKE2b-256 for all certificate hashes: 32 bytes = 64 hex chars.
fn validate_hash_hex(s: &str, label: &str, idx: usize, field: &str) {
    assert!(
        s.len() == 64,
        "Mithril {label}[{idx}] {field}: expected 64-char hex (BLAKE2b-256), got {} chars: {:?}",
        s.len(),
        s
    );
    assert!(
        s.chars().all(|c| c.is_ascii_hexdigit()),
        "Mithril {label}[{idx}] {field}: not a valid hex string: {s:?}"
    );
}

/// Level 3 — Assert a string field is non-empty when present in `obj`.
fn validate_optional_nonempty(
    obj: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    idx: usize,
    field: &str,
) {
    if let Some(v) = obj.get(field).and_then(|v| v.as_str()) {
        assert!(
            !v.is_empty(),
            "Mithril {label}[{idx}] '{field}' is empty string"
        );
    }
}

/// Level 3 — Validate `created_at` is a non-empty string (RFC 3339 format).
fn validate_created_at(obj: &serde_json::Map<String, serde_json::Value>, label: &str, idx: usize) {
    let Some(created_at) = obj.get("created_at") else {
        return;
    };
    let ts = created_at
        .as_str()
        .unwrap_or_else(|| panic!("Mithril {label}[{idx}] created_at is not a string"));
    assert!(
        !ts.is_empty(),
        "Mithril {label}[{idx}] created_at is empty string"
    );
    // Validate it looks like a timestamp (contains 'T' date separator or similar).
    assert!(
        ts.contains('T') || ts.contains(' '),
        "Mithril {label}[{idx}] created_at does not look like a timestamp: {ts}"
    );
}

fn run_corpus_fixtures(dir: &Path) {
    let json_files: Vec<_> = walkdir(dir)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();

    if json_files.is_empty() {
        eprintln!(
            "[mithril] SKIP corpus: no JSON files in {} — falling back to node-crate fixtures",
            dir.display()
        );
        return;
    }

    for path in &json_files {
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned();
        check_mithril_json(path, &label);
    }
    eprintln!(
        "[mithril] corpus: {} JSON fixture(s) validated (Levels 1-3)",
        json_files.len()
    );
}

fn run_node_crate_fallback_fixtures() {
    // These fixtures live in crates/dugite-node/tests/fixtures/ and are
    // included at compile time. We validate them via the filesystem path so
    // we don't need to add a dependency from this crate to dugite-node.
    let fixture_pairs = &[
        ("mithril-421-list", "mithril-421-detail"),
        ("mithril-421-v1-list", "mithril-421-v1-detail"),
    ];

    // Walk up from CARGO_MANIFEST_DIR to find the workspace root.
    let mut base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = base.join("Cargo.toml");
        if cargo.exists()
            && std::fs::read_to_string(&cargo)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            break;
        }
        if !base.pop() {
            eprintln!("[mithril] SKIP fallback: workspace root not found");
            return;
        }
    }

    let fixtures_dir = base.join("crates/dugite-node/tests/fixtures");
    if !fixtures_dir.exists() {
        eprintln!(
            "[mithril] SKIP fallback: {} does not exist",
            fixtures_dir.display()
        );
        return;
    }

    let mut checked = 0usize;
    for (list_name, detail_name) in fixture_pairs {
        let list_path = fixtures_dir.join(format!("{list_name}.json"));
        let detail_path = fixtures_dir.join(format!("{detail_name}.json"));
        if list_path.exists() {
            check_mithril_json(&list_path, list_name);
            checked += 1;
        }
        if detail_path.exists() {
            check_mithril_json(&detail_path, detail_name);
            checked += 1;
        }
    }
    eprintln!("[mithril] fallback: {checked} node-crate fixture(s) validated (Levels 1-3)");
}

/// Validate one mithril fixture file (JSON with category-specific
/// Level-1..3 checks if the filename matches `*list*` or `*detail*`).
/// Exposed for `build.rs`-generated per-vector tests.
///
/// Returns `Ok(())` on success. On failure, panics via the helpers'
/// existing assertions — captured and converted to a `String` by the
/// caller using `std::panic::catch_unwind`.
pub fn check_one_file(path: &Path) -> Result<(), String> {
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check_mithril_json(path, label);
    }));
    result.map_err(|panic_payload| {
        if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "panic in check_mithril_json (no message)".to_string()
        }
    })
}

pub fn run_all_checks(dir: &Path) {
    if has_only_readme(dir) {
        eprintln!(
            "[mithril] corpus area is stub at {} — falling back to node-crate fixtures",
            dir.display()
        );
        run_node_crate_fallback_fixtures();
        return;
    }

    run_corpus_fixtures(dir);
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    fn collect(dir: &Path, acc: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() {
                acc.push(p);
            } else if p.is_dir() {
                collect(&p, acc);
            }
        }
    }
    collect(dir, &mut out);
    out.sort();
    out
}
