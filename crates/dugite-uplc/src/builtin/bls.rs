//! BLS12-381 builtin denotations (CIP-0381).
//!
//! Implements the seventeen Plutus V3 BLS12-381 builtins via the
//! `blst` crate (the C library Cardano-node itself uses). All values
//! cross the boundary as compressed IETF/zkcrypto bytestrings —
//! 48 bytes for G1, 96 bytes for G2 — and are decompressed on entry
//! into each denotation. Subgroup checks are performed on every
//! uncompressed input per CIP-0381 §"Subgroup checks".
//!
//! ## Why direct FFI
//!
//! `blst` exposes a high-level `min_pk` / `min_sig` API oriented
//! around BLS signatures, but the Plutus builtins are *generic*
//! G1/G2 arithmetic — we need raw add / neg / scalar-mul / pairing
//! across both groups. The raw `blst_*` symbols (and the `blst_p1` /
//! `blst_p2` / `blst_fp12` structs) cover this without imposing the
//! signature-API constraints. The unsafe surface is small,
//! well-bounded, and audited upstream.
//!
//! The crate-root lint `unsafe_code = "deny"` is selectively
//! overridden via `#[allow(unsafe_code)]` here; no other file in
//! dugite-uplc uses `unsafe`.

#![allow(unsafe_code)]

use crate::machine::value::Value;
use crate::term::{BuiltinId, Constant, TypeTag};
use crate::UplcError;
use num_bigint::BigInt;

use blst::{
    blst_fp12, blst_fp12_finalverify, blst_fp12_mul, blst_hash_to_g1, blst_hash_to_g2,
    blst_miller_loop, blst_p1, blst_p1_add_or_double, blst_p1_affine, blst_p1_cneg,
    blst_p1_compress, blst_p1_from_affine, blst_p1_in_g1, blst_p1_is_equal, blst_p1_mult,
    blst_p1_to_affine, blst_p1_uncompress, blst_p2, blst_p2_add_or_double, blst_p2_affine,
    blst_p2_cneg, blst_p2_compress, blst_p2_from_affine, blst_p2_in_g2, blst_p2_is_equal,
    blst_p2_mult, blst_p2_to_affine, blst_p2_uncompress, BLST_ERROR,
};

pub const G1_COMPRESSED_BYTES: usize = 48;
pub const G2_COMPRESSED_BYTES: usize = 96;
const FP12_BYTES: usize = 576;

// ---------------------------------------------------------------------------
// Decompressed-point cache (#839 residual)
// ---------------------------------------------------------------------------
//
// dugite stores G1/G2 `Constant`s compressed (see the doc comments on
// `Constant::Bls12_381G1Element`/`Bls12_381G2Element` in `term.rs`), so
// every denotation that consumes one re-runs `blst_p1_uncompress`/
// `blst_p2_uncompress` (point decompression: a modular sqrt plus curve/
// subgroup arithmetic, ~100-400µs) even though the Plutus cost model
// charges the CHEAP in-memory-point cost (it assumes a decompressed
// representation, matching Haskell's `BLS12_381.G1.Element` wrapping an
// already-parsed `Point`). A script that references the same compressed
// point across several builtin calls (a base point reused across many
// `scalarMul`s, a public key appearing in more than one
// `multiScalarMul`/pairing call, ...) pays that cost on every call.
//
// This is a pure, deterministic memoization keyed by the compressed
// bytes — thread-local (the CEK machine is single-threaded per
// evaluation; different rayon workers get independent caches, so no
// synchronization is needed) and capped (`BLS_DECOMPRESS_CACHE_CAP`) so
// an adversarial script computing many distinct valid points cannot grow
// it without bound for the life of the evaluating thread. Because this
// only memoizes a pure function of the compressed bytes, an eviction (or
// this cache never having been populated at all) can only ever cost a
// re-decompression — it can NEVER produce a different point, so there is
// no correctness/consensus dependency on cache behavior, only a
// performance one. Deliberately NOT a `Constant`/`Value`/`Term`-level
// change (the alternative the issue also considered): attaching the
// cache to the value itself would need `Constant`'s derived
// `PartialEq`/`Eq`/`Clone` (used well beyond BLS) to be hand-rolled to
// ignore the cache field, which is a much larger blast radius for the
// same benefit.
const BLS_DECOMPRESS_CACHE_CAP: usize = 1024;

