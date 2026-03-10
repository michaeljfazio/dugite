/// VRF (Verifiable Random Function) support
///
/// In Cardano's Ouroboros Praos, VRF is used for:
/// 1. Leader election: determining if a stake pool can produce a block in a given slot
/// 2. Epoch nonce: contributing randomness to the epoch nonce
///
/// The VRF implementation uses ECVRF-ED25519-SHA512-Elligator2
/// (IETF draft-irtf-cfrg-vrf-03) as used by the Cardano reference node.
use curve25519_dalek_fork::constants::ED25519_BASEPOINT_POINT;
use thiserror::Error;
use vrf_dalek::vrf03::{PublicKey03, SecretKey03, VrfProof03};

#[derive(Error, Debug)]
pub enum VrfError {
    #[error("Invalid VRF proof: {0}")]
    InvalidProof(String),
    #[error("Invalid VRF public key")]
    InvalidPublicKey,
    #[error("VRF verification failed")]
    VerificationFailed,
}

/// Verify a VRF proof and return the 64-byte VRF output.
///
/// - `vrf_vkey`: 32-byte VRF verification key from the block header
/// - `proof_bytes`: 80-byte VRF proof from the block header
/// - `seed`: the VRF input (eta_v || slot for leader, eta_v || epoch for nonce)
///
/// Returns the 64-byte VRF output on success.
pub fn verify_vrf_proof(
    vrf_vkey: &[u8],
    proof_bytes: &[u8],
    seed: &[u8],
) -> Result<[u8; 64], VrfError> {
    if vrf_vkey.len() != 32 {
        return Err(VrfError::InvalidPublicKey);
    }
    if proof_bytes.len() != 80 {
        return Err(VrfError::InvalidProof(format!(
            "expected 80 bytes, got {}",
            proof_bytes.len()
        )));
    }

    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(vrf_vkey);
    let public_key = PublicKey03::from_bytes(&pk_bytes);

    let mut proof_arr = [0u8; 80];
    proof_arr.copy_from_slice(proof_bytes);
    let proof =
        VrfProof03::from_bytes(&proof_arr).map_err(|e| VrfError::InvalidProof(format!("{e:?}")))?;

    proof
        .verify(&public_key, seed)
        .map_err(|_| VrfError::VerificationFailed)
}

/// Extract the VRF output hash from a proof without verification.
///
/// This is used when you need the output value (e.g., for the leader
/// eligibility check) but have already verified the proof, or during
/// initial sync where verification may be deferred.
pub fn vrf_proof_to_hash(proof_bytes: &[u8]) -> Result<[u8; 64], VrfError> {
    if proof_bytes.len() != 80 {
        return Err(VrfError::InvalidProof(format!(
            "expected 80 bytes, got {}",
            proof_bytes.len()
        )));
    }

    let mut proof_arr = [0u8; 80];
    proof_arr.copy_from_slice(proof_bytes);
    let proof =
        VrfProof03::from_bytes(&proof_arr).map_err(|e| VrfError::InvalidProof(format!("{e:?}")))?;

    Ok(proof.proof_to_hash())
}

/// Check if a VRF output certifies leader election for a given slot.
///
/// Implements the exact same algorithm as Haskell's `checkLeaderNatValue`:
///
/// The check is: `p < 1 - (1-f)^sigma` where p = certNat / certNatMax.
/// Rearranged: `certNatMax / (certNatMax - certNat) < exp(sigma * |ln(1-f)|)`
///
/// Uses 34-decimal-digit fixed-point arithmetic (matching Haskell's `Digits34`)
/// with Taylor series comparison (`taylorExpCmp`) for exact precision.
pub fn check_leader_value(vrf_output: &[u8], relative_stake: f64, active_slot_coeff: f64) -> bool {
    leader_check::check_leader_value_exact(vrf_output, relative_stake, active_slot_coeff)
}

/// Exact-precision VRF leader check matching Haskell's `checkLeaderNatValue`.
///
/// Uses `num-bigint` for 34-digit fixed-point arithmetic, replicating:
/// - `Cardano.Protocol.TPraos.BHeader.checkLeaderNatValue`
/// - `Cardano.Ledger.NonIntegral.taylorExpCmp`
/// - `Cardano.Ledger.BaseTypes.FixedPoint` (10^34 resolution)
mod leader_check {
    use num_bigint::BigInt;
    use num_traits::{One, Signed, Zero};

