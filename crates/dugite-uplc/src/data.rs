//! The `Data` type — PlutusData.
//!
//! `Data` is the recursive sum type that the Cardano on-chain script
//! context (TxInfo) and per-redeemer datums/redeemers are encoded into.
//!
//! Encoding rules (mirroring the Haskell reference at
//! `IntersectMBO/plutus:plutus-core/.../PlutusCore/Data.hs`):
//!
//!  - `Constr i ds` where `0 ≤ i ≤ 6`  → CBOR tag `121 + i`, payload =
//!    CBOR array of the encoded `ds`.
//!  - `Constr i ds` where `7 ≤ i ≤ 127` → CBOR tag `1280 + (i - 7)`,
//!    payload = CBOR array of the encoded `ds`.
//!  - `Constr i ds` otherwise → CBOR tag `102`, payload = 2-element
//!    array `[i_as_u64, ds_as_array]`.
//!  - `Map es` → definite-length CBOR map (Haskell encoder always uses
//!    definite-length maps for Data).
//!  - `List ds` (and the args array of any `Constr i ds`) → encoded via
//!    the `cborg` `Serialise [a]` rule: an **empty** list is the
//!    definite-length `0x80` (`encodeListLen 0`); a **non-empty** list
//!    is the **indefinite**-length form `0x9f <items> 0xff`
//!    (`encodeListLenIndef … encodeBreak`). The decoder accepts either
//!    form. See `encode_list` for the byte-exact reproduction and the
//!    Haskell source quote.
//!  - `I i` → CBOR major-0/major-1 for `|i| < 2^64`; otherwise CBOR
//!    tag 2 (positive bignum) or tag 3 (negative bignum) wrapping a
//!    byte string ≤ 64 bytes.
//!  - `B bs` → definite-length CBOR byte string if `len ≤ 64`,
//!    otherwise indefinite-length with 64-byte chunks. The decoder
//!    accepts either form.
//!
//! **The encoder is not canonical.** Map entry order is preserved
//! as-is; duplicate keys are permitted. RFC 8949 §4.2 canonical form
//! is NOT enforced by the decoder.
//!
//! ## Defensive bounds
//!
//! The decoder threads an explicit depth counter (capped at
//! [`DATA_MAX_DEPTH`]) and rejects:
//!
//! - Definite-length `B` chunks longer than 64 bytes (Haskell
//!   `decodeBoundedBytes` invariant).
//! - Definite-length bignum payloads longer than 64 bytes.
//! - Pre-allocations on peer-controlled length headers exceeding the
//!   remaining buffer size (the same `safe_alloc_capacity` pattern as
//!   `dugite-serialization::decode::reader`).
//! - Recursion past [`DATA_MAX_DEPTH`].

use crate::UplcError;
use minicbor::data::{Tag, Type};
use minicbor::{Decoder, Encoder};
use num_bigint::{BigInt, Sign};

/// Maximum nesting depth accepted by the [`Data`] CBOR decoder. The
/// real on-chain limit is much lower than this; the cap is purely a
/// DoS guard against pathological adversarial input.
///
/// Haskell's `decodeData` has no explicit depth limit (it recurses on
/// the heap via GHC's native stack) and accepts far deeper nesting than
/// a naive cap would suggest — see #831 finding 3, which pins a
/// concrete case (258 nested indefinite lists) that Haskell decodes but
/// a 256 cap here would false-reject. This value matches
/// `dugite_serialization`'s consensus-path
/// `era_alonzo::MAX_PLUTUS_DATA_DEPTH`, so this decoder never
/// false-rejects a structure the consensus decoder accepts.
pub const DATA_MAX_DEPTH: usize = 1024;

/// Maximum payload length of a single definite-length byte / bignum
/// chunk, per the Haskell `decodeBoundedBytes` invariant.
const DATA_CHUNK_LIMIT: usize = 64;

/// Recursive PlutusData value. Maps 1:1 onto the Haskell
/// `Plutus.V1.Data` definition.
///
/// `Constr` and `Map` payloads are `Vec` rather than `BTreeMap` because
/// the on-chain encoding preserves insertion order — sorting on encode
/// would change the body hash for txs that gossip in non-sorted form,
/// breaking byte-exact round-trip.
///
/// `Clone`, `PartialEq`/`Eq`, `Hash`, and `Drop` are all hand-written
/// (not derived) as explicit-stack iterative traversals — see the impls
/// below (#832).
#[derive(Debug)]
pub enum Data {
    /// `Constr` — tagged sum: `Constr tag args`.
    ///
    /// The tag is an arbitrary-precision signed `Integer`, matching
    /// Haskell's `Data = Constr Integer [Data] | ...` — NOT a `u64`. A
    /// script can transiently construct a `Constr` with a negative or
    /// `> u64::MAX` tag via the `constrData` builtin at protocol
    /// versions before the van Rossem (PV11) `Word64`-unlifting gate
    /// (see `builtin::denotations::ConstrData` and issue #859/#828.5).
    /// The on-chain CBOR/flat wire encoding of a `Constr` tag is always
    /// a `Word64` on *decode* (a wide tag can only ever exist
    /// transiently inside a running script, never as a deserialised
    /// datum/redeemer/script literal) — see `encode_constr`/
    /// `decode_tagged` below.
    Constr(BigInt, Vec<Data>),
    /// `Map` — list of key-value pairs. Insertion order preserved;
    /// duplicates permitted.
    Map(Vec<(Data, Data)>),
    /// `List` — list of values.
    List(Vec<Data>),
    /// `I` — arbitrary-precision integer.
    I(BigInt),
    /// `B` — byte string.
    B(Vec<u8>),
}

