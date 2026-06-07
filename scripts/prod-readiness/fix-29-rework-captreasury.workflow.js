export const meta = {
  name: 'fix-29-rework-captreasury',
  description: 'BYTE-EXACT rework of #29: transient cap_treasury decremented by the FULL withdrawal fold per enact (cap check); keep epochs.treasury.0 disbursed-decremented (real money)',
  phases: [{ title: 'Fix', detail: 'cap_treasury (full-fold) for the cap check; registered-then-deregistered test' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['files_changed', 'diff_summary', 'test_added', 'test_fails_pre_rework_reasoning', 'all_registered_test_still_passes', 'checks', 'caveats', 'completed'],
  properties: {
    files_changed: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    test_added: { type: 'string' },
    test_fails_pre_rework_reasoning: { type: 'string', description: 'concrete reasoning (ideally empirically confirmed by temporarily reverting) that the new edge test would WRONGLY PASS the 2nd withdrawal under fix v1 (cap-check vs epochs.treasury.0) and correctly BLOCKS it after the cap_treasury rework' },
    all_registered_test_still_passes: { type: 'boolean', description: 'test_two_treasury_withdrawals_both_enact_in_one_pass (two 400M registered, both enact, treasury=200M) still passes' },
    checks: {
      type: 'object', additionalProperties: false, required: ['fmt', 'clippy', 'nextest'],
      properties: { fmt: { type: 'boolean' }, clippy: { type: 'boolean' }, nextest: { type: 'boolean' } },
    },
    caveats: { type: 'string' },
    completed: { type: 'boolean' },
  },
}

phase('Fix')
const fix = await agent(
  'BYTE-EXACT REWORK of dugite backlog #29 in the MAIN working tree (do NOT create a worktree; do NOT git commit). Single crate: '
  + 'dugite-ledger, single file crates/dugite-ledger/src/state/governance.rs. The current uncommitted fix v1 (cap-check against '
  + 'live epochs.treasury.0, accumulator deleted) is already in the tree — build ON TOP of it.\n\n'
  + 'WHY (gauntlet wq63ah2hg REFUTED v1): v1 cap-checks against epochs.treasury.0, which enact_gov_action_impl (~:2288) '
  + 'decrements by `disbursed` (REGISTERED-account total only). Canonical Haskell Conway Enact.hs decrements the TRANSIENT '
  + 'cap-basis ensTreasury by the FULL declared `fold wdrls` per enact (regardless of target registration); unregistered targets '
  + 'are filtered only LATER at the epoch boundary (applyEnactedWithdrawals) against the REAL casTreasury. dugite conflates both '
  + 'into epochs.treasury.0. So for a target registered-at-proposal then DEREGISTERED-before-enact (disbursed=0) plus a 2nd '
  + 'withdrawal in the same pass, v1 under-subtracts the cap basis (treasury - 0 instead of treasury - full) → it ALLOWS a 2nd '
  + 'withdrawal Haskell BLOCKS = treasury OVER-DISBURSEMENT.\n\n'
  + 'THE REWORK (byte-exact, exactly this):\n'
  + '  1. Before the ratification loop over proposals (where epochs.treasury.0 is first read for the pass), introduce a transient '
  + 'cap-basis: `let mut cap_treasury = epochs.treasury.0;` (= Haskell ensTreasury at pass start). Find the loop in '
  + 'ratify_proposals_impl that contains the cap check at ~:2739.\n'
  + '  2. At ~:2739 change the cap basis to the transient: `let remaining_treasury = cap_treasury;` (instead of epochs.treasury.0).\n'
  + '  3. After a TreasuryWithdrawals action is ENACTED (the spot where fix v1 deleted the old accumulator increment, right after '
  + 'enact_gov_action_impl at ~:2760), decrement cap_treasury by the FULL fold of the declared withdrawals (NOT disbursed): '
  + '`if let GovAction::TreasuryWithdrawals { withdrawals, .. } = action { cap_treasury = cap_treasury.saturating_sub('
  + 'withdrawals.values().fold(0u64, |acc, a| acc.saturating_add(a.0))); }`. This mirrors Haskell ensTreasury <-> fold wdrls.\n'
  + '  4. KEEP enact_gov_action_impl :2288 EXACTLY as-is — epochs.treasury.0 stays decremented by `disbursed` (the real money / '
  + 'reserves-treasury byte-exactness validated to mainnet ep247 / preprod ep293 must be UNTOUCHED). cap_treasury is ONLY for '
  + 'the cap check, never for the real treasury.\n'
  + 'Net model: cap_treasury = (pass-start treasury) - sum(FULL folds of enacted withdrawals) == Haskell ensTreasury; '
  + 'epochs.treasury.0 = (pass-start treasury) - sum(disbursed) == Haskell casTreasury after applyEnactedWithdrawals. For the '
  + 'ALL-REGISTERED case disbursed==full so cap_treasury==epochs.treasury.0 and behavior is identical to fix v1 (good).\n\n'
  + 'TESTS:\n'
  + '  (a) Keep test_two_treasury_withdrawals_both_enact_in_one_pass GREEN (two 400M to REGISTERED accts, treasury=1000M: '
  + 'cap_treasury 1000→600→200, both fit, both enact, epochs.treasury ends 200M). Confirm it still passes.\n'
  + '  (b) ADD a new test for the unregistered-target edge that fix v1 GOT WRONG. Scenario in ONE ratification pass: treasury='
  + '1000M; withdrawal A = 600M whose target reward account is NOT registered in certs.reward_accounts (so enact disburses 0, '
  + 'epochs.treasury.0 stays 1000M, but cap_treasury must drop by the FULL 600M → 400M); withdrawal B = 600M to a REGISTERED '
  + 'account; both proposals otherwise ratifiable. Assert B is BLOCKED (600M > cap_treasury 400M) — i.e. B is NOT enacted (its '
  + 'target credited 0, B not removed) — matching Haskell. (If a target that is registered-at-proposal then deregistered is hard '
  + 'to construct, test the equivalent: A targets an account simply absent from reward_accounts at enact time — the cap_treasury '
  + 'full-fold decrement is what matters.) In test_fails_pre_rework_reasoning, EMPIRICALLY confirm (temporarily revert :2739 to '
  + 'epochs.treasury.0 + remove the cap_treasury decrement) that the new test FAILS under v1 (B wrongly ENACTS because the cap '
  + 'basis stayed 1000M) and PASSES after the rework; then restore the rework.\n\n'
  + 'BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-ledger --all-targets -- -D warnings ; cargo nextest run '
  + '-p dugite-ledger. Report each pass/fail. completed=true ONLY if the rework is applied, BOTH tests are present + green, and '
  + 'fmt+clippy+nextest are green. Green tests are NOT byte-exact proof — a re-gauntlet follows. Do NOT commit.',
  { label: 'rework:29-cap_treasury', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
