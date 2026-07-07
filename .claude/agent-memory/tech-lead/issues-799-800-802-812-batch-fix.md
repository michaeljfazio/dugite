---
name: issues-799-800-802-812-batch-fix
description: 4-issue Conway governance batch fix (2026-07-06) — submission-order ratify tie-break (SNAPSHOT v25->v26), CC zero-threshold/minSize ordering, atomic+bounded PParams enactment, dead-path pvCanFollow exact-minor
metadata:
  type: project
---

Branch `fix/ledger-review-2026-07-04`. All 4 fixes oracle-confirmed (cardano-ledger-oracle, live-verified 2026-07-04/07-06) before implementation — see `.claude/agent-memory/cardano-ledger-oracle/conway-ratify-precision-facts.md` and `bounded-ratio-decode-and-enact-totality.md`.

## #799 — ratify tie-break must use submission order, not GovActionId (hash) order
Haskell `reorderActions` (`Governance/Internal.hs:534-544`) is `sortOn (actionPriority . gasAction) . toList` — stable, ties preserve OMap/submission order. dugite's `proposals: ImblOrdMap<GovActionId, _>` iterates by hash, so ties silently picked the smaller-hash proposal instead of the first-submitted one → wrong pparam/hardfork/committee winner on same-priority collisions.

Fix: added `ProposalState.submission_index: u64`, sourced from `GovernanceState::proposal_count` read BEFORE increment, at every ingest site — live path `eras/conway.rs::process_governance_votes_and_proposals` (~1881), dead-path `state/governance.rs::process_proposal`/`process_proposal_with_delta` (~230/~536), and `state/mod.rs::from_haskell_snapshot` reconstruction (enumeration index over `decode_proposals`' output — confirmed by that decoder's own doc comment that it returns entries in wire/OMap-insertion order, i.e. `StrictSeq`, not a re-sortable map). `ratify_proposals_impl`'s candidate tuple gained a 4th element; sort key became `(gov_action_priority(action), submission_index)`.

Non-obvious gotcha found and fixed beyond the literal issue text: `from_haskell_snapshot` never set `gov.proposal_count` at all (stayed at struct-default 0), which would have made a live-submitted proposal's `submission_index` collide with (or precede) reconstructed ones after a Haskell-dump import. Added `gov.proposal_count = gov.proposal_count.max(proposal_count as u64)` after the reconstruction loop.

Live block-apply path does NOT use `GovernanceChange::ProposeAction`/`governance_changes` at all for rollback — that delta vec is confirmed dead (see `ledger_seq.rs:166` comment: "never populated"). The live path instead snapshots the WHOLE `Arc<GovernanceState>` into `LedgerDelta.gov_snapshot` on every governance-mutating block. This means `submission_index` needs zero extra rollback plumbing for the live path — it rides along for free inside the wholesale Arc snapshot. Only the dead-code delta path (`process_proposal_with_delta` → `GovernanceChange::ProposeAction` → `ledger_seq.rs:1373`) uses the per-mutation delta, and it just clones the already-tagged `ProposalState`, so no separate fix was needed there either.

SNAPSHOT_VERSION 25→26 (GovernanceState/ProposalState embedded wholesale in bincode). `snapshot_format_hash_stability` test did NOT need a new EXPECTED_HASH — `canonical_ledger_state()` has zero governance proposals, and bincode serializes an empty map identically regardless of the value type's field count.

Test added: `test_ratify_tie_break_uses_submission_order_not_hash_order` — deliberately makes the SECOND-submitted proposal have the SMALLER GovActionId, proving the fix isn't accidentally correct via hash ordering. Also tightened the pre-existing loose `test_competing_proposals_same_prev_action_id` assertion (was `enacted_id == &id1 || enacted_id == &id2` with a comment admitting ambiguity) to assert deterministically on `id1` (first-submitted).

## #800 — CC zero-threshold shortcut must come AFTER the committeeMinSize gate
Old code: `if threshold.is_zero() { return true; }` fired before `active_size`/`committee_min_size` were even computed. Haskell's `votingThreshold` only yields `VotingThreshold t` when `bootstrap || activeCommitteeSize >= minSize`; otherwise `NoVotingThreshold` fails the CC leg outright regardless of threshold value. Fixed by moving the min-size gate first, then the zero-threshold auto-pass, then the all-abstain short-circuit (order matters: zero-threshold must still auto-pass an all-abstain vote, since Haskell's `0 %? 0 = True`).

**This fix broke 4 pre-existing tests** that set `committee_threshold = Some(0/1)` "so CC auto-approves" but never registered any CC members, relying on `mainnet_defaults().committee_min_size == 7` never being checked (the exact bug). Fixed by adding `committee_min_size = 0` alongside the threshold override in each: `test_hard_fork_ratification`, `test_parameter_change_ratification`, `test_treasury_withdrawal_ratification`, `test_treasury_withdrawal_via_governance_reduces_treasury` (all in `state/tests.rs`), mirroring the pattern `gov_test_state()` in governance.rs already used for the same reason. This is the textbook "test pinned the bug" case the task anticipated.

## #802 — atomic PParams enactment + rho/tau/d bounds, NOT a0
Oracle-confirmed: `UnitInterval`/`BoundedRatio` CBOR decode structurally rejects num>den (both PV≥12 and legacy paths route through `boundRational`); `NonNegativeInterval` (a0, minFeeRefScriptCostPerByte) has NO upper bound by design — "do not invent an upper-bound clamp for a0 in a Rust port"; Conway ENACT is `PredicateFailure = Void`, a total function, no re-validation possible. Oracle's practical-implication note: the *ideal* fix location is decode/proposal-submission time, not enactment — but the issue scopes the fix to enactment-time defense-in-depth, which is what got implemented (decode-time gap is a separate, unfiled, out-of-scope follow-up).

Fix (identical in both `protocol_params.rs::apply_protocol_param_update` and `governance.rs::apply_protocol_param_update_impl`): hoisted every `validate_threshold` call (15 pre-existing dvt_/pvt_ + 3 new rho/tau/d) to the top of the function, before any field write, so an `Err` leaves state fully untouched. `d` was previously not validated at all despite being `UnitInterval`-typed; note d was removed from PParams at Babbage per the oracle, so the `d`-update path is realistically pre-Conway-only, but the shared function still needed the check since Conway's copy shares the code.

Pre-existing, out-of-scope divergence noticed but NOT touched: `governance.rs::apply_protocol_param_update_impl` is missing the 4 Dijkstra-era fields (`max_ref_script_size_per_block/tx`, `ref_script_cost_stride/multiplier`) that `protocol_params.rs::apply_protocol_param_update` has — the two functions were already non-identical before this fix in that one respect.

## #812 — dead-path pvCanFollow exact +1 minor (not "any greater minor")
`governance.rs::process_proposal`/`process_proposal_with_delta` (both `#[allow(dead_code)]`, tests/future-wiring only) accepted `tgt_minor > cur_minor`; Haskell requires exactly `cur_minor + 1`. Fixed both copies identically. Confirmed the LIVE path (`eras/conway.rs::process_governance_votes_and_proposals`) has NO pvCanFollow check at all — a separate, more serious admission-time gap, filed as a follow-up by the user (not fixed here per explicit scope).

## Related
[[issues-794-795-797-808-809-789-801-batch-fix]] — prior batch in the same ledger-review series (2026-07-06 morning session; this is the afternoon follow-on).
