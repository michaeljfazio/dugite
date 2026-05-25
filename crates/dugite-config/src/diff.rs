//! Diff computation between the original and current config values.
//!
//! The diff view (`Ctrl+D`) shows only the parameters that have been changed
//! in the current editing session — the original (loaded) value on the left
//! and the new value on the right.
//!
//! # Data model
//!
//! [`DiffEntry`] pairs an original value with the current value for a single
//! key that has been modified.  [`compute_diff`] walks [`LoadedConfig`] and
//! collects all entries where `modified == true`, recording the original value
//! from the backup snapshot created at load time.
//!
//! Because the original value is not stored separately in [`ConfigEntry`] (the
//! entry is mutated in place by edits), the diff tracks it via a separate
//! [`OriginalValues`] snapshot that [`App`] captures at construction time.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::ConfigEntry;

// ---------------------------------------------------------------------------
// Original-values snapshot
// ---------------------------------------------------------------------------

/// A snapshot of original config values captured at config-load time.
///
/// This is built once when the [`App`] is constructed and never modified
/// again.  It is the ground truth for the "before" side of every diff.
///
/// The snapshot stores two parallel maps:
/// - `display`: display-string form keyed by top-level entry key (for
///   non-Object entries and fallback rendering).
/// - `values`: full [`Value`] snapshot keyed by top-level entry key (used to
///   walk Object sub-fields for per-leaf diff computation).
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
    /// Build a snapshot from the loaded entries.
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

    /// Return the original display value for `key`, or `None` if not found.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.display.get(key).map(String::as_str)
    }

    /// Return the original full JSON value for `key`, or `None` if not found.
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

// ---------------------------------------------------------------------------
// Diff entry
// ---------------------------------------------------------------------------

/// A single changed parameter in the diff view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    /// The JSON key.
    pub key: String,
    /// Value as it was when the file was loaded.
    pub original: String,
    /// Value after user edits.
    pub current: String,
}

// ---------------------------------------------------------------------------
// Compute diff
// ---------------------------------------------------------------------------

/// Collect all modified entries into a list of [`DiffEntry`]s.
///
/// Entries are returned in the same order as they appear in `entries`.
/// Only entries with `modified == true` are included.
///
/// For entries whose schema declares [`ParamType::Object`], one [`DiffEntry`]
/// is emitted per changed sub-leaf using dotted keys (e.g.
/// `"AcceptedConnectionsLimit.hardLimit"`).  For all other entries the
/// existing top-level single-line behaviour is preserved.
pub fn compute_diff(entries: &[ConfigEntry], originals: &OriginalValues) -> Vec<DiffEntry> {
    use crate::schema::{ParamType, KNOWN_PARAMS};

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
        // Fallback: top-level diff line (existing behaviour).
        let original = originals.get(&entry.key).unwrap_or("").to_string();
        out.push(DiffEntry {
            key: entry.key.clone(),
            original,
            current: entry.display_value(),
        });
    }
    out
}

/// Recursively walk the sub-fields of an Object entry, emitting one
/// [`DiffEntry`] per changed leaf.
///
/// `path` is the mutable scratch buffer of sub-key segments visited so far
/// (not including `top_key`).  It is push/popped as the recursion descends.
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
                let orig_child = orig_map
                    .and_then(|m| m.get(sub.key))
                    .unwrap_or(&Value::Null);
                let curr_child = curr_map
                    .and_then(|m| m.get(sub.key))
                    .unwrap_or(&Value::Null);
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

    // Unknown sub-keys: present in either side but absent from schema.
    let known: std::collections::HashSet<&str> = fields.iter().map(|s| s.key).collect();
    let mut all_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(m) = orig_map {
        all_keys.extend(m.keys().cloned());
    }
    if let Some(m) = curr_map {
        all_keys.extend(m.keys().cloned());
    }
    let mut unknowns: Vec<String> = all_keys
        .into_iter()
        .filter(|k| !known.contains(k.as_str()))
        .collect();
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

