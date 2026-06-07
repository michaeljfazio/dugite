export const meta = {
  name: 'gauntlet-29-v2-captreasury',
  description: 'Re-run refutation panel on the REWORKED #29 fix (transient cap_treasury full-fold-decremented) before commit',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #29 cap_treasury rework' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the REWORKED fix is shown wrong/incomplete OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #29, after a byte-exact REWORK). Read the ACTUAL current code: '
  + 'git diff on crates/dugite-ledger/src/state/governance.rs (ratify_proposals_impl). A PRIOR gauntlet refuted v1 (which cap-'
  + 'checked against epochs.treasury.0, decremented by `disbursed` registered-only) for OVER-DISBURSING the treasury in the '
  + 'unregistered-target edge. This REWORK separates the cap basis from the real treasury.\n\n'
  + 'WHAT THE REWORKED FIX DOES: in ratify_proposals_impl, a transient `let mut cap_treasury = epochs.treasury.0;` is introduced '
  + 'at pass start (~:2715). The cap check now uses `let remaining_treasury = cap_treasury;` (~:2756), and after each enacted '
  + 'TreasuryWithdrawals, `cap_treasury = cap_treasury.saturating_sub(withdrawals.values().fold(0u64, |acc,a| acc.saturating_add'
  + '(a.0)))` (~:2791) — decrementing by the FULL declared fold (NOT disbursed). enact_gov_action_impl (~:2288) still does '
  + '`epochs.treasury.0 = epochs.treasury.0.saturating_sub(disbursed)` (registered-only) UNCHANGED. So: cap_treasury = pass-start '
  + 'treasury - sum(FULL folds of enacted withdrawals); epochs.treasury.0 = pass-start treasury - sum(disbursed).\n\n'
  + 'CANONICAL HASKELL (source-confirmed conf 0.96; attack if you can): Conway Ratify.hs withdrawalCanWithdraw checks fold(wdrls) '
  + '<= ensTreasury; Enact.hs enactmentTransition decrements ensTreasury <-> fold(wdrls) per enact (FULL, regardless of target '
  + 'registration); ratifyTransition threads st\' so the next cap check sees the decremented ensTreasury. The real treasury '
  + '(casTreasury) is reduced only by the REGISTERED funds moved at the epoch boundary (applyEnactedWithdrawals; unregistered '
  + 'targets skipped). So Haskell ensTreasury == dugite cap_treasury (full-fold) and Haskell casTreasury == dugite '
  + 'epochs.treasury.0 (disbursed).\n\n'
  + 'GREEN STATUS (engine-verified independently): cap_treasury decrement uses the FULL fold (not disbursed); :2288 absent from '
  + 'the diff (untouched); fmt+clippy+nextest 1525/1525 pass. Tests: test_two_treasury_withdrawals_both_enact_in_one_pass '
  + '(all-registered, both 400M enact, treasury=200M) PASSES; test_treasury_withdrawal_unregistered_target_still_consumes_cap_'
  + 'basis (A=600M to UNREGISTERED target → disbursed 0 but cap_treasury 1000→400; B=600M registered → BLOCKED) PASSES and was '
  + 'EMPIRICALLY confirmed to FAIL under v1 (B wrongly enacted = over-disbursement).'

const LENSES = [
  {
    key: 'ensTreasury-exact-match',
    prompt: 'LENS: cap_treasury == Haskell ensTreasury, byte-for-byte, for ALL cases. Read the rework. (a) Is cap_treasury '
      + 'initialized to the treasury at PASS START (before any enact in this pass), matching ensTreasury entering RATIFY? (b) Is '
      + 'it decremented by the FULL fold(withdrawals) per enacted TreasuryWithdrawals (matching ensTreasury <-> fold wdrls), for '
      + 'BOTH all-registered and unregistered-target cases? (c) Is the threading correct — does cap_treasury persist + carry the '
      + 'decrement across loop iterations so the NEXT proposal\'s cap check sees it (not re-initialized per iteration)? (d) Is the '
      + 'cap-check comparison still total > remaining_treasury (i.e. fold(wdrls) <= cap_treasury), matching withdrawalCanWithdraw? '
      + '(e) Does cap_treasury correctly decrement ONLY for TreasuryWithdrawals (not other gov actions)? If cap_treasury diverges '
      + 'from Haskell ensTreasury in ANY case, refuted=true.',
  },
  {
    key: 'casTreasury-and-no-regression',
    prompt: 'LENS: real-treasury (casTreasury) correctness + no regression. (a) Is epochs.treasury.0 STILL the real treasury, '
      + 'decremented only by `disbursed` (registered) at :2288, UNCHANGED by the rework (cap_treasury must NOT leak into it)? '
      + 'Confirm cap_treasury is a pass-local var that never writes back to epochs.treasury.0 or any persisted ledger state. (b) '
      + 'Reserves/treasury byte-exactness validated to mainnet ep247 / preprod ep293: confirm the rework cannot perturb the real '
      + 'treasury totals (the ADA-moving :2288 leg + payout unchanged; cap_treasury feeds only the cap CHECK). (c) All-registered '
      + 'multi-withdrawal: disbursed==full ⇒ cap_treasury==epochs.treasury.0 ⇒ identical to v1/pre-bug correct behavior. (d) '
      + 'Single-withdrawal-per-epoch: byte-identical (cap_treasury==treasury at the only check). If any real-treasury regression '
      + 'or cap_treasury leak, refuted=true.',
  },
  {
    key: 'completeness-edges',
    prompt: 'LENS: completeness + edge cases of the rework. (a) saturating_sub edges: a withdrawal sum > cap_treasury (cap_treasury '
      + 'floors at 0) — does the cap check still correctly REJECT it (total > 0)? Does fold overflow saturate safely? (b) Is '
      + 'cap_treasury correctly SCOPED to one ratification pass / one epoch — it must NOT persist across epochs or leak into a '
      + 'later pass (re-initialized each call). (c) Does the unregistered-target test actually model Haskell — Haskell decrements '
      + 'ensTreasury by the full fold even for unregistered targets, and dugite now does too; confirm the test asserts the '
      + 'Haskell-correct BLOCK. (d) Any OTHER gov-action interaction (committee/pparam threading, rsDelayed, actionPriority order) '
      + 'that the cap_treasury introduction disturbs? (e) Is there a remaining divergence class the rework still misses (e.g. the '
      + 'ORDER of withdrawals within a pass affecting which gets blocked, vs Haskell\'s deterministic Map/priority order)? If the '
      + 'rework is incomplete or has a wrong edge, refuted=true.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the REWORKED #29 fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
