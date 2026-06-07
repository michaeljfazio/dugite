export const meta = {
  name: 'gauntlet-29-treasury-withdrawals',
  description: 'Refutation panel for the #29 TreasuryWithdrawals double-subtract fix before commit (Haskell-match, no-regression, disbursed-vs-full-sum residual)',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #29 fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/incomplete OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #29). Read the ACTUAL current code: git diff on '
  + 'crates/dugite-ledger/src/state/governance.rs.\n\n'
  + 'WHAT IT DOES: removes a DOUBLE-SUBTRACTION of prior treasury withdrawals in ratify_proposals_impl. Before, the cap basis '
  + 'was remaining_treasury = epochs.treasury.0.saturating_sub(enacted_withdrawals_total) (an accumulator) AND enact_gov_action_'
  + 'impl ALSO physically decremented epochs.treasury.0 by the disbursed amount — so the 2nd+ TreasuryWithdrawals in one epoch '
  + 'saw treasury - 2*w1. The fix: (1) cap basis is now "let remaining_treasury = epochs.treasury.0;" (the live, already-per-'
  + 'enact-decremented treasury); (2) the accumulator decl + its increment block were DELETED; (3) the physical decrement in '
  + 'enact_gov_action_impl (epochs.treasury.0 -= disbursed at ~:2288) + the reward-account payout were left UNCHANGED.\n\n'
  + 'CANONICAL HASKELL (source-confirmed conf 0.96; attack if you can): Conway Ratify.hs withdrawalCanWithdraw checks '
  + 'fold(wdrls) <= ensTreasury against the CURRENT ensTreasury (NO separate accumulator); Enact.hs enactmentTransition '
  + 'decrements ensTreasury <-> wdrlsAmount per-enact and unions ensWithdrawals; ratifyTransition threads st\' so the next '
  + 'iteration sees the decremented ensTreasury. The withdrawn funds move to reward accounts at the epoch boundary '
  + '(applyEnactedWithdrawals; unregistered targets skipped, post-bootstrap rejected at proposal via tag-17 '
  + 'TreasuryWithdrawalReturnAccountsDoNotExist).\n\n'
  + 'GREEN STATUS (engine-verified independently): accumulator removed (only survives in a test comment), cap-check uses live '
  + 'epochs.treasury.0, the :2288 physical decrement is unchanged; fmt+clippy+nextest 1524/1524 pass. New test '
  + 'test_two_treasury_withdrawals_both_enact_in_one_pass (treasury=1000M, two 400M to distinct REGISTERED accts → both enact, '
  + 'treasury=200M) — EMPIRICALLY confirmed to FAIL with the old accumulator present (2nd target credited 0) and pass after the '
  + 'fix. The cap-preservation test test_treasury_aggregate_withdrawal_cap (2nd over-cap blocked) still passes.'

const LENSES = [
  {
    key: 'haskell-single-subtraction-match',
    prompt: 'LENS: exact Conway RATIFY/ENACT match. Read ratify_proposals_impl + enact_gov_action_impl. Does the fix model match '
      + 'Haskell BYTE-FOR-BYTE? (a) Is the per-pass treasury threading correct — does each subsequent withdrawal\'s cap check see '
      + 'the treasury AFTER prior enacts in the SAME pass (i.e. enact mutates epochs.treasury.0 BEFORE the next iteration\'s :2739 '
      + 'read)? Confirm enact happens before the loop advances. (b) Is the ORDER of (check ratification) vs (enact) correct — '
      + 'dugite checks then enacts; Haskell\'s withdrawalCanWithdraw is part of the acceptance predicate BEFORE ENACT runs. (c) '
      + 'Does removing the accumulator leave the SINGLE subtraction (the :2288 physical decrement) exactly mirroring '
      + 'ensTreasury <-> wdrlsAmount? (d) Any interaction with the ratification ORDER (actionPriority, rsDelayed, the recursive '
      + 'committee/pparam threading) that the accumulator removal disturbs? If the threaded-treasury model deviates from '
      + 'Ratify.hs/Enact.hs, refuted=true.',
  },
  {
    key: 'no-regression',
    prompt: 'LENS: no regression. (a) Single-withdrawal-per-epoch: with one withdrawal the old accumulator was 0 at the only cap '
      + 'check, so old remaining_treasury == treasury == new remaining_treasury — confirm byte-identical (no change). (b) The '
      + 'byte-exact reserves/treasury totals validated to mainnet ep247 / preprod ep293: confirm the fix cannot perturb them (the '
      + 'ADA-moving :2288 leg is unchanged; only the CAP-CHECK basis changed, which only ever UN-blocks a previously-wrongly-'
      + 'rejected 2nd+ withdrawal — and those validated ranges had <=1 ratified TreasuryWithdrawal per epoch). (c) The genuine cap '
      + 'is still enforced (an over-budget aggregate is still rejected — test_treasury_aggregate_withdrawal_cap). (d) Does any '
      + 'OTHER code path read enacted_withdrawals_total (now deleted)? Grep to be sure nothing else breaks. If any regression, '
      + 'refuted=true.',
  },
  {
    key: 'disbursed-vs-fullsum-residual',
    prompt: 'LENS: the disbursed-vs-full-sum cap-basis residual. The diagnose flagged that dugite\'s physical :2288 decrement uses '
      + 'disbursed (the REGISTERED-account total) whereas Haskell decrements the transient cap-basis ensTreasury by the FULL '
      + 'fold(wdrls) (incl. unregistered targets), while moving only registered funds at the boundary. So when a TreasuryWithdrawals '
      + 'has UNREGISTERED targets, dugite\'s post-enact cap basis (treasury - disbursed) is LARGER than Haskell\'s (treasury - full '
      + 'sum) → dugite could allow a subsequent withdrawal Haskell blocks. Assess: (1) Is this a REAL remaining divergence after '
      + '#29\'s fix? (2) Can it actually occur on a real chain — does dugite (and Haskell) REJECT TreasuryWithdrawals to unregistered '
      + 'accounts at proposal/enact time (Conway tag-17 TreasuryWithdrawalReturnAccountsDoNotExist, post-bootstrap)? If so it is an '
      + 'edge case (pre-bootstrap early-Conway, or deregistration between propose and enact). (3) Does this residual BLOCK committing '
      + 'the #29 primary fix (the double-subtract removal), or is #29 commit-safe with the residual tracked as a separate item '
      + '(#29b)? Refuted=true ONLY if the residual makes the #29 fix WRONG or commit-unsafe; if it is a correctly-scoped separate '
      + 'follow-up, NOT refuted — but state explicitly whether #29b should be filed.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the #29 fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
