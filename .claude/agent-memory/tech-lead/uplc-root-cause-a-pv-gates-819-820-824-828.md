---
name: uplc-root-cause-a-pv-gates-819-820-824-828
description: Implemented #819/#820/#824/#828 PV-gates in dugite-uplc (text sizing, div/mod cost shape, case-on-constant, denotation edges); 999-corpus stayed byte-identical
type: reference
---

Implemented the "root-cause-A" (language,pv)-threading foundation plus 4 of its
gate fixes in `crates/dugite-uplc/` on branch `fix/ledger-review-2026-07-04`
(uncommitted — working-tree only). `SemanticsVariant` (A/B/C/D/E,
`crates/dugite-uplc/src/builtin/semantics.rs`) was ALREADY threaded through
the machine/denotation layer before this task; it was NOT threaded into the
cost layer (`charge_for_args`, `apply_v3`) — that was the actual plumbing gap.

## What changed (all within dugite-uplc, zero cross-crate callers of the
## touched signatures — confirmed via repo-wide grep)

- `BuiltinCosts::charge_for_args` (`builtin/cost.rs`) gained a `variant:
  SemanticsVariant` param. `AppendString|EqualsString|EncodeUtf8` now select
  `string_costed_by_char_count` (PV<11, A/B/C) vs `string_costed_by_byte_len`
  (PV>=11, D/E) via new `SemanticsVariant::text_costed_by_byte_length()`.
  Call sites: `builtin/dispatch.rs` (`force_builtin`/`apply_builtin`, which
  already received `variant`).
- `cost_apply::apply_v3` gained a `major_pv: u32` param. `v3_division_cpu`
  gained an `above_and_below: bool` selecting `ConstAboveDiagonal` (C,
  PV<11) vs `AboveAndBelowDiagonal` (E, PV>=11) — **only** for
  `divideInteger`/`modInteger` (via `is_variant_d(major_pv)`, reused from
  the V1/V2 gate — same PV11 threshold). `quotientInteger`/
  `remainderInteger` hardcode `false` at both call sites (unchanged at
  every PV, per the issue). Sole external call site:
  `eval_redeemer.rs::resolve_applied_costs` (already had `major_pv` in
  scope). Found: `apply_v3` previously emitted the C-shape UNCONDITIONALLY
  regardless of PV — this was backwards from what the plan doc assumed
  ("still emit E shapes"); the DEFAULT/context-free table
  (`builtin_cost_table()`, used by `evaluate()`/`new_counting`/corpus) was
  ALREADY correctly E-shaped and needed no change.
- `machine/step.rs::return_compute`'s `Frame::Cases` `Value::Const` arm now
  checks `!variant.case_on_constant_available()` (false for A/B/C) and
  returns `UplcError::ScriptError` before projecting the constant —
  matches the OTHER `case` failure paths in the same match (branch-count
  mismatch also uses `ScriptError`). `Value::Constr` arm untouched.
- `builtin/denotations.rs`: `Trace` now fails with `BuiltinTypeError` on a
  non-`String` first arg (was silently returning arg2 — a prior unit test
  pinned the WRONG behavior and had to be rewritten);
  `SliceByteString`'s `start`/`len` now go through `bigint_to_i64_or_failure`
  (existing helper, already used by shift/rotate) + new `i64_to_usize_clamped`
  instead of the silent `bigint_to_usize_clamped` (which stays correct and
  untouched for `DropList` — oracle-confirmed unbounded-Integer semantics,
  do not "fix" it); `VerifyEcdsaSecp256k1Signature` short-circuits to
  `Bool(false)` when either raw 32-byte sig half is all-zero, BEFORE
  calling `k256::Signature::from_bytes` (which is stricter than
  libsecp256k1 and would otherwise wrongly fail); `ExpModInteger` gained
  the missing modulus upper bound (`< 2^8191`) and switched the base/exp
  bound check from symmetric `magnitude() >= 2^8191` to asymmetric
  `x < -2^8191 || x >= 2^8191` (so `-2^8191` i.e. `minBoundI` is now
  correctly ACCEPTED); `ConstrData` gained
  `variant.constr_data_requires_word64()` (new `SemanticsVariant` method) —
  D/E keep the existing `BuiltinFailure` on out-of-u64 tags, A/B/C instead
  return `UplcError::Internal` with an explicit "known representational
  limitation" message (see below).

## #828.5 ConstrData — the one KNOWN INCOMPLETE fix (scoped, not solved)

Haskell's `Data::Constr` tag is `Integer` (arbitrary precision) at ALL
protocol versions; only the BUILTIN's declared argument type changes
(`Word64` unlift at D/E vs plain `Integer` at A/B/C). dugite's
`Data::Constr(u64, ...)` cannot represent a negative or >u64::MAX tag at
ANY variant — this is a hard representational wall, not a logic bug. Net
effect of the gate: for the fully-representable domain (`0..=u64::MAX`)
behavior is IDENTICAL across all 5 variants (no observable change). For the
non-representable domain, both branches still fail, but D/E raises
`BuiltinFailure` (a genuine protocol-level failure, byte-correct) while A/B/C
raises `UplcError::Internal` (an honest "dugite can't represent this" signal
— Haskell would actually SUCCEED here and keep running). Fully closing this
requires widening `Data::Constr`'s tag to a signed/bignum type across
`data.rs`, CBOR encode/decode, `cost.rs` sizeData, and `script_context.rs` —
explicitly out of scope; issue #828 itself calls this the "lowest
reachability" edge (transient, adversarial, pre-PV11 only, never observable
in on-chain CBOR since the wire format is always Word64-tagged).

## Verification method that matters here

The 999-case conformance corpus (`crates/dugite-uplc/tests/conformance/`,
gated behind `--features conformance`) runs a SINGLE (`SemanticsVariant::
LATEST` = E, no-PV) harness — it validates ONLY the PV>=11 / D/E side of
every gate and is STRUCTURALLY BLIND to a wrong PV<11 branch. Isolate it
from the 481 always-on unit tests via `cargo nextest run -p dugite-uplc
--features conformance -E 'binary(conformance)'` (999/999, separate nextest
binary named `conformance`) to get a clean before/after diff signal — the
combined run just reports 1495 total and doesn't tell you the split. Every
one of the 4 PV-gates above needed a hand-written PV-matrix test
(`apply_v3(&p, 9)` vs `apply_v3(&p, 11)`, `run_variant(id, args,
SemanticsVariant::C)` vs `::E`, etc.) — there is no shortcut via the
existing corpus or upstream fixtures for the PV<11 branch.

Also found while auditing: NO existing unit tests existed for
`ExpModInteger` or `VerifyEcdsaSecp256k1Signature` denotations at all before
this task (only an arity-mismatch smoke test) — needed real k256-generated
crypto material (`SigningKey::from_bytes` + `to_encoded_point(true)`) to
exercise the zero-scalar path.

Related: [[builtin-semantics-variant-costing]] (haskell-oracle memory, the
authoritative variant table used to design these gates),
[[uplc-builtin-flat-id-mismatch]] (prior, unrelated UPLC wire-format bug
class in this crate).
