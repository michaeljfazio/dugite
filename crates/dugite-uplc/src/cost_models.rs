//! Plutus cost-models wire-format decoder.
//!
//! The ledger passes `cost_models_cbor` to [`crate::phase_two::eval_phase_two_raw`]
//! as a CBOR map keyed by Plutus language version:
//!
//! ```text
//! cost_models = { ? 0 => [* int]   ; PlutusV1
//!               , ? 1 => [* int]   ; PlutusV2
//!               , ? 2 => [* int]   ; PlutusV3
//!               , ? 3 => [* int]   ; PlutusV4 (Dijkstra, issue #475 Phase 5)
//!               }
//! ```
//!
//! Each value is a flat array of per-parameter cost coefficients in the
//! canonical Plutus order (see `PlutusLedgerApi.Common.Versions` in the
//! Haskell reference). Decoding here only lifts the bytes into typed
//! `Vec<i64>` arrays — interpreting each entry against a particular
//! builtin lives one layer up (UPLC-9 part 4, per-redeemer CEK eval).
//!
//! ## Sizing
//!
//! Canonical Plutus parameter counts (V1=166, V2=185, V3=297 as of
//! PV11) are documented for reference but **not enforced** at the
//! decode boundary. cardano-node accepts on-chain ParameterChange
//! actions that grow or shrink the array as new builtins land (see
//! e.g. CIP-0117 / CIP-0123) — the decoder must tolerate arrays of
//! arbitrary size up to a defensive cap so adversarial cost models
//! cannot trigger an unbounded `Vec::with_capacity`. V4 (Dijkstra)
//! ships at the same 297-entry length as V3 (no new builtins in
//! PV1.2.0 vs PV1.1.0 per upstream `IntersectMBO/plutus` master).
//!
//! ## Adversarial-input contract
//!
//! Every public entry in this module returns `Result` and never
//! panics on malformed CBOR (per `lib.rs` §1). Length headers are
//! sanity-clamped before any `Vec::with_capacity` (per §2). Recursion
//! is bounded: we descend at most one level (top-level map → array
//! per language). Unknown map keys are skipped rather than rejected,
//! mirroring cardano-node's "future-language" tolerance.

use crate::phase_two::PhaseTwoError;
use minicbor::data::Type;
use minicbor::Decoder;

/// Defensive upper bound on the number of cost-model parameters per
/// Plutus language version. The Haskell reference currently tops out
/// at 297 (PlutusV3 at PV11); we pick a comfortable headroom so
/// future builtins land without code changes, but small enough that
/// an attacker cannot trick the decoder into an unbounded allocation.
const MAX_PARAMS_PER_VERSION: usize = 1024;

/// Defensive upper bound on the number of top-level map keys. The
/// canonical encoding has at most three entries (V1/V2/V3). We allow
/// a bit more to tolerate forward-compat ParameterChange actions
/// that introduce a new language version, but reject any encoding
/// that names hundreds of versions.
const MAX_VERSIONS: usize = 16;

/// CBOR map key for the V1 language. Matches `to_cbor` in
/// `dugite_primitives::transaction::CostModels`.
const KEY_V1: u32 = 0;
const KEY_V2: u32 = 1;
const KEY_V3: u32 = 2;
/// PlutusV4 cost-model slot (Dijkstra, issue #475 Phase 5).
const KEY_V4: u32 = 3;

/// Per-version cost-model coefficient arrays. Each `Vec<i64>` is the
/// raw flat array of parameter values in Plutus canonical order.
/// Versions absent from the wire encoding remain `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostModels {
    pub plutus_v1: Option<Vec<i64>>,
    pub plutus_v2: Option<Vec<i64>>,
    pub plutus_v3: Option<Vec<i64>>,
    /// PlutusV4 cost model (Dijkstra). Wire slot is map key 3.
    pub plutus_v4: Option<Vec<i64>>,
}

impl CostModels {
    /// True if the wire encoding declared no Plutus version at all.
    /// Callers (phase-2 evaluator) treat this as "no cost model
    /// configured" and use the CEK machine's default per-step cost
    /// constants from `machine::cost`.
    pub fn is_empty(&self) -> bool {
        self.plutus_v1.is_none()
            && self.plutus_v2.is_none()
            && self.plutus_v3.is_none()
            && self.plutus_v4.is_none()
    }
}

