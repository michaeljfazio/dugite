//! Script-related Phase-1 validation helpers.
//!
//! This module provides:
//! - `evaluate_native_script` — recursive native script evaluation
//! - `collect_available_script_hashes` — build the set of script hashes that a
//!   transaction has made available (witness set + reference inputs)
//! - `compute_script_ref_hash` — canonical Blake2b-224 hash for a `ScriptRef`
//! - `check_script_data_hash` — Rule 12: script integrity hash validation
//! - Reference-script size/fee helpers used by the fee rule

use std::collections::{HashMap, HashSet};

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::{
    Certificate, GovAction, NativeScript, Rational, ScriptRef, Transaction, TransactionInput, Voter,
};
use dugite_primitives::value::Lovelace;
use tracing::debug;

use crate::utxo::UtxoLookup;

use super::ValidationError;

// ---------------------------------------------------------------------------
// Native script evaluation
// ---------------------------------------------------------------------------

/// Evaluate a native script given the set of key hashes that signed
/// the transaction and the transaction's own `ValidityInterval`.
///
/// This is the canonical recursive evaluator matching the Cardano ledger
/// specification for native scripts (Shelley multi-sig and Mary timelocks).
///
/// **Timelock semantics (issue #787)**: Haskell's `evalTimelock` evaluates
/// `RequireTimeStart`/`RequireTimeExpire` against the transaction body's
/// OWN `ValidityInterval` bounds (`invalid_before` / `invalid_hereafter`),
/// never against the current chain slot — `SNothing` (an unset bound)
/// always evaluates to `False`. `invalid_before` and `invalid_hereafter`
/// are exactly `body.validity_interval_start` and `body.ttl`.
///
/// **Dijkstra `RequireGuard`**: the per-tx `guards` set is NOT visible to
/// this entry point — pre-Dijkstra callers never see a `RequireGuard`
/// node, and Dijkstra-aware callers must use
/// [`evaluate_native_script_with_guards`] instead. A `RequireGuard`
/// encountered here is conservatively rejected (`false`) rather than
/// silently treated as satisfied; this is the defensive choice that
/// guarantees no false positives at apply time. Issue #475 Phase 3.5.
pub fn evaluate_native_script(
    script: &NativeScript,
    signers: &HashSet<Hash32>,
    invalid_before: Option<SlotNo>,
    invalid_hereafter: Option<SlotNo>,
) -> bool {
    evaluate_native_script_with_guards(
        script,
        signers,
        invalid_before,
        invalid_hereafter,
        &HashSet::new(),
    )
}

/// Dijkstra-aware native script evaluator. Identical to
/// [`evaluate_native_script`] for all pre-Dijkstra constructors, plus
/// support for `RequireGuard(cred)` which is satisfied iff
/// `cred ∈ satisfied_guards`.
///
/// Mirrors upstream `evalDijkstraNativeScript`:
///
/// ```haskell
/// RequireGuard cred -> cred `OSet.member` guards
/// ```
///
/// `satisfied_guards` is the post-witness-check projection: the subset
/// of the tx's declared `guards` (TxBody key 14) that the Dijkstra
/// witness pipeline has confirmed as satisfied. Issue #475 Phase 3.5.
///
/// `invalid_before` / `invalid_hereafter` are the transaction's own
/// `ValidityInterval` bounds (see [`evaluate_native_script`] doc for the
/// #787 rationale) — NOT the current chain slot.
pub fn evaluate_native_script_with_guards(
    script: &NativeScript,
    signers: &HashSet<Hash32>,
    invalid_before: Option<SlotNo>,
    invalid_hereafter: Option<SlotNo>,
    satisfied_guards: &HashSet<Credential>,
) -> bool {
    match script {
        NativeScript::ScriptPubkey(keyhash) => signers.contains(keyhash),
        NativeScript::ScriptAll(scripts) => scripts.iter().all(|s| {
            evaluate_native_script_with_guards(
                s,
                signers,
                invalid_before,
                invalid_hereafter,
                satisfied_guards,
            )
        }),
        NativeScript::ScriptAny(scripts) => scripts.iter().any(|s| {
            evaluate_native_script_with_guards(
                s,
                signers,
                invalid_before,
                invalid_hereafter,
                satisfied_guards,
            )
        }),
        NativeScript::ScriptNOfK(n, scripts) => {
            let count = scripts
                .iter()
                .filter(|s| {
                    evaluate_native_script_with_guards(
                        s,
                        signers,
                        invalid_before,
                        invalid_hereafter,
                        satisfied_guards,
                    )
                })
                .count();
            count >= *n as usize
        }
        // `RequireTimeStart lockStart` succeeds iff `txStart = SJust s ∧
        // lockStart <= s`; `SNothing` ⇒ False. Never the application slot.
        NativeScript::InvalidBefore(lock_start) => invalid_before.is_some_and(|s| *lock_start <= s),
        // `RequireTimeExpire lockExp` succeeds iff `txExp = SJust e ∧
        // e <= lockExp`; `SNothing` ⇒ False. Never the application slot.
        NativeScript::InvalidHereafter(lock_exp) => {
            invalid_hereafter.is_some_and(|e| e <= *lock_exp)
        }
        NativeScript::RequireGuard(cred) => satisfied_guards.contains(cred),
    }
}

// ---------------------------------------------------------------------------
// Script hash utilities
// ---------------------------------------------------------------------------

/// Compute the native-script hash: `blake2b_224(0x00 || original_bytes)`.
///
/// Haskell `hashScript` hashes over the script's ORIGINAL wire bytes (the Timelock
/// `MemoBytes`), never a canonical re-encode — so a non-canonically-but-validly
/// encoded native script (indefinite-length outer array, non-minimal integer field)
/// hashes differently from `encode_native_script(decoded)`. When `original` is
/// available (decoded transactions), hash over it; fall back to a re-encode only for
/// locally-constructed scripts whose wire bytes we never had. See issue #862.
///
/// Shared by [`compute_script_ref_hash`]'s `NativeScript` arm, Phase-1 Rule 13
/// (`phase1.rs`), and [`check_extraneous_script_witnesses`]'s native-witness
/// `received` set (issue #791).
/// Original wire bytes of every witness native script, indexed, recovered from the
/// transaction's raw witness-set CBOR. `None` when the raw CBOR is absent (locally
/// constructed tx) — callers then fall back to a re-encode via [`native_script_hash`].
/// Every site that hashes a witness native script for matching must use this so the
/// hash is byte-identical across the whole tx (only non-canonically-encoded scripts
/// actually differ from a re-encode). See issue #862.
pub(crate) fn witness_native_original_bytes(tx: &Transaction) -> Option<Vec<Vec<u8>>> {
    tx.raw_witness_cbor
        .as_deref()
        .and_then(dugite_serialization::witness_native_script_original_bytes)
}

/// Original inner bytes of a reference NATIVE script carried in `utxo`, recovered
/// from the output's raw CBOR. `None` for Plutus refs / legacy outputs / absent raw.
pub(crate) fn reference_native_original_bytes(
    utxo: &dugite_primitives::transaction::TransactionOutput,
) -> Option<Vec<u8>> {
    utxo.raw_cbor
        .as_deref()
        .and_then(dugite_serialization::reference_native_script_original_bytes)
}

pub(crate) fn native_script_hash(ns: &NativeScript, original: Option<&[u8]>) -> Hash28 {
    let reencoded;
    let script_cbor: &[u8] = match original {
        Some(bytes) => bytes,
        None => {
            reencoded = dugite_serialization::encode_native_script(ns);
            &reencoded
        }
    };
    let mut tagged = Vec::with_capacity(1 + script_cbor.len());
    tagged.push(0x00);
    tagged.extend_from_slice(script_cbor);
    dugite_primitives::hash::blake2b_224(&tagged)
}

/// Compute the canonical script hash for a reference script.
///
/// Per the Cardano spec, the hash is `blake2b_224(type_tag || script_bytes)`:
/// - `0x00` — native script (with the script CBOR-encoded)
/// - `0x01` — Plutus V1
/// - `0x02` — Plutus V2
/// - `0x03` — Plutus V3
/// - `0x04` — Plutus V4 (Dijkstra, issue #475 Phase 5)
pub(crate) fn compute_script_ref_hash(
    script_ref: &ScriptRef,
    native_original: Option<&[u8]>,
) -> Hash28 {
    match script_ref {
        ScriptRef::NativeScript(ns) => native_script_hash(ns, native_original),
        ScriptRef::PlutusV1(bytes) => {
            let mut tagged = Vec::with_capacity(1 + bytes.len());
            tagged.push(0x01);
            tagged.extend_from_slice(bytes);
            dugite_primitives::hash::blake2b_224(&tagged)
        }
        ScriptRef::PlutusV2(bytes) => {
            let mut tagged = Vec::with_capacity(1 + bytes.len());
            tagged.push(0x02);
            tagged.extend_from_slice(bytes);
            dugite_primitives::hash::blake2b_224(&tagged)
        }
        ScriptRef::PlutusV3(bytes) => {
            let mut tagged = Vec::with_capacity(1 + bytes.len());
            tagged.push(0x03);
            tagged.extend_from_slice(bytes);
            dugite_primitives::hash::blake2b_224(&tagged)
        }
        ScriptRef::PlutusV4(bytes) => {
            // Dijkstra-only hash prefix `0x04` (issue #475 Phase 5).
            let mut tagged = Vec::with_capacity(1 + bytes.len());
            tagged.push(0x04);
            tagged.extend_from_slice(bytes);
            dugite_primitives::hash::blake2b_224(&tagged)
        }
    }
}

/// Collect all available script hashes from the transaction's witness set and
/// from UTxOs reachable by both spending inputs and reference inputs.
///
/// Matches Haskell's `scriptsProvided` which is defined over
/// `inputs txb <> referenceInputs txb`.  A script_ref on a *spending* input's
/// UTxO is as valid a source of a script witness as one on a reference input.
///
/// Used for witness completeness checks (Rule 9b) and minting policy checks
/// (Rule 3c).
pub(super) fn collect_available_script_hashes(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
) -> HashSet<Hash28> {
    let mut hashes = HashSet::new();

    // Native scripts: blake2b_224(0x00 || original_bytes). Hash over the ORIGINAL
    // wire bytes of each witness native script (Haskell hashScript over MemoBytes),
    // recovered from the witness set's raw CBOR; fall back to a re-encode only when
    // the raw CBOR is absent (locally-constructed tx). See issue #862.
    let witness_native_raws = tx
        .raw_witness_cbor
        .as_deref()
        .and_then(dugite_serialization::witness_native_script_original_bytes);
    for (i, script) in tx.witness_set.native_scripts.iter().enumerate() {
        let original = witness_native_raws
            .as_ref()
            .and_then(|v| v.get(i))
            .map(Vec::as_slice);
        hashes.insert(native_script_hash(script, original));
    }

    // Plutus V1: blake2b_224(0x01 || script_bytes)
    for s in &tx.witness_set.plutus_v1_scripts {
        let mut tagged = Vec::with_capacity(1 + s.len());
        tagged.push(0x01);
        tagged.extend_from_slice(s);
        hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
    }

    // Plutus V2: blake2b_224(0x02 || script_bytes)
    for s in &tx.witness_set.plutus_v2_scripts {
        let mut tagged = Vec::with_capacity(1 + s.len());
        tagged.push(0x02);
        tagged.extend_from_slice(s);
        hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
    }

    // Plutus V3: blake2b_224(0x03 || script_bytes)
    for s in &tx.witness_set.plutus_v3_scripts {
        let mut tagged = Vec::with_capacity(1 + s.len());
        tagged.push(0x03);
        tagged.extend_from_slice(s);
        hashes.insert(dugite_primitives::hash::blake2b_224(&tagged));
    }

    // Reference scripts from spending inputs AND reference inputs.
    //
    // Haskell's `scriptsProvided` is computed over `inputs txb <> referenceInputs txb`,
    // meaning a script_ref attached to a *spending* input UTxO is also available as a
    // witness for minting policies and script-locked inputs (Rule 9b). This matches the
    // Cardano ledger's `Cardano.Ledger.Alonzo.Tx.ScriptsProvided` definition.
    for inp in tx.body.inputs.iter().chain(tx.body.reference_inputs.iter()) {
        if let Some(utxo) = utxo_set.lookup(inp) {
            if let Some(script_ref) = &utxo.script_ref {
                // For a reference NATIVE script, hash over its original inner bytes
                // recovered from the output's raw CBOR (#862); Plutus refs already
                // carry their raw bytes so `native_original` is ignored for them.
                let native_original = utxo
                    .raw_cbor
                    .as_deref()
                    .and_then(dugite_serialization::reference_native_script_original_bytes);
                hashes.insert(compute_script_ref_hash(
                    script_ref,
                    native_original.as_deref(),
                ));
            }
        }
    }

    hashes
}

// ---------------------------------------------------------------------------
// Reference script size + tiered fee (Conway ledger spec)
// ---------------------------------------------------------------------------

