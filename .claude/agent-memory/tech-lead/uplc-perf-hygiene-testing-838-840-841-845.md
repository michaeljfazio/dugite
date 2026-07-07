---
name: uplc-perf-hygiene-testing-838-840-841-845
description: uplc perf (TxInfo cache via Rc-shared ScriptContext), error taxonomy (MachineError split from Internal), debug-scaffolding removal, and test-gap audit for #838/#840/#841/#845 (2026-07-06)
type: reference
---

Branch `fix/ledger-review-2026-07-04`, all 4 issues implemented and gated
green (999/999 conformance byte-identical, 1606/1606 dugite-uplc tests,
7318/7318 workspace tests). Non-consensus (perf/hygiene/tests), lower risk
than the ledger-review batches.

## #838 (perf: O(n²) TxInfo rebuild + env deep-clone)

Only did Fix 1 (TxInfo hoist) — Fix 2 (Rc<Constant> for the CEK env/Value
layer) was NOT attempted; it's a much wider, separate-commit-worthy change
per the verdict and out of scope for this pass.

Key discovery: `populate_tx_info_v1/v2/v3` are pure functions of
`(tx, resolved, slot_config)` ONLY — they do NOT take the per-redeemer `r`
at all (confirmed by reading `build_script_context` in eval_redeemer.rs:
`ScriptContextV{1,2,3} { tx_info, purpose/redeemer/script_info }` — the
purpose/redeemer is layered on separately). So rebuilding per redeemer was
pure waste, AND V2/V3's build internally re-runs `resolve_redeemers` over
**every** redeemer in the tx each time — the real O(n²).

Fix: `crates/dugite-uplc/src/eval_redeemer.rs` — new `pub(crate) struct
TxInfoCache { v1/v2/v3: Option<Rc<TxInfoVn>> }` with lazy `get_or_build_vN`
methods. `eval_resolved_redeemer` takes `tx_info_cache: &mut TxInfoCache`
(new last param); `phase_two.rs::eval_phase_two_raw` owns one `TxInfoCache`
per tx, created once before the redeemer loop, passed by `&mut` to every
call. Lazy build-on-first-use (not eager-build-all-languages-up-front)
was a deliberate choice: it preserves the exact original error-surfacing
order (a `PhaseTwoError` from building a language's TxInfo still surfaces
at exactly the redeemer index that would have hit it today).

To get the SECOND half of the win (not just "build once" but also "share
without cloning"), `ScriptContextV1/V2/V3.tx_info` field type changed from
owned `TxInfoV{1,2,3}` to `Rc<TxInfoV{1,2,3}>` (script_context.rs). Since
`to_data(&self)` only ever reads through `self.tx_info.to_data(...)`,
`Rc`'s `Deref` makes this a no-op change at every read site — only
construction sites needed `Rc::new(...)` wrapping (2 in script_context.rs
tests, 1 in tx_info.rs test, 1 in phase2_script_context_regression.rs
integration test — grepped exhaustively, confirmed no cross-crate usage of
`ScriptContextV*`/`TxInfoV*` outside dugite-uplc). Net effect: `.clone()`
on the cached value is an O(1) refcount bump, not a deep Vec clone.

Also hoisted the `DUGITE_PHASE2_UNCAPPED` env read (was polled twice per
redeemer in phase_two.rs, lines then 334/356) to once before the loop —
same fix requested by both #838 and #841's verdicts, done once.

`eval_resolved_redeemer`'s visibility had to drop from `pub` to
`pub(crate)`: it's not used outside dugite-uplc (only `eval_phase_two_raw`
is the external entry point, confirmed via grep across dugite-ledger/
dugite-node/dugite-cli), and `TxInfoCache` being `pub(crate)` in a `pub fn`
signature is a rustc `private_interfaces` warning under the workspace's
zero-warnings gate.

Byte-identical verification: 999/999 conformance corpus + all
phase2_onchain_budget fixtures (real captured mainnet/preprod txs with
known Haskell is_valid/ExUnits) stayed green — these fixtures are exactly
the right regression net for this refactor since they assert exact
ExUnits consumption, which would catch any TxInfo-content divergence.

## #840 (adversary-reachable errors mislabeled Internal)