    /// Scale factor for 34-digit fixed-point arithmetic (matches Haskell's E34).
    fn fp_scale() -> BigInt {
        BigInt::from(10).pow(34)
    }

    /// certNatMax = 2^256
    fn cert_nat_max() -> BigInt {
        BigInt::from(2).pow(256)
    }

    /// Convert a 32-byte VRF leader value to a big-endian BigInt.
    fn bytes_to_natural(bytes: &[u8]) -> BigInt {
        BigInt::from_bytes_be(num_bigint::Sign::Plus, bytes)
    }

    /// Fixed-point multiplication: (a * b) / scale
    fn fp_mul(a: &BigInt, b: &BigInt, scale: &BigInt) -> BigInt {
        (a * b) / scale
    }

    /// Fixed-point division: (a * scale) / b (truncates toward zero, matching Haskell Data.Fixed)
    fn fp_div(a: &BigInt, b: &BigInt, scale: &BigInt) -> BigInt {
        (a * scale) / b
    }

    /// Compute ln(x) for x in (0, 2) using the series:
    /// ln(x) = ln(1 + y) where y = x - 1
    /// ln(1 + y) = y - y^2/2 + y^3/3 - y^4/4 + ...
    ///
    /// All arithmetic is in fixed-point with the given scale.
    /// `x` is in fixed-point (i.e., actual_value * scale).
    fn fp_ln(x: &BigInt, scale: &BigInt) -> BigInt {
        // y = x - 1.0 (in fixed-point: x - scale)
        let y = x - scale;
        if y.is_zero() {
            return BigInt::zero();
        }

        let mut result = BigInt::zero();
        let mut y_power = y.clone(); // y^1
        let max_terms = 1000;

        for n in 1..=max_terms {
            let term = &y_power / BigInt::from(n);
            if n % 2 == 1 {
                result += &term;
            } else {
                result -= &term;
            }
            // Check convergence: if the term is zero at our precision, stop
            if term.is_zero() {
                break;
            }
            // y_power = y_power * y / scale (next power of y in fixed-point)
            y_power = fp_mul(&y_power, &y, scale);
        }

        result
    }

    /// Result of comparing a value against exp(x) using Taylor series.
    /// Matches Haskell's `CompareResult`.
    #[derive(Debug)]
    enum CompareResult {
        Above,      // cmp > exp(x)
        Below,      // cmp < exp(x)
        MaxReached, // couldn't determine within max iterations
    }

    /// Compare `cmp` against `exp(x)` using Taylor series expansion.
    /// Matches Haskell's `taylorExpCmp` from NonIntegral.hs.
    ///
    /// All values are in fixed-point (scaled by `scale`).
    /// `bound_x` is an upper bound on x (Haskell uses 3).
    fn taylor_exp_cmp(bound_x: &BigInt, cmp: &BigInt, x: &BigInt, scale: &BigInt) -> CompareResult {
        let max_n = 1000;
        let mut n = 0;
        let mut err = x.clone(); // current term = x (first-order)
        let mut acc = scale.clone(); // partial sum starts at 1.0 (= scale)
        let mut divisor = BigInt::one(); // factorial denominator

        loop {
            if n >= max_n {
                return CompareResult::MaxReached;
            }

            // acc' = acc + err (add current term)
            let acc_prime = &acc + &err;

            // Compute next term: err' = err * x / (divisor + 1)
            divisor += BigInt::one();
            let err_prime = fp_mul(&err, x, scale) / &divisor;

            // Error bound on remaining series: |err' * bound_x|
            let error_term = fp_mul(&err_prime, bound_x, scale).abs();

            // Compare
            if cmp >= &(&acc_prime + &error_term) {
                return CompareResult::Above;
            }
            if cmp < &(&acc_prime - &error_term) {
                return CompareResult::Below;
            }

            acc = acc_prime;
            err = err_prime;
            n += 1;
        }
    }

