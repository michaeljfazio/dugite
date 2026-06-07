export const meta = {
  name: 'fix-credential-ord',
  description: 'FIXING #26+#27: per-consumer ledger-ordered (Script<Key) comparator at the phase-2 ScriptContext/redeemer sites',
  phases: [{ title: 'Fix', detail: 'implement cmp_ledger + apply at phase-2 sites; targeted build' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['files_changed', 'diff_summary', 'tier', 'haskell_quote', 'checks', 'caveats', 'completed'],
  properties: {
    files_changed: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    tier: { type: 'string', enum: ['A', 'Aprime', 'B'] },
    haskell_quote: { type: 'string', description: 'the canonical Haskell Ord being matched (Voter order + Credential Script<Key + RewardAccount Ord)' },
    checks: {
      type: 'object',
      additionalProperties: false,
      required: ['fmt', 'check', 'nextest'],
      properties: {
        fmt: { type: 'boolean' },
        check: { type: 'boolean' },
        nextest: { type: 'boolean' },
      },
    },
    caveats: { type: 'string', description: 'any site not cleanly fixed, any assumption, or "none"' },
    completed: { type: 'boolean', description: 'true only if all intended sites changed + targeted checks green' },
  },
}

phase('Fix')
const fix = await agent(
  `Implement the byte-exact per-consumer fix for dugite backlog #26 + #27 (Credential-Ord inversion) IN THE MAIN WORKING TREE `
  + `(do NOT create a worktree; do NOT git commit anything — leave the changes uncommitted for a separate verify gate).\n\n`
  + `READ FIRST: scripts/prod-readiness/.audit/credential-ord-analysis.md (the decided plan), then the cited HEAD files.\n\n`
  + `BACKGROUND (already analyzed, conf 0.86 — do NOT re-derive, just apply): dugite's dugite_primitives::Credential derives Ord `
  + `as VerificationKey(0)<Script(1) => Key<Script. The Haskell LEDGER Credential is ScriptHashObj<KeyHashObj => Script<Key. `
  + `This shared enum's Key<Script Ord is CORRECT for its Plutus-Data-tag + canonical-CBOR-key roles and MUST NOT be flipped. `
  + `The ledger order (Script<Key) is needed ONLY at the phase-2 ScriptContext / redeemer-index construction sites. The reward/`
  + `stake conservation pipeline NEVER touches Credential Ord (credentials are erased to typed-Hash32 at ingest), so this change `
  + `MUST NOT touch crates/dugite-ledger reward/stake/epoch code.\n\n`
  + `EXACT CHANGES (apply all; keep them minimal + idiomatic to surrounding code):\n`
  + `1. crates/dugite-primitives/src/credentials.rs: ADD an inherent method \`pub fn cmp_ledger(&self, other: &Self) -> core::cmp::Ordering\` `
  + `that orders Script BEFORE VerificationKey (ledger ScriptHashObj<KeyHashObj), tie-broken by the 28-byte hash bytes ascending. `
  + `Keep the derived Ord (Key<Script) UNCHANGED. Augment the comment on test_credential_ord_key_before_script (:142-148) to note `
  + `this enum Ord is Plutus/CBOR order and is the OPPOSITE of the ledger Credential Ord (use cmp_ledger where ledger semantics apply).\n`
  + `2. crates/dugite-primitives/src/transaction.rs (Voter, ~501-506): ADD \`pub fn cmp_ledger(&self,&Self)->Ordering\` for Voter that `
  + `orders by the SAME variant order it already has (ConstitutionalCommittee<DRep<StakePool — this MATCHES Haskell CommitteeVoter<DRepVoter<StakePoolVoter, `
  + `confirm in the artifact), but uses Credential::cmp_ledger for the inner credential of CC/DRep and the raw Hash32 for StakePool. Keep derived Ord unchanged.\n`
  + `3. crates/dugite-uplc: emit/index in LEDGER order at the phase-2 sites (use the new comparators; sort a Vec by cmp_ledger rather than relying on BTreeMap iteration):\n`
  + `   - populate_gov.rs (txInfoVotes build, ~:119): order the votes map entries by Voter::cmp_ledger.\n`
  + `   - redeemer_resolve.rs (Vote index, ~:318): resolve the Vote redeemer index over the Voter::cmp_ledger-ordered sequence (the ledger Set.elemAt order), NOT the dugite-Voter BTreeMap order.\n`
  + `   - txInfoWdrl: populate_v3.rs (~:107-124) V3 + tx_info_populate.rs (~:569-583) V1/V2: order withdrawals by the ledger RewardAccount Ord = (Network, Credential Script<Key, hash). Within a tx all accounts share one network, so order by the stake-credential type (Script before Key) then 28-byte hash — NOT the raw [header||hash] blob order (which is Key<Script).\n`
  + `   - redeemer_resolve.rs (Reward index, ~:256): resolve the Reward redeemer index over that ledger-ordered withdrawal sequence.\n`
  + `   NOTE: extract the stake Credential from the 29-byte reward-account blob (header high-nibble 0xE=key / 0xF=script, low-nibble=network; bytes[1..29]=hash28) to apply cmp_ledger; or reuse any existing reward-account parse helper.\n`
  + `4. DO NOT change txInfoSignatories here (that is finding #30, a missing-sort, separate item). DO NOT change crates/dugite-serialization/src/encode/governance.rs here (separate consensus-CBOR item). Scope = dugite-primitives + dugite-uplc ONLY (2 crates).\n\n`
  + `TESTS: add focused unit/proptest coverage: (a) in credentials.rs, a key-cred and a script-cred with DIFFERENT hashes => cmp_ledger puts Script FIRST (and a same-hash case => Script first); keep test_credential_ord_key_before_script GREEN (enum Ord still Key<Script). (b) in dugite-uplc, a ScriptContext built from a tx with one key-stake + one script-stake withdrawal => txInfoWdrl lists the SCRIPT account first; and the Reward redeemer index resolves to the correct account. (c) analogous votes test if feasible.\n\n`
  + `BUILD (bounded — do NOT run the full workspace): cargo fmt --all ; cargo check -p dugite-primitives -p dugite-uplc ; cargo nextest run -p dugite-primitives -p dugite-uplc. Report each as pass/fail in checks{fmt,check,nextest}.\n\n`
  + `Return the StructuredOutput. completed=true ONLY if all the intended sites are changed AND fmt+check+nextest(targeted) are green. `
  + `Remember: green targeted tests are NOT proof of byte-exactness — a separate ScriptContext dump-diff gate will follow; your job is a correct, compiling, test-covered implementation of the specified ordering. Do NOT commit.`,
  { label: 'fix:26-27', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)

return { fix }