/// Calculate the total byte size of reference scripts from all UTxOs touched by a
/// transaction — both spending inputs and reference inputs.
///
/// Matches Haskell's `txNonDistinctRefScriptsSize` from
/// `Cardano.Ledger.Conway.Tx` (CIP-0112), which iterates over
/// `(inputs txb <> referenceInputs txb)` and sums `originalBytesSize` for every
/// UTxO that carries a `script_ref`.  The count is **non-distinct** — if the same
/// script hash appears in multiple UTxOs it is counted each time.
///
/// The `inputs` and `reference_inputs` slices are provided separately so callers can
/// supply the exact transaction body fields without allocating a merged vector.
/// Pass an empty slice for either argument if only one set is applicable (e.g. the
/// block-level pre-scan handles its own overlay and may call this differently).
///
/// # Within-block visibility
///
/// When called from `compute_min_fee` inside `validate_transaction`, `utxo_set` is
/// `&self.utxo_set` which already contains UTxOs created by all prior transactions in
/// the same block (applied sequentially by `apply_block`).  No separate overlay is
/// needed for the per-transaction fee check path.
pub(crate) fn calculate_ref_script_size(
    inputs: &[TransactionInput],
    reference_inputs: &[TransactionInput],
    utxo_set: &dyn UtxoLookup,
) -> u64 {
    // Haskell: `inputs txb `Set.union` referenceInputs txb` — a TxIn present
    // in BOTH the spending inputs and the reference inputs is only counted
    // ONCE. `.chain()` is concatenation, not set union, and double-counts a
    // shared TxIn's script_ref bytes (issue #788); dedup into a set first.
    let mut seen: HashSet<&TransactionInput> = HashSet::new();
    let mut total_size: u64 = 0;
    for inp in inputs.iter().chain(reference_inputs.iter()) {
        if !seen.insert(inp) {
            continue;
        }
        if let Some(utxo) = utxo_set.lookup(inp) {
            if let Some(script_ref) = &utxo.script_ref {
                total_size = total_size.saturating_add(script_ref_byte_size(script_ref));
            }
        }
    }
    total_size
}

/// Return the byte size of a single reference script.
pub(crate) fn script_ref_byte_size(script_ref: &ScriptRef) -> u64 {
    match script_ref {
        ScriptRef::NativeScript(ns) => dugite_serialization::encode_native_script(ns).len() as u64,
        // V4 byte sizing mirrors V1/V2/V3 — raw program byte length.
        ScriptRef::PlutusV1(bytes)
        | ScriptRef::PlutusV2(bytes)
        | ScriptRef::PlutusV3(bytes)
        | ScriptRef::PlutusV4(bytes) => bytes.len() as u64,
    }
}

/// Conway ledger tiered reference script fee calculation.
///
/// Divides the total script size into 25 KiB tiers, applying a 1.2× multiplier
/// per tier. The result is the **floor** of the exact rational sum — matching
/// Haskell `tierRefScriptFee` (`Coin $ floor (acc + toRational n * curTierPrice)`,
/// a single `floor` applied to the exact `Data.Ratio.Rational` accumulator).
///
/// # Algorithm: scaled-integer accumulation
///
/// Naive rational accumulation (`acc_num / acc_den`) overflows u128 beyond
/// tier ~25 because the cross-product denominator grows as `5^(n*(n-1)/2)`.
/// GCD reduction is insufficient for `base_fee_per_byte` values not divisible
/// by 5 (e.g., base = 1).
///
/// This implementation avoids cross-products entirely by separating each tier's
/// contribution into an integer part and a fractional remainder that is scaled
/// to a common denominator known at entry:
///
/// 1. Pre-count tiers: `k = ceil(total_size / 25600)`.
/// 2. Set common denominator `denom = 5^(k-1)`.
///    At k=41 (the 1 MiB cap), `denom = 5^40 ≈ 9.1×10²⁷ < u128::MAX`.
/// 3. Per tier `i` with `chunk` bytes:
///    - `contribution = chunk * price_num`  (price = base * (6/5)^i, GCD-reduced)
///    - `whole = contribution / price_den`  — exact integer quotient
///    - `tier_rem = contribution % price_den`
///    - `scaled_rem = tier_rem * (denom / price_den)` — always < denom since
///      `price_den` always divides `denom` (both are powers of 5)
/// 4. Accumulate: `acc_whole += whole`, `frac_scaled += scaled_rem`.
///    Drain any whole units from `frac_scaled` into `acc_whole` when
///    `frac_scaled >= denom`.
/// 5. Single floor: `fee = acc_whole` (the sub-unit `frac_scaled/denom` is
///    discarded). For a rational base `num/den` the final result is instead
///    `floor((acc_whole·denom + frac_scaled) / (denom·den))` — see
///    [`calculate_ref_script_tiered_fee_rational`].
///
/// # Overflow proofs (within the 1 MiB cap)
///
/// - `denom = 5^40 < 10^28 < u128::MAX` ✓
/// - `scaled_rem < denom < u128::MAX` per iteration ✓
/// - `frac_scaled < 41 * denom < 4 × 10^29 < u128::MAX` ✓ (41 tiers × denom)
/// - `chunk * price_num`: `price_num ≤ base * 6^40`. For realistic protocol
///   params (base ≤ 10^9 lovelace/byte), `chunk * price_num ≤ 25600 × 10^9 ×
///   2.23×10^31 ≈ 5.7×10^44` — would overflow for very large `base`.  All
///   multiplications therefore use `checked_mul`; if they overflow (unreachable
///   with realistic params), the function saturates to `u64::MAX`.
///
/// # Inputs beyond the cap
///
/// For `total_size > MAX_REF_SCRIPT_SIZE_TIER_CAP` (1 MiB), the function
/// short-circuits immediately with `u64::MAX`. Such inputs are already rejected
/// by the Conway block-body rule before fee calculation is invoked in
/// production.
// Integer-base convenience wrapper retained for the fee fixtures (which exercise
// the byte-exact integer path); production callers use the rational variant.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn calculate_ref_script_tiered_fee(base_fee_per_byte: u64, total_size: u64) -> u64 {
    // Integer base = the rational base_fee_per_byte/1.
    calculate_ref_script_tiered_fee_rational(base_fee_per_byte, 1, total_size)
}

/// Conway tiered reference-script fee with a **rational** per-byte base price.
///
/// Mirrors cardano-ledger `tierRefScriptFee multiplier sizeIncrement baseFeePerByte
/// size` where `baseFeePerByte = unboundRational ppMinFeeRefScriptCostPerByte` is a
/// full `Rational` carried through an exact rational accumulator, with a **single
/// `floor`** applied to the final sum (never per-tier).
///
/// Byte-exactness: the accumulator builds the exact sum
/// `S = Σ_i chunk_i · base_num · (6/5)^i` (numerator-only, identical to the
/// historical integer-base computation), giving `S = acc_whole + frac_scaled/denom`.
/// The true fee for base `base_num/base_den` is then
/// `floor(S / base_den) = floor((acc_whole·denom + frac_scaled) / (denom·base_den))`.
/// For `base_den == 1` this reduces to `acc_whole` — bit-identical to the prior
/// integer-only result, so every existing fixture is preserved exactly.
pub(super) fn calculate_ref_script_tiered_fee_rational(
    base_num: u64,
    base_den: u64,
    total_size: u64,
) -> u64 {
    const TIER_SIZE: u64 = 25_600; // 25 KiB (= 25 * 1024)

    // Inputs beyond the Conway 1 MiB block-body limit are rejected before this
    // function is called in production.  Saturate immediately so no floating-
    // point arithmetic is ever needed for out-of-range inputs.
    if total_size > MAX_REF_SCRIPT_SIZE_TIER_CAP {
        return u64::MAX;
    }
    // A zero base price (num == 0) or zero den (degenerate/never produced by the
    // decoder, which rejects den == 0) yields no fee.
    if total_size == 0 || base_num == 0 || base_den == 0 {
        return 0;
    }
    let base_fee_per_byte = base_num;

    // Pre-count tiers: ceil(total_size / TIER_SIZE).
    let k = total_size.div_ceil(TIER_SIZE);

    // Common denominator for all tier fractional parts: 5^(k-1).
    // price_den at tier i = 5^i / gcd(base, 5^i), which always divides 5^(k-1)
    // (since k-1 >= i), so scale_factor = denom / price_den is always exact.
    let denom: u128 = pow5(k - 1); // 5^0 = 1 when k = 1 (single tier)

    // Accumulated integer part of the sum.
    let mut acc_whole: u128 = 0;
    // Accumulated fractional part, scaled by `denom` (i.e., frac_scaled/denom ∈ [0,1)).
    let mut frac_scaled: u128 = 0;

    // Current tier price as exact rational price_num / price_den.
    // Tier 0: base/1.  Each tier multiplies by 6/5; GCD is reduced immediately.
    let mut price_num: u128 = base_fee_per_byte as u128;
    let mut price_den: u128 = 1;

    let mut remaining = total_size;

    while remaining > 0 {
        let chunk = remaining.min(TIER_SIZE) as u128;

        // Tier contribution = chunk * price_num / price_den.
        // checked_mul guards against overflow for very large base_fee_per_byte.
        let contribution = match chunk.checked_mul(price_num) {
            Some(v) => v,
            None => return u64::MAX,
        };
        // Exact integer quotient and remainder.
        let whole = contribution / price_den;
        let tier_rem = contribution % price_den; // in [0, price_den)

        // Accumulate integer part.
        acc_whole = match acc_whole.checked_add(whole) {
            Some(v) => v,
            None => return u64::MAX,
        };

        // Scale the fractional remainder to the common denominator.
        // scale_factor = denom / price_den is always a whole number because
        // price_den (a power of 5, after GCD reduction) divides denom = 5^(k-1).
        // scaled_rem < price_den * scale_factor = denom, so no overflow.
        let scale_factor = denom / price_den;
        let scaled_rem = match tier_rem.checked_mul(scale_factor) {
            Some(v) => v,
            None => return u64::MAX, // unreachable: scaled_rem < denom < u128::MAX
        };
        frac_scaled = match frac_scaled.checked_add(scaled_rem) {
            Some(v) => v,
            None => return u64::MAX, // unreachable: sum < 41*denom < 4e29 < u128::MAX
        };

        // Carry any whole units that accumulated in the fractional bucket.
        // This happens when multiple tiers each contribute close to 1 fractional unit.
        if frac_scaled >= denom {
            let carry = frac_scaled / denom;
            frac_scaled %= denom;
            acc_whole = match acc_whole.checked_add(carry) {
                Some(v) => v,
                None => return u64::MAX,
            };
        }

        remaining -= chunk as u64;

        // Advance price: multiply by 6/5 and immediately GCD-reduce to keep
        // price_num and price_den as small as possible, and to preserve the
        // invariant that price_den divides denom.
        price_num = match price_num.checked_mul(6) {
            Some(p) => p,
            None => return u64::MAX,
        };
        price_den = match price_den.checked_mul(5) {
            Some(p) => p,
            None => return u64::MAX,
        };
        let g = gcd_u128(price_num, price_den);
        price_num /= g;
        price_den /= g;
    }

    // Haskell's tierRefScriptFee applies a single `floor` to the final sum.
    // The accumulator holds the exact value S = acc_whole + frac_scaled/denom
    // for the *numerator-only* base price.  The true fee for base price
    // base_num/base_den is floor(S / base_den):
    //   floor((acc_whole·denom + frac_scaled) / (denom·base_den)).
    let total = if base_den == 1 {
        // frac_scaled < denom, so floor(S) == acc_whole.  Bit-identical to the
        // historical integer-base result; no extra arithmetic on the hot path.
        acc_whole
    } else {
        let numer = match acc_whole
            .checked_mul(denom)
            .and_then(|x| x.checked_add(frac_scaled))
        {
            Some(v) => v,
            None => return u64::MAX,
        };
        let den = match denom.checked_mul(base_den as u128) {
            Some(v) => v,
            None => return u64::MAX,
        };
        numer / den // exact floor division
    };
    // Saturate to u64::MAX if the fee exceeds u64 range (only possible for
    // unrealistically large base prices).
    u64::try_from(total).unwrap_or(u64::MAX)
}

/// Compute `5^n` exactly as a u128.
///
/// Used by [`calculate_ref_script_tiered_fee`] to build the common denominator
/// `denom = 5^(k-1)`.  At k=41 (the 1 MiB block cap), `5^40 ≈ 9.1×10²⁷`,
/// safely within u128 range.  `5^54 ≈ 5.6×10³⁷ < u128::MAX`; `5^55` overflows.
#[inline]
fn pow5(n: u64) -> u128 {
    let mut result: u128 = 1;
    for _ in 0..n {
        result = result
            .checked_mul(5)
            .expect("pow5: result overflows u128 — n must not exceed 54");
    }
    result
}

/// Upper bound on `total_size` for [`calculate_ref_script_tiered_fee`].
///
/// Any input exceeding this cap causes the function to immediately return
/// `u64::MAX` — the transaction is rejected regardless of the precise fee.
///
/// This value equals the Conway `maxRefScriptSizePerBlock` hard limit (1 MiB)
/// which is not a governance-updatable protocol parameter.  Exposed as
/// `pub(crate)` so that `apply.rs` can reuse the same constant for the
/// block-body check, keeping the two in sync.
pub(crate) const MAX_REF_SCRIPT_SIZE_TIER_CAP: u64 = 1024 * 1024; // 1 MiB

fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    if a == 0 {
        1
    } else {
        a
    }
}

// ---------------------------------------------------------------------------
// Output value size estimation (used by Rule 5a)
// ---------------------------------------------------------------------------

