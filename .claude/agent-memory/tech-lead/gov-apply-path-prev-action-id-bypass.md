---
name: gov-apply-path-prev-action-id-bypass
description: process_governance_votes_and_proposals bypassed InvalidPrevGovActionId — proposals with stale prev_id admitted silently, fail ratification forever
metadata:
  type: feedback
---

`b0a6da398` added `InvalidPrevGovActionId` validation to `LedgerState::process_proposal` and
`process_proposal_with_delta` (used only in tests), but left the production block-apply path
`ConwayRules::apply_valid_tx → process_governance_votes_and_proposals` in `conway.rs` completely
unvalidated.

**Root cause:** Two separate code paths process proposals:
1. `LedgerState::process_proposal` — has all Haskell `proposalsAddAction` checks (cases a, b, c)
2. `ConwayRules::apply_valid_tx → process_governance_votes_and_proposals` — inserts directly, NO validation

Any fix to governance submission validation MUST touch BOTH paths.

**Symptom:** Proposal with `prev_id = Some(stale)` (from a previous devnet run) gets admitted.
Votes accumulate. At every epoch boundary `prevActionAsExpected` silently rejects ratification.
10e assertion times out at 900s. The `gov-state` query shows the proposal sitting in-flight
with `prev_id=null` or mangled ppupdate.

**Fix (commit 1f1367a82):**
- Promote `genesis_root_is_valid` and `prev_action_matches_enacted_root` to `pub(crate)` in `governance.rs`
- Import and apply them inside `process_governance_votes_and_proposals` before inserting the proposal
- Invalid proposals are silently dropped (matching Haskell GOV rule: predicate failure = drop, not halt)

**Why:** Haskell's LEDGER rule invokes GOV rule inline per-tx during block application.
Dugite's `process_governance_votes_and_proposals` IS that GOV rule path. When fixes land
in `process_proposal`, they must be mirrored there too.

**How to apply:** Any future change to proposal validation logic (new checks, updated Haskell semantics)
must update BOTH `process_proposal` (tests + direct LedgerState calls) AND
`process_governance_votes_and_proposals` (block-apply path). A test in `conway.rs` using
`apply_valid_tx` is the only way to catch apply-path regressions.