Added `UplcError::MachineError(String)` (error.rs, after `FreeVariable`,
before `Internal`) and reclassified exactly the 6 sites the verdict named
— no more, no less:
- `machine/env.rs`: De Bruijn index-0 sentinel + out-of-range index (both
  in `Env::lookup`).
- `machine/step.rs`: case-on-non-enumerable-constant, case-scrutinee-not-
  Constr-or-enumerable, apply-non-function, force-non-Delay.

Left untouched (confirmed genuinely internal / explicitly out of scope
per the verdict): `bls.rs`'s arity-guaranteed `None` arms; the #828.5
constrData-tag-overflow `Internal` (tracked separately as #859 — a
representational limitation, not an adversary-input mislabel); step.rs's
`State::Done` re-step and the two "empty after non-empty check" defensive
arms (all three are structurally unreachable given the surrounding code's
own invariants, not data-dependent — correctly Internal).

Reachability nuance worth remembering: `eval_redeemer.rs` calls
`scope_check::check_scope` on the fully-applied term BEFORE CEK runs, and
scope_check's whole job is catching unbound de Bruijn indices — so on the
*production phase-2 path*, `env.rs`'s two branches are actually caught
earlier as `FreeVariable`, not hit at all. They're still correctly
`MachineError` (not `Internal`) because (a) `machine::step::evaluate`/
`evaluate_with_budget` are public API also used directly by the UPLC
conformance harness and `dugite-node/bin/replay_phase2.rs` without a
scope_check pre-pass, and (b) semantically these ARE Haskell's
`OpenTermEvaluatedMachineError` class regardless of which caller reaches
them. `step.rs`'s 4 sites, by contrast, are genuinely reachable on the
production path too — UPLC is untyped, so ill-typed apply/force/case-shape
mismatches are NOT caught by scope_check (which only checks variable
scoping, not term "type" shape).

Updated 5 test assertions from `Err(UplcError::Internal(_))` to
`Err(UplcError::MachineError(_))`: `env.rs::index_zero_is_sentinel_error`,
`step.rs::force_of_non_delay_errors`, `apply_non_function_errors`,
`open_term_var_errors`, `case_with_bytestring_scrutinee_errors`.

Confirmed safe to add the enum variant: no exhaustive `match` on
`UplcError` exists anywhere in the workspace (`PhaseTwoError::
is_script_evaluation_failure` matches on `PhaseTwoError`, not the inner
`UplcError` — the `ScriptEvaluationFailed(_)` wildcard arm absorbs it).
`PhaseTwoError::Internal` (phase_two.rs) is a completely different enum,
untouched, still the correct CollectErrors-class whole-tx-rejection type
per the #818/#833 gates — do not conflate the two.

## #841 (debug scaffolding removal)

Three edits, all pure removal / rename, zero behavior change with env vars
unset (production default):
1. `builtin/denotations.rs` `UnIData` arm: deleted the `DUGITE_DUMP_CTX`
   env-check + `eprintln!`; `Err(other) => {...}` collapsed to `Err(_) =>
   Err(builtin_failure(...))` (had to rename the binding since `other`
   became unused).
2. `DUGITE_PHASE2_UNCAPPED` double-poll — same fix as #838's hoist, done
   once in phase_two.rs (not eval_redeemer.rs, contra the issue text —
   verified the actual site during re-grep, matches the verdict's
   anchor-note correction).
3. `denotations.rs` test renamed `unwired_builtin_returns_internal` →
   `builtin_wrong_arity_returns_internal`; comment corrected —
   `VerifyEcdsaSecp256k1Signature` IS wired (the test only passes because
   empty args hit the `builtin_arity_mismatch` guard, which correctly
   returns `Internal` since arity mismatches ARE a genuine dugite-uplc bug,
   not adversary-reachable, given the dispatch layer is supposed to
   guarantee argument count before invoking any denotation).

## #845 (test-coverage gaps) — audit result: verdict was partially stale

