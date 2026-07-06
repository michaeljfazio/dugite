//! Apply an on-chain flat cost-model parameter array to the CEK machine's
//! [`MachineCosts`] (per-step costs) and [`BuiltinCosts`] (per-builtin
//! costs), byte-exact per Plutus language version.
//!
//! ## Why this exists
//!
//! The ledger transmits each Plutus language's cost model as a flat array of
//! integers in the canonical `ParamName` order (PlutusV1 = 166 entries). The
//! CEK machine previously always charged with a single hard-coded default
//! cost model (the *latest* / Conway-era model the conformance corpus is
//! computed against), regardless of which era's block was being validated.
//! That is correct for the current protocol version but **wrong for
//! historical eras**: mainnet Alonzo (epoch ~290) ran the original Alonzo
//! PlutusV1 cost model, whose coefficients — and, for the four integer
//! division builtins plus `multiplyInteger`, whose cost-function *shapes* —
//! differ from the latest model. Charging the wrong model both mis-reports
//! consumed `ExBudget` (causing false phase-2 failures where the chain
//! accepted the tx) and, because the memory dimension of `ExBudget` is the
//! *only* mechanism bounding allocation in the CEK machine
//! (`UntypedPlutusCore.Evaluation.Machine.Cek.ExBudgetMode`), lets an
//! under-charged builtin allocate unboundedly.
//!
//! ## Canonical ordering (byte-exact vs IntersectMBO)
//!
//! The flat-array index → parameter mapping is the constructor order of the
//! `ParamName` enum in `PlutusLedgerApi.V1.ParamName` (the `DO NOT REORDER`
//! enum), surfaced via `tagWithParamNames` in
//! `PlutusLedgerApi.Common.ParamName`. That order is alphabetical by the
//! parameter's `showParamName` text, with the eight CEK machine-step costs
//! (`cekApplyCost` … `cekVarCost`, each `-exBudgetCPU` then `-exBudgetMemory`)
//! interleaved between `blake2b_256` and `chooseData` (indices 17..=32).
//!
//! ## Shape source
//!
//! For every builtin whose cost-function *shape* is identical between V1 and
//! the latest model (all but the five below), we reuse the shape encoded in
//! [`BuiltinCosts::DEFAULT`] — which is itself validated byte-exact against
//! the plutus reference by the conformance budget goldens — and substitute
//! the on-chain coefficients. Only these five differ in V1 (per
//! `builtinCostModelA.json`):
//!
//! | builtin            | V1 cpu shape                              | V1 mem shape       |
//! |--------------------|-------------------------------------------|--------------------|
//! | `divideInteger`    | `const_above_diagonal`(`multiplied_sizes`)| `subtracted_sizes` |
//! | `modInteger`       | `const_above_diagonal`(`multiplied_sizes`)| `subtracted_sizes` |
//! | `quotientInteger`  | `const_above_diagonal`(`multiplied_sizes`)| `subtracted_sizes` |
//! | `remainderInteger` | `const_above_diagonal`(`multiplied_sizes`)| `subtracted_sizes` |
//! | `multiplyInteger`  | `added_sizes`                             | `added_sizes`      |

use crate::builtin::cost::{
    AboveBelowDiagP, BuiltinCosts, ConstAboveDiagMulP, ConstAboveDiagP, CostPair, CostingFun,
    DiagLinearP, Linear1, LinearYZP, QuadXY, Quadratic1, SubtractedSizesP,
};
use crate::machine::cost::MachineCosts;
use crate::machine::ExBudget;
use crate::term::BuiltinId;

/// Number of parameters in a PlutusV1 cost model (`ParamName` enum size).
pub const V1_PARAM_COUNT: usize = 166;

/// Number of parameters in a PlutusV2 cost model at the Vasil/Babbage
/// deployment (`PlutusLedgerApi.V2.ParamName`): the 166 V1 params plus the
/// nine added by `serialiseData` (4), `verifyEcdsaSecp256k1Signature` (2),
/// and `verifySchnorrSecp256k1Signature` (3). Per `tagWithParamNames`'
/// `zip`-truncation rule, a longer on-chain array (Plomin-era V2) is
/// accepted and the trailing params are ignored.
pub const V2_PARAM_COUNT: usize = 175;

/// A cost model resolved from on-chain parameters, ready to drive a CEK run.
#[derive(Debug, Clone)]
pub struct AppliedCosts {
    pub machine: MachineCosts,
    pub builtins: BuiltinCosts,
}

/// Failure to apply a flat cost-model array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostModelApplyError {
    /// The supplied array did not have the exact parameter count this Plutus
    /// language version requires.
    WrongLength { expected: usize, got: usize },
    /// A builtin's default shape is not one this applier knows how to refill
    /// from a flat array (should never happen for a supported version).
    UnsupportedShape { builtin: BuiltinId },
}

impl std::fmt::Display for CostModelApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength { expected, got } => {
                write!(f, "cost model has {got} params, expected {expected}")
            }
            Self::UnsupportedShape { builtin } => {
                write!(f, "cannot refill default shape for builtin {builtin:?}")
            }
        }
    }
}

impl std::error::Error for CostModelApplyError {}

/// Sequential reader over the flat parameter array.
struct Cursor<'a> {
    p: &'a [i64],
    i: usize,
}

impl Cursor<'_> {
    #[inline]
    fn next(&mut self) -> i64 {
        let v = self.p[self.i];
        self.i += 1;
        v
    }

    /// `intercept` then `slope` — the canonical sub-order for every
    /// one-variable-linear model (`-arguments-intercept` < `-arguments-slope`).
    #[inline]
    fn linear(&mut self) -> Linear1 {
        Linear1 {
            intercept: self.next(),
            slope: self.next(),
        }
    }

    #[inline]
    fn exbudget(&mut self) -> ExBudget {
        ExBudget {
            cpu: self.next(),
            mem: self.next(),
        }
    }
}

/// Reuse a builtin's *default* cost-function shape, substituting the next
/// on-chain coefficients. Consumes the exact number of flat parameters the
/// shape requires, in canonical sub-key order.
fn refill_shape(template: CostingFun, cur: &mut Cursor<'_>) -> Result<CostingFun, ()> {
    use CostingFun::*;
    Ok(match template {
        Constant(_) => Constant(cur.next()),
        LinearInX(_) => LinearInX(cur.linear()),
        LinearInY(_) => LinearInY(cur.linear()),
        LinearInZ(_) => LinearInZ(cur.linear()),
        AddedSizes(_) => AddedSizes(cur.linear()),
        MultipliedSizes(_) => MultipliedSizes(cur.linear()),
        MinSize(_) => MinSize(cur.linear()),
        MaxSize(_) => MaxSize(cur.linear()),
        LinearOnDiagonal(_) => LinearOnDiagonal(DiagLinearP {
            // `-constant` < `-intercept` < `-slope`.
            constant: cur.next(),
            intercept: cur.next(),
            slope: cur.next(),
        }),
        SubtractedSizes(_) => SubtractedSizes(SubtractedSizesP {
            // `-intercept` < `-minimum` < `-slope`.
            intercept: cur.next(),
            minimum: cur.next(),
            slope: cur.next(),
        }),
        // Shapes that never appear in the refilled (non-delta) set of any
        // supported version. If one is hit, the caller must override it
        // explicitly instead.
        _ => return Err(()),
    })
}

/// Refill both the cpu and mem cost functions of a builtin from its default
/// shape (cpu params come before mem params in the flat array).
fn refill_builtin(
    out: &mut BuiltinCosts,
    id: BuiltinId,
    cur: &mut Cursor<'_>,
) -> Result<(), CostModelApplyError> {
    let template = BuiltinCosts::DEFAULT.cost_pair(id);
    let cpu = refill_shape(template.cpu, cur)
        .map_err(|()| CostModelApplyError::UnsupportedShape { builtin: id })?;
    let mem = refill_shape(template.mem, cur)
        .map_err(|()| CostModelApplyError::UnsupportedShape { builtin: id })?;
    out.set_cost_pair(id, CostPair { cpu, mem });
    Ok(())
}

/// Protocol-version-dependent `BuiltinSemanticsVariant` for PlutusV1/V2.
///
/// Per IntersectMBO/plutus `PlutusLedgerApi.Common.ProtocolVersions`
/// (Note [Mapping of protocol versions and ledger languages to semantics
/// variants]) + `V2/EvaluationContext.hs`: PlutusV1 and PlutusV2 both use
/// `DefaultFunSemanticsVariantA` before the Chang/Conway hard fork
/// (`changPV = 9`) and `DefaultFunSemanticsVariantB` from PV9 onward. The
/// flat-array PARAM COUNT is identical across the variants (175 for V2); only
/// the cost-function SHAPE of two builtins changes — `multiplyInteger` cpu
/// (`added_sizes` → `multiplied_sizes`) and `verifyEd25519Signature` cpu
/// (`linear_in_z` → `linear_in_y`).
#[inline]
fn is_variant_b(major_pv: u32) -> bool {
    major_pv >= 9
}

