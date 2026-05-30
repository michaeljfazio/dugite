//! Per-builtin cost model — Plutus 1.65.0.0 `builtinCostModelE.json`.
//!
//! Each builtin has a `(CpuModel, MemModel)` pair. The models are
//! evaluated against the *ExMemory sizes* of the actual arguments (not
//! the values themselves) and the result is subtracted from the
//! remaining budget via [`crate::machine::cost::BudgetTracker::charge`].
//!
//! ## Sizing rules (mirrors Haskell `ExMemoryUsage`)
//!
//! The rules below are normative against the Haskell reference at
//! `IntersectMBO/plutus:plutus-core/.../ExMemoryUsage.hs`:
//!
//! - **Integer**: `if n==0 { 1 } else { floor(log2(|n|) / 64) + 1 }`
//!   (i.e. number of 64-bit words needed to store `|n|` in binary).
//! - **ByteString**: `((len - 1) quot 8) + 1` — so empty BS = 1, 1–8
//!   bytes = 1, 9–16 = 2, etc. (Haskell uses `quot`, not `div`; for
//!   non-negative inputs they agree, but the empty-string case is
//!   handled by the `(len-1)` trick: `((0-1) quot 8)+1 = 0+1 = 1`).
//! - **String** (for `appendString`/`equalsString`): uses
//!   `TextCostedByByteLength` — `utf8_byte_len quot 4` (truncating).
//! - **Unit / Bool / Int (i64) / G1 / G2 / MlResult**: constant.
//! - **List**: only the *length* (number of elements) is costed for the
//!   builtins that use it (e.g. `writeBits` arg2).
//! - **Data**: recursive node-count × 4 per node (see `sizeData`).
//! - **Pair / Delay / Lambda / Builtin**: 1 (opaque polymorphic arg).
//!
//! ## Cost-model shapes
//!
//! The `CostingFun` enum covers every model variant present in
//! `builtinCostModelE.json`. Variants not present in the JSON are
//! omitted; all evaluators are `const fn`-compatible.

use crate::data::Data;
use crate::machine::value::Value;
use crate::machine::ExBudget;
use crate::term::{BuiltinId, Constant};

// ─── Sizing ──────────────────────────────────────────────────────────────────

/// ExMemory size of a `Constant` value.
///
/// Mirrors `memoryUsage` from the Haskell `ExMemoryUsage` type-class.
pub fn size_of_constant(c: &Constant) -> i64 {
    match c {
        Constant::Integer(n) => size_of_integer(n),
        Constant::ByteString(bs) => size_of_bytestring(bs),
        Constant::String(s) => size_of_string_by_char_count(s),
        Constant::Unit => 1,
        Constant::Bool(_) => 1,
        Constant::ProtoList { elements, .. } => elements.len() as i64,
        Constant::ProtoPair { .. } => 1,
        Constant::Data(d) => size_of_data(d),
        // BLS elements have fixed ExMemory sizes per the Haskell reference.
        Constant::Bls12_381G1Element(_) => 18,
        Constant::Bls12_381G2Element(_) => 36,
        Constant::Bls12_381MlResult(_) => 72,
        // PV1.1.0 additions (CIP-???): Array is sized by element count
        // (mirrors ProtoList); Value is sized by total entry count
        // (sum of inner-map sizes) per Plutus `memoryUsage Value`.
        Constant::Array { elements, .. } => elements.len() as i64,
        Constant::Value(map) => map.values().map(|inner| inner.len() as i64).sum::<i64>(),
    }
}

/// ExMemory size of a `Value`.
///
/// For non-constant values (Lambda, Delay, Builtin, Constr) the
/// Haskell reference returns 1 — they are opaque polymorphic
/// arguments and the costing function is not supposed to inspect them.
pub fn size_of_value(v: &Value) -> i64 {
    match v {
        Value::Const(c) => size_of_constant(c),
        _ => 1,
    }
}

/// `memoryUsageInteger`: 1 for zero, else floor(log2(|n|) / 64) + 1.
fn size_of_integer(n: &num_bigint::BigInt) -> i64 {
    use num_bigint::Sign;
    if n.sign() == Sign::NoSign {
        return 1;
    }
    // bits() returns the number of bits excluding the sign bit.
    let bits = n.magnitude().bits(); // u64
                                     // Haskell: integerLog2(|n|) `div` 64 + 1
                                     // integerLog2(n) = floor(log2(n)) = bits - 1 for n >= 1
    let log2 = bits - 1;
    (log2 / 64 + 1) as i64
}

/// `memoryUsage` for `ByteString`: `((len - 1) quot 8) + 1`.
/// The empty string gives `((-1) quot 8) + 1 = 0 + 1 = 1`.
/// Haskell uses truncating integer division (`quot`).
fn size_of_bytestring(bs: &[u8]) -> i64 {
    let len = bs.len() as i64;
    ((len - 1) / 8 + 1).max(1) // for empty: ((-1)/8+1) in Rust = 0+1=1? No: -1/8 = 0 (truncation)
                               // Actually in Rust, -1_i64 / 8 = 0 (truncation toward zero), so:
                               // ((0 - 1) / 8 + 1) = ((-1) / 8 + 1) = (0 + 1) = 1.  ✓
}

/// `TextCostedByByteLength`: `utf8_byte_len quot 4`.
///
/// This is used only for `appendString` and `equalsString` in variant E.
pub fn size_of_string_by_byte_length(s: &str) -> i64 {
    (s.len() as i64) / 4
}

/// Standard `memoryUsage` for `Text` — costed by char count, in
/// 100-char chunks. This is only used for `decodeUtf8` result sizing
/// (and equalsString v1, but in E we always use TextCostedByByteLength).
fn size_of_string_by_char_count(s: &str) -> i64 {
    s.chars().count() as i64
}

/// `NumBytesCostedAsNumWords`: number of 8-byte words to hold `n` bytes.
///
/// `((abs(n) - 1) div 8) + 1` — Haskell div (floor division).
/// For `n == 0`: `((0 - 1) div 8) + 1 = (-1 div 8) + 1 = -1 + 1 = 0`.
pub fn num_bytes_as_num_words(n: i64) -> i64 {
    let a = n.unsigned_abs() as i64;
    if a == 0 {
        return 0;
    }
    (a - 1) / 8 + 1
}

/// `IntegerCostedLiterally`: `abs(n)` (the raw integer value, not its word-size).
pub fn integer_costed_literally(n: &num_bigint::BigInt) -> i64 {
    use num_traits::ToPrimitive;
    n.magnitude().to_i64().unwrap_or(i64::MAX).abs()
}

/// Convenience alias for the nested-map type used by `Constant::Value`.
type ValueMap = std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, i128>>;

/// `ValueTotalSize` ExMemory — count of distinct `(policy, token)` pairs.
///
/// Mirrors `memoryUsage (ValueTotalSize v) = singletonRose (totalSize v)`.
/// Used for builtins that take a `ValueTotalSize`-wrapped argument
/// (`unionValue`, `valueContains`, `valueData`).
pub fn value_total_size(map: &ValueMap) -> i64 {
    map.values().map(|inner| inner.len() as i64).sum::<i64>()
}

/// `ValueMaxDepth` ExMemory — `logOuter + logInner`.
///
/// Mirrors the Haskell `ExMemoryUsage ValueMaxDepth` instance:
/// ```text
/// logOuter = if outerSize > 0 then integerLog2(outerSize) + 1 else 0
/// logInner = if maxInnerSize > 0 then integerLog2(maxInnerSize) + 1 else 0
/// exMemory = logOuter + logInner
/// ```
/// Used for builtins that take a `ValueMaxDepth`-wrapped argument
/// (`insertCoin`, `lookupCoin`).
pub fn value_max_depth(map: &ValueMap) -> i64 {
    let outer_size = map.len();
    let max_inner = map.values().map(|inner| inner.len()).max().unwrap_or(0);
    let log_outer = if outer_size > 0 {
        (outer_size as u64).ilog2() as i64 + 1
    } else {
        0
    };
    let log_inner = if max_inner > 0 {
        (max_inner as u64).ilog2() as i64 + 1
    } else {
        0
    };
    log_outer + log_inner
}

