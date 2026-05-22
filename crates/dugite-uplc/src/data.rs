//! The `Data` type — PlutusData.
//!
//! `Data` is the recursive sum type that the Cardano on-chain script
//! context (TxInfo) and per-redeemer datums/redeemers are encoded into.
//! Its CBOR encoding is well-known and **not** subject to RFC 8949 §4.2
//! canonical-form enforcement at the decoder level (the Haskell decoder
//! accepts non-canonical encodings). However, *bignum* tag handling is
//! strict: positive bignums use tag 2 and negative bignums use tag 3,
//! and integer values outside the i64-fits-in-major-types range MUST
//! use the bignum form on encode.
//!
//! This is the scaffolding module; the actual CBOR codec lives in
//! `data::codec` (to be added).

use num_bigint::BigInt;

/// Recursive PlutusData value. Maps 1:1 onto the Haskell `Plutus.V1.Data`
/// definition.
///
/// `Constr` and `Map` payloads are `Vec` rather than `BTreeMap` because
/// the on-chain encoding preserves insertion order — sorting on encode
/// would change the body hash for txs that gossip in non-sorted form,
/// breaking byte-exact round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Data {
    /// `Constr` — tagged sum: `Constr tag args`.
    ///
    /// Encoding rule (CBOR):
    ///   - `tag` in `0..=6`  → CBOR tag `121 + tag`, payload is the
    ///     args array.
    ///   - `tag` in `7..=127` → CBOR tag `1280 + (tag - 7)`, payload
    ///     is the args array.
    ///   - `tag` outside that range → CBOR tag `102`, payload is
    ///     `[tag, args]`.
    Constr(u64, Vec<Data>),

    /// `Map` — list of key-value pairs.
    Map(Vec<(Data, Data)>),

    /// `List` — list of values.
    List(Vec<Data>),

    /// `I` — arbitrary-precision integer. Encoded as CBOR major-0/major-1
    /// for values in i64 range, otherwise as CBOR tag 2 (positive bignum)
    /// or tag 3 (negative bignum) wrapping a byte string.
    I(BigInt),

    /// `B` — byte string.
    B(Vec<u8>),
}
