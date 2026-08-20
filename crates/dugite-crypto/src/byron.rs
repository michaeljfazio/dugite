//! Byron-era Ed25519 verification (`cardano-crypto` 1.3.0 semantics).
//!
//! Byron predates cardano-base's libsodium-backed Ed25519 DSIGN (the strict
//! verifier `keys::PaymentVerificationKey::verify` implements for Shelley+,
//! #997). Byron's `Cardano.Crypto.Wallet.verify` (`CC.verify`) runs over
//! crypton/cryptonite's Ed25519, whose C core is **ed25519-donna** — the same
//! code `cardano-crypto` vendors under `cbits/ed25519/`. donna's
//! `ed25519_sign_open` (`ed25519.c:97-103`) accepts a strictly WIDER set of
//! signatures than both of dugite's existing Ed25519 verifiers:
//!
//! | input | donna (Byron) | `ed25519_dalek::verify` | `verify_strict` (#997) |
//! |---|---|---|---|
//! | S ∈ \[L, 2^253) (malleated valid sig) | accept | reject | reject |
//! | small-order A or R | accept (if equation holds) | accept | reject |
//!
//! Every historical Byron signature has a donna-accepted, dalek-rejected
//! malleated twin (`S' = S + L < 2^253` whenever `S < 2^253 - L`, which holds
//! for every valid `S < L`), so reusing either dalek entry point here is a
//! false-reject / chain-split bug, not a hardening improvement. See
//! `docs/superpowers/specs/2026-08-21-byron-signature-verification-design.md`
//! §1.1 for the full derivation, pinned against `cardano-crypto` 1.3.0.
//!
//! Do NOT reuse `keys::PaymentVerificationKey::verify` here: #997's
//! strictness is correct for Shelley-era DSIGN and wrong for Byron, which
//! predates that crypto stack entirely.

use sha2::{Digest, Sha512};

use curve25519_dalek_fork::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek_fork::scalar::Scalar;
use curve25519_dalek_fork::traits::VartimeMultiscalarMul as _;

/// ed25519-donna-exact signature verification, matching `cardano-crypto`
/// 1.3.0's `Cardano.Crypto.Wallet.verify` (crypton Ed25519 / ed25519-donna).
///
/// `xpub` is the 64-byte extended verification key (`CC.XPub`) — a 32-byte
/// Ed25519 public key followed by a 32-byte BIP32 chain code. The chain code
/// is never read: `CC.verify (XPub point _) ba (XSignature signature) = ...`
/// ignores it (design doc §1.1).
///
/// Verification algorithm, matching donna's `ed25519_sign_open`:
/// 1. Reject if `sig[63] & 0xE0 != 0` — only the TOP THREE BITS of `S` are
///    checked (`S < 2^253`), NOT full canonicality (`S < L`).
/// 2. `S` is reduced mod `L` before use
///    (`Scalar::from_bytes_mod_order`) — mathematically a no-op on the final
///    curve point vs. using `S` unreduced, since the basepoint has order
///    `L`, but this is the well-defined, safe way to obtain a
///    `curve25519-dalek` `Scalar` for the multiplication below.
/// 3. `A` must decompress to a curve point; small-order `A` is ACCEPTED
///    (no `is_small_order` check, unlike `vrf::verify_vrf_proof_v13`'s
///    public-key check — that omission here is deliberate, not an oversight).
/// 4. Cofactorless check: recompute `R' = [s]B - [H(R‖A‖M)]A` and
///    byte-compare its canonical encoding against the wire `R`. `R` itself
///    is never decompressed — it is used only as raw bytes inside the
///    challenge hash and in the final byte comparison, so a wire `R` that
///    encodes a small-order point (or, for that matter, anything at all) is
///    handled uniformly: it is accepted exactly when the byte comparison
///    matches, with no separate accept/reject path for small order.
pub fn verify_xsig(xpub: &[u8; 64], msg: &[u8], sig: &[u8; 64]) -> bool {
    let a_bytes = &xpub[0..32];
    let r_bytes = &sig[0..32];
    let s_bytes = &sig[32..64];

    // Step 1: only the top three bits of S are checked (S < 2^253).
    if sig[63] & 0xE0 != 0 {
        return false;
    }

    // Step 2: reduce S mod L.
    let mut s_arr = [0u8; 32];
    s_arr.copy_from_slice(s_bytes);
    let s = Scalar::from_bytes_mod_order(s_arr);

    // Step 3: A must decompress; small-order A is accepted.
    let a_point = match CompressedEdwardsY::from_slice(a_bytes).decompress() {
        Some(p) => p,
        None => return false,
    };

    // Challenge k = SHA-512(R || A || M) mod L.
    let k = {
        let mut hasher = Sha512::new();
        hasher.update(r_bytes);
        hasher.update(a_bytes);
        hasher.update(msg);
        let digest = hasher.finalize();
        let mut wide = [0u8; 64];
        wide.copy_from_slice(&digest);
        Scalar::from_bytes_mod_order_wide(&wide)
    };

    // Step 4: R' = [s]B - [k]A = (-k)*A + s*B, cofactorless.
    let r_check = EdwardsPoint::vartime_multiscalar_mul(
        [-k, s].iter().copied(),
        [
            a_point,
            curve25519_dalek_fork::constants::ED25519_BASEPOINT_POINT,
        ]
        .iter()
        .copied(),
    );

    r_check.compress().to_bytes() == *r_bytes
}