/// `DefaultFunSemanticsVariantD` gate for PlutusV1/V2 — the van Rossem hard fork
/// (`PV11`). Per IntersectMBO/plutus `PlutusLedgerApi.Common.ProtocolVersions`,
/// PlutusV1 and PlutusV2 move VariantB → VariantD at PV11 (V3 moves C → E).
///
/// Beyond the VariantB changes, VariantD changes the cost-function SHAPE of two
/// integer-division builtins (`builtinCostModelD.json` vs `…B.json`):
/// `modInteger`/`remainderInteger` MEMORY goes `subtracted_sizes` →
/// `linear_in_y2` (= `intercept + slope*size_y`, the `minimum` dropped), and
/// `modInteger`/`divideInteger` CPU goes `const_above_diagonal` →
/// `above_and_below_diagonal` (always runs the `multiplied_sizes` sub-model over
/// `(max,min)`, the diagonal `constant` dropped). `quotientInteger` is
/// unchanged, as are `divideInteger` mem and `remainderInteger` cpu. The flat
/// param count/order is identical across variants — only the interpretation of
/// the same coefficients changes.
#[inline]
fn is_variant_d(major_pv: u32) -> bool {
    major_pv >= 11
}

/// Integer-division (`divideInteger` / `modInteger` / `quotientInteger` /
/// `remainderInteger`) cost pair, variant-aware. Consumes 6 flat params in
/// canonical order in EVERY branch — cpu `[constant, intercept, slope]` then mem
/// `[intercept, minimum, slope]` — so the param cursor stays aligned; only the
/// cost-function SHAPE differs by semantics variant (see [`is_variant_d`]).
///
/// `cpu_variant_d` (set for `modInteger`/`divideInteger` at PV≥11): cpu becomes
/// `above_and_below_diagonal` over `multiplied_sizes`, which equals
/// [`CostingFun::MultipliedSizes`] because `max*min == x*y` (the diagonal
/// `constant` is dropped); otherwise `const_above_diagonal`
/// ([`CostingFun::ConstAboveDiagonalMul`]).
///
/// `mem_variant_d` (set for `modInteger`/`remainderInteger` at PV≥11): mem
/// becomes `linear_in_y2` = `intercept + slope*size_y` ([`CostingFun::LinearInY`],
/// the `minimum` dropped); otherwise `subtracted_sizes`
/// ([`CostingFun::SubtractedSizes`]).
///
/// Source: IntersectMBO/plutus `builtinCostModel{B,D}.json` +
/// `PlutusCore.Evaluation.Machine.CostingFun.Core` (`runTwoArgumentModel`,
/// `ModelTwoArgumentsLinearInY2` discards the minimum;
/// `ModelTwoArgumentsAboveAndBelowDiagonal` drops the constant).
fn divmod_cost(cur: &mut Cursor<'_>, cpu_variant_d: bool, mem_variant_d: bool) -> CostPair {
    // cpu params: [constant, intercept, slope] (const_above_diagonal / multiplied_sizes).
    let cpu_constant = cur.next();
    let cpu_lin = cur.linear(); // intercept, slope
    let cpu = if cpu_variant_d {
        // above_and_below_diagonal(multiplied_sizes) ≡ multiplied_sizes, since
        // max*min == x*y; the diagonal `constant` is dropped at VariantD.
        CostingFun::MultipliedSizes(cpu_lin)
    } else {
        CostingFun::ConstAboveDiagonalMul(ConstAboveDiagMulP {
            constant: cpu_constant,
            intercept: cpu_lin.intercept,
            slope: cpu_lin.slope,
        })
    };
    // mem params: [intercept, minimum, slope] (subtracted_sizes / linear_in_y2).
    let mem_intercept = cur.next();
    let mem_minimum = cur.next();
    let mem_slope = cur.next();
    let mem = if mem_variant_d {
        // linear_in_y2 == intercept + slope*size_y; the `minimum` is dropped.
        CostingFun::LinearInY(Linear1 {
            intercept: mem_intercept,
            slope: mem_slope,
        })
    } else {
        CostingFun::SubtractedSizes(SubtractedSizesP {
            intercept: mem_intercept,
            minimum: mem_minimum,
            slope: mem_slope,
        })
    };
    CostPair { cpu, mem }
}

/// `multiplyInteger` cost pair for the given semantics variant. Consumes 4
/// flat params (cpu intercept+slope, mem intercept+slope) regardless of
/// variant — only the cpu SHAPE differs. VariantA: cpu `added_sizes`;
/// VariantB (PV9+): cpu `multiplied_sizes`. mem is `added_sizes` in both.
fn multiply_integer(cur: &mut Cursor<'_>, variant_b: bool) -> CostPair {
    let cpu_lin = cur.linear();
    let cpu = if variant_b {
        CostingFun::MultipliedSizes(cpu_lin)
    } else {
        CostingFun::AddedSizes(cpu_lin)
    };
    let mem = CostingFun::AddedSizes(cur.linear());
    CostPair { cpu, mem }
}

/// `verifyEd25519Signature` cost pair for the given semantics variant.
/// Consumes 3 flat params (cpu intercept+slope, mem constant). VariantA: cpu
/// `linear_in_z` (costs on the signature size, arg3); VariantB (PV9+): cpu
/// `linear_in_y` (costs on the message size, arg2). mem is `constant` in both.
fn verify_ed25519(cur: &mut Cursor<'_>, variant_b: bool) -> CostPair {
    let cpu_lin = cur.linear();
    let cpu = if variant_b {
        CostingFun::LinearInY(cpu_lin)
    } else {
        CostingFun::LinearInZ(cpu_lin)
    };
    let mem = CostingFun::Constant(cur.next());
    CostPair { cpu, mem }
}

/// Apply a flat PlutusV1 cost-model array (166 entries, canonical
/// `ParamName` order) to a fresh [`AppliedCosts`].
pub fn apply_v1(p: &[i64], major_pv: u32) -> Result<AppliedCosts, CostModelApplyError> {
    let variant_b = is_variant_b(major_pv);
    let variant_d = is_variant_d(major_pv);
    if p.len() != V1_PARAM_COUNT {
        return Err(CostModelApplyError::WrongLength {
            expected: V1_PARAM_COUNT,
            got: p.len(),
        });
    }
    use BuiltinId::*;
    let mut b = BuiltinCosts::DEFAULT.clone();
    let mut m = MachineCosts::DEFAULT;
    let cur = &mut Cursor { p, i: 0 };

    // [0..=16] addInteger, appendByteString, appendString, bData, blake2b_256
    refill_builtin(&mut b, AddInteger, cur)?;
    refill_builtin(&mut b, AppendByteString, cur)?;
    refill_builtin(&mut b, AppendString, cur)?;
    refill_builtin(&mut b, BData, cur)?;
    refill_builtin(&mut b, Blake2b_256, cur)?;

    // [17..=32] CEK machine-step costs, alphabetical: apply, builtin, const,
    // delay, force, lam, startup, var — each (exBudgetCPU, exBudgetMemory).
    m.apply = cur.exbudget();
    m.builtin = cur.exbudget();
    m.constant = cur.exbudget();
    m.delay = cur.exbudget();
    m.force = cur.exbudget();
    m.lam = cur.exbudget();
    m.startup = cur.exbudget();
    m.var = cur.exbudget();

    // [33..] chooseData … verifyEd25519Signature, alphabetical.
    refill_builtin(&mut b, ChooseData, cur)?;
    refill_builtin(&mut b, ChooseList, cur)?;
    refill_builtin(&mut b, ChooseUnit, cur)?;
    refill_builtin(&mut b, ConsByteString, cur)?;
    refill_builtin(&mut b, ConstrData, cur)?;
    refill_builtin(&mut b, DecodeUtf8, cur)?;
    b.set_cost_pair(DivideInteger, divmod_cost(cur, variant_d, false));
    refill_builtin(&mut b, EncodeUtf8, cur)?;
    refill_builtin(&mut b, EqualsByteString, cur)?;
    refill_builtin(&mut b, EqualsData, cur)?;
    refill_builtin(&mut b, EqualsInteger, cur)?;
    refill_builtin(&mut b, EqualsString, cur)?;
    refill_builtin(&mut b, FstPair, cur)?;
    refill_builtin(&mut b, HeadList, cur)?;
    refill_builtin(&mut b, IData, cur)?;
    refill_builtin(&mut b, IfThenElse, cur)?;
    refill_builtin(&mut b, IndexByteString, cur)?;
    refill_builtin(&mut b, LengthOfByteString, cur)?;
    refill_builtin(&mut b, LessThanByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsInteger, cur)?;
    refill_builtin(&mut b, LessThanInteger, cur)?;
    refill_builtin(&mut b, ListData, cur)?;
    refill_builtin(&mut b, MapData, cur)?;
    refill_builtin(&mut b, MkCons, cur)?;
    refill_builtin(&mut b, MkNilData, cur)?;
    refill_builtin(&mut b, MkNilPairData, cur)?;
    refill_builtin(&mut b, MkPairData, cur)?;
    b.set_cost_pair(ModInteger, divmod_cost(cur, variant_d, variant_d));
    // multiplyInteger — VariantA cpu `added_sizes`; VariantB (PV9+)
    // `multiplied_sizes`.
    let mul = multiply_integer(cur, variant_b);
    b.set_cost_pair(MultiplyInteger, mul);
    refill_builtin(&mut b, NullList, cur)?;
    b.set_cost_pair(QuotientInteger, divmod_cost(cur, false, false));
    b.set_cost_pair(RemainderInteger, divmod_cost(cur, false, variant_d));
    refill_builtin(&mut b, Sha2_256, cur)?;
    refill_builtin(&mut b, Sha3_256, cur)?;
    refill_builtin(&mut b, SliceByteString, cur)?;
    refill_builtin(&mut b, SndPair, cur)?;
    refill_builtin(&mut b, SubtractInteger, cur)?;
    refill_builtin(&mut b, TailList, cur)?;
    refill_builtin(&mut b, Trace, cur)?;
    refill_builtin(&mut b, UnBData, cur)?;
    refill_builtin(&mut b, UnConstrData, cur)?;
    refill_builtin(&mut b, UnIData, cur)?;
    refill_builtin(&mut b, UnListData, cur)?;
    refill_builtin(&mut b, UnMapData, cur)?;
    // verifyEd25519Signature — VariantA cpu `linear_in_z`; VariantB (PV9+)
    // `linear_in_y`.
    let vfy = verify_ed25519(cur, variant_b);
    b.set_cost_pair(VerifyEd25519Signature, vfy);

    debug_assert_eq!(
        cur.i, V1_PARAM_COUNT,
        "V1 cost-model walk must consume exactly {V1_PARAM_COUNT} params"
    );
    if cur.i != V1_PARAM_COUNT {
        return Err(CostModelApplyError::WrongLength {
            expected: V1_PARAM_COUNT,
            got: cur.i,
        });
    }

    Ok(AppliedCosts {
        machine: m,
        builtins: b,
    })
}