/// Compute the EXACT CBOR-encoded size of a `Value`.
///
/// Matches Haskell `validateOutputTooBigUTxO`'s `serSize = BSL.length
/// (serialize v)` byte-exactly by serializing the value with dugite's own
/// CBOR encoder and taking the encoded length — rather than a hand-rolled
/// header-size estimate. The prior estimate under-counted the 28-byte
/// policy-ID bytestring header (2 bytes, not 1), map headers with >= 24
/// entries, and asset names >= 24 bytes, which let a maliciously-sized
/// multi-asset output slip under `maxValSize` (issue #793).
pub(super) fn estimate_value_cbor_size(value: &dugite_primitives::value::Value) -> u64 {
    dugite_serialization::encode_value(value).len() as u64
}

/// Estimate the CBOR encoding size of an unsigned integer.
///
/// No longer used by [`estimate_value_cbor_size`] (issue #793 replaced the
/// hand-rolled estimate with the exact serialized length), but retained —
/// and still exercised — as a standalone regression-guard unit under test.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn cbor_uint_size(value: u64) -> u64 {
    if value < 24 {
        1
    } else if value <= 0xFF {
        2
    } else if value <= 0xFFFF {
        3
    } else if value <= 0xFFFF_FFFF {
        5
    } else {
        9
    }
}

// ---------------------------------------------------------------------------
// Script data hash (Rule 12)
// ---------------------------------------------------------------------------

/// Check the script integrity hash (Rule 12).
///
/// If the transaction has redeemers or Plutus data, `script_data_hash` must be
/// set and match the computed value (this covers the "supplemental datums
/// only, no redeemers/scripts" case too — see [`has_plutus_scripts`]/its
/// caller, issue #790). If neither redeemers nor datums are present but
/// `script_data_hash` is set anyway, it is ALWAYS `UnexpectedScriptDataHash`
/// — there is no reference-script carve-out (issue #790): a `script_ref`
/// only contributes to `langViews` when actually invoked via a redeemer.
///
/// On success the function also returns whether Phase-2 Plutus evaluation is
/// needed (i.e. `has_redeemers`).
pub(super) fn check_script_data_hash(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    errors: &mut Vec<ValidationError>,
) {
    let body = &tx.body;
    let has_redeemers = !tx.witness_set.redeemers.is_empty();
    let has_datums = !tx.witness_set.plutus_data.is_empty();

    if has_redeemers || has_datums {
        if let Some(declared_hash) = &body.script_data_hash {
            // Determine which Plutus language versions are used.
            // Per Haskell mkScriptIntegrity: intersect scriptsProvided with
            // scriptsNeeded to determine the set of language versions that
            // contribute to the hash.
            let mut has_v1 = !tx.witness_set.plutus_v1_scripts.is_empty();
            let mut has_v2 = !tx.witness_set.plutus_v2_scripts.is_empty();
            let mut has_v3 = !tx.witness_set.plutus_v3_scripts.is_empty();

            // 1. Collect needed script hashes (spending inputs, minting, withdrawals, certs, votes)
            let mut scripts_needed: HashSet<Hash28> = HashSet::new();
            for input in &body.inputs {
                if let Some(utxo) = utxo_set.lookup(input) {
                    let ab = utxo.address.to_bytes();
                    if !ab.is_empty() {
                        let t = (ab[0] >> 4) & 0x0F;
                        // Script address types: 1,3,5,7 (bit 4 of header = 1)
                        if matches!(t, 1 | 3 | 5 | 7) && ab.len() >= 29 {
                            if let Ok(h) = Hash28::try_from(&ab[1..29]) {
                                scripts_needed.insert(h);
                            }
                        }
                    }
                }
            }
            for policy_id in body.mint.keys() {
                scripts_needed.insert(*policy_id);
            }
            for reward_addr in body.withdrawals.keys() {
                if reward_addr.len() >= 29 {
                    let header = reward_addr[0];
                    // Reward address type: 0xF0/0xF1 = script
                    if (header & 0x10) != 0 {
                        if let Ok(h) = Hash28::try_from(&reward_addr[1..29]) {
                            scripts_needed.insert(h);
                        }
                    }
                }
            }
            // Certificates with script credentials
            use dugite_primitives::credentials::Credential as Cred;
            for cert in &body.certificates {
                let cred: Option<&Cred> = match cert {
                    Certificate::StakeDeregistration(c) => Some(c),
                    Certificate::StakeDelegation { credential: c, .. } => Some(c),
                    Certificate::ConwayStakeRegistration { credential: c, .. } => Some(c),
                    Certificate::ConwayStakeDeregistration { credential: c, .. } => Some(c),
                    Certificate::VoteDelegation { credential: c, .. } => Some(c),
                    Certificate::StakeVoteDelegation { credential: c, .. } => Some(c),
                    Certificate::RegStakeDeleg { credential: c, .. } => Some(c),
                    Certificate::RegStakeVoteDeleg { credential: c, .. } => Some(c),
                    Certificate::VoteRegDeleg { credential: c, .. } => Some(c),
                    Certificate::CommitteeHotAuth {
                        cold_credential: c, ..
                    } => Some(c),
                    Certificate::CommitteeColdResign {
                        cold_credential: c, ..
                    } => Some(c),
                    Certificate::RegDRep { credential: c, .. } => Some(c),
                    Certificate::UnregDRep { credential: c, .. } => Some(c),
                    Certificate::UpdateDRep { credential: c, .. } => Some(c),
                    _ => None,
                };
                if let Some(Cred::Script(h)) = cred {
                    scripts_needed.insert(*h);
                }
            }
            // Voting procedures: DRep and CC voter script credentials
            for voter in body.voting_procedures.keys() {
                let cred: Option<&Cred> = match voter {
                    Voter::DRep(c) => Some(c),
                    Voter::ConstitutionalCommittee(c) => Some(c),
                    Voter::StakePool(_) => None,
                };
                if let Some(Cred::Script(h)) = cred {
                    scripts_needed.insert(*h);
                }
            }
            // Proposal procedures: guardrail script hashes
            for proposal in &body.proposal_procedures {
                match &proposal.gov_action {
                    GovAction::ParameterChange {
                        policy_hash: Some(h),
                        ..
                    }
                    | GovAction::TreasuryWithdrawals {
                        policy_hash: Some(h),
                        ..
                    } => {
                        scripts_needed.insert(*h);
                    }
                    _ => {}
                }
            }

            // 2. Collect provided scripts with their version tag
            let mut scripts_provided: HashMap<Hash28, u8> = HashMap::new();
            for s in &tx.witness_set.plutus_v1_scripts {
                let h = dugite_primitives::hash::blake2b_224_tagged(1, s);
                scripts_provided.insert(h, 1);
            }
            for s in &tx.witness_set.plutus_v2_scripts {
                let h = dugite_primitives::hash::blake2b_224_tagged(2, s);
                scripts_provided.insert(h, 2);
            }
            for s in &tx.witness_set.plutus_v3_scripts {
                let h = dugite_primitives::hash::blake2b_224_tagged(3, s);
                scripts_provided.insert(h, 3);
            }
            for input in body.inputs.iter().chain(body.reference_inputs.iter()) {
                if let Some(utxo) = utxo_set.lookup(input) {
                    let (tag, bytes) = match &utxo.script_ref {
                        Some(ScriptRef::PlutusV1(s)) => (1u8, s.as_slice()),
                        Some(ScriptRef::PlutusV2(s)) => (2, s.as_slice()),
                        Some(ScriptRef::PlutusV3(s)) => (3, s.as_slice()),
                        _ => continue,
                    };
                    let h = dugite_primitives::hash::blake2b_224_tagged(tag, bytes);
                    scripts_provided.insert(h, tag);
                }
            }

            // 3. Intersect: only USED scripts determine the language set
            for (hash, version) in &scripts_provided {
                if scripts_needed.contains(hash) {
                    match version {
                        1 => has_v1 = true,
                        2 => has_v2 = true,
                        3 => has_v3 = true,
                        _ => {}
                    }
                }
            }

            // Per Haskell `Cardano.Ledger.Alonzo.UTxOW.scriptsNeeded` the
            // language set is determined solely by the
            // scriptsNeeded ∩ scriptsProvided hash intersection.  If a
            // needed script-hash is missing from the provided set the tx is
            // invalid (`MissingScriptWitnessesUTXOW`); there is no fallback
            // path that ignores the hash check and trusts the on-chain
            // `script_ref` tag.  Surface the empty-intersection case so a
            // CBOR encoding bug elsewhere doesn't silently degrade to a
            // wrong cost-model integrity hash.
            if !has_v1 && !has_v2 && !has_v3 && has_redeemers && !scripts_needed.is_empty() {
                debug!(
                    needed_count = scripts_needed.len(),
                    provided_count = scripts_provided.len(),
                    needed = ?scripts_needed.iter().map(|h| h.to_hex()).collect::<Vec<_>>(),
                    provided = ?scripts_provided.keys().map(|h| h.to_hex()).collect::<Vec<_>>(),
                    "scriptsNeeded ∩ scriptsProvided is empty despite redeemers — \
                     downstream script_data_hash check will reject the tx (issue #633)"
                );
            }

            // Compute the expected script_data_hash. When raw tx CBOR is
            // available we use the in-house KeepRaw to preserve the original encoding
            // of redeemers and datums exactly.
            //
            // The empty/absent-redeemers term in the script-integrity preimage is
            // era-dependent: Alonzo/Babbage encode a LIST (empty = `0x80`), Conway
            // a MAP (empty = `0xa0`). Conway is protocol major 9+. Getting this
            // wrong breaks supplemental-datum-only txs (no redeemers) on pre-Conway
            // blocks — the ScriptDataHashMismatch class seen during from-genesis
            // replay of Babbage-era preview/preprod history.
            let redeemers_map_form = params.protocol_version_major >= 9;
            let computed = if let Some(raw) = tx.raw_cbor.as_ref() {
                dugite_serialization::compute_script_data_hash_from_cbor(
                    raw,
                    &params.cost_models,
                    has_v1,
                    has_v2,
                    has_v3,
                    redeemers_map_form,
                )
                .unwrap_or_else(|| {
                    dugite_serialization::compute_script_data_hash(
                        &tx.witness_set.redeemers,
                        &tx.witness_set.plutus_data,
                        &params.cost_models,
                        has_v1,
                        has_v2,
                        has_v3,
                        tx.witness_set.raw_redeemers_cbor.as_deref(),
                        tx.witness_set.raw_plutus_data_cbor.as_deref(),
                        redeemers_map_form,
                    )
                })
            } else {
                dugite_serialization::compute_script_data_hash(
                    &tx.witness_set.redeemers,
                    &tx.witness_set.plutus_data,
                    &params.cost_models,
                    has_v1,
                    has_v2,
                    has_v3,
                    tx.witness_set.raw_redeemers_cbor.as_deref(),
                    tx.witness_set.raw_plutus_data_cbor.as_deref(),
                    redeemers_map_form,
                )
            };

            if *declared_hash != computed {
                errors.push(ValidationError::ScriptDataHashMismatch {
                    expected: declared_hash.to_hex(),
                    actual: computed.to_hex(),
                });
            }
        } else {
            errors.push(ValidationError::MissingScriptDataHash);
        }
    } else if body.script_data_hash.is_some()
        && tx.witness_set.plutus_v1_scripts.is_empty()
        && tx.witness_set.plutus_v2_scripts.is_empty()
        && tx.witness_set.plutus_v3_scripts.is_empty()
    {
        // Issue #790: a declared `script_data_hash` with EMPTY redeemers,
        // langViews, AND datums must ALWAYS be `UnexpectedScriptDataHash` —
        // there is no ref-script carve-out. Per Haskell `mkScriptIntegrity`,
        // a reference script only enters `langViews` when it is actually
        // INVOKED via a redeemer (`has_redeemers` above); merely spending or
        // referencing a script_ref-carrying UTxO contributes nothing to the
        // script-integrity hash. The previous carve-out (skip the error
        // whenever ANY touched UTxO carried a script_ref, even unused)
        // let an adversary attach a junk `script_data_hash` to a vkey-only
        // tx that happens to reference a script_ref UTxO — over-accepting a
        // tx/block Haskell rejects with `PPViewHashesDontMatch`.
        errors.push(ValidationError::UnexpectedScriptDataHash);
    }
}

