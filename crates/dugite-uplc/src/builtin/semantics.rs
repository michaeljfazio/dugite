//! `BuiltinSemanticsVariant` — the per-(language, protocol-version) flag that
//! selects between alternative *result-level* denotations for a small set of
//! builtins.
//!
//! ## Why this exists
//!
//! A handful of Plutus builtins changed their **observable result** (not just
//! their cost) across protocol versions. The canonical example — and the only
//! one this type currently gates — is `consByteString`:
//!
//! * **Lenient** (`fromIntegral :: Integer -> Word8`, modular/Euclidean): the
//!   integer argument is reduced mod 256 to a byte and NEVER errors
//!   (256 → 0x00, 257 → 0x01, -1 → 0xFF, -256 → 0x00).
//! * **Strict** (`Word8` argument): the integer must be in `0..=255` or the
//!   builtin raises a `BuiltinFailure`.
//!
//! IntersectMBO/plutus encodes this with the `BuiltinSemanticsVariant`
//! associated type of `DefaultFun` (variants A, B, C, D, E). The mapping from
//! `(LedgerPlutusVersion, MajorProtocolVersion)` to a variant is given by
//! `PlutusLedgerApi.Common.Versions`
//! (Note [Mapping of protocol versions and ledger languages to semantics
//! variants]) and the per-language `EvaluationContext.hs`:
//!
//! ```text
//!   PlutusV1, PlutusV2:  pv < changPV (9)      -> A   (lenient consByteString)
//!                        changPV <= pv < vanRossemPV (11) -> B   (lenient)
//!                        pv >= vanRossemPV      -> D   (lenient)
//!   PlutusV3:            pv < vanRossemPV       -> C   (strict consByteString)
//!                        pv >= vanRossemPV      -> E   (strict)
//! ```
//!
//! Net load-bearing rule: **`consByteString` is strict iff the script language
//! is PlutusV3** — V1/V2 are lenient at every protocol version.
//!
//! Source: IntersectMBO/plutus `Builtins.hs`
//! (`consByteStringMeaning_V1`/`consByteStringMeaning_V2`) — see
//! <https://github.com/IntersectMBO/plutus/blob/d3c8d752/plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs>
//! (commit `d3c8d752`, cross-checked with `bddbf4b1`): the V1/V2 meaning uses
//! `BS.cons . fromIntegral` (lenient) while the V3 meaning takes a `Word8`
//! argument (strict, range-checked by the unlifting layer).
//!
//! The cost-model SHAPE differences between these variants are handled
//! independently in [`crate::cost_apply`] (see `is_variant_b`); this type only
//! governs the *result* of `consByteString`.

use crate::redeemer_resolve::ScriptLanguage;

/// First protocol version at which the Chang/Conway hard fork is active
/// (PlutusV1/V2 move from variant A → B). Mirrors `changPV` in
/// `PlutusLedgerApi.Common.Versions`.
pub const CHANG_PV: u32 = 9;

/// First protocol version at which the Plomin/van Rossem hard fork is active
/// (PlutusV1/V2 move B → D, PlutusV3 moves C → E). Mirrors `vanRossemPV`.
pub const VAN_ROSSEM_PV: u32 = 11;

/// The Plutus `BuiltinSemanticsVariant DefaultFun` selected for a script.
///
/// Derived once from `(ScriptLanguage, major_pv)` via
/// [`SemanticsVariant::for_script`] and threaded into the CEK denotation
/// layer. A small `Copy` value — pass it by value everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticsVariant {
    /// PlutusV1/V2, `pv < changPV`. Lenient `consByteString`.
    A,
    /// PlutusV1/V2, `changPV <= pv < vanRossemPV`. Lenient `consByteString`.
    B,
    /// PlutusV3, `pv < vanRossemPV`. Strict `consByteString`.
    C,
    /// PlutusV1/V2, `pv >= vanRossemPV`. Lenient `consByteString`.
    D,
    /// PlutusV3, `pv >= vanRossemPV`. Strict `consByteString`.
    E,
}

/// The latest / most-conservative variant. Standalone callers — the UPLC
/// conformance harness and the CEK unit tests, none of which carry a
/// `(language, pv)` context — default to this so their `consByteString`
/// behaviour stays STRICT (matching the V3-generated conformance vectors).
pub const LATEST: SemanticsVariant = SemanticsVariant::E;