/// Explicit-stack iterative structural equality.
///
/// A script can cheaply construct a `Data` value nested ~10^5–10^6 deep
/// (repeated `constrData`/`mkCons`/`listData`, each a separately-charged
/// builtin call) and then call `equalsData d d`, which compares via
/// `a == b`. The `#[derive(PartialEq)]` this replaces would recurse one
/// native stack frame per nesting level — adversarially deep input then
/// overflows the stack (process abort = remote DoS). This produces the
/// exact same result as the derived structural comparison (same
/// variant, same fields, elementwise, in order) for every input; only
/// the stack behavior changes. See #832.
impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        let mut stack: Vec<(&Data, &Data)> = vec![(self, other)];
        while let Some((a, b)) = stack.pop() {
            match (a, b) {
                (Data::Constr(ta, fa), Data::Constr(tb, fb)) => {
                    if ta != tb || fa.len() != fb.len() {
                        return false;
                    }
                    stack.extend(fa.iter().zip(fb.iter()));
                }
                (Data::Map(ea), Data::Map(eb)) => {
                    if ea.len() != eb.len() {
                        return false;
                    }
                    for ((ka, va), (kb, vb)) in ea.iter().zip(eb.iter()) {
                        stack.push((ka, kb));
                        stack.push((va, vb));
                    }
                }
                (Data::List(la), Data::List(lb)) => {
                    if la.len() != lb.len() {
                        return false;
                    }
                    stack.extend(la.iter().zip(lb.iter()));
                }
                (Data::I(ia), Data::I(ib)) => {
                    if ia != ib {
                        return false;
                    }
                }
                (Data::B(ba), Data::B(bb)) => {
                    if ba != bb {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

impl Eq for Data {}

/// Explicit-stack iterative deep clone.
///
/// `Data` holds its recursive children by value (`Vec<Data>` /
/// `Vec<(Data, Data)>`), so `#[derive(Clone)]` recurses one native stack
/// frame per nesting level — the identical latent stack-overflow class
/// that `PartialEq`, `Hash`, and `Drop` were hand-written to avoid
/// (#832), and it was the one impl left derived. A deep clone is still
/// reachable on the CEK hot path even after `Constant::Data` became
/// `Rc`-shared (#838 Fix 2): a `Var` lookup (`machine::step`) now clones
/// only the `Rc` pointer for the *common* case, but a builtin that
/// destructures a *shared* `Data` (`Rc::strong_count > 1` — e.g.
/// `unConstrData` on a ScriptContext still referenced elsewhere in the
/// env) falls back to exactly this deep clone
/// (`builtin::denotations::data_from_rc`). A nesting depth an adversary
/// drove to ~10^5–10^6 (repeated `constrData`/`listData`/`mapData`, each
/// a separately-charged builtin) would overflow the phase-2 evaluation
/// stack (unguarded, a 2 MiB rayon worker) = process abort = remote DoS
/// on that fallback path just as much as on the old always-clone path.
///
/// This produces the exact same value as the derived clone; only the
/// stack behavior changes. The clone is assembled bottom-up: a work-stack
/// of `Task`s drives a post-order traversal, each finished child is
/// pushed onto `out`, and a `Build*` task pops its children back off
/// `out` to reassemble the parent. Children are pushed in reverse so they
/// pop — and therefore land in `out` — in source order (a reordered
/// `Map`/`Constr` clone would change the body hash: byte-exactness is not
/// optional here).
impl Clone for Data {
    fn clone(&self) -> Self {
        enum Task<'a> {
            Descend(&'a Data),
            BuildConstr(BigInt, usize),
            BuildList(usize),
            BuildMap(usize),
        }
        let mut tasks: Vec<Task<'_>> = vec![Task::Descend(self)];
        let mut out: Vec<Data> = Vec::new();
        while let Some(task) = tasks.pop() {
            match task {
                Task::Descend(d) => match d {
                    Data::Constr(tag, args) => {
                        tasks.push(Task::BuildConstr(tag.clone(), args.len()));
                        for child in args.iter().rev() {
                            tasks.push(Task::Descend(child));
                        }
                    }
                    Data::List(items) => {
                        tasks.push(Task::BuildList(items.len()));
                        for child in items.iter().rev() {
                            tasks.push(Task::Descend(child));
                        }
                    }
                    Data::Map(entries) => {
                        tasks.push(Task::BuildMap(entries.len()));
                        // Push value-then-key within each pair, over the
                        // entries in reverse, so keys and values pop back in
                        // source order (k0, v0, k1, v1, …).
                        for (k, v) in entries.iter().rev() {
                            tasks.push(Task::Descend(v));
                            tasks.push(Task::Descend(k));
                        }
                    }
                    // Leaves clone directly — bounded, non-recursive.
                    Data::I(n) => out.push(Data::I(n.clone())),
                    Data::B(b) => out.push(Data::B(b.clone())),
                },
                Task::BuildConstr(tag, n) => {
                    let start = out.len() - n;
                    let args = out.split_off(start);
                    out.push(Data::Constr(tag, args));
                }
                Task::BuildList(n) => {
                    let start = out.len() - n;
                    let items = out.split_off(start);
                    out.push(Data::List(items));
                }
                Task::BuildMap(n) => {
                    let start = out.len() - 2 * n;
                    let flat = out.split_off(start);
                    let mut entries = Vec::with_capacity(n);
                    let mut it = flat.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        entries.push((k, v));
                    }
                    out.push(Data::Map(entries));
                }
            }
        }
        match out.pop() {
            Some(root) => root,
            // Unreachable: every completed subtree pushes exactly one node
            // onto `out`, and `self` is one subtree, so `out` ends with
            // exactly one root.
            None => unreachable!("clone traversal always reduces to exactly one root node"),
        }
    }
}

/// Explicit-stack iterative `Hash`, matching the iterative `PartialEq`
/// above (clippy's `derived_hash_with_manual_eq` correctly flags a
/// derived `Hash` alongside a hand-written `PartialEq` as a
/// consistency risk otherwise). Not currently load-bearing — `Data` is
/// never used as a hash-map/set key anywhere in this crate — but kept
/// consistent and non-recursive rather than suppressing the lint,
/// closing the same latent-recursion class of issue as `PartialEq` and
/// `Drop` (#832).
///
/// Push/pop order need not match a left-to-right recursive traversal —
/// it only needs to be deterministic for a given value, which the
/// explicit stack guarantees, so equal `Data` values always hash
/// identically.
impl std::hash::Hash for Data {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let mut stack: Vec<&Data> = vec![self];
        while let Some(d) = stack.pop() {
            match d {
                Data::Constr(tag, args) => {
                    0u8.hash(state);
                    tag.hash(state);
                    args.len().hash(state);
                    stack.extend(args.iter());
                }
                Data::Map(entries) => {
                    1u8.hash(state);
                    entries.len().hash(state);
                    for (k, v) in entries {
                        stack.push(k);
                        stack.push(v);
                    }
                }
                Data::List(items) => {
                    2u8.hash(state);
                    items.len().hash(state);
                    stack.extend(items.iter());
                }
                Data::I(n) => {
                    3u8.hash(state);
                    n.hash(state);
                }
                Data::B(b) => {
                    4u8.hash(state);
                    b.hash(state);
                }
            }
        }
    }
}

impl Drop for Data {
    /// Explicit-stack iterative teardown.
    ///
    /// `Data` is directly self-recursive (`Vec<Data>`/`Vec<(Data, Data)>`
    /// fields holding `Data` by value) — unlike the CEK env's cons-list
    /// (`machine::env::Env`), which is `Rc`-indirected through a
    /// *separate*, non-`Drop` `Node` type. That distinction matters: a
    /// naive port of "take children into a worklist, let each emptied
    /// shell drop normally" does NOT terminate here, because letting a
    /// popped, already-emptied `Data` value fall out of scope at the end
    /// of a loop iteration re-enters `Data`'s own `Drop::drop` — forever,
    /// for every value, since there is no separate non-`Drop` type to
    /// bottom out into.
    ///
    /// Instead, for every popped node this: (1) moves any `Data`
    /// children onto the explicit work-stack via `mem::take` on the
    /// container field — sound even though `Data: Drop`, since
    /// `mem::take` always leaves a fully-initialized replacement value
    /// behind rather than partially moving out of a live place; (2)
    /// explicitly drops any genuinely-owned non-`Data` leaf payload
    /// (`BigInt`/`Vec<u8>`) — including a `Constr` node's own `BigInt`
    /// tag, which is heap-backed and MUST be taken-and-dropped here
    /// too, not left for the final `mem::forget` — via the same
    /// `mem::take`-then-drop technique; and (3) `mem::forget`s the
    /// now-inert shell so its own `Drop::drop` is never invoked again.
    /// Step (3) is leak-free: by that point the shell owns only
    /// default/zero-capacity containers (`Vec::new()`, `BigInt::ZERO` —
    /// both non-heap-allocating).
    fn drop(&mut self) {
        let taken = std::mem::replace(self, Data::B(Vec::new()));
        let mut stack = vec![taken];
        while let Some(mut node) = stack.pop() {
            match &mut node {
                Data::Constr(tag, args) => {
                    std::mem::drop(std::mem::take(tag));
                    stack.extend(std::mem::take(args));
                }
                Data::List(items) => stack.extend(std::mem::take(items)),
                Data::Map(entries) => {
                    for (k, v) in std::mem::take(entries) {
                        stack.push(k);
                        stack.push(v);
                    }
                }
                Data::I(n) => std::mem::drop(std::mem::take(n)),
                Data::B(b) => std::mem::drop(std::mem::take(b)),
            }
            std::mem::forget(node);
        }
    }
}

impl Data {
    /// Ergonomic `Constr` constructor: accepts anything `Into<BigInt>`
    /// (small integer literals included) so call sites that only ever
    /// build small, in-range tags don't need to spell out
    /// `BigInt::from(..)` themselves. Semantically identical to
    /// `Data::Constr(tag.into(), args)`.
    pub fn constr(tag: impl Into<BigInt>, args: Vec<Data>) -> Data {
        Data::Constr(tag.into(), args)
    }

    /// Consume `self`, extracting the `Constr` tag/args if that's the
    /// variant, or handing `self` back unchanged (`Err`) otherwise.
    ///
    /// Once `Data` implements `Drop` (#832), a plain
    /// `match self { Data::Constr(tag, args) => .., }` on an *owned*
    /// `Data` value no longer compiles (E0509: partial moves out of a
    /// type that implements `Drop` are disallowed). This extracts the
    /// payload via `mem::take` through a `&mut self` borrow instead —
    /// sound because `mem::take` always leaves a fully-initialized
    /// replacement in the field, and the shallow leftover (`self`'s
    /// `Vec` is now empty) drops in O(1) with no recursion when this
    /// function returns.
    pub fn into_constr(mut self) -> Result<(BigInt, Vec<Data>), Data> {
        match &mut self {
            Data::Constr(tag, args) => Ok((std::mem::take(tag), std::mem::take(args))),
            _ => Err(self),
        }
    }

    /// See [`Data::into_constr`]; extracts the `Map` entries.
    pub fn into_map(mut self) -> Result<Vec<(Data, Data)>, Data> {
        match &mut self {
            Data::Map(entries) => Ok(std::mem::take(entries)),
            _ => Err(self),
        }
    }

    /// See [`Data::into_constr`]; extracts the `List` items.
    pub fn into_list(mut self) -> Result<Vec<Data>, Data> {
        match &mut self {
            Data::List(items) => Ok(std::mem::take(items)),
            _ => Err(self),
        }
    }

    /// See [`Data::into_constr`]; extracts the `I` integer.
    pub fn into_integer(mut self) -> Result<BigInt, Data> {
        match &mut self {
            Data::I(n) => Ok(std::mem::take(n)),
            _ => Err(self),
        }
    }

    /// See [`Data::into_constr`]; extracts the `B` byte string.
    pub fn into_bytes(mut self) -> Result<Vec<u8>, Data> {
        match &mut self {
            Data::B(b) => Ok(std::mem::take(b)),
            _ => Err(self),
        }
    }

    /// Encode the `Data` value to its canonical-on-chain CBOR bytes.
    pub fn to_cbor(&self) -> Result<Vec<u8>, UplcError> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out);
        encode_data(&mut e, self).map_err(|err| UplcError::Encode(err.to_string()))?;
        Ok(out)
    }

    /// Decode a `Data` value from CBOR bytes.
    ///
    /// Defensive invariants:
    ///
    ///  - Recursion depth is capped at [`DATA_MAX_DEPTH`].
    ///  - Definite-length byte / bignum chunks ≤ 64 bytes.
    ///  - `Vec::with_capacity` clamped to `min(declared, remaining_bytes)`.
    ///  - Trailing bytes after the outer value cause a `CborDecode` error.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, UplcError> {
        let mut d = Decoder::new(bytes);
        let data = decode_data(&mut d, 0)?;
        if d.position() != bytes.len() {
            return Err(UplcError::CborDecode(format!(
                "trailing {} bytes after Data value",
                bytes.len() - d.position()
            )));
        }
        Ok(data)
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

fn encode_data<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    data: &Data,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // `encode_data` recurses mutually with `encode_constr`/`encode_args`/
    // `encode_list` for every level of nesting. Machine-*constructed*
    // `Data` (e.g. via repeated `mkCons`/`constrData` builtin calls) never
    // passes through the depth-capped CBOR decoder at all, so an
    // adversarially deep value reaching `serialiseData`/`to_cbor` could
    // still overflow the native stack here even after `decode_data` is
    // hardened. Extend the stack transparently, mirroring `decode_data`
    // and the flat term decoder (`flat/term.rs`). See #832.
    stacker::maybe_grow(128 * 1024, 1024 * 1024, || encode_data_inner(e, data))
}