/// `DataNodeCount` ExMemory — count of nodes in the `Data` tree.
///
/// Mirrors the Haskell `ExMemoryUsage DataNodeCount` instance, which
/// counts every node (Constr, Map entry pair, List element, I, B) as 1
/// — unlike the standard `ExMemoryUsage Data` which adds 4 per node
/// plus the content size.  Used by `unValueData`.
pub fn data_node_count(d: &Data) -> i64 {
    let mut total = 0i64;
    let mut stack = vec![d];
    while let Some(node) = stack.pop() {
        total = total.saturating_add(1);
        match node {
            Data::Constr(_, fields) => {
                for f in fields {
                    stack.push(f);
                }
            }
            Data::Map(entries) => {
                for (k, v) in entries {
                    stack.push(k);
                    stack.push(v);
                }
            }
            Data::List(elems) => {
                for e in elems {
                    stack.push(e);
                }
            }
            Data::I(_) | Data::B(_) => {}
        }
    }
    total
}

/// Recursive `Data` memory size mirroring the Haskell `sizeData`
/// rose-tree fold (`ExMemoryUsage Data`):
///
/// * every node adds 4 (the `dataNodeRose` constant),
/// * `I n` contributes an additional `memoryUsageInteger n` (= 1 for
///   `n == 0`, else `ilog2(|n|).div(64) + 1`),
/// * `B bs` contributes an additional `((len-1) quot 8) + 1`,
/// * `Constr` / `Map` / `List` recurse into their children.
fn size_of_data(d: &Data) -> i64 {
    let mut total = 0i64;
    let mut stack = vec![d];
    while let Some(node) = stack.pop() {
        total = total.saturating_add(4);
        match node {
            Data::Constr(_, fields) => {
                for f in fields {
                    stack.push(f);
                }
            }
            Data::Map(entries) => {
                for (k, v) in entries {
                    stack.push(k);
                    stack.push(v);
                }
            }
            Data::List(elems) => {
                for e in elems {
                    stack.push(e);
                }
            }
            Data::I(n) => {
                total = total.saturating_add(memory_usage_integer(n));
            }
            Data::B(b) => {
                total = total.saturating_add(size_of_bytestring(b));
            }
        }
    }
    total
}

/// `memoryUsageInteger` from PlutusCore.Evaluation.Machine.ExMemoryUsage:
///   * `n == 0` → 1 (preserved special case from pre-GHC-9.2 `integerLog2`)
///   * else → `ilog2(|n|).div(64) + 1`
fn memory_usage_integer(n: &num_bigint::BigInt) -> i64 {
    use num_bigint::Sign;
    if n.sign() == Sign::NoSign {
        return 1;
    }
    // bits() = ilog2(|n|) + 1; we need ilog2(|n|).div(64) + 1.
    let bits = n.bits() as i64;
    if bits == 0 {
        return 1;
    }
    (bits - 1) / 64 + 1
}

// ─── CostingFun ──────────────────────────────────────────────────────────────

/// One-argument linear: `intercept + slope * x`.
#[derive(Debug, Clone, Copy)]
pub struct Linear1 {
    pub intercept: i64,
    pub slope: i64,
}

impl Linear1 {
    fn eval(self, x: i64) -> i64 {
        // Use saturating arithmetic so that very large costing inputs
        // (e.g. DropList with a huge integer argument) saturate at i64::MAX
        // rather than panicking.  The Haskell reference saturates at the
        // Word64/CekCost ceiling; i64::MAX is the closest Rust equivalent
        // (and matches the expected budget in the conformance tests).
        self.intercept.saturating_add(self.slope.saturating_mul(x))
    }
}

/// Quadratic in one variable: `c0 + c1*x + c2*x^2`.
#[derive(Debug, Clone, Copy)]
pub struct Quadratic1 {
    pub c0: i64,
    pub c1: i64,
    pub c2: i64,
}

impl Quadratic1 {
    fn eval(self, x: i64) -> i64 {
        self.c0
            .saturating_add(self.c1.saturating_mul(x))
            .saturating_add(self.c2.saturating_mul(x).saturating_mul(x))
    }
}

/// Subtracted sizes: `max(minimum, intercept + slope * (x - y))`.
#[derive(Debug, Clone, Copy)]
pub struct SubtractedSizesP {
    pub intercept: i64,
    pub slope: i64,
    pub minimum: i64,
}

impl SubtractedSizesP {
    fn eval(self, x: i64, y: i64) -> i64 {
        (self.intercept + self.slope * (x - y)).max(self.minimum)
    }
}

/// Diagonal constant: `if x == y { intercept + slope * x } else { constant }`.
#[derive(Debug, Clone, Copy)]
pub struct DiagLinearP {
    pub constant: i64,
    pub intercept: i64,
    pub slope: i64,
}

impl DiagLinearP {
    fn eval(self, x: i64, y: i64) -> i64 {
        if x == y {
            self.intercept + self.slope * x
        } else {
            self.constant
        }
    }
}

/// Const-above-diagonal: `if x > y { constant } else { model(x, y) }`.
///
/// Haskell says "if size1 < size2, return constant, else run model".
/// `size1` is arg1, `size2` is arg2. So: if arg1_size < arg2_size → constant.
/// Above-diagonal = arg1 >= arg2 → run quadratic model.
///
/// For `quotientInteger`, `remainderInteger`, `valueContains`: the
/// sub-model is the quadratic-in-x-and-y described by `QuadXY`.
#[derive(Debug, Clone, Copy)]
pub struct ConstAboveDiagP {
    /// Cost returned when arg1 size < arg2 size (= "below diagonal").
    pub constant: i64,
    pub model: QuadXY,
}

impl ConstAboveDiagP {
    fn eval(self, x: i64, y: i64) -> i64 {
        // Haskell: if size1 < size2 → constant; else run model(x, y)
        if x < y {
            self.constant
        } else {
            self.model.eval(x, y)
        }
    }
}

/// Above-and-below-diagonal (used by `divideInteger`/`modInteger`):
/// run the quadratic model with `(max, min)` of the two sizes.
#[derive(Debug, Clone, Copy)]
pub struct AboveBelowDiagP {
    pub model: QuadXY,
}

impl AboveBelowDiagP {
    fn eval(self, x: i64, y: i64) -> i64 {
        self.model.eval(x.max(y), x.min(y))
    }
}

/// `const_above_diagonal` with a `linear_in_x_and_y` sub-model (used by
/// `valueContains`).
#[derive(Debug, Clone, Copy)]
pub struct ConstAboveDiagLinXYP {
    pub constant: i64,
    pub intercept: i64,
    pub slope1: i64,
    pub slope2: i64,
}

impl ConstAboveDiagLinXYP {
    fn eval(self, x: i64, y: i64) -> i64 {
        if x < y {
            self.constant
        } else {
            self.intercept + self.slope1 * x + self.slope2 * y
        }
    }
}

/// `const_above_diagonal` with a `multiplied_sizes` sub-model. Used by the
/// integer-division builtins (`divideInteger`/`modInteger`/`quotientInteger`/
/// `remainderInteger`) in the **PlutusV1/V2** cost models, where the inner
/// model is the one-variable-linear `multiplied_sizes` rather than the
/// two-variable quadratic the latest default model uses.
///
/// Mirrors `ModelTwoArgumentsConstAboveDiagonal (ModelConstantOrTwoArguments c
/// (ModelTwoArgumentsMultipliedSizes ...))` in IntersectMBO/plutus
/// `CostingFun/Core.hs`: `if size1 < size2 then c else intercept + slope*(size1*size2)`.
/// The diagonal test is **strictly** `<` (size1 < size2 → constant).
#[derive(Debug, Clone, Copy)]
pub struct ConstAboveDiagMulP {
    /// Cost returned when arg1 size < arg2 size (= "below diagonal").
    pub constant: i64,
    pub intercept: i64,
    pub slope: i64,
}

