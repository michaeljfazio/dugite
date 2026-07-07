---
name: issue-804-genesisdeleg-mir-quorum
description: GenesisKeyDelegation live-path two-phase queue + MIR genesis-quorum/GenesisDelegCert witness gaps (issue #804), SNAPSHOT_VERSION 27->28
metadata:
  type: project
---

Implemented issue #804 on branch `fix/ledger-review-2026-07-04` (uncommitted — task explicitly said do not commit/branch).

## Oracle corrections to the issue text (both verified live against IntersectMBO/cardano-ledger, not the pre-built KB)

1. **Maturation window is `1 x stabilityWindow` (ceil(3k/f)), NOT `2x`.** The issue (and the pre-existing dead-code comment in `state/certificates.rs`) both said `2 * stabilityWindow`. That figure is a *different* mechanism — `getTheSlotOfNoReturn`'s PPUP/HFC "point of no return" deadline (`Cardano.Ledger.Slot`), unrelated to `GenesisDelegTxCert` maturation. The real Haskell code: `s' = slot +* Duration sp` where `sp = stabilityWindow` directly, no doubling.
2. **Adoption runs EVERY BLOCK via TICK, not just at epoch boundaries.** Haskell's `adoptGenesisDelegs` is called from `validatingTickTransition` (TICK's sole transition, which also runs NEWEPOCH) unconditionally on every block's own slot — a `TICKF` comment explicitly says "the genesis delegates are updated not only on the epoch boundary." Implemented as an unconditional call at the top of `apply_block_impl` (before Byron early-return, before the per-tx loop), comparing `fgdSlot <= block.slot().0`.
3. **MIR quorum checks the DELEGATE (hot) keys, not genesis (cold) keys.** `validateMIRInsufficientGenesisSigs`: `Set.fromList $ asWitness . genDelegKeyHash <$> Map.elems genMapping` — the map's VALUES. This is the opposite side from the pre-existing (and already-dead-on-live-path) `ValidationContext::genesis_delegates` field, which holds cold/genesis key hashes for `NonGenesisUpdatePPUP`. Required a NEW `genesis_delegate_keys` field, not reuse of the existing one.
4. Quorum check lives in **UTXOW**, not DELEG; keyed on `dsGenDelegs` only (never `dsFutureGenDelegs`); structurally impossible in Conway (`babbageUtxowMirTransition` not in Conway's `transitionRules`, `isInstantaneousRewards` is `AtMostEra "Babbage"`).

Full oracle Q&A archived in `.claude/agent-memory/cardano-ledger-oracle/shelley-genesisdeleg-and-mir-witness-quorum.md`.

## Part A — live two-phase queue

- New top-level `LedgerState.future_gen_delegs: HashMap<(u64, Hash28), (Hash28, Hash32)>` (state/mod.rs, next to `genesis_delegates`) — deliberately placed top-level (not inside `CertSubState`) so the #782 compile-time guard `_assert_ledger_state_fields_audited` (ledger_seq.rs) fails until wired through the full delta model, mirroring `genesis_delegates`'s exact pattern (LedgerDelta snapshot field, capture in `apply_block_with_delta_impl`, restore in `apply_delta_to_state`, explicit copy-back in `rollback_via_seq`).
- `apply_shelley_cert`'s `GenesisKeyDelegation` arm can't mutate this field directly (only has `&mut CertSubState`, not top-level `LedgerState`) — added two new free functions in `eras/common.rs` instead: `enqueue_genesis_key_delegations` (called from `state/apply.rs`'s per-tx loop, right after `apply_valid_tx` returns, guarded `pv < 9`) and `adopt_matured_genesis_delegs` (called unconditionally once per block). This is a **new architectural pattern** worth remembering: when a cert needs to mutate a field that lives on `LedgerState` directly rather than a sub-state, handle it at the `state/apply.rs` orchestrator level rather than widening the `EraRules::apply_valid_tx` trait signature across all 6 era implementors.
- SNAPSHOT_VERSION 27 -> 28; `future_gen_delegs` added to `LedgerStateSnapshot` right after `genesis_delegates` (positional bincode). `tests/snapshot_stability.rs`'s `EXPECTED_HASH` canary updated (`de7754e7...c082f`) — this is the SECOND time this exact canary needed updating in this session's memory; the failure message conveniently prints the new hash, no manual recomputation needed.
- Fixed the dead `state/certificates.rs::process_certificate` handler's comment (it wrongly claimed `2x stability_window` and "observationally equivalent" — both false per the oracle) WITHOUT changing its behavior (it's genuinely dead — grepped every call site, all `#[cfg(test)]`). Do not treat that dead handler as a correctness reference.

## Part B — witnesses

1. `cert_required_witnesses` (phase1.rs): `GenesisKeyDelegation` now returns the 28-byte truncation of `genesis_hash` as a required VKey witness (was `vec![]`).
2. New whole-transaction check `mir::check_mir_genesis_quorum` (NOT folded into the existing per-cert `validate_mir_cert` — Haskell's version is a UTXOW predicate over the whole tx, not a DELEG per-cert one). New `ValidationContext` fields `genesis_delegate_keys: Option<Arc<HashSet<Hash28>>>` (delegate/hot keys — distinct from the pre-existing `genesis_delegates` cold-key field) and `update_quorum: Option<u64>`. New `ValidationError::MIRInsufficientGenesisSigs { present, required, signers }`.
3. Wired live: `state/apply.rs`'s per-tx `ValidationContext` construction (the ValidateAll hot path) now builds `genesis_delegate_keys` from `self.genesis_delegates.values()` per-tx (NOT hoisted into the big block-level registry-snapshot tuple like pools/dreps/reward_accounts — genesis_delegates is at most ~7 entries even at full Shelley bootstrap, so per-tx rebuild is negligible and touching that tuple's complex type signature wasn't worth the risk).
4. **Cross-crate ripple discovered by `cargo build --workspace`, not by reasoning ahead of time**: `dugite-node/src/node/serve.rs::convert_validation_error` exhaustively matches every `ValidationError` variant (no wildcard arm) to build the N2C-facing `TxValidationError`. Adding the new enum variant broke this immediately (E0004 non-exhaustive). Fixed by mapping to the existing `TxValidationError::ScriptFailed { reason: format!(...) }` wire variant — this is the SAME established pattern already used for every other MIR/PPUP predicate in that file (`MIRCertificateTooLateInEpoch`, `NonGenesisUpdatePPUP`, etc. all ride `ScriptFailed` too, confirmed via `encode.rs`'s own comment: "This variant currently maps to ScriptFailed in serve.rs, so it won't reach here"). No `dugite-network` wire-format change needed. **Lesson: any new `dugite_ledger::validation::ValidationError` variant WILL require a `dugite-node/src/node/serve.rs` update — this exhaustive match is an undocumented but real cross-crate coupling point.**

## Reachability

Both AtMostEra Babbage (dead in new Conway blocks). Live-severity is real only for from-genesis Shelley-era replay under ValidateAll and adversarial-tx defense-in-depth — matches the issue's own P1-but-historical framing.

## Tests added (10 new, all passing)

- `state/tests.rs`: `test_804_genesis_key_delegation_enqueues_not_adopted`, `test_804_genesis_key_delegation_adopted_after_stability_window`, `test_804_genesis_key_delegation_rollback_restores_queue` (full `apply_block`/`LedgerSeq`/`rollback_via_seq` round trip — set `state.epochs.protocol_params.protocol_version_major = 8` to pass the new `pv < 9` enqueue gate even though `make_certs_block`/`make_test_block` hardcode `block.era = Conway`; the enqueue/adopt gate reads ledger-state pv, not block era, so this combination is valid and much simpler than building a custom pre-Conway `Block`).
- `validation/tests.rs`: 2 witness tests (missing/present) for `GenesisKeyDelegation`, built by truncating a `blake2b_224(vkey)` witness hash into a crafted `genesis_hash`'s first 28 bytes (can't invert blake2b224, so you construct the cert's hash FROM the witness, not vice versa — same trick the existing `make_cert_vkey_witness` helper uses for pool-owner tests).
- `validation/mir/tests.rs`: 5 unit tests for `check_mir_genesis_quorum` (no-MIR no-op, Conway no-op, missing-context lenient, below-quorum rejected, quorum-met accepted).

## Gate result

`cargo build --workspace --all-targets`, `cargo nextest run --workspace` (7197 passed), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` — all green.
