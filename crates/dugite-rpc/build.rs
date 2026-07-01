//! Generates prost/tonic stubs from the vendored UTxO RPC spec at
//! `proto/utxorpc/{v1alpha,v1beta}/{cardano,sync,query,submit,watch}/*.proto`.
//!
//! The output goes to `OUT_DIR` (per Cargo convention) and is `include!()`'d
//! by `src/proto/mod.rs`. We also emit a `FILE_DESCRIPTOR_SET` so the
//! server can expose gRPC reflection without an extra runtime dep.
//!
//! Spec version is pinned in `proto/VERSION` — see `just bump-utxorpc-spec`
//! for the refresh workflow.

use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root: PathBuf = PathBuf::from("proto");
    let proto_files = collect_protos(&proto_root)?;

    // Re-run if any .proto file changes, or the proto root itself
    // (so adding/removing files is picked up by cargo).
    println!("cargo:rerun-if-changed={}", proto_root.display());
    println!("cargo:rerun-if-changed=proto/VERSION");
    for p in &proto_files {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let out_dir: PathBuf = std::env::var_os("OUT_DIR").ok_or("OUT_DIR not set")?.into();
    let descriptor_path = out_dir.join("utxorpc_descriptor.bin");

    tonic_prost_build::configure()
        .build_server(true)
        // Client codegen enabled so our own integration tests can drive
        // the server without pulling in the upstream `utxorpc` Rust SDK
        // (which carries the v1alpha+v1beta types we already generate
        // ourselves). The generated client types end up under
        // `*_service_client` modules in OUT_DIR; only consumers that
        // import them pay the per-call client-codec cost.
        .build_client(true)
        .file_descriptor_set_path(&descriptor_path)
        // tonic-prost-build 0.14 unifies the `protos`/`includes` type params,
        // so the include list must match `proto_files`' element type (PathBuf).
        .compile_protos(&proto_files, &[proto_root])?;

    Ok(())
}

/// Recursively collect every `*.proto` under `root`. Sorted output so the
/// build is deterministic across filesystem walk orders.
fn collect_protos(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("proto") {
            out.push(path);
        }
    }
    Ok(())
}
