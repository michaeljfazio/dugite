//! Config file I/O — load, save, and backup Cardano node configuration files.
//!
//! The Cardano node configuration format is a flat JSON object (no nested
//! sections) where every key is a top-level string and values are booleans,
//! integers, or strings.  This module reads the file into a
//! [`serde_json::Value`] and exposes a typed view used by the TUI.
//!
//! # Backup strategy
//!
//! Before every save, the original file is copied to `<path>.bak`.  Only one
//! level of backup is kept — the previous `.bak` is silently overwritten.
//!
//! # Pretty-print format
//!
//! Saved files use 4-space indentation and a trailing newline, matching the
//! format used by the official Cardano config files.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::path::path_to_json_pointer;
use crate::schema::ParamType;
use crate::schema::{SubParamDef, KNOWN_PARAMS};

// ---------------------------------------------------------------------------
// Flat key-value entry (the TUI's working unit)
// ---------------------------------------------------------------------------

/// A single key-value pair extracted from the top-level JSON object, or
/// synthesised from the schema's default for a key the file does not pin.
///
/// The TUI works exclusively with this type — it never manipulates the raw
/// JSON `Value` tree directly after the initial parse.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    /// The JSON key exactly as found in the file (or the schema key for
    /// synthetic entries).
    pub key: String,
    /// Current value as a JSON `Value`.
    pub value: Value,
    /// Whether this entry has been modified since the file was loaded.
    pub modified: bool,
    /// True if this entry came from the file on disk; false if it was
    /// synthesised from the schema's default because the key was absent.
    /// Synthetic entries are only persisted when their value differs from
    /// the schema default.
    pub present_in_file: bool,
    /// For Object entries: JSON-pointer paths (e.g. "/Tls/CertPath") that were
    /// synthesised during `inject_schema_defaults`. Empty for non-Object
    /// entries and for sub-keys that were present in the on-disk file.
    /// Used by `save_config` to decide which synthetic leaves to prune.
    pub synthetic_paths: std::collections::HashSet<String>,
}