thread_local! {
    static G1_DECOMPRESS_CACHE: std::cell::RefCell<
        std::collections::HashMap<[u8; G1_COMPRESSED_BYTES], blst_p1>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    static G2_DECOMPRESS_CACHE: std::cell::RefCell<
        std::collections::HashMap<[u8; G2_COMPRESSED_BYTES], blst_p2>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

// `fp12_to_bytes`/`fp12_from_bytes` below memcpy `FP12_BYTES` raw bytes
// in/out of a `blst_fp12` through a pointer cast, with no per-call size
// check. Pin the assumed layout at compile time (#843): if a future
// `blst` bump ever changed `blst_fp12`'s size, this turns what would
// otherwise be a stack buffer overflow into a compile error.
const _: () = assert!(std::mem::size_of::<blst_fp12>() == FP12_BYTES);

/// Validate a 48-byte BLS12-381 G1 compressed encoding without
/// materialising a `Value`.  Used by the textual parser to reject
/// invalid encodings at parse time (matching the Plutus reference,
/// which fails `(con bls12_381_G1_element 0x…)` at parse for bad-zero,
/// off-curve, or out-of-subgroup encodings).
///
/// Returns `Err(reason)` with a stable, human-readable explanation.
pub fn validate_g1_compressed(bs: &[u8]) -> Result<(), String> {
    if bs.len() != G1_COMPRESSED_BYTES {
        return Err(format!(
            "G1 expects {G1_COMPRESSED_BYTES}-byte compressed input, got {}",
            bs.len()
        ));
    }
    let mut aff = blst_p1_affine::default();
    let err = unsafe { blst_p1_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        return Err(format!("G1 uncompress failed: {err:?}"));
    }
    let mut p = blst_p1::default();
    unsafe { blst_p1_from_affine(&mut p, &aff) };
    if !unsafe { blst_p1_in_g1(&p) } {
        return Err("G1 point not in prime-order subgroup".into());
    }
    Ok(())
}

/// Validate a 96-byte BLS12-381 G2 compressed encoding without
/// materialising a `Value`.  See [`validate_g1_compressed`].
pub fn validate_g2_compressed(bs: &[u8]) -> Result<(), String> {
    if bs.len() != G2_COMPRESSED_BYTES {
        return Err(format!(
            "G2 expects {G2_COMPRESSED_BYTES}-byte compressed input, got {}",
            bs.len()
        ));
    }
    let mut aff = blst_p2_affine::default();
    let err = unsafe { blst_p2_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        return Err(format!("G2 uncompress failed: {err:?}"));
    }
    let mut p = blst_p2::default();
    unsafe { blst_p2_from_affine(&mut p, &aff) };
    if !unsafe { blst_p2_in_g2(&p) } {
        return Err("G2 point not in prime-order subgroup".into());
    }
    Ok(())
}

/// Maximum length of the caller-supplied DST byte string for
/// `bls12_381_*_hashToGroup`.  RFC 9380 §5.3.3 caps the DST at 255
/// bytes; CIP-0381 mandates the same limit. The Plutus reference
/// fails evaluation when the input DST exceeds this.
const MAX_HASH_TO_GROUP_DST_BYTES: usize = 255;

// Public entry: dispatch one BLS builtin from `BuiltinId` + args.
pub fn denote_bls(id: BuiltinId, args: Vec<Value>) -> Result<Value, UplcError> {
    use BuiltinId::*;
    match id {
        Bls12_381_G1_Add => g1_binop(args, id, g1_add),
        Bls12_381_G1_Neg => g1_unop(args, id, g1_neg),
        Bls12_381_G1_ScalarMul => g1_scalar_mul(args, id),
        Bls12_381_G1_Equal => g1_eq(args, id),
        Bls12_381_G1_HashToGroup => g1_hash_to_group(args, id),
        Bls12_381_G1_Compress => g1_compress(args, id),
        Bls12_381_G1_Uncompress => g1_uncompress(args, id),

        Bls12_381_G2_Add => g2_binop(args, id, g2_add),
        Bls12_381_G2_Neg => g2_unop(args, id, g2_neg),
        Bls12_381_G2_ScalarMul => g2_scalar_mul(args, id),
        Bls12_381_G2_Equal => g2_eq(args, id),
        Bls12_381_G2_HashToGroup => g2_hash_to_group(args, id),
        Bls12_381_G2_Compress => g2_compress(args, id),
        Bls12_381_G2_Uncompress => g2_uncompress(args, id),

        Bls12_381_MillerLoop => miller_loop(args, id),
        Bls12_381_MulMlResult => mul_ml(args, id),
        Bls12_381_FinalVerify => final_verify(args, id),

        _ => Err(UplcError::Internal(format!(
            "denote_bls called with non-BLS builtin {:?}",
            id
        ))),
    }
}

// ---------------------------------------------------------------------------
// G1 ops
// ---------------------------------------------------------------------------

fn g1_add(a: &blst_p1, b: &blst_p1) -> blst_p1 {
    let mut out = blst_p1::default();
    // SAFETY: `out`, `a`, `b` are all valid pointers to fully-
    // initialised `blst_p1` structs (the default impl zero-fills,
    // representing the identity element; `a` and `b` come from
    // verified uncompress paths).
    unsafe { blst_p1_add_or_double(&mut out, a, b) };
    out
}

fn g1_neg(p: &blst_p1) -> blst_p1 {
    let mut out = *p;
    // SAFETY: blst_p1_cneg negates in place; `out` is a valid copy.
    unsafe { blst_p1_cneg(&mut out, true) };
    out
}

fn g1_binop<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: Fn(&blst_p1, &blst_p1) -> blst_p1,
{
    let (a, b) = take_two_g1(args, id)?;
    let out = op(&a, &b);
    Ok(g1_to_value(&out))
}

fn g1_unop<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: Fn(&blst_p1) -> blst_p1,
{
    let p = take_one_g1(args, id)?;
    let out = op(&p);
    Ok(g1_to_value(&out))
}

fn g1_scalar_mul(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    // (scalar: Integer) (point: G1) -> G1
    let mut it = args.into_iter();
    let s = unwrap_integer_for_bls(it.next(), id)?;
    let p_bytes = unwrap_g1_bytes(it.next(), id)?;
    let p = decompress_g1_trusted(&p_bytes, id)?;
    let scalar = bigint_to_blst_scalar(&s);
    let mut out = blst_p1::default();
    // SAFETY: `out`, `p` are valid `blst_p1`; `scalar` is a 32-byte
    // big-endian buffer that blst interprets as a curve-order element.
    unsafe { blst_p1_mult(&mut out, &p, scalar.as_ptr(), scalar.len() * 8) };
    Ok(g1_to_value(&out))
}

fn g1_eq(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let (a, b) = take_two_g1(args, id)?;
    // SAFETY: both `a` and `b` are validated G1 points.
    let eq = unsafe { blst_p1_is_equal(&a, &b) };
    Ok(Value::Const(Constant::Bool(eq)))
}

fn g1_hash_to_group(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let msg = unwrap_bytes(it.next(), id)?;
    let dst = unwrap_bytes(it.next(), id)?;
    // RFC 9380 §5.3.3 / CIP-0381: DST is limited to 255 bytes.  The
    // Plutus reference rejects evaluation when the input DST exceeds
    // this; blst itself would otherwise just hash a marker substring,
    // producing a different point silently.
    if dst.len() > MAX_HASH_TO_GROUP_DST_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "DST length {} exceeds RFC 9380 / CIP-0381 maximum {}",
                dst.len(),
                MAX_HASH_TO_GROUP_DST_BYTES
            ),
        });
    }
    let mut out = blst_p1::default();
    // SAFETY: `out` is a valid blst_p1; msg/dst are byte slices with
    // valid pointers + lengths. blst_hash_to_g1 follows RFC 9380.
    //
    // Signature: `blst_hash_to_g1(out, msg, msg_len, DST, DST_len,
    // aug, aug_len)`.  Plutus does not pass an augmentation string,
    // so we pass an empty `aug`.  Previously this slot incorrectly
    // re-passed `G1_DST`, which produced a different curve point.
    unsafe {
        blst_hash_to_g1(
            &mut out,
            msg.as_ptr(),
            msg.len(),
            dst.as_ptr(),
            dst.len(),
            std::ptr::null(),
            0,
        )
    };
    Ok(g1_to_value(&out))
}

fn g1_compress(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let p = take_one_g1(args, id)?;
    Ok(Value::Const(Constant::ByteString(compress_g1(&p).to_vec())))
}

fn g1_uncompress(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let bs = unwrap_bytes(args.into_iter().next(), id)?;
    if bs.len() != G1_COMPRESSED_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "G1 uncompress expects {G1_COMPRESSED_BYTES} bytes, got {}",
                bs.len()
            ),
        });
    }
    // `uncompress_g1` validates encoding + subgroup membership; blst's
    // compressed form is canonical, so the (already-checked) input
    // bytes are byte-identical to `compress_g1(&p)` — return `bs`
    // directly rather than paying for a redundant re-compression on
    // this hot builtin (#843).
    uncompress_g1(&bs, id)?;
    let mut arr = [0u8; G1_COMPRESSED_BYTES];
    arr.copy_from_slice(&bs);
    Ok(Value::Const(Constant::Bls12_381G1Element(Box::new(arr))))
}

// ---------------------------------------------------------------------------
// G2 ops — mirror of G1.
// ---------------------------------------------------------------------------

fn g2_add(a: &blst_p2, b: &blst_p2) -> blst_p2 {
    let mut out = blst_p2::default();
    unsafe { blst_p2_add_or_double(&mut out, a, b) };
    out
}

fn g2_neg(p: &blst_p2) -> blst_p2 {
    let mut out = *p;
    unsafe { blst_p2_cneg(&mut out, true) };
    out
}

fn g2_binop<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: Fn(&blst_p2, &blst_p2) -> blst_p2,
{
    let (a, b) = take_two_g2(args, id)?;
    let out = op(&a, &b);
    Ok(g2_to_value(&out))
}

fn g2_unop<F>(args: Vec<Value>, id: BuiltinId, op: F) -> Result<Value, UplcError>
where
    F: Fn(&blst_p2) -> blst_p2,
{
    let p = take_one_g2(args, id)?;
    let out = op(&p);
    Ok(g2_to_value(&out))
}

fn g2_scalar_mul(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let s = unwrap_integer_for_bls(it.next(), id)?;
    let p_bytes = unwrap_g2_bytes(it.next(), id)?;
    let p = decompress_g2_trusted(&p_bytes, id)?;
    let scalar = bigint_to_blst_scalar(&s);
    let mut out = blst_p2::default();
    unsafe { blst_p2_mult(&mut out, &p, scalar.as_ptr(), scalar.len() * 8) };
    Ok(g2_to_value(&out))
}

fn g2_eq(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let (a, b) = take_two_g2(args, id)?;
    let eq = unsafe { blst_p2_is_equal(&a, &b) };
    Ok(Value::Const(Constant::Bool(eq)))
}