impl Default for SemanticsVariant {
    /// STRICT / latest. See [`LATEST`].
    fn default() -> Self {
        LATEST
    }
}

impl SemanticsVariant {
    /// The latest / most-conservative variant (STRICT `consByteString`).
    pub const LATEST: SemanticsVariant = LATEST;

    /// Map `(language, major_pv)` to the Plutus `BuiltinSemanticsVariant`,
    /// mirroring IntersectMBO/plutus EXACTLY:
    ///
    /// * PlutusV1 | PlutusV2 → `A` (pv < 9), `B` (9 ≤ pv < 11), `D` (pv ≥ 11)
    /// * PlutusV3 | PlutusV4 → `C` (pv < 11), `E` (pv ≥ 11)
    ///
    /// PlutusV4 (Dijkstra): `IntersectMBO/plutus`'s own `PlutusLedgerLanguage`
    /// sum type has NO `PlutusV4` constructor at all (still-open
    /// `plutus#7342`), so there is no V4-specific `BuiltinSemanticsVariant` —
    /// `BuiltinSemanticsVariant DefaultFun` has exactly 5 constructors (A-E),
    /// no F. cardano-ledger's `PlutusV4` wraps V3's evaluation context
    /// verbatim, so V4 gets the V3 branch. In practice this is always `E`:
    /// V4 only exists at PV ≥ 12 (`guardPlutus: PlutusV4 -> natVersion @12`),
    /// which is always ≥ `VAN_ROSSEM_PV` (11). See `ScriptLanguage`'s doc
    /// comment (`dugite-uplc/src/redeemer_resolve.rs`) for the full citation.
    pub fn for_script(language: ScriptLanguage, major_pv: u32) -> Self {
        match language {
            ScriptLanguage::PlutusV1 | ScriptLanguage::PlutusV2 => {
                if major_pv < CHANG_PV {
                    SemanticsVariant::A
                } else if major_pv < VAN_ROSSEM_PV {
                    SemanticsVariant::B
                } else {
                    SemanticsVariant::D
                }
            }
            ScriptLanguage::PlutusV3 | ScriptLanguage::PlutusV4 => {
                if major_pv < VAN_ROSSEM_PV {
                    SemanticsVariant::C
                } else {
                    SemanticsVariant::E
                }
            }
        }
    }

    /// Whether `consByteString` uses the STRICT (Word8, range-checked)
    /// denotation. `true` for the PlutusV3 variants (`C`, `E`); `false` for
    /// the lenient PlutusV1/V2 variants (`A`, `B`, `D`).
    pub fn cons_byte_string_strict(self) -> bool {
        matches!(self, SemanticsVariant::C | SemanticsVariant::E)
    }

    /// Whether the bitwise builtins enforce the `maximumInputLength` (4096-byte
    /// input) cap on `writeBits`. Mirrors Plutus `ensurable` (Builtins.hs): true
    /// for D and E.
    pub fn bitwise_max_input_enforced(self) -> bool {
        matches!(self, SemanticsVariant::D | SemanticsVariant::E)
    }

    /// Whether `appendString`/`equalsString`/`encodeUtf8` size their `Text`
    /// argument(s) by UTF-8 byte-length ÷ 4 (`TextCostedByByteLength`).
    /// `true` for D/E (PV ≥ `VAN_ROSSEM_PV`); `false` for A/B/C
    /// (PV < `VAN_ROSSEM_PV`), which use plain `ExMemoryUsage Text` =
    /// character count. See issue #819 and
    /// `PlutusCore/Default/Builtins.hs:1499-1579`
    /// (`*_V1` char-count vs `*_V2` byte/4).
    pub fn text_costed_by_byte_length(self) -> bool {
        matches!(self, SemanticsVariant::D | SemanticsVariant::E)
    }

    /// Whether the PLC 1.1.0 `case`-on-Constant ("caser builtin") feature is
    /// available. `true` for D/E (PV ≥ `VAN_ROSSEM_PV`); `false` for A/B/C,
    /// where Haskell's `unCaserBuiltin` is `unavailableCaserBuiltin` and a
    /// `case` whose scrutinee reduces to a plain constant fails with
    /// `CekCaseBuiltinError`. `case` on a `Constr` scrutinee is unaffected
    /// by this gate. See issue #824.
    pub fn case_on_constant_available(self) -> bool {
        matches!(self, SemanticsVariant::D | SemanticsVariant::E)
    }