/// Check for extraneous script witnesses (Haskell `babbageMissingScripts` /
/// `ExtraneousScriptWitnessesUTXOW`).
///
/// `extra = received \ (needed ∪ refs)` where `received = Map.keysSet
/// scriptTxWits` is Haskell's set of ALL witness scripts — native included,
/// not just Plutus (issue #791). Witness scripts not needed by any script
/// purpose (after accounting for reference scripts) are reported. A script
/// provided as a witness that is only needed via a reference script IS
/// considered extraneous.
pub(super) fn check_extraneous_script_witnesses(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    errors: &mut Vec<ValidationError>,
) {
    // Only check when the transaction has ANY witness scripts — native
    // scripts count too (issue #791); a native-only witness set must not
    // early-return, or an unneeded `ScriptPubkey` witness would never be
    // flagged.
    let has_witness_scripts = !tx.witness_set.plutus_v1_scripts.is_empty()
        || !tx.witness_set.plutus_v2_scripts.is_empty()
        || !tx.witness_set.plutus_v3_scripts.is_empty()
        || !tx.witness_set.native_scripts.is_empty();
    if !has_witness_scripts {
        return;
    }

    let body = &tx.body;

    // 1. Witness script hashes ("received" — ALL scripts, native included).
    //    Native scripts hash over their ORIGINAL wire bytes (#862) so a
    //    non-canonically-encoded witness script matches the same hash the "needed"
    //    set derives from the on-chain address/policy.
    let mut witness_hashes: HashSet<Hash28> = HashSet::new();
    let witness_native_raws = witness_native_original_bytes(tx);
    for (i, ns) in tx.witness_set.native_scripts.iter().enumerate() {
        let original = witness_native_raws
            .as_ref()
            .and_then(|v| v.get(i))
            .map(Vec::as_slice);
        witness_hashes.insert(native_script_hash(ns, original));
    }
    for s in &tx.witness_set.plutus_v1_scripts {
        witness_hashes.insert(dugite_primitives::hash::blake2b_224_tagged(1, s));
    }
    for s in &tx.witness_set.plutus_v2_scripts {
        witness_hashes.insert(dugite_primitives::hash::blake2b_224_tagged(2, s));
    }
    for s in &tx.witness_set.plutus_v3_scripts {
        witness_hashes.insert(dugite_primitives::hash::blake2b_224_tagged(3, s));
    }

    // 2. Scripts needed (same logic as check_script_data_hash).
    let mut scripts_needed: HashSet<Hash28> = HashSet::new();
    // Spending inputs.
    for input in &body.inputs {
        if let Some(utxo) = utxo_set.lookup(input) {
            let ab = utxo.address.to_bytes();
            if !ab.is_empty() {
                let t = (ab[0] >> 4) & 0x0F;
                if matches!(t, 1 | 3 | 5 | 7) && ab.len() >= 29 {
                    if let Ok(h) = Hash28::try_from(&ab[1..29]) {
                        scripts_needed.insert(h);
                    }
                }
            }
        }
    }
    // Minting.
    for policy_id in body.mint.keys() {
        scripts_needed.insert(*policy_id);
    }
    // Withdrawals.
    for reward_addr in body.withdrawals.keys() {
        if reward_addr.len() >= 29 && (reward_addr[0] & 0x10) != 0 {
            if let Ok(h) = Hash28::try_from(&reward_addr[1..29]) {
                scripts_needed.insert(h);
            }
        }
    }
    // Certificates with script credentials.
    use dugite_primitives::credentials::Credential as Cred;
    for cert in &body.certificates {
        let cred: Option<&Cred> = match cert {
            Certificate::StakeDeregistration(c) => Some(c),
            Certificate::StakeDelegation { credential: c, .. } => Some(c),
            Certificate::ConwayStakeRegistration { credential: c, .. } => Some(c),
            Certificate::ConwayStakeDeregistration { credential: c, .. } => Some(c),
            Certificate::VoteDelegation { credential: c, .. } => Some(c),
            Certificate::StakeVoteDelegation { credential: c, .. } => Some(c),
            Certificate::RegStakeDeleg { credential: c, .. } => Some(c),
            Certificate::RegStakeVoteDeleg { credential: c, .. } => Some(c),
            Certificate::VoteRegDeleg { credential: c, .. } => Some(c),
            Certificate::CommitteeHotAuth {
                cold_credential: c, ..
            } => Some(c),
            Certificate::CommitteeColdResign {
                cold_credential: c, ..
            } => Some(c),
            Certificate::RegDRep { credential: c, .. } => Some(c),
            Certificate::UnregDRep { credential: c, .. } => Some(c),
            Certificate::UpdateDRep { credential: c, .. } => Some(c),
            _ => None,
        };
        if let Some(Cred::Script(h)) = cred {
            scripts_needed.insert(*h);
        }
    }
    // Voting procedures: DRep and CC voter script credentials.
    for voter in body.voting_procedures.keys() {
        let cred: Option<&Cred> = match voter {
            Voter::DRep(c) | Voter::ConstitutionalCommittee(c) => Some(c),
            Voter::StakePool(_) => None,
        };
        if let Some(Cred::Script(h)) = cred {
            scripts_needed.insert(*h);
        }
    }
    // Proposal procedures: guardrail script hashes.
    for proposal in &body.proposal_procedures {
        match &proposal.gov_action {
            GovAction::ParameterChange {
                policy_hash: Some(h),
                ..
            }
            | GovAction::TreasuryWithdrawals {
                policy_hash: Some(h),
                ..
            } => {
                scripts_needed.insert(*h);
            }
            _ => {}
        }
    }

    // 3. Reference script hashes (from spending inputs AND reference
    //    inputs). Haskell's `sRefs` includes NATIVE reference scripts too
    //    (issue #791) — `compute_script_ref_hash` covers every `ScriptRef`
    //    variant (native + PlutusV1-V4) uniformly.
    let mut ref_script_hashes: HashSet<Hash28> = HashSet::new();
    for input in body.inputs.iter().chain(body.reference_inputs.iter()) {
        if let Some(utxo) = utxo_set.lookup(input) {
            if let Some(script_ref) = &utxo.script_ref {
                let native_original = reference_native_original_bytes(&utxo);
                ref_script_hashes.insert(compute_script_ref_hash(
                    script_ref,
                    native_original.as_deref(),
                ));
            }
        }
    }

    // 4. extra = witness_hashes \ (scripts_needed \ ref_script_hashes)
    let needed_non_refs: HashSet<&Hash28> = scripts_needed.difference(&ref_script_hashes).collect();
    let mut extra: Vec<String> = witness_hashes
        .iter()
        .filter(|h| !needed_non_refs.contains(h))
        .map(|h| h.to_hex())
        .collect();

    if !extra.is_empty() {
        extra.sort(); // deterministic error output
        errors.push(ValidationError::ExtraneousScriptWitness { hashes: extra });
    }
}

/// Check that every script in the transaction's witness set is well-formed.
///
/// Mirrors Haskell's `MalformedScriptWitnesses` predicate from the Babbage
/// UTXOW rule (`eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxow.hs`,
/// line 260):
///
/// ```haskell
/// invalidScriptWits =
///   Map.filter (not . validScript (pp ^. ppProtocolVersionL)) scriptWits
/// failureOnNonEmptySet (Map.keysSet invalidScriptWits) MalformedScriptWitnesses
/// ```
///
/// `validScript pv script` (Alonzo `Cardano.Ledger.Alonzo.Scripts`, line 650):
/// - Native: `deepseq` (force to NF — trivially OK in Rust where decoding
///   already produced a fully-evaluated struct, no laziness/thunks).
/// - Plutus: `isValidPlutus pv plutusScript` —
///   `isValidPlutus v = isRight . decodePlutusRunnable v`. The script's
///   flat-encoded bytes must decode AND the language version must be
///   supported at the given major protocol version.
///
/// PV gates for Plutus languages:
/// - PlutusV1: PV >= 5 (Alonzo)
/// - PlutusV2: PV >= 7 (Babbage)
/// - PlutusV3: PV >= 9 (Conway)
pub(super) fn check_malformed_script_witnesses(
    tx: &Transaction,
    params: &ProtocolParameters,
    errors: &mut Vec<ValidationError>,
) {
    let pv = params.protocol_version_major;

    // Era gate: `MalformedScriptWitnesses` is part of the BABBAGE UTXOW rule
    // (`validateScriptsWellFormedTxOuts`, babbage.md:192 "Adds check: …") and
    // every later era. It does NOT exist in the Alonzo UTXOW transition
    // (`alonzoStyleWitness`). Running it pre-Babbage wrongly rejects on-chain
    // Alonzo PlutusV1 witnesses. Babbage begins at PV7 (Vasil). Mirror
    // cardano-ledger: the predicate is gated by era, not just the PV of the
    // script language.
    if pv < 7 {
        return;
    }
    let mut malformed: Vec<String> = Vec::new();

    // Plutus V1 → PV5+
    for s in &tx.witness_set.plutus_v1_scripts {
        if pv < 5 || !plutus_witness_script_decodes(s) {
            malformed.push(dugite_primitives::hash::blake2b_224_tagged(1, s).to_hex());
        }
    }
    // Plutus V2 → PV7+
    for s in &tx.witness_set.plutus_v2_scripts {
        if pv < 7 || !plutus_witness_script_decodes(s) {
            malformed.push(dugite_primitives::hash::blake2b_224_tagged(2, s).to_hex());
        }
    }
    // Plutus V3 → PV9+
    for s in &tx.witness_set.plutus_v3_scripts {
        if pv < 9 || !plutus_witness_script_decodes(s) {
            malformed.push(dugite_primitives::hash::blake2b_224_tagged(3, s).to_hex());
        }
    }
    // Native scripts: decoded successfully into our Rust enum — no thunks
    // to force, so they trivially pass `deepseq`. Nothing to check here.

    if !malformed.is_empty() {
        malformed.sort();
        errors.push(ValidationError::MalformedScriptWitnesses { hashes: malformed });
    }
}

/// Check that every reference script attached to an output PRODUCED by this
/// transaction is well-formed.
///
/// Mirrors Haskell `MalformedReferenceScripts` (Babbage UTXOW rule, same file
/// as `MalformedScriptWitnesses`, line 261):
///
/// ```haskell
/// rScripts = mapMaybe (strictMaybeToMaybe . view referenceScriptTxOutL) (toList txOuts)
/// invalidRefScripts = filter (not . validScript (pp ^. ppProtocolVersionL)) rScripts
/// invalidRefScriptHashes = Set.fromList $ map (hashScript @era) invalidRefScripts
/// failureOnNonEmptySet invalidRefScriptHashes MalformedReferenceScripts
/// ```
///
/// `txOuts = normalOuts <> foldMap singleton collateralReturn` — only outputs
/// PRODUCED by this tx. Reference scripts on UTxOs referenced via
/// `reference_inputs` were validated when the tx that created them was
/// applied; not re-checked here.
pub(super) fn check_malformed_reference_scripts(
    tx: &Transaction,
    params: &ProtocolParameters,
    errors: &mut Vec<ValidationError>,
) {
    let pv = params.protocol_version_major;

    // Era gate: `MalformedReferenceScripts` is introduced by the BABBAGE UTXOW
    // rule (`validateScriptsWellFormedTxOuts`, babbage.md:192) alongside
    // `MalformedScriptWitnesses`; it does not exist in Alonzo. Reference scripts
    // in outputs are themselves a Babbage feature, but gate explicitly to mirror
    // cardano-ledger. Babbage begins at PV7.
    if pv < 7 {
        return;
    }
    let mut malformed: Vec<String> = Vec::new();

    // Visit every output produced by this tx (normal outputs + collateral_return).
    let mut outputs: Vec<&dugite_primitives::transaction::TransactionOutput> =
        tx.body.outputs.iter().collect();
    if let Some(ref cr) = tx.body.collateral_return {
        outputs.push(cr);
    }

    for output in outputs {
        let Some(ref script_ref) = output.script_ref else {
            continue;
        };
        match script_ref {
            ScriptRef::NativeScript(_) => {
                // Decoded successfully into our enum → trivially OK.
            }
            ScriptRef::PlutusV1(bytes) => {
                if pv < 5 || !plutus_ref_script_decodes(bytes) {
                    // Plutus arm only — native_original is unused for Plutus refs.
                    malformed.push(compute_script_ref_hash(script_ref, None).to_hex());
                }
            }
            ScriptRef::PlutusV2(bytes) => {
                if pv < 7 || !plutus_ref_script_decodes(bytes) {
                    // Plutus arm only — native_original is unused for Plutus refs.
                    malformed.push(compute_script_ref_hash(script_ref, None).to_hex());
                }
            }
            ScriptRef::PlutusV3(bytes) => {
                if pv < 9 || !plutus_ref_script_decodes(bytes) {
                    // Plutus arm only — native_original is unused for Plutus refs.
                    malformed.push(compute_script_ref_hash(script_ref, None).to_hex());
                }
            }
            ScriptRef::PlutusV4(bytes) => {
                // Dijkstra era; PV gate to be confirmed. Conservative: require
                // PV >= 11 (post-Conway) AND decodes.
                if pv < 11 || !plutus_ref_script_decodes(bytes) {
                    // Plutus arm only — native_original is unused for Plutus refs.
                    malformed.push(compute_script_ref_hash(script_ref, None).to_hex());
                }
            }
        }
    }

    if !malformed.is_empty() {
        malformed.sort();
        malformed.dedup();
        errors.push(ValidationError::MalformedReferenceScripts { hashes: malformed });
    }
}

