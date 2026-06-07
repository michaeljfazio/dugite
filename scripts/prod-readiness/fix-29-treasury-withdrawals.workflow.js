export const meta = {
  name: 'fix-29-treasury-withdrawals',
  description: 'FIXING #29: remove the redundant TreasuryWithdrawals cap-check accumulator (double-subtract); cap-check against the live per-enact-decremented treasury',
  phases: [{ title: 'Fix', detail: 'delete accumulator + cap-check vs live treasury; reproducing unit test' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['files_changed', 'diff_summary', 'test_added', 'test_fails_pre_fix_reasoning', 'checks', 'caveats', 'completed'],
  properties: {
    files_changed: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    test_added: { type: 'string', description: 'name + what it asserts (two TreasuryWithdrawals in one epoch both enact; treasury ends correctly)' },
    test_fails_pre_fix_reasoning: { type: 'string', description: 'concrete reasoning that this test would FAIL with the accumulator present (the 2nd withdrawal blocked by the double-subtract) and passes only after the accumulator is removed' },
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
  `Implement dugite backlog #29 in the MAIN working tree (do NOT create a worktree; do NOT git commit). Single crate: `
  + `dugite-ledger.\n\n`
  + `BUG (source-confirmed conf 0.96 — see scripts/prod-readiness/engine-state.md #29 entry): in crates/dugite-ledger/src/state/`
  + `governance.rs ratify_proposals_impl, the TreasuryWithdrawals cap basis SUBTRACTS PRIOR WITHDRAWALS TWICE. enact_gov_action_`
  + `impl (~:2288) already physically decrements epochs.treasury.0 by the disbursed amount per enacted withdrawal (the CORRECT `
  + `single subtraction, mirroring Haskell Conway Enact.hs ensTreasury <-> wdrlsAmount). But the ratify loop ALSO maintains an `
  + `accumulator enacted_withdrawals_total (decl ~:2702, += ~:2761-2765) and computes the cap basis as remaining_treasury = `
  + `epochs.treasury.0.saturating_sub(enacted_withdrawals_total) (~:2733) — so the 2nd+ withdrawal in one epoch sees treasury - `
  + `2*w1. Haskell (Conway Ratify.hs withdrawalCanWithdraw) checks fold(wdrls) <= ensTreasury against the CURRENT, per-enact-`
  + `decremented ensTreasury with NO separate accumulator.\n\n`
  + `THE FIX (minimal, exactly this — do NOT change the physical :2288 decrement or the payout leg):\n`
  + `  1. Line ~:2733: change the cap basis to the LIVE treasury directly — replace `
  + `\`let remaining_treasury = epochs.treasury.0.saturating_sub(enacted_withdrawals_total);\` with `
  + `\`let remaining_treasury = epochs.treasury.0;\` (enact at :2288 already decremented it for any earlier withdrawal in this `
  + `pass, exactly mirroring Haskell reading the threaded decremented ensTreasury).\n`
  + `  2. DELETE the now-unused accumulator: the declaration \`let mut enacted_withdrawals_total: u64 = 0;\` (~:2702) and the `
  + `entire increment block \`if let GovAction::TreasuryWithdrawals { withdrawals, .. } = action { enacted_withdrawals_total += `
  + `... }\` (~:2761-2765). Make sure nothing else references enacted_withdrawals_total (grep first).\n`
  + `  3. Keep enact_gov_action_impl (~:2266-2288) EXACTLY as-is (the payout to reward accounts + the treasury decrement are `
  + `correct).\n\n`
  + `TEST (reproducing, Tier-A): add a unit test in governance.rs (or the ledger tests) that exercises TWO TreasuryWithdrawals `
  + `enacting in ONE ratification pass and proves both enact. Find an EXISTING governance/ratification test in this crate to use `
  + `as a template for constructing the EpochState/CertState/GovState + a ratifiable proposal (votes/committee/stake that pass `
  + `check_ratification_impl). Scenario: treasury = 1000M lovelace, two TreasuryWithdrawals proposals each 400M to DISTINCT `
  + `REGISTERED reward accounts, both with passing DRep+CC votes. Run the ratify/epoch-transition entry point. Assert: BOTH `
  + `reward accounts are credited 400M AND epochs.treasury ends at 200M (1000M - 400M - 400M). If a full ratify-pass test is `
  + `infeasible to set up cleanly, instead write the tightest test that exercises the cap basis across two withdrawals in one `
  + `pass and would be BLOCKED by the old accumulator (e.g. drive ratify_proposals_impl directly with two pre-ratified `
  + `withdrawals). In test_fails_pre_fix_reasoning, explain concretely why the test would fail with the accumulator present.\n\n`
  + `BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-ledger --all-targets -- -D warnings ; cargo nextest run `
  + `-p dugite-ledger. Report each pass/fail. Remember: green tests are NOT byte-exact proof — a gauntlet follows; your job is `
  + `the correct minimal fix + a test that genuinely exercises the multi-withdrawal path. completed=true ONLY if the 3 edits are `
  + `applied, the test added, and fmt+clippy+nextest green. Do NOT commit.`,
  { label: 'fix:29', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