/// Apply a flat PlutusV2 cost-model array (Vasil/Babbage: 175 entries,
/// canonical `PlutusLedgerApi.V2.ParamName` order) to a fresh
/// [`AppliedCosts`].
///
/// V2 is V1's walk with three builtins added at their alphabetical
/// positions (none of the 163 shared builtins changed shape between V1 and
/// V2, confirmed against `IntersectMBO/plutus`):
///   * `serialiseData` (linear_in_x / linear_in_x) — between
///     `remainderInteger` and `sha2_256`;
///   * `verifyEcdsaSecp256k1Signature` (constant / constant) — between
///     `unMapData` and `verifyEd25519Signature`;
///   * `verifySchnorrSecp256k1Signature` (linear_in_y / constant) — after
///     `verifyEd25519Signature`.
///
/// A longer on-chain array (Plomin-era V2 appends `integerToByteString`,
/// the bitwise group, …) is accepted: only the leading 175 params are
/// consumed and the rest ignored, exactly as `tagWithParamNames`' `zip`
/// truncates for a node whose `ParamName` enum predates those additions.
/// Builtins beyond index 174 therefore retain the reference default — a
/// no-op for any V2 script that does not call a Plomin-era builtin.
pub fn apply_v2(p: &[i64], major_pv: u32) -> Result<AppliedCosts, CostModelApplyError> {
    let variant_b = is_variant_b(major_pv);
    let variant_d = is_variant_d(major_pv);
    if p.len() < V2_PARAM_COUNT {
        return Err(CostModelApplyError::WrongLength {
            expected: V2_PARAM_COUNT,
            got: p.len(),
        });
    }
    use BuiltinId::*;
    let mut b = BuiltinCosts::DEFAULT.clone();
    let mut m = MachineCosts::DEFAULT;
    let cur = &mut Cursor { p, i: 0 };

    // [0..=16] addInteger, appendByteString, appendString, bData, blake2b_256
    refill_builtin(&mut b, AddInteger, cur)?;
    refill_builtin(&mut b, AppendByteString, cur)?;
    refill_builtin(&mut b, AppendString, cur)?;
    refill_builtin(&mut b, BData, cur)?;
    refill_builtin(&mut b, Blake2b_256, cur)?;

    // [17..=32] CEK machine-step costs, alphabetical: apply, builtin, const,
    // delay, force, lam, startup, var — each (exBudgetCPU, exBudgetMemory).
    m.apply = cur.exbudget();
    m.builtin = cur.exbudget();
    m.constant = cur.exbudget();
    m.delay = cur.exbudget();
    m.force = cur.exbudget();
    m.lam = cur.exbudget();
    m.startup = cur.exbudget();
    m.var = cur.exbudget();

    // [33..] chooseData … verifySchnorrSecp256k1Signature, alphabetical.
    refill_builtin(&mut b, ChooseData, cur)?;
    refill_builtin(&mut b, ChooseList, cur)?;
    refill_builtin(&mut b, ChooseUnit, cur)?;
    refill_builtin(&mut b, ConsByteString, cur)?;
    refill_builtin(&mut b, ConstrData, cur)?;
    refill_builtin(&mut b, DecodeUtf8, cur)?;
    b.set_cost_pair(DivideInteger, divmod_cost(cur, variant_d, false));
    refill_builtin(&mut b, EncodeUtf8, cur)?;
    refill_builtin(&mut b, EqualsByteString, cur)?;
    refill_builtin(&mut b, EqualsData, cur)?;
    refill_builtin(&mut b, EqualsInteger, cur)?;
    refill_builtin(&mut b, EqualsString, cur)?;
    refill_builtin(&mut b, FstPair, cur)?;
    refill_builtin(&mut b, HeadList, cur)?;
    refill_builtin(&mut b, IData, cur)?;
    refill_builtin(&mut b, IfThenElse, cur)?;
    refill_builtin(&mut b, IndexByteString, cur)?;
    refill_builtin(&mut b, LengthOfByteString, cur)?;
    refill_builtin(&mut b, LessThanByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsInteger, cur)?;
    refill_builtin(&mut b, LessThanInteger, cur)?;
    refill_builtin(&mut b, ListData, cur)?;
    refill_builtin(&mut b, MapData, cur)?;
    refill_builtin(&mut b, MkCons, cur)?;
    refill_builtin(&mut b, MkNilData, cur)?;
    refill_builtin(&mut b, MkNilPairData, cur)?;
    refill_builtin(&mut b, MkPairData, cur)?;
    b.set_cost_pair(ModInteger, divmod_cost(cur, variant_d, variant_d));
    // multiplyInteger — VariantA cpu `added_sizes`; VariantB (PV9+)
    // `multiplied_sizes`. (The integer-division builtins are handled by the
    // variant-aware `divmod_cost`, which switches their cost shapes at
    // VariantD/PV11.)
    let mul = multiply_integer(cur, variant_b);
    b.set_cost_pair(MultiplyInteger, mul);
    refill_builtin(&mut b, NullList, cur)?;
    b.set_cost_pair(QuotientInteger, divmod_cost(cur, false, false));
    b.set_cost_pair(RemainderInteger, divmod_cost(cur, false, variant_d));
    // serialiseData — new in V2, alphabetically before sha2_256.
    refill_builtin(&mut b, SerialiseData, cur)?;
    refill_builtin(&mut b, Sha2_256, cur)?;
    refill_builtin(&mut b, Sha3_256, cur)?;
    refill_builtin(&mut b, SliceByteString, cur)?;
    refill_builtin(&mut b, SndPair, cur)?;
    refill_builtin(&mut b, SubtractInteger, cur)?;
    refill_builtin(&mut b, TailList, cur)?;
    refill_builtin(&mut b, Trace, cur)?;
    refill_builtin(&mut b, UnBData, cur)?;
    refill_builtin(&mut b, UnConstrData, cur)?;
    refill_builtin(&mut b, UnIData, cur)?;
    refill_builtin(&mut b, UnListData, cur)?;
    refill_builtin(&mut b, UnMapData, cur)?;
    // verifyEcdsaSecp256k1Signature — new in V2, before verifyEd25519Signature.
    refill_builtin(&mut b, VerifyEcdsaSecp256k1Signature, cur)?;
    // verifyEd25519Signature — VariantA cpu `linear_in_z`; VariantB (PV9+)
    // `linear_in_y`.
    let vfy = verify_ed25519(cur, variant_b);
    b.set_cost_pair(VerifyEd25519Signature, vfy);
    // verifySchnorrSecp256k1Signature — new in V2, last param block.
    refill_builtin(&mut b, VerifySchnorrSecp256k1Signature, cur)?;

    debug_assert_eq!(
        cur.i, V2_PARAM_COUNT,
        "V2 cost-model walk must consume exactly {V2_PARAM_COUNT} params"
    );
    if cur.i != V2_PARAM_COUNT {
        return Err(CostModelApplyError::WrongLength {
            expected: V2_PARAM_COUNT,
            got: cur.i,
        });
    }

    Ok(AppliedCosts {
        machine: m,
        builtins: b,
    })
}

/// Number of parameters in a PlutusV3 cost model at the Conway/PV9 launch
/// (`PlutusLedgerApi.V3.ParamName`): 251 entries. This is the 175 V2 params
/// restructured (the four integer-division builtins each gain the quadratic
/// CPU coefficients, +5 each = +20; serialiseData/secp unchanged) plus the
/// `cekConstr`/`cekCase` machine costs (4) and the batch-4 builtins
/// (BLS12-381, keccak_256, blake2b_224, integerToByteString,
/// byteStringToInteger). Per `tagWithParamNames`' `zip`-truncation rule a
/// longer on-chain array (Plomin PV10 = 297, van Rossem PV11 = 350) is
/// accepted: the bitwise batch (251..=296) is consumed when present and any
/// PV11 tail is ignored (those builtins are not yet implemented).
pub const V3_PARAM_COUNT_BASE: usize = 251;

/// PlutusV3 cost-model parameter count after the Plomin (PV10) bitwise batch.
pub const V3_PARAM_COUNT_BITWISE: usize = 297;

