---
name: pool-retirement-output-too-small-wire-gaps
description: Two accept-with-wrong-reason/generic-fallback N2C reject gaps fixed - PoolRetirement of unregistered pool used the wrong predicate rule, OutputTooSmall had no wire encoder at all
metadata:
  type: project
---

Surfaced by tx-zoo negatives 16g (retire-nonexistent-pool) and 15m
(token-only-output-below-minUTxO) on branch `issue-1027-ledger-state-decodable`
(worktree `conway-testing-issues`), 2026-08-06.

## Gap 1: PoolRetirement of an unregistered pool used the DELEG predicate, not POOL

`crates/dugite-ledger/src/validation/mod.rs` had ONE shared loop (around the
"Delegation to unregistered pool" section) that pushed
`ValidationError::DelegateePoolNotRegistered` for BOTH stake-delegation certs
AND `PoolRetirement` certs pointing at an unregistered pool ID. Haskell raises
two DIFFERENT predicates for the identical "unregistered pool" condition:
`DelegateeStakePoolNotRegisteredDELEG` (DELEG rule) for delegation certs, but
`StakePoolNotRegisteredOnKeyPOOL` (POOL rule, `ShelleyPoolPredFailure` reused
unmodified in Conway) for retirement. Wrong-rule-same-shape bugs like this are
invisible to any same-process round-trip test — only a Haskell-shape byte
comparison catches them.

Fix: split the `PoolRetirement` cert out of the shared delegation-target match
into its own `if let` arm, raising a NEW `ValidationError::StakePoolNotRegisteredForRetirement`
distinct from `DelegateePoolNotRegistered`. Wired through
`crates/dugite-node/src/node/serve.rs`'s `convert_validation_error` to a new
`TxValidationError::StakePoolNotRegisteredOnKeyPOOL { pool_id }` in
`crates/dugite-network/src/lib.rs`, encoded via the EXISTING `encode_pool_failure`
helper at POOL tag 0 (`[2,[1,[2,[0,bstr28]]]]`) in
`crates/dugite-network/src/protocol/local_tx_submission/encode.rs`.

**The retirement-epoch bounds check (`StakePoolRetirementWrongEpochPOOL`,
POOL tag 1) was ALREADY correctly implemented and wired** —
`ValidationError::PoolRetirementTooLate`/`PoolRetirementTooEarly` in
`phase1.rs` (Rule 1e), mapped in serve.rs, encoded via `encode_pool_failure`
tag 1 with the 3-field (not 4-field — Haskell drops one Mismatch field)
shape. No gap there; do not re-audit.

## Gap 2: OutputTooSmall had a `TxValidationError` variant but ZERO CBOR encoder arm

`TxValidationError::OutputTooSmall { minimum, actual }` existed in
`dugite-network/src/lib.rs` but had no `encode_conway_ledger_pred_failure`
match arm at all — it silently fell through to the generic
`ConwayMempoolFailure` (tag 7) catch-all. Haskell's ONLY reachable form on a
Conway tx is `BabbageOutputTooSmallUTxO` (`ConwayUtxoPredFailure` tag 21,
`NonEmpty (TxOut era, Coin)` — the pre-Babbage tag-9 `OutputTooSmallUTxO`
bare-`TxOut`-list form is structurally unreachable and deliberately NOT
implemented).

Two-part fix:
1. `ValidationError::OutputTooSmall` (dugite-ledger `validation/mod.rs`)
   gained a third field, `output_index: usize`, set from `.enumerate()` at
   the Rule 5 raise site in `phase1.rs`. This was a SAFE field addition —
   every existing consumer (tests, serve.rs) already matched with `..`,
   so it compiled with zero breakage elsewhere.
2. `enrich_validation_errors` in `dugite-node/src/node/serve.rs` (the #1025
   aggregation layer, already merged on main) gained a new block that
   groups every `OutputTooSmall` occurrence from ONE `validate_transaction`
   call — dugite still raises one error PER offending output, matching
   Haskell's own per-tx `NonEmpty` aggregation only at this higher layer —
   re-encoding each offending output via `output_index` lookup into
   `tx.body.outputs` (reusing the `raw_output_hex` helper #1025 already
   introduced) paired with its required minimum coin, into ONE new
   `TxValidationError::BabbageOutputTooSmallUTxO { outputs: Vec<(String, u64)> }`.
   Encoded via `encode_utxo_failure(enc, 21, ...)` as a LIST (not set, not
   map — a `TxOut` could legitimately repeat) of `array(2)[txout_cbor, min_coin]`.

The bare (non-enriched) `TxValidationError::OutputTooSmall` mapping in
`convert_validation_error` is kept as the fallback for an out-of-range
`output_index` — unlike most #1025-era fallbacks this does NOT degrade to
`ScriptFailed`, because `OutputTooSmall` already had its own (wire-unencoded)
variant before this session; the `remaining_generic_failures_are_a_closed_justified_set`
test in serve.rs only tracks arms that map to `ScriptFailed`, so this variant
was never in that JUSTIFIED list and needed no entry there.

## Reusable pattern confirmed twice more

[[issue-1025-typed-wire-arms-residual-generic-mempool-reject]] (if that memory
doesn't exist yet, the source is `git log -1 -p e1022eb302`, "fix(node,network):
typed wire arms for 5 of the residual generic mempool-reject failures (#1025)")
is the template for BOTH gap classes: (1) same-shape-different-rule bugs
need a Haskell BYTE PATH comparison, never a shape-only check; (2) when
`dugite-ledger` raises N per-item errors but Haskell wants ONE aggregated
`NonEmpty`, the aggregation belongs in `dugite-node/serve.rs`'s
`enrich_validation_errors`, keyed by whatever locator field
(`output_index`, `oversized_outputs: Vec<usize>`, etc.) lets the enrichment
re-derive the wire payload from the already-decoded `tx` — extending the
`ValidationError` struct with an index field is cheap and safe as long as
existing match sites use `..`.