fn g2_hash_to_group(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let msg = unwrap_bytes(it.next(), id)?;
    let dst = unwrap_bytes(it.next(), id)?;
    // See `g1_hash_to_group`: RFC 9380 / CIP-0381 cap DST at 255 bytes.
    if dst.len() > MAX_HASH_TO_GROUP_DST_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "DST length {} exceeds RFC 9380 / CIP-0381 maximum {}",
                dst.len(),
                MAX_HASH_TO_GROUP_DST_BYTES
            ),
        });
    }
    let mut out = blst_p2::default();
    // Empty `aug` — same reasoning as `g1_hash_to_group`.
    unsafe {
        blst_hash_to_g2(
            &mut out,
            msg.as_ptr(),
            msg.len(),
            dst.as_ptr(),
            dst.len(),
            std::ptr::null(),
            0,
        )
    };
    Ok(g2_to_value(&out))
}

fn g2_compress(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let p = take_one_g2(args, id)?;
    Ok(Value::Const(Constant::ByteString(compress_g2(&p).to_vec())))
}

fn g2_uncompress(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let bs = unwrap_bytes(args.into_iter().next(), id)?;
    if bs.len() != G2_COMPRESSED_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "G2 uncompress expects {G2_COMPRESSED_BYTES} bytes, got {}",
                bs.len()
            ),
        });
    }
    // See `g1_uncompress`: canonical compressed form means the
    // already-validated input bytes are byte-identical to
    // `compress_g2(&p)` — avoid the redundant re-compression (#843).
    uncompress_g2(&bs, id)?;
    let mut arr = [0u8; G2_COMPRESSED_BYTES];
    arr.copy_from_slice(&bs);
    Ok(Value::Const(Constant::Bls12_381G2Element(Box::new(arr))))
}

// ---------------------------------------------------------------------------
// Pairing ops
// ---------------------------------------------------------------------------

fn miller_loop(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let g1_bytes = unwrap_g1_bytes(it.next(), id)?;
    let g2_bytes = unwrap_g2_bytes(it.next(), id)?;
    let g1 = decompress_g1_trusted(&g1_bytes, id)?;
    let g2 = decompress_g2_trusted(&g2_bytes, id)?;
    let mut g1_aff = blst_p1_affine::default();
    let mut g2_aff = blst_p2_affine::default();
    unsafe {
        blst_p1_to_affine(&mut g1_aff, &g1);
        blst_p2_to_affine(&mut g2_aff, &g2);
    }
    let mut out = blst_fp12::default();
    unsafe { blst_miller_loop(&mut out, &g2_aff, &g1_aff) };
    Ok(Value::Const(Constant::Bls12_381MlResult(Box::new(
        fp12_to_bytes(&out),
    ))))
}

fn mul_ml(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let a = unwrap_ml_bytes(it.next(), id)?;
    let b = unwrap_ml_bytes(it.next(), id)?;
    let a_fp = fp12_from_bytes(&a)?;
    let b_fp = fp12_from_bytes(&b)?;
    let mut out = blst_fp12::default();
    unsafe { blst_fp12_mul(&mut out, &a_fp, &b_fp) };
    Ok(Value::Const(Constant::Bls12_381MlResult(Box::new(
        fp12_to_bytes(&out),
    ))))
}

fn final_verify(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    // FinalVerify(a, b) == (finalExp(a) == finalExp(b)).
    //
    // `blst_fp12_finalverify` computes this directly (internally via
    // conjugation rather than a full inverse, so it is also cheaper
    // than the manual `a * b^-1; final_exp; is_equal` sequence this
    // used to do). Using it also removes a latent aliasing hazard
    // (#843): the previous code passed `&mut combined` and `&combined`
    // to `blst_final_exp` in the same call — overlapping mutable/shared
    // references to the same place, which is Stacked-Borrows UB even
    // though blst's C implementation happens to copy its input before
    // writing the output.
    let mut it = args.into_iter();
    let a = unwrap_ml_bytes(it.next(), id)?;
    let b = unwrap_ml_bytes(it.next(), id)?;
    let a_fp = fp12_from_bytes(&a)?;
    let b_fp = fp12_from_bytes(&b)?;
    // SAFETY: `a_fp`/`b_fp` are fully-initialised `blst_fp12` values;
    // `blst_fp12_finalverify` only reads through the given pointers.
    let final_exp = unsafe { blst_fp12_finalverify(&a_fp, &b_fp) };
    Ok(Value::Const(Constant::Bool(final_exp)))
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn uncompress_g1(bs: &[u8], id: BuiltinId) -> Result<blst_p1, UplcError> {
    if bs.len() != G1_COMPRESSED_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "G1 expects {G1_COMPRESSED_BYTES}-byte compressed input, got {}",
                bs.len()
            ),
        });
    }
    let mut aff = blst_p1_affine::default();
    let err = unsafe { blst_p1_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!("G1 uncompress failed: {:?}", err),
        });
    }
    let mut p = blst_p1::default();
    unsafe { blst_p1_from_affine(&mut p, &aff) };
    // CIP-0381 mandates a subgroup check.
    let in_g1 = unsafe { blst_p1_in_g1(&p) };
    if !in_g1 {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: "G1 point not in prime-order subgroup".into(),
        });
    }
    Ok(p)
}

/// Decompress G1 bytes that are already known-valid by construction —
/// i.e. extracted from a `Constant::Bls12_381G1Element`, never a raw
/// caller-supplied `ByteString` (see #816: `unwrap_g1_bytes` no longer
/// accepts `ByteString`). A `Bls12_381G1Element` can only come into
/// existence via a path that already performed the full subgroup check
/// (`uncompress_g1` for the `bls12_381_G1_uncompress` builtin, the
/// textual parser's `validate_g1_compressed`, `blst_hash_to_g1`'s
/// RFC 9380 output, which lands in-subgroup by construction, or group
/// arithmetic on already-valid points, which is closed under the
/// subgroup) — flat decode of a bare G1 constant is rejected outright
/// (no Haskell `Flat` instance exists for it). Re-running
/// `blst_p1_in_g1` here is therefore pure redundant work (#839): it
/// still decodes the compressed encoding (required, since `Constant`
/// stores only compressed bytes) but skips the expensive subgroup
/// re-check.
///
/// Memoized (#839 residual) via [`G1_DECOMPRESS_CACHE`): a script that
/// references the SAME compressed point across multiple builtin calls
/// (e.g. a base point reused across several `scalarMul`/`add` calls, or
/// the same public key appearing more than once across separate
/// `multiScalarMul`/pairing calls) pays the ~100-400µs decompression
/// cost once instead of on every call — closing the propagation/DoS gap
/// where the cost model charges the cheap in-memory-point cost but the
/// implementation was doing the expensive decompression every time.
/// This does NOT change the `Constant`/`Value`/`Term` shape (the
/// alternative the issue also considered): the cache is a pure,
/// deterministic memoization table keyed by the compressed bytes,
/// entirely internal to this module, so it cannot affect equality,
/// hashing, cloning, or encoding of a `Constant::Bls12_381G1Element`
/// anywhere else in the crate.
fn decompress_g1_trusted(bs: &[u8], id: BuiltinId) -> Result<blst_p1, UplcError> {
    debug_assert_eq!(
        bs.len(),
        G1_COMPRESSED_BYTES,
        "decompress_g1_trusted must only be called on bytes taken from a \
         Bls12_381G1Element, which is always exactly G1_COMPRESSED_BYTES long"
    );
    let mut key = [0u8; G1_COMPRESSED_BYTES];
    key.copy_from_slice(bs);
    if let Some(p) = G1_DECOMPRESS_CACHE.with(|c| c.borrow().get(&key).copied()) {
        return Ok(p);
    }
    let p = decompress_g1_trusted_uncached(bs, id)?;
    G1_DECOMPRESS_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        // Bound memory: an adversarial script computing an unbounded
        // number of DISTINCT valid points (each itself charged real
        // ExBudget to construct) would otherwise grow this cache without
        // limit for the lifetime of the evaluating thread. A flat clear
        // on overflow is simplest and sufficient here — this is a pure
        // memoization of a deterministic function, so evicting entries
        // can only ever cost a re-decompression, never a wrong answer.
        if cache.len() >= BLS_DECOMPRESS_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, p);
    });
    Ok(p)
}

