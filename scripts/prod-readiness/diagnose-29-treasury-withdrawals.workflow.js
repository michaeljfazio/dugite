export const meta = {
  name: 'diagnose-29-treasury-withdrawals',
  description: 'DIAGNOSE #29: confirm the Haskell Conway RATIFY/ENACT treasury model for multi TreasuryWithdrawals + the exact dugite double-subtract fix',
  phases: [{ title: 'Diagnose', detail: 'source-confirm withdrawalCanWithdraw cap basis + ensTreasury decrement model; specify fix' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['is_real_gap', 'haskell_model', 'haskell_source', 'dugite_bug', 'fix_plan', 'side_effects', 'confidence', 'caveats'],
  properties: {
    is_real_gap: { type: 'boolean' },
    haskell_model: { type: 'string', description: 'the exact Conway RATIFY/ENACT treasury accounting for TreasuryWithdrawals: is the cap check withdrawalCanWithdraw against the CURRENT (decremented-per-enact) ensTreasury with NO separate accumulator? OR against a constant treasury minus an accumulator? Does enactmentTransition decrement ensTreasury per-enact? Where do the withdrawn funds go (ensWithdrawals -> reward accounts at epoch boundary)?' },
    haskell_source: { type: 'string', description: 'canonical module/function + verbatim/paraphrased: Cardano.Ledger.Conway.Rules.Ratify withdrawalCanWithdraw + the ensTreasury threading; Cardano.Ledger.Conway.Rules.Enact enactmentTransition TreasuryWithdrawals case' },
    dugite_bug: { type: 'string', description: 'confirm the double-subtract: governance.rs:2733 remaining_treasury = treasury - enacted_withdrawals_total AND :2288 enact decrements treasury by disbursed AND :2762 accumulates — so prior withdrawals subtracted twice in the cap basis' },
    fix_plan: { type: 'string', description: 'the precise minimal change to match Haskell (e.g. cap-check against epochs.treasury.0 directly since enact already decrements it; remove the enacted_withdrawals_total accumulator) — OR the alternative if Haskell does NOT decrement per-enact (then keep the accumulator but do not also decrement). State which model dugite should adopt and exactly what to change.' },
    side_effects: { type: 'string', description: 'does dugite credit the withdrawn funds to the reward accounts (the payout leg)? does the fix interact with the order of ratification, the per-pass treasury threading, or the byte-exact reserves/treasury totals validated to ep247 mainnet / ep293 preprod? any regression risk to single-withdrawal epochs?' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  `DIAGNOSE dugite backlog #29 (a re-audit candidate, conf 0.78 — but the engine has ALREADY HEAD-VERIFIED the double-subtract `
  + `is real; your job is to source-confirm the canonical Haskell model so the fix is byte-exact, and rule out alternative `
  + `interpretations).\n\n`
  + `CONFIRMED DUGITE BEHAVIOR (read it yourself to verify): crates/dugite-ledger/src/state/governance.rs ratify_proposals_impl `
  + `at ~:2733 computes remaining_treasury = epochs.treasury.0.saturating_sub(enacted_withdrawals_total) and passes it to `
  + `check_ratification_impl (the cap check); at ~:2760 calls enact_gov_action_impl which (the TreasuryWithdrawals case ~:2288) `
  + `does epochs.treasury.0 = epochs.treasury.0.saturating_sub(disbursed); at ~:2762 does enacted_withdrawals_total += `
  + `sum(withdrawals). So for the 2nd withdrawal in one epoch, the cap basis = (treasury already decremented by w1) minus `
  + `(accumulator including w1) = treasury - 2*w1 → w1 subtracted TWICE → the 2nd+ withdrawal is wrongly blocked when w1+w2 <= `
  + `treasury but w2 > treasury - 2*w1.\n\n`
  + `THE CRUX — source-confirm the canonical Haskell Conway treasury model (do NOT guess): Read IntersectMBO/cardano-ledger `
  + `Cardano.Ledger.Conway.Rules.Ratify (withdrawalCanWithdraw + how the treasury it checks against is threaded across the `
  + `ratification pass) AND Cardano.Ledger.Conway.Rules.Enact (enactmentTransition, the TreasuryWithdrawals case — does it `
  + `decrement ensTreasury and/or accumulate ensWithdrawals?). Use the in-project refs `
  + `.claude/skills/haskell-ledger-cross-validation/references/era-rules/{conway,conway-gov-rules}.md FIRST, then `
  + `cardano-haskell-oracle / WebFetch the cardano-ledger source. Decisively answer: (1) Is the cap check `
  + `withdrawalCanWithdraw against the CURRENT ensTreasury (which IS decremented per-enact during the pass) with NO separate `
  + `accumulator subtraction? (2) Does enactmentTransition decrement ensTreasury per-enact (so subsequent checks see the smaller `
  + `treasury)? (3) Therefore which of dugite's two subtractions is the REDUNDANT one — the physical :2288 decrement, or the `
  + `:2733 accumulator subtraction — and what is the byte-exact-correct single-subtraction model?\n\n`
  + `Then give the minimal fix (which line to change so prior withdrawals are subtracted EXACTLY ONCE), the side-effects (the `
  + `payout leg to reward accounts; any threading/ordering interaction; regression risk to single-withdrawal epochs and to the `
  + `byte-exact reserves/treasury totals validated to mainnet ep247 / preprod ep293), confidence, and how_to_confirm (the unit `
  + `test from the backlog: treasury=1000M, two 400M withdrawals both ratified → both must enact, treasury=200M). Return the `
  + `StructuredOutput.`,
  { label: 'diagnose:29', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