    /// Whether `constrData`'s tag argument is unlifted as `Word64` (range
    /// `0..=2^64-1`, a genuine evaluation failure outside that range).
    /// `true` for D/E (PV ≥ `VAN_ROSSEM_PV`); `false` for A/B/C, where the
    /// argument type is plain `Integer` (no bound). See issues #828.5/#859
    /// — `Data::Constr`'s tag field is an arbitrary-precision `BigInt`
    /// (matching Haskell's `Integer` exactly), so the A/B/C branch holds
    /// precisely the same domain as Haskell with no representational gap.
    pub fn constr_data_requires_word64(self) -> bool {
        matches!(self, SemanticsVariant::D | SemanticsVariant::E)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_script_maps_v1_v2_to_lenient_variants() {
        // PlutusV1 / V2 → A (pv<9), B (9..11), D (pv>=11) — all lenient.
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV1, 8),
            SemanticsVariant::A
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV2, 8),
            SemanticsVariant::A
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV1, 9),
            SemanticsVariant::B
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV2, 10),
            SemanticsVariant::B
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV1, 11),
            SemanticsVariant::D
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV2, 12),
            SemanticsVariant::D
        );
    }

    /// Issue #1000 (PlutusV4/Dijkstra): V4 must map to EXACTLY the same
    /// variant as V3 at every PV — `IntersectMBO/plutus`'s
    /// `BuiltinSemanticsVariant DefaultFun` has exactly 5 constructors
    /// (A-E), no V4-specific F, and cardano-ledger's `PlutusV4` wraps V3's
    /// evaluation context verbatim. Also pins that V4's REAL range (PV >=
    /// 12, Dijkstra's introduction PV) always lands on `E` — V4 can never
    /// actually observe `C` on a real chain, even though the formula is
    /// written generally to mirror V3's.
    #[test]
    fn for_script_maps_v4_identically_to_v3() {
        for pv in 0u32..=15 {
            assert_eq!(
                SemanticsVariant::for_script(ScriptLanguage::PlutusV3, pv),
                SemanticsVariant::for_script(ScriptLanguage::PlutusV4, pv),
                "V3 and V4 must select the identical variant at pv={pv}"
            );
        }
        // V4's real range (PV >= 12) always resolves to E.
        for pv in 12u32..=15 {
            assert_eq!(
                SemanticsVariant::for_script(ScriptLanguage::PlutusV4, pv),
                SemanticsVariant::E
            );
        }
    }

    #[test]
    fn for_script_maps_v3_to_strict_variants() {
        // PlutusV3 → C (pv<11), E (pv>=11) — both strict.
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV3, 8),
            SemanticsVariant::C
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV3, 10),
            SemanticsVariant::C
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV3, 11),
            SemanticsVariant::E
        );
        assert_eq!(
            SemanticsVariant::for_script(ScriptLanguage::PlutusV3, 12),
            SemanticsVariant::E
        );
    }

    #[test]
    fn cons_byte_string_strict_only_for_v3_variants() {
        assert!(!SemanticsVariant::A.cons_byte_string_strict());
        assert!(!SemanticsVariant::B.cons_byte_string_strict());
        assert!(!SemanticsVariant::D.cons_byte_string_strict());
        assert!(SemanticsVariant::C.cons_byte_string_strict());
        assert!(SemanticsVariant::E.cons_byte_string_strict());
    }

    #[test]
    fn bitwise_max_input_enforced_only_for_d_and_e() {
        // `writeBits` enforces the 4096-byte maximumInputLength cap only under
        // variants D and E (Plutus `ensurable`). A/B/C impose no cap.
        assert!(!SemanticsVariant::A.bitwise_max_input_enforced());
        assert!(!SemanticsVariant::B.bitwise_max_input_enforced());
        assert!(!SemanticsVariant::C.bitwise_max_input_enforced());
        assert!(SemanticsVariant::D.bitwise_max_input_enforced());
        assert!(SemanticsVariant::E.bitwise_max_input_enforced());
    }

    #[test]
    fn default_and_latest_are_strict() {
        assert_eq!(SemanticsVariant::default(), SemanticsVariant::E);
        assert_eq!(SemanticsVariant::LATEST, SemanticsVariant::E);
        assert!(SemanticsVariant::default().cons_byte_string_strict());
    }
}