/// V3 integer-division CPU, quadratic-in-x-and-y sub-model. Flat sub-order
/// is the `ParamName` declaration order: `constant`, then the quadratic
/// coefficients `c00, c01, c02, c10, c11, c20`, then `minimum` — consumed
/// identically regardless of `above_and_below`, so the param cursor stays
/// aligned across both shapes (#820).
///
/// `above_and_below` selects the cost-function SHAPE:
///
/// * `false` (variant C, PV < `VAN_ROSSEM_PV`): `const_above_diagonal` —
///   `if size1 < size2 then constant else max(minimum, c00 + c10·x + c01·y +
///   c20·x² + c11·x·y + c02·y²)`. Mirrors `builtinCostModelC.json`.
/// * `true` (variant E, PV ≥ `VAN_ROSSEM_PV`): `above_and_below_diagonal` —
///   ALWAYS runs the quadratic sub-model over `(max(x,y), min(x,y))`; the
///   diagonal `constant` is dropped entirely (read from the cursor for
///   alignment, then discarded). Mirrors `builtinCostModelE.json`.
///
/// Only `divideInteger`/`modInteger` ever pass `true` — `quotientInteger`/
/// `remainderInteger` are `const_above_diagonal` at every protocol version
/// (see the call sites in [`apply_v3`]).
fn v3_division_cpu(cur: &mut Cursor<'_>, above_and_below: bool) -> CostingFun {
    let constant = cur.next();
    let c00 = cur.next();
    let c01 = cur.next();
    let c02 = cur.next();
    let c10 = cur.next();
    let c11 = cur.next();
    let c20 = cur.next();
    let minimum = cur.next();
    let model = QuadXY {
        c00,
        c10,
        c01,
        c20,
        c11,
        c02,
        minimum,
    };
    if above_and_below {
        CostingFun::AboveAndBelowDiagonal(AboveBelowDiagP { model })
    } else {
        CostingFun::ConstAboveDiagonal(ConstAboveDiagP { constant, model })
    }
}

/// V3 `divideInteger` / `quotientInteger`: quadratic CPU (shape selected by
/// `above_and_below_cpu`, see [`v3_division_cpu`]) + `subtracted_sizes`
/// memory (`-intercept`, `-minimum`, `-slope`) — memory shape is unaffected
/// by the PV11 gate (#820 is CPU-shape only).
fn v3_division_div_quot(cur: &mut Cursor<'_>, above_and_below_cpu: bool) -> CostPair {
    let cpu = v3_division_cpu(cur, above_and_below_cpu);
    let mem = CostingFun::SubtractedSizes(SubtractedSizesP {
        intercept: cur.next(),
        minimum: cur.next(),
        slope: cur.next(),
    });
    CostPair { cpu, mem }
}

/// V3 `modInteger` / `remainderInteger`: quadratic CPU (shape selected by
/// `above_and_below_cpu`, see [`v3_division_cpu`]) + `linear_in_y` memory
/// (no minimum — the result is bounded by the divisor `y`; unaffected by
/// the PV11 gate).
fn v3_division_mod_rem(cur: &mut Cursor<'_>, above_and_below_cpu: bool) -> CostPair {
    let cpu = v3_division_cpu(cur, above_and_below_cpu);
    let mem = CostingFun::LinearInY(cur.linear());
    CostPair { cpu, mem }
}

/// V3 `integerToByteString`: CPU `quadratic_in_z` (`c0, c1, c2`); memory
/// `literal_in_y_or_linear_in_z` (`intercept, slope`).
fn v3_integer_to_bytestring(cur: &mut Cursor<'_>) -> CostPair {
    let cpu = CostingFun::QuadraticInZ(Quadratic1 {
        c0: cur.next(),
        c1: cur.next(),
        c2: cur.next(),
    });
    let mem = CostingFun::LiteralInYOrLinearInZ(cur.linear());
    CostPair { cpu, mem }
}

/// V3 `byteStringToInteger`: CPU `quadratic_in_y` (`c0, c1, c2`); memory
/// `linear_in_y` (`intercept, slope`).
fn v3_bytestring_to_integer(cur: &mut Cursor<'_>) -> CostPair {
    let cpu = CostingFun::QuadraticInY(Quadratic1 {
        c0: cur.next(),
        c1: cur.next(),
        c2: cur.next(),
    });
    let mem = CostingFun::LinearInY(cur.linear());
    CostPair { cpu, mem }
}

/// V3 bitwise `andByteString`/`orByteString`/`xorByteString` (Plomin batch):
/// CPU `linear_in_y_and_z` (`intercept, slope1, slope2`); memory
/// `linear_in_max_yz` (`intercept, slope`).
fn v3_bitwise_logical(cur: &mut Cursor<'_>) -> CostPair {
    let cpu = CostingFun::LinearYZ(LinearYZP {
        intercept: cur.next(),
        slope1: cur.next(),
        slope2: cur.next(),
    });
    let mem = CostingFun::LinearMaxYZ(cur.linear());
    CostPair { cpu, mem }
}

