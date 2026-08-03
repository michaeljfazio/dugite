//! Fail-closed parsing of the set-shaped filter arguments carried by Local
//! State Queries.
//!
//! # Why this module exists (#963)
//!
//! Every filtered LSQ carries a `Set` — sometimes wrapped in a `Maybe` — naming
//! what the client wants answers for. Before #963 each ad-hoc parser was
//! written in this shape:
//!
//! ```ignore
//! let _ = decoder.tag();                       // result discarded
//! if let Ok(Some(n)) = decoder.array() {       // Ok(None) (indefinite) skipped
//!     for _ in 0..n {
//!         if let Ok(bytes) = decoder.bytes() { // decode failures skipped
//!             out.push(bytes.to_vec());
//!         }
//!     }
//! }
//! out                                          // empty on ANY failure
//! ```
//!
//! and every caller read an empty result as "no filter — answer for
//! everything". A parse failure was therefore indistinguishable from a client
//! asking for the whole set, so the filters failed **open**: dugite answered
//! `query pool-state --stake-pool-id A` with pools A *and* B, and asking for a
//! different pool returned byte-identical output.
//!
//! Two distinct defects hid behind that:
//!
//! 1. **The `Maybe` wrapper was never handled.** `encodeShelleyQuery`
//!    (ouroboros-consensus `Ouroboros/Consensus/Shelley/Ledger/Query.hs`) writes
//!    each argument with `toCBOR`, so `Maybe (Set …)` arguments — tags 19, 20,
//!    21 and 36 — arrive under `cardano-binary`'s `encodeMaybe`: `array(0)` for
//!    `Nothing`, `array(1) <set>` for `Just`. The old parser read the `Just`
//!    wrapper's `array(1)` header as if it were the set, found a tag where it
//!    wanted `bstr`, and gave up silently.
//! 2. **"Empty set" is not one rule.** cardano-ledger writes some of these
//!    handlers as `Map.restrictKeys m keys` — where an empty set selects
//!    *nothing* — and others with an explicit `| null keys = everything` guard.
//!    Both appear among the filtered queries, so the rule is recorded per query
//!    at each call site as an [`OnEmptySet`], with the upstream function named.
//!
//! Parse failures here are returned, never swallowed. dugite answers a
//! malformed filter loudly rather than serving a superset.
//!
//! Items are `pub` rather than `pub(crate)` because `mod filter` is private —
//! nothing here is reachable outside the crate — and because `fuzz/` compiles
//! this file in directly via `#[path]` (`dugite-node` cannot be a fuzz
//! dependency; see `fuzz/src/node/n2c_query/mod.rs`).

use minicbor::data::Type;

use crate::node::n2c_query::types::QueryResult;

/// Which CBOR shape a query's filter argument takes.
///
/// This follows directly from the constructor's Haskell type in
/// `encodeShelleyQuery`; the two shapes are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetArgShape {
    /// `Maybe (Set a)` — `array(0)` for `Nothing`, `array(1) <set>` for `Just`.
    Optional,
    /// `Set a` — the set stands alone.
    Required,
}

/// How a query answers an **explicitly empty** filter set.
///
/// Per-query, not per-shape. Always record the upstream function that decides
/// it at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnEmptySet {
    /// Upstream guards on `null keys` and answers for everything.
    AllItems,
    /// Upstream restricts by the set, so an empty set selects nothing.
    NoItems,
}

/// Why a filter argument could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterArgError(pub String);

impl std::fmt::Display for FilterArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed filter argument: {}", self.0)
    }
}

pub fn err(msg: impl Into<String>) -> FilterArgError {
    FilterArgError(msg.into())
}

