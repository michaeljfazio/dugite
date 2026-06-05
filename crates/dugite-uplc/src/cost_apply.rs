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
    BuiltinCosts, ConstAboveDiagMulP, CostPair, CostingFun, DiagLinearP, Linear1, SubtractedSizesP,
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

/// V1 integer-division cpu: `const_above_diagonal` over `multiplied_sizes`,
/// flat sub-order `constant`, `model-arguments-intercept`,
/// `model-arguments-slope`. mem: `subtracted_sizes`.
fn v1_division(cur: &mut Cursor<'_>) -> CostPair {
    let cpu = CostingFun::ConstAboveDiagonalMul(ConstAboveDiagMulP {
        constant: cur.next(),
        intercept: cur.next(),
        slope: cur.next(),
    });
    let mem = CostingFun::SubtractedSizes(SubtractedSizesP {
        intercept: cur.next(),
        minimum: cur.next(),
        slope: cur.next(),
    });
    CostPair { cpu, mem }
}

/// Apply a flat PlutusV1 cost-model array (166 entries, canonical
/// `ParamName` order) to a fresh [`AppliedCosts`].
pub fn apply_v1(p: &[i64]) -> Result<AppliedCosts, CostModelApplyError> {
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
    b.set_cost_pair(DivideInteger, v1_division(cur));
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
    b.set_cost_pair(ModInteger, v1_division(cur));
    // multiplyInteger — V1 cpu+mem are both `added_sizes`.
    {
        let cpu = CostingFun::AddedSizes(cur.linear());
        let mem = CostingFun::AddedSizes(cur.linear());
        b.set_cost_pair(MultiplyInteger, CostPair { cpu, mem });
    }
    refill_builtin(&mut b, NullList, cur)?;
    b.set_cost_pair(QuotientInteger, v1_division(cur));
    b.set_cost_pair(RemainderInteger, v1_division(cur));
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
    refill_builtin(&mut b, VerifyEd25519Signature, cur)?;

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
pub fn apply_v2(p: &[i64]) -> Result<AppliedCosts, CostModelApplyError> {
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
    b.set_cost_pair(DivideInteger, v1_division(cur));
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
    b.set_cost_pair(ModInteger, v1_division(cur));
    // multiplyInteger — V2 cpu+mem are both `added_sizes` (same as V1).
    {
        let cpu = CostingFun::AddedSizes(cur.linear());
        let mem = CostingFun::AddedSizes(cur.linear());
        b.set_cost_pair(MultiplyInteger, CostPair { cpu, mem });
    }
    refill_builtin(&mut b, NullList, cur)?;
    b.set_cost_pair(QuotientInteger, v1_division(cur));
    b.set_cost_pair(RemainderInteger, v1_division(cur));
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
    refill_builtin(&mut b, VerifyEd25519Signature, cur)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_length_is_rejected() {
        assert_eq!(
            apply_v1(&[0i64; 10]).unwrap_err(),
            CostModelApplyError::WrongLength {
                expected: 166,
                got: 10,
            }
        );
        // 166 succeeds.
        assert!(apply_v1(&[0i64; 166]).is_ok());
    }

    /// Feed `p[i] = i` so every flat index is a distinct value, then assert
    /// each builtin/coefficient picked up exactly the index the canonical
    /// `ParamName` order assigns it. This pins the whole index→shape map.
    #[test]
    fn synthetic_index_mapping_is_canonical() {
        let p: Vec<i64> = (0..166).collect();
        let a = apply_v1(&p).unwrap();

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

        // verifyEd25519Signature cpu: linear_in_y (p163,p164); mem const(p165).
        //   at y=2: cpu = 163 + 164*2.
        let vfy = a
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 2, 0);
        assert_eq!((vfy.cpu, vfy.mem), (163 + 164 * 2, 165));
    }

    #[test]
    fn v2_wrong_length_is_rejected() {
        assert_eq!(
            apply_v2(&[0i64; 10]).unwrap_err(),
            CostModelApplyError::WrongLength {
                expected: 175,
                got: 10,
            }
        );
        // Exactly 175 succeeds; a longer (Plomin-era) array is truncated.
        assert!(apply_v2(&[0i64; 175]).is_ok());
        assert!(apply_v2(&[0i64; 332]).is_ok());
    }

    /// Feed `p[i] = i` and pin the V2-specific index→shape mapping: the
    /// shared builtins keep their V1 positions through `decodeUtf8`, then the
    /// three V2-new builtins land at their canonical alphabetical slots
    /// (serialiseData 133–136, verifyEcdsa 167–168, verifySchnorr 172–174).
    #[test]
    fn v2_synthetic_index_mapping_is_canonical() {
        let p: Vec<i64> = (0..175).collect();
        let a = apply_v2(&p).unwrap();

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

        // verifyEd25519Signature: cpu linear_in_y(p169,p170), mem const(p171).
        let ed = a
            .builtins
            .cost_for(BuiltinId::VerifyEd25519Signature, 0, 2, 0);
        assert_eq!((ed.cpu, ed.mem), (169 + 170 * 2, 171));

        // verifySchnorrSecp256k1Signature: cpu linear_in_y(p172,p173), mem const(p174).
        let schnorr = a
            .builtins
            .cost_for(BuiltinId::VerifySchnorrSecp256k1Signature, 0, 2, 0);
        assert_eq!((schnorr.cpu, schnorr.mem), (172 + 173 * 2, 174));
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
        let a = apply_v1(&p).unwrap();

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
        // verifyEd25519Signature cpu linear_in_y(3345831, 1) at y=64 = 3345831 + 64.
        assert_eq!(
            a.builtins
                .cost_for(BuiltinId::VerifyEd25519Signature, 0, 64, 0)
                .cpu,
            3345831 + 64
        );
    }
}