/// Decode a WITNESS-SET Plutus script's bytes (`witness_set.plutus_vN_scripts`
/// element).
///
/// Mirrors Haskell `decodePlutusRunnable v bs`, which requires the
/// **mandatory CBOR-bytestring wrapper**: `deserialiseScript` first runs
/// `CBOR.decodeBytes` to recover the flat payload, then flat-decodes it —
/// a script whose flat bytes are NOT wrapped in a CBOR bytestring is a
/// hard decode error for every language. This is `Program::from_cbor`
/// ONLY (issue #792) — the previous `|| Program::from_flat(bytes)`
/// fallback let an adversary attach a raw-flat (unwrapped) witness script
/// that Haskell rejects but dugite accepted.
///
/// Byte-format note: the witness-set array element (`[* bytes]` per CDDL)
/// is itself a CBOR bytestring whose CONTENT is *another* CBOR-encoded
/// bytestring wrapping the flat program — i.e. `witness_set.plutus_vN_scripts[i]`,
/// once read off the wire (one bytestring unwrap performed by the array
/// decoder), still needs the `from_cbor` unwrap to reach the flat bytes.
/// This matches `dugite_uplc::eval_redeemer::decode_script_bytes_uncached`'s
/// own documented convention ("on-chain Plutus scripts are CBOR-encoded
/// byte-strings holding the flat-encoded program") and
/// `crate::plutus`'s production Phase-2 test fixtures, which build witness
/// scripts via `Program::to_cbor()` (not `to_flat()`).
///
/// Do NOT reuse this for `ScriptRef` (reference scripts) — see
/// [`plutus_ref_script_decodes`], which uses the opposite (already
/// singly-unwrapped) convention.
///
/// V3+ trailing-byte (`RemainderError`) decoder-exhaustion enforcement is
/// explicitly OUT OF SCOPE here — that root cause lives in `dugite-uplc`
/// (tracked separately, issues #822/#836).
fn plutus_witness_script_decodes(bytes: &[u8]) -> bool {
    dugite_uplc::program::Program::from_cbor(bytes).is_ok()
}

/// Decode a REFERENCE script's bytes (`ScriptRef::PlutusVN` payload).
///
/// Byte-format note: `read_script_ref` (`script_ref = #6.24(bytes .cbor
/// script)`, `script = [lang_tag, script_value]`) performs a SINGLE
/// bytestring unwrap of `script_value`, leaving `ScriptRef::PlutusVN(bytes)`
/// holding a CBOR-bytestring-WRAPPED flat program — exactly like a witness
/// script — NOT raw flat. This is confirmed three ways: (1) an actual
/// captured on-chain reference script (uplc fixture `tx6.json`) begins with
/// a CBOR bytestring header whose declared length wraps a flat program; (2)
/// `compute_script_ref_hash` hashes `lang_tag || bytes` and this must equal
/// the on-chain script hash, which cardano-ledger computes over
/// `lang_tag || cbor(flat)` — so `bytes` is `cbor(flat)`; (3) the phase-2
/// eval path (`dugite_uplc::…::decode_script_bytes`, #836) decodes these
/// very bytes with `from_cbor` and succeeds on real fixtures. A prior
/// revision used `from_flat` here (based on a synthetic Dijkstra round-trip
/// rather than real chain data) — that FALSE-REJECTED every legitimate
/// reference script as `MalformedReferenceScripts` (a consensus divergence).
/// Use `from_cbor` — the same as witness scripts.
fn plutus_ref_script_decodes(bytes: &[u8]) -> bool {
    dugite_uplc::program::Program::from_cbor(bytes).is_ok()
}

/// Return `true` when the transaction has any Plutus scripts or redeemers.
///
/// Used by `validate_transaction` to gate the COLLATERAL / redeemer-purpose
/// / Phase-2-execution checks, which only make sense when a script might
/// actually run. Deliberately does NOT count `witness_set.plutus_data`
/// (supplemental datums) — a datum-only tx never runs a script and must
/// NOT be forced through the collateral gate (issue #790). `Rule 12`
/// (`check_script_data_hash`) needs a wider "has redeemers OR datums"
/// condition than this, so it is called unconditionally in
/// `validate_transaction` rather than being gated on this function.
pub(super) fn has_plutus_scripts(tx: &Transaction) -> bool {
    !tx.witness_set.plutus_v1_scripts.is_empty()
        || !tx.witness_set.plutus_v2_scripts.is_empty()
        || !tx.witness_set.plutus_v3_scripts.is_empty()
        || !tx.witness_set.redeemers.is_empty()
}

/// Return the tiered reference-script fee for a transaction.
///
/// Per Haskell's `txNonDistinctRefScriptsSize`, the fee is based on the total
/// script bytes reachable from BOTH spending inputs and reference inputs.
/// Passing an empty slice for either argument is valid when that class of inputs
/// is absent from the transaction.
pub(super) fn ref_script_fee(
    inputs: &[TransactionInput],
    reference_inputs: &[TransactionInput],
    utxo_set: &dyn UtxoLookup,
    min_fee_ref_script_cost_per_byte: &Rational,
) -> u64 {
    let size = calculate_ref_script_size(inputs, reference_inputs, utxo_set);
    if size > 0 {
        calculate_ref_script_tiered_fee_rational(
            min_fee_ref_script_cost_per_byte.numerator,
            min_fee_ref_script_cost_per_byte.denominator,
            size,
        )
    } else {
        0
    }
}

/// Compute the total execution-unit fee component from the transaction's redeemers.
///
/// Haskell's `txscriptfee` (ExUnits.hs) computes a **single ceiling** over the
/// sum of both rational products:
///
///   `ceil(price_mem * Σ mem + price_step * Σ steps)`
///
/// NOT `ceil(price_mem * Σ mem) + ceil(price_step * Σ steps)` — per-component
/// ceiling would be up to 1 lovelace too high when both have fractional parts.
pub(super) fn ex_unit_fee(tx: &Transaction, params: &ProtocolParameters) -> u64 {
    let total_mem: u64 = tx
        .witness_set
        .redeemers
        .iter()
        .fold(0u64, |acc, r| acc.saturating_add(r.ex_units.mem));
    let total_steps: u64 = tx
        .witness_set
        .redeemers
        .iter()
        .fold(0u64, |acc, r| acc.saturating_add(r.ex_units.steps));

    let mem_price = &params.execution_costs.mem_price;
    let step_price = &params.execution_costs.step_price;

    if mem_price.denominator == 0 || step_price.denominator == 0 {
        return 0;
    }

    // Compute as a single rational sum then apply one ceiling.
    // sum = (mem_num * total_mem * step_den + step_num * total_steps * mem_den)
    //       / (mem_den * step_den)
    let mem_num = mem_price.numerator as u128;
    let mem_den = mem_price.denominator as u128;
    let step_num = step_price.numerator as u128;
    let step_den = step_price.denominator as u128;
    let m = total_mem as u128;
    let s = total_steps as u128;

    // Cross-multiply to a common denominator for exact addition.
    let numerator = mem_num * m * step_den + step_num * s * mem_den;
    let denominator = mem_den * step_den;

    if denominator == 0 {
        return 0;
    }
    numerator.div_ceil(denominator) as u64
}

/// Compute the Haskell-compatible transaction size for fee calculation.
///
/// Haskell's `toCBORForSizeComputation` (Alonzo+ eras) encodes the transaction
/// as a **3-element** CBOR array `[body, wits, aux_data]`, deliberately omitting
/// the `is_valid` boolean field to maintain fee-formula continuity with the Mary
/// era.  The on-chain representation (and our `raw_cbor`) is a **4-element** array
/// `[body, wits, is_valid, aux_data]`.  The difference is exactly 1 byte — the
/// CBOR encoding of the boolean (`0xF4`/`0xF5`).
///
/// We detect Alonzo+ by checking whether the first byte of `raw_cbor` is `0x84`
/// (definite-length array of 4).  Pre-Alonzo transactions start with `0x83`
/// (array of 3) and have no `is_valid` field, so no adjustment is needed.
///
/// Reference:
/// - `Cardano.Ledger.Alonzo.Tx.toCBORForSizeComputation` (cardano-ledger)
/// - Conway tiered reference script fee is based on script bytes, not tx size.
pub(super) fn fee_tx_size(tx: &Transaction, tx_size: u64) -> u64 {
    // The raw CBOR first byte tells us the CBOR major type and additional info.
    // 0x84 = major type 4 (array) with additional info 4 (length = 4 elements).
    // An Alonzo+ transaction is encoded as array(4); pre-Alonzo as array(3) = 0x83.
    let is_alonzo_plus = tx
        .raw_cbor
        .as_deref()
        .is_some_and(|b| b.first() == Some(&0x84));
    if is_alonzo_plus {
        // Subtract the 1-byte is_valid field that Haskell excludes from fee size.
        tx_size.saturating_sub(1)
    } else {
        tx_size
    }
}