/// `SignBlock (VerificationKey issuerVK) -> "01" <> CC.unXPub issuerVK <>
/// "\x09" <> network` (`Cardano.Crypto.Signing.Tag:110-111`).
///
/// **Trap**: `"01"` is the TWO ASCII characters `0x30 0x31`
/// (`OverloadedStrings` — a fossil of the old `proxySign` prefix), NOT the
/// single byte `0x01`. `"\x09"` IS the single byte `0x09`.
pub fn sign_tag_block(genesis_xpub: &[u8; 64], magic_cbor: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 64 + 1 + magic_cbor.len());
    out.extend_from_slice(b"01");
    out.extend_from_slice(genesis_xpub);
    out.push(0x09);
    out.extend_from_slice(magic_cbor);
    out
}

/// `SignCertificate -> "\x0a" <> network` — the single byte `0x0A`, not the
/// two ASCII characters `"0a"`.
pub fn sign_tag_certificate(magic_cbor: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + magic_cbor.len());
    out.push(0x0A);
    out.extend_from_slice(magic_cbor);
    out
}

/// `SignUSProposal -> "\x04" <> network` — the single byte `0x04`.
pub fn sign_tag_us_proposal(magic_cbor: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + magic_cbor.len());
    out.push(0x04);
    out.extend_from_slice(magic_cbor);
    out
}