/// The actual decompression work, uncached. See [`decompress_g1_trusted`].
fn decompress_g1_trusted_uncached(bs: &[u8], id: BuiltinId) -> Result<blst_p1, UplcError> {
    let mut aff = blst_p1_affine::default();
    let err = unsafe { blst_p1_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        // Should be unreachable: bytes came from an already-typed,
        // already-validated G1 element. Fail closed rather than panic.
        return Err(UplcError::Internal(format!(
            "{}: internal invariant violated — already-validated G1 element \
             failed to decode: {:?}",
            id.name(),
            err
        )));
    }
    let mut p = blst_p1::default();
    unsafe { blst_p1_from_affine(&mut p, &aff) };
    Ok(p)
}

fn compress_g1(p: &blst_p1) -> [u8; G1_COMPRESSED_BYTES] {
    let mut out = [0u8; G1_COMPRESSED_BYTES];
    unsafe { blst_p1_compress(out.as_mut_ptr(), p) };
    out
}

fn uncompress_g2(bs: &[u8], id: BuiltinId) -> Result<blst_p2, UplcError> {
    if bs.len() != G2_COMPRESSED_BYTES {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!(
                "G2 expects {G2_COMPRESSED_BYTES}-byte compressed input, got {}",
                bs.len()
            ),
        });
    }
    let mut aff = blst_p2_affine::default();
    let err = unsafe { blst_p2_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: format!("G2 uncompress failed: {:?}", err),
        });
    }
    let mut p = blst_p2::default();
    unsafe { blst_p2_from_affine(&mut p, &aff) };
    let in_g2 = unsafe { blst_p2_in_g2(&p) };
    if !in_g2 {
        return Err(UplcError::BuiltinFailure {
            builtin: id.name(),
            reason: "G2 point not in prime-order subgroup".into(),
        });
    }
    Ok(p)
}

/// Decompress G2 bytes that are already known-valid by construction.
/// See [`decompress_g1_trusted`] — the same invariant, and the same
/// (#839 residual) memoization via [`G2_DECOMPRESS_CACHE`], hold for G2.
fn decompress_g2_trusted(bs: &[u8], id: BuiltinId) -> Result<blst_p2, UplcError> {
    debug_assert_eq!(
        bs.len(),
        G2_COMPRESSED_BYTES,
        "decompress_g2_trusted must only be called on bytes taken from a \
         Bls12_381G2Element, which is always exactly G2_COMPRESSED_BYTES long"
    );
    let mut key = [0u8; G2_COMPRESSED_BYTES];
    key.copy_from_slice(bs);
    if let Some(p) = G2_DECOMPRESS_CACHE.with(|c| c.borrow().get(&key).copied()) {
        return Ok(p);
    }
    let p = decompress_g2_trusted_uncached(bs, id)?;
    G2_DECOMPRESS_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() >= BLS_DECOMPRESS_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, p);
    });
    Ok(p)
}

/// The actual decompression work, uncached. See [`decompress_g2_trusted`].
fn decompress_g2_trusted_uncached(bs: &[u8], id: BuiltinId) -> Result<blst_p2, UplcError> {
    let mut aff = blst_p2_affine::default();
    let err = unsafe { blst_p2_uncompress(&mut aff, bs.as_ptr()) };
    if err != BLST_ERROR::BLST_SUCCESS {
        return Err(UplcError::Internal(format!(
            "{}: internal invariant violated — already-validated G2 element \
             failed to decode: {:?}",
            id.name(),
            err
        )));
    }
    let mut p = blst_p2::default();
    unsafe { blst_p2_from_affine(&mut p, &aff) };
    Ok(p)
}

fn compress_g2(p: &blst_p2) -> [u8; G2_COMPRESSED_BYTES] {
    let mut out = [0u8; G2_COMPRESSED_BYTES];
    unsafe { blst_p2_compress(out.as_mut_ptr(), p) };
    out
}

fn g1_to_value(p: &blst_p1) -> Value {
    Value::Const(Constant::Bls12_381G1Element(Box::new(compress_g1(p))))
}

fn g2_to_value(p: &blst_p2) -> Value {
    Value::Const(Constant::Bls12_381G2Element(Box::new(compress_g2(p))))
}

fn fp12_to_bytes(fp: &blst_fp12) -> [u8; FP12_BYTES] {
    // blst_fp12 is repr(C) of 12 * blst_fp (48 bytes each) = 576 bytes.
    // We copy via byte-level memcpy through a pointer cast.
    let mut out = [0u8; FP12_BYTES];
    let src = fp as *const blst_fp12 as *const u8;
    unsafe { std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), FP12_BYTES) };
    out
}

fn fp12_from_bytes(bs: &[u8; FP12_BYTES]) -> Result<blst_fp12, UplcError> {
    let mut out = blst_fp12::default();
    let dst = &mut out as *mut blst_fp12 as *mut u8;
    unsafe { std::ptr::copy_nonoverlapping(bs.as_ptr(), dst, FP12_BYTES) };
    Ok(out)
}

/// BLS12-381 scalar-field modulus `r`, as big-endian bytes:
/// `0x73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001`.
const BLS_R_BE_BYTES: [u8; 32] = [
    0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8, 0x05,
    0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
];

/// Returns the scalar-field modulus `r` as a `BigInt`.
fn bls_scalar_r() -> BigInt {
    // Built from a fixed byte array rather than parsed from a hex
    // string at runtime (#843): the previous
    // `BigInt::parse_bytes(..).unwrap_or_else(|| BigInt::from(1))`
    // fallback was a live footgun — r=1 would silently reduce every
    // scalar to 0 (`n % 1 == 0`), turning every scalarMul/MSM into the
    // identity instead of erroring loudly. A byte-array construction
    // cannot fail, so there is no fallback path left to get wrong.
    BigInt::from_bytes_be(num_bigint::Sign::Plus, &BLS_R_BE_BYTES)
}

/// Check that a MSM scalar is within the valid Plutus range.
///
/// The Plutus specification (PlutusCore.Crypto.BLS12_381.Bounds) defines
/// the valid range for `multiScalarMul` scalars as a signed 512-byte
/// (4096-bit) integer:
///   lb = -(2^4095)   ub = 2^4095 - 1
/// Scalars outside `[lb, ub]` produce an evaluation failure; those
/// within the range are accepted and reduced mod r by blst before use.
/// This is NOT the same as `|s| < r` — scalars much larger than r are
/// fine as long as they fit in 512 bytes.
fn bigint_in_msm_scalar_range(n: &BigInt) -> bool {
    use num_bigint::Sign;
    // 0 is always in range.
    if n.sign() == Sign::NoSign {
        return true;
    }
    // ub = 2^4095 - 1, lb = -(2^4095).
    // For positive s: s <= ub  ↔  s <= 2^4095 - 1  ↔  s < 2^4095.
    // For negative s: s >= lb  ↔  |s| <= 2^4095.
    // Both can be expressed as |s| <= 2^4095 with the sign caveat that
    // -2^4095 is in range (lb) but 2^4095 is not (one past ub for positive).
    let mag = n.magnitude();
    let bound = num_bigint::BigUint::from(1u8) << 4095u32;
    if n.sign() == Sign::Plus {
        // positive: must be strictly less than 2^4095
        mag < &bound
    } else {
        // negative: |s| must be <= 2^4095 (the lower bound -(2^4095) is allowed)
        mag <= &bound
    }
}