fn encode_data_inner<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    data: &Data,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    match data {
        Data::Constr(tag, args) => encode_constr(e, tag, args),
        Data::Map(entries) => {
            // Haskell's encoder always uses definite-length CBOR maps
            // for Data. The decoder accepts indefinite, but byte-exact
            // round-trip requires definite on the encode path.
            e.map(entries.len() as u64)?;
            for (k, v) in entries {
                encode_data(e, k)?;
                encode_data(e, v)?;
            }
            Ok(())
        }
        Data::List(items) => encode_list(e, items),
        Data::I(n) => encode_integer(e, n),
        Data::B(bs) => encode_bytes(e, bs),
    }
}

fn encode_constr<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    tag: &BigInt,
    args: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // The compact tag-121..127 / tag-1280..1400 forms only apply to a
    // tag in `[0, 127]`; anything else (including any negative or
    // >u64::MAX tag a script can transiently build via `constrData` at
    // PV<11 — see #859) falls through to the general tag-102 form. This
    // matches Haskell's `encodeData`, whose `n >= 0 && n < 7` / `n >= 7
    // && n < 128` guards are plain `Integer` comparisons that are simply
    // false for a negative or huge `n`.
    if let Ok(small) = u64::try_from(tag) {
        if small <= 6 {
            e.tag(Tag::new(121 + small))?;
            return encode_args(e, args);
        } else if small <= 127 {
            e.tag(Tag::new(1280 + (small - 7)))?;
            return encode_args(e, args);
        }
    }
    // General tag-102 form: `encodeTag 102 <> encodeListLen 2 <>
    // encodeInteger n <> encode ds`. `encodeInteger` here is the exact
    // same arbitrary-precision integer encoding used for `Data::I` — for
    // an in-range tag this reproduces the plain `e.u64(tag)` byte-exact
    // (regression-tested), and for an out-of-range tag (never
    // on-chain-decodable — decode always requires Word64, see
    // `decode_tagged`) it degrades "softly" to the bignum/negint form
    // rather than panicking, matching Haskell's `encodeInteger` which
    // has no upper/lower bound.
    e.tag(Tag::new(102))?;
    e.array(2)?;
    encode_integer(e, tag)?;
    encode_args(e, args)?;
    Ok(())
}

fn encode_args<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    args: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // `Constr i ds` is encoded in Haskell as
    //   `encodeTag t <> encode ds`
    // where `encode ds` reuses the `Serialise [Data]` instance — see
    // `encode_list` for the exact empty / non-empty split.
    encode_list(e, args)
}