/// Compute the minimum fee including base formula, reference-script fee and
/// execution-unit costs.
///
/// The base fee uses [`fee_tx_size`] to match Haskell's `toCBORForSizeComputation`,
/// which omits the `is_valid` boolean from the size for Alonzo+ transactions.
///
/// The ref-script fee is only added for Conway (PV >= 9) and later eras.
/// Haskell: Babbage `getMinFeeTxUtxo` = `getShelleyMinFeeTxUtxo pp tx` (no
/// ref-script-byte charge); the term first appears in Conway
/// `getConwayMinFeeTxUtxo` (tierRefScriptFee).  Pre-Conway eras share
/// Shelley's `getShelleyMinFeeTxUtxo` which has no such term.
/// Reference: babbage.md §5 "Reference script fee (Babbage: NONE)";
/// conway.md §12 "Tiered reference script fee".
pub(super) fn compute_min_fee(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    params: &ProtocolParameters,
    tx_size: u64,
) -> Lovelace {
    // The tiered ref-script fee was introduced with Conway (PV9).  Pre-Conway
    // eras (Shelley through Babbage) use `getShelleyMinFeeTxUtxo` which has
    // no such term.  Mirror the Haskell era dispatch by gating on
    // `protocol_version_major >= 9`.
    let rs_fee = if params.protocol_version_major >= 9 {
        // Pass both spending inputs and reference inputs so that scripts embedded in
        // spending-input UTxOs are counted in the tiered fee — matching Haskell's
        // `txNonDistinctRefScriptsSize` which uses `inputs txb <> referenceInputs txb`.
        ref_script_fee(
            &tx.body.inputs,
            &tx.body.reference_inputs,
            utxo_set,
            &params.min_fee_ref_script_cost_per_byte,
        )
    } else {
        0
    };
    let eu_fee = ex_unit_fee(tx, params);
    // Use the Haskell-compatible size (excludes is_valid for Alonzo+).
    let effective_size = fee_tx_size(tx, tx_size);
    Lovelace(
        params
            .min_fee(effective_size)
            .0
            .saturating_add(rs_fee)
            .saturating_add(eu_fee),
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utxo::UtxoSet;
    use dugite_primitives::address::Address;
    use dugite_primitives::hash::{Hash28, Hash32, TransactionHash};
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::time::SlotNo;

    /// #862: `native_script_hash` hashes over the ORIGINAL wire bytes when supplied
    /// (Haskell hashScript over MemoBytes), which — for a non-canonically-encoded
    /// script — differs from the re-encode fallback. `blake2b_224(0x00 || original)`.
    #[test]
    fn native_script_hash_uses_original_bytes_862() {
        // Non-canonical indefinite-length ScriptPubkey: 0x9f 0x00 (0x581c<28>) 0xff
        let mut original = vec![0x9f, 0x00, 0x58, 0x1c];
        original.extend_from_slice(&[0xAB; 28]);
        original.push(0xff);
        let ns = dugite_serialization::decode_native_script_cbor(&original).unwrap();

        let h_orig = native_script_hash(&ns, Some(&original));
        let mut tagged = vec![0x00];
        tagged.extend_from_slice(&original);
        assert_eq!(
            h_orig,
            dugite_primitives::hash::blake2b_224(&tagged),
            "must hash 0x00 || original_bytes"
        );

        // The re-encode fallback yields a DIFFERENT hash for this non-canonical form,
        // which is exactly the divergence the fix removes.
        let h_reencode = native_script_hash(&ns, None);
        assert_ne!(
            h_orig, h_reencode,
            "original-bytes hash must differ from the re-encode for a non-canonical script"
        );
    }
    use dugite_primitives::transaction::OutputDatum;
    use dugite_primitives::transaction::{
        NativeScript, ScriptRef, Transaction, TransactionInput, TransactionOutput,
    };
    use dugite_primitives::value::{Lovelace, Value};
    use std::collections::HashSet;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a dummy 32-byte key hash seeded from a single byte value.
    fn key_hash(seed: u8) -> Hash32 {
        Hash32::from_bytes([seed; 32])
    }

    /// Build a dummy 28-byte hash seeded from a single byte value.
    fn hash28(seed: u8) -> Hash28 {
        Hash28::from_bytes([seed; 28])
    }

    /// Build a dummy TransactionInput seeded from a byte value.
    fn tx_input(seed: u8) -> TransactionInput {
        TransactionInput {
            transaction_id: TransactionHash::from_bytes([seed; 32]),
            index: 0,
        }
    }

    /// Build a minimal `TransactionOutput` with no datum and no script_ref.
    fn simple_output() -> TransactionOutput {
        TransactionOutput {
            address: Address::Enterprise(dugite_primitives::address::EnterpriseAddress {
                network: dugite_primitives::network::NetworkId::Testnet,
                payment: dugite_primitives::credentials::Credential::VerificationKey(hash28(0x01)),
            }),
            value: Value::lovelace(1_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    // -----------------------------------------------------------------------
    // Native script evaluation — 6 tests
    // -----------------------------------------------------------------------

    /// A ScriptPubkey script passes when its keyhash is in the signer set.
    #[test]
    fn test_native_script_pubkey_match() {
        let kh = key_hash(0xAA);
        let script = NativeScript::ScriptPubkey(kh);
        let mut signers = HashSet::new();
        signers.insert(kh);
        assert!(evaluate_native_script(&script, &signers, None, None));
    }

    /// A ScriptPubkey script fails when its keyhash is absent from the signer set.
    #[test]
    fn test_native_script_pubkey_no_match() {
        let kh = key_hash(0xAA);
        let other = key_hash(0xBB);
        let script = NativeScript::ScriptPubkey(kh);
        let mut signers = HashSet::new();
        signers.insert(other);
        assert!(!evaluate_native_script(&script, &signers, None, None));
    }

    /// ScriptAll requires every sub-script to pass.
    /// Passes when both signers are present; fails when only one is.
    #[test]
    fn test_native_script_all() {
        let kh_a = key_hash(0x01);
        let kh_b = key_hash(0x02);
        let script = NativeScript::ScriptAll(vec![
            NativeScript::ScriptPubkey(kh_a),
            NativeScript::ScriptPubkey(kh_b),
        ]);

        // Both signers — should pass.
        let mut signers = HashSet::new();
        signers.insert(kh_a);
        signers.insert(kh_b);
        assert!(evaluate_native_script(&script, &signers, None, None));

        // Only one signer — should fail.
        let mut signers_one = HashSet::new();
        signers_one.insert(kh_a);
        assert!(!evaluate_native_script(&script, &signers_one, None, None));

        // No signers — should fail.
        assert!(!evaluate_native_script(
            &script,
            &HashSet::new(),
            None,
            None
        ));
    }

    /// ScriptAny requires at least one sub-script to pass.
    #[test]
    fn test_native_script_any() {
        let kh_a = key_hash(0x01);
        let kh_b = key_hash(0x02);
        let script = NativeScript::ScriptAny(vec![
            NativeScript::ScriptPubkey(kh_a),
            NativeScript::ScriptPubkey(kh_b),
        ]);

        // Only signer A — should pass (any one is enough).
        let mut signers_a = HashSet::new();
        signers_a.insert(kh_a);
        assert!(evaluate_native_script(&script, &signers_a, None, None));

        // Only signer B — should also pass.
        let mut signers_b = HashSet::new();
        signers_b.insert(kh_b);
        assert!(evaluate_native_script(&script, &signers_b, None, None));

        // No signers — should fail.
        assert!(!evaluate_native_script(
            &script,
            &HashSet::new(),
            None,
            None
        ));
    }

    /// ScriptNOfK(2, [a, b, c]) passes only when at least 2 sub-scripts satisfy.
    #[test]
    fn test_native_script_n_of_k() {
        let kh_a = key_hash(0x01);
        let kh_b = key_hash(0x02);
        let kh_c = key_hash(0x03);
        let script = NativeScript::ScriptNOfK(
            2,
            vec![
                NativeScript::ScriptPubkey(kh_a),
                NativeScript::ScriptPubkey(kh_b),
                NativeScript::ScriptPubkey(kh_c),
            ],
        );

        // Exactly 2 of 3 signers present — should pass.
        let mut signers_ab = HashSet::new();
        signers_ab.insert(kh_a);
        signers_ab.insert(kh_b);
        assert!(evaluate_native_script(&script, &signers_ab, None, None));

        // All 3 signers — should still pass.
        let mut signers_all = HashSet::new();
        signers_all.insert(kh_a);
        signers_all.insert(kh_b);
        signers_all.insert(kh_c);
        assert!(evaluate_native_script(&script, &signers_all, None, None));

        // Only 1 of 3 — should fail.
        let mut signers_one = HashSet::new();
        signers_one.insert(kh_a);
        assert!(!evaluate_native_script(&script, &signers_one, None, None));

        // No signers — should fail.
        assert!(!evaluate_native_script(
            &script,
            &HashSet::new(),
            None,
            None
        ));
    }

    /// Issue #787: timelocks are evaluated against the TRANSACTION'S OWN
    /// `ValidityInterval` bounds (`invalid_before` / `invalid_hereafter`),
    /// never an application/current slot. `SNothing` (an unset bound)
    /// always evaluates to `False` — Haskell `evalTimelock`.
    ///
    /// `InvalidBefore(100)` succeeds iff `invalid_before = Some(s)` with
    /// `100 <= s`. `InvalidHereafter(100)` succeeds iff
    /// `invalid_hereafter = Some(e)` with `e <= 100`.
    #[test]
    fn test_native_script_time_locks() {
        let signers: HashSet<Hash32> = HashSet::new();

        let before = NativeScript::InvalidBefore(SlotNo(100));
        // Unset validity-interval-start ⇒ always False, regardless of bound.
        assert!(!evaluate_native_script(&before, &signers, None, None));
        // tx invalid_before(99) < lockStart(100) — not yet valid.
        assert!(!evaluate_native_script(
            &before,
            &signers,
            Some(SlotNo(99)),
            None
        ));
        // tx invalid_before(100) == lockStart(100) — valid.
        assert!(evaluate_native_script(
            &before,
            &signers,
            Some(SlotNo(100)),
            None
        ));
        // tx invalid_before(200) > lockStart(100) — valid.
        assert!(evaluate_native_script(
            &before,
            &signers,
            Some(SlotNo(200)),
            None
        ));

        let hereafter = NativeScript::InvalidHereafter(SlotNo(100));
        // Unset TTL ⇒ always False, regardless of bound.
        assert!(!evaluate_native_script(&hereafter, &signers, None, None));
        // tx ttl(99) <= lockExp(100) — valid.
        assert!(evaluate_native_script(
            &hereafter,
            &signers,
            None,
            Some(SlotNo(99))
        ));
        // tx ttl(100) == lockExp(100) — valid (non-strict upper bound on the tx side).
        assert!(evaluate_native_script(
            &hereafter,
            &signers,
            None,
            Some(SlotNo(100))
        ));
        // tx ttl(101) > lockExp(100) — invalid.
        assert!(!evaluate_native_script(
            &hereafter,
            &signers,
            None,
            Some(SlotNo(101))
        ));
    }

    /// Issue #787 regression: a "loose" ValidityInterval that a naive
    /// current-slot check would have satisfied must still fail when the
    /// TX'S OWN declared bound does not satisfy the timelock. This is the
    /// exact over-acceptance the bug allowed: a native script
    /// `InvalidBefore(100)` with tx `invalid_before = Some(50)` (the tx
    /// itself claims validity from slot 50) must fail even though a
    /// contemporaneous chain slot of, say, 150 would have looked valid
    /// under the old (buggy) `current_slot >= lockStart` semantics.
    #[test]
    fn test_native_script_time_lock_uses_tx_bound_not_current_slot() {
        let signers: HashSet<Hash32> = HashSet::new();
        let before = NativeScript::InvalidBefore(SlotNo(100));

        // The tx's own bound (50) does not satisfy lockStart(100), even
        // though a "current slot" of 150 (not a parameter anymore) would
        // have satisfied the old, incorrect `current_slot >= lockStart`
        // check.
        assert!(!evaluate_native_script(
            &before,
            &signers,
            Some(SlotNo(50)),
            None
        ));

        let hereafter = NativeScript::InvalidHereafter(SlotNo(100));
        // The tx's own ttl (200) does not satisfy `e <= lockExp(100)`,
        // even though a "current slot" of 50 would have looked valid
        // under the old `current_slot < lockExp` check.
        assert!(!evaluate_native_script(
            &hereafter,
            &signers,
            None,
            Some(SlotNo(200))
        ));
    }

    // -----------------------------------------------------------------------
    // Script hash computation — 1 test
    // -----------------------------------------------------------------------

    /// compute_script_ref_hash prefixes native scripts with 0x00 and Plutus V2
    /// scripts with 0x02 before hashing, matching the Cardano spec tag convention.
    #[test]
    fn test_script_hash_type_tags() {
        // --- Native script: tag 0x00 ---
        // Use a simple ScriptPubkey native script.
        let kh = key_hash(0x42);
        let ns = NativeScript::ScriptPubkey(kh);
        let script_ref_native = ScriptRef::NativeScript(ns.clone());

        let computed = compute_script_ref_hash(&script_ref_native, None);

        // Manually reproduce: blake2b_224(0x00 || script_cbor)
        let script_cbor = dugite_serialization::encode_native_script(&ns);
        let mut tagged_native = Vec::with_capacity(1 + script_cbor.len());
        tagged_native.push(0x00u8);
        tagged_native.extend_from_slice(&script_cbor);
        let expected_native = dugite_primitives::hash::blake2b_224(&tagged_native);

        assert_eq!(
            computed, expected_native,
            "Native script hash must equal blake2b_224(0x00 || script_cbor)"
        );

        // --- Plutus V2: tag 0x02 ---
        let plutus_v2_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];
        let script_ref_v2 = ScriptRef::PlutusV2(plutus_v2_bytes.clone());
        let computed_v2 = compute_script_ref_hash(&script_ref_v2, None);

        let mut tagged_v2 = Vec::with_capacity(1 + plutus_v2_bytes.len());
        tagged_v2.push(0x02u8);
        tagged_v2.extend_from_slice(&plutus_v2_bytes);
        let expected_v2 = dugite_primitives::hash::blake2b_224(&tagged_v2);

        assert_eq!(
            computed_v2, expected_v2,
            "Plutus V2 script hash must equal blake2b_224(0x02 || script_bytes)"
        );

        // The two hashes must differ because the prefix bytes differ.
        assert_ne!(
            computed, computed_v2,
            "Native and Plutus V2 hashes must differ (different tag bytes)"
        );
    }

    // -----------------------------------------------------------------------
    // Available script collection — 2 tests
    // -----------------------------------------------------------------------

    /// collect_available_script_hashes returns hashes for native and Plutus V2
    /// scripts present in the transaction witness set.
    #[test]
    fn test_available_scripts_from_witnesses() {
        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x11; 32]));

        // Add a native script to the witness set.
        let kh = key_hash(0x55);
        let ns = NativeScript::ScriptPubkey(kh);
        tx.witness_set.native_scripts.push(ns.clone());

        // Add a Plutus V2 script to the witness set.
        let v2_bytes = vec![0xCA, 0xFE, 0xBA, 0xBE];
        tx.witness_set.plutus_v2_scripts.push(v2_bytes.clone());

        let utxo_set = UtxoSet::new();
        let available = collect_available_script_hashes(&tx, &utxo_set);

        // Compute expected hash for the native script.
        let ns_cbor = dugite_serialization::encode_native_script(&ns);
        let mut ns_tagged = vec![0x00u8];
        ns_tagged.extend_from_slice(&ns_cbor);
        let ns_hash = dugite_primitives::hash::blake2b_224(&ns_tagged);

        // Compute expected hash for the Plutus V2 script.
        let mut v2_tagged = vec![0x02u8];
        v2_tagged.extend_from_slice(&v2_bytes);
        let v2_hash = dugite_primitives::hash::blake2b_224(&v2_tagged);

        assert!(
            available.contains(&ns_hash),
            "Native script hash should be in available set"
        );
        assert!(
            available.contains(&v2_hash),
            "Plutus V2 script hash should be in available set"
        );
        assert_eq!(
            available.len(),
            2,
            "Available set should contain exactly 2 hashes"
        );
    }

    /// collect_available_script_hashes picks up script_ref hashes from UTxOs
    /// reachable via reference inputs.
    #[test]
    fn test_available_scripts_from_ref_inputs() {
        let ref_input = tx_input(0xAB);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x22; 32]));
        tx.body.reference_inputs.push(ref_input.clone());

        // Build a UTxO that carries a Plutus V2 script_ref.
        let v2_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let mut utxo_set = UtxoSet::new();
        let mut output = simple_output();
        output.script_ref = Some(ScriptRef::PlutusV2(v2_bytes.clone()));
        utxo_set.insert(ref_input, output);

        let available = collect_available_script_hashes(&tx, &utxo_set);

        // Compute the expected hash.
        let mut tagged = vec![0x02u8];
        tagged.extend_from_slice(&v2_bytes);
        let expected = dugite_primitives::hash::blake2b_224(&tagged);

        assert!(
            available.contains(&expected),
            "Script hash from reference input's script_ref should be available"
        );
        assert_eq!(
            available.len(),
            1,
            "Available set should contain exactly 1 hash from the ref input"
        );
    }

    // -----------------------------------------------------------------------
    // Tiered fee calculation — 2 tests
    // -----------------------------------------------------------------------

    /// Single tier: total_size == TIER_SIZE (25,600 bytes).
    /// Fee = base_fee_per_byte * total_size = 15 * 25_600 = 384_000.
    #[test]
    fn test_tiered_fee_single_tier() {
        // Exactly one tier, no multiplier applied yet.
        let fee = calculate_ref_script_tiered_fee(15, 25_600);
        assert_eq!(
            fee, 384_000,
            "Single-tier fee must equal base_fee_per_byte * tier_size"
        );
    }

    /// Two tiers: total_size == 2 * TIER_SIZE (51,200 bytes).
    ///
    /// Tier 0: price = 15/1, contribution = 25_600 * 15 = 384_000, whole = 384_000, rem = 0
    /// Tier 1: price advances: 15*6=90, den 1*5=5, gcd(90,5)=5 → price = 18/1
    ///         contribution = 25_600 * 18 = 460_800, whole = 460_800, rem = 0
    /// Total (floor) = 384_000 + 460_800 = 844_800.
    #[test]
    fn test_tiered_fee_multiple_tiers() {
        let fee = calculate_ref_script_tiered_fee(15, 51_200);
        // Tier 0: 25_600 * 15 = 384_000
        // Tier 1: price = 18 (15 * 6/5 = 90/5 = 18), 25_600 * 18 = 460_800
        assert_eq!(
            fee, 844_800,
            "Two-tier fee must equal tier0 + tier1 contributions (floor, no fractional remainder)"
        );
    }

    // -----------------------------------------------------------------------
    // Min fee computation — 1 test
    // -----------------------------------------------------------------------

    /// compute_min_fee with a pre-Alonzo (raw_cbor = None) transaction and no
    /// reference scripts or redeemers.
    ///
    /// Expected: min_fee_a * tx_size + min_fee_b + 0 (ref_script) + 0 (ex_units).
    #[test]
    fn test_min_fee_computation() {
        let tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x33; 32]));
        // raw_cbor is None, so fee_tx_size returns tx_size unchanged.
        let tx_size: u64 = 200;

        let params = ProtocolParameters::mainnet_defaults();
        // min_fee_a = 44, min_fee_b = 155_381 (from mainnet_defaults).
        let expected = params.min_fee_a * tx_size + params.min_fee_b;

        let utxo_set = UtxoSet::new();
        let fee = compute_min_fee(&tx, &utxo_set, &params, tx_size);

        assert_eq!(
            fee,
            Lovelace(expected),
            "compute_min_fee with no scripts or ex-units must equal min_fee_a*size + min_fee_b"
        );
    }

    // -----------------------------------------------------------------------
    // Extraneous script witness checks — 2 tests
    // -----------------------------------------------------------------------

    /// A Plutus V2 script in the witness set that is not referenced by any
    /// spending input, minting policy, withdrawal, certificate, vote, or
    /// proposal must be rejected as extraneous.
    #[test]
    fn test_extraneous_witness_script_rejected() {
        // Build a dummy V2 script that is NOT needed by any script purpose.
        let script_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
        let script_hash = dugite_primitives::hash::blake2b_224_tagged(2, &script_bytes);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0xAA; 32]));
        tx.witness_set.plutus_v2_scripts.push(script_bytes.clone());

        // Spend a VKey-locked UTxO — not script-locked, so nothing is "needed".
        let input = tx_input(0x01);
        tx.body.inputs.push(input.clone());
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(input, simple_output());

        let mut errors = Vec::new();
        check_extraneous_script_witnesses(&tx, &utxo_set, &mut errors);

        assert_eq!(
            errors.len(),
            1,
            "Expected exactly one ExtraneousScriptWitness error"
        );
        match &errors[0] {
            ValidationError::ExtraneousScriptWitness { hashes } => {
                assert_eq!(hashes.len(), 1);
                assert_eq!(hashes[0], script_hash.to_hex());
            }
            other => panic!("Expected ExtraneousScriptWitness, got: {other:?}"),
        }
    }

    /// A Plutus V2 script in the witness set whose hash matches the payment
    /// credential of a script-locked spending input must NOT be flagged as
    /// extraneous — it is needed.
    #[test]
    fn test_needed_witness_script_accepted() {
        use dugite_primitives::address::{Address, BaseAddress};
        use dugite_primitives::credentials::Credential;

        // Build a dummy V2 script.
        let script_bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0x07];
        let script_hash = dugite_primitives::hash::blake2b_224_tagged(2, &script_bytes);

        // Build a script-locked base address: Script payment + VKey stake.
        let address = Address::Base(BaseAddress {
            network: dugite_primitives::network::NetworkId::Testnet,
            payment: Credential::Script(script_hash),
            stake: Credential::VerificationKey(hash28(0xEE)),
        });

        let input = tx_input(0x02);
        let utxo = TransactionOutput {
            address,
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(input.clone(), utxo);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0xBB; 32]));
        tx.body.inputs.push(input);
        tx.witness_set.plutus_v2_scripts.push(script_bytes);

        let mut errors = Vec::new();
        check_extraneous_script_witnesses(&tx, &utxo_set, &mut errors);

        assert!(
            errors.is_empty(),
            "Witness script matching a script-locked input must not be flagged as extraneous; got: {errors:?}"
        );
    }

    /// Issue #791: a native `ScriptPubkey` witness script that is NOT
    /// needed by any script purpose must be rejected as extraneous.
    /// Haskell's `ExtraneousScriptWitnessesUTXOW` (`sReceived = Map.keysSet
    /// scriptTxWits`) covers ALL scripts, native included — this also
    /// exercises the early-return fix: a NATIVE-ONLY witness set (no
    /// Plutus scripts/redeemers at all) must not skip the check.
    #[test]
    fn test_extraneous_native_witness_script_rejected() {
        let kh = key_hash(0x77);
        let ns = NativeScript::ScriptPubkey(kh);
        let ns_hash = native_script_hash(&ns, None);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0xCC; 32]));
        tx.witness_set.native_scripts.push(ns);

        // Spend a VKey-locked UTxO — not script-locked, so nothing is "needed".
        let input = tx_input(0x03);
        tx.body.inputs.push(input.clone());
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(input, simple_output());

        let mut errors = Vec::new();
        check_extraneous_script_witnesses(&tx, &utxo_set, &mut errors);

        assert_eq!(
            errors.len(),
            1,
            "Expected exactly one ExtraneousScriptWitness error for the unneeded native script"
        );
        match &errors[0] {
            ValidationError::ExtraneousScriptWitness { hashes } => {
                assert_eq!(hashes.len(), 1);
                assert_eq!(hashes[0], ns_hash.to_hex());
            }
            other => panic!("Expected ExtraneousScriptWitness, got: {other:?}"),
        }
    }

    /// A native `ScriptPubkey` witness script whose hash matches the
    /// payment credential of a script-locked spending input must NOT be
    /// flagged as extraneous — it is needed (positive-side counterpart to
    /// `test_extraneous_native_witness_script_rejected`, issue #791).
    #[test]
    fn test_needed_native_witness_script_accepted() {
        use dugite_primitives::address::{Address, BaseAddress};
        use dugite_primitives::credentials::Credential;

        let kh = key_hash(0x78);
        let ns = NativeScript::ScriptPubkey(kh);
        let ns_hash = native_script_hash(&ns, None);

        let address = Address::Base(BaseAddress {
            network: dugite_primitives::network::NetworkId::Testnet,
            payment: Credential::Script(ns_hash),
            stake: Credential::VerificationKey(hash28(0xEE)),
        });
        let input = tx_input(0x04);
        let utxo = TransactionOutput {
            address,
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(input.clone(), utxo);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0xDD; 32]));
        tx.body.inputs.push(input);
        tx.witness_set.native_scripts.push(ns);

        let mut errors = Vec::new();
        check_extraneous_script_witnesses(&tx, &utxo_set, &mut errors);

        assert!(
            errors.is_empty(),
            "Native witness script matching a script-locked input must not be flagged as extraneous; got: {errors:?}"
        );
    }

    /// Issue #791 (`ref_script_hashes`/`sRefs`): a native witness script
    /// that is only "needed" via a NATIVE reference script (not invoked as
    /// a spending/minting/cert credential) is still extraneous — a witness
    /// script that duplicates a reference script is not required.
    #[test]
    fn test_native_witness_script_matching_only_a_native_ref_script_is_extraneous() {
        let kh = key_hash(0x79);
        let ns = NativeScript::ScriptPubkey(kh);
        let ns_hash = native_script_hash(&ns, None);

        // The script-locked spending input is satisfied via a NATIVE
        // reference script attached to a reference input — not via the
        // witness set.
        let spend_input = tx_input(0x05);
        let ref_input = tx_input(0x06);

        use dugite_primitives::address::{Address, BaseAddress};
        use dugite_primitives::credentials::Credential;
        let script_locked_address = Address::Base(BaseAddress {
            network: dugite_primitives::network::NetworkId::Testnet,
            payment: Credential::Script(ns_hash),
            stake: Credential::VerificationKey(hash28(0xEE)),
        });
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(
            spend_input.clone(),
            TransactionOutput {
                address: script_locked_address,
                value: Value::lovelace(2_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            },
        );
        let mut ref_output = simple_output();
        ref_output.script_ref = Some(ScriptRef::NativeScript(ns.clone()));
        utxo_set.insert(ref_input.clone(), ref_output);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0xEE; 32]));
        tx.body.inputs.push(spend_input);
        tx.body.reference_inputs.push(ref_input);
        // The SAME native script is ALSO redundantly attached as a witness.
        tx.witness_set.native_scripts.push(ns);

        let mut errors = Vec::new();
        check_extraneous_script_witnesses(&tx, &utxo_set, &mut errors);

        assert_eq!(
            errors.len(),
            1,
            "A witness script satisfied only via a native reference script must be extraneous"
        );
        match &errors[0] {
            ValidationError::ExtraneousScriptWitness { hashes } => {
                assert_eq!(hashes, &vec![ns_hash.to_hex()]);
            }
            other => panic!("Expected ExtraneousScriptWitness, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Script data hash (Rule 12) — issue #790
    // -----------------------------------------------------------------------

    /// A vkey-only tx (no redeemers, no datums, no witness Plutus scripts,
    /// no reference inputs at all) with a junk `script_data_hash` must
    /// ALWAYS be rejected with `UnexpectedScriptDataHash`.
    #[test]
    fn test_script_data_hash_junk_rejected_no_ref_scripts() {
        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x10; 32]));
        tx.body.script_data_hash = Some(Hash32::from_bytes([0xAB; 32]));

        let utxo_set = UtxoSet::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut errors = Vec::new();
        check_script_data_hash(&tx, &utxo_set, &params, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::UnexpectedScriptDataHash)),
            "Expected UnexpectedScriptDataHash; got {errors:?}"
        );
    }

    /// Issue #790: a vkey-only tx with a junk `script_data_hash` is
    /// rejected EVEN WHEN a reference input carries an (unused)
    /// `script_ref` — the removed `has_ref_scripts` carve-out incorrectly
    /// excused this exact adversarial shape.
    #[test]
    fn test_script_data_hash_junk_rejected_with_unused_ref_script() {
        let ref_input = tx_input(0x11);
        let mut utxo_set = UtxoSet::new();
        let mut ref_output = simple_output();
        ref_output.script_ref = Some(ScriptRef::PlutusV2(vec![0xCA, 0xFE]));
        utxo_set.insert(ref_input.clone(), ref_output);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x12; 32]));
        tx.body.reference_inputs.push(ref_input);
        tx.body.script_data_hash = Some(Hash32::from_bytes([0xAB; 32]));

        let params = ProtocolParameters::mainnet_defaults();
        let mut errors = Vec::new();
        check_script_data_hash(&tx, &utxo_set, &params, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::UnexpectedScriptDataHash)),
            "Expected UnexpectedScriptDataHash even with an unused ref script; got {errors:?}"
        );
    }

    /// Issue #790(b): a supplemental-datum-only tx (no redeemers, no
    /// witness Plutus scripts) with the WRONG `script_data_hash` must be
    /// rejected with `ScriptDataHashMismatch`. This is exactly the case
    /// `has_plutus_scripts` misses (it does not count `plutus_data`) —
    /// `check_script_data_hash` itself already self-gates on
    /// `has_redeemers || has_datums`, so calling it directly here confirms
    /// the datum-only path is validated at the unit level; the
    /// `validate_transaction` caller now invokes it unconditionally so
    /// this path is reachable end-to-end too.
    #[test]
    fn test_script_data_hash_supplemental_datum_only_wrong_hash_rejected() {
        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x13; 32]));
        tx.witness_set
            .plutus_data
            .push(dugite_primitives::transaction::PlutusData::Integer(
                num_bigint::BigInt::from(0),
            ));
        tx.body.script_data_hash = Some(Hash32::from_bytes([0x00; 32])); // deliberately wrong

        let utxo_set = UtxoSet::new();
        let params = ProtocolParameters::mainnet_defaults();
        let mut errors = Vec::new();
        check_script_data_hash(&tx, &utxo_set, &params, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ScriptDataHashMismatch { .. })),
            "Expected ScriptDataHashMismatch for wrong hash on a datum-only tx; got {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Fix #743 — ref-script fee must be PV>=9 only
    // -----------------------------------------------------------------------

    /// Babbage-era tx (PV=7) that spends a UTxO carrying a reference script
    /// MUST NOT be charged a ref-script fee.  Before Fix #743 `compute_min_fee`
    /// unconditionally called `ref_script_fee`, so this would return a non-zero
    /// rs_fee even at PV<9.
    ///
    /// Haskell: Babbage `getMinFeeTxUtxo` = `getShelleyMinFeeTxUtxo pp tx` (no
    /// ref-script-byte charge).  The term first appears in Conway
    /// `getConwayMinFeeTxUtxo` (tierRefScriptFee, PV9+).
    /// babbage.md §5 "Reference script fee (Babbage: NONE)".
    #[test]
    fn test_no_ref_script_fee_at_pv7() {
        // A Plutus V2 script (5 bytes) attached to a UTxO.
        let script_bytes = vec![0x01u8, 0x02, 0x03, 0x04, 0x05];
        let spending_input = tx_input(0xAB);
        let mut output = simple_output();
        output.script_ref = Some(ScriptRef::PlutusV2(script_bytes));
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(spending_input.clone(), output);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x11; 32]));
        tx.body.inputs.push(spending_input);

        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 7; // Babbage
        params.min_fee_ref_script_cost_per_byte = Rational {
            numerator: 15,
            denominator: 1,
        };

        let tx_size: u64 = 200;
        let fee = compute_min_fee(&tx, &utxo_set, &params, tx_size);
        // At PV7 there must be no ref-script fee component.
        let expected_no_rs = Lovelace(params.min_fee(fee_tx_size(&tx, tx_size)).0);
        assert_eq!(
            fee, expected_no_rs,
            "Babbage (PV7) must not charge ref-script fee: got {fee:?}, expected {expected_no_rs:?}"
        );
    }

    /// Conway-era tx (PV=9) that spends a UTxO carrying a reference script
    /// MUST be charged a ref-script fee via the tiered formula.
    ///
    /// Haskell: Conway `getConwayMinFeeTxUtxo pp tx refScriptsSize` adds
    /// `tierRefScriptFee` (conway.md §12).
    #[test]
    fn test_ref_script_fee_at_pv9() {
        // A Plutus V2 script (5 bytes) attached to a UTxO.
        let script_bytes = vec![0x01u8, 0x02, 0x03, 0x04, 0x05];
        let script_size = script_bytes.len() as u64;
        let spending_input = tx_input(0xAB);
        let mut output = simple_output();
        output.script_ref = Some(ScriptRef::PlutusV2(script_bytes));
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(spending_input.clone(), output);

        let mut tx = Transaction::empty_with_hash(TransactionHash::from_bytes([0x22; 32]));
        tx.body.inputs.push(spending_input);

        let mut params = ProtocolParameters::mainnet_defaults();
        params.protocol_version_major = 9; // Conway
        params.min_fee_ref_script_cost_per_byte = Rational {
            numerator: 15,
            denominator: 1,
        };

        let tx_size: u64 = 200;
        let fee = compute_min_fee(&tx, &utxo_set, &params, tx_size);

        // At PV9 ref-script fee = tiered fee for 5 bytes at 15/byte = 5*15 = 75.
        let expected_rs = calculate_ref_script_tiered_fee(15, script_size);
        assert!(
            expected_rs > 0,
            "tiered fee for 5 bytes must be positive, got {expected_rs}"
        );
        let expected = Lovelace(params.min_fee(fee_tx_size(&tx, tx_size)).0 + expected_rs);
        assert_eq!(
            fee, expected,
            "Conway (PV9) must charge ref-script fee: got {fee:?}, expected {expected:?}"
        );
    }

    /// Issue #788: a UTxO carrying a `script_ref` that is listed as BOTH a
    /// spending input and a reference input must have its script bytes
    /// counted ONCE, not twice. Haskell: `inputs txb `Set.union`
    /// referenceInputs txb` — `.chain()` (concatenation) double-counts a
    /// shared TxIn, inflating the tiered ref-script fee and causing a
    /// false `FeeTooSmall` rejection of an otherwise-honest PV11 block.
    #[test]
    fn test_calculate_ref_script_size_dedups_shared_input() {
        let script_bytes = vec![0x01u8, 0x02, 0x03, 0x04, 0x05]; // 5 bytes
        let shared_input = tx_input(0x9A);
        let mut output = simple_output();
        output.script_ref = Some(ScriptRef::PlutusV2(script_bytes.clone()));
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(shared_input.clone(), output);

        // The SAME TxIn listed as both a spending input AND a reference
        // input (legal at PV >= 11 for non-V3 txs, per the disjointness
        // relaxation).
        let size = calculate_ref_script_size(
            std::slice::from_ref(&shared_input),
            std::slice::from_ref(&shared_input),
            &utxo_set,
        );

        assert_eq!(
            size,
            script_bytes.len() as u64,
            "a script_ref on a TxIn present in both inputs and reference_inputs \
             must be counted exactly once, got {size}"
        );
    }

    /// Boundary test: exactly one full 25,600-byte tier at PV=10.
    /// Haskell tierRefScriptFee: n < sizeIncrement → Coin $ floor (acc + toRational n * curTierPrice)
    /// For n=25,600 at base=15: fee = floor(0 + 25600 * 15) = 384,000.
    #[test]
    fn test_tiered_fee_boundary_one_tier_25600() {
        let fee = calculate_ref_script_tiered_fee(15, 25_600);
        assert_eq!(
            fee, 384_000,
            "25,600 bytes at 15/byte = 384,000 (single tier)"
        );
    }

    /// Boundary test: two full tiers (51,200 bytes) at PV=10.
    /// Tier 0: 25,600 * 15 = 384,000; Tier 1: 25,600 * 18 = 460,800; total = 844,800.
    #[test]
    fn test_tiered_fee_boundary_two_tiers_51200() {
        let fee = calculate_ref_script_tiered_fee(15, 51_200);
        assert_eq!(
            fee, 844_800,
            "51,200 bytes: tier0=384,000 + tier1=460,800 = 844,800"
        );
    }

    /// Boundary test: partial tier 3 at 80,000 bytes.
    /// Tier 0: 15/1,       25600 bytes → 384,000
    /// Tier 1: 18/1,       25600 bytes → 460,800
    /// Tier 2: 108/5,      25600 bytes → 2,764,800/5 = 552,960
    /// Tier 3: 648/25,     3200 bytes  → 2,073,600/25 = 82,944
    /// Total = 384,000 + 460,800 + 552,960 + 82,944 = 1,480,704.
    #[test]
    fn test_tiered_fee_boundary_80000() {
        let fee = calculate_ref_script_tiered_fee(15, 80_000);
        assert_eq!(fee, 1_480_704, "80,000 bytes tiered fee must be 1,480,704");
    }

    // -----------------------------------------------------------------------
    // Mainnet pin fixtures (#743/#744): three REAL Babbage transactions whose
    // dugite-vs-Haskell minimum-fee arithmetic was verified to the lovelace
    // against Koios mainnet ground truth during the 2026-06-12 FeeTooSmall
    // divergence investigation. These pin the END-TO-END pipeline:
    // standalone decode → raw_cbor → fee_tx_size (−1 is_valid) → 44·size +
    // min_fee_b + single-ceiling ExUnits fee, with NO ref-script term at PV7.
    //
    //   tx 1a36a841… (ep367, Minswap swap): wire 862 B → fee-size 861,
    //     eu = ceil(820,114·577/10⁴ + 270,993,264·721/10⁷) = 66,860
    //     min = 44·861 + 155,381 + 66,860 = 260,125 (fee paid: 260,345)
    //   tx 495d4ac7… : wire 864 B → fee-size 863, eu = 67,550 → min 260,903
    //   tx 47dab938… (ep380, 2 redeemers): wire 1,022 B → fee-size 1,021,
    //     eu = ceil over the SUMMED units (mem 3,452,498 / steps
    //     1,074,904,790) = 276,710 → min 477,015 (fee paid: 485,947)
    //
    // Pre-#743 dugite reported minimums 260,656 / 261,346 / 642,331 for these
    // very transactions (phantom Conway ref-script fee + missing size term),
    // producing 59,574 false FeeTooSmall divergences on mainnet ep367-385.
    // -----------------------------------------------------------------------

    const MAINNET_TX_1A36A841: &str = include_str!("fixtures/tx-1a36a841.hex");
    const MAINNET_TX_495D4AC7: &str = include_str!("fixtures/tx-495d4ac7.hex");
    const MAINNET_TX_47DAB938: &str = include_str!("fixtures/tx-47dab938.hex");

    fn hex_to_bytes(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    /// Mainnet Babbage protocol parameters as of epoch 367-385 (the regime the
    /// fixtures come from): a=44, b=155,381, prices mem 577/10⁴ steps 721/10⁷,
    /// PV7, Conway ref-script price already present in the params struct (15)
    /// — which must be IGNORED at PV<9.
    fn mainnet_babbage_params() -> ProtocolParameters {
        let mut params = ProtocolParameters::mainnet_defaults();
        params.min_fee_a = 44;
        params.min_fee_b = 155_381;
        params.protocol_version_major = 7;
        params.min_fee_ref_script_cost_per_byte = Rational {
            numerator: 15,
            denominator: 1,
        };
        params.execution_costs.mem_price.numerator = 577;
        params.execution_costs.mem_price.denominator = 10_000;
        params.execution_costs.step_price.numerator = 721;
        params.execution_costs.step_price.denominator = 10_000_000;
        params
    }

    fn pin_min_fee(hex: &str, expected_wire_len: usize, expected_min_fee: u64) {
        let bytes = hex_to_bytes(hex);
        assert_eq!(bytes.len(), expected_wire_len, "fixture wire length");
        let tx =
            dugite_serialization::decode::decode_transaction(5, &bytes).expect("decode Babbage tx");
        assert_eq!(
            tx.raw_cbor.as_ref().map(|c| c.len()),
            Some(expected_wire_len),
            "standalone decode must preserve full wire bytes"
        );
        let params = mainnet_babbage_params();
        // Empty UTxO set: at PV7 the ref-script term must not apply, and the
        // base+eu terms do not consult the UTxO set.
        let utxo_set = UtxoSet::new();
        let fee = compute_min_fee(&tx, &utxo_set, &params, bytes.len() as u64);
        assert_eq!(
            fee.0, expected_min_fee,
            "Haskell-exact Babbage minimum fee for the pinned mainnet tx"
        );
    }

    #[test]
    fn pin_mainnet_babbage_min_fee_tx_1a36a841() {
        pin_min_fee(MAINNET_TX_1A36A841, 862, 260_125);
    }

    #[test]
    fn pin_mainnet_babbage_min_fee_tx_495d4ac7() {
        pin_min_fee(MAINNET_TX_495D4AC7, 864, 260_903);
    }

    #[test]
    fn pin_mainnet_babbage_min_fee_tx_47dab938() {
        pin_min_fee(MAINNET_TX_47DAB938, 1_022, 477_015);
    }

    /// The same Minswap fixture evaluated at PV9 with its 2,561-byte PlutusV2
    /// reference script resolvable in the UTxO set MUST charge the tiered
    /// ref-script fee: 260,125 + 15·2,561 = 298,540 (single tier).  This pins
    /// the PV gate from both sides with real mainnet data.
    #[test]
    fn pin_mainnet_min_fee_pv9_adds_ref_script_term() {
        let bytes = hex_to_bytes(MAINNET_TX_1A36A841);
        let tx =
            dugite_serialization::decode::decode_transaction(5, &bytes).expect("decode Babbage tx");
        let ref_input = tx
            .body
            .reference_inputs
            .first()
            .expect("fixture has a reference input")
            .clone();
        let mut output = simple_output();
        output.script_ref = Some(ScriptRef::PlutusV2(vec![0u8; 2_561]));
        let mut utxo_set = UtxoSet::new();
        utxo_set.insert(ref_input, output);

        let mut params = mainnet_babbage_params();
        params.protocol_version_major = 9;
        let fee = compute_min_fee(&tx, &utxo_set, &params, bytes.len() as u64);
        assert_eq!(
            fee.0,
            260_125 + 38_415,
            "PV9 must add tiered ref-script fee (2,561 B × 15 = 38,415)"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #793 — estimate_value_cbor_size must equal the EXACT serialized
    // length across randomly-generated Values (multi-asset under-counting
    // regression guard).
    // -----------------------------------------------------------------------
    mod proptests {
        use super::*;
        use dugite_primitives::value::AssetName;
        use proptest::collection::{btree_map, vec as pvec};
        use proptest::prelude::*;

        /// Generate an arbitrary `AssetName` (0-32 bytes, per Cardano's cap).
        fn asset_name_strategy() -> impl Strategy<Value = AssetName> {
            pvec(any::<u8>(), 0..=32).prop_map(AssetName)
        }

        /// Generate an arbitrary 28-byte policy ID.
        fn policy_strategy() -> impl Strategy<Value = dugite_primitives::hash::Hash28> {
            pvec(any::<u8>(), 28..=28)
                .prop_map(|b| dugite_primitives::hash::Hash28::from_bytes(b.try_into().unwrap()))
        }

        /// Generate an arbitrary multi-asset `Value`: a random coin plus 0-4
        /// policies, each with 0-4 assets, exercising both short (<24-byte)
        /// and long CBOR header cases for names, policy counts, and asset
        /// counts (map headers >= 24 entries are exercised implicitly by
        /// birthday-collision-free random policies at the upper end of the
        /// range — the header-size formula itself is validated exactly by
        /// `dugite_serialization::encode_value`, so this proptest's job is
        /// only to confirm `estimate_value_cbor_size` never drifts from it).
        fn value_strategy() -> impl Strategy<Value = Value> {
            (
                any::<u64>(),
                btree_map(
                    policy_strategy(),
                    btree_map(asset_name_strategy(), any::<u64>(), 0..=4),
                    0..=4,
                ),
            )
                .prop_map(|(coin, multi_asset)| Value {
                    coin: Lovelace(coin),
                    multi_asset,
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(500))]

            /// `estimate_value_cbor_size` must always equal the TRUE
            /// serialized byte length (`encode_value(v).len()`), matching
            /// Haskell `validateOutputTooBigUTxO`'s exact `serSize`.
            #[test]
            fn estimate_value_cbor_size_matches_true_serialized_length(v in value_strategy()) {
                let estimated = estimate_value_cbor_size(&v);
                let exact = dugite_serialization::encode_value(&v).len() as u64;
                prop_assert_eq!(estimated, exact);
            }
        }
    }
}
