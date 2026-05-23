//! Unit tests for the tarball extractor logic.
//! Creates a synthetic in-memory tarball and verifies that
//! download-upstream-fixtures would unpack it correctly.

use std::path::PathBuf;

use flate2::write::GzEncoder;
use flate2::Compression;

fn make_synthetic_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut ar = tar::Builder::new(gz);
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        ar.append_data(&mut header, name, *content).unwrap();
    }
    let gz = ar.into_inner().unwrap();
    gz.finish().unwrap()
}

fn extract_tarball(data: &[u8], target: &std::path::Path) {
    let cursor = std::io::Cursor::new(data);
    let gz = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);
    archive.set_overwrite(true);
    archive.unpack(target).expect("extract failed");
}

#[test]
fn synthetic_tarball_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let files: &[(&str, &[u8])] = &[
        ("builtin/add-integer/success/expected.cbor", b"\x80"),
        ("term/delay/success/expected.cbor", b"\x81"),
        ("README.txt", b"hello"),
    ];
    let tarball = make_synthetic_tarball(files);
    let target = dir.path().join("out");
    std::fs::create_dir_all(&target).unwrap();
    extract_tarball(&tarball, &target);

    for (name, content) in files {
        let path = target.join(name);
        assert!(path.exists(), "missing: {name}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            *content,
            "content mismatch: {name}"
        );
    }
}

#[test]
fn empty_tarball_produces_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = make_synthetic_tarball(&[]);
    let target = dir.path().join("out");
    std::fs::create_dir_all(&target).unwrap();
    extract_tarball(&tarball, &target);
    // No panic means the empty archive was handled gracefully.
    let count = std::fs::read_dir(&target).unwrap().count();
    assert_eq!(count, 0);
}

#[test]
fn workspace_root_helper_finds_root() {
    // The workspace root must contain a Cargo.toml with [workspace].
    // Walk up from CARGO_MANIFEST_DIR and verify we find it.
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let found = loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists() {
            let text = std::fs::read_to_string(&cargo).unwrap();
            if text.contains("[workspace]") {
                break Some(dir.clone());
            }
        }
        if !dir.pop() {
            break None;
        }
    };
    let root = found.expect("workspace root not found");
    assert!(root.join("Cargo.toml").exists());
    assert!(root.join("tests/conformance").exists());
}
