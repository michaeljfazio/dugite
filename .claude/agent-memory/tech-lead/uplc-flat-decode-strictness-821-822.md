---
name: uplc-flat-decode-strictness-821-822
description: Implemented #821 (builtin-availability + Constr/Case version + tag-bound gate) and #822 (flat trailing-bytes/TooMuchSpace) in dugite-uplc; uncovered and fixed a latent #835 double-filler encoder bug that #822 unmasked
type: reference
---

Implemented on branch `fix/ledger-review-2026-07-04` (uncommitted, working
tree only). Extends the root-cause-A `(language, pv)` threading
([[uplc-root-cause-a-pv-gates-819-820-824-828]]) into the flat-DECODE layer,
which that pass explicitly deferred.

## #821 — builtin availability + Constr/Case version gate + Constr tag bound

- `BuiltinId::is_available_in(language, major_pv) -> bool`
  (`crates/dugite-uplc/src/term.rs`) — a static range-match on `as_u8()`
  reproducing the oracle's 6-batch table
  (`.claude/agent-memory/cardano-haskell-oracle/plutus-builtin-availability-gate.md`)
  exactly: batch boundaries are contiguous wire-ID ranges (0-50/51/52-53/
  54-72/73-74/75-86/87-100 → catch-all) because `BuiltinId`'s discriminants
  are themselves batch-ordered. This is a DISTINCT axis from
  `SemanticsVariant` (existence vs. denotation/costing of an
  already-available builtin) — do not conflate the two tables.
- `validate_program_availability(term, version, language, major_pv)` +
  recursive `validate_term_depth`/`validate_term_inner`
  (`crates/dugite-uplc/src/flat/term.rs`) — walks the ALREADY-DECODED term
  tree checking (1) every `Builtin` via `is_available_in`, (2) `Constr`/
  `Case` require program `version >= (1,1,0)` (mirrors the textual parser's
  existing gate at `syn/parser.rs`, now also enforced on the consensus flat
  path), (3) from PV11 a `Constr` tag is capped at 1024
  (`CONSTR_TAG_BOUND_PV`/`MAX_CONSTR_TAG`). Uses the same
  `stacker::maybe_grow` depth-safe recursion pattern as `decode_term_depth`
  (re-walking an already-decoded tree needs the same stack-growth guard).
- **Cache hazard, how handled**: `eval_redeemer::SCRIPT_DECODE_CACHE` is
  keyed on raw script bytes only, and decode is (language, pv)-independent
  by construction (same bytes always decode to the same `Term` tree
  regardless of context) — but Haskell's actual accept/reject verdict on
  those same bytes IS (language, pv)-dependent (e.g. a `bls12_381_G1_add`
  reference decodes fine as a `Term` under any context but is only
  *available* to a V1 script from PV11). Folding the availability check
  into the decode (or its cache) would let a cache hit from one
  (language, pv) context leak a wrong verdict into another. Fix: run
  `validate_program_availability` as a SEPARATE pass, unconditionally,
  every time `decode_script_bytes` returns — cache hit or not — from
  `eval_resolved_redeemer` (`eval_redeemer.rs`, right after the decode
  call, before the program is used). No cache-key change was needed or
  made; this trades a full O(program-size) tree-walk on every redeemer
  evaluation (including cache hits) for correctness. Not benchmarked —
  flag as a possible follow-up (secondary cache keyed on
  `(bytes, language, major_pv)`) if `apply_bench` regresses.
- The ledger-language-not-available pre-check (Haskell
  `LedgerLanguageNotAvailableError`, `ledgerLanguageIntroducedIn ll <= pv`)
  did NOT already exist anywhere in dugite (grepped dugite-ledger +
  dugite-uplc, zero hits) — it was NOT added in this pass; only the 3
  sub-gates explicitly scoped to #821 were implemented. Real gap: dugite
  currently has no structural rejection of e.g. a PlutusV3 script
  reference before PV9. Worth its own follow-up issue if not already
  filed.