/// Format a single JSON leaf value for diff display.
fn format_leaf(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "(unset)".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn make_entry(key: &str, value: Value, modified: bool) -> ConfigEntry {
        use std::collections::HashSet;
        ConfigEntry {
            key: key.to_string(),
            value,
            modified,
            present_in_file: true,
            synthetic_paths: HashSet::new(),
        }
    }

    #[test]
    fn test_original_values_captures_at_load() {
        let entries = vec![
            make_entry("EnableP2P", Value::Bool(true), false),
            make_entry("NetworkMagic", Value::Number(2.into()), false),
        ];
        let snap = OriginalValues::from_entries(&entries);
        assert_eq!(snap.get("EnableP2P"), Some("true"));
        assert_eq!(snap.get("NetworkMagic"), Some("2"));
    }

    #[test]
    fn test_compute_diff_returns_only_modified() {
        let originals = {
            let entries = vec![
                make_entry("EnableP2P", Value::Bool(true), false),
                make_entry("MinSeverity", Value::String("Info".into()), false),
            ];
            OriginalValues::from_entries(&entries)
        };

        // Simulate in-memory edits.
        let current = vec![
            make_entry("EnableP2P", Value::Bool(false), true), // changed
            make_entry("MinSeverity", Value::String("Info".into()), false), // unchanged
        ];

        let diff = compute_diff(&current, &originals);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].key, "EnableP2P");
        assert_eq!(diff[0].original, "true");
        assert_eq!(diff[0].current, "false");
    }

    #[test]
    fn test_compute_diff_empty_when_no_changes() {
        let originals = {
            let entries = vec![make_entry("EnableP2P", Value::Bool(true), false)];
            OriginalValues::from_entries(&entries)
        };
        let current = vec![make_entry("EnableP2P", Value::Bool(true), false)];
        let diff = compute_diff(&current, &originals);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_compute_diff_multiple_changes() {
        let originals = {
            let entries = vec![
                make_entry("EnableP2P", Value::Bool(true), false),
                make_entry("MinSeverity", Value::String("Info".into()), false),
                make_entry("NetworkMagic", Value::Number(2.into()), false),
            ];
            OriginalValues::from_entries(&entries)
        };

        let current = vec![
            make_entry("EnableP2P", Value::Bool(false), true),
            make_entry("MinSeverity", Value::String("Warning".into()), true),
            make_entry("NetworkMagic", Value::Number(2.into()), false), // not changed
        ];

        let diff = compute_diff(&current, &originals);
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].key, "EnableP2P");
        assert_eq!(diff[1].key, "MinSeverity");
        assert_eq!(diff[1].original, "Info");
        assert_eq!(diff[1].current, "Warning");
    }

    #[test]
    fn test_diff_entry_equality() {
        let a = DiffEntry {
            key: "k".into(),
            original: "old".into(),
            current: "new".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_diff_emits_per_subleaf_changes() {
        // Use AcceptedConnectionsLimit's real schema (post Task 14).
        let original_value =
            serde_json::json!({ "hardLimit": 512, "softLimit": 384, "delay": 5.0 });
        let current_value =
            serde_json::json!({ "hardLimit": 1024, "softLimit": 384, "delay": 5.0 });

        let originals_entries = vec![ConfigEntry {
            key: "AcceptedConnectionsLimit".to_string(),
            value: original_value,
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
        assert_eq!(diff.len(), 1, "only hardLimit changed");
        assert_eq!(diff[0].key, "AcceptedConnectionsLimit.hardLimit");
        assert_eq!(diff[0].original, "512");
        assert_eq!(diff[0].current, "1024");
    }

    #[test]
    fn test_diff_emits_two_subleaf_changes() {
        let original_value =
            serde_json::json!({ "hardLimit": 512, "softLimit": 384, "delay": 5.0 });
        let current_value =
            serde_json::json!({ "hardLimit": 1024, "softLimit": 500, "delay": 5.0 });

        let originals_entries = vec![ConfigEntry {
            key: "AcceptedConnectionsLimit".to_string(),
            value: original_value,
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
        assert_eq!(diff.len(), 2, "hardLimit and softLimit changed");
        let keys: Vec<&str> = diff.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains(&"AcceptedConnectionsLimit.hardLimit"));
        assert!(keys.contains(&"AcceptedConnectionsLimit.softLimit"));
    }

    #[test]
    fn test_diff_preserves_unknown_subkey_changes() {
        let original_value =
            serde_json::json!({ "hardLimit": 512, "softLimit": 384, "delay": 5.0 });
        let current_value = serde_json::json!({
            "hardLimit": 512,
            "softLimit": 384,
            "delay": 5.0,
            "NewFeature": "added"
        });

        let originals_entries = vec![ConfigEntry {
            key: "AcceptedConnectionsLimit".to_string(),
            value: original_value,
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
        assert_eq!(diff.len(), 1, "only NewFeature should appear");
        assert!(diff[0].key.contains("NewFeature"));
        assert!(
            diff[0].key.contains("[unknown]"),
            "unknown sub-key should be tagged"
        );
    }
}
