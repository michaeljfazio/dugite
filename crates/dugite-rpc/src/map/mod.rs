//! Dugite → utxorpc protobuf mapping.
//!
//! In-house translator from `dugite_primitives` decoded shapes to the
//! `utxorpc.v1beta.cardano` protobuf message types. Mapping is split per
//! top-level concept so each file stays small enough to read in one sitting
//! and golden tests cover one concern at a time: `common` helpers, `block`,
//! `tx` (inputs / outputs / fee / mint / withdrawals / validity /
//! certificates / witnesses / collateral / proposals / voting —
//! `native_bytes` always populated from the original `Block.raw_cbor` /
//! `Transaction.raw_body_cbor`), plus the standalone `cert` / `governance`
//! / `script` / `plutus_data` / `pparams` / `patterns` / `metadatum`
//! modules. `message_names` centralises the proto message-name constants
//! `masking::apply` needs (issue #1004).

pub mod block;
pub mod cert;
pub mod common;
pub mod governance;
/// The one INBOUND direction: attacker-controlled protobuf -> dugite types.
pub mod inbound;
pub mod message_names;
pub mod metadatum;
pub mod patterns;
pub mod plutus_data;
pub mod pparams;
pub mod script;
pub mod tx;