    /// Exact VRF leader eligibility check matching Haskell's `checkLeaderNatValue`.
    pub fn check_leader_value_exact(
        vrf_output: &[u8],
        relative_stake: f64,
        active_slot_coeff: f64,
    ) -> bool {
        if relative_stake <= 0.0 {
            return false;
        }
        if active_slot_coeff >= 1.0 {
            return true;
        }

        let scale = fp_scale();
        let cert_nat_max = cert_nat_max();

        // certNat = big-endian interpretation of the 32-byte VRF leader value
        let cert_nat = if vrf_output.len() >= 32 {
            bytes_to_natural(&vrf_output[..32])
        } else {
            bytes_to_natural(vrf_output)
        };

        // q = certNatMax - certNat
        let q = &cert_nat_max - &cert_nat;
        if q <= BigInt::zero() {
            return false; // certNat >= certNatMax, should not happen
        }

        // recip_q = certNatMax / q  (in fixed-point = certNatMax * scale / q)
        let recip_q = fp_div(&cert_nat_max, &q, &scale);

        // c = |ln(1 - f)| (positive, since ln(1-f) < 0 for f in (0,1))
        // Compute ln(1-f) in fixed-point
        let one_minus_f_fp = float_to_fixed(1.0 - active_slot_coeff, &scale);
        let ln_one_minus_f = fp_ln(&one_minus_f_fp, &scale); // negative
        let c = -&ln_one_minus_f; // positive

        // sigma in fixed-point
        let sigma_fp = float_to_fixed(relative_stake, &scale);

        // x = sigma * c (in fixed-point)
        let x = fp_mul(&sigma_fp, &c, &scale);

        // bound_x = 3 (in fixed-point)
        let bound_x = &scale * BigInt::from(3);

        // Check: recip_q < exp(x)?
        // taylorExpCmp returns BELOW if cmp < exp(x) → leader
        //                      ABOVE if cmp > exp(x) → not leader
        match taylor_exp_cmp(&bound_x, &recip_q, &x, &scale) {
            CompareResult::Below => true,       // recip_q < exp(x) → IS leader
            CompareResult::Above => false,      // recip_q >= exp(x) → NOT leader
            CompareResult::MaxReached => false, // conservative: not leader
        }
    }

    /// Convert an f64 value to fixed-point BigInt with 10^34 scale.
    /// Handles values like relative_stake (0.001734...) and active_slot_coeff (0.05).
    fn float_to_fixed(value: f64, scale: &BigInt) -> BigInt {
        // Multiply by 10^34 using string-based approach to maximize precision.
        // For values < 1, this preserves all significant digits.
        // Use rational conversion: value = numerator/denominator
        // We represent value as a fraction of u64 values for precision.

        if value <= 0.0 {
            return BigInt::zero();
        }
        if value >= 1.0 {
            // For values >= 1 (like 1.0 - f), use direct multiplication
            let int_part = value as u64;
            let frac = value - int_part as f64;
            let int_fp = scale * BigInt::from(int_part);
            let frac_fp = float_to_fixed(frac, scale);
            return int_fp + frac_fp;
        }

        // For 0 < value < 1, use the mantissa/exponent decomposition
        // value = mantissa * 2^exponent where mantissa in [0.5, 1.0)
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7FF) as i64 - 1023;
        let mantissa_bits = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000; // add implicit 1