Before adding anything, grepped the existing suite for every sibling issue
number the verdict listed as a potential gap. Result: **most are already
covered by prior batches**, confirmed via direct grep + test-name checks,
not just doc comments:
- Adversarial decode-rejection: #821 (1.0.0 Constr/Case gate — 10+ tests
  in `flat/term.rs`), #822 (trailing bytes — `program.rs::
  from_flat_rejects_trailing_byte_after_valid_program`), #823/#816/#827/
  #832 all have named test/doc coverage already.
- **Fuzz targets: ALREADY EXIST, contradicting the verdict's "none exist
  in the workspace today" claim.** `fuzz/fuzz_targets/
  dugite_uplc_program_decode.rs` (Program::from_cbor/from_flat, no-panic +
  flat round-trip identity, ASAN-aware depth clamping) and
  `dugite_uplc_data_decode.rs` (Data::from_cbor, no-panic + CBOR round-trip
  identity) are both wired into `fuzz/Cargo.toml` as
  `fuzz_dugite_uplc_program_decode` / `fuzz_dugite_uplc_data_decode`. The
  verdict's "none exist" claim was stale by the time this task ran — do
  not re-add these.
- Cost-model golden vs real on-chain data: `cost_apply.rs::
  mainnet_v3_reference_costs` already feeds a REAL mainnet V3 cost-model
  array (297 params, from Koios `epoch_params`,
  `tests/fixtures/mainnet_plutus_v3_costmodel.json`) through `apply_v3`
  and pins real coefficient-derived costs — this IS a "vs cardano-ledger"
  golden, just not literally a ScriptContext leaf-diff.
- `phase2_onchain_budget.rs` already validates real captured mainnet/
  preprod txs (with ground-truth Haskell `is_valid`/declared ExUnits)
  end-to-end through `eval_phase_two_raw` — this is the closest thing to
  "golden ScriptContext vs cardano-ledger" achievable without a literal
  Haskell-side dump, since it validates the FULL pipeline against
  Haskell's own on-chain verdict.

Genuine gap found and fixed: **V1 had no wrong-length cost-model golden**
even though V2 and V3 each had one (`v2_wrong_length_is_padded_or_
truncated_never_rejected`, `v3_wrong_length_...`) — added
`v1_wrong_length_is_padded_or_truncated_never_rejected` in `cost_apply.rs`
mirroring the same pattern (`apply_v1` shares `pad_or_truncate` with V2/V3,
same never-reject contract, #826).

Also added `phase2_script_context_regression.rs::
script_context_v1_populated_fixture_exact_cbor_golden` — the ONE
genuinely-missing item that survived the audit: no existing test pinned
the full exact CBOR bytes of a populated (non-trivial) ScriptContext
end-to-end (every existing test only pattern-matches on `Data` shape
fragments). Built a concrete non-empty `ScriptContextV1` (one input, one
output, fee, signer, valid range, Spending purpose), ran it once to
capture the ACTUAL encoder output (did NOT hand-fabricate the expected hex
— captured via a real test run, then hardcoded), and pinned it with a
doc comment marked `TODO(#845 follow-up)` explaining a real cardano-ledger
dump should replace/augment it when available. This is explicitly
sanctioned by the task instructions ("hand-constructed golden vectors...
clearly mark where real dumps should be dropped in, rather than
fabricating fixtures") — the distinction is: derived from the actual
(already unit-tested-correct) implementation and pinned as a regression
net, not invented independently.

Crypto edge vectors (#825 ed25519, #828 ecdsa) also confirmed already
covered by prior batches (see [[uplc-bls-unlifting-and-hardening-816-827-839-843]]
and [[uplc-root-cause-a-pv-gates-819-820-824-828]]).

## Gate results (2026-07-06)

`cargo build -p dugite-uplc --all-targets` clean.
`cargo nextest run -p dugite-uplc --features conformance`: 1606/1606 pass
(999/999 conformance corpus byte-identical — confirmed via explicit count,
not just "all green").
`cargo clippy -p dugite-uplc --all-targets --features conformance -- -D
warnings` clean (had to fix one clippy::expect_used violation — this
crate `deny`s `unwrap_used`/`expect_used` outside `#[cfg(test)]`, so
`TxInfoCache::get_or_build_v{1,2,3}` had to use `if let Some(cached) =
&self.vN { return Ok(cached.clone()); }` instead of
`.clone().expect(...)`).
`cargo fmt -p dugite-uplc -- --check` clean.
`cargo build --workspace --all-targets` clean.
`cargo nextest run --workspace`: 7318/7318 pass, 13 skipped (pre-existing
skips, unrelated).
`cargo clippy --workspace --all-targets -- -D warnings` clean.
`cargo fmt --all -- --check` clean.
