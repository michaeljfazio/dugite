//! Term-level flat codec (UPLC-2 part 2).
//!
//! Encodes and decodes a [`Term`] / [`Constant`] / [`Program`] to the
//! flat wire format. The encoding rules are the *normative* Haskell
//! `IntersectMBO/plutus:plutus-core/untyped-plutus-core/.../Flat.hs`
//! `encodeTerm` / `decodeTerm` / `encode (Some (ValueOf uni))`.
//!
//! ## Term tags (4 bits, MSB-first)
//!
//! | Tag | Constructor | Wire form (`ann` always omitted: `()` = 0 bits)          |
//! |-----|-------------|-----------------------------------------------------------|
//! | 0   | Var         | `tag` + varint(`db_index`)                                |
//! | 1   | Delay       | `tag` + term                                              |
//! | 2   | Lam         | `tag` + 0-bit binder + term                               |
//! | 3   | App         | `tag` + term + term                                       |
//! | 4   | Constant    | `tag` + universe-tag list + value bytes                   |
//! | 5   | Force       | `tag` + term                                              |
//! | 6   | Error       | `tag`                                                     |
//! | 7   | Builtin     | `tag` + 7-bit builtin id                                  |
//! | 8   | Constr      | `tag` + varint(`constr_tag`) + cons-list(term) (v1.1.0+)  |
//! | 9   | Case        | `tag` + term + cons-list(term) (v1.1.0+)                  |
//!
//! ## Constant universe-tag list (4-bit cons-prefixed)
//!
//! The flat constant encoding is a cons-bit-prefixed list of 4-bit
//! universe tags followed by the encoded value(s). Single-atom tags
//! we currently support:
//!
//! | Tag | Universe                | Value encoding                          |
//! |-----|-------------------------|-----------------------------------------|
//! | 0   | Integer                 | zig-zag varint (arbitrary precision)    |
//! | 1   | ByteString              | filler-aligned chunked byte string      |
//! | 2   | String                  | byte string of UTF-8 bytes              |
//! | 3   | Unit                    | (no bits)                               |
//! | 4   | Bool                    | 1 bit                                   |
//!
//! Compound tags (5=List, 6=Pair, 8=Data) use the Apply connector (7)
//! and are fully supported. BLS atomic tags (9/10/11) are not wired
//! for the flat constant payload (BLS constants do not appear in
//! flat-encoded scripts).
//!
//! ## Defensive properties
//!
//! - **Depth-bounded recursion** via an explicit [`FLAT_MAX_DEPTH`]
//!   counter on every term recursion.
//! - **No `unwrap` / `panic!`** on adversarial bytes — every error
//!   surfaces as [`UplcError::FlatDecode`].
//! - **Unknown term tags** (10..=15) and unknown universe tags
//!   (5..=15 for now) return typed errors.
//! - **Pre-allocations clamped** via `safe_alloc_capacity` against the
//!   remaining bit budget of the input.

use super::bits::{BitReader, BitWriter};
use super::{FlatResult, FLAT_MAX_DEPTH};
use crate::data::Data;
use crate::term::{BuiltinId, Constant, Term, TypeTag};

use crate::UplcError;
use std::rc::Rc;

use num_bigint::{BigInt, Sign};

/// Width of the term-constructor tag, in bits. Matches the Haskell
/// `termTagWidth = 4`.
const TERM_TAG_WIDTH: u8 = 4;

/// Width of the builtin discriminant, in bits. Matches the Haskell
/// `builtinTagWidth = 7`.
const BUILTIN_TAG_WIDTH: u8 = 7;

/// Width of an atomic universe-tag, in bits.
const CONST_TAG_WIDTH: u8 = 4;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Decode a single [`Term`] from a flat bit stream.
pub fn decode_term(r: &mut BitReader<'_>) -> FlatResult<Term> {
    decode_term_depth(r, 0)
}

/// Encode a [`Term`] into a [`BitWriter`].
pub fn encode_term(w: &mut BitWriter, t: &Term) -> FlatResult<()> {
    encode_term_depth(w, t, 0)
}

/// Decode a [`Constant`] from a flat bit stream.
pub fn decode_constant(r: &mut BitReader<'_>) -> FlatResult<Constant> {
    let type_tag = decode_type_tag(r)?;
    decode_constant_value(r, &type_tag)
}

/// Encode a [`Constant`] into a [`BitWriter`].
pub fn encode_constant(w: &mut BitWriter, c: &Constant) -> FlatResult<()> {
    let tag = constant_type_tag(c)?;
    encode_type_tag(w, &tag)?;
    encode_constant_value(w, c)
}

// ---------------------------------------------------------------------------
// Term codec
// ---------------------------------------------------------------------------

fn decode_term_depth(r: &mut BitReader<'_>, depth: usize) -> FlatResult<Term> {
    if depth > FLAT_MAX_DEPTH {
        return Err(UplcError::FlatDecode(format!(
            "term depth limit exceeded ({FLAT_MAX_DEPTH})"
        )));
    }
    // Deeply-nested scripts (large DeFi validators can exceed 500 levels) can
    // overflow the OS thread stack under default nextest / tokio thread sizes.
    // `stacker::maybe_grow` transparently extends the stack when the remaining
    // guard zone falls below 128 KiB, so we never crash and never need to know
    // the script's actual depth. The depth counter above still guards against
    // adversarial inputs that try to exhaust the heap via unbounded allocation.
    stacker::maybe_grow(128 * 1024, 1024 * 1024, || decode_term_inner(r, depth))
}