fn bigint_to_blst_scalar(n: &BigInt) -> [u8; 32] {
    // CIP-0381 scalar arg is an Integer interpreted mod r (the BLS
    // scalar-field order).  `blst_p1_mult` / `blst_p2_mult` expect the
    // scalar as a **little-endian** byte buffer (blst README,
    // §"Scalar input").  The Haskell cardano-crypto-class wrapper
    // passes it LE as well.
    //
    // We reduce mod r ourselves so blst sees a canonical 0..r-1
    // representative and negation works on the curve.  `blst_p1_mult`
    // does not perform a mod-r reduction on a 256-bit buffer; large
    // or negative inputs interpreted naively give a wrong point.
    let r = bls_scalar_r();
    // Reduce: ((n mod r) + r) mod r — handles negative inputs.
    let reduced = ((n % &r) + &r) % &r;
    let abs_be = reduced.magnitude().to_bytes_be();
    let mut out = [0u8; 32];
    for (i, &b) in abs_be.iter().rev().enumerate() {
        if i >= 32 {
            break;
        }
        out[i] = b;
    }
    out
}

// ---------------------------------------------------------------------------
// Argument unwrappers
// ---------------------------------------------------------------------------

fn take_one_g1(args: Vec<Value>, id: BuiltinId) -> Result<blst_p1, UplcError> {
    let bytes = unwrap_g1_bytes(args.into_iter().next(), id)?;
    decompress_g1_trusted(&bytes, id)
}

fn take_two_g1(args: Vec<Value>, id: BuiltinId) -> Result<(blst_p1, blst_p1), UplcError> {
    let mut it = args.into_iter();
    let a_bytes = unwrap_g1_bytes(it.next(), id)?;
    let b_bytes = unwrap_g1_bytes(it.next(), id)?;
    Ok((
        decompress_g1_trusted(&a_bytes, id)?,
        decompress_g1_trusted(&b_bytes, id)?,
    ))
}

fn take_one_g2(args: Vec<Value>, id: BuiltinId) -> Result<blst_p2, UplcError> {
    let bytes = unwrap_g2_bytes(args.into_iter().next(), id)?;
    decompress_g2_trusted(&bytes, id)
}

fn take_two_g2(args: Vec<Value>, id: BuiltinId) -> Result<(blst_p2, blst_p2), UplcError> {
    let mut it = args.into_iter();
    let a_bytes = unwrap_g2_bytes(it.next(), id)?;
    let b_bytes = unwrap_g2_bytes(it.next(), id)?;
    Ok((
        decompress_g2_trusted(&a_bytes, id)?,
        decompress_g2_trusted(&b_bytes, id)?,
    ))
}

fn unwrap_g1_bytes(v: Option<Value>, id: BuiltinId) -> Result<Vec<u8>, UplcError> {
    match v {
        Some(Value::Const(Constant::Bls12_381G1Element(boxed))) => Ok(boxed.to_vec()),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected G1 element, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
        None => Err(UplcError::Internal(format!(
            "{}: missing argument",
            id.name()
        ))),
    }
}

fn unwrap_g2_bytes(v: Option<Value>, id: BuiltinId) -> Result<Vec<u8>, UplcError> {
    match v {
        Some(Value::Const(Constant::Bls12_381G2Element(boxed))) => Ok(boxed.to_vec()),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected G2 element, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
        None => Err(UplcError::Internal(format!(
            "{}: missing argument",
            id.name()
        ))),
    }
}

fn unwrap_ml_bytes(v: Option<Value>, id: BuiltinId) -> Result<[u8; FP12_BYTES], UplcError> {
    match v {
        Some(Value::Const(Constant::Bls12_381MlResult(boxed))) => Ok(*boxed),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected MlResult, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
        None => Err(UplcError::Internal(format!(
            "{}: missing argument",
            id.name()
        ))),
    }
}

fn unwrap_bytes(v: Option<Value>, id: BuiltinId) -> Result<Vec<u8>, UplcError> {
    match v {
        Some(Value::Const(Constant::ByteString(b))) => Ok(b),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected ByteString, got {:?}",
                std::mem::discriminant(&other)
            ),
        }),
        None => Err(UplcError::Internal(format!(
            "{}: missing argument",
            id.name()
        ))),
    }
}

