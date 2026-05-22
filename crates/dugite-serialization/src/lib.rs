pub mod cbor;
pub mod decode;
pub mod decode_bounded;
pub mod encode;
pub mod error;
pub mod haskell_snapshot;
pub mod mempack;

pub use cbor::*;
pub use decode_bounded::*;
pub use encode::*;
pub use error::*;

// Public block / transaction decode API — routes through the in-house
// multi-era decoder under `decode/`.
pub use decode::cbor_helpers::{
    compute_block_body_size_from_cbor, extract_block_body_cbor, extract_block_body_components,
};
pub use decode::{
    decode_block, decode_block_minimal, decode_block_minimal_with_byron_epoch_length,
    decode_block_with_byron_epoch_length, decode_transaction, decode_transaction_input,
    decode_transaction_output,
};

// Helper exposed by the existing in-house pipeline; used by mithril chunk-file
// import to probe block identity without a full decode.
pub use cbor::extract_block_identity;