fn decode_term_inner(r: &mut BitReader<'_>, depth: usize) -> FlatResult<Term> {
    let tag = r.read_bits8(TERM_TAG_WIDTH)?;
    match tag {
        0 => {
            let idx = r.read_natural_u64()?;
            Ok(Term::Var(idx))
        }
        1 => {
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Delay(Rc::new(body)))
        }
        2 => {
            // Binder is encoded as zero bits for De Bruijn UPLC — see
            // `instance Flat (Binder DeBruijn)` in Haskell. Read
            // nothing and recurse straight into the body.
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Lam(Rc::new(body)))
        }
        3 => {
            let fun = decode_term_depth(r, depth + 1)?;
            let arg = decode_term_depth(r, depth + 1)?;
            Ok(Term::App(Rc::new(fun), Rc::new(arg)))
        }
        4 => {
            let c = decode_constant(r)?;
            Ok(Term::Const(c))
        }
        5 => {
            let body = decode_term_depth(r, depth + 1)?;
            Ok(Term::Force(Rc::new(body)))
        }
        6 => Ok(Term::Error),
        7 => {
            let raw = r.read_bits8(BUILTIN_TAG_WIDTH)?;
            // BuiltinId::from_u8 is the placeholder stub for now —
            // it returns `Internal`, which propagates as a typed
            // error to the caller. When UPLC-4 wires the table the
            // happy path here lights up automatically.
            let id = BuiltinId::from_u8(raw)?;
            Ok(Term::Builtin(id))
        }
        8 => {
            // Constr: varint(tag) + cons-list(term).  Matches Haskell
            // `encodeTerm (Constr _ i ts) = encodeTermTag 8 <> encode i
            //                                              <> encodeListWith encode ts`.
            let ctag = r.read_natural_u64()?;
            let args = decode_term_list(r, depth + 1)?;
            Ok(Term::Constr { tag: ctag, args })
        }
        9 => {
            // Case: scrutinee (term) + cons-list(term) branches.  Matches
            // Haskell `encodeTerm (Case _ t ts) = encodeTermTag 9
            //                                    <> encode t <> encodeListWith encode ts`.
            let scrutinee = decode_term_depth(r, depth + 1)?;
            let branches = decode_term_list(r, depth + 1)?;
            Ok(Term::Case {
                scrutinee: Rc::new(scrutinee),
                branches,
            })
        }
        _ => Err(UplcError::FlatDecode(format!(
            "unknown term tag {tag:#06b}"
        ))),
    }
}

/// Decode a `Flat`-encoded cons-list of terms.  Each element is prefixed
/// by a `1` continuation bit; a `0` terminates the list.  Mirrors
/// Haskell `decodeListWith decode`.
fn decode_term_list(r: &mut BitReader<'_>, depth: usize) -> FlatResult<Vec<Rc<Term>>> {
    let mut out = Vec::new();
    while r.read_bit()? {
        out.push(Rc::new(decode_term_depth(r, depth)?));
    }
    Ok(out)
}