impl ConfigEntry {
    /// Return the current value formatted as a concise display string.
    ///
    /// - Booleans render as `true` / `false`.
    /// - Numbers render without quotes.
    /// - Strings render without surrounding quotes.
    /// - Anything else renders as compact JSON.
    pub fn display_value(&self) -> String {
        match &self.value {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// Apply a string edit to this entry's value, coercing to the appropriate
    /// JSON type based on the existing value type.
    ///
    /// - Existing bool: parses "true"/"false".
    /// - Existing number: tries integer parse then float.
    /// - Existing string: stores as-is.
    /// - Other: stores as a JSON string.
    ///
    /// Returns `Err` if the parse fails.
    pub fn apply_edit(&mut self, raw: &str) -> Result<()> {
        let new_value = match &self.value {
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
        self.value = new_value;
        self.modified = true;
        Ok(())
    }

    /// Toggle a boolean value in place.
    ///
    /// Returns `Err` if the current value is not a boolean.
    pub fn toggle_bool(&mut self) -> Result<()> {
        match &self.value {
            Value::Bool(b) => {
                self.value = Value::Bool(!b);
                self.modified = true;
                Ok(())
            }
            _ => anyhow::bail!("cannot toggle non-boolean value"),
        }
    }

    /// Cycle an enum value forward through the provided list of choices.
    ///
    /// If the current value is not in `choices`, it is set to `choices[0]`.
    pub fn cycle_enum(&mut self, choices: &[&str]) {
        if choices.is_empty() {
            return;
        }
        let current = self.display_value();
        let next = choices
            .iter()
            .position(|c| *c == current.as_str())
            .map(|i| choices[(i + 1) % choices.len()])
            .unwrap_or(choices[0]);
        self.value = Value::String(next.to_string());
        self.modified = true;
    }

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
        let Some(slot) = self.value.pointer_mut(&pointer) else {
            return;
        };
        let current = match slot {
            Value::String(s) => s.clone(),
            ref other => other.to_string(),
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
}

// ---------------------------------------------------------------------------
// Object sub-field hydration
// ---------------------------------------------------------------------------

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
            ParamType::Object {
                fields: inner_fields,
            } => {
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

// ---------------------------------------------------------------------------
// Synthetic-default prune walker
// ---------------------------------------------------------------------------

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
            ParamType::Object {
                fields: inner_fields,
            } => {
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

// ---------------------------------------------------------------------------
// Loaded config
// ---------------------------------------------------------------------------

/// The full config file loaded into memory as an ordered list of entries.
///
/// Order is preserved from the original file so that save round-trips produce
/// minimal diffs.
#[derive(Debug)]
pub struct LoadedConfig {
    /// Absolute path of the file on disk.
    pub path: PathBuf,
    /// All key-value entries in file order.
    pub entries: Vec<ConfigEntry>,
}

impl LoadedConfig {
    /// Return `true` if any entry has been modified since load (or last save).
    pub fn is_modified(&self) -> bool {
        self.entries.iter().any(|e| e.modified)
    }

    /// Clear the `modified` flag on every entry.
    pub fn mark_clean(&mut self) {
        for entry in &mut self.entries {
            entry.modified = false;
        }
    }

    /// Append a synthetic entry for every schema parameter not already present
    /// in the file, using the schema's documented default value. Synthetic
    /// entries carry `present_in_file: false` so [`save_config`] can omit them
    /// when their value matches the default — preserving minimal file diffs
    /// while letting the TUI surface every parameter for editing.
    ///
    /// Parameters whose schema default cannot be represented (no default
    /// available for the parameter's type) are skipped.
    pub fn inject_schema_defaults(&mut self) {
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
                hydrate_object(
                    &mut entry.value,
                    fields,
                    &mut path,
                    &mut entry.synthetic_paths,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Load a Cardano node configuration JSON file from `path`.
///
/// The file must be a JSON object (`{...}`) at its top level.  All top-level
/// keys are extracted in iteration order (which, for `serde_json`, is
/// insertion/file order when using the `preserve_order` feature — but
/// standard `serde_json::Map` also iterates alphabetically in the absence of
/// that feature; either way every key is captured).
pub fn load_config(path: &Path) -> Result<LoadedConfig> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading config file '{}'", path.display()))?;

    let json: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing config file '{}' as JSON", path.display()))?;

    let obj = json.as_object().with_context(|| {
        format!(
            "config file '{}' must be a JSON object at the top level",
            path.display()
        )
    })?;

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

    Ok(LoadedConfig {
        path: path.to_path_buf(),
        entries,
    })
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// Save `config` back to its original file path.
///
/// Steps:
/// 1. Copy the current file to `<path>.bak` (overwriting any existing backup).
/// 2. Reconstruct a JSON object from the entry list (preserving order).
/// 3. Pretty-print with 4-space indent and a trailing newline.
/// 4. Write atomically via a temp file in the same directory then rename.
///
/// If the backup or write fails, the original file is left untouched.
pub fn save_config(config: &mut LoadedConfig) -> Result<()> {
    let path = config.path.clone();

    // Step 1 — backup.
    backup_file(&path)?;

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

    // Step 3 — pretty-print.
    let mut out = serde_json::to_string_pretty(&json).context("serialising config to JSON")?;
    out.push('\n'); // trailing newline

    // Step 4 — atomic write via a unique-name temp file in the same
    // directory, then atomic rename onto the target.
    //
    // `NamedTempFile::new_in(dir)` produces an OS-randomised path so
    // concurrent `save_config` calls (across cargo-test workers sharing
    // `/tmp` and, in principle, two long-running dugite-config instances
    // editing different files on the same volume) do not clobber each
    // other's temp file before the rename — observed as a CI flake in
    // `test_save_*` (2026-05-17).  The previous hardcoded
    // `.dugite-config.tmp` filename meant the second writer overwrote
    // the first, then the first's rename ENOENT'd.
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = NamedTempFile::new_in(dir)
        .with_context(|| format!("creating temp file for atomic write in '{}'", dir.display()))?;
    tmp.write_all(out.as_bytes())
        .with_context(|| format!("writing temp file '{}'", tmp.path().display()))?;
    tmp.as_file_mut()
        .sync_all()
        .with_context(|| format!("fsync temp file '{}'", tmp.path().display()))?;
    tmp.persist(&path)
        .map_err(|e| e.error)
        .with_context(|| format!("renaming temp file to '{}'", path.display()))?;

    // Mark all entries clean now that the file is on disk.
    config.mark_clean();

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Copy `path` to `<path>.bak`, silently overwriting any existing backup.
fn backup_file(path: &Path) -> Result<()> {
    // If the file does not exist yet there is nothing to back up.
    if !path.exists() {
        return Ok(());
    }
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    fs::copy(path, &bak).with_context(|| format!("creating backup '{}'", bak.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_load_simple_config() {
        let f = write_temp(r#"{"EnableP2P": true, "NetworkMagic": 2}"#);
        let config = load_config(f.path()).unwrap();
        assert_eq!(config.entries.len(), 2);
        assert_eq!(config.entries[0].key, "EnableP2P");
        assert_eq!(config.entries[0].value, Value::Bool(true));
        assert_eq!(config.entries[1].key, "NetworkMagic");
        assert_eq!(config.entries[1].value, Value::Number(2.into()));
    }

    #[test]
    fn test_load_rejects_non_object() {
        let f = write_temp(r#"[1, 2, 3]"#);
        assert!(load_config(f.path()).is_err());
    }

    #[test]
    fn test_load_rejects_invalid_json() {
        let f = write_temp(r#"{invalid"#);
        assert!(load_config(f.path()).is_err());
    }

    #[test]
    fn test_display_value_formats() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::Bool(true),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        assert_eq!(entry.display_value(), "true");

        entry.value = Value::Number(42.into());
        assert_eq!(entry.display_value(), "42");

        entry.value = Value::String("hello".into());
        assert_eq!(entry.display_value(), "hello");
    }

    #[test]
    fn test_apply_edit_bool() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::Bool(true),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        entry.apply_edit("false").unwrap();
        assert_eq!(entry.value, Value::Bool(false));
        assert!(entry.modified);
    }

    #[test]
    fn test_apply_edit_number() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::Number(1.into()),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        entry.apply_edit("99").unwrap();
        assert_eq!(entry.value, Value::Number(99.into()));
        assert!(entry.modified);
    }

    #[test]
    fn test_apply_edit_string() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::String("old".into()),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        entry.apply_edit("new").unwrap();
        assert_eq!(entry.value, Value::String("new".into()));
        assert!(entry.modified);
    }

    #[test]
    fn test_toggle_bool() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::Bool(false),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        entry.toggle_bool().unwrap();
        assert_eq!(entry.value, Value::Bool(true));
        assert!(entry.modified);
    }

    #[test]
    fn test_toggle_bool_on_non_bool_errors() {
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::Number(1.into()),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        assert!(entry.toggle_bool().is_err());
    }

    #[test]
    fn test_cycle_enum() {
        let choices = ["A", "B", "C"];
        let mut entry = ConfigEntry {
            key: "k".into(),
            value: Value::String("A".into()),
            modified: false,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        };
        entry.cycle_enum(&choices);
        assert_eq!(entry.display_value(), "B");
        entry.cycle_enum(&choices);
        assert_eq!(entry.display_value(), "C");
        entry.cycle_enum(&choices);
        assert_eq!(entry.display_value(), "A"); // wraps
    }

    #[test]
    fn test_is_modified_and_mark_clean() {
        let f = write_temp(r#"{"k": true}"#);
        let mut config = load_config(f.path()).unwrap();
        assert!(!config.is_modified());
        config.entries[0].modified = true;
        assert!(config.is_modified());
        config.mark_clean();
        assert!(!config.is_modified());
    }

    #[test]
    fn test_save_roundtrip() {
        let f = write_temp(r#"{"EnableP2P": true, "NetworkMagic": 2}"#);
        let path = f.path().to_path_buf();
        // Keep the NamedTempFile alive but we need to persist it.
        let persist = f.into_temp_path();

        let mut config = load_config(&path).unwrap();
        config.entries[0].toggle_bool().unwrap(); // EnableP2P -> false
        save_config(&mut config).unwrap();

        // Reload and verify.
        let config2 = load_config(&path).unwrap();
        assert_eq!(config2.entries[0].value, Value::Bool(false));
        assert_eq!(config2.entries[1].value, Value::Number(2.into()));
        assert!(!config2.is_modified());

        // Backup should exist.
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(bak.exists());

        // Cleanup.
        let _ = std::fs::remove_file(&bak);
        drop(persist);
    }

    #[test]
    fn test_inject_schema_defaults_adds_missing_keys() {
        // Minimal file with only one key; injection should add many schema keys.
        let f = write_temp(r#"{"MinSeverity": "Info"}"#);
        let mut config = load_config(f.path()).unwrap();
        let original_len = config.entries.len();
        assert_eq!(original_len, 1);

        config.inject_schema_defaults();
        assert!(
            config.entries.len() > original_len,
            "injection should add synthetic entries"
        );

        // The file-loaded entry retains its provenance.
        let min_sev = config
            .entries
            .iter()
            .find(|e| e.key == "MinSeverity")
            .unwrap();
        assert!(min_sev.present_in_file);
        assert!(!min_sev.modified);

        // A synthetic entry takes the schema default and is flagged not-present.
        let protocol = config.entries.iter().find(|e| e.key == "Protocol").unwrap();
        assert!(!protocol.present_in_file);
        assert!(!protocol.modified);
        assert_eq!(protocol.value, Value::String("Cardano".into()));
    }

    #[test]
    fn test_inject_schema_defaults_is_idempotent() {
        let f = write_temp(r#"{"MinSeverity": "Info"}"#);
        let mut config = load_config(f.path()).unwrap();
        config.inject_schema_defaults();
        let after_first = config.entries.len();
        config.inject_schema_defaults();
        assert_eq!(after_first, config.entries.len());
    }

    #[test]
    fn test_save_skips_unmodified_synthetic_entries() {
        // Pre-injection file has only MinSeverity; after injection many synthetic
        // entries exist but none have been touched. Save must not bloat the file.
        let f = write_temp(r#"{"MinSeverity": "Info"}"#);
        let path = f.path().to_path_buf();
        let persist = f.into_temp_path();

        let mut config = load_config(&path).unwrap();
        config.inject_schema_defaults();
        save_config(&mut config).unwrap();

        // Reload from disk and confirm only the original key persists.
        let reloaded = load_config(&path).unwrap();
        assert_eq!(reloaded.entries.len(), 1);
        assert_eq!(reloaded.entries[0].key, "MinSeverity");

        let bak = PathBuf::from(format!("{}.bak", path.display()));
        let _ = std::fs::remove_file(&bak);
        drop(persist);
    }

    #[test]
    fn test_save_persists_synthetic_entry_when_value_diverges() {
        let f = write_temp(r#"{"MinSeverity": "Info"}"#);
        let path = f.path().to_path_buf();
        let persist = f.into_temp_path();

        let mut config = load_config(&path).unwrap();
        config.inject_schema_defaults();

        // Mutate a synthetic entry away from its default.
        let protocol = config
            .entries
            .iter_mut()
            .find(|e| e.key == "Protocol")
            .unwrap();
        assert!(!protocol.present_in_file);
        protocol.apply_edit("TPraos").unwrap();

        save_config(&mut config).unwrap();

        let reloaded = load_config(&path).unwrap();
        let protocol_on_disk = reloaded.entries.iter().find(|e| e.key == "Protocol");
        assert!(protocol_on_disk.is_some(), "drifted entry should be saved");
        assert_eq!(
            protocol_on_disk.unwrap().value,
            Value::String("TPraos".into())
        );

        let bak = PathBuf::from(format!("{}.bak", path.display()));
        let _ = std::fs::remove_file(&bak);
        drop(persist);
    }

    #[test]
    fn test_save_drops_synthetic_entry_reverted_to_default() {
        // Touching a synthetic entry but landing back on the default value must
        // not bloat the file — the "minimal diff" guarantee.
        let f = write_temp(r#"{"MinSeverity": "Info"}"#);
        let path = f.path().to_path_buf();
        let persist = f.into_temp_path();

        let mut config = load_config(&path).unwrap();
        config.inject_schema_defaults();

        let protocol = config
            .entries
            .iter_mut()
            .find(|e| e.key == "Protocol")
            .unwrap();
        protocol.apply_edit("TPraos").unwrap();
        protocol.apply_edit("Cardano").unwrap(); // back to default

        save_config(&mut config).unwrap();
        let reloaded = load_config(&path).unwrap();
        assert!(reloaded.entries.iter().all(|e| e.key != "Protocol"));

        let bak = PathBuf::from(format!("{}.bak", path.display()));
        let _ = std::fs::remove_file(&bak);
        drop(persist);
    }

    #[test]
    fn test_config_entry_has_empty_synthetic_paths_by_default() {
        let f = write_temp(r#"{"EnableP2P": true}"#);
        let config = load_config(f.path()).unwrap();
        assert!(config.entries[0].synthetic_paths.is_empty());
    }

    #[test]
    fn test_inject_hydrates_object_subfields_when_object_empty() {
        use crate::schema::{ParamType, Reloadability, SubParamDef};
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
        use crate::schema::{ParamType, Reloadability, SubParamDef};
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
        use crate::schema::{ParamType, Reloadability, SubParamDef};
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
        assert!(
            paths.is_empty(),
            "user-provided value must not be synthetic"
        );
    }

    #[test]
    fn test_inject_recurses_into_nested_object() {
        use crate::schema::{ParamType, Reloadability, SubParamDef};
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
        assert!(
            paths.contains("/inner"),
            "intermediate object node is synthetic"
        );
        assert!(paths.contains("/inner/y"));
    }

    #[test]
    fn test_save_prunes_synthetic_default_subleaf() {
        use crate::schema::{ParamType, Reloadability, SubParamDef};
        use std::collections::HashSet;

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

        let mut value = serde_json::json!({ "a": 7, "b": 42 });
        let paths: HashSet<String> = ["/a", "/b"].iter().map(|s| s.to_string()).collect();
        prune_synthetic_defaults(&mut value, FIELDS, &mut Vec::new(), &paths);

        // 'a' is synthetic + still default → pruned. 'b' is synthetic but not default → kept.
        assert_eq!(value, serde_json::json!({ "b": 42 }));
    }

    #[test]
    fn test_save_prunes_keeps_user_set_leaf_even_if_at_default() {
        use crate::schema::{ParamType, Reloadability, SubParamDef};

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
        use crate::schema::{ParamType, Reloadability, SubParamDef};
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
        let paths: HashSet<String> = ["/inner", "/inner/y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        prune_synthetic_defaults(&mut value, OUTER, &mut Vec::new(), &paths);

        // Inner y is synthetic + default → pruned. Inner is now empty + synthetic → pruned too.
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn test_save_prunes_keeps_unknown_subkey() {
        use crate::schema::{ParamType, Reloadability, SubParamDef};
        use std::collections::HashSet;

        const FIELDS: &[SubParamDef] = &[SubParamDef {
            key: "a",
            param_type: ParamType::U64 { min: 0, max: 100 },
            default: "7",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        }];

        let mut value = serde_json::json!({ "a": 7, "NewFeature": "preserve me" });
        let paths: HashSet<String> = ["/a"].iter().map(|s| s.to_string()).collect();
        prune_synthetic_defaults(&mut value, FIELDS, &mut Vec::new(), &paths);

        assert_eq!(value, serde_json::json!({ "NewFeature": "preserve me" }));
    }
}