        // value = mantissa_bits / 2^52 * 2^exponent = mantissa_bits * 2^(exponent - 52)
        // In fixed-point: value * scale = mantissa_bits * scale * 2^(exponent - 52) / 1
        //                              = mantissa_bits * scale >> (52 - exponent)
        let shift = 52 - exponent;
        if shift >= 0 {
            (BigInt::from(mantissa_bits) * scale) >> shift as u64
        } else {
            (BigInt::from(mantissa_bits) * scale) << (-shift) as u64
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_fp_ln_one() {
            let scale = fp_scale();
            // ln(1) = 0
            let result = fp_ln(&scale, &scale);
            assert!(result.is_zero(), "ln(1) should be 0, got {}", result);
        }

        #[test]
        fn test_fp_ln_095() {
            let scale = fp_scale();
            // ln(0.95) ≈ -0.05129329438755058
            let x = float_to_fixed(0.95, &scale);
            let result = fp_ln(&x, &scale);
            // Convert back to f64 for comparison
            let result_f64 = bigint_to_f64(&result, &scale);
            let expected = (0.95f64).ln();
            assert!(
                (result_f64 - expected).abs() < 1e-15,
                "ln(0.95) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_float_to_fixed_roundtrip() {
            let scale = fp_scale();
            for &val in &[0.05, 0.001734, 0.5, 0.999, 0.0001] {
                let fp = float_to_fixed(val, &scale);
                let back = bigint_to_f64(&fp, &scale);
                assert!(
                    (back - val).abs() / val < 1e-14,
                    "Roundtrip failed for {}: got {}",
                    val,
                    back
                );
            }
        }

        #[test]
        fn test_exact_check_full_stake() {
            // With 100% stake, threshold = 1 - (1-0.05)^1 = 0.05
            // VRF output of all zeros → certNat = 0, p = 0, so 0 < 0.05 → leader
            assert!(check_leader_value_exact(&[0u8; 32], 1.0, 0.05));
        }

        #[test]
        fn test_exact_check_zero_stake() {
            assert!(!check_leader_value_exact(&[0u8; 32], 0.0, 0.05));
        }

        #[test]
        fn test_exact_check_high_output() {
            // VRF output of all 0xFF → certNat very close to 2^256, p ≈ 1
            // With any reasonable stake, 1 > threshold → not leader
            assert!(!check_leader_value_exact(&[0xFFu8; 32], 0.5, 0.05));
        }

        #[test]
        fn test_exact_matches_f64_common_cases() {
            // For common cases (well away from threshold), exact and f64 should agree
            let test_cases = vec![
                ([0u8; 32], 1.0, 0.05, true), // zero output, full stake → p=0 < threshold
                ([0x80u8; 32], 0.5, 0.05, false), // mid output (p≈0.5), half stake → p > threshold
                ([0x01u8; 32], 0.01, 0.05, false), // p≈0.004, stake=0.01 → threshold≈0.0005, p > threshold
                ([0xFFu8; 32], 1.0, 0.05, false),  // max output → p≈1, always above threshold
                ([0u8; 32], 0.5, 0.05, true),      // zero output, half stake → p=0 < any threshold
            ];

            for (output, stake, f, expected) in test_cases {
                let result = check_leader_value_exact(&output, stake, f);
                assert_eq!(
                    result, expected,
                    "Failed for stake={}, output[0]={:02x}",
                    stake, output[0]
                );
            }
        }

        fn bigint_to_f64(val: &BigInt, _scale: &BigInt) -> f64 {
            // Convert fixed-point BigInt to f64
            let (sign, digits) = val.to_bytes_be();
            let abs_val: f64 = digits.iter().fold(0.0f64, |acc, &b| acc * 256.0 + b as f64);
            let scale_f64: f64 = 1e34;
            let result = abs_val / scale_f64;
            if sign == num_bigint::Sign::Minus {
                -result
            } else {
                result
            }
        }
    }
}

/// A VRF key pair for proof generation
pub struct VrfKeyPair {
    pub secret_key: [u8; 32],
    pub public_key: [u8; 32],
}

/// Generate a VRF key pair from an existing 32-byte secret key.
pub fn generate_vrf_keypair_from_secret(secret: &[u8; 32]) -> VrfKeyPair {
    let sk = SecretKey03::from_bytes(secret);
    let (scalar, _) = sk.extend();
    let point = scalar * ED25519_BASEPOINT_POINT;
    let pk_bytes = point.compress().to_bytes();

    VrfKeyPair {
        secret_key: *secret,
        public_key: pk_bytes,
    }
}

/// Generate a new VRF key pair using a cryptographically secure RNG.
pub fn generate_vrf_keypair() -> VrfKeyPair {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    let sk = SecretKey03::from_bytes(&seed);
    let secret_bytes = sk.to_bytes();

    // Derive public key: extend secret to get scalar, then scalar * basepoint
    let (scalar, _) = sk.extend();
    let point = scalar * ED25519_BASEPOINT_POINT;
    let pk_bytes = point.compress().to_bytes();

    VrfKeyPair {
        secret_key: secret_bytes,
        public_key: pk_bytes,
    }
}

/// Generate a VRF proof for the given seed using a secret key.
///
/// Returns the 80-byte proof and 64-byte output.
pub fn generate_vrf_proof(
    secret_key: &[u8; 32],
    seed: &[u8],
) -> Result<([u8; 80], [u8; 64]), VrfError> {
    let sk = SecretKey03::from_bytes(secret_key);

    // Derive the public key from the secret key
    let (scalar, _) = sk.extend();
    let point = scalar * ED25519_BASEPOINT_POINT;
    let pk = PublicKey03::from_bytes(&point.compress().to_bytes());

    let proof = VrfProof03::generate(&pk, &sk, seed);
    let proof_bytes = proof.to_bytes();
    let output = proof.proof_to_hash();

    Ok((proof_bytes, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leader_check() {
        // A pool with 100% stake and f=0.05 should almost always be elected
        assert!(check_leader_value(&[0u8; 32], 1.0, 0.05));

        // A pool with 0% stake should never be elected
        assert!(!check_leader_value(&[128u8; 32], 0.0, 0.05));
    }

    #[test]
    fn test_vrf_verify_known_vector() {
        // Test vector from IOG's VRF implementation (draft-03)
        // Secret key: 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
        // Public key: d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
        // Proof: b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7
        //        ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156
        //        0f55edc256a787afe701677c0f602900
        // Output: 5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60
        //         3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc
        // Alpha (input): empty

        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        let proof = hex::decode(
            "b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7\
             ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156\
             0f55edc256a787afe701677c0f602900",
        )
        .unwrap();
        let expected_output = hex::decode(
            "5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60\
             3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc",
        )
        .unwrap();

        let result = verify_vrf_proof(&pk, &proof, &[]).unwrap();
        assert_eq!(&result[..], &expected_output[..]);
    }

    #[test]
    fn test_vrf_verify_with_alpha() {
        // Test vector with alpha_string = 0x72
        let pk = hex::decode("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
            .unwrap();
        let proof = hex::decode(
            "ae5b66bdf04b4c010bfe32b2fc126ead2107b697634f6f7337b9bff8785ee111\
             200095ece87dde4dbe87343f6df3b107d91798c8a7eb1245d3bb9c5aafb09335\
             8c13e6ae1111a55717e895fd15f99f07",
        )
        .unwrap();
        let expected_output = hex::decode(
            "94f4487e1b2fec954309ef1289ecb2e15043a2461ecc7b2ae7d4470607ef82eb\
             1cfa97d84991fe4a7bfdfd715606bc27e2967a6c557cfb5875879b671740b7d8",
        )
        .unwrap();

        let result = verify_vrf_proof(&pk, &proof, &[0x72]).unwrap();
        assert_eq!(&result[..], &expected_output[..]);
    }

    #[test]
    fn test_vrf_verify_invalid_proof() {
        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        // Corrupted proof
        let proof = vec![0u8; 80];
        let result = verify_vrf_proof(&pk, &proof, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_vrf_proof_to_hash() {
        let proof = hex::decode(
            "b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7\
             ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156\
             0f55edc256a787afe701677c0f602900",
        )
        .unwrap();
        let expected = hex::decode(
            "5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60\
             3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc",
        )
        .unwrap();

        let output = vrf_proof_to_hash(&proof).unwrap();
        assert_eq!(&output[..], &expected[..]);
    }

    #[test]
    fn test_vrf_keygen_and_sign() {
        let kp = generate_vrf_keypair();
        assert_eq!(kp.secret_key.len(), 32);
        assert_eq!(kp.public_key.len(), 32);

        // Generate a proof and verify it
        let seed = b"test_seed_data_for_vrf";
        let (proof, output) = generate_vrf_proof(&kp.secret_key, seed).unwrap();
        assert_eq!(proof.len(), 80);
        assert_eq!(output.len(), 64);

        // Verify the proof with the public key
        let verified_output = verify_vrf_proof(&kp.public_key, &proof, seed).unwrap();
        assert_eq!(verified_output, output);
    }

    #[test]
    fn test_vrf_keygen_unique() {
        let kp1 = generate_vrf_keypair();
        let kp2 = generate_vrf_keypair();
        assert_ne!(kp1.secret_key, kp2.secret_key);
        assert_ne!(kp1.public_key, kp2.public_key);
    }

    #[test]
    fn test_vrf_sign_leader_check() {
        let kp = generate_vrf_keypair();
        // Generate proofs for many slots — with 100% stake and f=0.05,
        // a pool is elected ~5% of slots, so check at least some pass
        let mut elected = 0;
        for slot in 0..200u64 {
            let mut seed = vec![0u8; 32]; // epoch nonce
            seed.extend_from_slice(&slot.to_be_bytes());
            let (_, output) = generate_vrf_proof(&kp.secret_key, &seed).unwrap();
            if check_leader_value(&output, 1.0, 0.05) {
                elected += 1;
            }
        }
        // With f=0.05 and 100% stake, expect ~10 out of 200 slots (5%)
        assert!(elected > 0, "Should win at least some slots");
        assert!(elected < 100, "Should not win most slots with f=0.05");
    }

    #[test]
    fn test_vrf_wrong_key_size() {
        assert!(verify_vrf_proof(&[0u8; 16], &[0u8; 80], &[]).is_err());
    }

    #[test]
    fn test_vrf_wrong_proof_size() {
        assert!(verify_vrf_proof(&[0u8; 32], &[0u8; 40], &[]).is_err());
    }
}