fn encode_term_depth(w: &mut BitWriter, t: &Term, depth: usize) -> FlatResult<()> {
    if depth > FLAT_MAX_DEPTH {
        return Err(UplcError::Encode(format!(
            "term depth limit exceeded ({FLAT_MAX_DEPTH})"
        )));
    }
    match t {
        Term::Var(i) => {
            w.write_bits8(0, TERM_TAG_WIDTH)?;
            w.write_natural_u64(*i)?;
        }
        Term::Delay(body) => {
            w.write_bits8(1, TERM_TAG_WIDTH)?;
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::Lam(body) => {
            w.write_bits8(2, TERM_TAG_WIDTH)?;
            // Binder is zero bits for De Bruijn UPLC.
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::App(fun, arg) => {
            w.write_bits8(3, TERM_TAG_WIDTH)?;
            encode_term_depth(w, fun, depth + 1)?;
            encode_term_depth(w, arg, depth + 1)?;
        }
        Term::Const(c) => {
            w.write_bits8(4, TERM_TAG_WIDTH)?;
            encode_constant(w, c)?;
        }
        Term::Force(body) => {
            w.write_bits8(5, TERM_TAG_WIDTH)?;
            encode_term_depth(w, body, depth + 1)?;
        }
        Term::Error => {
            w.write_bits8(6, TERM_TAG_WIDTH)?;
        }
        Term::Builtin(id) => {
            w.write_bits8(7, TERM_TAG_WIDTH)?;
            let raw = id.as_u8();
            if raw >= 128 {
                // 7-bit field overflow guard — should be unreachable
                // given the enum's largest discriminant is 87, but we
                // surface as a typed error rather than truncate.
                return Err(UplcError::Encode(format!(
                    "BuiltinId discriminant {raw} exceeds 7-bit wire field"
                )));
            }
            w.write_bits8(raw, BUILTIN_TAG_WIDTH)?;
        }
        Term::Constr { tag, args } => {
            w.write_bits8(8, TERM_TAG_WIDTH)?;
            w.write_natural_u64(*tag)?;
            encode_term_list(w, args, depth + 1)?;
        }
        Term::Case {
            scrutinee,
            branches,
        } => {
            w.write_bits8(9, TERM_TAG_WIDTH)?;
            encode_term_depth(w, scrutinee, depth + 1)?;
            encode_term_list(w, branches, depth + 1)?;
        }
    }
    Ok(())
}

/// Encode a slice of terms as a `Flat` cons-list (each element prefixed
/// by a `1` bit; terminator `0`).
fn encode_term_list(w: &mut BitWriter, terms: &[Rc<Term>], depth: usize) -> FlatResult<()> {
    for t in terms {
        w.write_bit(true);
        encode_term_depth(w, t, depth)?;
    }
    w.write_bit(false);
    Ok(())
}

// ---------------------------------------------------------------------------
// Constant codec — universe-tag list (full DefaultUni support)
// ---------------------------------------------------------------------------

/// Read a flat-encoded cons-list of 4-bit universe-tag atoms and return
/// the full [`TypeTag`]. This is the authoritative type-tag decoder.
///
/// ## Haskell flat encoding for DefaultUni
///
/// The Plutus `DefaultUni` type is serialised as a *flat cons-list* of
/// 4-bit atom tags (each preceded by a `1` continuation bit; a `0` bit
/// terminates the list). The atom alphabet is:
///
/// | atom | constructor        | wire role                          |
/// |------|--------------------|------------------------------------|
/// |  0   | `DefaultUniInt`    | Integer                            |
/// |  1   | `DefaultUniBS`     | ByteString                         |
/// |  2   | `DefaultUniStr`    | String                             |
/// |  3   | `DefaultUniUnit`   | Unit                               |
/// |  4   | `DefaultUniBool`   | Bool                               |
/// |  5   | `DefaultUniProtoList` | type-constructor for List       |
/// |  6   | `DefaultUniProtoPair` | type-constructor for Pair       |
/// |  7   | `DefaultUniApply`  | type-application (connector)       |
/// |  8   | `DefaultUniData`   | Data (CBOR-encoded PlutusData)     |
/// |  9   | `DefaultUniBls12_381_G1_Element` | BLS12-381 G1        |
/// | 10   | `DefaultUniBls12_381_G2_Element` | BLS12-381 G2        |
/// | 11   | `DefaultUniBls12_381_MlResult`   | BLS12-381 MlResult |
///
/// Compound type atom sequences are a PRE-ORDER (root-first) serialisation of
/// the application tree, so the `Apply` atom (7) comes FIRST:
///
/// * `List(a)`   → `[1][7][1][5][…type-a…][0]`              (`Apply(ProtoList, a)`)
/// * `Pair(a,b)` → `[1][7][1][7][1][6][…a…][…b…][0]`        (`Apply(Apply(ProtoPair, a), b)`)
/// * `Data`      → `[1][8][0]`
///
/// The Apply atom (7) is the application node: reading it consumes two
/// sub-universes (function then argument). The higher-kinded constructors
/// `ProtoList` (5) / `ProtoPair` (6) are only valid as the function side of
/// an Apply.
///
/// Reference: `IntersectMBO/plutus`,
/// `plutus-core/plutus-core/src/PlutusCore/Default/Universe.hs`
/// (`encodeUni`: `DefaultUniApply f a = 7 : encodeUni f ++ encodeUni a`;
/// `withDecodedUni`).
fn decode_type_tag(r: &mut BitReader<'_>) -> FlatResult<TypeTag> {
    // Read the cons-list of atoms into a flat Vec.
    // Each element: 1-bit cons (must be 1), 4-bit atom; terminated by 0 bit.
    let atoms = read_atom_list(r)?;
    if atoms.is_empty() {
        return Err(UplcError::FlatDecode("empty universe-tag list".into()));
    }
    // Parse the atom sequence into a TypeTag.
    let (tag, consumed) = parse_type_from_atoms(&atoms, 0)?;
    if consumed != atoms.len() {
        return Err(UplcError::FlatDecode(format!(
            "universe-tag list has {} leftover atoms after parsing type",
            atoms.len() - consumed
        )));
    }
    Ok(tag)
}

/// Read the raw cons-list of 4-bit type atoms from the bit stream.
fn read_atom_list(r: &mut BitReader<'_>) -> FlatResult<Vec<u8>> {
    // Cap at a safe maximum — no real Plutus type uses more than a handful
    // of atoms, and this guards against adversarial infinite loops.
    const MAX_TYPE_ATOMS: usize = 64;
    let mut atoms = Vec::new();
    while r.read_bit()? {
        if atoms.len() >= MAX_TYPE_ATOMS {
            return Err(UplcError::FlatDecode(format!(
                "universe-tag list exceeds {MAX_TYPE_ATOMS} atoms"
            )));
        }
        atoms.push(r.read_bits8(CONST_TAG_WIDTH)?);
    }
    Ok(atoms)
}

/// Intermediate node while parsing the pre-order universe-tag atom list.
///
/// Mirrors Haskell `withDecodedUni` (PlutusCore.Default.Universe): the
/// `Apply` atom (7) is the ROOT of a compound type, and the higher-kinded
/// constructors `ProtoList` (5) / `ProtoPair` (6) only become a concrete
/// [`TypeTag`] once fully applied. `encodeUni` is a pre-order (root-first)
/// serialisation of the application tree, so `List a = Apply(ProtoList, a)`
/// encodes as `[7, 5, …a]` and `Pair a b = Apply(Apply(ProtoPair, a), b)`
/// encodes as `[7, 7, 6, …a, …b]` — the Apply tag(s) come FIRST.
enum UniNode {
    /// A fully-applied, concrete type.
    Complete(TypeTag),
    /// `DefaultUniProtoList` — awaits one argument.
    ProtoList,
    /// `DefaultUniProtoPair` — awaits two arguments.
    ProtoPair,
    /// `DefaultUniProtoPair` applied to its first argument — awaits the second.
    ProtoPair1(TypeTag),
}

/// Parse one universe node from the pre-order atom list at `pos`.
/// Returns `(node, next_pos)`. Mirrors Haskell `withDecodedUni`: reading an
/// `Apply` (7) recursively decodes the function side then the argument side,
/// then applies them (the `withApplicable` kind check).
fn parse_uni_from_atoms(atoms: &[u8], pos: usize) -> FlatResult<(UniNode, usize)> {
    let atom = *atoms
        .get(pos)
        .ok_or_else(|| UplcError::FlatDecode("unexpected end of universe-tag atom list".into()))?;
    match atom {
        0 => Ok((UniNode::Complete(TypeTag::Integer), pos + 1)),
        1 => Ok((UniNode::Complete(TypeTag::ByteString), pos + 1)),
        2 => Ok((UniNode::Complete(TypeTag::String), pos + 1)),
        3 => Ok((UniNode::Complete(TypeTag::Unit), pos + 1)),
        4 => Ok((UniNode::Complete(TypeTag::Bool), pos + 1)),
        5 => Ok((UniNode::ProtoList, pos + 1)),
        6 => Ok((UniNode::ProtoPair, pos + 1)),
        7 => {
            // Apply: decode the function side, then the argument side, then
            // apply (mirrors Haskell `withDecodedUni`/`withApplicable`).
            let (func, after_f) = parse_uni_from_atoms(atoms, pos + 1)?;
            let (arg, after_a) = parse_uni_from_atoms(atoms, after_f)?;
            let arg_tag = match arg {
                UniNode::Complete(t) => t,
                _ => {
                    return Err(UplcError::FlatDecode(
                        "universe-tag Apply argument is an unapplied type constructor".into(),
                    ))
                }
            };
            let applied = match func {
                UniNode::ProtoList => UniNode::Complete(TypeTag::List(Box::new(arg_tag))),
                UniNode::ProtoPair => UniNode::ProtoPair1(arg_tag),
                UniNode::ProtoPair1(first) => {
                    UniNode::Complete(TypeTag::Pair(Box::new(first), Box::new(arg_tag)))
                }
                UniNode::Complete(_) => {
                    return Err(UplcError::FlatDecode(
                        "universe-tag Apply of a fully-applied (non-constructor) type".into(),
                    ))
                }
            };
            Ok((applied, after_a))
        }
        8 => Ok((UniNode::Complete(TypeTag::Data), pos + 1)),
        9 => Ok((UniNode::Complete(TypeTag::Bls12_381G1Element), pos + 1)),
        10 => Ok((UniNode::Complete(TypeTag::Bls12_381G2Element), pos + 1)),
        11 => Ok((UniNode::Complete(TypeTag::Bls12_381MlResult), pos + 1)),
        _ => Err(UplcError::FlatDecode(format!(
            "unknown universe tag atom {atom:#06b}"
        ))),
    }
}

/// Parse a single concrete [`TypeTag`] from the pre-order atom list starting at
/// `pos`. Returns `(tag, next_pos)`. The top-level node must be fully applied
/// (an unapplied `ProtoList`/`ProtoPair` is a malformed constant type).
fn parse_type_from_atoms(atoms: &[u8], pos: usize) -> FlatResult<(TypeTag, usize)> {
    let (node, next) = parse_uni_from_atoms(atoms, pos)?;
    match node {
        UniNode::Complete(t) => Ok((t, next)),
        _ => Err(UplcError::FlatDecode(
            "universe-tag list is an unapplied type constructor (List/Pair without argument)"
                .into(),
        )),
    }
}

/// Encode a full [`TypeTag`] as a flat cons-list of 4-bit atoms.
///
/// The atom sequence for compound types is pre-order (Apply tag 7 first):
/// - `List(a)` → `[1-cons][7][1-cons][5][…atoms for a…][0-term]`
/// - `Pair(a,b)` → `[1-cons][7][1-cons][7][1-cons][6][…a atoms…][…b atoms…][0-term]`
/// - `Data` → `[1-cons][8][0-term]`
fn encode_type_tag(w: &mut BitWriter, t: &TypeTag) -> FlatResult<()> {
    // Collect all atoms for this type tag, then write them as a cons-list.
    let mut atoms: Vec<u8> = Vec::new();
    collect_type_atoms(t, &mut atoms)?;
    for &atom in &atoms {
        w.write_bit(true); // cons-bit: element follows
        w.write_bits8(atom, CONST_TAG_WIDTH)?;
    }
    w.write_bit(false); // list terminator
    Ok(())
}

/// Recursively collect the flat 4-bit atom sequence for a type.
fn collect_type_atoms(t: &TypeTag, atoms: &mut Vec<u8>) -> FlatResult<()> {
    match t {
        TypeTag::Integer => atoms.push(0),
        TypeTag::ByteString => atoms.push(1),
        TypeTag::String => atoms.push(2),
        TypeTag::Unit => atoms.push(3),
        TypeTag::Bool => atoms.push(4),
        TypeTag::List(elem) => {
            // List a = Apply(ProtoList, a) → pre-order [7, 5, …a].
            atoms.push(7); // Apply
            atoms.push(5); // ProtoList
            collect_type_atoms(elem, atoms)?;
        }
        TypeTag::Pair(a, b) => {
            // Pair a b = Apply(Apply(ProtoPair, a), b) → pre-order [7, 7, 6, …a, …b].
            atoms.push(7); // Apply (outer)
            atoms.push(7); // Apply (inner)
            atoms.push(6); // ProtoPair
            collect_type_atoms(a, atoms)?;
            collect_type_atoms(b, atoms)?;
        }
        TypeTag::Data => atoms.push(8),
        TypeTag::Bls12_381G1Element => atoms.push(9),
        TypeTag::Bls12_381G2Element => atoms.push(10),
        TypeTag::Bls12_381MlResult => atoms.push(11),
        TypeTag::Array(_) | TypeTag::Value => {
            return Err(UplcError::Encode(
                "Array / Value flat encoding not yet wired".into(),
            ));
        }
    }
    Ok(())
}

fn constant_type_tag(c: &Constant) -> FlatResult<TypeTag> {
    match c {
        Constant::Integer(_) => Ok(TypeTag::Integer),
        Constant::ByteString(_) => Ok(TypeTag::ByteString),
        Constant::String(_) => Ok(TypeTag::String),
        Constant::Unit => Ok(TypeTag::Unit),
        Constant::Bool(_) => Ok(TypeTag::Bool),
        Constant::ProtoList { elem_type, .. } => Ok(TypeTag::List(Box::new(elem_type.clone()))),
        Constant::ProtoPair { a_type, b_type, .. } => Ok(TypeTag::Pair(
            Box::new(a_type.clone()),
            Box::new(b_type.clone()),
        )),
        Constant::Data(_) => Ok(TypeTag::Data),
        Constant::Bls12_381G1Element(_) => Ok(TypeTag::Bls12_381G1Element),
        Constant::Bls12_381G2Element(_) => Ok(TypeTag::Bls12_381G2Element),
        Constant::Bls12_381MlResult(_) => Ok(TypeTag::Bls12_381MlResult),
        Constant::Array { .. } | Constant::Value(_) => Err(UplcError::Encode(
            "Array / Value constants cannot appear in flat-encoded \
             scripts (flat codec not yet wired for PV1.1.0 types)"
                .into(),
        )),
    }
}

fn decode_constant_value(r: &mut BitReader<'_>, tag: &TypeTag) -> FlatResult<Constant> {
    match tag {
        TypeTag::Integer => {
            // Haskell `Flat Integer` is zig-zag of a Natural with
            // arbitrary-width 7-bit chunks. Real Plutus scripts can carry
            // arbitrary-precision integers (e.g., large nonces or amounts),
            // so we use the arbitrary-precision path.
            let v = read_integer_bigint(r)?;
            Ok(Constant::Integer(v))
        }
        TypeTag::ByteString => {
            let bs = r.read_bytestring()?;
            Ok(Constant::ByteString(bs))
        }
        TypeTag::String => {
            let bs = r.read_bytestring()?;
            let s = String::from_utf8(bs).map_err(|e| {
                UplcError::FlatDecode(format!("String constant not valid UTF-8: {e}"))
            })?;
            Ok(Constant::String(s))
        }
        TypeTag::Unit => Ok(Constant::Unit),
        TypeTag::Bool => {
            let b = r.read_bit()?;
            Ok(Constant::Bool(b))
        }
        TypeTag::Data => {
            // Data is encoded as a flat bytestring containing the CBOR-encoded
            // PlutusData value.  Read the raw bytes, then CBOR-decode them
            // into a `crate::data::Data` value.
            let raw = r.read_bytestring()?;
            let d = Data::from_cbor(&raw).map_err(|e| {
                UplcError::FlatDecode(format!("Data constant: CBOR decode failed: {e}"))
            })?;
            Ok(Constant::Data(d))
        }
        TypeTag::List(elem_type) => {
            // A list constant is a flat cons-list: each element is preceded
            // by a 1-continuation bit; a 0 bit terminates.
            // Each element value is decoded recursively with the element type.
            let mut elements = Vec::new();
            while r.read_bit()? {
                let elem = decode_constant_value(r, elem_type)?;
                elements.push(elem);
            }
            Ok(Constant::ProtoList {
                elem_type: (**elem_type).clone(),
                elements,
            })
        }
        TypeTag::Pair(a_type, b_type) => {
            // A pair constant is just the two element values back-to-back
            // (no separator bits).
            let a = decode_constant_value(r, a_type)?;
            let b = decode_constant_value(r, b_type)?;
            Ok(Constant::ProtoPair {
                a_type: (**a_type).clone(),
                b_type: (**b_type).clone(),
                a: Box::new(a),
                b: Box::new(b),
            })
        }
        TypeTag::Bls12_381G1Element
        | TypeTag::Bls12_381G2Element
        | TypeTag::Bls12_381MlResult
        | TypeTag::Array(_)
        | TypeTag::Value => Err(UplcError::FlatDecode(format!(
            "constant payload for type {tag:?} not yet wired"
        ))),
    }
}

fn encode_constant_value(w: &mut BitWriter, c: &Constant) -> FlatResult<()> {
    match c {
        Constant::Integer(n) => {
            // Full arbitrary-precision integer encoding (zig-zag + Natural).
            write_integer_bigint(w, n)?;
        }
        Constant::ByteString(bs) => {
            w.write_bytestring(bs)?;
        }
        Constant::String(s) => {
            w.write_bytestring(s.as_bytes())?;
        }
        Constant::Unit => {
            // Zero bits.
        }
        Constant::Bool(b) => {
            w.write_bit(*b);
        }
        Constant::Data(d) => {
            // Data is encoded as a bytestring containing the CBOR of the value.
            let raw = d.to_cbor().map_err(|e| {
                UplcError::Encode(format!("Data constant: CBOR encode failed: {e}"))
            })?;
            w.write_bytestring(&raw)?;
        }
        Constant::ProtoList {
            elem_type,
            elements,
        } => {
            // Cons-list: each element preceded by 1-bit, terminated by 0-bit.
            for elem in elements {
                w.write_bit(true);
                encode_constant_value(w, elem)?;
            }
            w.write_bit(false);
            let _ = elem_type; // used in type tag, not value encoding
        }
        Constant::ProtoPair { a, b, .. } => {
            // Two values back-to-back.
            encode_constant_value(w, a)?;
            encode_constant_value(w, b)?;
        }
        Constant::Bls12_381G1Element(_)
        | Constant::Bls12_381G2Element(_)
        | Constant::Bls12_381MlResult(_) => {
            return Err(UplcError::Encode(
                "BLS12-381 constants cannot appear in flat-encoded \
                 scripts per Haskell reference"
                    .into(),
            ));
        }
        Constant::Array { .. } | Constant::Value(_) => {
            return Err(UplcError::Encode(
                "Array / Value constants flat encoding not yet wired".into(),
            ));
        }
    }
    Ok(())
}

/// Encode an arbitrary-precision integer into the flat bit stream.
///
/// Applies zig-zag encoding (n>=0 → 2n; n<0 → 2|n|-1) then encodes the
/// resulting Natural as chunked 7-bit groups (LSB first, cont bit first).
fn write_integer_bigint(w: &mut BitWriter, n: &BigInt) -> FlatResult<()> {
    use num_traits::Zero;
    // Zig-zag encode to a non-negative Natural.
    let zigzag: BigInt = if n.sign() != Sign::Minus {
        // non-negative: 2n
        n << 1
    } else {
        // negative: 2|n| - 1 = (-n * 2) - 1
        ((-n) << 1) - 1u64
    };
    // Encode the Natural as chunked 7-bit groups (LSB-first).
    let mut remaining = zigzag;
    loop {
        let chunk: u8 = (remaining.iter_u64_digits().next().unwrap_or(0) & 0x7f) as u8;
        remaining >>= 7;
        let more = !remaining.is_zero();
        w.write_bit(more);
        w.write_bits8(chunk, 7)?;
        if !more {
            break;
        }
    }
    Ok(())
}

/// Read an arbitrary-precision integer from the flat bit stream.
///
/// The Haskell `Flat Integer` encoding is zig-zag over a `Natural`:
/// - Non-negative n → encoded as `2n`
/// - Negative n → encoded as `2|n| - 1`
///
/// The Natural itself is encoded as chunked 7-bit groups (LSB first),
/// each prefixed by a 1-bit continuation flag (1=more, 0=last).
///
/// This is the full arbitrary-precision implementation: each chunk is
/// accumulated into a `BigInt`. Scripts with large integer constants
/// (big nonces, hash values, etc.) need this path.
fn read_integer_bigint(r: &mut BitReader<'_>) -> FlatResult<BigInt> {
    // Read Natural (unsigned arbitrary-precision integer) as a BigInt.
    // Each iteration: 1 cont bit + 7 data bits (LSB first across chunks).
    let mut value = BigInt::from(0u64);
    let mut shift: u32 = 0;
    loop {
        let more = r.read_bit()?;
        let chunk = r.read_bits8(7)? as u64;
        if shift > 0 {
            value += BigInt::from(chunk) << shift;
        } else {
            value = BigInt::from(chunk);
        }
        if !more {
            break;
        }
        shift = shift
            .checked_add(7)
            .ok_or_else(|| UplcError::FlatDecode("Integer natural varint shift overflow".into()))?;
    }
    // Zig-zag decode: even → non-negative, odd → negative.
    let decoded = if value.bit(0) {
        // odd: -(value + 1) / 2
        let shifted: BigInt = (value + 1u64) >> 1;
        -shifted
    } else {
        // even: value / 2
        value >> 1
    };
    Ok(decoded)
}

/// Convert a [`BigInt`] to `i64`, rejecting out-of-range values with
/// a typed error. This is the i64-bounded subset of the Haskell
/// `Flat Integer` rule; arbitrary-precision support lives in the
/// follow-on commit.
fn bigint_to_i64(n: &BigInt) -> FlatResult<i64> {
    if n.sign() == Sign::NoSign {
        return Ok(0);
    }
    let digits: Vec<u64> = n.iter_u64_digits().collect();
    if digits.len() > 1 {
        return Err(UplcError::Encode(format!(
            "Integer outside i64 range for current flat encoder \
             (arbitrary-precision support pending): magnitude has \
             {} u64-digits",
            digits.len()
        )));
    }
    let mag = digits.first().copied().unwrap_or(0);
    match n.sign() {
        Sign::Plus => i64::try_from(mag)
            .map_err(|_| UplcError::Encode(format!("positive Integer {mag} exceeds i64::MAX"))),
        Sign::Minus => {
            // Negate carefully: -i64::MIN is i64::MAX + 1, which can't
            // fit. Build via signed conversion of the magnitude.
            if mag <= i64::MAX as u64 {
                Ok(-(mag as i64))
            } else if mag == (i64::MAX as u64) + 1 {
                Ok(i64::MIN)
            } else {
                Err(UplcError::Encode(format!(
                    "negative Integer magnitude {mag} exceeds i64 range"
                )))
            }
        }
        Sign::NoSign => unreachable_zero(),
    }
}

/// Unreachable in `bigint_to_i64` since we short-circuit `NoSign` at
/// the top of the function, but expressed as a typed error rather
/// than `unreachable!` to honour the no-panic invariant.
fn unreachable_zero() -> FlatResult<i64> {
    Err(UplcError::Internal(
        "bigint_to_i64: Sign::NoSign reached the body".into(),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_term(t: Term) -> Term {
        let mut w = BitWriter::new();
        encode_term(&mut w, &t).expect("encode");
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        decode_term(&mut r).expect("decode")
    }

    fn atoms(t: &TypeTag) -> Vec<u8> {
        let mut v = Vec::new();
        collect_type_atoms(t, &mut v).expect("collect");
        v
    }

    // Byte-exact universe-tag atom order must match Haskell `encodeUni`
    // (pre-order, Apply tag 7 first):
    //   List a   = Apply(ProtoList, a)          → [7, 5, …a]
    //   Pair a b = Apply(Apply(ProtoPair,a), b) → [7, 7, 6, …a, …b]
    // Reference (cardano-haskell-oracle, verified against IntersectMBO/plutus
    // PlutusCore/Default/Universe.hs `encodeUni`):
    //   encodeUni (DefaultUniApply f a) = 7 : encodeUni f ++ encodeUni a
    // Regression for the 69×/block "Apply atom (7) outside List/Pair" decode
    // failures on real preprod Babbage scripts (decoder had the order reversed).
    #[test]
    fn universe_tag_atom_order_matches_haskell() {
        assert_eq!(atoms(&TypeTag::Integer), vec![0]);
        assert_eq!(atoms(&TypeTag::Data), vec![8]);
        // List Integer = [7, 5, 0]
        assert_eq!(
            atoms(&TypeTag::List(Box::new(TypeTag::Integer))),
            vec![7, 5, 0]
        );
        // Pair Integer ByteString = [7, 7, 6, 0, 1]
        assert_eq!(
            atoms(&TypeTag::Pair(
                Box::new(TypeTag::Integer),
                Box::new(TypeTag::ByteString)
            )),
            vec![7, 7, 6, 0, 1]
        );
        // Nested: List (Pair Data Data) = [7, 5, 7, 7, 6, 8, 8]
        assert_eq!(
            atoms(&TypeTag::List(Box::new(TypeTag::Pair(
                Box::new(TypeTag::Data),
                Box::new(TypeTag::Data)
            )))),
            vec![7, 5, 7, 7, 6, 8, 8]
        );
    }

    #[test]
    fn parse_type_from_haskell_atom_order() {
        // Apply-first sequences (as emitted by Haskell encodeUni) must decode.
        let (t, n) = parse_type_from_atoms(&[7, 5, 0], 0).expect("List Integer");
        assert_eq!(t, TypeTag::List(Box::new(TypeTag::Integer)));
        assert_eq!(n, 3);

        let (t, n) = parse_type_from_atoms(&[7, 7, 6, 0, 1], 0).expect("Pair Int BS");
        assert_eq!(
            t,
            TypeTag::Pair(Box::new(TypeTag::Integer), Box::new(TypeTag::ByteString))
        );
        assert_eq!(n, 5);

        // The old (reversed) order [5, 7, 0] is now an unapplied ProtoList → error.
        assert!(parse_type_from_atoms(&[5, 7, 0], 0).is_err());
    }

    #[test]
    fn type_tag_atoms_roundtrip() {
        for t in [
            TypeTag::Integer,
            TypeTag::Bool,
            TypeTag::Data,
            TypeTag::List(Box::new(TypeTag::Integer)),
            TypeTag::Pair(Box::new(TypeTag::Integer), Box::new(TypeTag::ByteString)),
            TypeTag::List(Box::new(TypeTag::Pair(
                Box::new(TypeTag::Data),
                Box::new(TypeTag::Data),
            ))),
        ] {
            let a = atoms(&t);
            let (decoded, n) = parse_type_from_atoms(&a, 0).expect("decode");
            assert_eq!(decoded, t, "roundtrip {t:?}");
            assert_eq!(n, a.len(), "consumed all atoms for {t:?}");
        }
    }

    #[test]
    fn var_roundtrip() {
        for i in [0u64, 1, 100, u32::MAX as u64, u64::MAX] {
            let t = Term::Var(i);
            assert_eq!(rt_term(t.clone()), t, "var index {i}");
        }
    }

    #[test]
    fn error_roundtrip() {
        assert_eq!(rt_term(Term::Error), Term::Error);
    }

    #[test]
    fn delay_force_roundtrip() {
        let t = Term::Delay(Rc::new(Term::Force(Rc::new(Term::Error))));
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn lam_app_roundtrip() {
        // (lam (var 1)) — innermost binder reference (1-based).
        let t = Term::Lam(Rc::new(Term::Var(1)));
        assert_eq!(rt_term(t.clone()), t);

        let app = Term::App(Rc::new(t.clone()), Rc::new(Term::Var(2)));
        assert_eq!(rt_term(app.clone()), app);
    }

    #[test]
    fn const_int_roundtrip() {
        for n in [0i64, 1, -1, 1_000_000, -1_000_000, i64::MAX, i64::MIN] {
            let t = Term::Const(Constant::Integer(BigInt::from(n)));
            assert_eq!(rt_term(t.clone()), t, "int {n}");
        }
    }

    #[test]
    fn const_bytes_roundtrip() {
        for bs in [
            Vec::<u8>::new(),
            vec![0xab],
            (0..255u8).collect::<Vec<_>>(),
            (0..512u32).map(|i| (i & 0xff) as u8).collect(),
        ] {
            let t = Term::Const(Constant::ByteString(bs.clone()));
            assert_eq!(rt_term(t.clone()), t, "len={}", bs.len());
        }
    }

    #[test]
    fn const_string_roundtrip() {
        for s in ["", "x", "hello", "ünıçødé 🎉"] {
            let t = Term::Const(Constant::String(s.into()));
            assert_eq!(rt_term(t.clone()), t, "s={s:?}");
        }
    }

    #[test]
    fn const_unit_roundtrip() {
        assert_eq!(
            rt_term(Term::Const(Constant::Unit)),
            Term::Const(Constant::Unit)
        );
    }

    #[test]
    fn const_bool_roundtrip() {
        for b in [true, false] {
            let t = Term::Const(Constant::Bool(b));
            assert_eq!(rt_term(t.clone()), t, "b={b}");
        }
    }

    #[test]
    fn nested_structure_roundtrip() {
        // (app (lam (var 1)) (const #t))
        let t = Term::App(
            Rc::new(Term::Lam(Rc::new(Term::Var(1)))),
            Rc::new(Term::Const(Constant::Bool(true))),
        );
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn deeply_nested_terms_succeed_under_limit() {
        // Build a moderately deep `Force(Force(...))` chain. The
        // limit isn't testing FLAT_MAX_DEPTH itself (4096) — that
        // would risk a stack overflow from the recursive `Drop` of
        // the resulting `Box<Term>` tree on test thread stacks. We
        // only need to confirm "depth N works" for an N well above
        // any real-world script. The depth-limit *rejection* path
        // is exercised below in `rejects_overdeep_terms`.
        let depth = 128;
        let mut t = Term::Error;
        for _ in 0..depth {
            t = Term::Force(Rc::new(t));
        }
        assert_eq!(rt_term(t.clone()), t);
    }

    #[test]
    fn rejects_overdeep_terms_at_decode() {
        // Build a chain of `Force` tags past FLAT_MAX_DEPTH at the
        // bit level. Each `Force` is the 4-bit tag `0101` = 0x5,
        // and the chain terminates with `Error` (0110 = 0x6).
        //
        // For (FLAT_MAX_DEPTH + 4) levels, the encoder will fail
        // first (typed `Encode` error), so we hand-craft bits.
        let levels = FLAT_MAX_DEPTH + 4;
        let mut w = BitWriter::new();
        for _ in 0..levels {
            // Force tag (5 = 0b0101)
            w.write_bits8(5, TERM_TAG_WIDTH).unwrap();
        }
        // Error tag (6 = 0b0110)
        w.write_bits8(6, TERM_TAG_WIDTH).unwrap();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let result = decode_term(&mut r);
        assert!(
            matches!(result, Err(UplcError::FlatDecode(_))),
            "expected depth-limit error, got {result:?}"
        );
    }

    #[test]
    fn rejects_unknown_term_tag() {
        // 4-bit tag 10 (0b1010) is reserved-but-unwired (Constr).
        // 12+ is unknown.
        // Build the bytes manually: just 4 bits of 1100 (= 12)
        // followed by filler. 0b1100_0000: tag=12, then 4 padding bits.
        let bytes = vec![0xc0u8];
        let mut r = BitReader::new(&bytes);
        assert!(decode_term(&mut r).is_err());
    }

    #[test]
    fn reads_var_past_end_errors() {
        // tag=0 (Var) — 4 bits, then a Natural varint that's never
        // terminated.
        let bytes = vec![0x00];
        let mut r = BitReader::new(&bytes);
        let r_out = decode_term(&mut r);
        assert!(r_out.is_err(), "got {r_out:?}");
    }

    #[test]
    fn builtin_roundtrip_full_table() {
        for raw in 0u8..=87 {
            let id = BuiltinId::from_u8(raw).expect("from_u8");
            let t = Term::Builtin(id);
            let mut w = BitWriter::new();
            encode_term(&mut w, &t).expect("encode");
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            let decoded = decode_term(&mut r).expect("decode");
            assert_eq!(
                decoded,
                t,
                "builtin round-trip failed for {} (raw={raw})",
                id.name()
            );
        }
    }

    #[test]
    fn rejects_empty_universe_tag_list() {
        // tag=4 (Const), followed by a cons bit of 0 (empty list).
        // Layout: 0b0100_0xxx → 0x40 covers exactly tag=4 in the top
        // 4 bits + a single 0 cons bit at position 4.
        let bytes = vec![0x40, 0x00];
        let mut r = BitReader::new(&bytes);
        assert!(decode_term(&mut r).is_err());
    }
}