/// Denotation for `bls12_381_G1_multiScalarMul` and
/// `bls12_381_G2_multiScalarMul` (PV1.1.0).
///
/// Computes `Σᵢ sᵢ·Pᵢ` (multi-scalar multiplication = sum of scalar
/// multiples of group elements).
///
/// Per the Haskell reference (`PlutusCore.Crypto.BLS12_381.G1.multiScalarMul`
/// / `G2.multiScalarMul`, which is a bare `zip ss ps` with no length-equality
/// check, feeding `Cardano.Crypto.EllipticCurve.BLS12_381.Internal.blsMSM`):
/// the two lists are **not** required to have equal length — extra entries
/// in the longer list are silently ignored (`zip` truncates to the shorter
/// list), and `[] `×`[]` succeeds, returning the group identity. Confirmed
/// against the upstream conformance corpus (`multiScalarMul-08`: literal
/// `[] []` → identity; `multiScalarMul-09a`/`10a`: a longer scalar list
/// with "extra entries ... ignored").
pub fn denote_multi_scalar_mul(id: BuiltinId, args: Vec<Value>) -> Result<Value, UplcError> {
    use BuiltinId::*;
    let mut it = args.into_iter();
    let scalars_val = it
        .next()
        .ok_or_else(|| UplcError::Internal(format!("{}: missing scalar list arg", id.name())))?;
    let points_val = it
        .next()
        .ok_or_else(|| UplcError::Internal(format!("{}: missing points list arg", id.name())))?;

    // Unwrap the scalar list (ProtoList Integer). Haskell's `readKnown`
    // for `[Integer]` checks the list's *declared* element-type witness
    // (`geqL` on the `DefaultUni` type tag embedded at parse/decode
    // time) against the expected type — independent of the list's
    // contents, and independent of whether it is empty (#827). A list
    // declared `(list bool) []` must fail here exactly like a non-empty
    // wrongly-typed list would, rather than unlifting as `[]`.
    let scalars: Vec<BigInt> = match scalars_val {
        Value::Const(Constant::ProtoList {
            elem_type,
            elements,
        }) => {
            if elem_type != TypeTag::Integer {
                return Err(UplcError::BuiltinTypeError {
                    builtin: id.name(),
                    reason: format!(
                        "expected (list integer) for scalars, got (list {elem_type:?})"
                    ),
                });
            }
            elements
                .into_iter()
                .map(|c| match c {
                    Constant::Integer(i) => Ok(i),
                    other => Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: format!(
                            "scalar list must contain Integers, got {:?}",
                            std::mem::discriminant(&other)
                        ),
                    }),
                })
                .collect::<Result<_, _>>()?
        }
        other => {
            return Err(UplcError::BuiltinTypeError {
                builtin: id.name(),
                reason: format!(
                    "expected ProtoList for scalars, got {:?}",
                    std::mem::discriminant(&other)
                ),
            })
        }
    };

    match id {
        Bls12_381_G1_MultiScalarMul => {
            // Unwrap the G1 point list. Same declared-elem_type check as
            // the scalar list above (#827) — including for an empty list.
            let points_bytes: Vec<Vec<u8>> = match points_val {
                Value::Const(Constant::ProtoList {
                    elem_type,
                    elements,
                }) => {
                    if elem_type != TypeTag::Bls12_381G1Element {
                        return Err(UplcError::BuiltinTypeError {
                            builtin: id.name(),
                            reason: format!(
                                "expected (list bls12_381_G1_element) for points, got (list {elem_type:?})"
                            ),
                        });
                    }
                    elements
                        .into_iter()
                        .map(|c| match c {
                            Constant::Bls12_381G1Element(boxed) => Ok(boxed.to_vec()),
                            other => Err(UplcError::BuiltinTypeError {
                                builtin: id.name(),
                                reason: format!(
                                    "G1 list must contain G1 elements, got {:?}",
                                    std::mem::discriminant(&other)
                                ),
                            }),
                        })
                        .collect::<Result<_, _>>()?
                }
                other => {
                    return Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: format!(
                            "expected ProtoList for G1 points, got {:?}",
                            std::mem::discriminant(&other)
                        ),
                    })
                }
            };
            // Truncate to the shorter list (Haskell zip semantics: extra
            // entries in either list are silently ignored).
            // Accumulate: start with G1 identity, add each sᵢ·Pᵢ.
            let mut acc = blst_p1::default(); // identity
            for (s, p_bytes) in scalars.into_iter().zip(points_bytes) {
                // Range check: scalar must be in (-r, r) so blst sees a
                // canonical representative.  |s| >= r is an error per the
                // Plutus conformance tests (multiScalarMul-13b/13d).
                if !bigint_in_msm_scalar_range(&s) {
                    return Err(UplcError::BuiltinFailure {
                        builtin: id.name(),
                        reason: "scalar is out of range (|s| >= r)".to_string(),
                    });
                }
                let p = decompress_g1_trusted(&p_bytes, id)?;
                let scalar = bigint_to_blst_scalar(&s);
                let mut term = blst_p1::default();
                // SAFETY: `term`, `p` are valid blst_p1; scalar is 32
                // big-endian bytes.
                unsafe { blst_p1_mult(&mut term, &p, scalar.as_ptr(), scalar.len() * 8) };
                unsafe { blst_p1_add_or_double(&mut acc, &acc, &term) };
            }
            Ok(g1_to_value(&acc))
        }
        Bls12_381_G2_MultiScalarMul => {
            // Same declared-elem_type check as G1 above (#827).
            let points_bytes: Vec<Vec<u8>> = match points_val {
                Value::Const(Constant::ProtoList {
                    elem_type,
                    elements,
                }) => {
                    if elem_type != TypeTag::Bls12_381G2Element {
                        return Err(UplcError::BuiltinTypeError {
                            builtin: id.name(),
                            reason: format!(
                                "expected (list bls12_381_G2_element) for points, got (list {elem_type:?})"
                            ),
                        });
                    }
                    elements
                        .into_iter()
                        .map(|c| match c {
                            Constant::Bls12_381G2Element(boxed) => Ok(boxed.to_vec()),
                            other => Err(UplcError::BuiltinTypeError {
                                builtin: id.name(),
                                reason: format!(
                                    "G2 list must contain G2 elements, got {:?}",
                                    std::mem::discriminant(&other)
                                ),
                            }),
                        })
                        .collect::<Result<_, _>>()?
                }
                other => {
                    return Err(UplcError::BuiltinTypeError {
                        builtin: id.name(),
                        reason: format!(
                            "expected ProtoList for G2 points, got {:?}",
                            std::mem::discriminant(&other)
                        ),
                    })
                }
            };
            // Truncate to the shorter list (Haskell zip semantics).
            let mut acc = blst_p2::default();
            for (s, p_bytes) in scalars.into_iter().zip(points_bytes) {
                if !bigint_in_msm_scalar_range(&s) {
                    return Err(UplcError::BuiltinFailure {
                        builtin: id.name(),
                        reason: "scalar is out of range (|s| >= r)".to_string(),
                    });
                }
                let p = decompress_g2_trusted(&p_bytes, id)?;
                let scalar = bigint_to_blst_scalar(&s);
                let mut term = blst_p2::default();
                unsafe { blst_p2_mult(&mut term, &p, scalar.as_ptr(), scalar.len() * 8) };
                unsafe { blst_p2_add_or_double(&mut acc, &acc, &term) };
            }
            Ok(g2_to_value(&acc))
        }
        _ => Err(UplcError::Internal(format!(
            "denote_multi_scalar_mul called with non-MSM builtin {:?}",
            id
        ))),
    }
}