impl ConstAboveDiagMulP {
    fn eval(self, x: i64, y: i64) -> i64 {
        if x < y {
            self.constant
        } else {
            self.intercept
                .saturating_add(self.slope.saturating_mul(x.saturating_mul(y)))
        }
    }
}

/// Quadratic in x and y, with a minimum floor.
///
/// Standard `c_ij = coefficient of x^i * y^j` convention (matches
/// IntersectMBO/plutus `TwoVariableQuadraticFunction`):
///
///   `c00 + c10·x + c01·y + c20·x² + c11·x·y + c02·y²`
///
/// Used by `quotientInteger` / `remainderInteger` / `valueContains` /
/// `divideInteger` / `modInteger`.
#[derive(Debug, Clone, Copy)]
pub struct QuadXY {
    pub c00: i64,
    pub c10: i64,
    pub c01: i64,
    pub c20: i64,
    pub c11: i64,
    pub c02: i64,
    pub minimum: i64,
}

impl QuadXY {
    fn eval(self, x: i64, y: i64) -> i64 {
        (self.c00
            + self.c10 * x
            + self.c01 * y
            + self.c20 * x * x
            + self.c11 * x * y
            + self.c02 * y * y)
            .max(self.minimum)
    }
}

/// `with_interaction_in_x_and_y`: `c00 + c10*x + c01*y + c11*x*y`.
///
/// Field naming follows the standard polynomial convention where `cij` is
/// the coefficient of `x^i * y^j`.  `c10` multiplies x (x^1 y^0) and
/// `c01` multiplies y (x^0 y^1).  This matches the Haskell definition:
///   `c10*x + c01*y + c11*x*y + c00`
/// (PlutusCore.Evaluation.Machine.CostingFun.Core, `with_interaction_in_x_and_y`).
#[derive(Debug, Clone, Copy)]
pub struct InteractionXYP {
    pub c00: i64,
    pub c01: i64,
    pub c10: i64,
    pub c11: i64,
}

impl InteractionXYP {
    fn eval(self, x: i64, y: i64) -> i64 {
        self.c00
            .saturating_add(self.c10.saturating_mul(x))
            .saturating_add(self.c01.saturating_mul(y))
            .saturating_add(self.c11.saturating_mul(x).saturating_mul(y))
    }
}

/// Two-variable linear: `intercept + slope1*y + slope2*z`.
#[derive(Debug, Clone, Copy)]
pub struct LinearYZP {
    pub intercept: i64,
    pub slope1: i64,
    pub slope2: i64,
}

impl LinearYZP {
    fn eval(self, y: i64, z: i64) -> i64 {
        self.intercept + self.slope1 * y + self.slope2 * z
    }
}

/// `expModInteger` cost: `c00 + c11*ee*mm + c12*ee*mm*mm`,
/// with 50% penalty if `aa > mm`.
#[derive(Debug, Clone, Copy)]
pub struct ExpModP {
    pub coefficient00: i64,
    pub coefficient11: i64,
    pub coefficient12: i64,
}

impl ExpModP {
    fn eval(self, aa: i64, ee: i64, mm: i64) -> i64 {
        let cost0 =
            self.coefficient00 + self.coefficient11 * ee * mm + self.coefficient12 * ee * mm * mm;
        if aa <= mm {
            cost0
        } else {
            cost0 + cost0 / 2
        }
    }
}

/// All costing-function shapes used by `builtinCostModelE.json`.
///
/// Named after the JSON `"type"` field. The enum variants map 1:1 to
/// the JSON model kinds so the constant table below is human-readable.
#[derive(Debug, Clone, Copy)]
pub enum CostingFun {
    /// `constant_cost` — a flat cost regardless of argument sizes.
    Constant(i64),
    /// `linear_in_x` — linear in arg1 size.
    LinearInX(Linear1),
    /// `linear_in_y` — linear in arg2 size.
    LinearInY(Linear1),
    /// `linear_in_z` — linear in arg3 size.
    LinearInZ(Linear1),
    /// `added_sizes` — linear in sum of arg1 + arg2 sizes.
    AddedSizes(Linear1),
    /// `multiplied_sizes` — linear in product of arg1 × arg2 sizes.
    MultipliedSizes(Linear1),
    /// `min_size` — linear in min(arg1, arg2) sizes.
    MinSize(Linear1),
    /// `max_size` — linear in max(arg1, arg2) sizes.
    MaxSize(Linear1),
    /// `subtracted_sizes` — `max(minimum, intercept + slope*(x-y))`.
    SubtractedSizes(SubtractedSizesP),
    /// `quadratic_in_x` — quadratic in arg1 size.
    QuadraticInX(Quadratic1),
    /// `quadratic_in_y` — quadratic in arg2 size.
    QuadraticInY(Quadratic1),
    /// `quadratic_in_z` — quadratic in arg3 size.
    QuadraticInZ(Quadratic1),
    /// `linear_on_diagonal` — linear in arg1 size on-diagonal, constant off.
    LinearOnDiagonal(DiagLinearP),
    /// `const_above_diagonal` with a `quadratic_in_x_and_y` sub-model.
    ConstAboveDiagonal(ConstAboveDiagP),
    /// `above_and_below_diagonal` (always quadratic sub-model in E).
    AboveAndBelowDiagonal(AboveBelowDiagP),
    /// `const_above_diagonal` with a `linear_in_x_and_y` sub-model.
    ConstAboveDiagonalLinearXY(ConstAboveDiagLinXYP),
    /// `const_above_diagonal` with a `multiplied_sizes` sub-model (PlutusV1/V2
    /// integer division).
    ConstAboveDiagonalMul(ConstAboveDiagMulP),
    /// `with_interaction_in_x_and_y`.
    WithInteractionXY(InteractionXYP),
    /// `linear_in_y_and_z` — `intercept + slope1*y + slope2*z`.
    LinearYZ(LinearYZP),
    /// `linear_in_max_yz` — linear in max(arg2, arg3) sizes.
    LinearMaxYZ(Linear1),
    /// `literal_in_y_or_linear_in_z` — if width_y == 0 use linear_in_z
    /// else return size_y directly.
    LiteralInYOrLinearInZ(Linear1),
    /// `exp_mod_cost`.
    ExpModCost(ExpModP),
}

impl CostingFun {
    /// Evaluate against argument sizes.
    ///
    /// Arguments beyond what the model uses are ignored. Sizes not
    /// present (because the builtin takes fewer than 3 args) should be
    /// passed as 0.
    pub fn eval(&self, x: i64, y: i64, z: i64) -> i64 {
        match self {
            Self::Constant(c) => *c,
            Self::LinearInX(f) => f.eval(x),
            Self::LinearInY(f) => f.eval(y),
            Self::LinearInZ(f) => f.eval(z),
            Self::AddedSizes(f) => f.eval(x + y),
            Self::MultipliedSizes(f) => f.eval(x * y),
            Self::MinSize(f) => f.eval(x.min(y)),
            Self::MaxSize(f) => f.eval(x.max(y)),
            Self::SubtractedSizes(f) => f.eval(x, y),
            Self::QuadraticInX(f) => f.eval(x),
            Self::QuadraticInY(f) => f.eval(y),
            Self::QuadraticInZ(f) => f.eval(z),
            Self::LinearOnDiagonal(f) => f.eval(x, y),
            Self::ConstAboveDiagonal(p) => p.eval(x, y),
            Self::AboveAndBelowDiagonal(p) => p.eval(x, y),
            Self::ConstAboveDiagonalLinearXY(p) => p.eval(x, y),
            Self::ConstAboveDiagonalMul(p) => p.eval(x, y),
            Self::WithInteractionXY(f) => f.eval(x, y),
            Self::LinearYZ(f) => f.eval(y, z),
            Self::LinearMaxYZ(f) => f.eval(y.max(z)),
            Self::LiteralInYOrLinearInZ(f) => {
                if y == 0 {
                    // width == 0 → linear_in_z
                    f.eval(z)
                } else {
                    // width != 0 → return the width directly (= size_y)
                    y
                }
            }
            Self::ExpModCost(f) => f.eval(x, y, z),
        }
    }
}

