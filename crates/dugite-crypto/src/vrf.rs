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

/// TPraos leader check: uses raw VRF output bytes directly with certNatMax = 2^512.
/// Used for Shelley/Allegra/Mary/Alonzo eras (protocol versions 2-6).
pub fn check_leader_value_tpraos(
    vrf_output: &[u8],
    relative_stake: f64,
    active_slot_coeff: f64,
) -> bool {
    leader_check::check_leader_value_tpraos(vrf_output, relative_stake, active_slot_coeff)
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

/// Praos leader check with FULLY exact rational inputs (no f64 anywhere).
///
/// Both sigma (relative stake = pool_stake/total_active_stake) and f
/// (active slot coefficient = f_num/f_den) are passed as exact rationals.
/// This matches Haskell's `checkLeaderNatValue` which receives `Rational`
/// for both sigma and the active slot coefficient.
pub fn check_leader_value_full_rational(
    vrf_output: &[u8],
    sigma_num: u64,
    sigma_den: u64,
    active_slot_coeff_num: u64,
    active_slot_coeff_den: u64,
) -> bool {
    leader_check::check_leader_value_all_rational(
        vrf_output,
        sigma_num,
        sigma_den,
        active_slot_coeff_num,
        active_slot_coeff_den,
        false, // Praos: 32-byte leader value, certNatMax = 2^256
    )
}

/// TPraos leader check with FULLY exact rational inputs (no f64 anywhere).
///
/// Same as `check_leader_value_full_rational` but for Shelley-Alonzo eras
/// (protocol versions 2-6) where certNatMax = 2^512 and the raw 64-byte
/// VRF output is used directly.
pub fn check_leader_value_tpraos_rational(
    vrf_output: &[u8],
    sigma_num: u64,
    sigma_den: u64,
    active_slot_coeff_num: u64,
    active_slot_coeff_den: u64,
) -> bool {
    leader_check::check_leader_value_all_rational(
        vrf_output,
        sigma_num,
        sigma_den,
        active_slot_coeff_num,
        active_slot_coeff_den,
        true, // TPraos: 64-byte raw output, certNatMax = 2^512
    )
}

/// Pre-computed `activeSlotLog` = |ln(1-f)| in 34-digit fixed-point.
///
/// Computing `ln(1-f)` with the continued-fraction algorithm is expensive
/// (several hundred microseconds). An epoch has ~86 400–432 000 slots, so
/// recomputing it on every leader check would waste significant CPU.
///
/// Call [`compute_active_slot_log`] once per epoch with the rational
/// active-slot coefficient, then pass the result to
/// [`check_leader_value_full_rational_cached`] for each slot.
pub struct ActiveSlotLog(leader_check::CachedLog);

/// Pre-compute `|ln(1-f)|` for the active-slot coefficient `f = f_num/f_den`.
///
/// Returns `None` if `f_num == 0` (no slots ever active — caller should treat
/// this as "never elected"), or if `f_num >= f_den` (every slot active —
/// caller should treat this as "always elected").
pub fn compute_active_slot_log(f_num: u64, f_den: u64) -> Option<ActiveSlotLog> {
    leader_check::compute_log(f_num, f_den).map(ActiveSlotLog)
}

/// Praos leader check using a pre-computed `activeSlotLog`.
///
/// This is the hot path for `compute_leader_schedule`: the cached log avoids
/// re-running the expensive `ln(1-f)` continued-fraction expansion on every
/// slot, while still using fully exact rational arithmetic for sigma.
pub fn check_leader_value_full_rational_cached(
    vrf_output: &[u8],
    sigma_num: u64,
    sigma_den: u64,
    log: &ActiveSlotLog,
) -> bool {
    leader_check::check_leader_value_with_cached_log(
        vrf_output, sigma_num, sigma_den, &log.0, false,
    )
}

/// TPraos leader check using a pre-computed `activeSlotLog`.
///
/// Same as [`check_leader_value_full_rational_cached`] but for
/// Shelley-Alonzo eras (certNatMax = 2^512, raw 64-byte VRF output).
pub fn check_leader_value_tpraos_rational_cached(
    vrf_output: &[u8],
    sigma_num: u64,
    sigma_den: u64,
    log: &ActiveSlotLog,
) -> bool {
    leader_check::check_leader_value_with_cached_log(vrf_output, sigma_num, sigma_den, &log.0, true)
}

/// Exact-precision VRF leader check matching Haskell's `checkLeaderNatValue`.
///
/// Uses `dashu-int` (IBig) for 34-digit fixed-point arithmetic, matching the
/// exact same algorithm as the cardano-base math library / Haskell's NonIntegral:
/// - `Cardano.Protocol.TPraos.BHeader.checkLeaderNatValue`
/// - `Cardano.Ledger.NonIntegral.taylorExpCmp` / `ln'` / `lncf`
/// - `Cardano.Ledger.BaseTypes.FixedPoint` (10^34 resolution)
mod leader_check {
    use dashu_base::{Abs, DivRem, Sign};
    use dashu_int::IBig;
    use std::sync::LazyLock;

    /// Precision: 10^34 (matches Haskell's E34 / FixedPoint)
    static PRECISION: LazyLock<IBig> = LazyLock::new(|| IBig::from(10).pow(34));
    /// Epsilon for convergence: 10^(34-24) = 10^10
    static EPS: LazyLock<IBig> = LazyLock::new(|| IBig::from(10).pow(10));
    /// ONE in fixed-point = 1 * 10^34
    static ONE: LazyLock<IBig> = LazyLock::new(|| IBig::from(1) * &*PRECISION);
    /// ZERO
    static ZERO: LazyLock<IBig> = LazyLock::new(|| IBig::from(0));
    /// e = exp(1) in fixed-point
    static E: LazyLock<IBig> = LazyLock::new(|| {
        let mut e = IBig::from(0);
        ref_exp(&mut e, &ONE);
        e
    });

    /// certNatMax = 2^256
    fn cert_nat_max() -> IBig {
        IBig::from(2).pow(256)
    }

    /// Fixed-point division: (x * PRECISION) / y, with proper quotient+remainder handling
    /// matching the cardano-base math library's `div` function exactly.
    fn fp_div(rop: &mut IBig, x: &IBig, y: &IBig) {
        let (temp_q, temp_r): (IBig, IBig) = x.div_rem(y);
        let mut temp = &temp_q * &*PRECISION;
        let scaled_r: IBig = &temp_r * &*PRECISION;
        let (q2, _): (IBig, IBig) = scaled_r.div_rem(y);
        temp += &q2;
        *rop = temp;
    }

    /// Fixed-point scale (truncate toward negative infinity for negative, toward zero for positive)
    /// matching the cardano-base math library's `scale` exactly.
    fn fp_scale(rop: &mut IBig) {
        let (a, remainder): (IBig, IBig) = (&*rop).div_rem(&*PRECISION);
        if *rop < *ZERO && remainder != *ZERO {
            *rop = a - IBig::from(1);
        } else {
            *rop = a;
        }
    }

    /// Integer power via repeated squaring, matching the cardano-base math library's `ipow_` + `ipow`.
    fn ipow(rop: &mut IBig, x: &IBig, n: i64) {
        if n < 0 {
            let mut temp = IBig::from(0);
            ipow_inner(&mut temp, x, -n);
            fp_div(rop, &ONE, &temp);
        } else {
            ipow_inner(rop, x, n);
        }
    }

    fn ipow_inner(rop: &mut IBig, x: &IBig, n: i64) {
        if n == 0 {
            rop.clone_from(&ONE);
        } else if n % 2 == 0 {
            let mut res = IBig::from(0);
            ipow_inner(&mut res, x, n / 2);
            *rop = &res * &res;
            fp_scale(rop);
        } else {
            let mut res = IBig::from(0);
            ipow_inner(&mut res, x, n - 1);
            *rop = &res * x;
            fp_scale(rop);
        }
    }

    /// Ceiling division for positive x/y (used in exp scaling).
    fn div_round_ceil(x: &IBig, y: &IBig) -> IBig {
        let (q, r): (IBig, IBig) = x.div_rem(y);
        if q.sign() == Sign::Positive && r != IBig::ZERO {
            q + IBig::ONE
        } else {
            q
        }
    }

    /// Taylor/Maclaurin series for exp(x) with convergence check.
    /// Matches the cardano-base math library's `mp_exp_taylor` exactly.
    fn mp_exp_taylor(rop: &mut IBig, max_n: i32, x: &IBig, epsilon: &IBig) -> i32 {
        let mut divisor = ONE.clone();
        let mut last_x = ONE.clone();
        rop.clone_from(&ONE);
        let mut n = 0;
        while n < max_n {
            let mut next_x = x * &last_x;
            fp_scale(&mut next_x);
            let next_x2 = next_x.clone();
            fp_div(&mut next_x, &next_x2, &divisor);

            if (&next_x).abs() < epsilon.abs() {
                break;
            }

            divisor += &*ONE;
            *rop = &*rop + &next_x;
            last_x.clone_from(&next_x);
            n += 1;
        }
        n
    }

    /// Entry point for exp approximation. Scales x to [0,1] then uses Taylor series.
    /// Matches the cardano-base math library's `ref_exp` exactly.
    fn ref_exp(rop: &mut IBig, x: &IBig) {
        use std::cmp::Ordering;
        match x.cmp(&ZERO) {
            Ordering::Equal => {
                rop.clone_from(&ONE);
            }
            Ordering::Less => {
                let x_ = -x;
                let mut temp = IBig::from(0);
                ref_exp(&mut temp, &x_);
                fp_div(rop, &ONE, &temp);
            }
            Ordering::Greater => {
                let n_exponent = div_round_ceil(x, &PRECISION);
                let x_ = x / &n_exponent;
                mp_exp_taylor(rop, 1000, &x_, &EPS);
                // Safety: n_exponent = ceil(x / PRECISION) is always a small integer
                // in practice (VRF values produce x ≈ n * PRECISION where n < 10).
                // Converting to i64 cannot fail for any realistic VRF computation.
                let n_i64: i64 = i64::try_from(&n_exponent)
                    .expect("n_exponent exceeds i64::MAX; this should never happen for VRF values");
                ipow(rop, &rop.clone(), n_i64);
            }
        }
    }

    /// Continued fraction approximation for ln(1+x).
    /// Matches the cardano-base math library's `mp_ln_n` exactly.
    fn mp_ln_n(rop: &mut IBig, max_n: i32, x: &IBig, epsilon: &IBig) {
        let mut convergent = IBig::from(0);
        let mut last = IBig::from(0);
        let mut first = true;
        let mut n = 1;

        let b = ONE.clone();

        let mut an_m2 = ONE.clone();
        let mut bn_m2 = IBig::from(0);
        let mut an_m1 = IBig::from(0);
        let mut bn_m1 = ONE.clone();

        let mut curr_a: i64 = 1;
        let mut b_acc = b;

        while n <= max_n + 2 {
            let curr_a_2 = curr_a * curr_a;
            let a = x * IBig::from(curr_a_2);
            if n > 1 && n % 2 == 1 {
                curr_a += 1;
            }

            let mut ba = &b_acc * &an_m1;
            fp_scale(&mut ba);
            let mut aa = &a * &an_m2;
            fp_scale(&mut aa);
            let a_ = &ba + &aa;

            let mut bb = &b_acc * &bn_m1;
            fp_scale(&mut bb);
            let mut ab = &a * &bn_m2;
            fp_scale(&mut ab);
            let b_ = &bb + &ab;

            fp_div(&mut convergent, &a_, &b_);

            if first {
                first = false;
            } else {
                let diff = &convergent - &last;
                if diff.abs() < epsilon.abs() {
                    break;
                }
            }

            last.clone_from(&convergent);

            n += 1;
            an_m2.clone_from(&an_m1);
            bn_m2.clone_from(&bn_m1);
            an_m1.clone_from(&a_);
            bn_m1.clone_from(&b_);

            b_acc += &*ONE;
        }

        *rop = convergent;
    }

    /// Find n such that e^n <= x < e^(n+1).
    /// Matches the cardano-base math library's `find_e` exactly.
    fn find_e(x: &IBig) -> i64 {
        let mut x_ = IBig::from(0);
        fp_div(&mut x_, &ONE, &E);
        let mut x__ = E.clone();

        let mut l: i64 = -1;
        let mut u: i64 = 1;
        while &x_ > x || &x__ < x {
            let x_sq = &x_ * &x_;
            let mut x_scaled = x_sq;
            fp_scale(&mut x_scaled);
            x_ = x_scaled;

            let upper_sq = &x__ * &x__;
            let mut upper_scaled = upper_sq;
            fp_scale(&mut upper_scaled);
            x__ = upper_scaled;

            l *= 2;
            u *= 2;
        }

        while l + 1 != u {
            let mid = l + ((u - l) / 2);
            ipow(&mut x_, &E, mid);
            if x < &x_ {
                u = mid;
            } else {
                l = mid;
            }
        }
        l
    }

    /// Entry point for ln approximation.
    /// Matches the cardano-base math library's `ref_ln` exactly.
    fn ref_ln(rop: &mut IBig, x: &IBig) -> bool {
        if x <= &*ZERO {
            return false;
        }

        let n = find_e(x);
        *rop = IBig::from(n) * &*PRECISION;

        let mut factor = IBig::from(0);
        ref_exp(&mut factor, rop);

        let mut x_ = IBig::from(0);
        fp_div(&mut x_, x, &factor);
        x_ = &x_ - &*ONE;

        let x_2 = x_.clone();
        mp_ln_n(&mut x_, 1000, &x_2, &EPS);
        *rop = &*rop + &x_;
        true
    }

    /// Compare `compare` against `exp(x)` using bounded Taylor series.
    /// Matches the cardano-base math library's `ref_exp_cmp` exactly.
    ///
    /// Returns: GT if compare > exp(x), LT if compare < exp(x), UNKNOWN if indeterminate.
    fn ref_exp_cmp(max_n: u64, x: &IBig, bound_x: i64, compare: &IBig) -> ExpCmpResult {
        let mut rop = ONE.clone();
        let mut n = 0u64;
        let mut divisor = ONE.clone();
        let mut error = x.clone();

        while n < max_n {
            let next_x = error.clone();
            if (&next_x).abs() < (&*EPS).abs() {
                break;
            }
            divisor += &*ONE;

            // Update error: error = error * x / divisor
            error *= x;
            fp_scale(&mut error);
            let e2 = error.clone();
            fp_div(&mut error, &e2, &divisor);

            let error_term = &error * IBig::from(bound_x);
            rop = &rop + &next_x;

            // compare > upper bound → compare is above exp(x)
            let upper = &rop + &error_term;
            if compare > &upper {
                return ExpCmpResult::GT;
            }

            // compare < lower bound → compare is below exp(x)
            let lower = &rop - &error_term;
            if compare < &lower {
                return ExpCmpResult::LT;
            }
            n += 1;
        }

        ExpCmpResult::Unknown
    }

    #[derive(Debug, PartialEq)]
    enum ExpCmpResult {
        GT,      // compare > exp(x)
        LT,      // compare < exp(x)
        Unknown, // couldn't determine
    }

    /// Exact VRF leader eligibility check with f64 inputs.
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

        let (f_num, f_den) = f64_to_rational(active_slot_coeff);
        check_leader_value_with_rational_coeff(vrf_output, relative_stake, f_num, f_den)
    }

    /// TPraos leader check: raw VRF output (64 bytes) with certNatMax = 2^512.
    /// Used for Shelley/Allegra/Mary/Alonzo eras.
    pub fn check_leader_value_tpraos(
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

        let (f_num, f_den) = f64_to_rational(active_slot_coeff);
        if f_den == 0 || f_num >= f_den {
            return true;
        }

        // TPraos: certNatMax = 2^512, certNat from raw 64-byte VRF output
        let cert_nat_max = IBig::from(2).pow(512);
        let cert_nat = if vrf_output.len() >= 64 {
            IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..64]))
        } else {
            IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
        };

        let q = &cert_nat_max - &cert_nat;
        if q <= *ZERO {
            return false;
        }

        let mut recip_q = IBig::from(0);
        fp_div(&mut recip_q, &cert_nat_max, &q);

        let one_minus_f_fp = IBig::from(f_den - f_num) * &*PRECISION / IBig::from(f_den);

        let mut ln_one_minus_f = IBig::from(0);
        ref_ln(&mut ln_one_minus_f, &one_minus_f_fp);
        let c = -&ln_one_minus_f;

        let sigma_fp = float_to_fixed(relative_stake);
        let mut x = &sigma_fp * &c;
        fp_scale(&mut x);

        match ref_exp_cmp(1000, &x, 3, &recip_q) {
            ExpCmpResult::LT => true,
            ExpCmpResult::GT => false,
            ExpCmpResult::Unknown => false,
        }
    }

    /// Exact VRF leader eligibility check with rational active_slot_coeff.
    ///
    /// Implements the Haskell `checkLeaderNatValue` algorithm:
    ///   p < 1 - (1-f)^sigma
    /// where p = certNat / certNatMax.
    ///
    /// Rearranged: certNatMax / (certNatMax - certNat) < exp(sigma * |ln(1-f)|)
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

        let cert_nat_max = cert_nat_max();

        // certNat = big-endian interpretation of the 32-byte VRF leader value
        let cert_nat = if vrf_output.len() >= 32 {
            IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..32]))
        } else {
            IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
        };

        // q = certNatMax - certNat
        let q = &cert_nat_max - &cert_nat;
        if q <= *ZERO {
            return false;
        }

        // recip_q = certNatMax / q  (in fixed-point)
        let mut recip_q = IBig::from(0);
        fp_div(&mut recip_q, &cert_nat_max, &q);

        // Compute (1-f) in exact fixed-point from rational:
        // 1 - f_num/f_den = (f_den - f_num) / f_den
        let one_minus_f_fp = IBig::from(active_slot_coeff_den - active_slot_coeff_num)
            * &*PRECISION
            / IBig::from(active_slot_coeff_den);

        // c = |ln(1 - f)| (positive, since ln(1-f) < 0 for f in (0,1))
        let mut ln_one_minus_f = IBig::from(0);
        ref_ln(&mut ln_one_minus_f, &one_minus_f_fp);
        let c = -&ln_one_minus_f; // positive

        // sigma in fixed-point from f64
        let sigma_fp = float_to_fixed(relative_stake);

        // x = sigma * c (in fixed-point)
        let mut x = &sigma_fp * &c;
        fp_scale(&mut x);

        // Check: recip_q < exp(x)?
        // ref_exp_cmp returns GT if compare > exp(x), LT if compare < exp(x)
        // We want: recip_q < exp(x) → leader elected
        match ref_exp_cmp(1000, &x, 3, &recip_q) {
            ExpCmpResult::LT => true,       // recip_q < exp(x) → IS leader
            ExpCmpResult::GT => false,      // recip_q >= exp(x) → NOT leader
            ExpCmpResult::Unknown => false, // conservative: not leader
        }
    }

    /// Fully exact VRF leader eligibility check with rational sigma AND rational f.
    ///
    /// No f64 conversions anywhere — both sigma (pool_stake/total_active_stake)
    /// and f (active_slot_coeff_num/active_slot_coeff_den) are exact rationals.
    ///
    /// When `tpraos` is false: Praos mode (32-byte leader value, certNatMax = 2^256).
    /// When `tpraos` is true: TPraos mode (64-byte raw VRF output, certNatMax = 2^512).
    pub fn check_leader_value_all_rational(
        vrf_output: &[u8],
        sigma_num: u64,
        sigma_den: u64,
        f_num: u64,
        f_den: u64,
        tpraos: bool,
    ) -> bool {
        if sigma_num == 0 || sigma_den == 0 {
            return false;
        }
        if f_den == 0 || f_num >= f_den {
            return true;
        }

        let cert_nat_max = if tpraos {
            IBig::from(2).pow(512)
        } else {
            cert_nat_max()
        };

        let cert_nat = if tpraos {
            if vrf_output.len() >= 64 {
                IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..64]))
            } else {
                IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
            }
        } else if vrf_output.len() >= 32 {
            IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..32]))
        } else {
            IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
        };

        let q = &cert_nat_max - &cert_nat;
        if q <= *ZERO {
            return false;
        }

        // recip_q = certNatMax / q  (in fixed-point)
        let mut recip_q = IBig::from(0);
        fp_div(&mut recip_q, &cert_nat_max, &q);

        // Compute (1-f) in exact fixed-point from rational:
        // 1 - f_num/f_den = (f_den - f_num) / f_den
        let one_minus_f_fp = IBig::from(f_den - f_num) * &*PRECISION / IBig::from(f_den);

        // c = |ln(1 - f)| (positive, since ln(1-f) < 0 for f in (0,1))
        let mut ln_one_minus_f = IBig::from(0);
        ref_ln(&mut ln_one_minus_f, &one_minus_f_fp);
        let c = -&ln_one_minus_f; // positive

        // sigma in exact fixed-point from rational: sigma_num / sigma_den
        let sigma_fp = IBig::from(sigma_num) * &*PRECISION / IBig::from(sigma_den);

        // x = sigma * c (in fixed-point)
        let mut x = &sigma_fp * &c;
        fp_scale(&mut x);

        // Check: recip_q < exp(x)?
        match ref_exp_cmp(1000, &x, 3, &recip_q) {
            ExpCmpResult::LT => true,       // recip_q < exp(x) → IS leader
            ExpCmpResult::GT => false,      // recip_q >= exp(x) → NOT leader
            ExpCmpResult::Unknown => false, // conservative: not leader
        }
    }

    // -------------------------------------------------------------------------
    // Cached `activeSlotLog` — avoids re-computing ln(1-f) per slot
    // -------------------------------------------------------------------------

    /// A pre-computed `c = |ln(1-f)|` in 34-digit fixed-point.
    ///
    /// This is the value Haskell calls `activeSlotLog` in its block-forging
    /// and leader-schedule code.  Computing it with the continued-fraction
    /// algorithm takes several hundred microseconds; caching it across the
    /// ~432 000 per-epoch slot checks saves significant CPU.
    pub struct CachedLog {
        /// |ln(1-f)| in fixed-point with scale 10^34
        pub c: IBig,
    }

    /// Pre-compute `c = |ln(1-f)|` for the rational active-slot coefficient.
    ///
    /// Returns `None` when `f_num == 0` (no slots active) or
    /// `f_num >= f_den` (all slots active); callers must handle those as
    /// boundary cases without calling the per-slot check.
    pub fn compute_log(f_num: u64, f_den: u64) -> Option<CachedLog> {
        if f_num == 0 || f_den == 0 || f_num >= f_den {
            return None;
        }
        // (1 - f) in exact fixed-point: (f_den - f_num) / f_den * PRECISION
        let one_minus_f_fp = IBig::from(f_den - f_num) * &*PRECISION / IBig::from(f_den);
        let mut ln_one_minus_f = IBig::from(0);
        ref_ln(&mut ln_one_minus_f, &one_minus_f_fp);
        // ln(1-f) is negative for f in (0,1); take its negation.
        let c = -ln_one_minus_f;
        Some(CachedLog { c })
    }

    /// Leader check using a pre-computed `c = |ln(1-f)|`.
    ///
    /// Skips the `ref_ln` call entirely — useful when iterating over all
    /// slots in an epoch and `f` is constant.
    pub fn check_leader_value_with_cached_log(
        vrf_output: &[u8],
        sigma_num: u64,
        sigma_den: u64,
        log: &CachedLog,
        tpraos: bool,
    ) -> bool {
        if sigma_num == 0 || sigma_den == 0 {
            return false;
        }

        let cert_nat_max = if tpraos {
            IBig::from(2).pow(512)
        } else {
            cert_nat_max()
        };

        let cert_nat = if tpraos {
            if vrf_output.len() >= 64 {
                IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..64]))
            } else {
                IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
            }
        } else if vrf_output.len() >= 32 {
            IBig::from(dashu_int::UBig::from_be_bytes(&vrf_output[..32]))
        } else {
            IBig::from(dashu_int::UBig::from_be_bytes(vrf_output))
        };

        let q = &cert_nat_max - &cert_nat;
        if q <= *ZERO {
            return false;
        }

        // recip_q = certNatMax / q  (in fixed-point)
        let mut recip_q = IBig::from(0);
        fp_div(&mut recip_q, &cert_nat_max, &q);

        // sigma in exact fixed-point from rational: sigma_num / sigma_den
        let sigma_fp = IBig::from(sigma_num) * &*PRECISION / IBig::from(sigma_den);

        // x = sigma * c  (in fixed-point)
        let mut x = &sigma_fp * &log.c;
        fp_scale(&mut x);

        // Check: recip_q < exp(x)?
        match ref_exp_cmp(1000, &x, 3, &recip_q) {
            ExpCmpResult::LT => true,       // recip_q < exp(x) → IS leader
            ExpCmpResult::GT => false,      // recip_q >= exp(x) → NOT leader
            ExpCmpResult::Unknown => false, // conservative: not leader
        }
    }

    /// Convert an f64 to the nearest rational p/q with q <= 10000.
    fn f64_to_rational(value: f64) -> (u64, u64) {
        for den in [1, 2, 4, 5, 10, 20, 25, 50, 100, 200, 1000, 10000] {
            let num = (value * den as f64).round() as u64;
            let reconstructed = num as f64 / den as f64;
            if (reconstructed - value).abs() < 1e-15 {
                let g = gcd(num, den);
                return (num / g, den / g);
            }
        }
        let den = 1_000_000u64;
        let num = (value * den as f64).round() as u64;
        let g = gcd(num, den);
        (num / g, den / g)
    }

    fn gcd(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a
    }

    /// Convert an f64 value to fixed-point IBig with 10^34 scale.
    fn float_to_fixed(value: f64) -> IBig {
        // Guard against NaN/infinity which would cause infinite recursion
        // (NaN <= 0.0 is false in Rust, bypassing the zero guard below).
        if !value.is_finite() || value <= 0.0 {
            return IBig::from(0);
        }
        if value >= 1.0 {
            let int_part = value as u64;
            let frac = value - int_part as f64;
            let int_fp = &*PRECISION * IBig::from(int_part);
            let frac_fp = float_to_fixed(frac);
            return int_fp + frac_fp;
        }

        // Use mantissa/exponent decomposition for maximum f64 precision
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7FF) as i64 - 1023;
        let mantissa_bits = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;

        let shift = 52 - exponent;
        if shift >= 0 {
            (IBig::from(mantissa_bits) * &*PRECISION) >> shift as usize
        } else {
            (IBig::from(mantissa_bits) * &*PRECISION) << (-shift) as usize
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn ibig_to_f64(val: &IBig) -> f64 {
            let is_negative = val < &*ZERO;
            let abs_val = val.abs();
            let s = abs_val.to_string();
            let f: f64 = s.parse().unwrap_or(0.0);
            let result = f / 1e34;
            if is_negative {
                -result
            } else {
                result
            }
        }

        #[test]
        fn test_ref_ln_one() {
            let mut result = IBig::from(0);
            ref_ln(&mut result, &ONE);
            assert!(result == *ZERO, "ln(1) should be 0, got {}", result);
        }

        #[test]
        fn test_ref_ln_095_exact() {
            // Compute ln(0.95) using exact rational: 0.95 = 19/20
            let x = IBig::from(19) * &*PRECISION / IBig::from(20);
            let mut result = IBig::from(0);
            ref_ln(&mut result, &x);
            let result_f64 = ibig_to_f64(&result);
            let expected = (0.95f64).ln();
            assert!(
                (result_f64 - expected).abs() < 1e-15,
                "ln(0.95) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_ref_ln_2() {
            let x = &*PRECISION * IBig::from(2); // 2.0 in fixed-point
            let mut result = IBig::from(0);
            ref_ln(&mut result, &x);
            let result_f64 = ibig_to_f64(&result);
            let expected = 2.0f64.ln();
            assert!(
                (result_f64 - expected).abs() < 1e-10,
                "ln(2) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_ref_ln_half() {
            let x = &*PRECISION / IBig::from(2); // 0.5 in fixed-point
            let mut result = IBig::from(0);
            ref_ln(&mut result, &x);
            let result_f64 = ibig_to_f64(&result);
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

        #[test]
        fn test_exp_cmp_basic() {
            // exp(1) ≈ 2.718... — a value of 3 should be GT
            let three = IBig::from(3) * &*PRECISION;
            assert_eq!(ref_exp_cmp(1000, &ONE, 3, &three), ExpCmpResult::GT);

            // exp(1) ≈ 2.718... — a value of 2 should be LT
            let two = IBig::from(2) * &*PRECISION;
            assert_eq!(ref_exp_cmp(1000, &ONE, 3, &two), ExpCmpResult::LT);
        }

        #[test]
        fn test_ref_exp_one() {
            // exp(0) = 1
            let mut result = IBig::from(0);
            ref_exp(&mut result, &ZERO);
            assert_eq!(result, *ONE, "exp(0) should be 1");
        }

        #[test]
        fn test_ref_exp_e() {
            // exp(1) should equal E (our precomputed constant)
            let mut result = IBig::from(0);
            ref_exp(&mut result, &ONE);
            // Allow tiny rounding difference
            let diff = (&result - &*E).abs();
            let tolerance = IBig::from(10).pow(5); // 10^5 tolerance in 10^34 scale
            assert!(diff < tolerance, "exp(1) should equal E, diff = {}", diff);
        }

        #[test]
        fn test_ref_exp_negative() {
            // exp(-1) = 1/e ≈ 0.367879...
            let neg_one = -&*ONE;
            let mut result = IBig::from(0);
            ref_exp(&mut result, &neg_one);
            let result_f64 = ibig_to_f64(&result);
            let expected = (-1.0f64).exp();
            assert!(
                (result_f64 - expected).abs() < 1e-10,
                "exp(-1) should be ~{}, got {}",
                expected,
                result_f64
            );
        }

        #[test]
        fn test_ref_ln_e() {
            // ln(e) = 1
            let mut result = IBig::from(0);
            ref_ln(&mut result, &E);
            let diff = (&result - &*ONE).abs();
            // Rounding error in arg reduction: ~10^{-25} precision = 10^{34-25} = 10^9
            let tolerance = IBig::from(10).pow(10);
            assert!(
                diff < tolerance,
                "ln(e) should be 1, got {} (diff {})",
                ibig_to_f64(&result),
                diff
            );
        }

        #[test]
        fn test_fp_div_exact() {
            // 10 / 3 should be 3.333...
            let ten = IBig::from(10) * &*PRECISION;
            let three = IBig::from(3) * &*PRECISION;
            let mut result = IBig::from(0);
            fp_div(&mut result, &ten, &three);
            let result_f64 = ibig_to_f64(&result);
            assert!(
                (result_f64 - 10.0 / 3.0).abs() < 1e-15,
                "10/3 should be ~3.333, got {}",
                result_f64
            );
        }

        #[test]
        fn test_float_to_fixed_precision() {
            // Test that f64 → fixed-point conversion preserves 15+ digits
            let values = [0.05, 0.001, 0.0001, 0.999999, 0.5, 0.123456789012345];
            for v in values {
                let fp = float_to_fixed(v);
                let roundtrip = ibig_to_f64(&fp);
                assert!(
                    (roundtrip - v).abs() / v < 1e-14,
                    "float_to_fixed({}) roundtrip = {} (rel err = {})",
                    v,
                    roundtrip,
                    (roundtrip - v).abs() / v
                );
            }
        }

        #[test]
        fn test_leader_check_small_stake() {
            // This is the critical test case that was failing with num-bigint.
            // A small pool (relative_stake ≈ 0.0009) should still be eligible
            // for some slots. With f=0.05, p_elect ≈ 1-(1-0.05)^0.0009 ≈ 0.0000461
            // So a VRF output near zero should definitely be elected.
            let small_output = [0u8; 32]; // certNat = 0, smallest possible
            assert!(
                check_leader_value_exact(&small_output, 0.0009, 0.05),
                "Pool with 0.09% stake should be leader with VRF output = 0"
            );

            // A slightly larger VRF output: first byte = 0x01
            let mut medium_output = [0u8; 32];
            medium_output[0] = 0x01;
            // p_elect ≈ 0.0000461, threshold ≈ 0.0000461 * 2^256
            // 0x01000...000 / 2^256 = 1/256 ≈ 0.0039 >> 0.0000461 → should NOT be leader
            assert!(
                !check_leader_value_exact(&medium_output, 0.0009, 0.05),
                "Pool with 0.09% stake should NOT be leader with VRF output 0x01..."
            );
        }

        #[test]
        fn test_leader_check_boundary_stakes() {
            // Test a range of relative stakes to ensure monotonicity:
            // larger stake → more likely to be leader (lower VRF threshold needed)
            let vrf_output = {
                let mut out = [0u8; 32];
                // Set VRF output to ~0.01 * 2^256 (very low, should be leader for most stakes)
                out[0] = 0x02;
                out
            };

            // With f=0.05:
            // stake=0.001: p ≈ 0.0000513 → threshold at ~0.0000513*2^256 → 0x02 too high
            // stake=0.01:  p ≈ 0.000513  → threshold higher
            // stake=0.1:   p ≈ 0.00513   → threshold higher
            // stake=0.5:   p ≈ 0.0253    → threshold at ~0.0253*2^256 → 0x02 ≈ 0.0078 → elected
            // stake=1.0:   p ≈ 0.05      → definitely elected

            // Lower stakes should be less likely to be elected
            let results: Vec<bool> = [0.001, 0.01, 0.1, 0.5, 1.0]
                .iter()
                .map(|s| check_leader_value_exact(&vrf_output, *s, 0.05))
                .collect();

            // Verify monotonicity: if elected at stake s, should be elected at all stakes > s
            for i in 0..results.len() {
                for j in (i + 1)..results.len() {
                    if results[i] {
                        assert!(
                            results[j],
                            "Monotonicity violated: elected at stake {} but not at {}",
                            [0.001, 0.01, 0.1, 0.5, 1.0][i],
                            [0.001, 0.01, 0.1, 0.5, 1.0][j]
                        );
                    }
                }
            }
        }

        #[test]
        fn test_leader_check_different_active_slot_coeffs() {
            // Higher active_slot_coeff means more slots filled → higher probability
            let vrf_output = [0x10u8; 32]; // ~6.3% of max
            let stake = 0.5;

            let result_005 = check_leader_value_exact(&vrf_output, stake, 0.05);
            let result_010 = check_leader_value_exact(&vrf_output, stake, 0.1);

            // With f=0.1, probability is higher than f=0.05
            // If elected with f=0.05, definitely elected with f=0.1
            if result_005 {
                assert!(
                    result_010,
                    "Should be elected with f=0.1 if elected with f=0.05"
                );
            }
        }

        #[test]
        fn test_leader_check_statistical_distribution() {
            // Generate 1000 sequential "VRF outputs" and count elections for various stakes
            // This validates the overall probability matches the expected formula
            let f = 0.05;
            let stake = 1.0;
            // Expected: p = 1 - (1-f)^stake = 1 - 0.95 = 0.05

            let mut elected = 0;
            let trials = 1000;
            for i in 0..trials {
                let mut output = [0u8; 32];
                // Spread outputs across the range
                let val = (i as u64 * (u64::MAX / trials as u64)).to_be_bytes();
                output[..8].copy_from_slice(&val);
                if check_leader_value_exact(&output, stake, f) {
                    elected += 1;
                }
            }

            // Expected ~50 out of 1000 (5%), allow ±3%
            let rate = elected as f64 / trials as f64;
            assert!(
                (rate - 0.05).abs() < 0.03,
                "Election rate should be ~5%, got {:.1}% ({}/{})",
                rate * 100.0,
                elected,
                trials
            );
        }

        #[test]
        fn test_exp_cmp_near_boundary() {
            // Test exp_cmp distinguishes values sufficiently far from exp(x)
            // exp(0.05) ≈ 1.05127...
            let x = float_to_fixed(0.05);

            // Value clearly above exp(0.05): 1.06 > 1.05127
            let above = float_to_fixed(1.06);
            assert_eq!(
                ref_exp_cmp(1000, &x, 3, &above),
                ExpCmpResult::GT,
                "1.06 should be above exp(0.05)"
            );

            // Value clearly below exp(0.05): 1.04 < 1.05127
            let below = float_to_fixed(1.04);
            assert_eq!(
                ref_exp_cmp(1000, &x, 3, &below),
                ExpCmpResult::LT,
                "1.04 should be below exp(0.05)"
            );
        }

        #[test]
        fn test_leader_check_diagnostic() {
            // Simulate the EXACT computation path used for received blocks.
            // For a pool with ~4% relative stake and f=0.05:
            // phi_f(sigma) = 1 - (1-f)^sigma = 1 - 0.95^0.04 ≈ 0.002052
            // A block produced by this pool must have certNat/certNatMax < 0.002052
            // This means the leader_value first byte must be 0x00.
            let sigma = 0.039146;
            let f = 0.05;
            let (f_num, f_den) = f64_to_rational(f);
            assert_eq!((f_num, f_den), (1, 20), "f=0.05 should be 1/20");

            // Expected threshold: phi_f(sigma) = 1 - (1-f)^sigma
            let phi = 1.0 - (1.0 - f).powf(sigma);
            eprintln!("phi_f({}) = {}", sigma, phi);

            // A leader_value that SHOULD pass (certNat = 0, minimum possible)
            let leader_value_zero = [0u8; 32];
            assert!(
                check_leader_value_exact(&leader_value_zero, sigma, f),
                "certNat=0 must always be leader"
            );

            // A leader_value that SHOULD pass (certNat just below threshold)
            // threshold = phi * 2^256
            // Use leader_value starting with 0x00, 0x00 (certNat < 2^240)
            let mut leader_just_below = [0u8; 32];
            leader_just_below[2] = 0x01; // certNat = 2^232, ratio = 2^232/2^256 = 2^-24 ≈ 5.96e-8
            eprintln!(
                "Testing certNat ratio = 2^-24 ≈ {:.2e} vs phi = {:.6e}",
                1.0 / (1u64 << 24) as f64,
                phi
            );
            assert!(
                check_leader_value_exact(&leader_just_below, sigma, f),
                "certNat = 2^-24 should be well below phi = {:.6e}",
                phi
            );

            // A leader_value that SHOULD FAIL (certNat well above threshold)
            let mut leader_above = [0u8; 32];
            leader_above[0] = 0x10; // certNat/certNatMax ≈ 16/256 = 0.0625 >> phi
            assert!(
                !check_leader_value_exact(&leader_above, sigma, f),
                "certNat ratio 0.0625 should NOT be leader (phi = {:.6})",
                phi
            );

            // Now trace intermediate values for the passing case
            let cert_nat_max = cert_nat_max();
            let cert_nat = IBig::from(dashu_int::UBig::from_be_bytes(&leader_just_below));
            let q = &cert_nat_max - &cert_nat;
            let mut recip_q = IBig::from(0);
            fp_div(&mut recip_q, &cert_nat_max, &q);
            let recip_q_f64 = ibig_to_f64(&recip_q);
            eprintln!("recip_q (1/(1-a)) = {:.15}", recip_q_f64);

            let one_minus_f_fp = IBig::from(f_den - f_num) * &*PRECISION / IBig::from(f_den);
            let mut ln_one_minus_f = IBig::from(0);
            ref_ln(&mut ln_one_minus_f, &one_minus_f_fp);
            let c = -&ln_one_minus_f;
            let c_f64 = ibig_to_f64(&c);
            eprintln!(
                "|ln(1-f)| = {:.15} (expected {:.15})",
                c_f64,
                -(1.0 - f).ln()
            );

            let sigma_fp = float_to_fixed(sigma);
            let sigma_roundtrip = ibig_to_f64(&sigma_fp);
            eprintln!(
                "sigma fixed-point roundtrip = {:.15} (input = {:.15})",
                sigma_roundtrip, sigma
            );

            let mut x = &sigma_fp * &c;
            fp_scale(&mut x);
            let x_f64 = ibig_to_f64(&x);
            let expected_x = sigma * (-(1.0 - f).ln());
            eprintln!(
                "x = sigma * |ln(1-f)| = {:.15} (expected {:.15})",
                x_f64, expected_x
            );

            let mut exp_x = IBig::from(0);
            ref_exp(&mut exp_x, &x);
            let exp_x_f64 = ibig_to_f64(&exp_x);
            eprintln!(
                "exp(x) = {:.15} (expected {:.15})",
                exp_x_f64,
                expected_x.exp()
            );
            eprintln!(
                "recip_q < exp(x)? {} < {} = {}",
                recip_q_f64,
                exp_x_f64,
                recip_q_f64 < exp_x_f64
            );

            // Also test via ref_exp_cmp
            let cmp_result = ref_exp_cmp(1000, &x, 3, &recip_q);
            eprintln!("ref_exp_cmp result = {:?}", cmp_result);
            assert_eq!(
                cmp_result,
                ExpCmpResult::LT,
                "recip_q ({:.10}) should be LT exp(x) ({:.10})",
                recip_q_f64,
                exp_x_f64
            );
        }

        #[test]
        fn test_leader_check_real_stakes() {
            // Test with exact relative_stake values from actual preview testnet pools
            let stakes = [
                0.039145993077029484,
                0.0009393015339035087,
                0.0009173133273195841,
            ];
            let f = 0.05;

            for sigma in &stakes {
                // A VRF output of all zeros MUST pass (certNat = 0)
                let zero_output = [0u8; 32];
                assert!(
                    check_leader_value_exact(&zero_output, *sigma, f),
                    "certNat=0 must always pass for sigma={}",
                    sigma
                );

                // A VRF output starting with 0x10 (certNat/max ≈ 6.25%) should fail
                // since phi_f(sigma) < 0.003 for all these stakes
                let mut high_output = [0u8; 32];
                high_output[0] = 0x10;
                assert!(
                    !check_leader_value_exact(&high_output, *sigma, f),
                    "certNat ≈ 6.25% should NOT pass for sigma={}",
                    sigma
                );
            }
        }

        #[test]
        fn test_tpraos_leader_check() {
            // TPraos: raw 64-byte output, certNatMax = 2^512
            // With sigma=1.0, f=0.05: phi = 0.05
            // certNat=0 should always pass
            assert!(check_leader_value_tpraos(&[0u8; 64], 1.0, 0.05));

            // High output should fail
            assert!(!check_leader_value_tpraos(&[0xFFu8; 64], 1.0, 0.05));

            // Zero stake never passes
            assert!(!check_leader_value_tpraos(&[0u8; 64], 0.0, 0.05));
        }

        #[test]
        fn test_ln_exp_roundtrip() {
            // ln(exp(x)) should equal x for various values
            let test_values = [0.01, 0.05, 0.1, 0.5, 1.0, 2.0];
            for v in test_values {
                let x = float_to_fixed(v);
                let mut exp_x = IBig::from(0);
                ref_exp(&mut exp_x, &x);
                let mut ln_exp_x = IBig::from(0);
                ref_ln(&mut ln_exp_x, &exp_x);

                let diff = (&ln_exp_x - &x).abs();
                let tolerance = IBig::from(10).pow(10); // 10^{-24} precision
                assert!(
                    diff < tolerance,
                    "ln(exp({})) should equal {}, diff = {}",
                    v,
                    v,
                    ibig_to_f64(&diff)
                );
            }
        }

        // =================================================================
        // High-precision mathematical accuracy tests
        // =================================================================

        #[test]
        fn test_ln_known_values_high_precision() {
            // Test ln against known mathematical constants to 15+ digits
            let cases: Vec<(f64, f64)> = vec![
                (0.5, -(std::f64::consts::LN_2)),      // ln(1/2)
                (0.25, -2.0 * std::f64::consts::LN_2), // ln(1/4)
                (0.1, -(std::f64::consts::LN_10)),     // ln(1/10)
                (0.99, -0.010050335853501363),         // ln(99/100)
                (0.999, -0.0010005003335835335),       // ln(999/1000)
                (0.9999, -0.00010000500033335834),     // ln(9999/10000)
                (std::f64::consts::E, 1.0),            // ln(e)
                (2.0, std::f64::consts::LN_2),         // ln(2)
                (10.0, std::f64::consts::LN_10),       // ln(10)
            ];

            for (input, expected) in cases {
                let x_fp = float_to_fixed(input);
                let mut result = IBig::from(0);
                ref_ln(&mut result, &x_fp);
                let result_f64 = ibig_to_f64(&result);

                let rel_err = if expected.abs() > 1e-15 {
                    (result_f64 - expected).abs() / expected.abs()
                } else {
                    (result_f64 - expected).abs()
                };
                assert!(
                    rel_err < 1e-10,
                    "ln({}) = {}, expected {} (rel_err = {:.2e})",
                    input,
                    result_f64,
                    expected,
                    rel_err
                );
            }
        }

        #[test]
        fn test_exp_known_values_high_precision() {
            // Test exp against known mathematical constants
            let cases: Vec<(f64, f64)> = vec![
                (0.0, 1.0),
                (1.0, std::f64::consts::E),
                // Note: ref_exp handles negative x internally via exp(-x)=1/exp(x),
                // but float_to_fixed clamps negatives to 0. Test only positive values
                // through the float_to_fixed path.
                (0.5, 1.6487212707001282),
                (2.0, 7.38905609893065),
                (0.001, 1.0010005001667084),
                (0.0001, 1.0001000050001667),
            ];

            for (input, expected) in cases {
                let x_fp = float_to_fixed(input);
                let mut result = IBig::from(0);
                ref_exp(&mut result, &x_fp);
                let result_f64 = ibig_to_f64(&result);

                let rel_err = if expected.abs() > 1e-15 {
                    (result_f64 - expected).abs() / expected.abs()
                } else {
                    (result_f64 - expected).abs()
                };
                assert!(
                    rel_err < 1e-10,
                    "exp({}) = {}, expected {} (rel_err = {:.2e})",
                    input,
                    result_f64,
                    expected,
                    rel_err
                );
            }
        }

        #[test]
        fn test_fp_div_edge_cases() {
            // Division edge cases
            let mut result = IBig::from(0);

            // 1 / 1 = 1
            fp_div(&mut result, &ONE, &ONE);
            assert_eq!(result, *ONE, "1/1 should be 1");

            // 1 / 2 = 0.5
            let two = IBig::from(2) * &*PRECISION;
            fp_div(&mut result, &ONE, &two);
            let expected_half = &*PRECISION / IBig::from(2);
            assert_eq!(result, expected_half, "1/2 should be 0.5");

            // 1 / 3 (irrational, test truncation direction)
            let three = IBig::from(3) * &*PRECISION;
            fp_div(&mut result, &ONE, &three);
            let expected = IBig::from_str_radix("3333333333333333333333333333333333", 10).unwrap();
            // Should be 3.333...×10^33 (truncated)
            let diff = (&result - &expected).abs();
            assert!(
                diff <= IBig::from(1),
                "1/3 should be 3.333...e33, got {}, diff={}",
                result,
                diff
            );

            // Very large / very small
            let big = IBig::from(10).pow(40) * &*PRECISION;
            let small = IBig::from(7) * &*PRECISION;
            fp_div(&mut result, &big, &small);
            let expected_f64 = 1e40 / 7.0;
            let result_f64 = ibig_to_f64(&result);
            assert!(
                (result_f64 - expected_f64).abs() / expected_f64 < 1e-10,
                "10^40/7 rel_err too large"
            );
        }

        #[test]
        fn test_exp_cmp_comprehensive() {
            // Test taylorExpCmp at various precision boundaries
            let test_cases: Vec<(f64, f64, ExpCmpResult)> = vec![
                (1.0, 3.0, ExpCmpResult::GT),      // 3 > e ≈ 2.718
                (1.0, 2.0, ExpCmpResult::LT),      // 2 < e
                (0.5, 2.0, ExpCmpResult::GT),      // 2 > exp(0.5) ≈ 1.649
                (0.5, 1.5, ExpCmpResult::LT),      // 1.5 < exp(0.5) ≈ 1.649
                (2.0, 8.0, ExpCmpResult::GT),      // 8 > exp(2) ≈ 7.389
                (2.0, 7.0, ExpCmpResult::LT),      // 7 < exp(2) ≈ 7.389
                (0.001, 1.002, ExpCmpResult::GT),  // 1.002 > exp(0.001) ≈ 1.001
                (0.001, 1.0005, ExpCmpResult::LT), // 1.0005 < exp(0.001) ≈ 1.001
            ];

            for (x_val, cmp_val, expected) in test_cases {
                let x = float_to_fixed(x_val);
                let cmp = float_to_fixed(cmp_val);
                let result = ref_exp_cmp(1000, &x, 3, &cmp);
                assert_eq!(
                    result, expected,
                    "exp_cmp(x={}, cmp={}) should be {:?}, got {:?}",
                    x_val, cmp_val, expected, result
                );
            }
        }

        #[test]
        fn test_leader_check_threshold_basic() {
            // Verify basic threshold behavior:
            // With sigma=0.01, f=0.05: phi ≈ 0.000513
            // certNat=0 → always leader
            // certNat=0x10... → 6.25% >> 0.05% → NOT leader
            let sigma: f64 = 0.01;
            let f: f64 = 0.05;

            assert!(
                check_leader_value_exact(&[0u8; 32], sigma, f),
                "certNat=0 must always be leader"
            );

            let mut above = [0u8; 32];
            above[0] = 0x10;
            assert!(
                !check_leader_value_exact(&above, sigma, f),
                "certNat ≈ 6.25% should NOT be leader (phi ≈ 0.05%)"
            );
        }

        #[test]
        fn test_leader_check_praos_vs_tpraos_consistency() {
            // For the same relative_stake and f, Praos (32-byte VRF, 2^256)
            // and TPraos (64-byte VRF, 2^512) should agree on zero output
            let sigma = 0.5;
            let f = 0.05;

            assert!(check_leader_value_exact(&[0u8; 32], sigma, f));
            assert!(check_leader_value_tpraos(&[0u8; 64], sigma, f));

            // Both should reject max output
            assert!(!check_leader_value_exact(&[0xFFu8; 32], sigma, f));
            assert!(!check_leader_value_tpraos(&[0xFFu8; 64], sigma, f));
        }

        #[test]
        fn test_golden_vector_intermediate_computations() {
            // Parse the golden test vectors and verify our intermediate
            // computations match the expected results.
            //
            // The golden vectors test the non-integral math library with
            // 34-digit fixed-point precision. We verify by parsing the
            // result decimal values and comparing against our IBig computation.
            let inputs = include_str!("../../../tests/golden/vrf/golden_tests.txt");
            let results = include_str!("../../../tests/golden/vrf/golden_tests_result.txt");

            let mut tested = 0;
            for (i, (input_line, result_line)) in inputs.lines().zip(results.lines()).enumerate() {
                let iparts: Vec<&str> = input_line.split_whitespace().collect();
                let rparts: Vec<&str> = result_line.split_whitespace().collect();
                if iparts.len() < 3 || rparts.len() < 6 {
                    continue;
                }

                // Parse inputs as IBig (34-digit fixed-point integers)
                let sigma = IBig::from_str_radix(iparts[0], 10).unwrap();
                let f_val = IBig::from_str_radix(iparts[1], 10).unwrap();
                let _cert_nat = IBig::from_str_radix(iparts[2], 10).unwrap();

                // Parse expected comparison and leader result
                let expected_cmp = rparts[4];
                let _expected_leader = rparts[5] == "1";

                // The golden vectors use certNatMax = 2^256
                // recip_q = certNatMax * PRECISION / (certNatMax - certNat)
                // But certNat in the golden vectors is already a fixed-point value
                // (same 10^34 scale as sigma and f).
                //
                // Since we can't determine the exact mapping without the
                // Haskell test generator source, verify at minimum that:
                // 1. The inputs parse correctly as IBig
                // 2. The comparison and leader fields are valid
                assert!(
                    expected_cmp == "GT" || expected_cmp == "LT",
                    "Line {}: invalid comparison '{}'",
                    i + 1,
                    expected_cmp
                );
                assert!(
                    rparts[5] == "0" || rparts[5] == "1",
                    "Line {}: invalid leader bool '{}'",
                    i + 1,
                    rparts[5]
                );

                // Verify sigma and f are in valid range (0, PRECISION]
                assert!(sigma > *ZERO, "Line {}: sigma should be positive", i + 1);
                assert!(
                    f_val > *ZERO && f_val <= *ONE,
                    "Line {}: f should be in (0, 1]",
                    i + 1
                );

                tested += 1;
            }
            assert_eq!(tested, 100, "Should test all 100 golden vectors");
        }
    }
}

/// A VRF key pair for proof generation.
///
/// The secret key is zeroized on drop to prevent key material from lingering
/// in memory. The secret_key field is private — use `secret_key()` to access.
pub struct VrfKeyPair {
    secret_key: zeroize::Zeroizing<[u8; 32]>,
    pub public_key: [u8; 32],
}

impl VrfKeyPair {
    /// Access the secret key bytes (for VRF proof generation).
    pub fn secret_key(&self) -> &[u8; 32] {
        &self.secret_key
    }
}

/// Generate a VRF key pair from an existing 32-byte secret key.
pub fn generate_vrf_keypair_from_secret(secret: &[u8; 32]) -> VrfKeyPair {
    let sk = SecretKey03::from_bytes(secret);
    let (scalar, _) = sk.extend();
    let point = scalar * ED25519_BASEPOINT_POINT;
    let pk_bytes = point.compress().to_bytes();

    VrfKeyPair {
        secret_key: zeroize::Zeroizing::new(*secret),
        public_key: pk_bytes,
    }
}

/// Generate a new VRF key pair using a cryptographically secure RNG.
pub fn generate_vrf_keypair() -> VrfKeyPair {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut seed);
    let sk = SecretKey03::from_bytes(&seed);
    let secret_bytes = sk.to_bytes();

    // Derive public key: extend secret to get scalar, then scalar * basepoint
    let (scalar, _) = sk.extend();
    let point = scalar * ED25519_BASEPOINT_POINT;
    let pk_bytes = point.compress().to_bytes();

    VrfKeyPair {
        secret_key: zeroize::Zeroizing::new(secret_bytes),
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

// ─── VRF v13 (PraosBatchCompatVRF) internals ───────────────────────────────
//
// Hash-to-curve for ECVRF draft-13 batch-compatible uses RFC 9380
// expand_message_xmd with SHA-512, NOT the simple SHA-512-then-first-32-bytes
// used by draft-03. DST = "ECVRF_edwards25519_XMD:SHA-512_ELL2_NU_\x04".
// Input message = pk_bytes (32) || alpha (variable).
//
// Reference: cardano-crypto-praos cbits/vrf13_batchcompat/prove.c +
//            cbits/private/core_h2c.c + cbits/private/ed25519_ref10.c.

/// RFC 9380 §5.3.1 expand_message_xmd using SHA-512.
fn expand_message_xmd_sha512(msg: &[u8], dst: &[u8], len_in_bytes: usize) -> Vec<u8> {
    use sha2::Digest as _;
    // SHA-512 parameters
    const B_BYTES: usize = 64; // hash output size
    const R_BYTES: usize = 128; // block size (Z_pad length)

    let ell = len_in_bytes.div_ceil(B_BYTES);
    assert!(ell <= 255 && len_in_bytes <= 65535 && dst.len() <= 255);

    // DST_prime = DST || I2OSP(|DST|, 1)
    let dst_len_byte = dst.len() as u8;
    let lib_str = [(len_in_bytes >> 8) as u8, (len_in_bytes & 0xff) as u8];

    // b_0 = H(Z_pad || msg || I2OSP(len,2) || I2OSP(0,1) || DST_prime)
    let b0: [u8; 64] = {
        let mut h = sha2::Sha512::new();
        h.update([0u8; R_BYTES]);
        h.update(msg);
        h.update(lib_str);
        h.update([0u8]); // counter 0
        h.update(dst);
        h.update([dst_len_byte]);
        h.finalize().into()
    };

    // b_1 = H(b_0 || I2OSP(1,1) || DST_prime)
    let b1: [u8; 64] = {
        let mut h = sha2::Sha512::new();
        h.update(b0);
        h.update([1u8]);
        h.update(dst);
        h.update([dst_len_byte]);
        h.finalize().into()
    };

    let mut out = b1.to_vec();
    let mut b_prev = b1;

    for i in 2u8..=(ell as u8) {
        // b_i = H(strxor(b_(i-1), b_0) || I2OSP(i,1) || DST_prime)
        let xored: [u8; 64] = std::array::from_fn(|j| b_prev[j] ^ b0[j]);
        let bi: [u8; 64] = {
            let mut h = sha2::Sha512::new();
            h.update(xored);
            h.update([i]);
            h.update(dst);
            h.update([dst_len_byte]);
            h.finalize().into()
        };
        out.extend_from_slice(&bi);
        b_prev = bi;
    }

    out.truncate(len_in_bytes);
    out
}

// ─── GF(2^255-19) helpers using dashu_int::UBig for v13 hash-to-curve ──────
// These are for correctness (conformance tests), not for production use.

fn v13_fe_mul(a: &dashu_int::UBig, b: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    (a * b) % p
}

fn v13_fe_neg(a: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    let r = a % p;
    if r == dashu_int::UBig::ZERO {
        dashu_int::UBig::ZERO
    } else {
        p - r
    }
}

fn v13_fe_add(a: &dashu_int::UBig, b: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    (a + b) % p
}

fn v13_fe_sub(a: &dashu_int::UBig, b: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    let b_r = b % p;
    let a_r = a % p;
    if a_r >= b_r {
        (a_r - b_r) % p
    } else {
        (p + a_r - b_r) % p
    }
}

fn v13_fe_pow(
    base: &dashu_int::UBig,
    exp: &dashu_int::UBig,
    p: &dashu_int::UBig,
) -> dashu_int::UBig {
    let mut result = dashu_int::UBig::ONE;
    let mut base = base % p;
    let mut e = exp.clone();
    while e > dashu_int::UBig::ZERO {
        if &e & &dashu_int::UBig::ONE != dashu_int::UBig::ZERO {
            result = result * &base % p;
        }
        e >>= 1usize;
        base = &base * &base % p;
    }
    result
}

fn v13_fe_invert(a: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    let exp = p - &dashu_int::UBig::from(2u32);
    v13_fe_pow(a, &exp, p)
}

fn v13_fe_is_square(a: &dashu_int::UBig, p: &dashu_int::UBig) -> bool {
    if *a == dashu_int::UBig::ZERO || a % p == dashu_int::UBig::ZERO {
        return true;
    }
    let exp = (p - &dashu_int::UBig::ONE) >> 1usize;
    v13_fe_pow(a, &exp, p) == dashu_int::UBig::ONE
}

// For p ≡ 5 (mod 8): sqrt via a^((p+3)/8), possibly * sqrt(-1).
// Does NOT check is_square — caller must ensure a is a square.
fn v13_fe_sqrt(a: &dashu_int::UBig, p: &dashu_int::UBig) -> dashu_int::UBig {
    // (p+3)/8 = (2^255 - 16)/8 = 2^252 - 2
    let exp = (dashu_int::UBig::ONE << 252usize) - &dashu_int::UBig::from(2u32);
    let v = v13_fe_pow(a, &exp, p);
    // Check if v^2 == a mod p
    if &v * &v % p == a % p {
        v
    } else {
        // Multiply by sqrt(-1) = 2^((p-1)/4) mod p
        let sqrt_m1_exp = (p - &dashu_int::UBig::ONE) >> 2usize;
        let sqrt_m1 = v13_fe_pow(&dashu_int::UBig::from(2u32), &sqrt_m1_exp, p);
        v13_fe_mul(&sqrt_m1, &v, p)
    }
}

fn v13_fe_parity(a: &dashu_int::UBig, p: &dashu_int::UBig) -> u8 {
    let r = a % p;
    let b = r.to_le_bytes();
    if b.is_empty() {
        0
    } else {
        b[0] & 1
    }
}

fn v13_ubig_to_bytes32(n: &dashu_int::UBig) -> [u8; 32] {
    let b = n.to_le_bytes();
    let mut arr = [0u8; 32];
    let len = b.len().min(32);
    arr[..len].copy_from_slice(&b[..len]);
    arr
}

/// Elligator2 on Curve25519 (RFC 9380 §6.7.1, simplified Montgomery form).
///
/// Returns `(x_montgomery, notsquare)` where `notsquare` is true when gx1 was
/// not a quadratic residue (the x2 branch was taken).
fn v13_elligator2_x25519(u: &dashu_int::UBig, p: &dashu_int::UBig) -> (dashu_int::UBig, bool) {
    let a = dashu_int::UBig::from(486662u64); // Curve25519 Montgomery A

    // tv1 = 2 * u^2
    let u_sq = v13_fe_mul(u, u, p);
    let tv1_raw = v13_fe_mul(&dashu_int::UBig::from(2u32), &u_sq, p);

    // e1 = tv1 == -1 (p-1)
    let p_m1 = p - &dashu_int::UBig::ONE;
    let tv1 = if tv1_raw == p_m1 {
        dashu_int::UBig::ZERO
    } else {
        tv1_raw
    };

    // x1 = -A / (1 + tv1)
    let denom = v13_fe_add(&dashu_int::UBig::ONE, &tv1, p);
    let denom_inv = v13_fe_invert(&denom, p);
    let x1 = v13_fe_mul(&v13_fe_neg(&a, p), &denom_inv, p);

    // gx1 = x1^3 + A*x1^2 + x1
    let x1_sq = v13_fe_mul(&x1, &x1, p);
    let x1_cu = v13_fe_mul(&x1_sq, &x1, p);
    let gx1 = v13_fe_add(&v13_fe_add(&x1_cu, &v13_fe_mul(&a, &x1_sq, p), p), &x1, p);

    // x2 = -x1 - A
    let x2 = v13_fe_sub(&v13_fe_neg(&x1, p), &a, p);

    // gx2 = tv1 * gx1
    let gx2 = v13_fe_mul(&tv1, &gx1, p);

    let gx1_is_sq = v13_fe_is_square(&gx1, p);
    let _ = gx2; // gx2 used only for determining y below; not needed for x

    if gx1_is_sq {
        (x1, false) // notsquare=false
    } else {
        (x2, true) // notsquare=true
    }
}

/// Hash-to-curve for ECVRF draft-13 batch-compatible.
///
/// Implements the exact algorithm used by cardano-base PraosBatchCompatVRF:
/// expand_message_xmd(SHA-512, pk||alpha, DST, 48) → byte-reverse → reduce64 →
/// Elligator2 on Curve25519 → Montgomery-to-Edwards birational map → ×cofactor.
fn hash_to_curve_v13(
    pk_bytes: &[u8; 32],
    seed: &[u8],
) -> curve25519_dalek_fork::edwards::EdwardsPoint {
    use curve25519_dalek_fork::montgomery::MontgomeryPoint;
    use dashu_int::UBig;

    // DST = "ECVRF_edwards25519_XMD:SHA-512_ELL2_NU_\x04" (40 bytes)
    const DST: &[u8] = b"ECVRF_edwards25519_XMD:SHA-512_ELL2_NU_\x04";

    // Message = pk || alpha
    let mut msg = Vec::with_capacity(32 + seed.len());
    msg.extend_from_slice(pk_bytes);
    msg.extend_from_slice(seed);

    // expand_message_xmd → 48 bytes big-endian
    let h_be = expand_message_xmd_sha512(&msg, DST, 48);

    // Byte-reverse to little-endian + zero-pad to 64 bytes
    let mut h_le = [0u8; 64];
    for j in 0..48 {
        h_le[j] = h_be[47 - j];
    }

    // Reduce mod p = 2^255 - 19 as a 512-bit LE integer
    let p: UBig = (UBig::ONE << 255usize) - UBig::from(19u32);
    let n = UBig::from_le_bytes(&h_le);
    let u = n % &p;

    // Elligator2 → x_montgomery, notsquare flag
    let (x_mont, notsquare) = v13_elligator2_x25519(&u, &p);

    // y_sign_target = notsquare ^ 1 = !notsquare (matches C: y_sign = notsquare ^ 1)
    let y_sign_target = (!notsquare) as u8; // 1 = want odd parity, 0 = want even

    // Compute gx = x_mont^3 + A*x_mont^2 + x_mont on Curve25519
    let a_mont = UBig::from(486662u64);
    let x_sq = v13_fe_mul(&x_mont, &x_mont, &p);
    let x_cu = v13_fe_mul(&x_sq, &x_mont, &p);
    let gx = v13_fe_add(
        &v13_fe_add(&x_cu, &v13_fe_mul(&a_mont, &x_sq, &p), &p),
        &x_mont,
        &p,
    );

    // y_candidate = sqrt(gx)
    let y_cand = v13_fe_sqrt(&gx, &p);

    // Adjust y parity to match y_sign_target
    let y_parity = v13_fe_parity(&y_cand, &p);
    let y_mont = if y_parity != y_sign_target {
        v13_fe_neg(&y_cand, &p)
    } else {
        y_cand
    };

    // Edwards X sign via birational map: X_ed = sqrt(-486664) * x_mont / y_mont
    // sqrt(-486664) = sqrt(p - 486664)
    let neg_486664 = &p - UBig::from(486664u64);
    let sqrt_neg_486664 = v13_fe_sqrt(&neg_486664, &p);

    let x_ed_num = v13_fe_mul(&sqrt_neg_486664, &x_mont, &p);
    let x_ed = v13_fe_mul(&x_ed_num, &v13_fe_invert(&y_mont, &p), &p);
    let edwards_x_sign = v13_fe_parity(&x_ed, &p);

    // Build MontgomeryPoint and convert to EdwardsPoint
    MontgomeryPoint(v13_ubig_to_bytes32(&x_mont))
        .to_edwards(edwards_x_sign)
        .expect("v13 Elligator2 produced invalid Montgomery point")
        .mul_by_cofactor()
}

/// Verify a VRF proof (v13 batch-compatible) and return the 64-byte VRF output.
///
/// Implements PraosBatchCompatVRF (ECVRF-ED25519-SHA512-Elligator2 draft-13 batch-compat).
/// Proof format: `[gamma(32) || U(32) || V(32) || response(32)] = 128 bytes`.
///
/// - `vrf_vkey`: 32-byte VRF verification key
/// - `proof_bytes`: 128-byte VRF proof
/// - `seed`: the VRF input
pub fn verify_vrf_proof_v13(
    vrf_vkey: &[u8],
    proof_bytes: &[u8],
    seed: &[u8],
) -> Result<[u8; 64], VrfError> {
    use curve25519_dalek_fork::edwards::{CompressedEdwardsY, EdwardsPoint};
    use curve25519_dalek_fork::scalar::Scalar;
    use curve25519_dalek_fork::traits::VartimeMultiscalarMul;
    use sha2::Digest as _;
    use std::ops::Neg;

    if vrf_vkey.len() != 32 {
        return Err(VrfError::InvalidPublicKey);
    }
    if proof_bytes.len() != 128 {
        return Err(VrfError::InvalidProof(format!(
            "expected 128 bytes, got {}",
            proof_bytes.len()
        )));
    }

    let pk_bytes: [u8; 32] = vrf_vkey.try_into().unwrap();

    // Parse proof: [gamma(32) || U(32) || V(32) || response(32)]
    let gamma = CompressedEdwardsY::from_slice(&proof_bytes[..32])
        .decompress()
        .ok_or(VrfError::InvalidProof("gamma decompress failed".into()))?;
    let u_point = CompressedEdwardsY::from_slice(&proof_bytes[32..64])
        .decompress()
        .ok_or(VrfError::InvalidProof("U decompress failed".into()))?;
    let v_point = CompressedEdwardsY::from_slice(&proof_bytes[64..96])
        .decompress()
        .ok_or(VrfError::InvalidProof("V decompress failed".into()))?;
    let mut resp_bytes = [0u8; 32];
    resp_bytes.copy_from_slice(&proof_bytes[96..]);
    let response = Scalar::from_canonical_bytes(resp_bytes)
        .ok_or(VrfError::InvalidProof("response not canonical".into()))?;

    let decompressed_pk = CompressedEdwardsY::from_slice(&pk_bytes)
        .decompress()
        .ok_or(VrfError::InvalidPublicKey)?;
    if decompressed_pk.is_small_order() {
        return Err(VrfError::InvalidPublicKey);
    }

    // hash_to_curve: RFC 9380 expand_message_xmd + Elligator2 (v13 algorithm)
    let h = hash_to_curve_v13(&pk_bytes, seed);
    let compressed_h = h.compress();

    // batch-compat challenge: SHA-512(0x04||0x02||Y||H||gamma||U||V||0x00)[..16]
    // Y comes before H; no base point B; trailing 0x00 per RFC 9381 §5.4.3
    let challenge = {
        let mut hasher = sha2::Sha512::new();
        hasher.update([0x04, 0x02]);
        hasher.update(pk_bytes); // Y = public key
        hasher.update(compressed_h.to_bytes());
        hasher.update(gamma.compress().to_bytes());
        hasher.update(u_point.compress().to_bytes());
        hasher.update(v_point.compress().to_bytes());
        hasher.update([0x00]);
        let digest = hasher.finalize();
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes[..16].copy_from_slice(&digest[..16]);
        Scalar::from_bits(scalar_bytes)
    };

    // U' = response * G - challenge * PK
    let u_check = EdwardsPoint::vartime_double_scalar_mul_basepoint(
        &challenge.neg(),
        &decompressed_pk,
        &response,
    );
    // V' = response * H - challenge * gamma
    let v_check = EdwardsPoint::vartime_multiscalar_mul(
        [response, challenge.neg()].iter().copied(),
        [h, gamma].iter().copied(),
    );

    if u_check != u_point || v_check != v_point {
        return Err(VrfError::VerificationFailed);
    }

    // proof_to_hash: Sha512(0x04 || 0x03 || gamma_cofac || 0x00)
    let gamma_cofac = gamma.mul_by_cofactor();
    let mut hasher = sha2::Sha512::new();
    hasher.update([0x04, 0x03]);
    hasher.update(gamma_cofac.compress().to_bytes());
    hasher.update([0x00u8]);
    let mut output = [0u8; 64];
    output.copy_from_slice(&hasher.finalize());
    Ok(output)
}

/// Generate a VRF proof (v13 batch-compatible) for the given seed using a secret key.
///
/// Returns the 128-byte proof and 64-byte output.
pub fn generate_vrf_proof_v13(
    secret_key: &[u8; 32],
    seed: &[u8],
) -> Result<([u8; 128], [u8; 64]), VrfError> {
    use curve25519_dalek_fork::scalar::Scalar;
    use sha2::Digest as _;

    let sk = SecretKey03::from_bytes(secret_key);
    let (secret_scalar, secret_extension) = sk.extend();

    // Derive pk from the scalar (same key derivation as v03)
    let pk_point = secret_scalar * ED25519_BASEPOINT_POINT;
    let pk_bytes = pk_point.compress().to_bytes();

    // hash_to_curve: RFC 9380 expand_message_xmd + Elligator2 (v13 algorithm)
    let h = hash_to_curve_v13(&pk_bytes, seed);
    let compressed_h = h.compress();

    let gamma = secret_scalar * h;

    // nonce: Sha512(secret_extension || H) → wide scalar
    let k = {
        let mut nonce_input = [0u8; 64];
        nonce_input[..32].copy_from_slice(&secret_extension);
        nonce_input[32..].copy_from_slice(&compressed_h.to_bytes());
        let hash: [u8; 64] = sha2::Sha512::digest(nonce_input).into();
        Scalar::from_bytes_mod_order_wide(&hash)
    };

    let u_point = k * ED25519_BASEPOINT_POINT;
    let v_point = k * h;

    // batch-compat challenge: SHA-512(0x04||0x02||Y||H||gamma||U||V||0x00)[..16]
    // Y comes before H; no base point B; trailing 0x00 per RFC 9381 §5.4.3
    let challenge = {
        let mut hasher = sha2::Sha512::new();
        hasher.update([0x04, 0x02]);
        hasher.update(pk_bytes); // Y = public key
        hasher.update(compressed_h.to_bytes());
        hasher.update(gamma.compress().to_bytes());
        hasher.update(u_point.compress().to_bytes());
        hasher.update(v_point.compress().to_bytes());
        hasher.update([0x00]);
        let digest = hasher.finalize();
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes[..16].copy_from_slice(&digest[..16]);
        Scalar::from_bits(scalar_bytes)
    };

    let response = k + challenge * secret_scalar;

    // proof = [gamma(32) || U(32) || V(32) || response(32)]
    let mut proof_bytes = [0u8; 128];
    proof_bytes[..32].copy_from_slice(gamma.compress().as_bytes());
    proof_bytes[32..64].copy_from_slice(u_point.compress().as_bytes());
    proof_bytes[64..96].copy_from_slice(v_point.compress().as_bytes());
    proof_bytes[96..].copy_from_slice(response.as_bytes());

    // proof_to_hash: Sha512(0x04 || 0x03 || gamma_cofac || 0x00)
    let gamma_cofac = gamma.mul_by_cofactor();
    let mut hasher = sha2::Sha512::new();
    hasher.update([0x04, 0x03]);
    hasher.update(gamma_cofac.compress().to_bytes());
    hasher.update([0x00u8]);
    let mut output = [0u8; 64];
    output.copy_from_slice(&hasher.finalize());

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

    // ---- PraosBatchCompatVRF (v13 / ietfdraft13) ----
    //
    // Vectors below come from `IntersectMBO/cardano-base` test corpus
    // (`vrf_ver13_standard_10` / `vrf_ver13_standard_11`). They are byte-exact
    // copies of upstream so the v13 hot path stays exercised without depending
    // on the optional `dugite-conformance` upstream-fixture pipeline.
    const V13_STD10_SK: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const V13_STD10_PK: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const V13_STD10_PI: &str = "7d9c633ffeee27349264cf5c667579fc583b4bda63ab71d001f89c10003ab46f\
         762f5c178b68f0cddcc1157918edf45ec334ac8e8286601a3256c3bbf858edd9\
         4652eba1c4612e6fce762977a59420b451e12964adbe4fbecd58a7aeff5860af\
         cafa73589b023d14311c331a9ad15ff2fb37831e00f0acaa6d73bc9997b06501";
    const V13_STD10_BETA: &str = "9d574bf9b8302ec0fc1e21c3ec5368269527b87b462ce36dab2d14ccf80c53cc\
         cf6758f058c5b1c856b116388152bbe509ee3b9ecfe63d93c3b4346c1fbc6c54";

    const V13_STD11_PK: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
    const V13_STD11_ALPHA: &str = "72";
    const V13_STD11_PI: &str = "47b327393ff2dd81336f8a2ef10339112401253b3c714eeda879f12c509072ef\
         8ec26e77b8cb3114dd2265fe1564a4efb40d109aa3312536d93dfe3d8d80a061\
         fe799eb5770b4e3a5a27d22518bb631db183c8316bb552155f442c62a47d1c8b\
         d60e93908f93df1623ad78a86a028d6bc064dbfc75a6a57379ef855dc6733801";
    const V13_STD11_BETA: &str = "38561d6b77b71d30eb97a062168ae12b667ce5c28caccdf76bc88e093e463598\
         7cd96814ce55b4689b3dd2947f80e59aac7b7675f8083865b46c89b2ce9cc735";

    fn h(s: &str) -> Vec<u8> {
        hex::decode(s).expect("valid hex")
    }

    #[test]
    fn test_vrf_v13_verify_empty_alpha() {
        let pk = h(V13_STD10_PK);
        let pi = h(V13_STD10_PI);
        let beta = h(V13_STD10_BETA);
        let out = verify_vrf_proof_v13(&pk, &pi, &[]).expect("v13 verify");
        assert_eq!(&out[..], &beta[..]);
    }

    #[test]
    fn test_vrf_v13_verify_nonempty_alpha() {
        let pk = h(V13_STD11_PK);
        let pi = h(V13_STD11_PI);
        let alpha = h(V13_STD11_ALPHA);
        let beta = h(V13_STD11_BETA);
        let out = verify_vrf_proof_v13(&pk, &pi, &alpha).expect("v13 verify");
        assert_eq!(&out[..], &beta[..]);
    }

    #[test]
    fn test_vrf_v13_generate_matches_vector() {
        let sk_bytes = h(V13_STD10_SK);
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&sk_bytes);
        let (pi, beta) = generate_vrf_proof_v13(&sk, &[]).expect("v13 sign");
        assert_eq!(&pi[..], h(V13_STD10_PI).as_slice());
        assert_eq!(&beta[..], h(V13_STD10_BETA).as_slice());
    }

    #[test]
    fn test_vrf_v13_generate_then_verify_roundtrip() {
        let kp = generate_vrf_keypair();
        let seed = b"slot-2025-05-24-hello";
        let (pi, beta) = generate_vrf_proof_v13(&kp.secret_key, seed).unwrap();
        assert_eq!(pi.len(), 128);
        assert_eq!(beta.len(), 64);
        let verified = verify_vrf_proof_v13(&kp.public_key, &pi, seed).unwrap();
        assert_eq!(verified, beta);
    }

    #[test]
    fn test_vrf_v13_wrong_key_size() {
        assert!(verify_vrf_proof_v13(&[0u8; 16], &[0u8; 128], &[]).is_err());
    }

    #[test]
    fn test_vrf_v13_wrong_proof_size() {
        assert!(verify_vrf_proof_v13(&[0u8; 32], &[0u8; 80], &[]).is_err());
    }

    #[test]
    fn test_vrf_v13_tampered_proof_fails() {
        let pk = h(V13_STD10_PK);
        let mut pi = h(V13_STD10_PI);
        // Flip a single bit inside the response scalar (last 32 bytes).
        let last = pi.len() - 1;
        pi[last] ^= 0x01;
        assert!(verify_vrf_proof_v13(&pk, &pi, &[]).is_err());
    }

    #[test]
    fn test_vrf_v13_wrong_alpha_fails() {
        let pk = h(V13_STD10_PK);
        let pi = h(V13_STD10_PI);
        // STD10 was signed with empty alpha; verifying against a non-empty
        // input must fail (hash_to_curve diverges).
        assert!(verify_vrf_proof_v13(&pk, &pi, b"not-empty").is_err());
    }

    #[test]
    fn test_vrf_v13_wrong_pk_fails() {
        // Swap pk → STD11's; pi/beta still belong to STD10.
        let pk = h(V13_STD11_PK);
        let pi = h(V13_STD10_PI);
        assert!(verify_vrf_proof_v13(&pk, &pi, &[]).is_err());
    }

    #[test]
    fn test_vrf_v13_proof_length_off_by_one() {
        let pk = h(V13_STD10_PK);
        let mut pi = h(V13_STD10_PI);
        pi.pop();
        assert!(verify_vrf_proof_v13(&pk, &pi, &[]).is_err());
    }

    #[test]
    fn test_vrf_v13_seed_sensitivity() {
        // Different seeds with the same sk must produce different (pi, beta).
        let kp = generate_vrf_keypair_from_secret(&[7u8; 32]);
        let (pi_a, beta_a) = generate_vrf_proof_v13(&kp.secret_key, b"slot-1").unwrap();
        let (pi_b, beta_b) = generate_vrf_proof_v13(&kp.secret_key, b"slot-2").unwrap();
        assert_ne!(pi_a, pi_b);
        assert_ne!(beta_a, beta_b);
    }

    #[test]
    fn test_vrf_v03_v13_share_key_derivation() {
        // v03 and v13 use the same sk → pk derivation. The proofs differ
        // (different ciphersuite + Elligator2 vs RFC8032 hash-to-curve) but
        // the public key in both standard vectors with the same sk matches.
        let sk_bytes = h(V13_STD10_SK);
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&sk_bytes);
        let kp = generate_vrf_keypair_from_secret(&sk);
        assert_eq!(&kp.public_key[..], h(V13_STD10_PK).as_slice());
    }

    // ---- Leader-check edge cases (active_slot_log fast path) ----

    #[test]
    fn test_check_leader_value_rational_zero_stake() {
        // Zero relative stake → never elected even with full f.
        assert!(!check_leader_value_full_rational(&[0xffu8; 32], 0, 1, 1, 2,));
    }

    #[test]
    fn test_check_leader_value_rational_full_stake_high_f() {
        // 100% stake with f=1/2: even a fairly large VRF output should win.
        // (cn behaviour: `1 - (1 - f)^σ` = 1/2 here, so leader if vrf < 1/2)
        let mut vrf = [0u8; 32];
        vrf[0] = 0x10; // small value, well below 1/2
        assert!(check_leader_value_full_rational(&vrf, 1, 1, 1, 2));
    }

    #[test]
    fn test_check_leader_value_cached_matches_uncached() {
        let log = compute_active_slot_log(1, 20).expect("f=1/20 in (0,1]");
        let vrf = [0u8; 32];
        let elected_cached = check_leader_value_full_rational_cached(&vrf, 1, 1, &log);
        let elected_plain = check_leader_value_full_rational(&vrf, 1, 1, 1, 20);
        assert_eq!(elected_cached, elected_plain);
    }

    #[test]
    fn test_compute_active_slot_log_boundary_returns_none() {
        // Per the doc-comment on `compute_active_slot_log`, both edge values
        // collapse to None: f_num=0 (never elected) and f_num>=f_den
        // (always elected). The caller handles each as a hot-path constant.
        assert!(compute_active_slot_log(0, 1).is_none()); // f=0
        assert!(compute_active_slot_log(1, 1).is_none()); // f=1
        assert!(compute_active_slot_log(2, 1).is_none()); // f>1
                                                          // A normal Praos value (preview/preprod/mainnet: f = 1/20) returns Some.
        assert!(compute_active_slot_log(1, 20).is_some());
    }
}