/// Parse a filter argument.
///
/// Returns `None` for Haskell `Nothing` (or for no argument bytes at all) and
/// `Some(items)` for `Just s` / a bare `Set`. An empty `Some` means the client
/// explicitly asked for an empty set; what that selects is the caller's rule.
pub fn parse_set<T>(
    decoder: &mut minicbor::Decoder<'_>,
    shape: SetArgShape,
    read_elem: impl Fn(&mut minicbor::Decoder<'_>, usize) -> Result<T, FilterArgError>,
) -> Result<Option<Vec<T>>, FilterArgError> {
    // No argument bytes at all. No cardano-node client produces this — every
    // filtered tag is encoded as `array(2)` — but the in-crate handler tests
    // drive handlers with an empty decoder, and answering "no filter" for "no
    // argument" is both harmless and what dugite has always done. The case that
    // must *not* silently mean this is a malformed argument, below.
    //
    // Emptiness is decided from the buffer, NOT from `datatype()`. minicbor's
    // `type_of` peeks the payload byte for headers `0x38..=0x3b` to choose an
    // integer width, so `datatype()` reports `EndOfInput` for a *truncated
    // item* as readily as for an exhausted decoder. Treating that as "no
    // argument" reintroduced the #963 fail-open on a one-byte input; the
    // `fuzz_lsq_filter_arg` target found it within a minute of first running.
    if decoder.position() >= decoder.input().len() {
        return Ok(None);
    }

    if shape == SetArgShape::Optional {
        // A `Maybe` is always an array, so a leading tag cannot be the wrapper.
        // Accept a bare (tagged) set as `Just` for clients that omit it —
        // unambiguous, since a `Set` never begins with anything else.
        if decoder.datatype().map_err(|e| err(e.to_string()))? != Type::Tag {
            let len = decoder
                .array()
                .map_err(|e| err(format!("expected Maybe wrapper: {e}")))?
                .ok_or_else(|| {
                    err(
                        "indefinite-length Maybe wrapper; cardano-binary's encodeMaybe \
                         always writes a definite array(0) or array(1)",
                    )
                })?;
            match len {
                // `encodeMaybe Nothing = encodeListLen 0`
                0 => return Ok(None),
                // `encodeMaybe (Just x) = encodeListLen 1 <> x`
                1 => {}
                n => {
                    return Err(err(format!(
                        "Maybe wrapper must be array(0) or array(1), got array({n})"
                    )));
                }
            }
        }
    }

    read_set(decoder, read_elem).map(Some)
}

/// Read `toCBOR (Set a)`: an optional `tag(258)` followed by an array.
///
/// The tag is unconditional under `cardano-binary`'s `ToCBOR (Set a)`
/// (`encodeSetSkel`) but PV>=9-gated under `cardano-ledger-binary`'s `EncCBOR`
/// (`toEraCBOR`), and both encoders are used by different query tags, so it is
/// accepted either way. `encodeContainerSkel` always writes a *definite* array;
/// the indefinite form is accepted because #938 established that dugite reads
/// both framings wherever upstream might emit either.
fn read_set<T>(
    decoder: &mut minicbor::Decoder<'_>,
    read_elem: impl Fn(&mut minicbor::Decoder<'_>, usize) -> Result<T, FilterArgError>,
) -> Result<Vec<T>, FilterArgError> {
    if decoder.datatype().map_err(|e| err(e.to_string()))? == Type::Tag {
        let tag = decoder
            .tag()
            .map_err(|e| err(format!("expected set tag: {e}")))?;
        if u64::from(tag) != 258 {
            return Err(err(format!(
                "expected set tag 258, got tag {}",
                u64::from(tag)
            )));
        }
    }

    let len = decoder
        .array()
        .map_err(|e| err(format!("expected set array: {e}")))?;

    let mut out = Vec::new();
    match len {
        Some(n) => {
            for i in 0..n {
                out.push(read_elem(decoder, i as usize)?);
            }
        }
        None => loop {
            if decoder.datatype().map_err(|e| err(e.to_string()))? == Type::Break {
                decoder
                    .skip()
                    .map_err(|e| err(format!("expected break: {e}")))?;
                break;
            }
            let i = out.len();
            out.push(read_elem(decoder, i)?);
        },
    }
    Ok(out)
}

/// Decode a filter argument, mapping a parse failure to an explicit query error
/// and an empty set to the query's own rule.
///
/// `Ok(None)` means "answer for everything"; `Ok(Some(items))` is a genuine
/// filter. `Err` carries the [`QueryResult::Error`] to return, boxed because
/// `QueryResult` is a large enum and an `Err` variant that size trips
/// `clippy::result_large_err` on every call site.
pub fn filter_arg<T>(
    decoder: &mut minicbor::Decoder<'_>,
    query: &str,
    shape: SetArgShape,
    on_empty: OnEmptySet,
    read_elem: impl Fn(&mut minicbor::Decoder<'_>, usize) -> Result<T, FilterArgError>,
) -> Result<Option<Vec<T>>, Box<QueryResult>> {
    match parse_set(decoder, shape, read_elem) {
        Ok(None) => Ok(None),
        Ok(Some(items)) if items.is_empty() && on_empty == OnEmptySet::AllItems => Ok(None),
        Ok(Some(items)) => Ok(Some(items)),
        Err(e) => {
            tracing::debug!("{query}: {e}");
            Err(Box::new(QueryResult::Error(format!("{query}: {e}"))))
        }
    }
}

// ─── element readers ────────────────────────────────────────────────────
//
// Widths are enforced rather than tolerated. A filter that silently keeps a
// hash it can never match would answer "no such entry" for an entry that
// exists — the same class of quiet wrong answer as #963, pointing the other
// way.

/// `KeyHash StakePool` — always `bstr(28)`.
pub fn read_pool_id(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
) -> Result<Vec<u8>, FilterArgError> {
    read_hash(decoder, index, 28, "pool key hash")
}

/// `Credential kr` — `[0, bstr(28)]` (KeyHashObj) or `[1, bstr(28)]`
/// (ScriptHashObj), per cardano-ledger `EncCBOR (Credential kr)`. Returns
/// `(discriminator, hash)`.
pub fn read_credential(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
) -> Result<(u8, Vec<u8>), FilterArgError> {
    expect_array(decoder, index, 2, "Credential")?;
    let kind = decoder
        .u8()
        .map_err(|e| err(format!("element {index}: Credential discriminator: {e}")))?;
    if kind > 1 {
        return Err(err(format!(
            "element {index}: Credential discriminator must be 0 (KeyHashObj) \
             or 1 (ScriptHashObj), got {kind}"
        )));
    }
    // Both `KeyHash` and `ScriptHash` are `ADDRHASH`-wide.
    let hash = read_hash(decoder, index, 28, "credential hash")?;
    Ok((kind, hash))
}

/// `DRep` — `[0, bstr(28)]`, `[1, bstr(28)]`, `[2]` (AlwaysAbstain) or `[3]`
/// (AlwaysNoConfidence), per cardano-ledger `EncCBOR DRep`. Returns
/// `(discriminator, Option<hash>)`.
pub fn read_drep(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
) -> Result<(u8, Option<Vec<u8>>), FilterArgError> {
    let len = decoder
        .array()
        .map_err(|e| err(format!("element {index}: expected DRep array: {e}")))?
        .ok_or_else(|| err(format!("element {index}: indefinite-length DRep")))?;
    let kind = decoder
        .u8()
        .map_err(|e| err(format!("element {index}: DRep discriminator: {e}")))?;
    match (kind, len) {
        (0 | 1, 2) => Ok((kind, Some(read_hash(decoder, index, 28, "DRep hash")?))),
        (2 | 3, 1) => Ok((kind, None)),
        _ => Err(err(format!(
            "element {index}: DRep tag {kind} with array({len}); expected \
             [0|1, bstr(28)] or [2|3]"
        ))),
    }
}

/// `GovActionId` — `[bstr(32), Word16]`, per cardano-ledger
/// `EncCBOR GovActionId` (`Rec GovActionId !> To gaidTxId !> To gaidGovActionIx`).
pub fn read_gov_action_id(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
) -> Result<(Vec<u8>, u32), FilterArgError> {
    expect_array(decoder, index, 2, "GovActionId")?;
    let tx_id = read_hash(decoder, index, 32, "GovActionId txId")?;
    // `GovActionIx` is a `Word16`; dugite carries it as `u32` internally.
    let ix = decoder
        .u16()
        .map_err(|e| err(format!("element {index}: GovActionId index: {e}")))?;
    Ok((tx_id, u32::from(ix)))
}

fn expect_array(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
    want: u64,
    what: &str,
) -> Result<(), FilterArgError> {
    let len = decoder
        .array()
        .map_err(|e| err(format!("element {index}: expected {what} array: {e}")))?
        .ok_or_else(|| err(format!("element {index}: indefinite-length {what}")))?;
    if len != want {
        return Err(err(format!(
            "element {index}: {what} must be array({want}), got array({len})"
        )));
    }
    Ok(())
}

fn read_hash(
    decoder: &mut minicbor::Decoder<'_>,
    index: usize,
    width: usize,
    what: &str,
) -> Result<Vec<u8>, FilterArgError> {
    let bytes = decoder
        .bytes()
        .map_err(|e| err(format!("element {index}: {what} is not a byte string: {e}")))?;
    if bytes.len() != width {
        return Err(err(format!(
            "element {index}: {what} is {} bytes, expected {width}",
            bytes.len()
        )));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(f: impl FnOnce(&mut minicbor::Encoder<&mut Vec<u8>>)) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        f(&mut e);
        buf
    }

    fn pools(bytes: &[u8], shape: SetArgShape) -> Result<Option<Vec<Vec<u8>>>, FilterArgError> {
        parse_set(&mut minicbor::Decoder::new(bytes), shape, read_pool_id)
    }

    /// `encodeMaybe Nothing = encodeListLen 0`.
    #[test]
    fn nothing_is_no_filter() {
        let b = enc(|e| {
            e.array(0).unwrap();
        });
        assert_eq!(pools(&b, SetArgShape::Optional).unwrap(), None);
    }

    /// `encodeMaybe (Just x) = encodeListLen 1 <> x`. This is the exact shape
    /// `cardano-cli` sends and the one #963 could not decode.
    #[test]
    fn just_singleton_round_trips() {
        let b = enc(|e| {
            e.array(1).unwrap();
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.bytes(&[7u8; 28]).unwrap();
        });
        assert_eq!(
            pools(&b, SetArgShape::Optional).unwrap(),
            Some(vec![vec![7u8; 28]])
        );
    }

    /// `Just mempty` is a real, distinct value — it is not `Nothing`.
    #[test]
    fn just_empty_is_distinguished_from_nothing() {
        let b = enc(|e| {
            e.array(1).unwrap();
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(0).unwrap();
        });
        assert_eq!(pools(&b, SetArgShape::Optional).unwrap(), Some(vec![]));
    }

    #[test]
    fn bare_set_is_accepted_for_optional_args() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.bytes(&[3u8; 28]).unwrap();
        });
        assert_eq!(
            pools(&b, SetArgShape::Optional).unwrap(),
            Some(vec![vec![3u8; 28]])
        );
    }

    #[test]
    fn untagged_set_is_accepted_for_required_args() {
        let b = enc(|e| {
            e.array(2).unwrap();
            e.bytes(&[1u8; 28]).unwrap();
            e.bytes(&[2u8; 28]).unwrap();
        });
        assert_eq!(
            pools(&b, SetArgShape::Required).unwrap(),
            Some(vec![vec![1u8; 28], vec![2u8; 28]])
        );
    }

    #[test]
    fn indefinite_set_is_accepted() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.begin_array().unwrap();
            e.bytes(&[9u8; 28]).unwrap();
            e.end().unwrap();
        });
        assert_eq!(
            pools(&b, SetArgShape::Required).unwrap(),
            Some(vec![vec![9u8; 28]])
        );
    }

    #[test]
    fn no_argument_bytes_means_no_filter() {
        assert_eq!(pools(&[], SetArgShape::Required).unwrap(), None);
        assert_eq!(pools(&[], SetArgShape::Optional).unwrap(), None);
    }

    /// A *truncated* item is not an absent one. `0x3a` is the header of a
    /// 4-byte negative integer with no payload; minicbor's `datatype()` answers
    /// `EndOfInput` for it, because `type_of` peeks the payload to choose an
    /// integer width. Reading that as "no argument" is the #963 fail-open, and
    /// it is what `fuzz_lsq_filter_arg` found in the first version of this
    /// module.
    #[test]
    fn a_truncated_header_is_not_an_absent_argument() {
        for header in [0x38u8, 0x39, 0x3a, 0x3b] {
            for shape in [SetArgShape::Required, SetArgShape::Optional] {
                assert!(
                    pools(&[header], shape).is_err(),
                    "{header:#04x} ({shape:?}) must not decode to \"no filter\""
                );
            }
        }
    }

    /// The heart of #963: every one of these used to yield an empty vector,
    /// which every caller read as "answer for everything".
    #[test]
    fn malformed_arguments_error_rather_than_meaning_everything() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "bare integer",
                enc(|e| {
                    e.u32(7).unwrap();
                }),
            ),
            (
                "wrong set tag",
                enc(|e| {
                    e.tag(minicbor::data::Tag::new(259)).unwrap();
                    e.array(0).unwrap();
                }),
            ),
            (
                "element is not bytes",
                enc(|e| {
                    e.tag(minicbor::data::Tag::new(258)).unwrap();
                    e.array(1).unwrap();
                    e.u32(1).unwrap();
                }),
            ),
            (
                "element is the wrong width",
                enc(|e| {
                    e.tag(minicbor::data::Tag::new(258)).unwrap();
                    e.array(1).unwrap();
                    e.bytes(&[1u8; 32]).unwrap();
                }),
            ),
            (
                "truncated set",
                enc(|e| {
                    e.tag(minicbor::data::Tag::new(258)).unwrap();
                    e.array(2).unwrap();
                    e.bytes(&[1u8; 28]).unwrap();
                }),
            ),
        ];
        for (name, bytes) in cases {
            assert!(
                pools(&bytes, SetArgShape::Required).is_err(),
                "{name} must not decode to a filter"
            );
        }
    }

    #[test]
    fn maybe_wrapper_of_impossible_length_errors() {
        let b = enc(|e| {
            e.array(2).unwrap();
            e.u32(1).unwrap();
            e.u32(2).unwrap();
        });
        assert!(pools(&b, SetArgShape::Optional).is_err());
    }

    #[test]
    fn credentials_carry_their_discriminator() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(2).unwrap();
            e.array(2).unwrap();
            e.u8(0).unwrap();
            e.bytes(&[1u8; 28]).unwrap();
            e.array(2).unwrap();
            e.u8(1).unwrap();
            e.bytes(&[2u8; 28]).unwrap();
        });
        let got = parse_set(
            &mut minicbor::Decoder::new(&b),
            SetArgShape::Required,
            read_credential,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got, vec![(0, vec![1u8; 28]), (1, vec![2u8; 28])]);
    }

    #[test]
    fn credential_rejects_unknown_discriminator() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.array(2).unwrap();
            e.u8(2).unwrap();
            e.bytes(&[1u8; 28]).unwrap();
        });
        assert!(parse_set(
            &mut minicbor::Decoder::new(&b),
            SetArgShape::Required,
            read_credential
        )
        .is_err());
    }

    /// All four `DRep` constructors, including the two payload-less ones.
    #[test]
    fn drep_covers_all_four_constructors() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(4).unwrap();
            e.array(2).unwrap();
            e.u8(0).unwrap();
            e.bytes(&[1u8; 28]).unwrap();
            e.array(2).unwrap();
            e.u8(1).unwrap();
            e.bytes(&[2u8; 28]).unwrap();
            e.array(1).unwrap();
            e.u8(2).unwrap();
            e.array(1).unwrap();
            e.u8(3).unwrap();
        });
        let got = parse_set(
            &mut minicbor::Decoder::new(&b),
            SetArgShape::Required,
            read_drep,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            got,
            vec![
                (0, Some(vec![1u8; 28])),
                (1, Some(vec![2u8; 28])),
                (2, None),
                (3, None),
            ]
        );
    }

    /// `[2]`/`[3]` carry no hash, `[0]`/`[1]` must.
    #[test]
    fn drep_rejects_arity_mismatch() {
        let bad = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.array(1).unwrap();
            e.u8(0).unwrap();
        });
        assert!(parse_set(
            &mut minicbor::Decoder::new(&bad),
            SetArgShape::Required,
            read_drep
        )
        .is_err());
    }

    /// `GovActionId` is `[bstr(32), Word16]` — note the 32-byte txId, not 28.
    #[test]
    fn gov_action_id_round_trips() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.array(2).unwrap();
            e.bytes(&[5u8; 32]).unwrap();
            e.u16(3).unwrap();
        });
        let got = parse_set(
            &mut minicbor::Decoder::new(&b),
            SetArgShape::Required,
            read_gov_action_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got, vec![(vec![5u8; 32], 3u32)]);
    }

    #[test]
    fn gov_action_id_rejects_a_28_byte_tx_id() {
        let b = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(1).unwrap();
            e.array(2).unwrap();
            e.bytes(&[5u8; 28]).unwrap();
            e.u16(0).unwrap();
        });
        assert!(parse_set(
            &mut minicbor::Decoder::new(&b),
            SetArgShape::Required,
            read_gov_action_id
        )
        .is_err());
    }

    /// `OnEmptySet` is the only thing that separates the two empty-set rules,
    /// and both are live upstream.
    #[test]
    fn on_empty_set_selects_between_the_two_upstream_rules() {
        let empty = enc(|e| {
            e.tag(minicbor::data::Tag::new(258)).unwrap();
            e.array(0).unwrap();
        });

        let all = filter_arg(
            &mut minicbor::Decoder::new(&empty),
            "q",
            SetArgShape::Required,
            OnEmptySet::AllItems,
            read_pool_id,
        )
        .unwrap();
        assert_eq!(all, None, "null-guard queries answer for everything");

        let none = filter_arg(
            &mut minicbor::Decoder::new(&empty),
            "q",
            SetArgShape::Required,
            OnEmptySet::NoItems,
            read_pool_id,
        )
        .unwrap();
        assert_eq!(none, Some(vec![]), "restrictKeys queries select nothing");
    }
}
