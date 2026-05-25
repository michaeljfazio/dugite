//! Generated UTxO RPC protobuf bindings.
//!
//! The `.proto` sources are vendored at `crates/dugite-rpc/proto/utxorpc/`
//! and compiled to Rust by `build.rs` (via tonic-build → prost-build →
//! protoc). The output lives in `OUT_DIR` and is `include!()`'d into the
//! module tree below.
//!
//! Two API versions are exposed in parallel:
//!
//! * [`v1alpha`] — older, kept for one release cycle of backwards
//!   compatibility while clients migrate.
//! * [`v1beta`] — current, the source of truth for mapping logic.
//!
//! [`FILE_DESCRIPTOR_SET`] is the encoded
//! [`google.protobuf.FileDescriptorSet`] covering every proto in both
//! versions; the server feeds this to `tonic-reflection` so `grpcurl
//! -plaintext :port list` works without bundling a separate descriptor
//! artifact.

#![allow(clippy::all, clippy::pedantic, missing_docs, rust_2018_idioms)]
// prost-generated code includes derive macros that occasionally trip newer
// lints; the upstream protobuf shapes are not ours to clean up.
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

/// The full encoded `FileDescriptorSet` for every vendored proto, suitable
/// for `tonic_reflection::server::Builder::register_encoded_file_descriptor_set`.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/utxorpc_descriptor.bin"));

/// `utxorpc.v1alpha.*` generated bindings.
pub mod v1alpha {
    pub mod cardano {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1alpha.cardano.rs"));
    }
    pub mod sync {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1alpha.sync.rs"));
    }
    pub mod query {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1alpha.query.rs"));
    }
    pub mod submit {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1alpha.submit.rs"));
    }
    pub mod watch {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1alpha.watch.rs"));
    }
}

/// `utxorpc.v1beta.*` generated bindings.
pub mod v1beta {
    pub mod cardano {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1beta.cardano.rs"));
    }
    pub mod sync {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1beta.sync.rs"));
    }
    pub mod query {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1beta.query.rs"));
    }
    pub mod submit {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1beta.submit.rs"));
    }
    pub mod watch {
        include!(concat!(env!("OUT_DIR"), "/utxorpc.v1beta.watch.rs"));
    }
}