fn unwrap_integer_for_bls(v: Option<Value>, id: BuiltinId) -> Result<BigInt, UplcError> {
    match v {
        Some(Value::Const(Constant::Integer(i))) => Ok(i),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!("expected Integer, got {:?}", std::mem::discriminant(&other)),
        }),
        None => Err(UplcError::Internal(format!(
            "{}: missing argument",
            id.name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(b: &[u8]) -> Value {
        Value::Const(Constant::ByteString(b.to_vec()))
    }

    #[test]
    fn g1_zero_compresses_to_canonical_infinity() {
        // The G1 identity (point at infinity) compresses to a known
        // 48-byte value: `0xc0` followed by 47 zero bytes (per IETF
        // ZCash serialisation, infinity bit set).
        let zero = blst_p1::default();
        let bytes = compress_g1(&zero);
        assert_eq!(bytes[0], 0xc0);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn g2_zero_compresses_to_canonical_infinity() {
        let zero = blst_p2::default();
        let bytes = compress_g2(&zero);
        assert_eq!(bytes[0], 0xc0);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn g1_uncompress_rejects_wrong_length() {
        let err = denote_bls(BuiltinId::Bls12_381_G1_Uncompress, vec![bs(&[0u8; 47])]).unwrap_err();
        assert!(matches!(err, UplcError::BuiltinFailure { .. }));
    }

    #[test]
    fn g1_hash_to_group_is_deterministic() {
        let v1 = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"hello"), bs(b"dst")],
        )
        .unwrap();
        let v2 = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"hello"), bs(b"dst")],
        )
        .unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn g1_add_is_commutative() {
        let a = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"a"), bs(b"dst")],
        )
        .unwrap();
        let b = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"b"), bs(b"dst")],
        )
        .unwrap();
        let ab = denote_bls(BuiltinId::Bls12_381_G1_Add, vec![a.clone(), b.clone()]).unwrap();
        let ba = denote_bls(BuiltinId::Bls12_381_G1_Add, vec![b, a]).unwrap();
        assert_eq!(ab, ba);
    }

    // ── #839 residual: decompressed-point cache ─────────────────────

    /// A repeated call decompressing the SAME compressed G1 bytes must
    /// return a bit-identical point whether it's served from the cache
    /// (warm) or freshly decompressed (cold) — pins that the memoization
    /// added for #839 cannot change the result, only whether the
    /// expensive `blst_p1_uncompress` path runs.
    #[test]
    fn decompress_g1_trusted_cache_hit_matches_cold_decompress() {
        let point = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"cache-g1"), bs(b"dst")],
        )
        .unwrap();
        let Value::Const(Constant::Bls12_381G1Element(compressed)) = point else {
            panic!("expected G1 element");
        };
        // First call: populates the cache (or reuses it from an earlier
        // test on the same thread — either way, deterministic).
        let cold =
            decompress_g1_trusted_uncached(compressed.as_slice(), BuiltinId::Bls12_381_G1_Add)
                .unwrap();
        let warm =
            decompress_g1_trusted(compressed.as_slice(), BuiltinId::Bls12_381_G1_Add).unwrap();
        assert!(
            unsafe { blst_p1_is_equal(&cold, &warm) },
            "cached decompression must be bit-identical to a fresh decompress"
        );
        // Call again — this time definitely a cache hit.
        let warm2 =
            decompress_g1_trusted(compressed.as_slice(), BuiltinId::Bls12_381_G1_Add).unwrap();
        assert!(unsafe { blst_p1_is_equal(&cold, &warm2) });
    }

    /// Same as above for G2.
    #[test]
    fn decompress_g2_trusted_cache_hit_matches_cold_decompress() {
        let point = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"cache-g2"), bs(b"dst")],
        )
        .unwrap();
        let Value::Const(Constant::Bls12_381G2Element(compressed)) = point else {
            panic!("expected G2 element");
        };
        let cold =
            decompress_g2_trusted_uncached(compressed.as_slice(), BuiltinId::Bls12_381_G2_Add)
                .unwrap();
        let warm =
            decompress_g2_trusted(compressed.as_slice(), BuiltinId::Bls12_381_G2_Add).unwrap();
        assert!(unsafe { blst_p2_is_equal(&cold, &warm) });
        let warm2 =
            decompress_g2_trusted(compressed.as_slice(), BuiltinId::Bls12_381_G2_Add).unwrap();
        assert!(unsafe { blst_p2_is_equal(&cold, &warm2) });
    }

    /// A script that references the same G1 point across multiple
    /// separate builtin calls (the actual scenario #839 is about — e.g.
    /// a base point reused across several `scalarMul`s) must produce
    /// results identical to what an uncached implementation would give.
    /// Exercises the cache purely through the public `denote_bls` entry
    /// point (no internal cache access) so this also serves as an
    /// end-to-end proof that caching is transparent to callers.
    #[test]
    fn repeated_scalar_mul_on_same_cached_point_is_consistent() {
        let base = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"repeated-base"), bs(b"dst")],
        )
        .unwrap();
        let scalar = Value::Const(Constant::Integer(BigInt::from(7)));
        let r1 = denote_bls(
            BuiltinId::Bls12_381_G1_ScalarMul,
            vec![scalar.clone(), base.clone()],
        )
        .unwrap();
        let r2 = denote_bls(BuiltinId::Bls12_381_G1_ScalarMul, vec![scalar, base]).unwrap();
        assert_eq!(r1, r2, "repeated scalarMul on the same point must agree");
    }

    /// Cache-overflow robustness: computing more than
    /// `BLS_DECOMPRESS_CACHE_CAP` DISTINCT valid G1 points must not error
    /// or panic — the cache clears itself on overflow (a pure
    /// memoization, so eviction only ever costs a re-decompression).
    /// Uses a small local loop rather than actually exceeding the real
    /// (1024-entry) cap, by driving the SAME cache with enough distinct
    /// keys to force at least one clear-and-refill cycle within a
    /// reasonable test runtime — the loop bound is deliberately larger
    /// than the cap so the overflow branch is guaranteed to execute.
    #[test]
    fn cache_overflow_clears_without_error() {
        for i in 0..(BLS_DECOMPRESS_CACHE_CAP + 8) {
            let msg = format!("overflow-{i}");
            let point = denote_bls(
                BuiltinId::Bls12_381_G1_HashToGroup,
                vec![bs(msg.as_bytes()), bs(b"dst")],
            )
            .unwrap();
            // Immediately re-use it (forces a decompress right after
            // insertion/eviction) to prove correctness isn't disturbed.
            let doubled = denote_bls(BuiltinId::Bls12_381_G1_Add, vec![point.clone(), point])
                .expect("must not error even across a cache clear-and-refill cycle");
            assert!(matches!(
                doubled,
                Value::Const(Constant::Bls12_381G1Element(_))
            ));
        }
    }

    #[test]
    fn g1_neg_is_self_inverse() {
        let a = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"x"), bs(b"dst")],
        )
        .unwrap();
        let neg_a = denote_bls(BuiltinId::Bls12_381_G1_Neg, vec![a.clone()]).unwrap();
        let sum = denote_bls(BuiltinId::Bls12_381_G1_Add, vec![a, neg_a]).unwrap();
        // a + (-a) = identity (G1 zero point, compress = 0xc0 + zeros)
        if let Value::Const(Constant::Bls12_381G1Element(b)) = sum {
            assert_eq!(b[0], 0xc0);
        } else {
            panic!("expected G1 element");
        }
    }

    #[test]
    fn g1_equal_self() {
        let a = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"x"), bs(b"dst")],
        )
        .unwrap();
        let eq = denote_bls(BuiltinId::Bls12_381_G1_Equal, vec![a.clone(), a]).unwrap();
        assert_eq!(eq, Value::Const(Constant::Bool(true)));
    }

    #[test]
    fn g2_neg_is_self_inverse() {
        let a = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"x"), bs(b"dst")],
        )
        .unwrap();
        let neg_a = denote_bls(BuiltinId::Bls12_381_G2_Neg, vec![a.clone()]).unwrap();
        let sum = denote_bls(BuiltinId::Bls12_381_G2_Add, vec![a, neg_a]).unwrap();
        if let Value::Const(Constant::Bls12_381G2Element(b)) = sum {
            assert_eq!(b[0], 0xc0);
        } else {
            panic!("expected G2 element");
        }
    }

    #[test]
    fn miller_loop_followed_by_final_verify_returns_true_for_matching() {
        // e(G1_hash("a"), G2_hash("b")) == e(G1_hash("a"), G2_hash("b"))
        let g1 = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"a"), bs(b"dst")],
        )
        .unwrap();
        let g2 = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"b"), bs(b"dst")],
        )
        .unwrap();
        let ml = denote_bls(
            BuiltinId::Bls12_381_MillerLoop,
            vec![g1.clone(), g2.clone()],
        )
        .unwrap();
        let ml2 = denote_bls(BuiltinId::Bls12_381_MillerLoop, vec![g1, g2]).unwrap();
        let verify = denote_bls(BuiltinId::Bls12_381_FinalVerify, vec![ml, ml2]).unwrap();
        assert_eq!(verify, Value::Const(Constant::Bool(true)));
    }

    #[test]
    fn final_verify_returns_false_for_non_matching_pairing() {
        // e(hash("a"), hash("c")) != e(hash("b"), hash("c")) for a != b.
        // Regression for #843: `final_verify` now calls
        // `blst_fp12_finalverify` directly instead of the old manual
        // inverse+mul+final_exp+is_equal sequence — confirm it still
        // distinguishes non-matching pairings, not just matching ones.
        let g1a = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"a"), bs(b"dst")],
        )
        .unwrap();
        let g1b = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"b"), bs(b"dst")],
        )
        .unwrap();
        let g2 = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"c"), bs(b"dst")],
        )
        .unwrap();
        let ml_a = denote_bls(BuiltinId::Bls12_381_MillerLoop, vec![g1a, g2.clone()]).unwrap();
        let ml_b = denote_bls(BuiltinId::Bls12_381_MillerLoop, vec![g1b, g2]).unwrap();
        let verify = denote_bls(BuiltinId::Bls12_381_FinalVerify, vec![ml_a, ml_b]).unwrap();
        assert_eq!(verify, Value::Const(Constant::Bool(false)));
    }

    // ── #816: G1/G2 group-consuming builtins must reject a raw
    // ByteString where a group element is required ──────────────────

    fn valid_g1_point_bytes() -> [u8; G1_COMPRESSED_BYTES] {
        let v = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"816-g1"), bs(b"dst")],
        )
        .unwrap();
        match v {
            Value::Const(Constant::Bls12_381G1Element(b)) => *b,
            _ => panic!("expected G1 element"),
        }
    }

    fn valid_g2_point_bytes() -> [u8; G2_COMPRESSED_BYTES] {
        let v = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"816-g2"), bs(b"dst")],
        )
        .unwrap();
        match v {
            Value::Const(Constant::Bls12_381G2Element(b)) => *b,
            _ => panic!("expected G2 element"),
        }
    }

    fn assert_type_error(result: Result<Value, UplcError>) {
        match result {
            Err(UplcError::BuiltinTypeError { .. }) => {}
            other => panic!("expected BuiltinTypeError, got {other:?}"),
        }
    }

    #[test]
    fn g1_builtins_reject_bytestring_in_place_of_element() {
        // A `Constant::ByteString` holding a *valid* compressed G1
        // point must still be rejected by every builtin that consumes
        // a G1 element — Haskell's `readKnown` distinguishes the types
        // at the unlifting boundary regardless of the byte payload.
        let raw = valid_g1_point_bytes();
        let as_bs = || bs(&raw);

        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G1_Add,
            vec![as_bs(), as_bs()],
        ));
        assert_type_error(denote_bls(BuiltinId::Bls12_381_G1_Neg, vec![as_bs()]));
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G1_ScalarMul,
            vec![Value::Const(Constant::Integer(BigInt::from(3))), as_bs()],
        ));
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G1_Equal,
            vec![as_bs(), as_bs()],
        ));
        assert_type_error(denote_bls(BuiltinId::Bls12_381_G1_Compress, vec![as_bs()]));

        let g2 = denote_bls(
            BuiltinId::Bls12_381_G2_HashToGroup,
            vec![bs(b"pair"), bs(b"dst")],
        )
        .unwrap();
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_MillerLoop,
            vec![as_bs(), g2],
        ));
    }

    #[test]
    fn g2_builtins_reject_bytestring_in_place_of_element() {
        let raw = valid_g2_point_bytes();
        let as_bs = || bs(&raw);

        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G2_Add,
            vec![as_bs(), as_bs()],
        ));
        assert_type_error(denote_bls(BuiltinId::Bls12_381_G2_Neg, vec![as_bs()]));
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G2_ScalarMul,
            vec![Value::Const(Constant::Integer(BigInt::from(3))), as_bs()],
        ));
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_G2_Equal,
            vec![as_bs(), as_bs()],
        ));
        assert_type_error(denote_bls(BuiltinId::Bls12_381_G2_Compress, vec![as_bs()]));

        let g1 = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"pair"), bs(b"dst")],
        )
        .unwrap();
        assert_type_error(denote_bls(
            BuiltinId::Bls12_381_MillerLoop,
            vec![g1, as_bs()],
        ));
    }

    // ── #827: multiScalarMul must check the declared list elem_type,
    // including for empty lists; length mismatch truncates (zip) ────

    #[test]
    fn msm_g1_rejects_wrong_typed_empty_scalar_list() {
        // `(list bool) []` fed where `[Integer]` is expected must fail
        // even though the list is empty — matching Haskell's `geqL`
        // universe-tag check, which never inspects list contents.
        let bad_scalars = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bool,
            elements: vec![],
        });
        let points = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bls12_381G1Element,
            elements: vec![],
        });
        let err = denote_multi_scalar_mul(
            BuiltinId::Bls12_381_G1_MultiScalarMul,
            vec![bad_scalars, points],
        )
        .unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn msm_g1_rejects_wrong_typed_empty_points_list() {
        // An empty G2-element list fed where `[G1.Element]` is
        // expected must fail (wrong group), not silently unlift as `[]`.
        let scalars = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Integer,
            elements: vec![],
        });
        let bad_points = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bls12_381G2Element,
            elements: vec![],
        });
        let err = denote_multi_scalar_mul(
            BuiltinId::Bls12_381_G1_MultiScalarMul,
            vec![scalars, bad_points],
        )
        .unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn msm_g2_rejects_wrong_typed_empty_points_list() {
        let scalars = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Integer,
            elements: vec![],
        });
        let bad_points = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bls12_381G1Element,
            elements: vec![],
        });
        let err = denote_multi_scalar_mul(
            BuiltinId::Bls12_381_G2_MultiScalarMul,
            vec![scalars, bad_points],
        )
        .unwrap_err();
        assert!(matches!(err, UplcError::BuiltinTypeError { .. }));
    }

    #[test]
    fn msm_g1_empty_times_empty_is_identity() {
        // Correctly-typed empty lists must still succeed, returning the
        // G1 identity — Haskell's `blsMSM (zip [] [])` = `blsZero`.
        let scalars = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Integer,
            elements: vec![],
        });
        let points = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bls12_381G1Element,
            elements: vec![],
        });
        let result = denote_multi_scalar_mul(
            BuiltinId::Bls12_381_G1_MultiScalarMul,
            vec![scalars, points],
        )
        .unwrap();
        match result {
            Value::Const(Constant::Bls12_381G1Element(b)) => {
                assert_eq!(b[0], 0xc0);
                assert!(b[1..].iter().all(|&x| x == 0));
            }
            other => panic!("expected G1 element, got {other:?}"),
        }
    }

    #[test]
    fn msm_g1_mismatched_lengths_truncate_to_shorter_zip() {
        // Haskell's denotation is a bare `zip ss ps` with no length
        // check (confirmed against PlutusCore.Crypto.BLS12_381.G1 via
        // the cardano-haskell-oracle, and the upstream conformance
        // vectors multiScalarMul-08/09a/10a): extra entries in the
        // longer list are silently dropped. A 2-scalar / 1-point call
        // must equal `scalarMul(scalars[0], points[0])`.
        let p0 = denote_bls(
            BuiltinId::Bls12_381_G1_HashToGroup,
            vec![bs(b"msm-p0"), bs(b"dst")],
        )
        .unwrap();
        let p0_bytes = match &p0 {
            Value::Const(Constant::Bls12_381G1Element(b)) => **b,
            _ => panic!("expected G1 element"),
        };

        let scalars = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Integer,
            elements: vec![
                Constant::Integer(BigInt::from(7)),
                // No matching point for this second scalar — must be
                // ignored, not cause a BuiltinFailure.
                Constant::Integer(BigInt::from(99)),
            ],
        });
        let points = Value::Const(Constant::ProtoList {
            elem_type: TypeTag::Bls12_381G1Element,
            elements: vec![Constant::Bls12_381G1Element(Box::new(p0_bytes))],
        });
        let msm_result = denote_multi_scalar_mul(
            BuiltinId::Bls12_381_G1_MultiScalarMul,
            vec![scalars, points],
        )
        .unwrap();

        let expected = denote_bls(
            BuiltinId::Bls12_381_G1_ScalarMul,
            vec![Value::Const(Constant::Integer(BigInt::from(7))), p0],
        )
        .unwrap();

        assert_eq!(msm_result, expected);
    }

    // ── #843: bls_scalar_r must be the real BLS12-381 scalar-field
    // modulus, not the unreachable-but-dangerous fallback ────────────

    #[test]
    fn bls_scalar_r_matches_known_modulus() {
        let r = bls_scalar_r();
        let expected = BigInt::parse_bytes(
            b"52435875175126190479447740508185965837690552500527637822603658699938581184513",
            10,
        )
        .unwrap();
        assert_eq!(r, expected);
    }

    #[test]
    fn fp12_size_matches_blst_layout() {
        assert_eq!(std::mem::size_of::<blst_fp12>(), FP12_BYTES);
    }
}