/// Decode a cost-models CBOR blob into [`CostModels`].
///
/// Returns [`PhaseTwoError::CostModelDecode`] for any malformed CBOR
/// (non-map top level, oversize length headers, non-integer entries,
/// truncated input, etc.) so the caller can fail the tx cleanly
/// without panicking.
pub fn decode_cost_models_cbor(cbor: &[u8]) -> Result<CostModels, PhaseTwoError> {
    let mut d = Decoder::new(cbor);
    let map_len = d
        .map()
        .map_err(|e| PhaseTwoError::CostModelDecode(format!("expected top-level map: {e}")))?;
    let mut out = CostModels::default();

    // Strict caps: definite-length maps are checked against MAX_VERSIONS up
    // front. Indefinite-length maps are walked entry-by-entry with the same
    // cap enforced as we go.
    match map_len {
        Some(n) => {
            let n_usize: usize = n.try_into().map_err(|_| {
                PhaseTwoError::CostModelDecode(format!("map length {n} exceeds platform usize"))
            })?;
            if n_usize > MAX_VERSIONS {
                return Err(PhaseTwoError::CostModelDecode(format!(
                    "map length {n_usize} exceeds defensive cap {MAX_VERSIONS}"
                )));
            }
            for _ in 0..n_usize {
                consume_one_entry(&mut d, &mut out)?;
            }
        }
        None => {
            // Indefinite-length map: read until the break code.
            let mut seen: usize = 0;
            loop {
                if d.datatype()
                    .map_err(|e| PhaseTwoError::CostModelDecode(format!("peek datatype: {e}")))?
                    == Type::Break
                {
                    d.skip().map_err(|e| {
                        PhaseTwoError::CostModelDecode(format!("consume break: {e}"))
                    })?;
                    break;
                }
                if seen >= MAX_VERSIONS {
                    return Err(PhaseTwoError::CostModelDecode(format!(
                        "indefinite map exceeded defensive cap {MAX_VERSIONS}"
                    )));
                }
                consume_one_entry(&mut d, &mut out)?;
                seen += 1;
            }
        }
    }

    Ok(out)
}

/// Consume one (key, value) pair from the top-level map.
///
/// Known keys (0/1/2/3 → V1/V2/V3/V4) populate the matching field on
/// [`CostModels`]. Unknown keys are skipped — both the key (a single
/// CBOR integer the decoder already consumed) and the value (which
/// `skip` walks past in a single call).
///
/// Duplicate occurrences of the same known key overwrite the prior
/// value; this matches the CBOR-canonical semantics that the **last**
/// occurrence wins, and lines up with how `minicbor::Decoder::map`
/// surfaces the entries.
fn consume_one_entry(d: &mut Decoder<'_>, out: &mut CostModels) -> Result<(), PhaseTwoError> {
    // The key may be encoded as any unsigned integer width. We accept
    // u8/u16/u32 and reject anything broader (the keys are tiny enums).
    let key = d
        .u32()
        .map_err(|e| PhaseTwoError::CostModelDecode(format!("map key not a small uint: {e}")))?;
    match key {
        KEY_V1 => out.plutus_v1 = Some(read_int_array(d, "V1")?),
        KEY_V2 => out.plutus_v2 = Some(read_int_array(d, "V2")?),
        KEY_V3 => out.plutus_v3 = Some(read_int_array(d, "V3")?),
        KEY_V4 => out.plutus_v4 = Some(read_int_array(d, "V4")?),
        unknown => {
            // Forward-compat: skip the value associated with an unknown
            // version key rather than rejecting the whole encoding.
            tracing::debug!(
                key = unknown,
                "cost_models: skipping unknown language version key"
            );
            d.skip().map_err(|e| {
                PhaseTwoError::CostModelDecode(format!("skipping unknown key {unknown}: {e}"))
            })?;
        }
    }
    Ok(())
}

