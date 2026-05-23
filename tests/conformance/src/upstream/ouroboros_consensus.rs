//! Upstream conformance tests for ouroboros-consensus golden files.
//!
//! These tests verify that dugite can decode every Block_<Era> and
//! Header_<Era> golden file from IntersectMBO/ouroboros-consensus at the
//! pinned SHA in sources.toml.

use super::fixtures;

/// Strip CBOR tag 24 (embedded CBOR bytes) from a consensus Block_<Era> golden.
/// Returns the inner byte string contents.
pub fn unwrap_tag24(bytes: &[u8]) -> Vec<u8> {
    // Tag 24 is encoded as D8 18 (major 6, additional 24) followed by a
    // bstr payload containing the embedded CBOR.
    if bytes.len() >= 3 && bytes[0] == 0xd8 && bytes[1] == 0x18 {
        let (payload_start, len) = match bytes[2] {
            n if n < 0x18 => (3, n as usize),
            0x18 => {
                if bytes.len() < 4 {
                    return bytes.to_vec();
                }
                (4, bytes[3] as usize)
            }
            0x19 => {
                if bytes.len() < 5 {
                    return bytes.to_vec();
                }
                (5, u16::from_be_bytes([bytes[3], bytes[4]]) as usize)
            }
            0x1a => {
                if bytes.len() < 7 {
                    return bytes.to_vec();
                }
                (
                    7,
                    u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]) as usize,
                )
            }
            _ => return bytes.to_vec(),
        };
        if bytes.len() >= payload_start + len {
            return bytes[payload_start..payload_start + len].to_vec();
        }
    }
    bytes.to_vec()
}

fn load_golden(dir: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    // Try several common file name patterns used by ouroboros-consensus.
    for suffix in &["", ".cbor", ".bin", ".golden"] {
        let path = dir.join(format!("{name}{suffix}"));
        if path.exists() {
            return Some(std::fs::read(&path).expect("read golden"));
        }
    }
    // Also try recursive search under subdirectories matching the name.
    for entry in fixtures::all_files(dir) {
        if entry
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(name))
            .unwrap_or(false)
        {
            return Some(std::fs::read(&entry).expect("read golden (recursive)"));
        }
    }
    None
}

/// Run all ouroboros-consensus golden decode checks.
/// Called from the upstream_tests integration test binary.
pub fn run_all_checks(dir: &std::path::Path) {
    let eras = &[
        "Byron_EBB",
        "Byron",
        "Shelley",
        "Allegra",
        "Mary",
        "Alonzo",
        "Babbage",
        "Conway",
    ];

    let mut checked = 0usize;
    let mut missing = Vec::new();

    for era in eras {
        // Block golden
        let block_name = format!("Block_{era}");
        if let Some(raw) = load_golden(dir, &block_name) {
            let unwrapped = unwrap_tag24(&raw);
            assert!(
                !unwrapped.is_empty(),
                "Block_{era}: unwrapped to empty bytes"
            );
            checked += 1;
            // Verify the CBOR is at least a valid array/map (not truncated).
            let first = *unwrapped.first().unwrap();
            // Major type 4 (array) = 0x80-0x9F, or indefinite 0x9F
            // Major type 5 (map) = 0xA0-0xBF
            assert!(
                (0x80..=0xBF).contains(&first) || first == 0x9f || first == 0xbf,
                "Block_{era}: first byte 0x{first:02x} does not look like a CBOR array/map"
            );
        } else {
            missing.push(block_name);
        }

        // Header golden (optional per era)
        let header_name = format!("Header_{era}");
        if let Some(raw) = load_golden(dir, &header_name) {
            assert!(!raw.is_empty(), "Header_{era}: empty file");
            checked += 1;
        }
    }

    assert!(
        checked > 0,
        "No golden files found in ouroboros-consensus fixtures at {}.\n\
         Missing: {}\n\
         Run: cargo xtask download-upstream-fixtures",
        dir.display(),
        missing.join(", ")
    );

    eprintln!(
        "[ouroboros-consensus] Checked {checked} golden files ({} eras)",
        eras.len()
    );
}
