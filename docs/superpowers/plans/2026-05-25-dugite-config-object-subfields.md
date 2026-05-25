# dugite-config Object Sub-field Editing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace raw-JSON editing of Object config params (`AcceptedConnectionsLimit`, `Rpc`, `Storage`) with schema-driven sub-field rows in dugite-config's tree, so operators edit typed leaves with validation/defaults/diff like every other parameter.

**Architecture:** Extend `ParamType::Object` to carry a recursive sub-schema (`SubParamDef`). On load, hydrate every schema-known sub-field with its default into the entry's `serde_json::Value`, recording the path as "synthetic". On save, walk the synthetic paths and prune any leaf still equal to its default, cascading the prune up empty sub-objects. The view-model gains path-addressed `Item` rows with depth/container flags; cursor movement skips rows whose ancestor container is collapsed. Edits at sub-leaves route through `Value::pointer_mut`. Unknown sub-keys round-trip verbatim through the raw-JSON edit path.

**Tech Stack:** Rust 2021, `serde_json` (already in `dugite-config`), `ratatui` (rendering), `tempfile` (tests), `anyhow` (errors). Existing `dugite-config` crate at `crates/dugite-config/`. Spec: `docs/superpowers/specs/2026-05-25-dugite-config-object-subfields-design.md`.

---

## Background — files this plan touches

All file paths are relative to repo root (`/Users/michaelfazio/Source/dugite`).

- `crates/dugite-config/src/schema.rs` — schema types and the KNOWN_PARAMS table
- `crates/dugite-config/src/config.rs` — `ConfigEntry`, `LoadedConfig`, hydration, save
- `crates/dugite-config/src/app.rs` — view-model: `App`, `Section`, `Item`, edit dispatch
- `crates/dugite-config/src/diff.rs` — diff snapshot + computation
- `crates/dugite-config/src/ui.rs` — ratatui rendering of rows
- `crates/dugite-config/src/search.rs` — fuzzy filter over items
- `crates/dugite-config/tests/config_coverage.rs` — invariants across schema
- New: `crates/dugite-config/src/path.rs` — JSON-pointer helpers

## Quick-reference commands

- Single-test run: `cargo nextest run -p dugite-config -E 'test(<name>)'`
- Crate test sweep: `cargo nextest run -p dugite-config`
- Workspace lint gate: `cargo clippy -p dugite-config --all-targets -- -D warnings`
- Format check: `cargo fmt -- --check`
- TUI smoke test (manual): `cargo run -p dugite-config -- --config config/preview/config.json`

The pre-commit hook in `crates/dugite-config/`'s parent repo refuses commits that span more than two crates; every commit in this plan stays inside `crates/dugite-config/` plus the optional `docs/` change in Task 17. Set `DUGITE_PRECOMMIT_STRICT=1` if you want enforcement.

---

### Task 1: Add SubParamDef + recursive ParamType::Object { fields }

**Files:**
- Modify: `crates/dugite-config/src/schema.rs:42-122` (`ParamType` enum), `crates/dugite-config/src/schema.rs:148-203` (`ParamDef` + `default_as_json`)
- Test: `crates/dugite-config/src/schema.rs` (tests module at bottom of file)

This task introduces the new `SubParamDef` struct and changes `ParamType::Object` from a unit variant to a struct variant carrying `fields: &'static [SubParamDef]`. All existing `ParamType::Object` usages in `KNOWN_PARAMS` (currently 3: `AcceptedConnectionsLimit`, `Rpc`, `Storage`) move to `ParamType::Object { fields: &[] }` — sub-schemas are populated in Tasks 14–16. `default_as_json` for Object becomes a recursive synthesiser of `Value::Object` from `fields`.

- [ ] **Step 1: Write the failing test**

Add to `crates/dugite-config/src/schema.rs` at the bottom of `mod tests`:

```rust
#[test]
fn test_subparam_default_as_json_recurses() {
    // Build a nested sub-schema by hand: outer { "x": u64=1, "inner": object { "y": bool=true } }.
    const INNER: &[SubParamDef] = &[SubParamDef {
        key: "y",
        param_type: ParamType::Bool,
        default: "true",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];
    const OUTER: &[SubParamDef] = &[
        SubParamDef {
            key: "x",
            param_type: ParamType::U64 { min: 0, max: 10 },
            default: "1",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
        SubParamDef {
            key: "inner",
            param_type: ParamType::Object { fields: INNER },
            default: "",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
    ];

    let outer = ParamDef {
        key: "Outer",
        section: "Test",
        param_type: ParamType::Object { fields: OUTER },
        default: "",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    };

    let v = outer.default_as_json().expect("object default");
    let obj = v.as_object().expect("object");
    assert_eq!(obj["x"], serde_json::json!(1));
    assert_eq!(obj["inner"], serde_json::json!({ "y": true }));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_subparam_default_as_json_recurses)'`
Expected: compile error — `SubParamDef` not defined and `ParamType::Object` doesn't accept `{ fields }`.

- [ ] **Step 3: Implement SubParamDef and the new ParamType::Object shape**

Edit `crates/dugite-config/src/schema.rs:42-72`. Replace the `ParamType` enum:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    Bool,
    U64 { min: u64, max: u64 },
    F64 { min: f64, max: f64 },
    String,
    Enum { values: &'static [&'static str] },
    Path,
    /// A nested JSON object with a fixed sub-schema. Empty `fields` means the
    /// schema does not yet model any sub-key (object is treated as opaque and
    /// edited as a single read-only row, same as before this feature).
    Object { fields: &'static [SubParamDef] },
}
```

Add a new struct above `ParamDef` (around line 148):

```rust
// ---------------------------------------------------------------------------
// Sub-parameter definition (for fields inside an Object param)
// ---------------------------------------------------------------------------

/// A single field inside a [`ParamType::Object`].
///
/// Identical in shape to [`ParamDef`] except for the missing `section` field —
/// sub-fields live under their parent's section.
///
/// A sub-field's `default` follows the same rules as `ParamDef::default`. An
/// empty string for a numeric / Bool / Enum leaf signals "no schema default"
/// (the leaf is not hydrated and only appears in the tree if present in the
/// on-disk file). For String / Path leaves, an empty default is a valid empty
/// string.
#[derive(Debug, Clone)]
pub struct SubParamDef {
    pub key: &'static str,
    pub param_type: ParamType,
    pub default: &'static str,
    pub description: &'static str,
    pub tuning_hint: &'static str,
    pub reloadability: Reloadability,
}

impl SubParamDef {
    /// Parse the sub-field's default into a JSON value matching its type, or
    /// `None` if the default string cannot be parsed (numeric / Bool / Enum
    /// with `default: ""`).
    pub fn default_as_json(&self) -> Option<Value> {
        match &self.param_type {
            ParamType::Bool => self.default.parse::<bool>().ok().map(Value::Bool),
            ParamType::U64 { .. } => self
                .default
                .parse::<u64>()
                .ok()
                .map(|n| Value::Number(serde_json::Number::from(n))),
            ParamType::F64 { .. } => self
                .default
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            ParamType::String | ParamType::Path | ParamType::Enum { .. } => {
                Some(Value::String(self.default.to_string()))
            }
            ParamType::Object { fields } => Some(object_default(fields)),
        }
    }
}