/// Encode a list of `Data` using the `Serialise [a]` rule from `cborg`.
///
/// This is the byte-exact behaviour required by the Plutus `serialiseData`
/// builtin, whose Haskell denotation is a *structural canonical re-encode*
/// (NOT a memoised verbatim copy of the on-chain bytes):
///
/// ```text
/// -- plutus-core/.../PlutusCore/Default/Builtins.hs
/// toBuiltinMeaning _semvar SerialiseData =
///     let serialiseDataDenotation :: Data -> BS.ByteString
///         serialiseDataDenotation = BSL.toStrict . serialise
///      in makeBuiltinMeaning serialiseDataDenotation
///           (runCostingFunOneArgument . paramSerialiseData)
/// ```
///
/// `serialise` is `Codec.Serialise.serialise` over the `Serialise Data`
/// instance (`encode = encodeData`). `encodeData` emits `List ds -> encode ds`
/// and `Constr i ds -> encodeTag … <> encode ds`, where `encode ds` reuses
/// the `cborg` `Serialise [a]` instance:
///
/// ```text
/// -- serialise: Codec/Serialise/Class.hs   (defaultEncodeList)
/// defaultEncodeList []     = encodeListLen 0
/// defaultEncodeList (x:xs) = encodeListLenIndef
///                          <> foldr (\v r -> encode v <> r) encodeBreak (x:xs)
/// ```
///
/// i.e. an empty list is `0x80` (definite length zero); a non-empty list
/// is `0x9f <items> 0xff` (indefinite length). Because `serialise` ALWAYS
/// re-encodes structurally, a non-canonical on-chain `Data` (e.g. a
/// definite-length non-empty Constr-args array) is re-encoded into this
/// indefinite form — the original verbatim bytes are NOT preserved. We
/// therefore must reproduce this empty/non-empty asymmetry byte-for-byte
/// (rather than memoise the input bytes) to keep
/// `blake2b256(serialiseData d)` equal to cardano-node's.
fn encode_list<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    items: &[Data],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    if items.is_empty() {
        e.array(0)?;
    } else {
        e.begin_array()?;
        for item in items {
            encode_data(e, item)?;
        }
        e.end()?;
    }
    Ok(())
}

fn encode_integer<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    n: &BigInt,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // CBOR major-0/major-1 hold values in `[0, 2^64)` (major 0) and
    // `[-2^64, -1]` (major 1, encoding `-1 - k`). Anything outside
    // that range uses the bignum tag form.
    //
    // Strategy:
    //   - non-negative `n ≤ u64::MAX` → `e.u64(n)`.
    //   - negative `n ≥ -2^63` (i.e. fits in `i64`) → `e.i64(n)`.
    //   - negative `n` in `[-2^64, -2^63 - 1)` → hand-encode major-1
    //     with the internal `k = -1 - n` value (which fits in `u64`).
    //   - everything else → bignum tag (`encode_bignum`).
    match n.sign() {
        Sign::NoSign => {
            e.u64(0)?;
        }
        Sign::Plus => {
            if n.iter_u64_digits().count() == 1 {
                if let Some(v) = n.iter_u64_digits().next() {
                    e.u64(v)?;
                    return Ok(());
                }
            }
            return encode_bignum(e, n);
        }
        Sign::Minus => {
            // `mag` is `-n`, always `> 0`.
            let mag: BigInt = -n;
            // Internal CBOR negint value `k = mag - 1`. Fits in `u64`
            // iff `mag <= 2^64`.
            let k: BigInt = &mag - 1;
            if k.iter_u64_digits().count() <= 1 {
                let k_u64 = k.iter_u64_digits().next().unwrap_or(0);
                if k_u64 <= i64::MAX as u64 {
                    // Safe to go through `i64` — the represented value
                    // `-1 - k_u64` lies in `[i64::MIN + 1, -1]`, which
                    // is representable.
                    e.i64(-1 - (k_u64 as i64))?;
                } else {
                    // `k_u64` is in `(i64::MAX, u64::MAX]`. We hand-write
                    // the major-1 CBOR encoding so we can carry the
                    // full u64 range. Layout: 0x3b (major 1, ai = 27)
                    // followed by the 8-byte big-endian internal value.
                    encode_raw_negint_u64(e, k_u64)?;
                }
                return Ok(());
            }
            return encode_bignum(e, n);
        }
    }
    Ok(())
}

fn encode_raw_negint_u64<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    k: u64,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    // 0x20 = major 1, 0x1b = additional info 27 (8-byte length).
    // The `Encoder::writer_mut()` accessor exposes the underlying
    // sink so we can write the negint header + payload directly,
    // sidestepping the lack of a `u64`-range negint method on the
    // public Encoder API.
    let mut header = [0u8; 9];
    header[0] = 0x3b;
    header[1..].copy_from_slice(&k.to_be_bytes());
    e.writer_mut()
        .write_all(&header)
        .map_err(minicbor::encode::Error::write)?;
    Ok(())
}

fn encode_bignum<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    n: &BigInt,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    let (sign, magnitude): (Tag, BigInt) = if n.sign() == Sign::Minus {
        // CBOR tag 3 encodes `-1 - n`, so the payload bytes are the
        // big-endian bytes of `-1 - n`.
        let adjusted: BigInt = -BigInt::from(1) - n;
        (Tag::new(3), adjusted)
    } else {
        (Tag::new(2), n.clone())
    };
    let mut bytes = magnitude.to_bytes_be().1;
    if bytes.is_empty() {
        bytes.push(0);
    }
    e.tag(sign)?;
    encode_bytes_raw(e, &bytes)?;
    Ok(())
}

fn encode_bytes<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    bs: &[u8],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    encode_bytes_raw(e, bs)
}

fn encode_bytes_raw<W: minicbor::encode::Write>(
    e: &mut Encoder<W>,
    bs: &[u8],
) -> Result<(), minicbor::encode::Error<W::Error>> {
    if bs.len() <= DATA_CHUNK_LIMIT {
        e.bytes(bs)?;
    } else {
        e.begin_bytes()?;
        for chunk in bs.chunks(DATA_CHUNK_LIMIT) {
            e.bytes(chunk)?;
        }
        e.end()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

fn decode_data(d: &mut Decoder<'_>, depth: usize) -> Result<Data, UplcError> {
    if depth > DATA_MAX_DEPTH {
        return Err(UplcError::CborDecode(format!(
            "Data nesting depth exceeds limit ({DATA_MAX_DEPTH})"
        )));
    }
    // `decode_data` recurses mutually with `decode_tagged`/`decode_array`/
    // `decode_map`, so raising DATA_MAX_DEPTH to 1024 (to match the
    // consensus decoder, #831 finding 3) pushes hundreds of stack frames
    // in debug builds — enough to overflow the default thread stack
    // before the depth check itself ever fires. Extend the stack
    // transparently rather than relying on the depth cap alone to bound
    // native stack usage, mirroring the flat term decoder's identical
    // use of `stacker::maybe_grow` (`flat/term.rs`).
    stacker::maybe_grow(128 * 1024, 1024 * 1024, || decode_data_inner(d, depth))
}

fn decode_data_inner(d: &mut Decoder<'_>, depth: usize) -> Result<Data, UplcError> {
    let ty = d
        .datatype()
        .map_err(|e| UplcError::CborDecode(format!("datatype: {e}")))?;
    match ty {
        Type::Tag => decode_tagged(d, depth),
        Type::Array | Type::ArrayIndef => Ok(Data::List(decode_array(d, depth)?)),
        Type::Map | Type::MapIndef => Ok(Data::Map(decode_map(d, depth)?)),
        Type::Bytes | Type::BytesIndef => Ok(Data::B(decode_bytes(d)?)),
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            let v = d
                .u64()
                .map_err(|e| UplcError::CborDecode(format!("uint: {e}")))?;
            Ok(Data::I(BigInt::from(v)))
        }
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
            let v = d
                .i64()
                .map_err(|e| UplcError::CborDecode(format!("int: {e}")))?;
            Ok(Data::I(BigInt::from(v)))
        }
        Type::Int => {
            // Wide negative outside i64 range — read as `Int` and
            // promote to BigInt.
            let v = d
                .int()
                .map_err(|e| UplcError::CborDecode(format!("int: {e}")))?;
            // minicbor's `Int` is a signed-CBOR-int wrapper; convert
            // via its display, then parse — narrow and slow but works
            // for the rare case.
            let s = v.to_string();
            let parsed = s
                .parse::<BigInt>()
                .map_err(|e| UplcError::CborDecode(format!("bigint parse: {e}")))?;
            Ok(Data::I(parsed))
        }
        other => Err(UplcError::CborDecode(format!(
            "unexpected CBOR major for Data: {other:?}"
        ))),
    }
}