/// `SignUSVote -> "\x06" <> network` — the single byte `0x06`.
pub fn sign_tag_us_vote(magic_cbor: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + magic_cbor.len());
    out.push(0x06);
    out.extend_from_slice(magic_cbor);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use curve25519_dalek_fork::constants::ED25519_BASEPOINT_POINT;

    /// Build a real, honestly-produced Ed25519 signature via `ed25519_dalek`
    /// so we have a positive fixture without needing an external test
    /// vector. A real signature verifies identically under donna and dalek
    /// (design doc §1.1's "every real signature verifies identically"
    /// claim), so this is a valid basis for the donna-exact verifier too.
    fn honest_signature(seed: u8, msg: &[u8]) -> ([u8; 64], [u8; 64]) {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        let sig = sk.sign(msg);

        let mut xpub = [0u8; 64];
        xpub[..32].copy_from_slice(vk.as_bytes());
        // Chain code is ignored by `verify_xsig` — arbitrary non-zero filler
        // proves that claim rather than assuming it.
        xpub[32..].copy_from_slice(&[0xAB; 32]);

        let mut xsig = [0u8; 64];
        xsig.copy_from_slice(&sig.to_bytes());
        (xpub, xsig)
    }

    // ---- accept-real: a genuinely signed message verifies ----

    #[test]
    fn accepts_an_honest_signature() {
        let msg = b"01\x1a\x2d\x96\x4a\x09some byron sign-tag message";
        let (xpub, sig) = honest_signature(7, msg);
        assert!(verify_xsig(&xpub, msg, &sig));
    }

    #[test]
    fn chain_code_bytes_do_not_affect_verification() {
        let msg = b"chain code independence";
        let (mut xpub, sig) = honest_signature(11, msg);
        assert!(verify_xsig(&xpub, msg, &sig));
        // Flip every chain-code byte; the signature must still verify.
        for b in xpub[32..].iter_mut() {
            *b ^= 0xFF;
        }
        assert!(verify_xsig(&xpub, msg, &sig));
    }

    // ---- reject-wrong: RED proofs ----

    #[test]
    fn rejects_wrong_key() {
        let msg = b"message";
        let (_xpub, sig) = honest_signature(1, msg);
        let (other_xpub, _) = honest_signature(2, msg);
        assert!(!verify_xsig(&other_xpub, msg, &sig));
    }

    #[test]
    fn rejects_corrupted_signature_bytes() {
        let msg = b"message";
        let (xpub, mut sig) = honest_signature(3, msg);
        sig[0] ^= 0x01;
        assert!(!verify_xsig(&xpub, msg, &sig));
    }

    #[test]
    fn rejects_signature_valid_for_a_different_message() {
        let msg_a = b"message A";
        let msg_b = b"message B, not A";
        let (xpub, sig) = honest_signature(4, msg_a);
        assert!(!verify_xsig(&xpub, msg_b, &sig));
    }

    #[test]
    fn rejects_s_with_high_bits_set() {
        // Start from an honest signature, then set one of the top three
        // bits of S (byte 63). Step 1 of `verify_xsig` must reject this
        // before any curve arithmetic runs, regardless of R/A.
        let msg = b"high bit message";
        let (xpub, mut sig) = honest_signature(5, msg);
        sig[63] |= 0x80;
        assert!(!verify_xsig(&xpub, msg, &sig));
    }

    // ---- the donna-vs-dalek discriminator ----

    /// The group order L, little-endian, as used throughout curve25519-dalek
    /// (`BASEPOINT_ORDER` / `constants::L`, RFC 8032's `2^252 +
    /// 27742317777372353535851937790883648493`).
    const L_LE: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x10,
    ];

    /// `S + L`, the malleated twin of a valid signature's `S`. Ed25519
    /// dalek's `verify` and `verify_strict` both reject any `S >= L`;
    /// donna accepts `S` up to `2^253`. This test is the one that fails if
    /// `verify_xsig` is ever "simplified" to call `ed25519_dalek::verify` —
    /// see the module doc's table.
    #[test]
    fn accepts_s_plus_l_malleated_twin_of_a_valid_signature() {
        let msg = b"malleability probe";
        let (xpub, sig) = honest_signature(6, msg);

        // s' = s + L, computed as a 256-bit little-endian addition. Since
        // s < L < 2^253 and L < 2^253, s' = s + L < 2*2^253 = 2^254 — safely
        // inside the 2^253 donna bound whenever s < 2^253 - L (true for the
        // overwhelming majority of valid s, and in particular for this
        // fixed seed, asserted below rather than assumed).
        let s: [u8; 32] = sig[32..64].try_into().unwrap();
        let mut s_prime = [0u8; 32];
        let mut carry: u16 = 0;
        for i in 0..32 {
            let sum = s[i] as u16 + L_LE[i] as u16 + carry;
            s_prime[i] = sum as u8;
            carry = sum >> 8;
        }
        assert_eq!(carry, 0, "s + L must not overflow 256 bits for a valid s");
        assert_eq!(
            s_prime[31] & 0xE0,
            0,
            "s + L must satisfy donna's S < 2^253 bound for this fixture"
        );

        let mut malleated = sig;
        malleated[32..64].copy_from_slice(&s_prime);

        // The dalek verifiers reject this (S is non-canonical / >= L) —
        // proves the twin is a real malleability case, not a no-op.
        {
            use ed25519_dalek::{Signature, VerifyingKey};
            let vk = VerifyingKey::from_bytes(xpub[..32].try_into().unwrap()).unwrap();
            let dalek_sig = Signature::from_bytes(&malleated);
            assert!(
                ed25519_dalek::Verifier::verify(&vk, msg, &dalek_sig).is_err(),
                "the malleated signature must NOT verify under plain dalek verify \
                 (otherwise this is not exercising the donna/dalek divergence)"
            );
        }

        // donna-exact verify_xsig ACCEPTS it.
        assert!(
            verify_xsig(&xpub, msg, &malleated),
            "verify_xsig must accept the S+L malleated twin, matching donna \
             semantics — a dalek-backed implementation would wrongly reject \
             this, i.e. reject a signature cardano-node accepts"
        );
    }

    /// `A` = the identity point (`0x01 00 … 00`, order 1). With `A` =
    /// identity, `[k]A = identity` for every `k`, so the cofactorless
    /// equation degenerates to `[s]B = R`: for ANY scalar `s`, setting
    /// `R = compress([s]B)` produces a signature donna (and `verify_xsig`)
    /// accepts for ANY message, while `A` never decompress-fails and the
    /// function must not panic. This is the #997 shape one primitive over —
    /// Byron's OWN verifier is permissive here BY DESIGN (design doc §1.1),
    /// unlike Shelley's `verify_strict`.
    #[test]
    fn small_order_identity_key_does_not_panic_and_follows_the_equation() {
        let identity_xpub_first_32: [u8; 32] = {
            let mut b = [0u8; 32];
            b[0] = 0x01;
            b
        };
        let mut xpub = [0u8; 64];
        xpub[..32].copy_from_slice(&identity_xpub_first_32);

        // s = 5 (canonical, well under the donna bound); R = compress([s]B).
        let s_scalar = Scalar::from_bytes_mod_order({
            let mut b = [0u8; 32];
            b[0] = 5;
            b
        });
        let r_point = s_scalar * ED25519_BASEPOINT_POINT;
        let mut sig = [0u8; 64];
        sig[0..32].copy_from_slice(&r_point.compress().to_bytes());
        sig[32..64].copy_from_slice(s_scalar.as_bytes());

        // Accepted for an arbitrary message — no panic, equation holds.
        assert!(verify_xsig(&xpub, b"any message at all", &sig));
        assert!(verify_xsig(&xpub, b"a completely different message", &sig));
    }

    #[test]
    fn rejects_a_that_does_not_decompress() {
        // A y-coordinate whose corresponding x^2 has no square root mod p
        // does not decompress to a curve point. 0x02 with the high bit
        // clear is such a value for the standard Ed25519 curve — verified
        // here by asserting decompression fails, so the test is grounded in
        // measurement rather than assumption.
        let mut bad = [0u8; 32];
        bad[0] = 0x02;
        assert!(
            CompressedEdwardsY::from_slice(&bad).decompress().is_none(),
            "test fixture assumption: 0x02.. must not decompress"
        );
        let mut xpub = [0u8; 64];
        xpub[..32].copy_from_slice(&bad);
        let sig = [0u8; 64];
        assert!(!verify_xsig(&xpub, b"msg", &sig));
    }

    // ---- tag builders ----

    #[test]
    fn sign_tag_block_matches_the_ascii_01_trap() {
        let xpub = [0x11u8; 64];
        let magic = [0x1A, 0x2D, 0x96, 0x4A, 0x09]; // mainnet magic, CBOR-encoded
        let tag = sign_tag_block(&xpub, &magic);
        assert_eq!(&tag[0..2], b"01"); // ASCII 0x30 0x31, NOT byte 0x01
        assert_eq!(tag[0], 0x30);
        assert_eq!(tag[1], 0x31);
        assert_eq!(&tag[2..66], &xpub[..]);
        assert_eq!(tag[66], 0x09);
        assert_eq!(&tag[67..], &magic[..]);
    }

    #[test]
    fn sign_tag_certificate_is_a_single_control_byte_not_ascii() {
        let magic = [0x01];
        let tag = sign_tag_certificate(&magic);
        assert_eq!(tag, vec![0x0A, 0x01]);
    }

    #[test]
    fn sign_tag_us_proposal_and_vote_bytes() {
        let magic = [0x02];
        assert_eq!(sign_tag_us_proposal(&magic), vec![0x04, 0x02]);
        assert_eq!(sign_tag_us_vote(&magic), vec![0x06, 0x02]);
    }
}