/// Recursively synthesise a `Value::Object` from a list of sub-field defaults.
/// Leaves with no parseable default (e.g. numeric with `default: ""`) are
/// omitted from the result.
pub(crate) fn object_default(fields: &[SubParamDef]) -> Value {
    let mut map = serde_json::Map::new();
    for sub in fields {
        if let Some(v) = sub.default_as_json() {
            map.insert(sub.key.to_string(), v);
        }
    }
    Value::Object(map)
}
```

Update `ParamType::label` (around line 63) to handle the new variant:

```rust
ParamType::Object { .. } => "object",
```

Update `ParamType::validate` (around line 119):

```rust
ParamType::Object { .. } => Ok(()),
```

Update `ParamDef::default_as_json` for `Object` (around line 200) to delegate:

```rust
ParamType::Object { fields } => Some(object_default(fields)),
```

Update **every** `ParamType::Object` literal in `KNOWN_PARAMS` (currently at `schema.rs:918`, `schema.rs:940`, `schema.rs:967`) to:

```rust
param_type: ParamType::Object { fields: &[] },
```

Also remove the now-unused `default: r#"{...}"#` JSON strings — for Object entries `default_as_json` no longer parses them. Replace with `default: ""` on the three Object `ParamDef`s. The TUI will display "{}" for an all-empty-fields object, which matches behavior; once Tasks 14-16 populate `fields`, defaults render properly. Update the existing test at `schema.rs:1471-1474` that checks `AcceptedConnectionsLimit` exists in `KNOWN_PARAMS` — it should still pass; just verify.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_subparam_default_as_json_recurses)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite to verify nothing else broke**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass. Pre-existing tests around `AcceptedConnectionsLimit` / `Rpc` / `Storage` defaults may need their assertions adjusted — if a test asserts the object default value, change the expected to `serde_json::json!({})` (empty object — fields are filled in later tasks). Fix any breakage inline before continuing.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/schema.rs
git commit -m "feat(dugite-config): SubParamDef + recursive ParamType::Object { fields }"
```

---

### Task 2: JSON-pointer helpers (new path.rs module)

**Files:**
- Create: `crates/dugite-config/src/path.rs`
- Modify: `crates/dugite-config/src/lib.rs` (or wherever modules are declared — search for the `mod schema;` line)

This task introduces helpers that translate `Vec<String>` paths into RFC 6901 JSON Pointers (`/Tls/CertPath` and friends), with proper `/`-and-`~` escaping. Used by every later task that touches sub-fields.

- [ ] **Step 1: Locate the module-declaration site**

Run: `grep -n "^mod " crates/dugite-config/src/main.rs crates/dugite-config/src/lib.rs 2>/dev/null`
Expected: a list of `mod ...;` lines. Note the file where modules are declared (most likely `main.rs` since dugite-config is a binary crate — verify and use it).

- [ ] **Step 2: Write the failing test**

Create `crates/dugite-config/src/path.rs` with only the tests at first:

```rust
//! JSON-pointer helpers for addressing sub-fields inside Object entries.
//!
//! See RFC 6901. The two characters that need escaping inside a path segment
//! are `~` (becomes `~0`) and `/` (becomes `~1`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_path_is_empty_pointer() {
        let p: Vec<String> = vec![];
        assert_eq!(path_to_json_pointer(&p), "");
    }

    #[test]
    fn test_single_segment() {
        let p = vec!["Tls".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/Tls");
    }

    #[test]
    fn test_two_segments() {
        let p = vec!["Tls".to_string(), "CertPath".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/Tls/CertPath");
    }

    #[test]
    fn test_escapes_tilde_and_slash() {
        let p = vec!["weird~key/with-slash".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/weird~0key~1with-slash");
    }

    #[test]
    fn test_escape_order_tilde_before_slash() {
        // Encoding must replace ~ FIRST, then /, otherwise an input "/" becomes
        // "~01" which decodes back to "~1" not "/".
        let p = vec!["~/".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/~0~1");
    }
}
```

Add the module to the declaration site (e.g. `main.rs`):

```rust
mod path;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -p dugite-config -E 'test(path::tests)'`
Expected: compile error — `path_to_json_pointer` not defined.

- [ ] **Step 4: Implement the helper**

Add to `crates/dugite-config/src/path.rs` (above the test module):

```rust
/// Translate a `Vec<String>` path into an RFC 6901 JSON Pointer.
///
/// - An empty path yields `""` (the whole document).
/// - Otherwise each segment is escaped (`~` → `~0`, `/` → `~1`) and prefixed
///   with `/`.
pub fn path_to_json_pointer(path: &[String]) -> String {
    let mut out = String::new();
    for seg in path {
        out.push('/');
        out.push_str(&escape_segment(seg));
    }
    out
}

fn escape_segment(seg: &str) -> String {
    // Replace `~` first, otherwise `/` → `~1` would in turn be re-encoded.
    seg.replace('~', "~0").replace('/', "~1")
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo nextest run -p dugite-config -E 'test(path::tests)'`
Expected: all four PASS.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/path.rs crates/dugite-config/src/main.rs
git commit -m "feat(dugite-config): JSON-pointer helpers for sub-field paths"
```

---

### Task 3: ConfigEntry::synthetic_paths field

**Files:**
- Modify: `crates/dugite-config/src/config.rs:38-52` (`ConfigEntry`), `crates/dugite-config/src/config.rs:174-189` (`inject_schema_defaults`), `crates/dugite-config/src/config.rs:217-225` (`load_config`)
- Modify: `crates/dugite-config/src/diff.rs:96-103` (test helper), `crates/dugite-config/src/app.rs` (any inline `ConfigEntry { ... }` literal)
- Test: `crates/dugite-config/src/config.rs` (tests module)

Add the `synthetic_paths: HashSet<String>` field to `ConfigEntry`. Default to empty everywhere; no behavioral change in this task — the field is plumbed but unused. This isolates the structural churn from the behavior change in Task 5.

- [ ] **Step 1: Write the failing test**

Add to the tests module of `crates/dugite-config/src/config.rs`:

```rust
#[test]
fn test_config_entry_has_empty_synthetic_paths_by_default() {
    let f = write_temp(r#"{"EnableP2P": true}"#);
    let config = load_config(f.path()).unwrap();
    assert!(config.entries[0].synthetic_paths.is_empty());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_config_entry_has_empty_synthetic_paths_by_default)'`
Expected: compile error — `synthetic_paths` field does not exist.

- [ ] **Step 3: Add the field and plumb it through**

Edit `crates/dugite-config/src/config.rs:38-52` — replace the `ConfigEntry` struct:

```rust
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub key: String,
    pub value: Value,
    pub modified: bool,
    pub present_in_file: bool,
    /// For Object entries: JSON-pointer paths (e.g. "/Tls/CertPath") that were
    /// synthesised during `inject_schema_defaults`. Empty for non-Object
    /// entries and for sub-keys that were present in the on-disk file.
    /// Used by `save_config` to decide which synthetic leaves to prune.
    pub synthetic_paths: std::collections::HashSet<String>,
}
```

Update `load_config` (around line 217-225):

```rust
let entries = obj
    .iter()
    .map(|(k, v)| ConfigEntry {
        key: k.clone(),
        value: v.clone(),
        modified: false,
        present_in_file: true,
        synthetic_paths: std::collections::HashSet::new(),
    })
    .collect();
```

Update `inject_schema_defaults` (around line 181-186):

```rust
self.entries.push(ConfigEntry {
    key: def.key.to_string(),
    value,
    modified: false,
    present_in_file: false,
    synthetic_paths: std::collections::HashSet::new(),
});
```

Now run a search to find every other `ConfigEntry { ... }` literal that needs updating:

```bash
grep -rn "ConfigEntry {" crates/dugite-config/
```

Update each — typical occurrences:
- `crates/dugite-config/src/diff.rs:96-103` (the `make_entry` test helper) — add `synthetic_paths: std::collections::HashSet::new(),`
- Any other tests that construct `ConfigEntry` literally.

- [ ] **Step 4: Run the failing test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_config_entry_has_empty_synthetic_paths_by_default)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass. Fix any literal-construction sites the grep missed.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/config.rs crates/dugite-config/src/diff.rs
# add any other paths your grep turned up:
git status --short  # double-check
git commit -m "feat(dugite-config): plumb ConfigEntry::synthetic_paths"
```

---

### Task 4: Recursive inject_schema_defaults for Object entries

**Files:**
- Modify: `crates/dugite-config/src/config.rs:166-189` (`inject_schema_defaults`)
- Test: same file's tests module

After Task 3 the field exists but is always empty. This task fills it: for each `ParamType::Object { fields }` entry, recursively walk `fields` and inject any missing leaf into the entry's `Value::Object`, recording each inserted path in `synthetic_paths`.

Must be idempotent: re-running on an already-hydrated entry adds no new paths.

- [ ] **Step 1: Write the failing test**

Add to `crates/dugite-config/src/config.rs` tests module:

```rust
#[test]
fn test_inject_hydrates_object_subfields_when_object_empty() {
    // Construct a config that has Rpc: {} on disk. After hydration the entry
    // must contain every leaf the schema defines with its default value, and
    // synthetic_paths must list each leaf path.
    //
    // NOTE: this test depends on the Rpc sub-schema existing. Until Task 15
    // populates Rpc::fields, this test must run AFTER manually adding a
    // stub sub-schema to a test-only ParamDef. We avoid that by using
    // AcceptedConnectionsLimit's schema once Task 14 is done — sequence this
    // test to run when its real schema exists.
    //
    // For Task 4 we instead test the recursion machinery via a synthetic
    // helper that doesn't rely on KNOWN_PARAMS:
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    const FIELDS: &[SubParamDef] = &[
        SubParamDef {
            key: "a",
            param_type: ParamType::U64 { min: 0, max: 100 },
            default: "7",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
        SubParamDef {
            key: "b",
            param_type: ParamType::Bool,
            default: "true",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
    ];

    let mut value = serde_json::json!({});
    let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    hydrate_object(&mut value, FIELDS, &mut Vec::new(), &mut paths);

    assert_eq!(value, serde_json::json!({ "a": 7, "b": true }));
    assert!(paths.contains("/a"));
    assert!(paths.contains("/b"));
    assert_eq!(paths.len(), 2);
}

#[test]
fn test_inject_object_subfields_idempotent() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    const FIELDS: &[SubParamDef] = &[SubParamDef {
        key: "a",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "7",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    let mut value = serde_json::json!({});
    let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    hydrate_object(&mut value, FIELDS, &mut Vec::new(), &mut paths);
    hydrate_object(&mut value, FIELDS, &mut Vec::new(), &mut paths);

    assert_eq!(value, serde_json::json!({ "a": 7 }));
    assert_eq!(paths.len(), 1);
}

#[test]
fn test_inject_does_not_hydrate_present_subkey() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    const FIELDS: &[SubParamDef] = &[SubParamDef {
        key: "a",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "7",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    let mut value = serde_json::json!({ "a": 42 });
    let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    hydrate_object(&mut value, FIELDS, &mut Vec::new(), &mut paths);

    assert_eq!(value, serde_json::json!({ "a": 42 }));
    assert!(paths.is_empty(), "user-provided value must not be synthetic");
}

#[test]
fn test_inject_recurses_into_nested_object() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    const INNER: &[SubParamDef] = &[SubParamDef {
        key: "y",
        param_type: ParamType::Bool,
        default: "true",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];
    const OUTER: &[SubParamDef] = &[SubParamDef {
        key: "inner",
        param_type: ParamType::Object { fields: INNER },
        default: "",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    let mut value = serde_json::json!({});
    let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    hydrate_object(&mut value, OUTER, &mut Vec::new(), &mut paths);

    assert_eq!(value, serde_json::json!({ "inner": { "y": true } }));
    assert!(paths.contains("/inner"), "intermediate object node is synthetic");
    assert!(paths.contains("/inner/y"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p dugite-config -E 'test(test_inject_hydrates_object_subfields_when_object_empty)'`
Expected: compile error — `hydrate_object` not defined.

- [ ] **Step 3: Implement the recursion**

Add to `crates/dugite-config/src/config.rs` above the existing `impl LoadedConfig`:

```rust
use crate::schema::SubParamDef;
use crate::path::path_to_json_pointer;

/// Recursively ensure every schema-known leaf in `fields` is present in
/// `value`. Records every inserted path in `synthetic_paths`. The pointer
/// strings recorded use RFC 6901 form (see `path::path_to_json_pointer`).
///
/// Pre-condition: `value` is a `Value::Object` (or `Value::Null`, which is
/// promoted to an empty object before walking).
pub(crate) fn hydrate_object(
    value: &mut Value,
    fields: &[SubParamDef],
    path: &mut Vec<String>,
    synthetic_paths: &mut std::collections::HashSet<String>,
) {
    if value.is_null() {
        *value = Value::Object(serde_json::Map::new());
    }
    let Some(map) = value.as_object_mut() else {
        return; // Type mismatch — leave the user's value alone.
    };

    for sub in fields {
        path.push(sub.key.to_string());

        match &sub.param_type {
            ParamType::Object { fields: inner_fields } => {
                // Create the nested object if absent and record it as synthetic.
                let inserted = !map.contains_key(sub.key);
                let entry = map
                    .entry(sub.key.to_string())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if inserted {
                    synthetic_paths.insert(path_to_json_pointer(path));
                }
                hydrate_object(entry, inner_fields, path, synthetic_paths);
            }
            _ => {
                if !map.contains_key(sub.key) {
                    if let Some(default_value) = sub.default_as_json() {
                        map.insert(sub.key.to_string(), default_value);
                        synthetic_paths.insert(path_to_json_pointer(path));
                    }
                }
            }
        }

        path.pop();
    }
}
```

Add the `use` for `ParamType` at the top of the file if not present:

```rust
use crate::schema::{KNOWN_PARAMS, ParamType};
```

Now extend `LoadedConfig::inject_schema_defaults` (around line 174-189) to also hydrate Objects after the existing top-level pass:

```rust
pub fn inject_schema_defaults(&mut self) {
    use std::collections::HashSet;

    // Pass 1 — append synthetic top-level entries for every schema key not
    // already present in the file.
    let present: HashSet<String> = self.entries.iter().map(|e| e.key.clone()).collect();
    for def in KNOWN_PARAMS {
        if present.contains(def.key) {
            continue;
        }
        if let Some(value) = def.default_as_json() {
            self.entries.push(ConfigEntry {
                key: def.key.to_string(),
                value,
                modified: false,
                present_in_file: false,
                synthetic_paths: HashSet::new(),
            });
        }
    }

    // Pass 2 — for every entry whose schema is `ParamType::Object`, recursively
    // hydrate each missing leaf and record the path in `synthetic_paths`.
    let lookup: std::collections::HashMap<&str, &'static crate::schema::ParamDef> =
        KNOWN_PARAMS.iter().map(|d| (d.key, d)).collect();
    for entry in self.entries.iter_mut() {
        let Some(def) = lookup.get(entry.key.as_str()).copied() else {
            continue;
        };
        if let ParamType::Object { fields } = &def.param_type {
            let mut path: Vec<String> = Vec::new();
            hydrate_object(&mut entry.value, fields, &mut path, &mut entry.synthetic_paths);
        }
    }
}
```

- [ ] **Step 4: Run the new tests and confirm they pass**

Run: `cargo nextest run -p dugite-config -E 'test(test_inject)'`
Expected: all four PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass. The existing `test_inject_schema_defaults_*` tests in `config.rs` still pass because the schema Object entries currently have `fields: &[]` (Task 1), so the new recursion is a no-op on them.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/config.rs
git commit -m "feat(dugite-config): recursive Object sub-field hydration"
```

---

### Task 5: save_config prunes synthetic-default sub-leaves

**Files:**
- Modify: `crates/dugite-config/src/config.rs:246-302` (`save_config`)
- Test: same file's tests module

For each entry, before emitting, walk its `synthetic_paths` and remove any leaf whose current value still equals its schema default. Cascade-prune any sub-object that becomes empty. Leaves the user touched are not in `synthetic_paths` (Task 9 clears them). Leaves in the original file are not in `synthetic_paths`.

- [ ] **Step 1: Write the failing test**

Add to `crates/dugite-config/src/config.rs` tests module:

```rust
#[test]
fn test_save_prunes_synthetic_default_subleaf() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    use std::collections::HashSet;

    // Build a one-off ConfigEntry that mimics what Task 4 produces.
    const FIELDS: &[SubParamDef] = &[
        SubParamDef {
            key: "a",
            param_type: ParamType::U64 { min: 0, max: 100 },
            default: "7",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
        SubParamDef {
            key: "b",
            param_type: ParamType::U64 { min: 0, max: 100 },
            default: "9",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        },
    ];

    let mut value = serde_json::json!({ "a": 7, "b": 42 }); // a is at default, b is not
    let mut paths: HashSet<String> = ["/a", "/b"].iter().map(|s| s.to_string()).collect();
    prune_synthetic_defaults(&mut value, FIELDS, &mut Vec::new(), &paths);

    // 'a' is synthetic + still default → pruned. 'b' is synthetic but not default → kept.
    assert_eq!(value, serde_json::json!({ "b": 42 }));
}

#[test]
fn test_save_prunes_keeps_user_set_leaf_even_if_at_default() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};

    const FIELDS: &[SubParamDef] = &[SubParamDef {
        key: "a",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "7",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    // Value equals default, but path is NOT in synthetic_paths → user touched
    // it (or it was in the original file). Must be retained.
    let mut value = serde_json::json!({ "a": 7 });
    let paths = std::collections::HashSet::new();
    prune_synthetic_defaults(&mut value, FIELDS, &mut Vec::new(), &paths);

    assert_eq!(value, serde_json::json!({ "a": 7 }));
}

#[test]
fn test_save_prunes_nested_empty_object() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    use std::collections::HashSet;

    const INNER: &[SubParamDef] = &[SubParamDef {
        key: "y",
        param_type: ParamType::Bool,
        default: "true",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];
    const OUTER: &[SubParamDef] = &[SubParamDef {
        key: "inner",
        param_type: ParamType::Object { fields: INNER },
        default: "",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    let mut value = serde_json::json!({ "inner": { "y": true } });
    let paths: HashSet<String> = ["/inner", "/inner/y"].iter().map(|s| s.to_string()).collect();
    prune_synthetic_defaults(&mut value, OUTER, &mut Vec::new(), &paths);

    // Inner y is synthetic + default → pruned. Inner is now empty + synthetic → pruned too.
    assert_eq!(value, serde_json::json!({}));
}

#[test]
fn test_save_prunes_keeps_unknown_subkey() {
    use crate::schema::{ParamType, SubParamDef, Reloadability};
    use std::collections::HashSet;

    const FIELDS: &[SubParamDef] = &[SubParamDef {
        key: "a",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "7",
        description: "",
        tuning_hint: "",
        reloadability: Reloadability::Restart,
    }];

    // "a" is at default and synthetic. "NewFeature" is unknown — must round-trip.
    let mut value = serde_json::json!({ "a": 7, "NewFeature": "preserve me" });
    let paths: HashSet<String> = ["/a"].iter().map(|s| s.to_string()).collect();
    prune_synthetic_defaults(&mut value, FIELDS, &mut Vec::new(), &paths);

    assert_eq!(value, serde_json::json!({ "NewFeature": "preserve me" }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p dugite-config -E 'test(test_save_prunes)'`
Expected: compile error — `prune_synthetic_defaults` not defined.

- [ ] **Step 3: Implement the prune walker**

Add to `crates/dugite-config/src/config.rs` near `hydrate_object`:

```rust
/// Walk `value` against `fields`, removing any leaf at a path in
/// `synthetic_paths` whose value equals the schema default. Cascade-prune any
/// sub-object that becomes empty AND whose path is itself in
/// `synthetic_paths` (i.e. the parent object was also synthetically created).
///
/// Unknown sub-keys (present in `value` but absent from `fields`) are left
/// untouched.
pub(crate) fn prune_synthetic_defaults(
    value: &mut Value,
    fields: &[SubParamDef],
    path: &mut Vec<String>,
    synthetic_paths: &std::collections::HashSet<String>,
) {
    let Some(map) = value.as_object_mut() else {
        return;
    };

    for sub in fields {
        path.push(sub.key.to_string());
        let pointer = path_to_json_pointer(path);

        match &sub.param_type {
            ParamType::Object { fields: inner_fields } => {
                if let Some(child) = map.get_mut(sub.key) {
                    prune_synthetic_defaults(child, inner_fields, path, synthetic_paths);
                    // After child-pruning, drop the now-empty synthetic parent.
                    if synthetic_paths.contains(&pointer) {
                        if let Some(child_map) = child.as_object() {
                            if child_map.is_empty() {
                                map.remove(sub.key);
                            }
                        }
                    }
                }
            }
            _ => {
                if synthetic_paths.contains(&pointer) {
                    if let Some(current) = map.get(sub.key) {
                        if let Some(default_value) = sub.default_as_json() {
                            if current == &default_value {
                                map.remove(sub.key);
                            }
                        }
                    }
                }
            }
        }

        path.pop();
    }
}
```

Now wire it into `save_config` (around line 246-268). Replace the rebuild section:

```rust
// Step 2 — rebuild JSON object in entry order, skipping synthetic entries
// that still hold the schema default (keeps the file's diff minimal).
let lookup: std::collections::HashMap<&str, &'static crate::schema::ParamDef> =
    KNOWN_PARAMS.iter().map(|d| (d.key, d)).collect();
let schema_defaults: std::collections::HashMap<&str, Value> = KNOWN_PARAMS
    .iter()
    .filter_map(|d| d.default_as_json().map(|v| (d.key, v)))
    .collect();

let mut obj = serde_json::Map::new();
for entry in &config.entries {
    // Per-Object sub-field prune (operates on a clone — entry stays hydrated
    // so the UI keeps its current view after save).
    let mut emit_value = entry.value.clone();
    if let Some(def) = lookup.get(entry.key.as_str()).copied() {
        if let ParamType::Object { fields } = &def.param_type {
            let mut p: Vec<String> = Vec::new();
            prune_synthetic_defaults(&mut emit_value, fields, &mut p, &entry.synthetic_paths);
        }
    }

    // Top-level synthetic+default prune (existing behaviour).
    if !entry.present_in_file {
        if let Some(default) = schema_defaults.get(entry.key.as_str()) {
            if &emit_value == default {
                continue;
            }
        }
    }
    obj.insert(entry.key.clone(), emit_value);
}
let json = Value::Object(obj);
```

- [ ] **Step 4: Run the new tests and confirm they pass**

Run: `cargo nextest run -p dugite-config -E 'test(test_save_prunes)'`
Expected: all four PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass. The existing `test_save_*` tests still pass because Object entries currently have `fields: &[]` so the new path is a no-op.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/config.rs
git commit -m "feat(dugite-config): prune synthetic-default sub-leaves on save"
```

---

### Task 6: Item view-model with path, depth, container flags

**Files:**
- Modify: `crates/dugite-config/src/app.rs:37-44` (`Item`), `crates/dugite-config/src/app.rs:648-702` (`build_sections`)
- Test: tests module in same file

Replace the two-field `Item` (`entry_idx`, `def`) with a structure that carries a JSON path inside its parent entry, a depth, and container/expansion flags. Rewrite `build_sections` to do a depth-first walk of each Object entry's sub-schema, emitting a header row plus one row per schema-known leaf and per unknown sub-key.

Top-level rows have empty `path` and `depth == 0`. Nested rows have `depth >= 1`. Container rows (Object) have `is_container == true` and default `expanded == false`.

- [ ] **Step 1: Write the failing test**

Add to `crates/dugite-config/src/app.rs` tests module:

```rust
#[test]
fn test_build_sections_emits_object_header_and_subleaves() {
    // Build a config containing AcceptedConnectionsLimit AFTER Tasks 14 has
    // populated its sub-schema. To make this test independent of the schema
    // migration sequencing, we construct a small in-memory App via a special
    // builder — but the simplest route is to defer until Task 14.
    //
    // For now write the test against the actual schema once Task 14 lands.
    // To unblock Task 6, write a smaller test that ONLY checks the Item type
    // shape and one synthetic walk:

    use crate::schema::{KNOWN_PARAMS};
    // Verify all current ParamType::Object entries default-collapsed.
    let mut app = make_app(r#"{}"#);
    // After Task 14-16 every ParamType::Object entry expands to a header + leaves.
    // For Task 6 alone (with fields: &[]) we only get the header.
    let any_object = KNOWN_PARAMS.iter().any(|d| matches!(
        &d.param_type,
        crate::schema::ParamType::Object { fields: _ }
    ));
    assert!(any_object, "test premise: at least one Object exists in schema");

    // Locate the AcceptedConnectionsLimit header row and confirm its flags.
    let mut found = false;
    for section in &app.sections {
        for item in &section.items {
            if app.config.entries[item.entry_idx].key == "AcceptedConnectionsLimit"
                && item.path.is_empty()
            {
                assert!(item.is_container, "Object row must be a container");
                assert!(!item.expanded, "Object rows start collapsed");
                assert_eq!(item.depth, 0, "top-level row depth must be 0");
                found = true;
            }
        }
    }
    assert!(found, "AcceptedConnectionsLimit header row not found");
    // Avoid unused-mut warning — we read app via iteration above.
    let _ = &mut app;
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_build_sections_emits_object_header_and_subleaves)'`
Expected: fail — `is_container` field doesn't exist.

- [ ] **Step 3: Implement the new Item shape**

Edit `crates/dugite-config/src/app.rs:37-44`:

```rust
/// A single row in the left-panel tree.
///
/// A row addresses either a whole top-level entry (`path.is_empty()`) or a
/// sub-field inside an Object entry (e.g. `path = ["Tls", "CertPath"]`).
#[derive(Debug, Clone)]
pub struct Item {
    /// Index into [`LoadedConfig::entries`] for this row's top-level entry.
    pub entry_idx: usize,
    /// Path inside the top-level entry's `Value`. Empty for top-level rows.
    pub path: Vec<String>,
    /// Resolved schema, if any.
    pub def: ItemDef,
    /// Nesting depth (0 = top-level). Drives left-padding in the UI.
    pub depth: u8,
    /// True for Object rows (no inline edit; Enter toggles `expanded`).
    pub is_container: bool,
    /// Only meaningful when `is_container`. Object rows start `false`.
    pub expanded: bool,
}

/// Resolved schema for an [`Item`].
#[derive(Debug, Clone, Copy)]
pub enum ItemDef {
    /// Top-level entry with a `ParamDef`.
    Top(&'static crate::schema::ParamDef),
    /// Sub-leaf inside an Object entry.
    Sub(&'static crate::schema::SubParamDef),
    /// No schema available — either a top-level Unknown key or an unknown
    /// sub-key under a known Object.
    Unknown,
}

impl ItemDef {
    /// Return the leaf's `ParamType`, if any.
    pub fn param_type(&self) -> Option<&'static ParamType> {
        match self {
            ItemDef::Top(def) => Some(&def.param_type),
            ItemDef::Sub(sub) => Some(&sub.param_type),
            ItemDef::Unknown => None,
        }
    }

    /// Return the leaf's reloadability, if any.
    pub fn reloadability(&self) -> Option<Reloadability> {
        match self {
            ItemDef::Top(def) => Some(def.reloadability),
            ItemDef::Sub(sub) => Some(sub.reloadability),
            ItemDef::Unknown => None,
        }
    }
}
```

You'll need to import `ParamType`, `SubParamDef`, `Reloadability` at the top of `app.rs` — check existing imports and extend the `use crate::schema::...` line.

Replace `build_sections` (around line 648-702) with a version that walks Objects:

```rust
fn build_sections(
    config: &LoadedConfig,
    lookup: &HashMap<&'static str, &'static ParamDef>,
) -> Vec<Section> {
    let mut section_map: HashMap<String, Vec<Item>> = HashMap::new();

    for (entry_idx, entry) in config.entries.iter().enumerate() {
        let def_opt = lookup.get(entry.key.as_str()).copied();
        let section_name = def_opt
            .map(|d| d.section.to_string())
            .unwrap_or_else(|| SECTION_UNKNOWN.to_string());
        let items_for_section = section_map.entry(section_name).or_default();

        // Emit the top-level row.
        let top_def = def_opt.map(ItemDef::Top).unwrap_or(ItemDef::Unknown);
        let is_object = matches!(
            def_opt.map(|d| &d.param_type),
            Some(ParamType::Object { .. })
        );
        items_for_section.push(Item {
            entry_idx,
            path: Vec::new(),
            def: top_def,
            depth: 0,
            is_container: is_object,
            expanded: false, // Object rows start collapsed (Section C of spec).
        });

        // For Object entries, walk the sub-schema and unknown sub-keys.
        if let Some(def) = def_opt {
            if let ParamType::Object { fields } = &def.param_type {
                let mut path: Vec<String> = Vec::new();
                walk_object_rows(entry_idx, &entry.value, fields, 1, &mut path, items_for_section);
            }
        }
    }

    // Schema-order sort: order top-level entries by schema position; sub-rows
    // stay immediately after their parent (walk_object_rows already emits them
    // in schema order). To preserve the contiguity, we cluster items by
    // top-level entry, sort the clusters by parent key's schema order, then
    // re-flatten.
    let schema_order: HashMap<&str, usize> = KNOWN_PARAMS
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key, i))
        .collect();
    for items in section_map.values_mut() {
        sort_items_by_schema(items, config, &schema_order);
    }

    let mut names: Vec<String> = section_map.keys().cloned().collect();
    names.sort_by(|a, b| {
        let pa = section_priority(a.as_str());
        let pb = section_priority(b.as_str());
        pa.cmp(&pb).then(a.cmp(b))
    });

    names
        .into_iter()
        .map(|name| Section {
            items: section_map.remove(&name).unwrap_or_default(),
            name,
            expanded: true,
        })
        .collect()
}

/// Append rows for every schema-known sub-field of `value` plus any unknown
/// sub-keys present in the file, in schema order then alphabetical for
/// unknowns.
fn walk_object_rows(
    entry_idx: usize,
    value: &Value,
    fields: &[SubParamDef],
    depth: u8,
    path: &mut Vec<String>,
    out: &mut Vec<Item>,
) {
    let map = match value.as_object() {
        Some(m) => m,
        None => return,
    };

    // Schema-known sub-fields first, in declared order.
    for sub in fields {
        path.push(sub.key.to_string());

        match &sub.param_type {
            ParamType::Object { fields: inner_fields } => {
                out.push(Item {
                    entry_idx,
                    path: path.clone(),
                    def: ItemDef::Sub(sub),
                    depth,
                    is_container: true,
                    expanded: false,
                });
                if let Some(child) = map.get(sub.key) {
                    walk_object_rows(entry_idx, child, inner_fields, depth + 1, path, out);
                }
            }
            _ => {
                out.push(Item {
                    entry_idx,
                    path: path.clone(),
                    def: ItemDef::Sub(sub),
                    depth,
                    is_container: false,
                    expanded: false,
                });
            }
        }

        path.pop();
    }

    // Then unknown sub-keys (present in file but absent from schema), alpha order.
    let known: std::collections::HashSet<&str> = fields.iter().map(|s| s.key).collect();
    let mut unknown_keys: Vec<&String> =
        map.keys().filter(|k| !known.contains(k.as_str())).collect();
    unknown_keys.sort();
    for k in unknown_keys {
        path.push(k.clone());
        out.push(Item {
            entry_idx,
            path: path.clone(),
            def: ItemDef::Unknown,
            depth,
            is_container: false, // even if value is an Object — no schema for it
            expanded: false,
        });
        path.pop();
    }
}

fn sort_items_by_schema(
    items: &mut Vec<Item>,
    config: &LoadedConfig,
    schema_order: &HashMap<&str, usize>,
) {
    // Group items into clusters keyed by entry_idx (preserves intra-cluster order).
    let mut clusters: HashMap<usize, Vec<Item>> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    for item in items.drain(..) {
        if !clusters.contains_key(&item.entry_idx) {
            order.push(item.entry_idx);
        }
        clusters.entry(item.entry_idx).or_default().push(item);
    }

    order.sort_by(|a, b| {
        let key_a = config.entries[*a].key.as_str();
        let key_b = config.entries[*b].key.as_str();
        let pa = schema_order.get(key_a).copied().unwrap_or(usize::MAX);
        let pb = schema_order.get(key_b).copied().unwrap_or(usize::MAX);
        pa.cmp(&pb).then(key_a.cmp(key_b))
    });

    for idx in order {
        items.extend(clusters.remove(&idx).unwrap_or_default());
    }
}
```

Update imports at the top of `app.rs` to include `Value`:

```rust
use serde_json::Value;
```

The compilation will fail wherever `item.def` is read as `Option<&'static ParamDef>` — convert those sites to use the new `ItemDef`. Search:

```bash
grep -n "item.def\|\.def\b" crates/dugite-config/src/app.rs crates/dugite-config/src/ui.rs
```

Typical conversion: `def.map(|d| ...)` → `def.param_type().map(|pt| ...)` etc. Each call site needs a small edit. Don't change behaviour here — just preserve it; Tasks 8-9 actually take advantage of the new fields.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_build_sections_emits_object_header_and_subleaves)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass. The existing cursor/move/edit tests still operate on the (unchanged) flow because Object `fields` is still `&[]` from Task 1 — there are no extra rows yet. Tests that read `item.def` need their assertions adjusted (was `Option<&ParamDef>` → now `ItemDef`). Fix them inline.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/app.rs crates/dugite-config/src/ui.rs
# also any other call sites your grep turned up:
git status --short
git commit -m "feat(dugite-config): path-addressed Item with depth/container flags"
```

---

### Task 7: Cursor visibility skipping for collapsed containers

**Files:**
- Modify: `crates/dugite-config/src/app.rs:170-225` (`cursor_up` / `cursor_down`)
- Test: same file's tests module

Add an `is_visible(item)` helper and have `cursor_up` / `cursor_down` skip rows whose ancestor container is collapsed.

- [ ] **Step 1: Write the failing test**

Add to `app.rs` tests module:

```rust
#[test]
fn test_cursor_skips_rows_under_collapsed_container() {
    let mut app = make_app(r#"{}"#);

    // After build_sections every Object header is collapsed. The cursor must
    // never land on a sub-row whose parent header is collapsed.
    // Locate AcceptedConnectionsLimit row (top-level, depth 0).
    move_cursor_to_key(&mut app, "AcceptedConnectionsLimit");
    let sec = app.cursor_section;
    let item_idx_header = app.cursor_item;

    // Confirm the next visible row is NOT a sub-row (path non-empty) of the
    // same entry — it must be the next top-level row (or end of section).
    app.cursor_down();
    let new_item = &app.sections[app.cursor_section].items[app.cursor_item];
    if app.cursor_section == sec {
        // Still in the same section — the new row's path must be empty (top-level)
        // or it belongs to a different top-level entry.
        let header_entry_idx =
            app.sections[sec].items[item_idx_header].entry_idx;
        assert!(
            new_item.path.is_empty() || new_item.entry_idx != header_entry_idx,
            "cursor_down landed on a sub-row of the collapsed container at item {} (path = {:?})",
            app.cursor_item,
            new_item.path
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_cursor_skips_rows_under_collapsed_container)'`
Expected: PASS even before Task 7 (because Object `fields` is still empty — no sub-rows). This is fine — keep the test, it becomes meaningful once Tasks 14-16 land. Continue to Step 3 to add the helper now so the later test runs cleanly.

- [ ] **Step 3: Implement is_visible and use it from cursor_up/down**

Add as an impl method on `App` (place near `selected_item`, around line 580):

```rust
/// Return whether `item` should be cursor-addressable given the current
/// state of its ancestor container rows.
///
/// An item is hidden iff some prefix of its `path` is a container row in the
/// same section whose `expanded == false`. Top-level rows (empty path) are
/// always visible (subject to their section's `expanded`, which is handled
/// elsewhere).
pub fn is_visible(&self, section_idx: usize, item_idx: usize) -> bool {
    let section = &self.sections[section_idx];
    let item = &section.items[item_idx];

    if item.path.is_empty() {
        return true;
    }

    // For each prefix of `item.path`, find the matching container row in the
    // same section under the same entry_idx. If any such container has
    // expanded=false, the row is hidden.
    for prefix_len in 0..item.path.len() {
        let prefix = &item.path[..prefix_len];
        // Find a container row with this prefix.
        for candidate in &section.items {
            if candidate.entry_idx != item.entry_idx {
                continue;
            }
            if !candidate.is_container {
                continue;
            }
            if candidate.path.len() == prefix.len() && candidate.path[..] == prefix[..] {
                if !candidate.expanded {
                    return false;
                }
            }
        }
    }
    true
}
```

Now update `cursor_down` (around line 195-225). Find the inner loop and skip non-visible rows:

```rust
pub fn cursor_down(&mut self) {
    if self.search_active && !self.filtered_items.is_empty() {
        // (unchanged search path)
        let pos = /* existing */ 0;
        if pos + 1 < self.filtered_items.len() {
            let (sec, item) = self.filtered_items[pos + 1];
            self.cursor_section = sec;
            self.cursor_item = item;
        }
        return;
    }

    let (mut sec, mut item) = (self.cursor_section, self.cursor_item);
    let total_sections = self.sections.len();
    loop {
        let expanded = self.sections[sec].expanded;
        let item_count = self.sections[sec].items.len();
        if expanded && item + 1 < item_count {
            item += 1;
        } else if sec + 1 < total_sections {
            sec += 1;
            item = 0;
        } else {
            // No further row exists.
            return;
        }
        if self.is_visible(sec, item) {
            self.cursor_section = sec;
            self.cursor_item = item;
            return;
        }
    }
}
```

Do the symmetric change for `cursor_up`. Be careful to preserve the existing
"move to last item of previous section" semantics.

(If your current `cursor_down` has logic interlaced with the search path, copy
the existing function verbatim into your editor before refactoring — only the
inner movement loop changes.)

- [ ] **Step 4: Run the new test and the existing cursor tests**

Run: `cargo nextest run -p dugite-config -E 'test(cursor)'`
Expected: all PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/app.rs
git commit -m "feat(dugite-config): cursor skips rows under collapsed containers"
```

---

### Task 8: begin_edit toggles container expansion

**Files:**
- Modify: `crates/dugite-config/src/app.rs:250-288` (`begin_edit`)
- Test: same file's tests module

Pressing Enter on a container row should flip `item.expanded` and **not** open a typing buffer. Pressing Enter on a non-container row keeps today's behavior (bool toggle / enum cycle / typing buffer).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_begin_edit_on_container_toggles_expansion() {
    let mut app = make_app(r#"{}"#);
    move_cursor_to_key(&mut app, "AcceptedConnectionsLimit");

    // Initially collapsed.
    let initial = app.sections[app.cursor_section].items[app.cursor_item].expanded;
    assert!(!initial, "Object rows start collapsed");

    app.begin_edit();
    assert!(
        app.sections[app.cursor_section].items[app.cursor_item].expanded,
        "begin_edit on container must expand it"
    );
    assert_eq!(app.edit_mode, EditMode::None, "no typing buffer should open");

    app.begin_edit();
    assert!(
        !app.sections[app.cursor_section].items[app.cursor_item].expanded,
        "second begin_edit must collapse"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_begin_edit_on_container_toggles_expansion)'`
Expected: FAIL — begin_edit doesn't yet know about containers, may open a typing buffer or do nothing useful.

- [ ] **Step 3: Update begin_edit**

Edit `crates/dugite-config/src/app.rs:250-288`. Insert a container check at the top of the match:

```rust
pub fn begin_edit(&mut self) {
    if self.edit_mode != EditMode::None {
        return;
    }
    let Some(item) = self.selected_item().cloned() else {
        return;
    };

    // Container row → toggle expansion, no typing buffer.
    if item.is_container {
        let sec = self.cursor_section;
        let i = self.cursor_item;
        let cur = self.sections[sec].items[i].expanded;
        self.sections[sec].items[i].expanded = !cur;
        self.feedback = Some(if !cur {
            format!("Expanded '{}'", self.config.entries[item.entry_idx].key)
        } else {
            format!("Collapsed '{}'", self.config.entries[item.entry_idx].key)
        });
        return;
    }

    // Existing per-leaf dispatch (Bool toggle / Enum cycle / Typing buffer).
    let entry_idx = item.entry_idx;
    match item.def.param_type() {
        Some(ParamType::Bool) => {
            // ... existing Bool branch, but path-aware (see Task 9)
        }
        Some(ParamType::Enum { values }) => {
            // ... existing Enum branch, but path-aware (see Task 9)
        }
        _ => {
            // Open typing buffer pre-filled with current value at the path
            // (or top-level value if path is empty).
            let entry = &self.config.entries[entry_idx];
            let current = display_value_at_path(&entry.value, &item.path);
            self.edit_mode = EditMode::Typing {
                buffer: current,
                error: None,
            };
        }
    }
}
```

Add a free helper near the bottom of `app.rs`:

```rust
/// Return a display string for the value at `path` inside `value`. Falls back
/// to the whole value's display if `path` is empty.
fn display_value_at_path(value: &Value, path: &[String]) -> String {
    let pointer = crate::path::path_to_json_pointer(path);
    let v = if pointer.is_empty() {
        value
    } else {
        match value.pointer(&pointer) {
            Some(v) => v,
            None => return String::new(),
        }
    };
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
```

You'll need `selected_item` to return a cloneable handle — change its signature to return `Option<Item>` (via `.cloned()`) or return both the section and item indices and let the caller borrow. The simplest patch: keep `selected_item` returning a `&Item` but clone the small `Item` struct at the begin_edit call site (as in the code above).

- [ ] **Step 4: Run the new test and existing edit tests**

Run: `cargo nextest run -p dugite-config -E 'test(begin_edit)'`
Expected: all PASS. Existing tests still touch top-level non-container rows, which take the existing path.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/app.rs
git commit -m "feat(dugite-config): Enter on Object row toggles expansion"
```

---

### Task 9: Path-aware edit application (Bool toggle, Enum cycle, Typing commit)

**Files:**
- Modify: `crates/dugite-config/src/app.rs:250-343` (`begin_edit` Bool/Enum branches; `confirm_edit`)
- Modify: `crates/dugite-config/src/config.rs` — add path-aware helpers on `ConfigEntry`
- Test: tests module of `app.rs`

Generalise `ConfigEntry::toggle_bool`, `cycle_enum`, and `apply_edit` to operate at a path. After any successful edit, remove the edited path from the parent entry's `synthetic_paths` (the leaf is no longer synthetic) and mark the top-level entry `modified = true`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_subleaf_edit_writes_through_pointer_and_clears_synthetic() {
    // Once Task 14 populates AcceptedConnectionsLimit.fields, hardLimit is a
    // synthetic-default leaf. Edit it and check the result.
    let mut app = make_app(r#"{}"#);

    // Find hardLimit sub-leaf and move cursor onto it.
    let mut located: Option<(usize, usize)> = None;
    for (sec_idx, section) in app.sections.iter().enumerate() {
        for (item_idx, item) in section.items.iter().enumerate() {
            if app.config.entries[item.entry_idx].key == "AcceptedConnectionsLimit"
                && item.path == vec!["hardLimit".to_string()]
            {
                located = Some((sec_idx, item_idx));
            }
        }
    }
    let (sec, idx) = located.expect("hardLimit row must exist");
    // Force the section + container open so the cursor can land there.
    app.sections[sec].items[/* find AcceptedConnectionsLimit header */ 0].expanded = true;
    app.cursor_section = sec;
    app.cursor_item = idx;

    // Open buffer, type a value, confirm.
    app.begin_edit();
    assert!(matches!(app.edit_mode, EditMode::Typing { .. }));
    // Replace the buffer contents.
    if let EditMode::Typing { buffer, .. } = &mut app.edit_mode {
        buffer.clear();
        buffer.push_str("1024");
    }
    app.confirm_edit();

    // Locate the parent entry and check the value.
    let parent = app
        .config
        .entries
        .iter()
        .find(|e| e.key == "AcceptedConnectionsLimit")
        .expect("parent entry exists");
    assert_eq!(
        parent.value.pointer("/hardLimit"),
        Some(&serde_json::json!(1024)),
        "edited value should be written at /hardLimit"
    );
    assert!(parent.modified, "parent entry must be marked modified");
    assert!(
        !parent.synthetic_paths.contains("/hardLimit"),
        "touched leaf must no longer be synthetic"
    );
}
```

(This test depends on Task 14 having populated AcceptedConnectionsLimit's `fields`. If sequencing this task before Task 14, replace `hardLimit`/`1024` with a hand-rolled `ParamDef` injected into a test-only schema. Easier: do Task 14 first, then Task 9, since Task 14 is a pure data edit.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_subleaf_edit_writes_through_pointer_and_clears_synthetic)'`
Expected: FAIL — confirm_edit currently writes via `entry.apply_edit(&raw)`, which clobbers the top-level value not the leaf.

- [ ] **Step 3: Implement path-aware edit application**

Add to `crates/dugite-config/src/config.rs` impl `ConfigEntry`:

```rust
/// Apply a string edit at the given path inside this entry's value, using
/// the existing-value's type to choose a parse strategy (same rules as
/// [`apply_edit`]). When `path` is empty, behaves identically to
/// `apply_edit`.
///
/// On success, marks the entry `modified` and removes the path's pointer
/// from `synthetic_paths`.
pub fn apply_edit_at(&mut self, path: &[String], raw: &str) -> Result<()> {
    if path.is_empty() {
        return self.apply_edit(raw);
    }
    let pointer = crate::path::path_to_json_pointer(path);
    let slot = self
        .value
        .pointer_mut(&pointer)
        .with_context(|| format!("no value at '{pointer}'"))?;
    let new_value = match slot {
        Value::Bool(_) => raw
            .parse::<bool>()
            .map(Value::Bool)
            .with_context(|| format!("'{raw}' is not a valid boolean"))?,
        Value::Number(_) => {
            if let Ok(i) = raw.parse::<i64>() {
                Value::Number(serde_json::Number::from(i))
            } else if let Ok(f) = raw.parse::<f64>() {
                Value::Number(
                    serde_json::Number::from_f64(f)
                        .with_context(|| format!("'{raw}' is not finite"))?,
                )
            } else {
                anyhow::bail!("'{raw}' is not a valid number")
            }
        }
        Value::String(_) => Value::String(raw.to_string()),
        _ => Value::String(raw.to_string()),
    };
    *slot = new_value;
    self.modified = true;
    self.synthetic_paths.remove(&pointer);
    Ok(())
}

/// Toggle a boolean at the given path. Empty path operates on the whole entry.
pub fn toggle_bool_at(&mut self, path: &[String]) -> Result<()> {
    if path.is_empty() {
        return self.toggle_bool();
    }
    let pointer = crate::path::path_to_json_pointer(path);
    let slot = self
        .value
        .pointer_mut(&pointer)
        .with_context(|| format!("no value at '{pointer}'"))?;
    match slot {
        Value::Bool(b) => {
            *slot = Value::Bool(!*b);
            self.modified = true;
            self.synthetic_paths.remove(&pointer);
            Ok(())
        }
        _ => anyhow::bail!("cannot toggle non-boolean at '{pointer}'"),
    }
}

/// Cycle an enum value at the given path through `choices`.
pub fn cycle_enum_at(&mut self, path: &[String], choices: &[&str]) {
    if path.is_empty() {
        return self.cycle_enum(choices);
    }
    if choices.is_empty() {
        return;
    }
    let pointer = crate::path::path_to_json_pointer(path);
    let Some(slot) = self.value.pointer_mut(&pointer) else { return; };
    let current = match slot {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let next = choices
        .iter()
        .position(|c| *c == current.as_str())
        .map(|i| choices[(i + 1) % choices.len()])
        .unwrap_or(choices[0]);
    *slot = Value::String(next.to_string());
    self.modified = true;
    self.synthetic_paths.remove(&pointer);
}

/// Return a display string for the value at the given path. Empty path
/// returns the same as `display_value`.
pub fn display_value_at(&self, path: &[String]) -> String {
    if path.is_empty() {
        return self.display_value();
    }
    let pointer = crate::path::path_to_json_pointer(path);
    match self.value.pointer(&pointer) {
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}
```

Now route the `begin_edit` Bool / Enum branches and `confirm_edit` through these. Edit `crates/dugite-config/src/app.rs`:

```rust
// In begin_edit, replace the Bool branch with:
Some(ParamType::Bool) => {
    let entry = &mut self.config.entries[entry_idx];
    if let Err(e) = entry.toggle_bool_at(&item.path) {
        self.feedback = Some(format!("Toggle failed: {e}"));
    } else {
        let new_val = entry.display_value_at(&item.path);
        self.feedback = Some(format!("Set to {new_val}"));
    }
}
// Replace the Enum branch with:
Some(ParamType::Enum { values }) => {
    let choices: Vec<&str> = values.to_vec();
    let entry = &mut self.config.entries[entry_idx];
    entry.cycle_enum_at(&item.path, &choices);
    let new_val = entry.display_value_at(&item.path);
    self.feedback = Some(format!("Set to {new_val}"));
}
```

And `confirm_edit` (around line 309-343). Replace the apply call:

```rust
let entry = &mut self.config.entries[entry_idx];
if let Err(e) = entry.apply_edit_at(&item_path, &raw) {
    if let EditMode::Typing { error, .. } = &mut self.edit_mode {
        *error = Some(e.to_string());
    }
    return;
}
```

You'll need to capture `item.path` before the mutable borrow of `entry`. Hold it in `let item_path = item.path.clone();` earlier in the function.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_subleaf_edit_writes_through_pointer_and_clears_synthetic)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/app.rs crates/dugite-config/src/config.rs
git commit -m "feat(dugite-config): path-aware edit dispatch for sub-leaves"
```

---

### Task 10: Search includes sub-leaves

**Files:**
- Modify: `crates/dugite-config/src/app.rs` (`recompute_filter` — find via grep)
- Test: tests module of `app.rs`

`recompute_filter` iterates `(sec_idx, item_idx, key, description, tuning_hint)` over the items. For sub-leaves the displayed key is `Parent.SubKey...` so search should index by joined display key plus sub-field description/hint.

- [ ] **Step 1: Locate recompute_filter**

Run: `grep -n "recompute_filter\|search(" crates/dugite-config/src/app.rs`
Expected: locate the function. Read it to understand its current shape.

- [ ] **Step 2: Write a failing test**

Add to `app.rs` tests module. This depends on Task 14 for `AcceptedConnectionsLimit.hardLimit` to exist; sequence accordingly:

```rust
#[test]
fn test_search_matches_subleaf_by_dotted_key() {
    let mut app = make_app(r#"{}"#);
    app.enter_search();
    app.search_type_char('h');
    app.search_type_char('a');
    app.search_type_char('r');
    app.search_type_char('d');

    // "hardLimit" sub-leaf of AcceptedConnectionsLimit must be in filtered_items.
    let found = app.filtered_items.iter().any(|(sec, idx)| {
        let item = &app.sections[*sec].items[*idx];
        item.path == vec!["hardLimit".to_string()]
            && app.config.entries[item.entry_idx].key == "AcceptedConnectionsLimit"
    });
    assert!(found, "search 'hard' must surface AcceptedConnectionsLimit.hardLimit");
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_search_matches_subleaf_by_dotted_key)'`
Expected: FAIL (or PASS-by-accident depending on the existing key text — if FAIL, continue).

- [ ] **Step 4: Update recompute_filter**

Find the function (likely around line 380-430). The current iterator builds a tuple per item using `entry.key` and `def.description` / `def.tuning_hint`. Replace with a path-aware build:

```rust
fn recompute_filter(&mut self) {
    self.filtered_items.clear();
    let query = self.search_query.trim();
    if query.is_empty() {
        return;
    }

    let mut items: Vec<(usize, usize, String, &'static str, &'static str)> = Vec::new();
    for (sec_idx, section) in self.sections.iter().enumerate() {
        for (item_idx, item) in section.items.iter().enumerate() {
            let entry = &self.config.entries[item.entry_idx];
            // Display key: top-level key + dotted sub-path.
            let mut key_text = entry.key.clone();
            for seg in &item.path {
                key_text.push('.');
                key_text.push_str(seg);
            }
            let (desc, hint) = match &item.def {
                ItemDef::Top(d) => (d.description, d.tuning_hint),
                ItemDef::Sub(s) => (s.description, s.tuning_hint),
                ItemDef::Unknown => ("", ""),
            };
            items.push((sec_idx, item_idx, key_text, desc, hint));
        }
    }

    let results = crate::search::search(query, items.into_iter());
    self.filtered_items = results.into_iter().map(|m| (m.section_idx, m.item_idx)).collect();
}
```

Verify the `MatchResult` struct shape — check `crates/dugite-config/src/search.rs` to confirm field names (`section_idx` / `item_idx`). If different, adjust the mapping accordingly.

The signature of `search::search` may need to accept an owned `String` for the key — confirm by reading its definition. If today it borrows `&'static str`, change the type bounds so it accepts `String` or borrow appropriately. Adjust call site.

- [ ] **Step 5: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_search_matches_subleaf_by_dotted_key)'`
Expected: PASS.

- [ ] **Step 6: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 7: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/dugite-config/src/app.rs crates/dugite-config/src/search.rs
git commit -m "feat(dugite-config): search indexes sub-leaves with dotted keys"
```

---

### Task 11: UI rendering of container rows and depth indent

**Files:**
- Modify: `crates/dugite-config/src/ui.rs:404-...` (`render_item_row`) and its call sites at lines 316/366
- Test: minimal — UI is rendered via ratatui; functional tests focus on the helper that builds the visible row text

Render rows with `2 + 2*depth` leading spaces, prefix container rows with `▸ ` (collapsed) or `▾ ` (expanded), suppress the value column on container rows (the row shows just the key), and route reloadability indicator from `ItemDef` instead of from `Option<&ParamDef>`.

- [ ] **Step 1: Identify all sites that read `item.def` / `item.entry_idx`**

Run: `grep -n "item\.\|\.def\b\|\.reloadability" crates/dugite-config/src/ui.rs`
Expected: a handful of call sites. Read each to understand how the current code uses `Option<&ParamDef>`.

- [ ] **Step 2: Update render_item_row signature and body**

Replace `render_item_row` (around line 404). New signature:

```rust
fn render_item_row(
    entry: &ConfigEntry,
    item: &Item,                 // carries path, depth, is_container, expanded, def
    display_value: &str,         // already resolved at the row's path by the caller
    is_cursor: bool,
    is_typing: bool,
    key_ranges: &[(usize, usize)],
    width: usize,
) -> ListItem<'static> {
    use crate::app::ItemDef;
    // Reloadability indicator.
    let reload_indicator_owned: String = item
        .def
        .reloadability()
        .map(|r| format!("{} ", r.indicator()))
        .unwrap_or_default();
    let reload_color = match item.def.reloadability() {
        Some(Reloadability::Hot) => C_RELOAD_HOT,
        Some(Reloadability::Restart) => C_RELOAD_RESTART,
        None => C_MUTED,
    };
    let reload_indicator: &str = &reload_indicator_owned;

    // Indent grows with depth.
    let indent: String = " ".repeat(2 + 2 * (item.depth as usize));

    // Container glyph prefixes the key when this row is a container.
    let container_glyph = if item.is_container {
        if item.expanded { "▾ " } else { "▸ " }
    } else {
        ""
    };

    // Row's display key: top-level key for path=[], otherwise the last segment.
    let row_key: &str = if let Some(last) = item.path.last() {
        last.as_str()
    } else {
        entry.key.as_str()
    };
    let key_label = format!("{indent}{container_glyph}{row_key}");

    // Container rows show no value column.
    let (raw_value, is_default_only) = if item.is_container {
        (String::new(), false)
    } else {
        let is_default_only = !entry.present_in_file && !entry.modified;
        let v = if is_default_only {
            format!("{display_value} (default)")
        } else {
            display_value.to_string()
        };
        (v, is_default_only)
    };

    // ... rest of function (value coloring, truncation, span building) unchanged
    // except: `def` is now `item.def` and `value_color_for` takes `Option<&ParamType>`.
    // Replace `value_color_for(def, display_value)` with
    // `value_color_for(item.def.param_type(), display_value)`.
    // ...
}
```

Update the helper `value_color_for` (find via grep) to accept `Option<&ParamType>`:

```rust
fn value_color_for(param_type: Option<&ParamType>, _value: &str) -> Color {
    match param_type {
        Some(ParamType::Bool) => C_SUCCESS,
        // ... preserve existing per-type coloring ...
        _ => C_FG,
    }
}
```

Update the two call sites (around lines 316 and 366) to pass the new arguments:

```rust
let row = render_item_row(
    entry,
    item,
    &display_value,
    is_cursor,
    is_typing,
    &key_ranges,
    w,
);
```

`display_value` is now computed from the entry + path:

```rust
let display_value = entry.display_value_at(&item.path);
```

Also: cursor visibility — when rendering the section body, skip items where `is_visible(sec_idx, item_idx)` returns false. Add to the render loop (around line 296):

```rust
for (item_idx, item) in section.items.iter().enumerate() {
    if !app.is_visible(sec_idx, item_idx) {
        continue;
    }
    // ... existing render code ...
}
```

- [ ] **Step 3: Manually smoke-test the TUI**

Run: `cargo build -p dugite-config`
Then: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: TUI launches. Navigate to `AcceptedConnectionsLimit` (still has `fields: &[]` until Task 14, so the row should render as a container header with no children visible — that's correct for now). Confirm container glyph `▸` appears. Press Enter — glyph flips to `▾`. Press Enter again — back to `▸`. Quit with `q`.

If anything renders broken, fix inline before commit.

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 5: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/dugite-config/src/ui.rs
git commit -m "feat(dugite-config): render container rows and depth indent"
```

---

### Task 12: Right-panel description for sub-leaves

**Files:**
- Modify: `crates/dugite-config/src/ui.rs` — find the right-panel renderer (likely a `render_description_panel` or similar)
- No test (UI prose); manual smoke test

Currently the right panel reads `def: Option<&ParamDef>` and shows the key/type/default/description/hint. For sub-leaves, switch to the `SubParamDef` text; for unknown sub-keys show a "(unknown sub-field)" placeholder.

- [ ] **Step 1: Locate the right-panel renderer**

Run: `grep -n "description\|tuning_hint\|render_right\|render_desc" crates/dugite-config/src/ui.rs`
Expected: locate the function that builds the right-panel paragraphs.

- [ ] **Step 2: Update the renderer to use ItemDef**

For each access to `def.description` / `def.tuning_hint` / `def.param_type.label()`, branch on `item.def`:

```rust
let (key_text, type_label, default_text, description, hint) = match &item.def {
    ItemDef::Top(d) => (
        entry.key.as_str(),
        d.param_type.label(),
        d.default,
        d.description,
        d.tuning_hint,
    ),
    ItemDef::Sub(s) => (
        // Show the dotted path so operators see context.
        // Build the dotted display string once at call site or pass it in.
        dotted_key.as_str(),
        s.param_type.label(),
        s.default,
        s.description,
        s.tuning_hint,
    ),
    ItemDef::Unknown => (
        entry.key.as_str(),
        "unknown",
        "",
        "Sub-field not documented in dugite-config schema. Edited as raw JSON.",
        "",
    ),
};
```

Pass `dotted_key` (`format!("{}.{}", entry.key, item.path.join("."))` for non-empty paths) from the caller.

- [ ] **Step 3: Smoke-test the TUI**

Run: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: navigate to an Object row, expand it (after Tasks 14-16, sub-leaves appear). Cursor on a sub-leaf → right panel shows the sub-leaf's description and tuning_hint, not the parent's.

For Task 12 (before 14-16), at minimum confirm the panel still renders correctly for top-level rows.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p dugite-config && cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/dugite-config/src/ui.rs
git commit -m "feat(dugite-config): right panel shows sub-leaf description"
```

---

### Task 13: Sub-leaf diff output

**Files:**
- Modify: `crates/dugite-config/src/diff.rs`
- Test: same file's tests module

Today `OriginalValues` stores `HashMap<String, String>` of display strings keyed by top-level key. For per-leaf diffs we need the original `Value` so we can `.pointer()` into it. Extend the snapshot to also store `HashMap<String, Value>`.

`compute_diff` walks each modified entry; for Object entries it descends the schema and emits one `DiffEntry` per changed leaf with `key = "Parent.Sub.Leaf"`. Non-Object entries keep today's output.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_diff_emits_per_subleaf_changes() {
    use crate::schema::{ParamType, SubParamDef, Reloadability, ParamDef, KNOWN_PARAMS};

    // Use AcceptedConnectionsLimit's real schema (post Task 14).
    let acl_def: &ParamDef = KNOWN_PARAMS
        .iter()
        .find(|d| d.key == "AcceptedConnectionsLimit")
        .expect("schema entry");

    // Build entries: original Rpc-style object → user changed `hardLimit`.
    let original_value = serde_json::json!({ "hardLimit": 512, "softLimit": 384, "delay": 5.0 });
    let current_value = serde_json::json!({ "hardLimit": 1024, "softLimit": 384, "delay": 5.0 });

    let originals_entries = vec![ConfigEntry {
        key: "AcceptedConnectionsLimit".to_string(),
        value: original_value.clone(),
        modified: false,
        present_in_file: true,
        synthetic_paths: std::collections::HashSet::new(),
    }];
    let snap = OriginalValues::from_entries(&originals_entries);

    let current = vec![ConfigEntry {
        key: "AcceptedConnectionsLimit".to_string(),
        value: current_value,
        modified: true,
        present_in_file: true,
        synthetic_paths: std::collections::HashSet::new(),
    }];

    let diff = compute_diff(&current, &snap);
    let _ = acl_def; // (referenced for context; actual schema lookup is internal to compute_diff)
    assert_eq!(diff.len(), 1, "only hardLimit changed");
    assert_eq!(diff[0].key, "AcceptedConnectionsLimit.hardLimit");
    assert_eq!(diff[0].original, "512");
    assert_eq!(diff[0].current, "1024");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_diff_emits_per_subleaf_changes)'`
Expected: FAIL — current diff emits one `DiffEntry { key: "AcceptedConnectionsLimit", ... }` with the whole object as a string.

- [ ] **Step 3: Extend OriginalValues and compute_diff**

Replace the `OriginalValues` definition and `from_entries`:

```rust
#[derive(Debug, Default)]
pub struct OriginalValues {
    /// Display-string snapshot keyed by top-level entry key. Preserved for
    /// non-Object entries.
    display: HashMap<String, String>,
    /// Full JSON value snapshot keyed by top-level entry key. Used to compute
    /// per-leaf diffs on Object entries.
    values: HashMap<String, Value>,
}

impl OriginalValues {
    pub fn from_entries(entries: &[ConfigEntry]) -> Self {
        let display = entries
            .iter()
            .map(|e| (e.key.clone(), e.display_value()))
            .collect();
        let values = entries
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        OriginalValues { display, values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.display.get(key).map(String::as_str)
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}
```

You'll need `use serde_json::Value;` at the top.

Rewrite `compute_diff`:

```rust
pub fn compute_diff(entries: &[ConfigEntry], originals: &OriginalValues) -> Vec<DiffEntry> {
    use crate::schema::{KNOWN_PARAMS, ParamType};

    let lookup: HashMap<&str, &'static crate::schema::ParamDef> =
        KNOWN_PARAMS.iter().map(|d| (d.key, d)).collect();

    let mut out = Vec::new();
    for entry in entries.iter().filter(|e| e.modified) {
        // Object entry with a schema → recurse for per-leaf lines.
        if let Some(def) = lookup.get(entry.key.as_str()).copied() {
            if let ParamType::Object { fields } = &def.param_type {
                if let Some(orig) = originals.value(&entry.key) {
                    let mut path: Vec<String> = Vec::new();
                    walk_object_diff(&entry.key, orig, &entry.value, fields, &mut path, &mut out);
                    continue;
                }
            }
        }
        // Fallback: top-level diff line.
        let original = originals.get(&entry.key).unwrap_or("").to_string();
        out.push(DiffEntry {
            key: entry.key.clone(),
            original,
            current: entry.display_value(),
        });
    }
    out
}

fn walk_object_diff(
    top_key: &str,
    orig: &Value,
    curr: &Value,
    fields: &[crate::schema::SubParamDef],
    path: &mut Vec<String>,
    out: &mut Vec<DiffEntry>,
) {
    use crate::schema::ParamType;

    let orig_map = orig.as_object();
    let curr_map = curr.as_object();

    // Schema-known sub-fields first.
    for sub in fields {
        path.push(sub.key.to_string());
        match &sub.param_type {
            ParamType::Object { fields: inner } => {
                let orig_child = orig_map.and_then(|m| m.get(sub.key)).unwrap_or(&Value::Null);
                let curr_child = curr_map.and_then(|m| m.get(sub.key)).unwrap_or(&Value::Null);
                walk_object_diff(top_key, orig_child, curr_child, inner, path, out);
            }
            _ => {
                let orig_leaf = orig_map.and_then(|m| m.get(sub.key));
                let curr_leaf = curr_map.and_then(|m| m.get(sub.key));
                if orig_leaf != curr_leaf {
                    let dotted = std::iter::once(top_key.to_string())
                        .chain(path.iter().cloned())
                        .collect::<Vec<_>>()
                        .join(".");
                    out.push(DiffEntry {
                        key: dotted,
                        original: format_leaf(orig_leaf),
                        current: format_leaf(curr_leaf),
                    });
                }
            }
        }
        path.pop();
    }

    // Unknown sub-keys (present in either side but absent from schema).
    let known: std::collections::HashSet<&str> = fields.iter().map(|s| s.key).collect();
    let mut all_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(m) = orig_map { all_keys.extend(m.keys().cloned()); }
    if let Some(m) = curr_map { all_keys.extend(m.keys().cloned()); }
    let mut unknowns: Vec<String> = all_keys.into_iter().filter(|k| !known.contains(k.as_str())).collect();
    unknowns.sort();
    for k in unknowns {
        let o = orig_map.and_then(|m| m.get(&k));
        let c = curr_map.and_then(|m| m.get(&k));
        if o != c {
            path.push(k.clone());
            let dotted = std::iter::once(top_key.to_string())
                .chain(path.iter().cloned())
                .collect::<Vec<_>>()
                .join(".");
            out.push(DiffEntry {
                key: format!("{dotted} [unknown]"),
                original: format_leaf(o),
                current: format_leaf(c),
            });
            path.pop();
        }
    }
}

fn format_leaf(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "(unset)".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}
```

- [ ] **Step 4: Run the new test and the existing diff tests**

Run: `cargo nextest run -p dugite-config -E 'test(diff)'`
Expected: all PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 6: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/dugite-config/src/diff.rs
git commit -m "feat(dugite-config): per-sub-leaf diff entries for Object params"
```

---

### Task 14: Populate AcceptedConnectionsLimit sub-schema

**Files:**
- Modify: `crates/dugite-config/src/schema.rs:914-930`
- Test: same file's tests module

Fill in the 3 sub-fields per spec section F.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_accepted_connections_limit_subschema() {
    let def = KNOWN_PARAMS
        .iter()
        .find(|d| d.key == "AcceptedConnectionsLimit")
        .expect("present");
    match &def.param_type {
        ParamType::Object { fields } => {
            assert_eq!(fields.len(), 3, "expected 3 sub-fields");
            let keys: Vec<&str> = fields.iter().map(|s| s.key).collect();
            assert_eq!(keys, vec!["hardLimit", "softLimit", "delay"]);
            assert!(matches!(fields[0].param_type, ParamType::U64 { .. }));
            assert!(matches!(fields[2].param_type, ParamType::F64 { .. }));
            assert_eq!(fields[0].default, "512");
            assert_eq!(fields[1].default, "384");
            assert_eq!(fields[2].default, "5.0");
        }
        _ => panic!("expected Object"),
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p dugite-config -E 'test(test_accepted_connections_limit_subschema)'`
Expected: FAIL — `fields: &[]`.

- [ ] **Step 3: Populate fields**

Edit `crates/dugite-config/src/schema.rs:914-930`. Add a static array above it:

```rust
const ACCEPTED_CONNECTIONS_LIMIT_FIELDS: &[SubParamDef] = &[
    SubParamDef {
        key: "hardLimit",
        param_type: ParamType::U64 { min: 0, max: 65535 },
        default: "512",
        description: "Maximum concurrent inbound connections. New connections \
                      are refused above this.",
        tuning_hint: "Lower on memory-constrained relays. 512 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "softLimit",
        param_type: ParamType::U64 { min: 0, max: 65535 },
        default: "384",
        description: "Threshold above which new inbound connections are progressively \
                      delayed by up to `delay` seconds.",
        tuning_hint: "Typically 75% of hardLimit.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "delay",
        param_type: ParamType::F64 { min: 0.0, max: 60.0 },
        default: "5.0",
        description: "Maximum delay (seconds) applied to new connections above softLimit. \
                      Linear ramp between softLimit and hardLimit.",
        tuning_hint: "Raise (up to 30s) to slow down aggressive inbound peers.",
        reloadability: Reloadability::Restart,
    },
];
```

Replace `fields: &[]` with `fields: ACCEPTED_CONNECTIONS_LIMIT_FIELDS` for the AcceptedConnectionsLimit `ParamDef`. Also set the parent `default: ""`.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_accepted_connections_limit_subschema)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 6: Smoke-test the TUI**

Run: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: navigate to `AcceptedConnectionsLimit`, press Enter → expands, three sub-rows appear (`hardLimit`, `softLimit`, `delay`) with default values shown muted. Press Enter on `hardLimit` → typing buffer opens. Type `1024`, press Enter → value updates, top-level row marked modified.

- [ ] **Step 7: Run clippy + format and commit**

```bash
cargo clippy -p dugite-config --all-targets -- -D warnings
cargo fmt -- --check
git add crates/dugite-config/src/schema.rs
git commit -m "feat(dugite-config): AcceptedConnectionsLimit sub-schema"
```

---

### Task 15: Populate Rpc sub-schema (with nested Tls)

**Files:**
- Modify: `crates/dugite-config/src/schema.rs:937-962`
- Test: same file's tests module

Same shape as Task 14 but with the nested `Tls` container.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_rpc_subschema_with_tls() {
    let def = KNOWN_PARAMS.iter().find(|d| d.key == "Rpc").expect("present");
    match &def.param_type {
        ParamType::Object { fields } => {
            // Keys in declared order.
            let keys: Vec<&str> = fields.iter().map(|s| s.key).collect();
            assert_eq!(keys, vec![
                "Enabled",
                "ListenAddr",
                "Port",
                "MaxConcurrentStreams",
                "StreamBufferSize",
                "ReflectionEnabled",
                "WebEnabled",
                "AlphaEnabled",
                "Tls",
            ]);

            // Tls is itself an Object.
            let tls = fields.iter().find(|s| s.key == "Tls").unwrap();
            match &tls.param_type {
                ParamType::Object { fields: tls_fields } => {
                    let tls_keys: Vec<&str> = tls_fields.iter().map(|s| s.key).collect();
                    assert_eq!(tls_keys, vec!["CertPath", "KeyPath"]);
                    assert!(matches!(tls_fields[0].param_type, ParamType::Path));
                }
                _ => panic!("Tls must be an Object"),
            }
        }
        _ => panic!("Rpc must be Object"),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p dugite-config -E 'test(test_rpc_subschema_with_tls)'`
Expected: FAIL.

- [ ] **Step 3: Populate fields**

```rust
const RPC_TLS_FIELDS: &[SubParamDef] = &[
    SubParamDef {
        key: "CertPath",
        param_type: ParamType::Path,
        default: "",
        description: "Path to TLS certificate PEM file. Empty = TLS disabled.",
        tuning_hint: "Required if exposing the RPC port off-loopback.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "KeyPath",
        param_type: ParamType::Path,
        default: "",
        description: "Path to TLS private key PEM file.",
        tuning_hint: "Required if CertPath is set.",
        reloadability: Reloadability::Restart,
    },
];

const RPC_FIELDS: &[SubParamDef] = &[
    SubParamDef {
        key: "Enabled",
        param_type: ParamType::Bool,
        default: "false",
        description: "Master switch for the gRPC server. CLI --rpc-host/--rpc-port \
                      force-enable; --no-rpc force-disable.",
        tuning_hint: "Leave off unless serving an integrator/indexer.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "ListenAddr",
        param_type: ParamType::String,
        default: "127.0.0.1",
        description: "Bind IP. 127.0.0.1 (default) keeps the endpoint on loopback.",
        tuning_hint: "Only expose off-loopback with TLS configured.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "Port",
        param_type: ParamType::U64 { min: 1, max: 65535 },
        default: "50051",
        description: "TCP port for gRPC traffic.",
        tuning_hint: "Default 50051 is the gRPC convention.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "MaxConcurrentStreams",
        param_type: ParamType::U64 { min: 1, max: 4096 },
        default: "64",
        description: "HTTP/2 streams-per-connection cap.",
        tuning_hint: "Raise for clients that fan out many subscriptions.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "StreamBufferSize",
        param_type: ParamType::U64 { min: 1, max: 65536 },
        default: "256",
        description: "Per-stream server-side event buffer size.",
        tuning_hint: "Increase if clients see overflow under heavy load.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "ReflectionEnabled",
        param_type: ParamType::Bool,
        default: "true",
        description: "Enable gRPC reflection (useful for grpcurl, evans, etc).",
        tuning_hint: "Disable for hardened deployments.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "WebEnabled",
        param_type: ParamType::Bool,
        default: "false",
        description: "Accept gRPC-Web / HTTP1.1 traffic in addition to native HTTP/2.",
        tuning_hint: "Enable only when serving browser dApps directly.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "AlphaEnabled",
        param_type: ParamType::Bool,
        default: "true",
        description: "Expose v1alpha endpoints alongside v1beta.",
        tuning_hint: "Disable once all integrators have moved to v1beta.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "Tls",
        param_type: ParamType::Object { fields: RPC_TLS_FIELDS },
        default: "",
        description: "Optional TLS termination — set both CertPath and KeyPath.",
        tuning_hint: "Required when binding off-loopback.",
        reloadability: Reloadability::Restart,
    },
];
```

Wire `fields: RPC_FIELDS` into the `Rpc` `ParamDef` and set its parent `default: ""`.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_rpc_subschema)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite + smoke test**

Run: `cargo nextest run -p dugite-config`
Then: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: navigate to `Rpc`, expand → 9 sub-rows with `Tls` shown as nested container. Expand `Tls` → 2 sub-rows. Edit `Rpc.Port` → updates inline. Save (`Ctrl+S`) and re-open → file contains only the edited leaf (others pruned).

- [ ] **Step 6: Run clippy + format and commit**

```bash
cargo clippy -p dugite-config --all-targets -- -D warnings
cargo fmt -- --check
git add crates/dugite-config/src/schema.rs
git commit -m "feat(dugite-config): Rpc sub-schema with nested Tls"
```

---

### Task 16: Populate Storage sub-schema

**Files:**
- Modify: `crates/dugite-config/src/schema.rs:964-983`
- Test: same file's tests module

Includes two leaves with `default: ""` (profile-derived).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_storage_subschema() {
    let def = KNOWN_PARAMS.iter().find(|d| d.key == "Storage").expect("present");
    match &def.param_type {
        ParamType::Object { fields } => {
            let keys: Vec<&str> = fields.iter().map(|s| s.key).collect();
            assert_eq!(keys, vec![
                "profile",
                "immutableIndexType",
                "mmapLoadFactor",
                "utxoBackend",
                "utxoMemtableSizeMb",
                "utxoBlockCacheSizeMb",
                "utxoBloomFilterBits",
            ]);
            // utxoMemtableSizeMb has no schema default.
            let memtable = fields.iter().find(|s| s.key == "utxoMemtableSizeMb").unwrap();
            assert_eq!(memtable.default, "");
            assert!(memtable.default_as_json().is_none(), "U64 with empty default → no hydration");
        }
        _ => panic!("Storage must be Object"),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p dugite-config -E 'test(test_storage_subschema)'`
Expected: FAIL.

- [ ] **Step 3: Populate fields**

```rust
const STORAGE_FIELDS: &[SubParamDef] = &[
    SubParamDef {
        key: "profile",
        param_type: ParamType::Enum {
            values: &["ultra-memory", "high-memory", "low-memory", "minimal"],
        },
        default: "high-memory",
        description: "Preset memory profile. Sets memtable / cache defaults below \
                      unless they are individually overridden.",
        tuning_hint: "Match to host RAM: 'high-memory' for 16GB, 'low-memory' for 8GB.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "immutableIndexType",
        param_type: ParamType::Enum { values: &["mmap", "in-memory"] },
        default: "mmap",
        description: "Storage strategy for the ImmutableDB block index.",
        tuning_hint: "'mmap' is the default; 'in-memory' uses more RAM but is slightly faster.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "mmapLoadFactor",
        param_type: ParamType::F64 { min: 0.0, max: 1.0 },
        default: "0.7",
        description: "Hash-table load factor for the mmap immutable index.",
        tuning_hint: "Lower → more memory, faster lookup. 0.7 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "utxoBackend",
        param_type: ParamType::Enum { values: &["lsm", "in-memory"] },
        default: "lsm",
        description: "UTxO-HD storage backend.",
        tuning_hint: "Production deployments should use 'lsm'.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "utxoMemtableSizeMb",
        param_type: ParamType::U64 { min: 1, max: 65536 },
        default: "",
        description: "LSM memtable size in MB. Empty → derived from profile.",
        tuning_hint: "Override to tune flush cadence vs memory headroom.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "utxoBlockCacheSizeMb",
        param_type: ParamType::U64 { min: 1, max: 65536 },
        default: "",
        description: "LSM block-cache size in MB. Empty → derived from profile.",
        tuning_hint: "Larger cache → fewer disk reads, more RSS.",
        reloadability: Reloadability::Restart,
    },
    SubParamDef {
        key: "utxoBloomFilterBits",
        param_type: ParamType::U64 { min: 1, max: 32 },
        default: "10",
        description: "Bloom-filter bits per key in the LSM SSTables.",
        tuning_hint: "10 ≈ 1% false-positive rate. Increase if profile shows high disk reads.",
        reloadability: Reloadability::Restart,
    },
];
```

Wire `fields: STORAGE_FIELDS` into the `Storage` `ParamDef`. Set its parent `default: ""`.

- [ ] **Step 4: Run the new test and confirm it passes**

Run: `cargo nextest run -p dugite-config -E 'test(test_storage_subschema)'`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite + smoke test**

Run: `cargo nextest run -p dugite-config`
Then: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: Storage row expands to 7 leaves; `utxoMemtableSizeMb` and `utxoBlockCacheSizeMb` appear only when set in the file (otherwise no row). Enum cycle works on `profile` (Enter cycles ultra-memory → high-memory → low-memory → minimal → wrap).

- [ ] **Step 6: Run clippy + format and commit**

```bash
cargo clippy -p dugite-config --all-targets -- -D warnings
cargo fmt -- --check
git add crates/dugite-config/src/schema.rs
git commit -m "feat(dugite-config): Storage sub-schema"
```

---

### Task 17: config_coverage invariants for sub-schemas

**Files:**
- Modify: `crates/dugite-config/tests/config_coverage.rs`
- Test: this file itself

Add two invariants:

1. Every `ParamType::Object` `ParamDef` has non-empty `fields` (catches forgetting to populate when adding a new Object).
2. Every `SubParamDef` whose `default` is non-empty parses successfully via `default_as_json`.

- [ ] **Step 1: Read the existing test file**

Run: `wc -l crates/dugite-config/tests/config_coverage.rs && head -40 crates/dugite-config/tests/config_coverage.rs`
Expected: shows the file structure and existing imports.

- [ ] **Step 2: Add the failing tests (they'll PASS — Tasks 14-16 already done)**

Append to `crates/dugite-config/tests/config_coverage.rs`:

```rust
use dugite_config::schema::{KNOWN_PARAMS, ParamType, SubParamDef};

fn walk_subfields<F: FnMut(&SubParamDef, &[&str])>(
    fields: &[SubParamDef],
    parent_path: &mut Vec<&'static str>,
    visit: &mut F,
) {
    for sub in fields {
        parent_path.push(sub.key);
        let path_view: Vec<&str> = parent_path.iter().copied().collect();
        visit(sub, &path_view);
        if let ParamType::Object { fields: inner } = &sub.param_type {
            walk_subfields(inner, parent_path, visit);
        }
        parent_path.pop();
    }
}

#[test]
fn every_object_param_has_non_empty_fields() {
    for def in KNOWN_PARAMS {
        if let ParamType::Object { fields } = &def.param_type {
            assert!(
                !fields.is_empty(),
                "Object param '{}' has empty fields — populate sub-schema",
                def.key
            );
        }
    }
}

#[test]
fn every_subfield_default_is_parseable() {
    for def in KNOWN_PARAMS {
        if let ParamType::Object { fields } = &def.param_type {
            let mut path: Vec<&'static str> = vec![def.key];
            walk_subfields(fields, &mut path, &mut |sub, full_path| {
                // Empty default is the explicit "no hydration" signal — skip.
                if sub.default.is_empty()
                    && matches!(
                        sub.param_type,
                        ParamType::U64 { .. }
                            | ParamType::F64 { .. }
                            | ParamType::Bool
                            | ParamType::Enum { .. }
                    )
                {
                    return;
                }
                assert!(
                    sub.default_as_json().is_some(),
                    "sub-field {:?} has unparseable default '{}'",
                    full_path,
                    sub.default
                );
            });
        }
    }
}
```

You may need to expose `SubParamDef` and helpers from the crate root. Check `crates/dugite-config/src/lib.rs` (or `main.rs`) — if `schema` is a private mod, add `pub mod schema;` so the integration test can reach it. If there's no `lib.rs` (binary-only), create one:

```rust
// crates/dugite-config/src/lib.rs
pub mod schema;
pub mod config;
pub mod path;
pub mod app;
pub mod diff;
pub mod search;
// (mirror everything `main.rs` declares)
```

And in `crates/dugite-config/Cargo.toml`, ensure both `[[bin]]` and `[lib]` are declared:

```toml
[lib]
name = "dugite_config"
path = "src/lib.rs"

[[bin]]
name = "dugite-config"
path = "src/main.rs"
```

The binary's `main.rs` then becomes `use dugite_config::{schema, app, ui, ...};` plus the existing `fn main()`.

If this restructure is too invasive, move the coverage tests *inside* `tests/` files that use `include_str!` and inline-include the schema source — or simpler still: keep the tests as `#[cfg(test)]` modules inside `schema.rs` next to the existing `test_subparam_default_as_json_recurses`. Choose whichever is least invasive to existing structure.

- [ ] **Step 3: Run the new tests**

Run: `cargo nextest run -p dugite-config -E 'test(every_object_param_has_non_empty_fields) | test(every_subfield_default_is_parseable)'`
Expected: PASS.

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo nextest run -p dugite-config`
Expected: all tests pass.

- [ ] **Step 5: Run clippy + format**

Run: `cargo clippy -p dugite-config --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/dugite-config/tests/config_coverage.rs
# also Cargo.toml / lib.rs if you did the restructure:
git status --short
git commit -m "test(dugite-config): coverage invariants for Object sub-schemas"
```

---

### Task 18: Final integration test and CI gate

**Files:**
- No new code changes. Run the full workspace gate as the project's `just check` recipe.

- [ ] **Step 1: Run the workspace gate**

Run: `just check`
(If not available: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo build --all-targets && cargo nextest run --workspace`.)
Expected: PASS.

- [ ] **Step 2: Manual smoke test on a real config**

Run: `cargo run -p dugite-config -- --config config/preview/config.json`
Expected: smoke run through every flow:
1. Start. All sections expanded; all Object headers (`AcceptedConnectionsLimit`, `Rpc`, `Storage`) shown collapsed with `▸ `.
2. Cursor onto `AcceptedConnectionsLimit`, press Enter → expands to `▾ ` with three sub-rows visible.
3. Press Enter on `hardLimit` → typing buffer opens with `512`. Type `1024`, press Enter → value updates, top-level row marked modified.
4. Move down to `Rpc`, expand. Move down into `Tls`, expand. Both should show their leaves.
5. Press `Ctrl+D` → diff overlay shows `AcceptedConnectionsLimit.hardLimit: 512 → 1024`.
6. Press `/` and type `port` → search includes `Rpc.Port` row.
7. Press `Ctrl+S` to save. Re-open. File only contains `"AcceptedConnectionsLimit": {"hardLimit": 1024}` (other leaves pruned). Restore the original preview config from `.bak` if you don't want to leave the change in your tree.
8. Quit with `q`.

If any flow breaks, stop and fix in a new sub-task before proceeding.

- [ ] **Step 3: Commit the final state**

If any tiny fix slipped in during smoke testing:

```bash
git status --short
git add -p   # selectively stage
git commit -m "fix(dugite-config): post-integration tweaks for Object sub-fields"
```

If nothing changed, this task is a no-op.

- [ ] **Step 4: Push the branch**

```bash
git log --oneline -20   # confirm the task chain
git push
```

---

## Self-review checklist

- [x] **Spec coverage** — every section A-H has at least one mapping task. Section A → Task 1; B → Tasks 3-5; C → Tasks 6-7; D → Tasks 8-9; E → Task 13; F → Tasks 14-16; G → tests inline in 1-16 + Task 17; H is non-goals (no task).
- [x] **No placeholders** — every step shows the exact code/command/expected output.
- [x] **Type consistency** — `Item`, `ItemDef`, `SubParamDef`, `synthetic_paths`, `apply_edit_at`, `display_value_at`, `toggle_bool_at`, `cycle_enum_at` names are stable across tasks.
- [x] **Sequencing note** — Tasks 9, 10, 13 reference behavior that depends on Tasks 14-16 having populated schemas. The plan calls this out inline; the implementer can either land 14-16 first (recommended) or carry a stub schema for the test.
- [x] **Commit cadence** — every task ends with a commit. No mega-commits spanning multiple concerns.