fn decode_tagged(d: &mut Decoder<'_>, depth: usize) -> Result<Data, UplcError> {
    // #831 finding 2: `cborg`'s `peekTokenType` (which the Haskell
    // `PlutusData` `Serialise` instance is built on) maps ONLY the
    // 1-byte inline tag header (`0xc2` positive / `0xc3` negative) to
    // `TypeInteger` -> the bignum decode path. Any wider, non-minimal
    // encoding of the same tag value (e.g. `d8 02`, `d9 00 02`) is
    // `TypeTag` -> `decodeConstr`, which fails since 2/3 aren't valid
    // constructor tags. Peek the raw header byte BEFORE consuming the
    // tag so a wide-form 2/3 is routed to the rejection arm, matching
    // Haskell's acceptance boundary exactly.
    let is_inline_bignum_header = matches!(
        d.input().get(d.position()).copied(),
        Some(0xc2) | Some(0xc3)
    );
    let tag = d
        .tag()
        .map_err(|e| UplcError::CborDecode(format!("tag: {e}")))?
        .as_u64();
    match (is_inline_bignum_header, tag) {
        (true, 2) => {
            // Positive bignum.
            let bytes = decode_bytes(d)?;
            Ok(Data::I(BigInt::from_bytes_be(Sign::Plus, &bytes)))
        }
        (true, 3) => {
            // Negative bignum: stored as `-1 - n`, so the result is
            // `-1 - magnitude`.
            let bytes = decode_bytes(d)?;
            let magnitude = BigInt::from_bytes_be(Sign::Plus, &bytes);
            Ok(Data::I(-BigInt::from(1) - magnitude))
        }
        (_, tag @ 121..=127) => {
            let constr_tag = tag - 121;
            let args = decode_array(d, depth)?;
            Ok(Data::Constr(BigInt::from(constr_tag), args))
        }
        (_, tag @ 1280..=1400) => {
            let constr_tag = tag - 1280 + 7;
            let args = decode_array(d, depth)?;
            Ok(Data::Constr(BigInt::from(constr_tag), args))
        }
        (_, 102) => {
            // #831 finding 1: Haskell's `decodeConstrExtended` uses
            // `decodeListLenOrIndef`, which accepts BOTH a
            // definite-length array(2) and an indefinite-length array
            // closed by an explicit break — it does not require
            // definite-length here. Accept both forms, consuming the
            // trailing break for the indefinite case.
            let len = d
                .array()
                .map_err(|e| UplcError::CborDecode(format!("constr-102 outer: {e}")))?;
            match len {
                Some(2) => {
                    let constr_tag = d
                        .u64()
                        .map_err(|e| UplcError::CborDecode(format!("constr-102 tag: {e}")))?;
                    let args = decode_array(d, depth)?;
                    Ok(Data::Constr(BigInt::from(constr_tag), args))
                }
                Some(n) => Err(UplcError::CborDecode(format!(
                    "tag 102: expected array(2), got Some({n})"
                ))),
                None => {
                    let constr_tag = d
                        .u64()
                        .map_err(|e| UplcError::CborDecode(format!("constr-102 tag: {e}")))?;
                    let args = decode_array(d, depth)?;
                    expect_break(d)?;
                    Ok(Data::Constr(BigInt::from(constr_tag), args))
                }
            }
        }
        (_, other) => Err(UplcError::CborDecode(format!(
            "unsupported CBOR tag for Data: {other}"
        ))),
    }
}

/// Consume exactly one CBOR break byte (`0xff`) at the current position.
///
/// Mirrors `dugite_serialization::decode::reader::Reader::expect_break`
/// — closes an indefinite-length structural array whose entries were
/// read individually rather than via [`decode_array`]. Used to close
/// the outer indefinite-length `[i, fields]` array of an indefinite
/// tag-102 `Constr` (#831 finding 1).
fn expect_break(d: &mut Decoder<'_>) -> Result<(), UplcError> {
    match d
        .datatype()
        .map_err(|e| UplcError::CborDecode(format!("expect_break: {e}")))?
    {
        Type::Break => d
            .skip()
            .map_err(|e| UplcError::CborDecode(format!("expect_break: {e}"))),
        other => Err(UplcError::CborDecode(format!(
            "expected CBOR break (0xff) to close indefinite tag-102 array, found {other:?}"
        ))),
    }
}

fn decode_array(d: &mut Decoder<'_>, depth: usize) -> Result<Vec<Data>, UplcError> {
    let header = d
        .array()
        .map_err(|e| UplcError::CborDecode(format!("array: {e}")))?;
    match header {
        Some(n) => {
            let cap = safe_alloc_capacity(n, d.input(), d.position());
            let mut out = Vec::with_capacity(cap);
            for _ in 0..n {
                out.push(decode_data(d, depth + 1)?);
            }
            Ok(out)
        }
        None => {
            let mut out = Vec::new();
            loop {
                match d
                    .datatype()
                    .map_err(|e| UplcError::CborDecode(format!("array-indef peek: {e}")))?
                {
                    Type::Break => {
                        d.skip().map_err(|e| {
                            UplcError::CborDecode(format!("array-indef break: {e}"))
                        })?;
                        break;
                    }
                    _ => out.push(decode_data(d, depth + 1)?),
                }
            }
            Ok(out)
        }
    }
}

