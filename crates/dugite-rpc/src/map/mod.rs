//! Dugite → utxorpc protobuf mapping.
//!
//! In-house translator from `dugite_primitives` decoded shapes to the
//! `utxorpc.v1beta.cardano` protobuf message types. Mapping is split per
//! top-level concept so each file stays small enough to read in one sitting
//! and golden tests cover one concern at a time.
//!
//! M1.B scope (this commit): `common` helpers, `block`, `tx` (Conway-shape
//! with inputs/outputs/fee/mint/withdrawals/validity/auxiliary-hash
//! populated; certs/governance/scripts/witnesses left empty pending M2's
//! full mapping). `native_bytes` always populated from the original
//! `Block.raw_cbor` / `Transaction.raw_body_cbor`.
//!
//! M2 fills the remaining `Tx` fields plus the standalone mapping modules
//! for cert / governance / script / plutus_data / pparams / era_summary /
//! genesis / patterns / metadatum / asset.

pub mod block;
pub mod cert;
pub mod common;
pub mod metadatum;
pub mod plutus_data;
pub mod pparams;
pub mod script;
pub mod tx;
