pub mod cbor;
pub mod decode;
pub mod decode_bounded;
pub mod dual_decode;
pub mod encode;
pub mod error;
pub mod haskell_snapshot;
pub mod mempack;
pub mod multi_era;

pub use cbor::*;
pub use decode_bounded::*;
pub use encode::*;
pub use error::*;

// Pallas-free helpers stay sourced from `multi_era` (they don't touch the
// pallas decoder — they just walk the outer CBOR envelope). After the M4
// in-house decoder lands these will move under `decode/`.
pub use multi_era::{
    compute_block_body_size_from_cbor, dugite_hash_to_pallas28, dugite_hash_to_pallas32,
    extract_block_body_cbor, extract_block_body_components, pallas_hash_to_dugite28,
    pallas_hash_to_dugite32, DecodeMode,
};

// The public `decode_*` entry points route through the dual-decode harness
// so that DUGITE_DUAL_DECODE can cross-check the in-house decoder against
// pallas (no-op pre-M4; real comparison after M4 lands).
pub use dual_decode::{
    decode_block, decode_block_minimal, decode_block_minimal_with_byron_epoch_length,
    decode_block_with_byron_epoch_length, decode_transaction, dual_decode_mode, DualDecodeMode,
};
