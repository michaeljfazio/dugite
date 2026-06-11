//! Lightweight checkpoints loader (cardano-node `CheckpointsFile`).
//!
//! Parses the JSON
//! `{"checkpoints":[{"blockNo":N,"hash":hex},...]}` into a
//! `block_number → header-hash` map, optionally pinned by a Blake2b-256
//! hash of the raw file bytes (`CheckpointsFileHash`). Duplicate block
//! numbers are a parse error (cardano-node `Cardano.Node.Protocol.Checkpoints`).
//!
//! The resulting map is installed on `OuroborosPraos` and enforced in
//! `validate_header` for every header in BOTH consensus modes — exactly
//! cardano-node's `validateIfCheckpoint` semantics (after the block-number
//! and slot envelope checks, before additional checks).

use std::collections::HashMap;
use std::path::Path;

use dugite_primitives::hash::Hash32;

/// Load and validate a checkpoints file.
///
/// `expected_file_hash` (if any) is the hex Blake2b-256 of the raw bytes.
pub fn load_checkpoints(
    path: &Path,
    expected_file_hash: Option<&str>,
) -> Result<HashMap<u64, Hash32>, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("failed to read checkpoints file {}: {e}", path.display()))?;

    if let Some(expected) = expected_file_hash {
        let actual = dugite_primitives::hash::blake2b_256(&bytes);
        let actual_hex = actual.to_hex();
        let expected = expected.trim().to_lowercase();
        if actual_hex != expected {
            return Err(format!(
                "checkpoints file hash mismatch for {}: expected {expected}, got {actual_hex}",
                path.display()
            ));
        }
    }

    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("failed to parse checkpoints JSON: {e}"))?;
    let arr = value
        .get("checkpoints")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "checkpoints file missing `checkpoints` array".to_string())?;

    let mut map = HashMap::new();
    for entry in arr {
        let block_no = entry
            .get("blockNo")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "checkpoint entry missing integer `blockNo`".to_string())?;
        let hash_hex = entry
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "checkpoint entry missing string `hash`".to_string())?;
        let hash_bytes = hex::decode(hash_hex.trim())
            .map_err(|e| format!("checkpoint hash `{hash_hex}` is not valid hex: {e}"))?;
        if hash_bytes.len() != 32 {
            return Err(format!(
                "checkpoint hash `{hash_hex}` must be 32 bytes, got {}",
                hash_bytes.len()
            ));
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(&hash_bytes);
        if map.insert(block_no, Hash32::from_bytes(h)).is_some() {
            return Err(format!("duplicate checkpoint for block number {block_no}"));
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn loads_valid_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "00".repeat(32);
        let p = write(
            dir.path(),
            "cp.json",
            &format!(r#"{{"checkpoints":[{{"blockNo":1000,"hash":"{hash}"}}]}}"#),
        );
        let map = load_checkpoints(&p, None).unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&1000));
    }

    #[test]
    fn duplicate_block_number_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "11".repeat(32);
        let p = write(
            dir.path(),
            "dup.json",
            &format!(
                r#"{{"checkpoints":[{{"blockNo":5,"hash":"{hash}"}},{{"blockNo":5,"hash":"{hash}"}}]}}"#
            ),
        );
        assert!(load_checkpoints(&p, None)
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn file_hash_pin_mismatch_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "22".repeat(32);
        let p = write(
            dir.path(),
            "pin.json",
            &format!(r#"{{"checkpoints":[{{"blockNo":1,"hash":"{hash}"}}]}}"#),
        );
        // Correct pin passes.
        let bytes = std::fs::read(&p).unwrap();
        let real = dugite_primitives::hash::blake2b_256(&bytes).to_hex();
        assert!(load_checkpoints(&p, Some(&real)).is_ok());
        // Wrong pin fails.
        let err = load_checkpoints(&p, Some(&"ab".repeat(32))).unwrap_err();
        assert!(err.contains("hash mismatch"), "{err}");
    }

    #[test]
    fn missing_array_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.json", r#"{"NetworkMagic":764824073}"#);
        assert!(load_checkpoints(&p, None)
            .unwrap_err()
            .contains("missing `checkpoints`"));
    }

    #[test]
    fn bad_hash_length_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "short.json",
            r#"{"checkpoints":[{"blockNo":1,"hash":"abcd"}]}"#,
        );
        assert!(load_checkpoints(&p, None)
            .unwrap_err()
            .contains("must be 32 bytes"));
    }
}