fn decode_map(d: &mut Decoder<'_>, depth: usize) -> Result<Vec<(Data, Data)>, UplcError> {
    let header = d
        .map()
        .map_err(|e| UplcError::CborDecode(format!("map: {e}")))?;
    match header {
        Some(n) => {
            let cap = safe_alloc_capacity(n, d.input(), d.position());
            let mut out = Vec::with_capacity(cap);
            for _ in 0..n {
                let k = decode_data(d, depth + 1)?;
                let v = decode_data(d, depth + 1)?;
                out.push((k, v));
            }
            Ok(out)
        }
        None => {
            let mut out = Vec::new();
            loop {
                match d
                    .datatype()
                    .map_err(|e| UplcError::CborDecode(format!("map-indef peek: {e}")))?
                {
                    Type::Break => {
                        d.skip()
                            .map_err(|e| UplcError::CborDecode(format!("map-indef break: {e}")))?;
                        break;
                    }
                    _ => {
                        let k = decode_data(d, depth + 1)?;
                        let v = decode_data(d, depth + 1)?;
                        out.push((k, v));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn decode_bytes(d: &mut Decoder<'_>) -> Result<Vec<u8>, UplcError> {
    let ty = d
        .datatype()
        .map_err(|e| UplcError::CborDecode(format!("bytes peek: {e}")))?;
    match ty {
        Type::Bytes => {
            let slice = d
                .bytes()
                .map_err(|e| UplcError::CborDecode(format!("bytes: {e}")))?;
            if slice.len() > DATA_CHUNK_LIMIT {
                return Err(UplcError::CborDecode(format!(
                    "definite-length byte string of {} bytes exceeds chunk limit ({DATA_CHUNK_LIMIT})",
                    slice.len()
                )));
            }
            Ok(slice.to_vec())
        }
        Type::BytesIndef => {
            // Consume the 0x5f header.
            let iter = d
                .bytes_iter()
                .map_err(|e| UplcError::CborDecode(format!("bytes-indef: {e}")))?;
            let mut out = Vec::new();
            for chunk in iter {
                let chunk =
                    chunk.map_err(|e| UplcError::CborDecode(format!("bytes-indef chunk: {e}")))?;
                if chunk.len() > DATA_CHUNK_LIMIT {
                    return Err(UplcError::CborDecode(format!(
                        "indefinite-length chunk of {} bytes exceeds chunk limit ({DATA_CHUNK_LIMIT})",
                        chunk.len()
                    )));
                }
                out.extend_from_slice(chunk);
            }
            Ok(out)
        }
        other => Err(UplcError::CborDecode(format!(
            "expected byte string, got {other:?}"
        ))),
    }
}

/// Cap an initial `Vec::with_capacity` value derived from a CBOR
/// length header so a forged length can't drive a multi-exabyte
/// allocation. Mirrors `dugite-serialization`'s `safe_alloc_capacity`.
fn safe_alloc_capacity(declared: u64, input: &[u8], pos: usize) -> usize {
    let remaining = input.len().saturating_sub(pos);
    usize::try_from(declared)
        .unwrap_or(usize::MAX)
        .min(remaining)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(d: &Data) -> Data {
        let bytes = d.to_cbor().expect("encode");
        Data::from_cbor(&bytes).expect("decode")
    }

    #[test]
    fn roundtrip_small_int() {
        for n in [-1000i64, -1, 0, 1, 100, 1_000_000] {
            let d = Data::I(BigInt::from(n));
            assert_eq!(rt(&d), d, "n={n}");
        }
    }

    #[test]
    fn roundtrip_bignum_positive() {
        // 2^65 — needs the positive bignum tag.
        let n = BigInt::from(1u64) << 65;
        let d = Data::I(n);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_bignum_negative() {
        // -(2^65)
        let pos: BigInt = BigInt::from(1u64) << 65;
        let n = -pos;
        let d = Data::I(n);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_empty_bytes() {
        let d = Data::B(vec![]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_small_bytes() {
        let d = Data::B((0..64u8).collect());
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_large_bytes_indefinite() {
        // 200-byte string — encoded indefinite with chunks of 64.
        let d = Data::B((0..200u8).collect());
        let bytes = d.to_cbor().unwrap();
        // First byte must be 0x5f (indefinite-length byte string).
        assert_eq!(bytes[0], 0x5f, "expected indefinite-length byte string");
        assert_eq!(rt(&d), d);
    }

    // ─────────────────────────────────────────────────────────────────
    // `serialiseData` byte-exactness vs Haskell `encodeData` / cborg.
    //
    // Haskell `serialiseData` is `BSL.toStrict . serialise` — a
    // *structural canonical re-encode*. The `Serialise [a]` instance
    // (`defaultEncodeList`) renders an empty list as the definite `0x80`
    // and any non-empty list as the indefinite `0x9f … 0xff`. These
    // tests pin that asymmetry and, crucially, prove that a non-canonical
    // (definite-length, non-empty) input is RE-ENCODED to indefinite —
    // i.e. `to_cbor()` is NOT a memoised verbatim copy of the input.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn empty_constr_args_are_definite_0x80() {
        // Constr 0 [] → tag 121 (0xd879) + empty list (0x80).
        let d = Data::Constr(BigInt::from(0), vec![]);
        assert_eq!(hex::encode(d.to_cbor().unwrap()), "d87980");
        // Bare empty List → 0x80.
        assert_eq!(hex::encode(Data::List(vec![]).to_cbor().unwrap()), "80");
    }

    #[test]
    fn nonempty_constr_args_are_indefinite_0x9f_0xff() {
        // Constr 1 [ Constr 0 [ B 0xab, I 7 ] ]
        //   → d87a 9f ( d879 9f 41ab 07 ff ) ff
        // This is the on-chain shape `d87a9fd8799f…` of the failing-tx
        // datum: the args arrays are INDEFINITE-length.
        let d = Data::Constr(
            BigInt::from(1),
            vec![Data::Constr(
                BigInt::from(0),
                vec![Data::B(vec![0xab]), Data::I(BigInt::from(7))],
            )],
        );
        assert_eq!(hex::encode(d.to_cbor().unwrap()), "d87a9fd8799f41ab07ffff");
        // Bare non-empty List uses the same indefinite framing.
        let l = Data::List(vec![Data::I(BigInt::from(1)), Data::I(BigInt::from(2))]);
        assert_eq!(hex::encode(l.to_cbor().unwrap()), "9f0102ff");
    }

    #[test]
    fn definite_input_is_reencoded_to_indefinite_not_memoised() {
        // Feed a DEFINITE-length non-empty Constr (`d87a81…`) — a
        // perfectly valid, decodable, but non-canonical encoding. Haskell
        // `serialise` re-encodes it to the indefinite form; so must we.
        // If `serialiseData` instead returned the *verbatim* input bytes
        // (a "memoise the original" implementation), this assertion would
        // fail and the resulting hash would diverge from cardano-node.
        let definite_input = hex::decode("d87a81d8798241ab07").unwrap();
        let d = Data::from_cbor(&definite_input).unwrap();
        let reenc = d.to_cbor().unwrap();
        assert_eq!(
            hex::encode(&reenc),
            "d87a9fd8799f41ab07ffff",
            "definite-length input must be re-encoded as indefinite"
        );
        assert_ne!(
            definite_input, reenc,
            "to_cbor() must NOT echo the verbatim (non-canonical) input"
        );
    }

    #[test]
    fn gold_failing_tx_datum_round_trips_byte_exact() {
        // Real on-chain PlutusV3 datum from preprod tx
        // d653e36923…(creation) — its blake2b256 is the on-chain
        // datum_hash bbd352028feffe9a80a2822b46b9858bc1cf883cff383e1191b47d27ed708eb0.
        // Source: Koios preprod /datum_info. 276 bytes, 8 indefinite-length
        // CBOR arrays (`d87a9fd8799f…`). Decoding then re-encoding with
        // `to_cbor()` must reproduce the bytes EXACTLY, which is what makes
        // `serialiseData(datum)` hash to the on-chain datum_hash and lets
        // PlutusV3 7afbde08's `blake2b256(serialiseData datum) == datum_hash`
        // check pass.
        const GOLD_DATUM_HEX: &str = "d87a9fd8799fd8799fd8799f581c9929c128c357ff9b7bdd79ee69d3540e87da001777f15a4c914928dcffd87a80ff1a017d78401a004c4b401a0243d5801b0000019e5bf806edd8799fd8799f581c43d7590ef124ba849222553b19fb84d056a7306dbcfec925002896f3ffd87a80ff58205afe303b6b0feae7632926b07e73921978d8fa7f02ca358a8676de1d3381b89c582097450e7fc42aa1f45e9f0abda20d32024bc40c3351a390ec409a42951657b2c858201dec2a9a7014a1fa0ae3b3fb7d8b483e5f40c427627d8b1c73f3fec282904d62581c57a437cbed5709a2214d40bdf44eb08d1b88e97967798d83ec774fb6581c535e4be12d936e564b44b33618f2ae55090b1ac0f3be37ef8beb60e7ffff";
        let datum = hex::decode(GOLD_DATUM_HEX).unwrap();
        assert_eq!(datum.len(), 276);
        let d = Data::from_cbor(&datum).unwrap();
        let reenc = d.to_cbor().unwrap();
        assert_eq!(
            reenc, datum,
            "serialiseData(datum) must reproduce the verbatim-equivalent 276-byte canonical form"
        );
    }

    #[test]
    fn rejects_definite_bytes_over_64() {
        // Construct a definite-length CBOR byte string of 65 bytes
        // by hand: 0x58 = bytes(uint8), 0x41 = 65, then 65 0x00 bytes.
        let mut bytes = vec![0x58, 0x41];
        bytes.extend(std::iter::repeat_n(0u8, 65));
        let err = Data::from_cbor(&bytes).expect_err("must reject");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn roundtrip_constr_small_tag() {
        for tag in [0u64, 3, 6] {
            let d = Data::Constr(BigInt::from(tag), vec![Data::I(BigInt::from(42))]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    #[test]
    fn roundtrip_constr_medium_tag() {
        for tag in [7u64, 50, 127] {
            let d = Data::Constr(BigInt::from(tag), vec![Data::I(BigInt::from(42))]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    #[test]
    fn roundtrip_constr_large_tag() {
        // 128 and above use tag 102 wrapping [tag, args].
        for tag in [128u64, 1000, u64::MAX] {
            let d = Data::Constr(BigInt::from(tag), vec![]);
            assert_eq!(rt(&d), d, "tag={tag}");
        }
    }

    /// #859: a transient `Constr` tag outside the `Word64` range (only
    /// reachable pre-PV11 via `constrData` — see
    /// `builtin::denotations::ConstrData`) must still *encode* without
    /// panicking, matching Haskell's `encodeInteger`, which has no
    /// bound. Such a value can never have come FROM the on-chain CBOR
    /// decoder (decode always requires the tag to fit `Word64` — see
    /// `decode_tagged`), so encoding it is intentionally a "soft",
    /// one-way, non-round-tripping degenerate case: re-decoding those
    /// bytes must fail cleanly (never panic, never silently truncate).
    #[test]
    fn out_of_word64_range_constr_tag_encodes_without_panicking_and_does_not_round_trip() {
        // Negative tag: general tag-102 form with a CBOR negint payload.
        let neg = Data::Constr(BigInt::from(-1), vec![]);
        let neg_bytes = neg
            .to_cbor()
            .expect("negative tag must encode without panicking");
        // d8 66 = tag 102; 82 = array(2); 20 = negint(-1) (encodeInteger,
        // same path as Data::I); 80 = empty args array.
        assert_eq!(hex::encode(&neg_bytes), "d866822080");
        let redecoded = Data::from_cbor(&neg_bytes);
        assert!(
            redecoded.is_err(),
            "a negint constr-102 tag must be rejected on decode (Word64-only), not silently \
             reinterpreted: {redecoded:?}"
        );

        // Oversized (> u64::MAX) tag: general tag-102 form with a bignum payload.
        let huge_tag_value: BigInt = BigInt::from(1u64) << 80;
        let huge = Data::Constr(huge_tag_value, vec![]);
        let huge_bytes = huge
            .to_cbor()
            .expect("oversized tag must encode without panicking");
        assert_eq!(
            hex::encode(&huge_bytes)[0..6],
            hex::encode([0xd8, 0x66, 0x82])
        );
        let redecoded_huge = Data::from_cbor(&huge_bytes);
        assert!(
            redecoded_huge.is_err(),
            "a bignum-tagged constr-102 tag must be rejected on decode (Word64-only): \
             {redecoded_huge:?}"
        );
    }

    #[test]
    fn roundtrip_list() {
        let d = Data::List(vec![
            Data::I(BigInt::from(1)),
            Data::I(BigInt::from(2)),
            Data::B(vec![0xde, 0xad]),
        ]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_large_list_indefinite() {
        let items: Vec<Data> = (0..100).map(|i| Data::I(BigInt::from(i))).collect();
        let d = Data::List(items);
        let bytes = d.to_cbor().unwrap();
        // First byte must be 0x9f (indefinite-length array).
        assert_eq!(bytes[0], 0x9f, "expected indefinite-length array");
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_map() {
        let d = Data::Map(vec![
            (Data::B(vec![0x01]), Data::I(BigInt::from(10))),
            (Data::B(vec![0x02]), Data::I(BigInt::from(20))),
        ]);
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn roundtrip_map_preserves_duplicates() {
        // Critical: the encoder must not deduplicate.
        let d = Data::Map(vec![
            (Data::B(vec![0x01]), Data::I(BigInt::from(10))),
            (Data::B(vec![0x01]), Data::I(BigInt::from(20))),
        ]);
        let decoded = rt(&d);
        if let Data::Map(es) = &decoded {
            assert_eq!(es.len(), 2);
        } else {
            panic!("expected Map");
        }
    }

    #[test]
    fn roundtrip_nested() {
        let d = Data::Constr(
            BigInt::from(0),
            vec![
                Data::Map(vec![(
                    Data::B(b"key".to_vec()),
                    Data::List(vec![Data::I(BigInt::from(7))]),
                )]),
                Data::Constr(BigInt::from(127), vec![Data::I(BigInt::from(-1))]),
            ],
        );
        assert_eq!(rt(&d), d);
    }

    #[test]
    fn accepts_tag102_indefinite_outer_array() {
        // #831 finding 1: `d8 66 9f 00 80 ff` = tag 102, indefinite
        // `[0, []]` => Constr 0 []. Haskell's `decodeConstrExtended`
        // uses `decodeListLenOrIndef`, which accepts an indefinite
        // outer array closed by an explicit break; the prior dugite
        // decoder required `Some(2)` and rejected this.
        let bytes = [0xd8, 0x66, 0x9f, 0x00, 0x80, 0xff];
        let d = Data::from_cbor(&bytes).expect("must accept indefinite tag-102 array");
        assert_eq!(d, Data::Constr(BigInt::from(0), vec![]));
    }

    #[test]
    fn accepts_tag102_indefinite_outer_array_with_nonempty_fields() {
        // Same shape as above but with one field, to exercise the
        // `decode_array` call inside the indefinite tag-102 branch:
        // tag 102, indefinite `[1, [42]]`.
        let mut bytes = vec![0xd8, 0x66, 0x9f, 0x01];
        bytes.extend(
            Data::List(vec![Data::I(BigInt::from(42))])
                .to_cbor()
                .unwrap(),
        );
        bytes.push(0xff);
        let d = Data::from_cbor(&bytes).expect("must accept indefinite tag-102 array");
        assert_eq!(
            d,
            Data::Constr(BigInt::from(1), vec![Data::I(BigInt::from(42))])
        );
    }

    #[test]
    fn rejects_tag102_indefinite_missing_break() {
        // Same as `accepts_tag102_indefinite_outer_array` but with the
        // trailing `0xff` break omitted — must still be rejected
        // (mirrors Haskell's `decodeBreakOr` check).
        let bytes = [0xd8, 0x66, 0x9f, 0x00, 0x80];
        let err = Data::from_cbor(&bytes).expect_err("must reject missing break");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn rejects_tag102_wrong_definite_length() {
        // tag 102 with a definite array of length 3 (not 2) must still
        // be rejected — only the *indefinite* form gained new
        // acceptance; a malformed definite length is still an error.
        let bytes = [0xd8, 0x66, 0x83, 0x00, 0x80, 0x80];
        let err = Data::from_cbor(&bytes).expect_err("must reject array(3)");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn rejects_wide_header_positive_bignum_tag() {
        // #831 finding 2: `d8 02 41 05` is tag 2 encoded via the
        // non-minimal 1-byte-argument header (`0xd8 0x02`) rather than
        // the minimal inline form (`0xc2`). `cborg`'s `peekTokenType`
        // (which the Haskell `PlutusData` decoder is built on) maps
        // ONLY the inline `0xc2`/`0xc3` headers to the bignum path; a
        // wide-header encoding of tag 2 falls through to
        // `decodeConstr`, which rejects it (2 isn't a valid
        // constructor tag). dugite previously decoded this as `I(5)`.
        let bytes = [0xd8, 0x02, 0x41, 0x05];
        let err = Data::from_cbor(&bytes).expect_err("must reject wide-header tag 2");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn rejects_wide_header_negative_bignum_tag() {
        // Same as above for tag 3 (negative bignum), via `d9 00 03`
        // (2-byte-argument wide header).
        let bytes = [0xd9, 0x00, 0x03, 0x41, 0x05];
        let err = Data::from_cbor(&bytes).expect_err("must reject wide-header tag 3");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn accepts_minimal_header_bignum_tags() {
        // Regression: the minimal, canonical single-byte tag headers
        // (`0xc2`/`0xc3`) must still decode via the bignum path exactly
        // as before.
        let pos = [0xc2, 0x41, 0x05];
        assert_eq!(Data::from_cbor(&pos).unwrap(), Data::I(BigInt::from(5)));
        let neg = [0xc3, 0x41, 0x05];
        // tag 3 payload `n` decodes to `-1 - n` = `-6`.
        assert_eq!(Data::from_cbor(&neg).unwrap(), Data::I(BigInt::from(-6)));
    }

    #[test]
    fn rejects_overlydeep_data() {
        // Hand-craft 1100 nested CBOR-tag-121 (Constr 0) wrappers each
        // wrapping an empty array, terminating in an empty array.
        // Goes well past DATA_MAX_DEPTH (1024, matching the consensus
        // decoder's cap — see #831 finding 3).
        let mut bytes = Vec::new();
        for _ in 0..1100 {
            bytes.push(0xd8); // tag, 1-byte argument
            bytes.push(121); // tag 121
            bytes.push(0x81); // array(1)
        }
        bytes.push(0x80); // innermost empty array
        let err = Data::from_cbor(&bytes).expect_err("must reject");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn accepts_data_at_haskell_depth_that_a_256_cap_would_reject() {
        // #831 finding 3: 258 nested indefinite-length lists (well
        // within tx size limits) decode fine under Haskell's unbounded
        // recursion; a 256-deep cap here would have false-rejected
        // this. Pin acceptance at a depth between the old (256) and
        // new (1024) caps.
        let mut bytes = Vec::new();
        bytes.extend(std::iter::repeat_n(0x9f, 258)); // indefinite-length array opens
        bytes.push(0x80); // innermost empty (definite) array
        bytes.extend(std::iter::repeat_n(0xff, 258)); // close each indefinite array
        let d = Data::from_cbor(&bytes).expect("must accept depth-258 nesting");
        // Sanity: round-trips back through the canonical encoder.
        assert_eq!(Data::from_cbor(&d.to_cbor().unwrap()).unwrap(), d);
    }

    /// #832: a machine-*constructed* `Data` value (e.g. built via
    /// repeated `constrData`/`mkCons`/`listData` builtin calls) never
    /// passes through the depth-capped CBOR decoder at all, so it can
    /// be nested far deeper than [`DATA_MAX_DEPTH`] — a script can
    /// cheaply reach ~10^5-10^6 levels within budget. This exercises
    /// every recursive `Data` traversal an adversarial script can
    /// trigger on such a value: `equalsData` (`PartialEq`), the deep
    /// `Clone` a CEK `Var` lookup performs on a bound `Constant::Data`
    /// (`machine::step`), `serialiseData` (`to_cbor`, stacker-guarded),
    /// and the final `Drop` when it goes out of scope. Before #832, the
    /// derived recursive `PartialEq`/`Drop` would overflow the native
    /// stack at this depth; `Clone` was left derived until this fix and
    /// overflowed the same way (process abort — a remote DoS). Mirrors
    /// the CEK env's `deep_chain_extends_and_drops` (`machine/env.rs`).
    #[test]
    fn deeply_nested_data_compares_clones_serialises_and_drops_without_overflow() {
        const DEPTH: usize = 200_000;
        let mut d = Data::I(BigInt::from(0));
        for _ in 0..DEPTH {
            d = Data::List(vec![d]);
        }
        // Deep `Clone` — the CEK `Var`-lookup path. Must not overflow and
        // must reproduce the value exactly.
        let cloned = d.clone();
        assert!(
            cloned == d,
            "clone of 200k-deep Data must equal the original"
        );
        // `equalsData d d` — exercises the iterative `PartialEq`.
        #[allow(clippy::eq_op)]
        {
            assert!(d == d, "equalsData on 200k-deep Data must not overflow");
        }
        // `serialiseData d` — exercises the stacker-guarded encoder.
        let cbor = d.to_cbor().expect("serialiseData must not overflow");
        assert!(!cbor.is_empty());
        // `d`, `cloned`, and `cbor` drop here at scope exit, exercising the
        // iterative `Drop` on two full 200k-deep structures.
    }

    /// The iterative `Clone` must reproduce a mixed `Constr`/`Map`/`List`
    /// value with byte-exact field order. A clone that reordered `Map`
    /// entries or `Constr` args would change `serialiseData`/the body hash
    /// — a silent consensus divergence.
    #[test]
    fn iterative_clone_preserves_structure_and_order() {
        let d = Data::Constr(
            BigInt::from(7),
            vec![
                Data::Map(vec![
                    (Data::I(BigInt::from(1)), Data::B(vec![0xAA])),
                    (
                        Data::I(BigInt::from(2)),
                        Data::List(vec![Data::I(BigInt::from(3))]),
                    ),
                ]),
                Data::List(vec![
                    Data::I(BigInt::from(-9)),
                    Data::Constr(BigInt::from(0), vec![Data::B(vec![])]),
                ]),
                Data::B(vec![1, 2, 3, 4]),
            ],
        );
        let cloned = d.clone();
        assert!(cloned == d, "clone must be structurally equal");
        assert_eq!(
            cloned.to_cbor().unwrap(),
            d.to_cbor().unwrap(),
            "clone must serialise byte-identically (order preserved)"
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        let d = Data::I(BigInt::from(1));
        let mut bytes = d.to_cbor().unwrap();
        bytes.push(0x00);
        let err = Data::from_cbor(&bytes).expect_err("must reject trailing");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }

    #[test]
    fn empty_input_is_error() {
        let err = Data::from_cbor(&[]).expect_err("must reject empty");
        assert!(matches!(err, UplcError::CborDecode(_)), "got {err:?}");
    }
}
