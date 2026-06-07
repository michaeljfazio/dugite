export const meta = {
  name: 'fix-credential-ord-rework',
  description: 'Surgical rework of #26/#27: V1/V2 txInfoWdrl must be Plutus Key<Script; keep V3+redeemer-index at ledger Script<Key',
  phases: [{ title: 'Fix', detail: 'confirm V1/V2-vs-V3 ordering vs cardano-ledger source, then surgical revert + tests' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['source_confirmed', 'source_quote', 'files_changed', 'diff_summary', 'tests_updated', 'checks', 'caveats', 'completed'],
  properties: {
    source_confirmed: { type: 'boolean', description: 'true ONLY if cardano-ledger source confirms V1/V2 txInfoWdrl = Plutus Key<Script AND V3 = ledger Script<Key' },
    source_quote: { type: 'string', description: 'verbatim/paraphrased canonical Haskell source establishing the V1/V2 vs V3 ordering, with module names' },
    files_changed: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    tests_updated: { type: 'string', description: 'which tests were added/flipped (esp. the now-wrong V1/V2 script-first assertion)' },
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
  `Surgical REWORK of the dugite #26/#27 Credential-Ord fix (currently UNCOMMITTED in the MAIN working tree). A refutation `
  + `gauntlet found that the fix WRONGLY forces ledger Script<Key onto the V1/V2 txInfoWdrl FIELD. Work IN THE MAIN TREE; do NOT `
  + `commit.\n\n`
  + `STEP 1 — CONFIRM against canonical cardano-ledger + plutus source (this is a hard gate; quote the source). Establish:\n`
  + `  (A) PlutusV1 AND V2 txInfoWdrl are built by cardano-ledger Alonzo/Plutus/TxInfo.hs \`transWithdrawals\`, which FOLDS the `
  + `ledger withdrawals into a FRESH Plutus Data.Map StakingCredential Integer and then \`Map.toList\` — i.e. RE-SORTED by the `
  + `PLUTUS Credential Ord. Plutus Credential = PubKeyCredential | ScriptCredential ⇒ Key < Script. (Babbage/TxInfo.hs reuses `
  + `Alonzo.transTxBodyWithdrawals for both V1 and V2.) So V1/V2 txInfoWdrl tie-break is KEY < SCRIPT.\n`
  + `  (B) PlutusV3 txInfoWdrl is built by Conway/TxInfo.hs \`transTxBodyWithdrawals = transMap ... (unWithdrawals ...)\` which `
  + `preserves the LEDGER Map RewardAccount order (RewardAccount Ord ⇒ Credential Script<Key), unsafeFromList does NOT re-sort. `
  + `So V3 txInfoWdrl tie-break is SCRIPT < KEY.\n`
  + `  (C) The Reward/Vote redeemer-pointer INDEX uses the LEDGER Set.elemAt / redeemerPointerInverse order (Script<Key), `
  + `version-independent.\n`
  + `Use WebFetch on the IntersectMBO/cardano-ledger + IntersectMBO/plutus repos (and the in-project refs under `
  + `.claude/skills/haskell-ledger-cross-validation/references/era-rules/) to verify. Set source_confirmed=true ONLY if the `
  + `source genuinely confirms (A)+(B). If the source CONTRADICTS this (e.g. V1/V2 is actually Script<Key), set `
  + `source_confirmed=false, change NO code, and explain — do not guess.\n\n`
  + `STEP 2 — ONLY IF source_confirmed: apply the SURGICAL correction in crates/dugite-uplc/src/tx_info_populate.rs:\n`
  + `  - Make \`withdrawals_to_plutus\` (the V1/V2 txInfoWdrl builder, called at populate_v1_v2.rs:64 and :114) emit entries in `
  + `PLUTUS order Key<Script. The cleanest correct way: order by dugite's DERIVED Credential Ord (VerificationKey<Script, then `
  + `28-byte hash) — which equals the pre-fix blob-BTreeMap order for a single-network tx and equals the Plutus Credential Ord. `
  + `Do this by parsing each reward-account blob to its stake Credential (reuse the existing parse), pairing with the amount, and `
  + `sorting by the PrimCred's DERIVED Ord (\`a.0.cmp(&b.0)\`), NOT cmp_ledger. (Equivalently, restore the pre-fix per-entry `
  + `blob-order body of withdrawals_to_plutus — whichever is minimal and clearly correct.)\n`
  + `  - KEEP \`ledger_ordered_withdrawals\` (Script<Key, cmp_ledger) EXACTLY as is — it is correct and is used by populate_v3.rs `
  + `(V3 txInfoWdrl) and redeemer_resolve.rs (the Reward redeemer-pointer index). Do NOT change populate_v3.rs, populate_gov.rs, `
  + `redeemer_resolve.rs, credentials.rs, or transaction.rs — the Voter::cmp_ledger, V3, votes, and redeemer-index changes are `
  + `CORRECT and must stay.\n\n`
  + `STEP 3 — TESTS: the earlier fix added a withdrawals_to_plutus test asserting the SCRIPT account comes first for V1/V2 — that `
  + `assertion is now WRONG; FLIP it to assert the KEY account comes first (Plutus Key<Script). Add/keep a CONTRAST test pair: a `
  + `mixed key+script withdrawal set yields KEY-first via withdrawals_to_plutus (V1/V2) but SCRIPT-first via ledger_ordered_`
  + `withdrawals (V3) and the Reward redeemer-index resolves over the SCRIPT-first ledger order. Keep ledger_ordered_withdrawals_`
  + `puts_script_before_key green (it tests the V3/ledger helper).\n\n`
  + `STEP 4 — BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-primitives -p dugite-uplc --all-targets -- -D warnings ; `
  + `cargo nextest run -p dugite-primitives -p dugite-uplc. Report each pass/fail.\n\n`
  + `Return the StructuredOutput. completed=true ONLY if source_confirmed AND the surgical change is applied AND tests flipped/added `
  + `AND fmt+clippy+nextest green. Do NOT commit.`,
  { label: 'rework:v1v2-wdrl', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