- Single `UplcError::FlatDecode` variant used throughout (not `Internal` —
  `Internal` is adversary-reachable per issue #840) — matches Haskell's own
  granularity (`CBORDeserialiseError (OtherReason msg)` covers
  builtin-unavailability, constr-tag-bound, and constant-header-bound
  alike; only ledger-language gets a typed constructor).

## #822 — flat trailing bytes / TooMuchSpace

- `Program::from_flat` (`crates/dugite-uplc/src/program.rs`) now checks
  `BitReader::bits_remaining() == 0` after the mandatory `read_filler()`
  call and rejects with `FlatDecode("TooMuchSpace: ...")` otherwise.
  `bits_remaining()` was already `pub`; no new BitReader API needed.
  Canonical writer output always lands the filler's terminating `1` bit
  within the current (already-aligned-target) byte, so this correctly
  distinguishes "legitimate final padding" from "adversary appended bytes
  after a valid program" without any special-casing.
- `Program::from_cbor`'s SEPARATE trailing-outer-CBOR-bytes gap (Haskell:
  `RemainderError`, V3+ only, V1/V2 exempt — a distinct ledger-language-
  gated rule) was explicitly left untouched, matching the issue's own
  "related, lighter... likely at the ledger layer" framing and the sibling
  #835/#842 out-of-scope note.

## Unmasked latent bug: #835 double-filler in `to_flat` (fixed, scoped)

Implementing #822 correctly immediately broke 5 existing tests (4 in
`eval_redeemer.rs`, 1 integration test `cek_v3_spend_71579b77_flat_evaluates`
in `tests/phase2_onchain_budget.rs`) with `TooMuchSpace: 8 trailing bit(s)`.
Root cause: `Program::to_flat()` called `w.write_filler()` itself AND THEN
`w.finish()` (which ALSO unconditionally calls `write_filler()`) — every
`to_flat`/`to_cbor`-encoded program carried a spurious extra `0x01`
sentinel byte at the end. This is issue #835, and its own text predicted
exactly this: "fix that first or the round-trip tests will mask this one."
Since leaving 5 tests broken is not acceptable (CLAUDE.md: all tests must
pass) and the fix is a 1-line redundant-call removal with no behavioral
ambiguity (`finish()`'s doc comment already states it appends "the `1 0*`
filler that the flat spec requires"), fixed it as a minimal, scoped
prerequisite: removed the explicit `w.write_filler()` call in `to_flat()`
(`crates/dugite-uplc/src/program.rs`), relying solely on `finish()`.
This is NOT a full #835 audit — only the one call site `to_flat` exercises
was touched. The committed binary fixture
`crates/dugite-uplc/tests/fixtures/phase2_onchain/applied-Spend-0.flat`
(captured via `DUGITE_DUMP_APPLIED_DIR`, which calls `to_flat()`) had the
same baked-in double-`0x01` tail and was truncated by exactly 1 byte to
match the corrected encoder output; re-verified it still decodes to the
same `Program` and evaluates to the same result.

## Verification

999-corpus (`cargo nextest run -p dugite-uplc --features conformance -E
'binary(conformance)'`) stayed byte-identical (999/999) — expected, since
the corpus is real Haskell-flat bytes never round-tripped through dugite's
(buggy, now-fixed) encoder, and is E-variant/no-PV so it doesn't exercise
the PV<11 availability branches at all (structurally blind, per
[[uplc-root-cause-a-pv-gates-819-820-824-828]]). Full crate suite
(`cargo nextest run -p dugite-uplc --features conformance`): 1515/1515.
Full workspace (`cargo nextest run --workspace`): 7224 passed, 13 skipped
(pre-existing skips, unrelated). clippy/fmt clean on both
`-p dugite-uplc` and `--workspace`.

Related: [[uplc-root-cause-a-pv-gates-819-820-824-828]],
`.claude/agent-memory/cardano-haskell-oracle/plutus-builtin-availability-gate.md`.
