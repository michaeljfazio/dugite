export const meta = {
  name: 'gauntlet-credential-ord-v2',
  description: 'Re-run Tier-A\' refutation panel on the REWORKED #26/#27 fix (V1/V2 Plutus Key<Script; V3+index ledger Script<Key)',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the corrected version-split ordering' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the CORRECTED fix is shown wrong/incomplete via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  `Fix under test (UNCOMMITTED in the main tree; backlog #26 + #27, after a surgical REWORK). Read the ACTUAL current code: `
  + `git diff on crates/dugite-primitives/src/{credentials.rs,transaction.rs} + crates/dugite-uplc/src/{populate_gov.rs,`
  + `populate_v3.rs,populate_v1_v2.rs,redeemer_resolve.rs,tx_info_populate.rs}. A PRIOR gauntlet refuted an earlier version that `
  + `wrongly forced ledger Script<Key onto the V1/V2 txInfoWdrl field; this REWORK split the ordering by Plutus version. Verify `
  + `the rework is now correct.\n\n`
  + `WHAT THE CORRECTED FIX DOES:\n`
  + `- Credential enum derived Ord = Key<Script (UNCHANGED — correct for Plutus-Data-tag + canonical-CBOR-key roles). Added `
  + `Credential::cmp_ledger (Script<Key) + Voter::cmp_ledger (variant rank CC<DRep<SPO, inner cred via cmp_ledger).\n`
  + `- V1/V2 txInfoWdrl: tx_info_populate.rs withdrawals_to_plutus parses each reward-account blob to its stake credential and `
  + `sorts by the DERIVED Credential Ord (a.0.cmp(&b.0)) = PLUTUS Key<Script. Used by populate_v1_v2.rs:64 (V1) + :114 (V2).\n`
  + `- V3 txInfoWdrl: populate_v3.rs uses ledger_ordered_withdrawals = cmp_ledger = ledger Script<Key.\n`
  + `- Reward redeemer-pointer index (redeemer_resolve.rs resolve_reward) resolves over ledger_ordered_withdrawals (ledger Script<Key).\n`
  + `- txInfoVotes (V3/Conway) ordered by Voter::cmp_ledger (ledger Script<Key inner); Vote redeemer index over the same.\n`
  + `- The reward/stake conservation pipeline is untouched (credentials erased to typed-Hash32 at ingest).\n\n`
  + `CANONICAL HASKELL (source-confirmed; attack if you can): V1 Alonzo.Plutus.TxInfo transWithdrawals folds withdrawals into a `
  + `FRESH Plutus Data.Map StakingCredential then Map.toList => Plutus Credential Ord (PubKeyCredential=0 < ScriptCredential=1 => `
  + `Key<Script); V2 (Babbage) = unsafeFromList of that same Plutus-ordered list. V3 (Conway) transTxBodyWithdrawals = transMap `
  + `over the LEDGER Map RewardAccount (Credential ScriptHashObj < KeyHashObj => Script<Key), unsafeFromList no re-sort. The `
  + `Reward/Vote redeemer-pointer index = ledger Set.elemAt order (Script<Key), version-independent.\n\n`
  + `GREEN STATUS (independently re-verified by the engine): fmt + clippy(-D warnings) + nextest 732/732 pass, including `
  + `withdrawals_to_plutus_v1v2_puts_key_before_script_contrast_v3, ledger_ordered_withdrawals_puts_script_before_key, `
  + `reward_redeemer_index_resolves_over_ledger_script_first_order.`

const LENSES = [
  {
    key: 'v1v2-plutus-order',
    prompt: `LENS: V1/V2 txInfoWdrl ordering correctness. Read withdrawals_to_plutus (tx_info_populate.rs) and its V1/V2 callers `
      + `(populate_v1_v2.rs:64,114). Does it NOW emit txInfoWdrl in PLUTUS Key<Script order, matching cardano-ledger Alonzo `
      + `transWithdrawals -> Map.toList by the Plutus Credential Ord? Verify the derived PrimCred Ord (a.0.cmp(&b.0)) genuinely `
      + `equals the Plutus StakingCredential Ord for ALL cases (cross-type Key-before-Script AND same-type hash tie-break AND any `
      + `Pointer/edge case). If V1/V2 still uses cmp_ledger (Script<Key) anywhere, or the derived-Ord sort does NOT equal the `
      + `Plutus order, refuted=true.`,
  },
  {
    key: 'v3-and-index-ledger-order',
    prompt: `LENS: V3 txInfoWdrl + redeemer-index + votes use the LEDGER order. Read populate_v3.rs (V3 wdrl), redeemer_resolve.rs `
      + `(resolve_reward / resolve_vote), populate_gov.rs (txInfoVotes). Do they ALL use ledger Script<Key (cmp_ledger / `
      + `ledger_ordered_withdrawals / Voter::cmp_ledger), matching Conway transMap (ledger Map order) and the ledger redeemer-`
      + `pointer index? Is the redeemer-index order CONSISTENT with the txInfoWdrl field order it indexes for V3 (both ledger) and `
      + `correctly INDEPENDENT for V1/V2 (field=Plutus Key<Script, but the index space is still ledger Script<Key — confirm `
      + `cardano-ledger resolves the V1/V2 Rewarding redeemer pointer over the ledger Map, NOT the Plutus-ordered field)? If any `
      + `V3/index/votes site uses the wrong order, or the V1/V2 redeemer index was wrongly switched to Plutus order, refuted=true.`,
  },
  {
    key: 'no-common-case-regression',
    prompt: `LENS: no common-case regression + completeness. For the COMMON case (single-entry, or multi-entry of one credential `
      + `type), is each version's txInfoWdrl/txInfoVotes byte-identical to the PRE-fix output (so the byte-exact-validated history `
      + `is preserved)? Note V1/V2 must equal pre-fix blob order; V3 is an intentional CORRECTION (pre-fix V3 used blob Key<Script, `
      + `now ledger Script<Key) so V3 mixed-cred output legitimately CHANGES — confirm that change is toward the Haskell-correct `
      + `order and does not regress single/same-type V3 txs. Also: does the partial scope (TreasuryWithdrawals/UpdateCommittee maps `
      + `+ gov-CBOR still inverted, tracked as #26b; txInfoSignatories=#30) create a NEW within-ScriptContext inconsistency that is `
      + `worse than before? A pre-existing still-inverted EXCLUDED site independently tracked is NOT a refutation; only a NEW `
      + `regression or in-scope miss is. If found, refuted=true.`,
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent(`Adversarially REFUTE the CORRECTED fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n${CONTEXT}\n\n${l.prompt}`,
      { label: `refute:${l.key}`, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