/// Read a `[* int]` array, clamping the declared length against
/// [`MAX_PARAMS_PER_VERSION`] before any allocation.
fn read_int_array(d: &mut Decoder<'_>, label: &'static str) -> Result<Vec<i64>, PhaseTwoError> {
    let arr_len = d
        .array()
        .map_err(|e| PhaseTwoError::CostModelDecode(format!("{label}: expected array: {e}")))?;
    match arr_len {
        Some(n) => {
            let n_usize: usize = n.try_into().map_err(|_| {
                PhaseTwoError::CostModelDecode(format!(
                    "{label}: array length {n} exceeds platform usize"
                ))
            })?;
            if n_usize > MAX_PARAMS_PER_VERSION {
                return Err(PhaseTwoError::CostModelDecode(format!(
                    "{label}: array length {n_usize} exceeds defensive cap \
                     {MAX_PARAMS_PER_VERSION}"
                )));
            }
            let mut out = Vec::with_capacity(n_usize);
            for i in 0..n_usize {
                let v = d.i64().map_err(|e| {
                    PhaseTwoError::CostModelDecode(format!(
                        "{label}[{i}]: not a signed integer: {e}"
                    ))
                })?;
                out.push(v);
            }
            Ok(out)
        }
        None => {
            // Indefinite array — walk to the break code, clamping as we go.
            let mut out: Vec<i64> = Vec::new();
            loop {
                let ty = d
                    .datatype()
                    .map_err(|e| PhaseTwoError::CostModelDecode(format!("{label}: peek: {e}")))?;
                if ty == Type::Break {
                    d.skip().map_err(|e| {
                        PhaseTwoError::CostModelDecode(format!("{label}: consume break: {e}"))
                    })?;
                    return Ok(out);
                }
                if out.len() >= MAX_PARAMS_PER_VERSION {
                    return Err(PhaseTwoError::CostModelDecode(format!(
                        "{label}: indefinite array exceeded defensive cap \
                         {MAX_PARAMS_PER_VERSION}"
                    )));
                }
                let v = d.i64().map_err(|e| {
                    PhaseTwoError::CostModelDecode(format!(
                        "{label}[{i}]: not a signed integer: {e}",
                        i = out.len()
                    ))
                })?;
                out.push(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build CBOR `map<u8, [int]>` matching the ledger's encoding.
    fn build_map_cbor(entries: &[(u32, &[i64])]) -> Vec<u8> {
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.map(entries.len() as u64).unwrap();
        for (key, costs) in entries {
            enc.u32(*key).unwrap();
            enc.array(costs.len() as u64).unwrap();
            for c in *costs {
                enc.i64(*c).unwrap();
            }
        }
        buf
    }

    #[test]
    fn decodes_single_v3_entry() {
        let cbor = build_map_cbor(&[(KEY_V3, &[1, 2, 3, -7])]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert!(cm.plutus_v1.is_none());
        assert!(cm.plutus_v2.is_none());
        assert_eq!(cm.plutus_v3.as_deref(), Some(&[1i64, 2, 3, -7][..]));
        assert!(cm.plutus_v4.is_none());
        assert!(!cm.is_empty());
    }

    /// PlutusV4 cost-model slot 3 decodes into `plutus_v4`
    /// (Dijkstra, issue #475 Phase 5).
    #[test]
    fn decodes_single_v4_entry() {
        let cbor = build_map_cbor(&[(KEY_V4, &[100, 200, -1])]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert!(cm.plutus_v1.is_none());
        assert!(cm.plutus_v2.is_none());
        assert!(cm.plutus_v3.is_none());
        assert_eq!(cm.plutus_v4.as_deref(), Some(&[100i64, 200, -1][..]));
        assert!(!cm.is_empty());
    }

    #[test]
    fn decodes_all_three_versions() {
        let v1 = vec![10i64; 50];
        let v2 = vec![20i64; 60];
        let v3 = vec![30i64; 70];
        let cbor = build_map_cbor(&[(KEY_V1, &v1), (KEY_V2, &v2), (KEY_V3, &v3)]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v1.as_ref().map(|v| v.len()), Some(50));
        assert_eq!(cm.plutus_v2.as_ref().map(|v| v.len()), Some(60));
        assert_eq!(cm.plutus_v3.as_ref().map(|v| v.len()), Some(70));
        assert!(cm.plutus_v4.is_none());
        assert_eq!(cm.plutus_v1.unwrap()[0], 10);
        assert_eq!(cm.plutus_v2.unwrap()[0], 20);
        assert_eq!(cm.plutus_v3.unwrap()[0], 30);
    }

    /// All four supported versions (V1+V2+V3+V4) decode into the
    /// matching field. Dijkstra cost-model wire shape (issue #475 Phase 5).
    #[test]
    fn decodes_all_four_versions() {
        let v1 = vec![10i64; 5];
        let v2 = vec![20i64; 6];
        let v3 = vec![30i64; 7];
        let v4 = vec![40i64; 8];
        let cbor = build_map_cbor(&[(KEY_V1, &v1), (KEY_V2, &v2), (KEY_V3, &v3), (KEY_V4, &v4)]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v1.as_ref().map(|v| v.len()), Some(5));
        assert_eq!(cm.plutus_v2.as_ref().map(|v| v.len()), Some(6));
        assert_eq!(cm.plutus_v3.as_ref().map(|v| v.len()), Some(7));
        assert_eq!(cm.plutus_v4.as_ref().map(|v| v.len()), Some(8));
        assert_eq!(cm.plutus_v4.as_ref().unwrap()[0], 40);
    }

    #[test]
    fn empty_map_is_legal_and_yields_default() {
        let cbor = build_map_cbor(&[]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert!(cm.is_empty());
    }

    #[test]
    fn unknown_version_key_is_skipped_not_rejected() {
        // Mix a real V2 entry with a future-version key (99). The decoder
        // tolerates the unknown key but still surfaces the V2 entry.
        let v2 = vec![1, 2, 3];
        let cbor = build_map_cbor(&[(KEY_V2, &v2), (99, &[7, 8])]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v2.as_deref(), Some(&[1i64, 2, 3][..]));
        assert!(cm.plutus_v1.is_none());
        assert!(cm.plutus_v3.is_none());
    }

    #[test]
    fn duplicate_known_key_last_wins() {
        let cbor = build_map_cbor(&[(KEY_V1, &[1, 2]), (KEY_V1, &[9, 8, 7])]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v1.as_deref(), Some(&[9i64, 8, 7][..]));
    }

    #[test]
    fn dugite_primitives_cost_models_roundtrip() {
        use dugite_primitives::transaction::CostModels as PrimCostModels;
        let prim = PrimCostModels {
            plutus_v1: Some(vec![1, 2, 3]),
            plutus_v2: None,
            plutus_v3: Some(vec![100, 200, -300]),
            plutus_v4: Some(vec![7, 8, 9, 10]),
            ..Default::default()
        };
        let cbor = prim.to_cbor().unwrap();
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v1, prim.plutus_v1);
        assert_eq!(cm.plutus_v2, prim.plutus_v2);
        assert_eq!(cm.plutus_v3, prim.plutus_v3);
        assert_eq!(cm.plutus_v4, prim.plutus_v4);
    }

    #[test]
    fn rejects_non_map_top_level() {
        // Array, not map.
        let cbor = vec![0x80];
        let err = decode_cost_models_cbor(&cbor).unwrap_err();
        assert!(matches!(err, PhaseTwoError::CostModelDecode(_)));
    }

    #[test]
    fn rejects_empty_input() {
        let err = decode_cost_models_cbor(&[]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::CostModelDecode(_)));
    }

    #[test]
    fn rejects_truncated_value_array() {
        // Map(1) with key 0 → array(3) but only one int follows.
        let cbor = vec![0xa1, 0x00, 0x83, 0x01];
        let err = decode_cost_models_cbor(&cbor).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("V1["),
            "expected per-element error, got: {msg}"
        );
    }

    #[test]
    fn rejects_string_value() {
        // Map(1) with key 1 → text("oops").
        let mut cbor = vec![0xa1, 0x01];
        cbor.extend([0x64, b'o', b'o', b'p', b's']);
        let err = decode_cost_models_cbor(&cbor).unwrap_err();
        assert!(matches!(err, PhaseTwoError::CostModelDecode(_)));
    }

    #[test]
    fn rejects_oversize_version_count() {
        // Map(MAX_VERSIONS + 1) — declared length alone trips the cap.
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.map((MAX_VERSIONS + 1) as u64).unwrap();
        // We don't even bother encoding entries — the cap fires first.
        let err = decode_cost_models_cbor(&buf).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("defensive cap"), "got: {msg}");
    }

    #[test]
    fn rejects_oversize_param_array() {
        use minicbor::Encoder;
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.map(1).unwrap();
        enc.u32(KEY_V3).unwrap();
        enc.array((MAX_PARAMS_PER_VERSION + 1) as u64).unwrap();
        let err = decode_cost_models_cbor(&buf).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("V3") && msg.contains("defensive cap"),
            "got: {msg}"
        );
    }

    #[test]
    fn indefinite_length_map_decodes() {
        // Indefinite-length map with one V2 entry and a definite-length array.
        let cbor = vec![0xbf, 0x01, 0x83, 0x01, 0x02, 0x03, 0xff];
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v2.as_deref(), Some(&[1i64, 2, 3][..]));
    }

    #[test]
    fn indefinite_length_array_decodes() {
        // Map(1) key V1, indefinite-length array [1, 2, 3].
        let cbor = vec![0xa1, 0x00, 0x9f, 0x01, 0x02, 0x03, 0xff];
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(cm.plutus_v1.as_deref(), Some(&[1i64, 2, 3][..]));
    }

    #[test]
    fn negative_costs_are_accepted() {
        // PV-bumping ParameterChange actions can legitimately introduce
        // negative coefficients in piecewise cost models (e.g., where a
        // builtin's amortised cost has a y-intercept subtracted off).
        let cbor = build_map_cbor(&[(KEY_V3, &[-1, -2, i64::MIN / 4])]);
        let cm = decode_cost_models_cbor(&cbor).unwrap();
        assert_eq!(
            cm.plutus_v3.as_deref(),
            Some(&[-1i64, -2, i64::MIN / 4][..])
        );
    }
}