// ─── BuiltinCosts ────────────────────────────────────────────────────────────

/// Per-builtin cost pair `(cpu_model, mem_model)`.
#[derive(Debug, Clone, Copy)]
pub struct CostPair {
    pub cpu: CostingFun,
    pub mem: CostingFun,
}

impl CostPair {
    const fn constant(cpu: i64, mem: i64) -> Self {
        Self {
            cpu: CostingFun::Constant(cpu),
            mem: CostingFun::Constant(mem),
        }
    }

    /// Evaluate both cpu and mem against argument sizes.
    pub fn eval(&self, x: i64, y: i64, z: i64) -> ExBudget {
        ExBudget {
            cpu: self.cpu.eval(x, y, z),
            mem: self.mem.eval(x, y, z),
        }
    }
}

// Shorthand constructors so the table stays readable.
const fn c(cpu: i64, mem: i64) -> CostPair {
    CostPair::constant(cpu, mem)
}
const fn lin1(i: i64, s: i64) -> Linear1 {
    Linear1 {
        intercept: i,
        slope: s,
    }
}
const fn quad1(c0: i64, c1: i64, c2: i64) -> Quadratic1 {
    Quadratic1 { c0, c1, c2 }
}

/// The shared `quadratic_in_x_and_y` sub-model used by divide/quotient/
/// remainder/mod operations.
const DIVMOD_QUAD: QuadXY = QuadXY {
    c00: 123203,
    c01: 7305,
    c02: -900,
    c10: 1716,
    c11: 960,
    c20: 57,
    minimum: 85848,
};
const DIVMOD_SUBSIZE: SubtractedSizesP = SubtractedSizesP {
    intercept: 0,
    slope: 1,
    minimum: 1,
};

/// Builtin cost-model table for Plutus 1.65.0.0 (`builtinCostModelE.json`).
///
/// The table is indexed by `BuiltinId` discriminant (which is `u8`).
/// The `DEFAULT` constant is the reference cost-model the conformance
/// corpus goldens are computed against.
#[derive(Clone)]
pub struct BuiltinCosts {
    table: [CostPair; 101],
}

impl std::fmt::Debug for BuiltinCosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BuiltinCosts")
    }
}

impl BuiltinCosts {
    /// Read the cost pair for a builtin (used by the cost-model applier to
    /// reuse the default model's per-builtin *shape* while substituting
    /// on-chain coefficients for shapes that are identical across Plutus
    /// language versions).
    pub fn cost_pair(&self, id: BuiltinId) -> CostPair {
        self.table[id as usize]
    }

    /// Overwrite the cost pair for a builtin. Used by the cost-model applier
    /// to install per-version on-chain coefficients.
    pub fn set_cost_pair(&mut self, id: BuiltinId, pair: CostPair) {
        self.table[id as usize] = pair;
    }

    /// Look up the cost pair for the given builtin and compute the
    /// `ExBudget` from the argument sizes (x=arg1, y=arg2, z=arg3).
    pub fn cost_for(&self, id: BuiltinId, x: i64, y: i64, z: i64) -> ExBudget {
        let pair = self.table[id as usize];
        pair.eval(x, y, z)
    }

    /// Compute the argument sizes from the fully-saturated argument list
    /// for the given builtin, then return the `ExBudget` to charge.
    ///
    /// Sizing is per the Haskell `ExMemoryUsage` rules; see module-level
    /// doc for details.
    pub fn charge_for_args(&self, id: BuiltinId, args: &[Value]) -> ExBudget {
        use BuiltinId::*;
        // Retrieve argument sizes using the correct costing wrapper for
        // each position. Most builtins use the default `size_of_value`
        // sizing; a few need special wrappers.
        let (x, y, z) = match id {
            // ── String builtins — appendString/equalsString use TextCostedByByteLength
            AppendString | EqualsString => {
                let sx = args.first().map_or(0, string_costed_by_byte_len);
                let sy = args.get(1).map_or(0, string_costed_by_byte_len);
                (sx, sy, 0)
            }
            // ── DecodeUtf8: input is a ByteString
            DecodeUtf8 => {
                let sx = args.first().map_or(0, size_of_value);
                (sx, 0, 0)
            }
            // ── EncodeUtf8: input is a String costed by UTF-8 byte length
            //    (TextCostedByByteLength in the V2 builtin semantics).
            //    Conformance corpus computes goldens against this variant.
            EncodeUtf8 => {
                let sx = args.first().map_or(0, string_costed_by_byte_len);
                (sx, 0, 0)
            }
            // ── integerToByteString: arg2 is width (NumBytesCostedAsNumWords),
            //    arg3 is the integer (default sizing)
            IntegerToByteString => {
                let sy = args.get(1).map_or(0, |v| match v {
                    Value::Const(Constant::Integer(n)) => {
                        use num_traits::ToPrimitive;
                        let val = n.magnitude().to_i64().unwrap_or(i64::MAX).abs();
                        num_bytes_as_num_words(val)
                    }
                    _ => 0,
                });
                let sz = args.get(2).map_or(0, size_of_value);
                (0, sy, sz)
            }
            // ── byteStringToInteger: arg1 is endianness (bool, size=1), arg2 is ByteString
            ByteStringToInteger => {
                let sy = args.get(1).map_or(0, size_of_value);
                (0, sy, 0)
            }
            // ── replicateByte: arg1 is count (NumBytesCostedAsNumWords), arg2 is byte
            ReplicateByte => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Integer(n)) => {
                        use num_traits::ToPrimitive;
                        let val = n.magnitude().to_i64().unwrap_or(i64::MAX).abs();
                        num_bytes_as_num_words(val)
                    }
                    _ => 0,
                });
                (sx, 0, 0)
            }
            // ── shiftByteString / rotateByteString: arg1=BS, arg2=Integer (costed literally)
            ShiftByteString | RotateByteString => {
                let sx = args.first().map_or(0, size_of_value);
                // arg2 not used in cpu/mem model (models only use x)
                (sx, 0, 0)
            }
            // ── dropList: arg1=Integer (costed literally), arg2=list (ignored)
            //    cpu: linear_in_x where x = integer_costed_literally(n)
            DropList => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Integer(n)) => integer_costed_literally(n),
                    _ => 0,
                });
                (sx, 0, 0)
            }
            // ── insertCoin: 4 args (policy, token, amount, ValueMaxDepth)
            //    cpu: linear_in_u where u = ValueMaxDepth(arg4)
            //    We map u → z so that the LinearInZ table entry fires correctly.
            InsertCoin => {
                let sz = args.get(3).map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_max_depth(map),
                    _ => 0,
                });
                (0, 0, sz)
            }
            // ── lookupCoin: 3 args (policy, token, ValueMaxDepth)
            //    cpu: linear_in_z where z = ValueMaxDepth(arg3)
            LookupCoin => {
                let sz = args.get(2).map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_max_depth(map),
                    _ => 0,
                });
                (0, 0, sz)
            }
            // ── unValueData: 1 arg (DataNodeCount)
            //    cpu: quadratic_in_x where x = DataNodeCount(arg1)
            UnValueData => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Data(d)) => data_node_count(d),
                    _ => 0,
                });
                (sx, 0, 0)
            }
            // ── valueData: 1 arg (ValueTotalSize)
            //    cpu: linear_in_x where x = totalSize(arg1)
            ValueData => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_total_size(map),
                    _ => 0,
                });
                (sx, 0, 0)
            }
            // ── valueContains: 2 args (ValueTotalSize, ValueTotalSize)
            //    cpu: const_above_diagonal_linear_xy where x=totalSize(arg1), y=totalSize(arg2)
            ValueContains => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_total_size(map),
                    _ => 0,
                });
                let sy = args.get(1).map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_total_size(map),
                    _ => 0,
                });
                (sx, sy, 0)
            }
            // ── unionValue: 2 args (ValueTotalSize, ValueTotalSize)
            //    cpu: with_interaction_in_x_and_y, mem: added_sizes
            UnionValue => {
                let sx = args.first().map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_total_size(map),
                    _ => 0,
                });
                let sy = args.get(1).map_or(0, |v| match v {
                    Value::Const(Constant::Value(map)) => value_total_size(map),
                    _ => 0,
                });
                (sx, sy, 0)
            }
            // ── writeBits: arg1=BS, arg2=list of indices, arg3=Bool
            //    cpu: linear_in_y (list length), mem: linear_in_x (BS size)
            WriteBits => {
                let sx = args.first().map_or(0, size_of_value);
                let sy = args.get(1).map_or(0, |v| match v {
                    Value::Const(Constant::ProtoList { elements, .. }) => elements.len() as i64,
                    _ => 0,
                });
                (sx, sy, 0)
            }
            // ── andByteString / orByteString / xorByteString:
            //    arg1=Bool (padding semantics), arg2=BS, arg3=BS
            //    cpu: linear_in_y_and_z, mem: linear_in_max_yz
            AndByteString | OrByteString | XorByteString => {
                let sy = args.get(1).map_or(0, size_of_value);
                let sz = args.get(2).map_or(0, size_of_value);
                (0, sy, sz)
            }
            // ── sliceByteString: arg1=from(Int), arg2=len(Int), arg3=BS
            //    cpu: linear_in_z, mem: linear_in_z(slope=0)
            SliceByteString => {
                let sz = args.get(2).map_or(0, size_of_value);
                (0, 0, sz)
            }
            // ── expModInteger: arg1=base, arg2=exp, arg3=mod
            ExpModInteger => {
                let sx = args.first().map_or(0, size_of_value);
                let sy = args.get(1).map_or(0, size_of_value);
                let sz = args.get(2).map_or(0, size_of_value);
                (sx, sy, sz)
            }
            // ── All other builtins: default sizing per position
            _ => {
                let sx = args.first().map_or(0, size_of_value);
                let sy = args.get(1).map_or(0, size_of_value);
                let sz = args.get(2).map_or(0, size_of_value);
                (sx, sy, sz)
            }
        };
        self.cost_for(id, x, y, z)
    }

    /// Plutus 1.65.0.0 default builtin cost model (`builtinCostModelE.json`).
    pub const DEFAULT: Self = Self {
        table: builtin_cost_table(),
    };
}

