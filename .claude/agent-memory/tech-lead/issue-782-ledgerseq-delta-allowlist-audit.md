---
name: issue-782-ledgerseq-delta-allowlist-audit
description: LedgerSeq delta model was an undocumented allowlist missing 11+ LedgerState fields; fixed with a compile-time guard test
type: reference
---

`LedgerDelta`/`apply_delta_to_state` (crates/dugite-ledger/src/ledger_seq.rs) is the
rollback-reconstruction path used by `rollback_via_seq`, `advance_anchor`, and
`state_at_index`. It was built as an undocumented ALLOWLIST: only fields some prior
bug report (#763 epoch_blocks_by_pool, ep292 reward_accounts) actually observed as
stale got a snapshot field. Issue #782 found 11 more `LedgerState` fields with ZERO
delta representation: `utxo.pending_donations`, `era`, `certs.pointer_map`,
`certs.script_stake_credentials`, `certs.total_stake_key_deposits`,
`certs.pending_mir_{reserves,treasury,delta_reserves,delta_treasury}`,
`genesis_delegates` (top-level, NOT in any sub-state — needs an explicit copy-back in
`rollback_via_seq` too, since that fn does `self.certs = new_state.certs` etc.
wholesale but assigns top-level fields like `era`/`pending_donations` one at a time),
`consensus.opcert_counters`, `consensus.extra_entropy`, `epochs.pending_pp_updates` /
`future_pp_updates`, `epochs.rupd_addrs_rew`.

**Fix pattern per field** (mirrors the existing reward_accounts_snapshot/pool_params_snapshot
idiom): add `Option<T>` field to `LedgerDelta` (or `BlockFieldsDelta` for cheap scalars),
capture pre-block clone + post-block content-diff (or unconditional if the field is
tiny/imbl/Arc — O(1) clone) in `apply_block_with_delta_impl` (state/apply.rs), restore
in `apply_delta_to_state` / `apply_block_fields` / `apply_epoch_transition_delta`
(ledger_seq.rs).

**Perf-conscious deviation**: `opcert_counters: HashMap<Hash28,u64>` is a plain
(non-Arc) map that grows unboundedly for the life of the chain (never cleared) and is
mutated for AT MOST ONE pool_id per block (in `compute_shelley_nonce`). A full-map
content-diff would cost O(pool count) forever. Instead captured a targeted
`Option<(Hash28, u64)>` single-key delta (read back the post-block value for that one
key) — O(1) and exact, since only one key can change per block.

**Guard test added**: `_assert_ledger_state_fields_audited` in ledger_seq.rs —
`#[allow(dead_code)]`, never called, destructures `LedgerState { field_a: _, ... }`
with NO `..` rest pattern, listing every field explicitly (including `pub(crate)
cached_validation_registry`). Adding a future `LedgerState` field without touching
this function is now a COMPILE ERROR, forcing the author into
`apply_block_with_delta_impl` + `apply_delta_to_state` + (if top-level)
`rollback_via_seq`. This class of bug (silent allowlist omission) is otherwise
invisible to both unit tests and offline replay — see [[project_763_root_cause_block_counter_2026_06_22]] for the prior instance of exactly this failure mode.

**SNAPSHOT_VERSION 24→25**: bumped even though `LedgerStateSnapshot`'s bincode LAYOUT
is unchanged (confirmed via `snapshot_format_hash_stability` test staying green with
the same EXPECTED_HASH) — the bump exists purely to quarantine anchors that may have
been silently mis-advanced by the OLD (incomplete) delta model before being persisted.
This is a legitimate exception to the "bump only on layout change" rule stated in the
constant's own doc comment.

**Test idiom for rollback-correctness regressions** (crates/dugite-ledger/src/state/tests.rs,
new section after the Mithril-ancillary tests): MUST go through the REAL
`apply_block_with_delta` + `LedgerSeq::push` + `rollback_via_seq` path, not manually
constructed `LedgerDelta` literals — the bug class lives in the SNAPSHOT-CAPTURE
decision inside `apply_block_with_delta_impl`, which a hand-built delta bypasses
entirely (would validate the restore logic but never catch a capture regression).
Added `make_certs_block` (generalizes `make_pool_registration_block` to arbitrary
cert lists) and `make_pool_params` helpers. Every fix in this cluster was verified by
temporarily reverting it and confirming the new test fails with the exact "stale
value" symptom, then restoring — not just "test passes with fix present."

**#783(c) "compensating add+remove" construction gotcha**: a naive same-block
add-to-map-A + remove-from-map-A test is easy, but for `pending_retirements` the ONLY
way to remove an entry per-block is re-registering that pool (which ALWAYS also
touches `future_pool_params`), so a naive 2-pool version doesn't isolate the
length-check blind spot (the length change is caught via the OTHER map's `.len()`
still tripping the OR-check). Isolating it requires pre-staging the cancelled pool
in `future_pool_params` from an EARLIER block so the compensating block's
re-registration OVERWRITES that entry in place (length stays 1→1) rather than
growing it (0→1) — only then do ALL THREE legacy signals (pool_params Arc identity,
pending_retirements.len(), future_pool_params.len()) stay unchanged while content
on both maps genuinely differs.