/// Apply a flat PlutusV3 cost-model array (Conway: 251 entries at PV9,
/// canonical `PlutusLedgerApi.V3.ParamName` order; 297 at Plomin PV10) to a
/// fresh [`AppliedCosts`].
///
/// V3 differs from V2 in three structural ways (cf. `cardano-haskell-oracle`,
/// `IntersectMBO/plutus` `PlutusLedgerApi.V3.ParamName` + `builtinCostModelC.json`):
///   1. the four integer-division builtins use a quadratic-in-x-and-y CPU
///      model (8 CPU params each) instead of the V1/V2 `multiplied_sizes`
///      (3); `mod`/`remainder` lose the memory `minimum` (`linear_in_y`
///      instead of `subtracted_sizes`). `divideInteger`/`modInteger`
///      ADDITIONALLY switch CPU shape at `major_pv >= VAN_ROSSEM_PV` (11):
///      `const_above_diagonal` (variant C) → `above_and_below_diagonal`
///      (variant E) — see [`v3_division_cpu`]. `quotientInteger`/
///      `remainderInteger` stay `const_above_diagonal` at every PV (#820);
///   2. `multiplyInteger` CPU becomes `multiplied_sizes` (V1/V2 used
///      `added_sizes`); this is the reference-default shape, so `refill_builtin`
///      reuses it;
///   3. `cekConstr`/`cekCase` machine costs appear at flat indices 193..=196
///      (between the secp builtins and the BLS batch), followed by the
///      BLS12-381 / keccak_256 / blake2b_224 / integerToByteString /
///      byteStringToInteger builtins.
///
/// `major_pv` is the block's major protocol version, used only to select
/// the `divideInteger`/`modInteger` CPU shape described above (#820); every
/// other builtin's shape is PV-independent within V3.
pub fn apply_v3(p: &[i64], major_pv: u32) -> Result<AppliedCosts, CostModelApplyError> {
    if p.len() < V3_PARAM_COUNT_BASE {
        return Err(CostModelApplyError::WrongLength {
            expected: V3_PARAM_COUNT_BASE,
            got: p.len(),
        });
    }
    // `divideInteger`/`modInteger` CPU shape gate (#820). Shares the same
    // `vanRossemPV` (11) threshold as PlutusV1/V2's variant B→D switch
    // (`is_variant_d`); for V3 this is the C→E switch. `quotientInteger`/
    // `remainderInteger` never consult this — they pass `false`
    // unconditionally at both call sites below.
    let divmod_above_and_below = is_variant_d(major_pv);
    use BuiltinId::*;
    let mut b = BuiltinCosts::DEFAULT.clone();
    let mut m = MachineCosts::DEFAULT;
    let cur = &mut Cursor { p, i: 0 };

    // [0..=16] addInteger, appendByteString, appendString, bData, blake2b_256
    refill_builtin(&mut b, AddInteger, cur)?;
    refill_builtin(&mut b, AppendByteString, cur)?;
    refill_builtin(&mut b, AppendString, cur)?;
    refill_builtin(&mut b, BData, cur)?;
    refill_builtin(&mut b, Blake2b_256, cur)?;

    // [17..=32] CEK machine-step costs, alphabetical: apply, builtin, const,
    // delay, force, lam, startup, var — each (exBudgetCPU, exBudgetMemory).
    m.apply = cur.exbudget();
    m.builtin = cur.exbudget();
    m.constant = cur.exbudget();
    m.delay = cur.exbudget();
    m.force = cur.exbudget();
    m.lam = cur.exbudget();
    m.startup = cur.exbudget();
    m.var = cur.exbudget();

    // [33..=48] chooseData … decodeUtf8.
    refill_builtin(&mut b, ChooseData, cur)?;
    refill_builtin(&mut b, ChooseList, cur)?;
    refill_builtin(&mut b, ChooseUnit, cur)?;
    refill_builtin(&mut b, ConsByteString, cur)?;
    refill_builtin(&mut b, ConstrData, cur)?;
    refill_builtin(&mut b, DecodeUtf8, cur)?;
    // [49..=59] divideInteger — quadratic CPU (const_above_diagonal at
    // PV<11 / above_and_below_diagonal at PV>=11, #820) + subtracted_sizes
    // memory.
    let div = v3_division_div_quot(cur, divmod_above_and_below);
    b.set_cost_pair(DivideInteger, div);
    // [60..=113] encodeUtf8 … mkPairData.
    refill_builtin(&mut b, EncodeUtf8, cur)?;
    refill_builtin(&mut b, EqualsByteString, cur)?;
    refill_builtin(&mut b, EqualsData, cur)?;
    refill_builtin(&mut b, EqualsInteger, cur)?;
    refill_builtin(&mut b, EqualsString, cur)?;
    refill_builtin(&mut b, FstPair, cur)?;
    refill_builtin(&mut b, HeadList, cur)?;
    refill_builtin(&mut b, IData, cur)?;
    refill_builtin(&mut b, IfThenElse, cur)?;
    refill_builtin(&mut b, IndexByteString, cur)?;
    refill_builtin(&mut b, LengthOfByteString, cur)?;
    refill_builtin(&mut b, LessThanByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsByteString, cur)?;
    refill_builtin(&mut b, LessThanEqualsInteger, cur)?;
    refill_builtin(&mut b, LessThanInteger, cur)?;
    refill_builtin(&mut b, ListData, cur)?;
    refill_builtin(&mut b, MapData, cur)?;
    refill_builtin(&mut b, MkCons, cur)?;
    refill_builtin(&mut b, MkNilData, cur)?;
    refill_builtin(&mut b, MkNilPairData, cur)?;
    refill_builtin(&mut b, MkPairData, cur)?;
    // [114..=123] modInteger — quadratic CPU (const_above_diagonal at
    // PV<11 / above_and_below_diagonal at PV>=11, #820) + linear_in_y
    // memory.
    let modi = v3_division_mod_rem(cur, divmod_above_and_below);
    b.set_cost_pair(ModInteger, modi);
    // [124..=127] multiplyInteger — V3 cpu `multiplied_sizes` / mem
    // `added_sizes` (= reference default shape).
    refill_builtin(&mut b, MultiplyInteger, cur)?;
    // [128..=129] nullList.
    refill_builtin(&mut b, NullList, cur)?;
    // [130..=140] quotientInteger — quadratic CPU, ALWAYS
    // const_above_diagonal (unchanged at every protocol version, #820) +
    // subtracted_sizes memory.
    let quot = v3_division_div_quot(cur, false);
    b.set_cost_pair(QuotientInteger, quot);
    // [141..=150] remainderInteger — quadratic CPU, ALWAYS
    // const_above_diagonal (unchanged at every protocol version, #820) +
    // linear_in_y memory.
    let rem = v3_division_mod_rem(cur, false);
    b.set_cost_pair(RemainderInteger, rem);
    // [151..=154] serialiseData.
    refill_builtin(&mut b, SerialiseData, cur)?;
    // [155..=192] sha2_256 … verifySchnorrSecp256k1Signature.
    refill_builtin(&mut b, Sha2_256, cur)?;
    refill_builtin(&mut b, Sha3_256, cur)?;
    refill_builtin(&mut b, SliceByteString, cur)?;
    refill_builtin(&mut b, SndPair, cur)?;
    refill_builtin(&mut b, SubtractInteger, cur)?;
    refill_builtin(&mut b, TailList, cur)?;
    refill_builtin(&mut b, Trace, cur)?;
    refill_builtin(&mut b, UnBData, cur)?;
    refill_builtin(&mut b, UnConstrData, cur)?;
    refill_builtin(&mut b, UnIData, cur)?;
    refill_builtin(&mut b, UnListData, cur)?;
    refill_builtin(&mut b, UnMapData, cur)?;
    refill_builtin(&mut b, VerifyEcdsaSecp256k1Signature, cur)?;
    refill_builtin(&mut b, VerifyEd25519Signature, cur)?;
    refill_builtin(&mut b, VerifySchnorrSecp256k1Signature, cur)?;
    // [193..=196] cekConstrCost, cekCaseCost (Conway-specific machine steps).
    m.constr = cur.exbudget();
    m.case_ = cur.exbudget();
    // [197..=234] BLS12-381 batch (alphabetical: G1 ops, G2 ops, finalVerify,
    // millerLoop, mulMlResult). All Constant or linear_in_x shapes.
    refill_builtin(&mut b, Bls12_381_G1_Add, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_Compress, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_Equal, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_HashToGroup, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_Neg, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_ScalarMul, cur)?;
    refill_builtin(&mut b, Bls12_381_G1_Uncompress, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_Add, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_Compress, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_Equal, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_HashToGroup, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_Neg, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_ScalarMul, cur)?;
    refill_builtin(&mut b, Bls12_381_G2_Uncompress, cur)?;
    refill_builtin(&mut b, Bls12_381_FinalVerify, cur)?;
    refill_builtin(&mut b, Bls12_381_MillerLoop, cur)?;
    refill_builtin(&mut b, Bls12_381_MulMlResult, cur)?;
    // [235..=240] keccak_256, blake2b_224.
    refill_builtin(&mut b, Keccak_256, cur)?;
    refill_builtin(&mut b, Blake2b_224, cur)?;
    // [241..=250] integerToByteString, byteStringToInteger (special shapes).
    let i2b = v3_integer_to_bytestring(cur);
    b.set_cost_pair(IntegerToByteString, i2b);
    let b2i = v3_bytestring_to_integer(cur);
    b.set_cost_pair(ByteStringToInteger, b2i);

    debug_assert_eq!(
        cur.i, V3_PARAM_COUNT_BASE,
        "V3 base cost-model walk must consume exactly {V3_PARAM_COUNT_BASE} params"
    );
    if cur.i != V3_PARAM_COUNT_BASE {
        return Err(CostModelApplyError::WrongLength {
            expected: V3_PARAM_COUNT_BASE,
            got: cur.i,
        });
    }

    // [251..=296] Plomin (PV10) bitwise batch — only present when the on-chain
    // array advertises them. Consumed when `p.len() >= 297`; a PV11 (350) tail
    // beyond index 296 is ignored (those builtins are not yet implemented, so
    // they retain the reference default and a script calling one fails
    // elsewhere rather than mis-costing).
    if p.len() >= V3_PARAM_COUNT_BITWISE {
        let and = v3_bitwise_logical(cur);
        b.set_cost_pair(AndByteString, and);
        let or = v3_bitwise_logical(cur);
        b.set_cost_pair(OrByteString, or);
        let xor = v3_bitwise_logical(cur);
        b.set_cost_pair(XorByteString, xor);
        refill_builtin(&mut b, ComplementByteString, cur)?;
        refill_builtin(&mut b, ReadBit, cur)?;
        refill_builtin(&mut b, WriteBits, cur)?;
        refill_builtin(&mut b, ReplicateByte, cur)?;
        refill_builtin(&mut b, ShiftByteString, cur)?;
        refill_builtin(&mut b, RotateByteString, cur)?;
        refill_builtin(&mut b, CountSetBits, cur)?;
        refill_builtin(&mut b, FindFirstSetBit, cur)?;
        refill_builtin(&mut b, Ripemd_160, cur)?;

        debug_assert_eq!(
            cur.i, V3_PARAM_COUNT_BITWISE,
            "V3 bitwise batch must consume exactly {V3_PARAM_COUNT_BITWISE} params"
        );
        if cur.i != V3_PARAM_COUNT_BITWISE {
            return Err(CostModelApplyError::WrongLength {
                expected: V3_PARAM_COUNT_BITWISE,
                got: cur.i,
            });
        }
    }

    Ok(AppliedCosts {
        machine: m,
        builtins: b,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `divmod_cost` must select the correct cost-function SHAPE per semantics
    /// variant while consuming the SAME 6 params (cursor alignment), reading the
    /// coefficients into the right fields. VariantD (PV≥11) switches
    /// modInteger/divideInteger cpu → multiplied_sizes and
    /// modInteger/remainderInteger mem → linear_in_y2; VariantB keeps
    /// const_above_diagonal / subtracted_sizes. (PV11 byte-correctness fix.)
    #[test]
    fn divmod_cost_variant_boundary_shapes() {
        // cpu [constant, intercept, slope], mem [intercept, minimum, slope]
        // (modInteger D-model coefficients).
        let p = [85848_i64, 228465, 122, 0, 1, 1];

        // VariantD modInteger: cpu AND mem switch.
        let mut cur = Cursor { p: &p, i: 0 };
        let d = divmod_cost(&mut cur, true, true);
        assert_eq!(cur.i, 6, "divmod_cost must consume exactly 6 params");
        match d.cpu {
            CostingFun::MultipliedSizes(l) => assert_eq!((l.intercept, l.slope), (228465, 122)),
            other => panic!("VariantD cpu must be MultipliedSizes, got {other:?}"),
        }
        match d.mem {
            CostingFun::LinearInY(l) => assert_eq!((l.intercept, l.slope), (0, 1)),
            other => panic!("VariantD mem must be LinearInY (linear_in_y2), got {other:?}"),
        }

        // VariantB: cpu const_above_diagonal(mul), mem subtracted_sizes.
        let mut cur = Cursor { p: &p, i: 0 };
        let b = divmod_cost(&mut cur, false, false);
        match b.cpu {
            CostingFun::ConstAboveDiagonalMul(c) => {
                assert_eq!((c.constant, c.intercept, c.slope), (85848, 228465, 122))
            }
            other => panic!("VariantB cpu must be ConstAboveDiagonalMul, got {other:?}"),
        }
        match b.mem {
            CostingFun::SubtractedSizes(s) => {
                assert_eq!((s.intercept, s.minimum, s.slope), (0, 1, 1))
            }
            other => panic!("VariantB mem must be SubtractedSizes, got {other:?}"),
        }
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert_eq!(
            apply_v1(&[0i64; 10], 8).unwrap_err(),
            CostModelApplyError::WrongLength {
                expected: 166,
                got: 10,
            }
        );
        // 166 succeeds.
        assert!(apply_v1(&[0i64; 166], 8).is_ok());
    }

    /// Feed `p[i] = i` so every flat index is a distinct value, then assert
    /// each builtin/coefficient picked up exactly the index the canonical
    /// `ParamName` order assigns it. This pins the whole index→shape map.
    #[test]
    fn synthetic_index_mapping_is_canonical() {
        let p: Vec<i64> = (0..166).collect();
        // pre-Conway (VariantA): multiplyInteger=added_sizes, verifyEd25519=linear_in_z.
        let a = apply_v1(&p, 8).unwrap();

        // addInteger: cpu max_size(intercept=p0, slope=p1); mem max_size(p2,p3).
        // at sizes (1,1): cpu = 0 + 1*max(1,1) = 1; mem = 2 + 3*1 = 5.
        let add = a.builtins.cost_for(BuiltinId::AddInteger, 1, 1, 0);
        assert_eq!((add.cpu, add.mem), (1, 5));

        // CEK machine costs: apply=p17/18 … startup=p29/30 … var=p31/32.
        assert_eq!((a.machine.apply.cpu, a.machine.apply.mem), (17, 18));
        assert_eq!((a.machine.startup.cpu, a.machine.startup.mem), (29, 30));
        assert_eq!((a.machine.var.cpu, a.machine.var.mem), (31, 32));

        // divideInteger cpu: const_above_diagonal(multiplied_sizes)
        //   constant=p49, intercept=p50, slope=p51.
        //   below diagonal (x<y): constant.
        let d_below = a.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        assert_eq!(d_below.cpu, 49);
        //   above/on diagonal (x>=y): intercept + slope*(x*y) = 50 + 51*(3*2).
        let d_above = a.builtins.cost_for(BuiltinId::DivideInteger, 3, 2, 0);
        assert_eq!(d_above.cpu, 50 + 51 * 6);
        // divideInteger mem: subtracted_sizes(intercept=p52, minimum=p53, slope=p54)
        //   slope branch (x>>y): 52 + 54*(60-1) > minimum.
        let d_mem = a.builtins.cost_for(BuiltinId::DivideInteger, 60, 1, 0);
        assert_eq!(d_mem.mem, 52 + 54 * 59);
        //   minimum branch (x==y): 52 + 54*0 = 52 < minimum 53 → clamps to 53.
        let d_mem_min = a.builtins.cost_for(BuiltinId::DivideInteger, 1, 1, 0);
        assert_eq!(d_mem_min.mem, 53);

        // multiplyInteger cpu+mem: added_sizes (p115,p116) / (p117,p118).
        //   at (1,1): cpu = 115 + 116*(1+1); mem = 117 + 118*2.
        let mul = a.builtins.cost_for(BuiltinId::MultiplyInteger, 1, 1, 0);
        assert_eq!((mul.cpu, mul.mem), (115 + 116 * 2, 117 + 118 * 2));

        // verifyEd25519Signature cpu: VariantA linear_in_z (p163,p164); mem const(p165).
        //   at z=2: cpu = 163 + 164*2.
        let vfy = a
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 0, 2);
        assert_eq!((vfy.cpu, vfy.mem), (163 + 164 * 2, 165));
    }

    #[test]
    fn v2_wrong_length_is_rejected() {
        assert_eq!(
            apply_v2(&[0i64; 10], 9).unwrap_err(),
            CostModelApplyError::WrongLength {
                expected: 175,
                got: 10,
            }
        );
        // Exactly 175 succeeds; a longer (Plomin-era) array is truncated.
        assert!(apply_v2(&[0i64; 175], 9).is_ok());
        assert!(apply_v2(&[0i64; 332], 9).is_ok());
    }

    /// Feed `p[i] = i` and pin the V2-specific index→shape mapping: the
    /// shared builtins keep their V1 positions through `decodeUtf8`, then the
    /// three V2-new builtins land at their canonical alphabetical slots
    /// (serialiseData 133–136, verifyEcdsa 167–168, verifySchnorr 172–174).
    #[test]
    fn v2_synthetic_index_mapping_is_canonical() {
        let p: Vec<i64> = (0..175).collect();
        // pre-Conway (VariantA): verifyEd25519=linear_in_z.
        let a = apply_v2(&p, 8).unwrap();

        // Shared machine costs sit at the same indices as V1.
        assert_eq!((a.machine.apply.cpu, a.machine.apply.mem), (17, 18));
        assert_eq!((a.machine.startup.cpu, a.machine.startup.mem), (29, 30));
        assert_eq!((a.machine.var.cpu, a.machine.var.mem), (31, 32));

        // serialiseData: cpu linear_in_x(p133,p134), mem linear_in_x(p135,p136).
        //   at x=2: cpu = 133 + 134*2; mem = 135 + 136*2.
        let ser = a.builtins.cost_for(BuiltinId::SerialiseData, 2, 0, 0);
        assert_eq!((ser.cpu, ser.mem), (133 + 134 * 2, 135 + 136 * 2));

        // verifyEcdsaSecp256k1Signature: cpu const(p167), mem const(p168).
        let ecdsa = a
            .builtins
            .cost_for(BuiltinId::VerifyEcdsaSecp256k1Signature, 1, 1, 1);
        assert_eq!((ecdsa.cpu, ecdsa.mem), (167, 168));

        // verifyEd25519Signature: VariantA cpu linear_in_z(p169,p170), mem const(p171).
        //   at z=2: cpu = 169 + 170*2.
        let ed = a
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 0, 2);
        assert_eq!((ed.cpu, ed.mem), (169 + 170 * 2, 171));

        // verifySchnorrSecp256k1Signature: cpu linear_in_y(p172,p173), mem const(p174).
        let schnorr = a
            .builtins
            .cost_for(BuiltinId::VerifySchnorrSecp256k1Signature, 0, 2, 0);
        assert_eq!((schnorr.cpu, schnorr.mem), (172 + 173 * 2, 174));
    }

    #[test]
    fn v3_wrong_length_is_rejected() {
        assert_eq!(
            apply_v3(&[0i64; 10], 9).unwrap_err(),
            CostModelApplyError::WrongLength {
                expected: 251,
                got: 10,
            }
        );
        // Exactly 251 (PV9) succeeds; 297 (PV10 bitwise) succeeds; a longer
        // (PV11, 350) array is accepted and the tail beyond 296 ignored.
        assert!(apply_v3(&[0i64; 251], 9).is_ok());
        assert!(apply_v3(&[0i64; 297], 10).is_ok());
        assert!(apply_v3(&[0i64; 350], 11).is_ok());
        // 251 < len < 297 still consumes only the base 251 (no bitwise batch).
        assert!(apply_v3(&[0i64; 280], 10).is_ok());
    }

    /// Feed `p[i] = i` and pin the V3-specific index→shape mapping: the
    /// canonical `PlutusLedgerApi.V3.ParamName` order with the changed
    /// integer-division quadratic shapes, `cekConstr`/`cekCase` at 193–196,
    /// the BLS batch, and integerToByteString/byteStringToInteger.
    ///
    /// Uses `major_pv=9` (variant C, PV<11) so `divideInteger`/`modInteger`
    /// exercise the `const_above_diagonal` shape asserted below — see
    /// `divide_and_mod_integer_cpu_shape_is_pv_gated` for the #820 PV-matrix
    /// coverage of the PV>=11 `above_and_below_diagonal` switch.
    #[test]
    fn v3_synthetic_index_mapping_is_canonical() {
        let p: Vec<i64> = (0..251).collect();
        let a = apply_v3(&p, 9).unwrap();

        // Machine costs: shared block at 17–32, Conway constr/case at 193–196.
        assert_eq!((a.machine.apply.cpu, a.machine.apply.mem), (17, 18));
        assert_eq!((a.machine.startup.cpu, a.machine.startup.mem), (29, 30));
        assert_eq!((a.machine.var.cpu, a.machine.var.mem), (31, 32));
        assert_eq!((a.machine.constr.cpu, a.machine.constr.mem), (193, 194));
        assert_eq!((a.machine.case_.cpu, a.machine.case_.mem), (195, 196));

        // divideInteger cpu: const_above_diagonal(quadratic_in_x_and_y).
        //   constant=p49; c00=p50,c01=p51,c02=p52,c10=p53,c11=p54,c20=p55,min=p56.
        //   below diagonal (x<y): constant=49.
        let d_below = a.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        assert_eq!(d_below.cpu, 49);
        //   above/on diagonal (x>=y) at (2,1):
        //   c00 + c10*x + c01*y + c20*x² + c11*x*y + c02*y²
        //   = 50 + 53*2 + 51*1 + 55*4 + 54*2 + 52*1 = 587 (> min 56).
        let d_above = a.builtins.cost_for(BuiltinId::DivideInteger, 2, 1, 0);
        assert_eq!(d_above.cpu, 50 + 53 * 2 + 51 + 55 * 4 + 54 * 2 + 52);
        // divideInteger mem: subtracted_sizes(intercept=p57,min=p58,slope=p59).
        //   at (2,1): max(58, 57 + 59*(2-1)) = 116.
        assert_eq!(d_above.mem, 116);

        // modInteger cpu quad at 114–121; mem linear_in_y(p122,p123) — NO minimum.
        let m_mod = a.builtins.cost_for(BuiltinId::ModInteger, 3, 2, 0);
        assert_eq!(m_mod.mem, 122 + 123 * 2);

        // multiplyInteger: cpu multiplied_sizes(p124,p125), mem added_sizes(p126,p127).
        //   at (2,3): cpu = 124 + 125*(2*3); mem = 126 + 127*(2+3).
        let mul = a.builtins.cost_for(BuiltinId::MultiplyInteger, 2, 3, 0);
        assert_eq!((mul.cpu, mul.mem), (124 + 125 * 6, 126 + 127 * 5));

        // quotientInteger mem: subtracted_sizes(intercept=p138,min=p139,slope=p140).
        //   at (2,1): max(139, 138 + 140*(2-1)) = 278.
        let quot = a.builtins.cost_for(BuiltinId::QuotientInteger, 2, 1, 0);
        assert_eq!(quot.mem, 278);
        // remainderInteger mem: linear_in_y(p149,p150) — NO minimum.
        let rem = a.builtins.cost_for(BuiltinId::RemainderInteger, 3, 2, 0);
        assert_eq!(rem.mem, 149 + 150 * 2);

        // serialiseData: cpu linear_in_x(p151,p152), mem linear_in_x(p153,p154).
        let ser = a.builtins.cost_for(BuiltinId::SerialiseData, 2, 0, 0);
        assert_eq!((ser.cpu, ser.mem), (151 + 152 * 2, 153 + 154 * 2));

        // BLS12-381 G1_add: constant cpu(p197) / constant mem(p198).
        let g1 = a.builtins.cost_for(BuiltinId::Bls12_381_G1_Add, 1, 1, 0);
        assert_eq!((g1.cpu, g1.mem), (197, 198));

        // integerToByteString: cpu quadratic_in_z(c0=p241,c1=p242,c2=p243);
        //   mem literal_in_y_or_linear_in_z(p244,p245), y==0 → linear_in_z.
        let i2b = a.builtins.cost_for(BuiltinId::IntegerToByteString, 0, 0, 2);
        assert_eq!(i2b.cpu, 241 + 242 * 2 + 243 * 4);
        assert_eq!(i2b.mem, 244 + 245 * 2);

        // byteStringToInteger: cpu quadratic_in_y(c0=p246,c1=p247,c2=p248);
        //   mem linear_in_y(p249,p250).
        let b2i = a.builtins.cost_for(BuiltinId::ByteStringToInteger, 0, 2, 0);
        assert_eq!(b2i.cpu, 246 + 247 * 2 + 248 * 4);
        assert_eq!(b2i.mem, 249 + 250 * 2);
    }

    /// #820 PV-matrix golden: `divideInteger`/`modInteger` CPU shape must
    /// switch from `const_above_diagonal` (PV<11, variant C) to
    /// `above_and_below_diagonal` (PV>=11, variant E) — a genuine
    /// observable divergence for "below diagonal" args (size1 < size2),
    /// where C returns a flat constant and E ALWAYS runs the quadratic
    /// sub-model over `(max, min)`. `quotientInteger`/`remainderInteger`
    /// must NOT move — `const_above_diagonal` at both PVs. The conformance
    /// corpus runs a single (LATEST=E, no-PV) harness and cannot catch a
    /// wrong PV<11 branch, hence this dedicated matrix test.
    #[test]
    fn divide_and_mod_integer_cpu_shape_is_pv_gated() {
        let p: Vec<i64> = (0..251).collect();
        // PV9 and PV10 are both variant C (pre-vanRossem); PV11 is the
        // first variant-E protocol version.
        let a_pv9 = apply_v3(&p, 9).unwrap();
        let a_pv10 = apply_v3(&p, 10).unwrap();
        let a_pv11 = apply_v3(&p, 11).unwrap();

        // ── divideInteger: constant=p49; c00=p50,c01=p51,c02=p52,c10=p53,
        //    c11=p54,c20=p55,min=p56. Below-diagonal args (x=1 < y=2).
        //    model.eval(x,y) = c00 + c10*x + c01*y + c20*x^2 + c11*x*y + c02*y^2.
        //    above_and_below_diagonal evaluates model.eval(max(1,2), min(1,2))
        //    = model.eval(2, 1) — identical to the "above diagonal at (2,1)"
        //    case already pinned in `v3_synthetic_index_mapping_is_canonical`.
        let div_quad_below = 50 + 53 * 2 + 51 + 55 * 4 + 54 * 2 + 52;

        let div_pv9 = a_pv9.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        let div_pv10 = a_pv10.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        let div_pv11 = a_pv11.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        assert_eq!(div_pv9.cpu, 49, "PV9 divideInteger below-diagonal must use the flat constant (const_above_diagonal, variant C)");
        assert_eq!(div_pv10.cpu, 49, "PV10 divideInteger below-diagonal must use the flat constant (const_above_diagonal, variant C)");
        assert_eq!(
            div_pv11.cpu, div_quad_below,
            "PV11 divideInteger below-diagonal must run the quadratic model \
             over (max,min), not the constant (above_and_below_diagonal, variant E)"
        );
        assert_ne!(
            div_pv10.cpu, div_pv11.cpu,
            "PV10 vs PV11 divideInteger CPU must diverge for size1<size2 — this IS the #820 bug"
        );

        // ── modInteger: constant=p114; c00=p115,c01=p116,c02=p117,c10=p118,
        //    c11=p119,c20=p120,min=p121. Below-diagonal args (x=1 < y=2).
        let mod_quad_below = 115 + 118 * 2 + 116 + 120 * 4 + 119 * 2 + 117;
        let mod_pv10 = a_pv10.builtins.cost_for(BuiltinId::ModInteger, 1, 2, 0);
        let mod_pv11 = a_pv11.builtins.cost_for(BuiltinId::ModInteger, 1, 2, 0);
        assert_eq!(
            mod_pv10.cpu, 114,
            "PV10 modInteger below-diagonal must use the flat constant"
        );
        assert_eq!(
            mod_pv11.cpu, mod_quad_below,
            "PV11 modInteger below-diagonal must run the quadratic model over (max,min)"
        );
        assert_ne!(mod_pv10.cpu, mod_pv11.cpu);

        // ── quotientInteger / remainderInteger: UNCHANGED at every PV.
        let quot_pv10 = a_pv10
            .builtins
            .cost_for(BuiltinId::QuotientInteger, 1, 2, 0);
        let quot_pv11 = a_pv11
            .builtins
            .cost_for(BuiltinId::QuotientInteger, 1, 2, 0);
        assert_eq!(
            quot_pv10.cpu, 130,
            "quotientInteger stays const_above_diagonal at PV10"
        );
        assert_eq!(
            quot_pv10.cpu, quot_pv11.cpu,
            "quotientInteger CPU shape must NOT move across PV11 (#820 explicitly excludes it)"
        );

        let rem_pv10 = a_pv10
            .builtins
            .cost_for(BuiltinId::RemainderInteger, 1, 2, 0);
        let rem_pv11 = a_pv11
            .builtins
            .cost_for(BuiltinId::RemainderInteger, 1, 2, 0);
        assert_eq!(
            rem_pv10.cpu, 141,
            "remainderInteger stays const_above_diagonal at PV10"
        );
        assert_eq!(
            rem_pv10.cpu, rem_pv11.cpu,
            "remainderInteger CPU shape must NOT move across PV11 (#820 explicitly excludes it)"
        );
    }

    /// With the Plomin (PV10, 297-param) batch present, the bitwise builtins
    /// land at 251–296 with their `linear_in_y_and_z` / `linear_in_max_yz`
    /// shapes.
    #[test]
    fn v3_synthetic_bitwise_batch_mapping() {
        let p: Vec<i64> = (0..297).collect();
        let a = apply_v3(&p, 10).unwrap();

        // andByteString: cpu linear_in_y_and_z(intercept=p251,slope1=p252,
        //   slope2=p253) at (y=2,z=3) = 251 + 252*2 + 253*3; mem
        //   linear_in_max_yz(p254,p255) at max(2,3)=3 = 254 + 255*3.
        let and = a.builtins.cost_for(BuiltinId::AndByteString, 0, 2, 3);
        assert_eq!(and.cpu, 251 + 252 * 2 + 253 * 3);
        assert_eq!(and.mem, 254 + 255 * 3);

        // ripemd_160 (last bitwise builtin): cpu linear_in_x(p294,p295),
        //   mem constant(p296).
        let rip = a.builtins.cost_for(BuiltinId::Ripemd_160, 2, 0, 0);
        assert_eq!((rip.cpu, rip.mem), (294 + 295 * 2, 296));
    }

    /// The actual mainnet PlutusV3 cost model (297 params, Plomin/PV10),
    /// captured from Koios `epoch_params`. Validates `apply_v3` against a real
    /// on-chain array end-to-end: it must apply cleanly and the V3-specific
    /// builtins must evaluate to the values implied by the real coefficients.
    #[test]
    fn mainnet_v3_reference_costs() {
        let raw = include_str!("../tests/fixtures/mainnet_plutus_v3_costmodel.json");
        let p: Vec<i64> = raw
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|s| s.trim().parse::<i64>().expect("int"))
            .collect();
        assert_eq!(p.len(), 297, "real mainnet V3 model is 297 params (PV10)");
        // PV10 (variant C): divideInteger below-diagonal must use the flat
        // constant asserted below, not the PV>=11 above_and_below_diagonal
        // shape (#820).
        let a = apply_v3(&p, 10).unwrap();

        // cekConstr / cekCase machine costs (193..=196) = (16000,100).
        assert_eq!((a.machine.constr.cpu, a.machine.constr.mem), (16000, 100));
        assert_eq!((a.machine.case_.cpu, a.machine.case_.mem), (16000, 100));

        // divideInteger [49..=59] = 85848,123203,7305,-900,1716,549,57,85848,0,1,1.
        //   below diagonal (x<y) → constant 85848.
        let d_below = a.builtins.cost_for(BuiltinId::DivideInteger, 1, 2, 0);
        assert_eq!(d_below.cpu, 85848);
        //   above diagonal at (2,1): c00 + c10·2 + c01·1 + c20·4 + c11·2 + c02·1
        //     = 123203 + 1716*2 + 7305 + 57*4 + 549*2 + (-900) = 134366.
        let d_above = a.builtins.cost_for(BuiltinId::DivideInteger, 2, 1, 0);
        assert_eq!(
            d_above.cpu,
            123203 + 1716 * 2 + 7305 + 57 * 4 + 549 * 2 - 900
        );
        //   mem subtracted_sizes(intercept=0,min=1,slope=1) at (2,1): max(1,1)=1.
        assert_eq!(d_above.mem, 1);

        // multiplyInteger [124..=127] = 90434,519,0,1 (multiplied_sizes/added_sizes).
        //   at (2,3): cpu = 90434 + 519*(2*3); mem = 0 + 1*(2+3).
        let mul = a.builtins.cost_for(BuiltinId::MultiplyInteger, 2, 3, 0);
        assert_eq!((mul.cpu, mul.mem), (90434 + 519 * 6, 5));

        // integerToByteString [241..=245] = 1293828,28716,63,0,1.
        //   cpu quadratic_in_z at z=2: 1293828 + 28716*2 + 63*4; mem (y=0) 0+1*2.
        let i2b = a.builtins.cost_for(BuiltinId::IntegerToByteString, 0, 0, 2);
        assert_eq!(i2b.cpu, 1293828 + 28716 * 2 + 63 * 4);
        assert_eq!(i2b.mem, 2);

        // andByteString [251..=255] = 100181,726,719,0,1 (PV10 bitwise batch).
        //   cpu linear_yz at (y=2,z=3): 100181 + 726*2 + 719*3; mem max(2,3) → 3.
        let and = a.builtins.cost_for(BuiltinId::AndByteString, 0, 2, 3);
        assert_eq!(and.cpu, 100181 + 726 * 2 + 719 * 3);
        assert_eq!(and.mem, 3);
    }

    /// #764 Part B: confirm the budget-exhausted flood is the DEFAULT-cost
    /// fallback, NOT a real-model over-count. When the V3 cost model is absent
    /// (the from-genesis PPUP-wipe, pre-#764-Part-A), `resolve_applied_costs`
    /// returns `None` and the CEK runs on `BuiltinCosts::DEFAULT`, which charges
    /// MORE than the real on-chain V3 model for `equalsByteString` — the ~1453-cpu
    /// overrun (signature cpu_remaining=14547). With the real model, `apply_v3`
    /// is byte-exact, so once Part A guarantees V3 is always present the flood
    /// disappears. This test pins both facts.
    #[test]
    fn v3_default_fallback_overcharges_equals_bytestring() {
        let raw = include_str!("../tests/fixtures/mainnet_plutus_v3_costmodel.json");
        let p: Vec<i64> = raw
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|s| s.trim().parse::<i64>().expect("int"))
            .collect();
        let real = apply_v3(&p, 10).unwrap();

        // Real on-chain V3 equalsByteString = LinearOnDiagonal(const=24548,
        // intercept=29498, slope=38) [fixture idx 64..=66]. For equal-length
        // args (on-diagonal) the cost is intercept + slope*n.
        let n = 58i64;
        let real_cost = real
            .builtins
            .cost_for(BuiltinId::EqualsByteString, n, n, 0)
            .cpu;
        assert_eq!(
            real_cost,
            29498 + 38 * n,
            "apply_v3 must map equalsByteString to the real on-chain coefficients \
             (no real-model over-count)"
        );

        // DEFAULT equalsByteString = LinearOnDiagonal(const=30623, intercept=28755,
        // slope=75) — strictly MORE than the real model for any n>0. This is the
        // over-charge that produced the flood when V3 was absent.
        let default_cost = BuiltinCosts::DEFAULT
            .cost_for(BuiltinId::EqualsByteString, n, n, 0)
            .cpu;
        assert_eq!(default_cost, 28755 + 75 * n);
        assert!(
            default_cost > real_cost,
            "DEFAULT over-charges vs the real V3 model (DEFAULT={default_cost}, \
             real={real_cost}) — the source of the #764 budget-exhausted flood \
             when V3 was wiped to None on a from-genesis sync"
        );
    }

    /// The actual mainnet Alonzo PlutusV1 cost model (the 166 values from
    /// `config/mainnet/alonzo-genesis.json`, sorted into canonical
    /// `ParamName` order exactly as the ledger seeds them). Asserts a
    /// representative spread of real on-chain coefficients land correctly.
    #[test]
    fn mainnet_alonzo_v1_reference_costs() {
        // Sorted-by-key values of costModels.PlutusV1 in mainnet alonzo-genesis.
        let p: [i64; 166] = [
            197209, 0, 1, 1, 396231, 621, 0, 1, 150000, 1000, 0, 1, 150000, 32, 2477736, 29175, 4,
            29773, 100, 29773, 100, 29773, 100, 29773, 100, 29773, 100, 29773, 100, 100, 100,
            29773, 100, 150000, 32, 150000, 32, 150000, 32, 150000, 1000, 0, 1, 150000, 32, 150000,
            1000, 0, 8, 148000, 425507, 118, 0, 1, 1, 150000, 1000, 0, 8, 150000, 112536, 247, 1,
            150000, 10000, 1, 136542, 1326, 1, 1000, 150000, 1000, 1, 150000, 32, 150000, 32,
            150000, 32, 1, 1, 150000, 1, 150000, 4, 103599, 248, 1, 103599, 248, 1, 145276, 1366,
            1, 179690, 497, 1, 150000, 32, 150000, 32, 150000, 32, 150000, 32, 150000, 32, 150000,
            32, 148000, 425507, 118, 0, 1, 1, 61516, 11218, 0, 1, 150000, 32, 148000, 425507, 118,
            0, 1, 1, 148000, 425507, 118, 0, 1, 1, 2477736, 29175, 4, 0, 82363, 4, 150000, 5000, 0,
            1, 150000, 32, 197209, 0, 1, 1, 150000, 32, 150000, 32, 150000, 32, 150000, 32, 150000,
            32, 150000, 32, 150000, 32, 3345831, 1, 1,
        ];
        // Alonzo = PV5 (pre-Conway VariantA): multiplyInteger=added_sizes,
        // verifyEd25519=linear_in_z.
        let a = apply_v1(&p, 5).unwrap();

        // addInteger cpu max_size(197209, 0) → flat 197209 at any size.
        assert_eq!(
            a.builtins.cost_for(BuiltinId::AddInteger, 5, 9, 0).cpu,
            197209
        );
        // CEK startup is the famous (100, 100); other step costs 29773/100.
        assert_eq!((a.machine.startup.cpu, a.machine.startup.mem), (100, 100));
        assert_eq!(a.machine.apply.cpu, 29773);
        // divideInteger cpu: below diagonal = constant 148000;
        //   above = 425507 + 118*(x*y).
        assert_eq!(
            a.builtins.cost_for(BuiltinId::DivideInteger, 1, 4, 0).cpu,
            148000
        );
        assert_eq!(
            a.builtins.cost_for(BuiltinId::DivideInteger, 4, 2, 0).cpu,
            425507 + 118 * 8
        );
        // multiplyInteger cpu added_sizes(61516, 11218) at (2,3) = 61516 + 11218*5.
        assert_eq!(
            a.builtins.cost_for(BuiltinId::MultiplyInteger, 2, 3, 0).cpu,
            61516 + 11218 * 5
        );
        // verifyEd25519Signature cpu VariantA linear_in_z(3345831, 1) at z=64 = 3345831 + 64.
        assert_eq!(
            a.builtins
                .cost_for(BuiltinId::VerifyEd25519Signature, 0, 0, 64)
                .cpu,
            3345831 + 64
        );
    }

    /// The V1/V2 BuiltinSemanticsVariant switch at Chang/Conway (PV9): both
    /// multiplyInteger (added_sizes → multiplied_sizes) and
    /// verifyEd25519Signature (linear_in_z → linear_in_y) change shape, while
    /// the flat-array param positions are identical. Validated against the real
    /// current mainnet V2 coefficients (multiplyInteger 90434/519).
    #[test]
    fn v2_semantics_variant_switches_at_pv9() {
        // Real mainnet V2 multiplyInteger coefficients live at flat [115,116].
        let mut p: Vec<i64> = (0..175).collect();
        p[115] = 90434; // cpu intercept
        p[116] = 519; // cpu slope

        // VariantA (PV8): cpu = added_sizes → 90434 + 519*(x+y).
        let a8 = apply_v2(&p, 8).unwrap();
        let m8 = a8.builtins.cost_for(BuiltinId::MultiplyInteger, 4, 5, 0);
        assert_eq!(m8.cpu, 90434 + 519 * (4 + 5));
        // verifyEd25519: VariantA linear_in_z (costs on z, p169/p170).
        let v8 = a8
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 7, 3);
        assert_eq!(v8.cpu, 169 + 170 * 3); // uses z=3, ignores y

        // VariantB (PV9): cpu = multiplied_sizes → 90434 + 519*(x*y).
        let a9 = apply_v2(&p, 9).unwrap();
        let m9 = a9.builtins.cost_for(BuiltinId::MultiplyInteger, 4, 5, 0);
        assert_eq!(m9.cpu, 90434 + 519 * (4 * 5));
        // verifyEd25519: VariantB linear_in_y (costs on y, p169/p170).
        let v9 = a9
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 7, 3);
        assert_eq!(v9.cpu, 169 + 170 * 7); // uses y=7, ignores z

        // The two variants genuinely differ for the same array.
        assert_ne!(m8.cpu, m9.cpu);
        assert_ne!(v8.cpu, v9.cpu);
    }
}