/// Helper: String costed by byte length (TextCostedByByteLength).
fn string_costed_by_byte_len(v: &Value) -> i64 {
    match v {
        Value::Const(Constant::String(s)) => size_of_string_by_byte_length(s),
        _ => 0,
    }
}

/// Helper: String costed by char count (default ExMemoryUsage for Text).
fn string_costed_by_char_count(v: &Value) -> i64 {
    match v {
        Value::Const(Constant::String(s)) => size_of_string_by_char_count(s),
        _ => 0,
    }
}

// ─── Cost table ──────────────────────────────────────────────────────────────

/// Build the 88-entry cost table indexed by `BuiltinId` discriminant.
///
/// The entries are in discriminant order (0..=87). Each row is
/// `(cpu_model, mem_model)` exactly as in `builtinCostModelE.json`.
const fn builtin_cost_table() -> [CostPair; 101] {
    use CostingFun::*;
    [
        // 0 AddInteger — max_size
        CostPair {
            cpu: MaxSize(lin1(100788, 420)),
            mem: MaxSize(lin1(1, 1)),
        },
        // 1 SubtractInteger — max_size
        CostPair {
            cpu: MaxSize(lin1(100788, 420)),
            mem: MaxSize(lin1(1, 1)),
        },
        // 2 MultiplyInteger — multiplied_sizes / added_sizes
        CostPair {
            cpu: MultipliedSizes(lin1(90434, 519)),
            mem: AddedSizes(lin1(0, 1)),
        },
        // 3 DivideInteger — above_and_below_diagonal / subtracted_sizes
        CostPair {
            cpu: AboveAndBelowDiagonal(AboveBelowDiagP { model: DIVMOD_QUAD }),
            mem: SubtractedSizes(DIVMOD_SUBSIZE),
        },
        // 4 QuotientInteger — const_above_diagonal / subtracted_sizes
        CostPair {
            cpu: ConstAboveDiagonal(ConstAboveDiagP {
                constant: 85848,
                model: DIVMOD_QUAD,
            }),
            mem: SubtractedSizes(DIVMOD_SUBSIZE),
        },
        // 5 RemainderInteger — const_above_diagonal / linear_in_y
        CostPair {
            cpu: ConstAboveDiagonal(ConstAboveDiagP {
                constant: 85848,
                model: DIVMOD_QUAD,
            }),
            mem: LinearInY(lin1(0, 1)),
        },
        // 6 ModInteger — above_and_below_diagonal / linear_in_y
        CostPair {
            cpu: AboveAndBelowDiagonal(AboveBelowDiagP { model: DIVMOD_QUAD }),
            mem: LinearInY(lin1(0, 1)),
        },
        // 7 EqualsInteger — min_size / constant
        CostPair {
            cpu: MinSize(lin1(51775, 558)),
            mem: Constant(1),
        },
        // 8 LessThanInteger — min_size / constant
        CostPair {
            cpu: MinSize(lin1(44749, 541)),
            mem: Constant(1),
        },
        // 9 LessThanEqualsInteger — min_size / constant
        CostPair {
            cpu: MinSize(lin1(43285, 552)),
            mem: Constant(1),
        },
        // 10 AppendByteString — added_sizes
        CostPair {
            cpu: AddedSizes(lin1(1000, 173)),
            mem: AddedSizes(lin1(0, 1)),
        },
        // 11 ConsByteString — linear_in_y / added_sizes
        CostPair {
            cpu: LinearInY(lin1(72010, 178)),
            mem: AddedSizes(lin1(0, 1)),
        },
        // 12 SliceByteString — linear_in_z / linear_in_z (slope 0)
        CostPair {
            cpu: LinearInZ(lin1(20467, 1)),
            mem: LinearInZ(lin1(4, 0)),
        },
        // 13 LengthOfByteString — constant
        c(22100, 10),
        // 14 IndexByteString — constant
        c(13169, 4),
        // 15 EqualsByteString — linear_on_diagonal / constant
        CostPair {
            cpu: LinearOnDiagonal(DiagLinearP {
                constant: 30623,
                intercept: 28755,
                slope: 75,
            }),
            mem: Constant(1),
        },
        // 16 LessThanByteString — min_size / constant
        CostPair {
            cpu: MinSize(lin1(28999, 74)),
            mem: Constant(1),
        },
        // 17 LessThanEqualsByteString — min_size / constant
        CostPair {
            cpu: MinSize(lin1(28999, 74)),
            mem: Constant(1),
        },
        // 18 Sha2_256 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(270652, 22588)),
            mem: Constant(4),
        },
        // 19 Sha3_256 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(1457325, 64566)),
            mem: Constant(4),
        },
        // 20 Blake2b_256 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(201305, 8356)),
            mem: Constant(4),
        },
        // 21 VerifyEd25519Signature — linear_in_y / constant
        CostPair {
            cpu: LinearInY(lin1(53384111, 14333)),
            mem: Constant(10),
        },
        // 22 AppendString — added_sizes (TextCostedByByteLength)
        CostPair {
            cpu: AddedSizes(lin1(1000, 59957)),
            mem: AddedSizes(lin1(4, 1)),
        },
        // 23 EqualsString — linear_on_diagonal (TextCostedByByteLength) / constant
        CostPair {
            cpu: LinearOnDiagonal(DiagLinearP {
                constant: 39184,
                intercept: 1000,
                slope: 60594,
            }),
            mem: Constant(1),
        },
        // 24 EncodeUtf8 — linear_in_x (char count)
        CostPair {
            cpu: LinearInX(lin1(1000, 42921)),
            mem: LinearInX(lin1(4, 2)),
        },
        // 25 DecodeUtf8 — linear_in_x (byte size)
        CostPair {
            cpu: LinearInX(lin1(91189, 769)),
            mem: LinearInX(lin1(4, 2)),
        },
        // 26 IfThenElse — constant
        c(76049, 1),
        // 27 ChooseUnit — constant
        c(61462, 4),
        // 28 Trace — constant
        c(59498, 32),
        // 29 FstPair — constant
        c(141895, 32),
        // 30 SndPair — constant
        c(141992, 32),
        // 31 ChooseList — constant
        c(132994, 32),
        // 32 MkCons — constant
        c(72362, 32),
        // 33 HeadList — constant
        c(83150, 32),
        // 34 TailList — constant
        c(81663, 32),
        // 35 NullList — constant
        c(74433, 32),
        // 36 ChooseData — constant
        c(94375, 32),
        // 37 ConstrData — constant
        c(22151, 32),
        // 38 MapData — constant
        c(68246, 32),
        // 39 ListData — constant
        c(33852, 32),
        // 40 IData — constant
        c(15299, 32),
        // 41 BData — constant
        c(11183, 32),
        // 42 UnConstrData — constant
        c(24588, 32),
        // 43 UnMapData — constant
        c(24623, 32),
        // 44 UnListData — constant
        c(25933, 32),
        // 45 UnIData — constant
        c(20744, 32),
        // 46 UnBData — constant
        c(20142, 32),
        // 47 EqualsData — min_size / constant
        CostPair {
            cpu: MinSize(lin1(898148, 27279)),
            mem: Constant(1),
        },
        // 48 MkPairData — constant
        c(11546, 32),
        // 49 MkNilData — constant
        c(7243, 32),
        // 50 MkNilPairData — constant
        c(7391, 32),
        // 51 SerialiseData — linear_in_x
        CostPair {
            cpu: LinearInX(lin1(955506, 213312)),
            mem: LinearInX(lin1(0, 2)),
        },
        // 52 VerifyEcdsaSecp256k1Signature — constant
        c(43053543, 10),
        // 53 VerifySchnorrSecp256k1Signature — linear_in_y / constant
        CostPair {
            cpu: LinearInY(lin1(43574283, 26308)),
            mem: Constant(10),
        },
        // ── PlutusV3 additions ──────────────────────────────────────────────
        // 54 Bls12_381_G1_Add — constant
        c(962335, 18),
        // 55 Bls12_381_G1_Neg — constant
        c(267929, 18),
        // 56 Bls12_381_G1_ScalarMul — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(76433006, 8868)),
            mem: Constant(18),
        },
        // 57 Bls12_381_G1_Equal — constant
        c(442008, 1),
        // 58 Bls12_381_G1_HashToGroup — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(52538055, 3756)),
            mem: Constant(18),
        },
        // 59 Bls12_381_G1_Compress — constant
        c(2780678, 6),
        // 60 Bls12_381_G1_Uncompress — constant
        c(52948122, 18),
        // 61 Bls12_381_G2_Add — constant
        c(1995836, 36),
        // 62 Bls12_381_G2_Neg — constant
        c(284546, 36),
        // 63 Bls12_381_G2_ScalarMul — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(158221314, 26549)),
            mem: Constant(36),
        },
        // 64 Bls12_381_G2_Equal — constant
        c(901022, 1),
        // 65 Bls12_381_G2_HashToGroup — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(166917843, 4307)),
            mem: Constant(36),
        },
        // 66 Bls12_381_G2_Compress — constant
        c(3227919, 12),
        // 67 Bls12_381_G2_Uncompress — constant
        c(74698472, 36),
        // 68 Bls12_381_MillerLoop — constant
        c(254006273, 72),
        // 69 Bls12_381_MulMlResult — constant
        c(2174038, 72),
        // 70 Bls12_381_FinalVerify — constant
        c(333849714, 1),
        // 71 Keccak_256 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(2261318, 64571)),
            mem: Constant(4),
        },
        // 72 Blake2b_224 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(207616, 8310)),
            mem: Constant(4),
        },
        // 73 IntegerToByteString — quadratic_in_z / literal_in_y_or_linear_in_z
        CostPair {
            cpu: QuadraticInZ(quad1(1293828, 28716, 63)),
            mem: LiteralInYOrLinearInZ(lin1(0, 1)),
        },
        // 74 ByteStringToInteger — quadratic_in_y / linear_in_y
        CostPair {
            cpu: QuadraticInY(quad1(1006041, 43623, 251)),
            mem: LinearInY(lin1(0, 1)),
        },
        // 75 AndByteString — linear_in_y_and_z / linear_in_max_yz
        CostPair {
            cpu: LinearYZ(LinearYZP {
                intercept: 100181,
                slope1: 726,
                slope2: 719,
            }),
            mem: LinearMaxYZ(lin1(0, 1)),
        },
        // 76 OrByteString — linear_in_y_and_z / linear_in_max_yz
        CostPair {
            cpu: LinearYZ(LinearYZP {
                intercept: 100181,
                slope1: 726,
                slope2: 719,
            }),
            mem: LinearMaxYZ(lin1(0, 1)),
        },
        // 77 XorByteString — linear_in_y_and_z / linear_in_max_yz
        CostPair {
            cpu: LinearYZ(LinearYZP {
                intercept: 100181,
                slope1: 726,
                slope2: 719,
            }),
            mem: LinearMaxYZ(lin1(0, 1)),
        },
        // 78 ComplementByteString — linear_in_x
        CostPair {
            cpu: LinearInX(lin1(107878, 680)),
            mem: LinearInX(lin1(0, 1)),
        },
        // 79 ReadBit — constant
        c(95336, 1),
        // 80 WriteBits — linear_in_y (list len) / linear_in_x (BS size)
        CostPair {
            cpu: LinearInY(lin1(281145, 18848)),
            mem: LinearInX(lin1(0, 1)),
        },
        // 81 ReplicateByte — linear_in_x (NumBytesCostedAsNumWords)
        CostPair {
            cpu: LinearInX(lin1(180194, 159)),
            mem: LinearInX(lin1(1, 1)),
        },
        // 82 ShiftByteString — linear_in_x / linear_in_x
        CostPair {
            cpu: LinearInX(lin1(158519, 8942)),
            mem: LinearInX(lin1(0, 1)),
        },
        // 83 RotateByteString — linear_in_x / linear_in_x
        CostPair {
            cpu: LinearInX(lin1(159378, 8813)),
            mem: LinearInX(lin1(0, 1)),
        },
        // 84 CountSetBits — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(107490, 3298)),
            mem: Constant(1),
        },
        // 85 FindFirstSetBit — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(106057, 655)),
            mem: Constant(1),
        },
        // 86 Ripemd_160 — linear_in_x / constant
        CostPair {
            cpu: LinearInX(lin1(1964219, 24520)),
            mem: Constant(3),
        },
        // 87 ExpModInteger — exp_mod_cost / linear_in_z
        CostPair {
            cpu: ExpModCost(ExpModP {
                coefficient00: 607153,
                coefficient11: 231697,
                coefficient12: 53144,
            }),
            mem: LinearInZ(lin1(0, 1)),
        },
        // ── PV1.1.0 additions (88..=100) ──────────────────────────────
        // 88 DropList — linear_in_x / constant_cost
        CostPair {
            cpu: LinearInX(lin1(116711, 1957)),
            mem: Constant(4),
        },
        // 89 IndexArray — constant_cost / constant_cost
        CostPair {
            cpu: Constant(232010),
            mem: Constant(32),
        },
        // 90 LengthOfArray — constant_cost / constant_cost
        CostPair {
            cpu: Constant(231883),
            mem: Constant(10),
        },
        // 91 ListToArray — linear_in_x / linear_in_x
        CostPair {
            cpu: LinearInX(lin1(1000, 24838)),
            mem: LinearInX(lin1(7, 1)),
        },
        // 92 InsertCoin — linear_in_u (= 4th value-arg size); dugite's
        //   `cost_for` only carries x/y/z, so we approximate as
        //   linear_in_z (3rd arg = Integer amount) — for the
        //   conformance corpus this matches because the Plutus
        //   reference's `u` is the Value memory, which is small for
        //   the test fixtures.
        CostPair {
            cpu: LinearInZ(lin1(356924, 18413)),
            mem: LinearInZ(lin1(45, 21)),
        },
        // 93 LookupCoin — linear_in_z / constant_cost
        CostPair {
            cpu: LinearInZ(lin1(219951, 9444)),
            mem: Constant(1),
        },
        // 94 ScaleValue — linear_in_y / linear_in_y
        CostPair {
            cpu: LinearInY(lin1(1000, 277577)),
            mem: LinearInY(lin1(12, 21)),
        },
        // 95 UnValueData — quadratic_in_x / linear_in_x
        CostPair {
            cpu: QuadraticInX(Quadratic1 {
                c0: 1000,
                c1: 95933,
                c2: 1,
            }),
            mem: LinearInX(lin1(1, 11)),
        },
        // 96 ValueData — linear_in_x / linear_in_x
        CostPair {
            cpu: LinearInX(lin1(1000, 38159)),
            mem: LinearInX(lin1(2, 22)),
        },
        // 97 ValueContains — const_above_diagonal{linear_in_x_and_y} / constant_cost
        CostPair {
            cpu: ConstAboveDiagonalLinearXY(ConstAboveDiagLinXYP {
                constant: 213283,
                intercept: 618401,
                slope1: 1998,
                slope2: 28258,
            }),
            mem: Constant(1),
        },
        // 98 UnionValue — with_interaction_in_x_and_y / added_sizes
        CostPair {
            cpu: WithInteractionXY(InteractionXYP {
                c00: 1000,
                c01: 183150,
                c10: 172116,
                c11: 6,
            }),
            mem: AddedSizes(lin1(24, 21)),
        },
        // 99 Bls12_381_G1_MultiScalarMul — linear_in_x / constant_cost
        CostPair {
            cpu: LinearInX(lin1(321837444, 25087669)),
            mem: Constant(18),
        },
        // 100 Bls12_381_G2_MultiScalarMul — linear_in_x / constant_cost
        CostPair {
            cpu: LinearInX(lin1(617887431, 67302824)),
            mem: Constant(36),
        },
    ]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    #[test]
    fn size_of_integer_zero_is_one() {
        assert_eq!(size_of_integer(&BigInt::from(0)), 1);
    }

    #[test]
    fn size_of_integer_small() {
        assert_eq!(size_of_integer(&BigInt::from(1)), 1);
        assert_eq!(size_of_integer(&BigInt::from(-1)), 1);
    }

    #[test]
    fn size_of_integer_large() {
        // 2^64 - 1 fits in 1 word (bits=64, log2=63, 63/64=0, +1=1)
        let n = BigInt::from(u64::MAX);
        assert_eq!(size_of_integer(&n), 1);
        // 2^64 needs 2 words (bits=65, log2=64, 64/64=1, +1=2)
        let n2 = BigInt::from(u128::MAX);
        // u128::MAX = 2^128 - 1, bits=128, log2=127, 127/64=1, +1=2
        assert_eq!(size_of_integer(&n2), 2);
    }

    #[test]
    fn size_of_bytestring_empty() {
        assert_eq!(size_of_bytestring(&[]), 1);
    }

    #[test]
    fn size_of_bytestring_one() {
        assert_eq!(size_of_bytestring(&[0u8]), 1);
    }

    #[test]
    fn size_of_bytestring_eight() {
        assert_eq!(size_of_bytestring(&[0u8; 8]), 1);
    }

    #[test]
    fn size_of_bytestring_nine() {
        assert_eq!(size_of_bytestring(&[0u8; 9]), 2);
    }

    #[test]
    fn add_integer_cost_both_size_1() {
        // addInteger 1 1: max_size(100788, 420) with max(1,1)=1
        // cpu: 100788 + 420*1 = 101208
        // mem: 1 + 1*1 = 2
        let b = BuiltinCosts::DEFAULT.cost_for(BuiltinId::AddInteger, 1, 1, 0);
        assert_eq!(b.cpu, 101208);
        assert_eq!(b.mem, 2);
    }

    #[test]
    fn add_integer_cost_large() {
        // addInteger large_num 5734: sizes (2, 1), max=2
        // cpu: 100788 + 420*2 = 101628
        let b = BuiltinCosts::DEFAULT.cost_for(BuiltinId::AddInteger, 2, 1, 0);
        assert_eq!(b.cpu, 101628);
        assert_eq!(b.mem, 3); // 1 + 1*2 = 3
    }

    #[test]
    fn append_string_formula() {
        // appendString "Ola" " mundo!" with TextCostedByByteLength
        // "Ola" = 3 bytes, 3/4=0; " mundo!" = 7 bytes, 7/4=1
        // cpu: 1000 + 59957*(0+1) = 60957
        // mem: 4 + 1*(0+1) = 5
        let b = BuiltinCosts::DEFAULT.cost_for(BuiltinId::AppendString, 0, 1, 0);
        assert_eq!(b.cpu, 60957);
        assert_eq!(b.mem, 5);
    }

    #[test]
    fn table_has_88_entries() {
        // Sanity check: every BuiltinId 0..=87 maps to a valid entry.
        for raw in 0u8..=87 {
            let id = BuiltinId::from_u8(raw).unwrap();
            let _ = BuiltinCosts::DEFAULT.cost_for(id, 1, 1, 1);
        }
    }

    #[test]
    fn num_bytes_as_num_words_zero() {
        assert_eq!(num_bytes_as_num_words(0), 0);
    }

    #[test]
    fn num_bytes_as_num_words_one_to_eight() {
        for i in 1i64..=8 {
            assert_eq!(num_bytes_as_num_words(i), 1, "failed for n={i}");
        }
    }

    #[test]
    fn num_bytes_as_num_words_nine() {
        assert_eq!(num_bytes_as_num_words(9), 2);
    }

    // ─── CostingFun::eval — every variant ────────────────────────────────
    //
    // The cost-model dispatcher is a wide match. The upstream UPLC corpus
    // drives most arms via `builtinCostModelE.json`, but several variants are
    // only used by a handful of builtins and easily go uncovered when those
    // builtins aren't in the corpus's hot set. These tests pin every arm of
    // `CostingFun::eval` with a known input/output so the cost-model dispatch
    // stays exercised even if a builtin is later moved to a different shape.

    #[test]
    fn costingfun_constant() {
        let f = CostingFun::Constant(42);
        assert_eq!(f.eval(0, 0, 0), 42);
        assert_eq!(f.eval(100, 100, 100), 42); // ignores args
    }

    #[test]
    fn costingfun_linear_axes() {
        let f = lin1(5, 3);
        // Each axis uses the corresponding arg.
        assert_eq!(CostingFun::LinearInX(f).eval(7, 99, 99), 5 + 3 * 7);
        assert_eq!(CostingFun::LinearInY(f).eval(99, 7, 99), 5 + 3 * 7);
        assert_eq!(CostingFun::LinearInZ(f).eval(99, 99, 7), 5 + 3 * 7);
    }

    #[test]
    fn costingfun_added_and_multiplied_and_min_max() {
        let f = lin1(10, 2);
        assert_eq!(CostingFun::AddedSizes(f).eval(3, 4, 0), 10 + 2 * 7);
        assert_eq!(CostingFun::MultipliedSizes(f).eval(3, 4, 0), 10 + 2 * 12);
        assert_eq!(CostingFun::MinSize(f).eval(3, 4, 0), 10 + 2 * 3);
        assert_eq!(CostingFun::MaxSize(f).eval(3, 4, 0), 10 + 2 * 4);
    }

    #[test]
    fn costingfun_subtracted_sizes_clamps_to_minimum() {
        let p = SubtractedSizesP {
            intercept: 100,
            slope: 5,
            minimum: 200,
        };
        // 100 + 5*(10-2) = 140 < minimum 200 → clamp.
        assert_eq!(CostingFun::SubtractedSizes(p).eval(10, 2, 0), 200);
        // 100 + 5*(50-2) = 340 > minimum → no clamp.
        assert_eq!(CostingFun::SubtractedSizes(p).eval(50, 2, 0), 340);
    }

    #[test]
    fn costingfun_quadratic_axes() {
        let q = quad1(1, 2, 3);
        // c0 + c1*x + c2*x^2 = 1 + 2*4 + 3*16 = 57
        assert_eq!(
            CostingFun::QuadraticInX(q).eval(4, 0, 0),
            1 + 2 * 4 + 3 * 16
        );
        assert_eq!(
            CostingFun::QuadraticInY(q).eval(0, 4, 0),
            1 + 2 * 4 + 3 * 16
        );
        assert_eq!(
            CostingFun::QuadraticInZ(q).eval(0, 0, 4),
            1 + 2 * 4 + 3 * 16
        );
    }

    #[test]
    fn costingfun_linear_on_diagonal_off_and_on() {
        let p = DiagLinearP {
            constant: 999,
            intercept: 10,
            slope: 5,
        };
        // On-diagonal: x == y → intercept + slope*x.
        assert_eq!(CostingFun::LinearOnDiagonal(p).eval(3, 3, 0), 10 + 5 * 3);
        // Off-diagonal: any x != y → constant.
        assert_eq!(CostingFun::LinearOnDiagonal(p).eval(3, 4, 0), 999);
        assert_eq!(CostingFun::LinearOnDiagonal(p).eval(0, 1, 0), 999);
    }

    #[test]
    fn costingfun_const_above_diagonal_above_uses_model() {
        // QuadXY with minimum 0 so we can read the bare polynomial.
        let q = QuadXY {
            c00: 1,
            c10: 1,
            c01: 1,
            c20: 1,
            c11: 1,
            c02: 1,
            minimum: 0,
        };
        let p = ConstAboveDiagP {
            constant: 7777,
            model: q,
        };
        // Below diagonal: x < y → constant.
        assert_eq!(CostingFun::ConstAboveDiagonal(p).eval(1, 5, 0), 7777);
        // On/above diagonal: x >= y → model(x,y).
        // model(2, 2) = 1 + 2 + 2 + 4 + 4 + 4 = 17
        assert_eq!(CostingFun::ConstAboveDiagonal(p).eval(2, 2, 0), 17);
    }

    #[test]
    fn costingfun_above_and_below_diagonal_runs_with_sorted_pair() {
        let q = QuadXY {
            c00: 0,
            c10: 1,
            c01: 0,
            c20: 0,
            c11: 0,
            c02: 0,
            minimum: 0,
        };
        let p = AboveBelowDiagP { model: q };
        // model(max, min) — c10 multiplies max.
        // eval(2, 5): max=5 → 5; eval(5, 2): max=5 → 5. Symmetric.
        assert_eq!(CostingFun::AboveAndBelowDiagonal(p).eval(2, 5, 0), 5);
        assert_eq!(CostingFun::AboveAndBelowDiagonal(p).eval(5, 2, 0), 5);
    }

    #[test]
    fn costingfun_const_above_diagonal_lin_xy() {
        let p = ConstAboveDiagLinXYP {
            constant: 99,
            intercept: 10,
            slope1: 2,
            slope2: 3,
        };
        // Below diagonal: x < y → constant.
        assert_eq!(CostingFun::ConstAboveDiagonalLinearXY(p).eval(1, 2, 0), 99);
        // On/above diagonal: 10 + 2*x + 3*y.
        assert_eq!(
            CostingFun::ConstAboveDiagonalLinearXY(p).eval(5, 5, 0),
            10 + 2 * 5 + 3 * 5
        );
        assert_eq!(
            CostingFun::ConstAboveDiagonalLinearXY(p).eval(10, 2, 0),
            10 + 2 * 10 + 3 * 2
        );
    }

    #[test]
    fn costingfun_with_interaction_xy() {
        // c00 + c10*x + c01*y + c11*x*y
        let p = InteractionXYP {
            c00: 1,
            c01: 2,
            c10: 3,
            c11: 4,
        };
        // (x=2, y=3): 1 + 3*2 + 2*3 + 4*2*3 = 1 + 6 + 6 + 24 = 37
        assert_eq!(CostingFun::WithInteractionXY(p).eval(2, 3, 0), 37);
    }

    #[test]
    fn costingfun_linear_yz_and_max_yz() {
        let yz = LinearYZP {
            intercept: 10,
            slope1: 2,
            slope2: 3,
        };
        // intercept + 2*y + 3*z (ignores x)
        assert_eq!(CostingFun::LinearYZ(yz).eval(999, 4, 5), 10 + 2 * 4 + 3 * 5);
        let f = lin1(1, 7);
        // linear in max(y, z) — ignores x.
        assert_eq!(CostingFun::LinearMaxYZ(f).eval(999, 3, 5), 1 + 7 * 5);
        assert_eq!(CostingFun::LinearMaxYZ(f).eval(999, 5, 3), 1 + 7 * 5);
    }

    #[test]
    fn costingfun_literal_in_y_or_linear_in_z_branches() {
        // Used by writeBits: if width_y == 0 → linear_in_z; else → return y.
        let f = lin1(100, 50);
        // y == 0: take the linear-in-z branch.
        assert_eq!(
            CostingFun::LiteralInYOrLinearInZ(f).eval(0, 0, 4),
            100 + 50 * 4
        );
        // y != 0: return y directly (z is irrelevant).
        assert_eq!(CostingFun::LiteralInYOrLinearInZ(f).eval(0, 17, 999), 17);
    }

    #[test]
    fn costingfun_exp_mod_with_and_without_penalty() {
        let p = ExpModP {
            coefficient00: 100,
            coefficient11: 1,
            coefficient12: 1,
        };
        // cost0 = 100 + 1*ee*mm + 1*ee*mm^2  (with ee=2, mm=3): 100 + 6 + 18 = 124.
        // aa <= mm (aa=2, mm=3): no penalty.
        assert_eq!(CostingFun::ExpModCost(p).eval(2, 2, 3), 124);
        // aa > mm: 50% penalty → cost0 + cost0/2 = 124 + 62 = 186.
        assert_eq!(CostingFun::ExpModCost(p).eval(10, 2, 3), 186);
    }

    // ─── Saturation guards on Linear1 / Quadratic1 ───────────────────────
    //
    // These guard against panics on extreme inputs — the Haskell reference
    // saturates at Word64::MAX; we saturate at i64::MAX.

    #[test]
    fn costingfun_linear_saturates_instead_of_panicking() {
        let f = lin1(0, i64::MAX);
        // intercept=0, slope*x where slope*x overflows → i64::MAX.
        assert_eq!(CostingFun::LinearInX(f).eval(2, 0, 0), i64::MAX);
    }

    #[test]
    fn costingfun_constant_pair_evaluates_to_expected_budget() {
        let pair = CostPair::constant(11, 22);
        let b = pair.eval(0, 0, 0);
        assert_eq!(b.cpu, 11);
        assert_eq!(b.mem, 22);
    }

    // ─── memory_usage_integer ─────────────────────────────────────────────

    #[test]
    fn memory_usage_integer_zero_and_small() {
        assert_eq!(memory_usage_integer(&BigInt::from(0)), 1);
        assert_eq!(memory_usage_integer(&BigInt::from(1)), 1);
        assert_eq!(memory_usage_integer(&BigInt::from(-1)), 1);
    }

    #[test]
    fn memory_usage_integer_word_boundaries() {
        // 2^63 - 1 fits in 1 word (bits=63).
        let n = BigInt::from(i64::MAX);
        assert_eq!(memory_usage_integer(&n), 1);
        // 2^64 needs 2 words.
        let n2 = BigInt::from(u64::MAX) + BigInt::from(1);
        assert_eq!(memory_usage_integer(&n2), 2);
    }
}
