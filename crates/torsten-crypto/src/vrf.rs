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
/// with continued fraction ln() and Taylor series exp() comparison for exact precision.
pub fn check_leader_value(vrf_output: &[u8], relative_stake: f64, active_slot_coeff: f64) -> bool {
    leader_check::check_leader_value_exact(vrf_output, relative_stake, active_slot_coeff)
}

/// Check leader value with exact rational active_slot_coeff (e.g., 1/20 for 0.05).
/// This avoids f64 precision loss when converting the protocol parameter.
pub fn check_leader_value_rational(
    vrf_output: &[u8],
    relative_stake: f64,
    active_slot_coeff_num: u64,
    active_slot_coeff_den: u64,
) -> bool {
    leader_check::check_leader_value_with_rational_coeff(
        vrf_output,
        relative_stake,
        active_slot_coeff_num,
        active_slot_coeff_den,
    )
}

/// Exact-precision VRF leader check matching Haskell's `checkLeaderNatValue`.
///
/// Uses `num-bigint` for 34-digit fixed-point arithmetic, replicating:
/// - `Cardano.Protocol.TPraos.BHeader.checkLeaderNatValue`
/// - `Cardano.Ledger.NonIntegral.taylorExpCmp` / `ln'` / `lncf`
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

    /// Compute ln(1+x) using Euler's generalized continued fraction.
    ///
    /// Matches Haskell's `lncf` from `Cardano.Ledger.NonIntegral`:
    ///
    /// The continued fraction for ln(1+x) has coefficients:
    ///   a_n: [x, 1²x, 1²x, 2²x, 2²x, 3²x, 3²x, ...]
    ///   b_n: [1, 2, 3, 4, 5, 6, 7, ...]
    ///
    /// Converges for all x >= 0 (unlike the Taylor series which requires |x| < 1).
    /// Uses the Wallis recurrence for evaluating the convergents.
    /// Convergence criterion: |x_n - x_{n-1}| < 10^{-24} (matching Haskell's epsilon).
    fn fp_lncf(x_fp: &BigInt, scale: &BigInt) -> BigInt {
        if x_fp.is_zero() {
            return BigInt::zero();
        }

        // Epsilon: 10^{-24} in fixed-point = 10^{34-24} = 10^{10}
        let epsilon = BigInt::from(10).pow(10);
        let max_n = 1000usize;

        // Initial Wallis recurrence state (all in fixed-point):
        // A_{-1} = 1.0, B_{-1} = 0, A_0 = 0 (b_0=0 implicit), B_0 = 1.0
        let mut a_nm2 = scale.clone(); // A_{-1}
        let mut b_nm2 = BigInt::zero(); // B_{-1}
        let mut a_nm1 = BigInt::zero(); // A_0
        let mut b_nm1 = scale.clone(); // B_0

        let mut last_xn: Option<BigInt> = None;

        for n in 0..max_n {
            // a_n coefficient (fixed-point): coeff(n) * x
            // Pattern: x, 1²x, 1²x, 2²x, 2²x, 3²x, 3²x, ...
            let coeff: u64 = if n == 0 {
                1
            } else {
                let k = (n as u64).div_ceil(2);
                k * k
            };
            let an_fp = BigInt::from(coeff) * x_fp;

            // b_n coefficient (fixed-point): (n+1) represented as fixed-point
            let bn_fp = BigInt::from(n as u64 + 1) * scale;

            // Wallis recurrence (fixed-point arithmetic):
            // A_n = b_n * A_{n-1} + a_n * A_{n-2}
            let a_n = fp_mul(&bn_fp, &a_nm1, scale) + fp_mul(&an_fp, &a_nm2, scale);
            let b_n = fp_mul(&bn_fp, &b_nm1, scale) + fp_mul(&an_fp, &b_nm2, scale);

            // Convergent: x_n = A_n / B_n (in fixed-point)
            if !b_n.is_zero() {
                let xn = fp_div(&a_n, &b_n, scale);

                if let Some(ref prev) = last_xn {
                    if (&xn - prev).abs() < epsilon {
                        return xn;
                    }
                }
                last_xn = Some(xn);
            }

            // Shift state
            a_nm2 = a_nm1;
            b_nm2 = b_nm1;
            a_nm1 = a_n;
            b_nm1 = b_n;
        }

        last_xn.unwrap_or_default()
    }

    /// Compute ln(x) for positive x using argument reduction + continued fraction.
    ///
    /// Matches Haskell's `ln'` → `splitLn` → `findE` → `lncf` pipeline:
    /// - For x < 1: ln(x) = -ln(1/x)
    /// - For x >= 1: find n where e^n <= x < e^(n+1), then ln(x) = n + lncf(x/e^n - 1)
    ///
    /// `x_fp` is x in fixed-point. Returns ln(x) in fixed-point.
    fn fp_ln(x_fp: &BigInt, scale: &BigInt) -> BigInt {
        if x_fp <= &BigInt::zero() {
            return BigInt::zero();
        }
        if x_fp == scale {
            return BigInt::zero(); // ln(1) = 0
        }

        // For x < 1: ln(x) = -ln(1/x) (matches Haskell splitLn)
        if x_fp < scale {
            let recip = fp_div(scale, x_fp, scale); // 1/x in fixed-point
            return -fp_ln(&recip, scale);
        }

        // x >= 1: find n such that e^n <= x < e^(n+1)
        let e_fp = fp_exp_taylor(scale, scale); // e ≈ 2.718...

        let mut e_power = scale.clone(); // e^0 = 1
        let mut n: u64 = 0;

        loop {
            let next = fp_mul(&e_power, &e_fp, scale);
            if &next > x_fp {
                break;
            }
            e_power = next;
            n += 1;
            if n > 1000 {
                break;
            }
        }

        // r = x / e^n - 1 (value in [0, e-1) suitable for continued fraction)
        let r = fp_div(x_fp, &e_power, scale) - scale;

        // ln(x) = n + ln(1 + r)
        let ln_1_plus_r = fp_lncf(&r, scale);
        scale * BigInt::from(n) + ln_1_plus_r
    }

    /// Compute exp(x) using Taylor series (for computing e = exp(1)).
    /// Only used internally for argument reduction in ln.
    fn fp_exp_taylor(x: &BigInt, scale: &BigInt) -> BigInt {
        let mut result = scale.clone(); // 1.0
        let mut term = x.clone(); // x^1/1!
        for n in 1..100 {
            result += &term;
            term = fp_mul(&term, x, scale) / BigInt::from(n + 1);
            if term.is_zero() {
                break;
            }
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

    /// Exact VRF leader eligibility check with f64 inputs.
    /// Converts active_slot_coeff to nearest exact rational before computing.
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

        // Convert active_slot_coeff to nearest rational with denominator up to 10000.
        // Common values: 0.05 = 1/20, 0.1 = 1/10, etc.
        let (f_num, f_den) = f64_to_rational(active_slot_coeff);
        check_leader_value_with_rational_coeff(vrf_output, relative_stake, f_num, f_den)
    }

    /// Exact VRF leader eligibility check with rational active_slot_coeff.
    pub fn check_leader_value_with_rational_coeff(
        vrf_output: &[u8],
        relative_stake: f64,
        active_slot_coeff_num: u64,
        active_slot_coeff_den: u64,
    ) -> bool {
        if relative_stake <= 0.0 {
            return false;
        }
        if active_slot_coeff_den == 0 || active_slot_coeff_num >= active_slot_coeff_den {
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
            return false;
        }

        // recip_q = certNatMax / q  (in fixed-point)
        let recip_q = fp_div(&cert_nat_max, &q, &scale);

        // Compute (1-f) in exact fixed-point from rational:
        // 1 - f_num/f_den = (f_den - f_num) / f_den
        // In fixed-point: (f_den - f_num) * scale / f_den
        let one_minus_f_fp = BigInt::from(active_slot_coeff_den - active_slot_coeff_num) * &scale
            / BigInt::from(active_slot_coeff_den);

        // c = |ln(1 - f)| (positive, since ln(1-f) < 0 for f in (0,1))
        let ln_one_minus_f = fp_ln(&one_minus_f_fp, &scale); // negative
        let c = -&ln_one_minus_f; // positive

        // sigma in fixed-point
        let sigma_fp = float_to_fixed(relative_stake, &scale);

        // x = sigma * c (in fixed-point)
        let x = fp_mul(&sigma_fp, &c, &scale);

        // bound_x = 3 (in fixed-point)
        let bound_x = &scale * BigInt::from(3);

        // Check: recip_q < exp(x)?
        match taylor_exp_cmp(&bound_x, &recip_q, &x, &scale) {
            CompareResult::Below => true,       // recip_q < exp(x) → IS leader
            CompareResult::Above => false,      // recip_q >= exp(x) → NOT leader
            CompareResult::MaxReached => false, // conservative: not leader
        }
    }

    /// Convert an f64 to the nearest rational p/q with q <= 10000.
    /// Handles common Cardano values like 0.05 = 1/20.
    fn f64_to_rational(value: f64) -> (u64, u64) {
        // Try common denominators first (exact matches)
        for den in [1, 2, 4, 5, 10, 20, 25, 50, 100, 200, 1000, 10000] {
            let num = (value * den as f64).round() as u64;
            let reconstructed = num as f64 / den as f64;
            if (reconstructed - value).abs() < 1e-15 {
                // Simplify with GCD
                let g = gcd(num, den);
                return (num / g, den / g);
            }
        }
        // Fallback: use large denominator
        let den = 1_000_000u64;
        let num = (value * den as f64).round() as u64;
        let g = gcd(num, den);
        (num / g, den / g)
    }

    /// Greatest common divisor
    fn gcd(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a
    }

    /// Convert an f64 value to fixed-point BigInt with 10^34 scale.
    fn float_to_fixed(value: f64, scale: &BigInt) -> BigInt {
        if value <= 0.0 {
            return BigInt::zero();
        }
        if value >= 1.0 {
            let int_part = value as u64;
            let frac = value - int_part as f64;
            let int_fp = scale * BigInt::from(int_part);
            let frac_fp = float_to_fixed(frac, scale);
            return int_fp + frac_fp;
        }

        // Use mantissa/exponent decomposition for maximum f64 precision
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7FF) as i64 - 1023;
        let mantissa_bits = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;

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
            let result = fp_ln(&scale, &scale);
            assert!(result.is_zero(), "ln(1) should be 0, got {}", result);
        }

        #[test]
        fn test_fp_ln_095_exact() {
            let scale = fp_scale();
            // Compute ln(0.95) using exact rational: 0.95 = 19/20
            let x = BigInt::from(19u64) * &scale / BigInt::from(20u64);
            let result = fp_ln(&x, &scale);
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
        fn test_fp_ln_2() {
            let scale = fp_scale();
            // ln(2) via argument reduction: n=0, r = 2/1 - 1 = 1.0
            // This exercises the continued fraction with x=1 (where Taylor converges slowly)
            let x = &scale * 2; // 2.0 in fixed-point
            let result = fp_ln(&x, &scale);
            let result_f64 = bigint_to_f64(&result, &scale);
            let expected = 2.0f64.ln();
            assert!(
                (result_f64 - expected).abs() < 1e-10,
                "ln(2) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_fp_ln_half() {
            let scale = fp_scale();
            // ln(0.5) = -ln(2) — tests the x<1 branch
            let x = &scale / 2; // 0.5 in fixed-point
            let result = fp_ln(&x, &scale);
            let result_f64 = bigint_to_f64(&result, &scale);
            let expected = 0.5f64.ln();
            assert!(
                (result_f64 - expected).abs() < 1e-10,
                "ln(0.5) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_f64_to_rational() {
            assert_eq!(f64_to_rational(0.05), (1, 20));
            assert_eq!(f64_to_rational(0.1), (1, 10));
            assert_eq!(f64_to_rational(0.5), (1, 2));
            assert_eq!(f64_to_rational(1.0), (1, 1));
        }

        #[test]
        fn test_exact_check_full_stake() {
            assert!(check_leader_value_exact(&[0u8; 32], 1.0, 0.05));
        }

        #[test]
        fn test_exact_check_zero_stake() {
            assert!(!check_leader_value_exact(&[0u8; 32], 0.0, 0.05));
        }

        #[test]
        fn test_exact_check_high_output() {
            assert!(!check_leader_value_exact(&[0xFFu8; 32], 0.5, 0.05));
        }

        #[test]
        fn test_exact_matches_common_cases() {
            let test_cases = vec![
                ([0u8; 32], 1.0, 0.05, true),
                ([0x80u8; 32], 0.5, 0.05, false),
                ([0x01u8; 32], 0.01, 0.05, false),
                ([0xFFu8; 32], 1.0, 0.05, false),
                ([0u8; 32], 0.5, 0.05, true),
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

        #[test]
        fn test_rational_coeff_matches() {
            // check_leader_value_exact(f64) and check_leader_value_with_rational_coeff
            // should give the same result for exact rationals
            let output = [0u8; 32];
            assert_eq!(
                check_leader_value_exact(&output, 1.0, 0.05),
                check_leader_value_with_rational_coeff(&output, 1.0, 1, 20),
            );
            let output2 = [0x80u8; 32];
            assert_eq!(
                check_leader_value_exact(&output2, 0.5, 0.05),
                check_leader_value_with_rational_coeff(&output2, 0.5, 1, 20),
            );
        }

        fn bigint_to_f64(val: &BigInt, _scale: &BigInt) -> f64 {
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
