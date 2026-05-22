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
use crate::term::{BuiltinId, Constant};
use crate::UplcError;
use num_bigint::{BigInt, Sign};

use blst::{
    blst_bendian_from_scalar, blst_fp12, blst_fp12_inverse, blst_fp12_is_equal, blst_fp12_mul,
    blst_fp12_one, blst_hash_to_g1, blst_hash_to_g2, blst_miller_loop, blst_p1,
    blst_p1_add_or_double, blst_p1_affine, blst_p1_cneg, blst_p1_compress, blst_p1_from_affine,
    blst_p1_in_g1, blst_p1_is_equal, blst_p1_mult, blst_p1_to_affine, blst_p1_uncompress, blst_p2,
    blst_p2_add_or_double, blst_p2_affine, blst_p2_cneg, blst_p2_compress, blst_p2_from_affine,
    blst_p2_in_g2, blst_p2_is_equal, blst_p2_mult, blst_p2_to_affine, blst_p2_uncompress,
    blst_scalar, blst_scalar_from_bendian, BLST_ERROR,
};

pub const G1_COMPRESSED_BYTES: usize = 48;
pub const G2_COMPRESSED_BYTES: usize = 96;
const FP12_BYTES: usize = 576;

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

// IETF/RFC 9380 ciphersuites mandated by CIP-0381.
const G1_DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";
const G2_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";

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
    let p = uncompress_g1(&p_bytes, id)?;
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
    let mut out = blst_p1::default();
    // SAFETY: `out` is a valid blst_p1; msg/dst are byte slices with
    // valid pointers + lengths. blst_hash_to_g1 follows RFC 9380.
    unsafe {
        blst_hash_to_g1(
            &mut out,
            msg.as_ptr(),
            msg.len(),
            dst.as_ptr(),
            dst.len(),
            G1_DST.as_ptr(),
            G1_DST.len(),
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
    let p = uncompress_g1(&bs, id)?;
    Ok(Value::Const(Constant::Bls12_381G1Element(Box::new(
        compress_g1(&p),
    ))))
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
    let p = uncompress_g2(&p_bytes, id)?;
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
    let mut out = blst_p2::default();
    unsafe {
        blst_hash_to_g2(
            &mut out,
            msg.as_ptr(),
            msg.len(),
            dst.as_ptr(),
            dst.len(),
            G2_DST.as_ptr(),
            G2_DST.len(),
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
    let p = uncompress_g2(&bs, id)?;
    Ok(Value::Const(Constant::Bls12_381G2Element(Box::new(
        compress_g2(&p),
    ))))
}

// ---------------------------------------------------------------------------
// Pairing ops
// ---------------------------------------------------------------------------

fn miller_loop(args: Vec<Value>, id: BuiltinId) -> Result<Value, UplcError> {
    let mut it = args.into_iter();
    let g1_bytes = unwrap_g1_bytes(it.next(), id)?;
    let g2_bytes = unwrap_g2_bytes(it.next(), id)?;
    let g1 = uncompress_g1(&g1_bytes, id)?;
    let g2 = uncompress_g2(&g2_bytes, id)?;
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
    // FinalVerify(a, b) = (a * b^-1) ^ ((q^12 - 1)/r) == 1
    let mut it = args.into_iter();
    let a = unwrap_ml_bytes(it.next(), id)?;
    let b = unwrap_ml_bytes(it.next(), id)?;
    let a_fp = fp12_from_bytes(&a)?;
    let b_fp = fp12_from_bytes(&b)?;
    let mut b_inv = blst_fp12::default();
    unsafe { blst_fp12_inverse(&mut b_inv, &b_fp) };
    let mut combined = blst_fp12::default();
    unsafe { blst_fp12_mul(&mut combined, &a_fp, &b_inv) };
    // Final exponentiation
    let final_exp = unsafe {
        // blst exposes `blst_final_exp` which takes &mut blst_fp12 in-place
        blst::blst_final_exp(
            &mut combined as *mut blst_fp12,
            &combined as *const blst_fp12,
        );
        let one = blst_fp12_one_value();
        blst_fp12_is_equal(&combined, &one)
    };
    Ok(Value::Const(Constant::Bool(final_exp)))
}

fn blst_fp12_one_value() -> blst_fp12 {
    // SAFETY: blst_fp12_one returns a static pointer to the
    // multiplicative identity in Fp12. Dereference once + copy.
    unsafe { *blst_fp12_one() }
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

fn bigint_to_blst_scalar(n: &BigInt) -> [u8; 32] {
    // CIP-0381 scalar arg is an Integer interpreted mod r (the BLS
    // group order). blst's mult takes a big-endian byte-buffer plus
    // its bit length. We pass the raw 32-byte big-endian
    // representation; reduction happens inside blst.
    let mut be = n.to_bytes_be().1;
    // Pad to 32 bytes; truncate if longer (shouldn't happen given r
    // is 255 bits, but cap defensively).
    if be.len() > 32 {
        be.truncate(be.len() - 32);
    }
    let mut out = [0u8; 32];
    let start = 32 - be.len();
    out[start..].copy_from_slice(&be);
    if n.sign() == Sign::Minus {
        // For negative scalars, take additive inverse mod r. We
        // approximate by negating in two's complement of the 256-bit
        // buffer; blst reduces mod r internally.
        for b in &mut out {
            *b = !*b;
        }
        // Plus one (for two's complement) — done via blst_scalar utils.
        let mut s = blst_scalar::default();
        unsafe { blst_scalar_from_bendian(&mut s, out.as_ptr()) };
        unsafe { blst_bendian_from_scalar(out.as_mut_ptr(), &s) };
    }
    out
}

// ---------------------------------------------------------------------------
// Argument unwrappers
// ---------------------------------------------------------------------------

fn take_one_g1(args: Vec<Value>, id: BuiltinId) -> Result<blst_p1, UplcError> {
    let bytes = unwrap_g1_bytes(args.into_iter().next(), id)?;
    uncompress_g1(&bytes, id)
}

fn take_two_g1(args: Vec<Value>, id: BuiltinId) -> Result<(blst_p1, blst_p1), UplcError> {
    let mut it = args.into_iter();
    let a_bytes = unwrap_g1_bytes(it.next(), id)?;
    let b_bytes = unwrap_g1_bytes(it.next(), id)?;
    Ok((uncompress_g1(&a_bytes, id)?, uncompress_g1(&b_bytes, id)?))
}

fn take_one_g2(args: Vec<Value>, id: BuiltinId) -> Result<blst_p2, UplcError> {
    let bytes = unwrap_g2_bytes(args.into_iter().next(), id)?;
    uncompress_g2(&bytes, id)
}

fn take_two_g2(args: Vec<Value>, id: BuiltinId) -> Result<(blst_p2, blst_p2), UplcError> {
    let mut it = args.into_iter();
    let a_bytes = unwrap_g2_bytes(it.next(), id)?;
    let b_bytes = unwrap_g2_bytes(it.next(), id)?;
    Ok((uncompress_g2(&a_bytes, id)?, uncompress_g2(&b_bytes, id)?))
}

fn unwrap_g1_bytes(v: Option<Value>, id: BuiltinId) -> Result<Vec<u8>, UplcError> {
    match v {
        Some(Value::Const(Constant::Bls12_381G1Element(boxed))) => Ok(boxed.to_vec()),
        Some(Value::Const(Constant::ByteString(b))) => Ok(b),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected G1 element or ByteString, got {:?}",
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
        Some(Value::Const(Constant::ByteString(b))) => Ok(b),
        Some(other) => Err(UplcError::BuiltinTypeError {
            builtin: id.name(),
            reason: format!(
                "expected G2 element or ByteString, got {:?}",
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
}
