export const meta = {
  name: 'analyze-credential-ord',
  description: 'Map every byte-exact-affecting Credential-Ord usage site (Haskell vs dugite HEAD) to decide #26 fix level',
  phases: [
    { title: 'Research', detail: 'Haskell Ord + usage sites; dugite HEAD ordering; the reward/stake guard' },
    { title: 'Synthesize', detail: 'usage map + fix-level recommendation, write durable file' },
  ],
}

// #26 ANALYZING: dugite Credential = VerificationKey(0)|Script(1) => derived Ord Key<Script.
// Haskell Cardano.Ledger.Credential.Credential = ScriptHashObj|KeyHashObj => Script<Key (INVERTED).
// Decide: is the fix a Credential-level Ord change (broad/risky) or per-consumer ordering fixes
// (#26 Voter/votes, #27 RewardAccount/withdrawals, ...)? GUARD: reward/stake byte-exactness
// (preprod ep293 / mainnet ep247) is VALIDATED — the fix must NOT regress it.

const REFS = '.claude/skills/haskell-ledger-cross-validation/references/era-rules'

const RESEARCH = [
  {
    key: 'haskell-ord-and-sites',
    prompt:
      `You are mapping the canonical Haskell (IntersectMBO/cardano-ledger + plutus) ordering semantics for credentials and credential-keyed collections. Read ${REFS}/{INDEX,shelley-core,shelley-rewards,shelley-certs,conway}.md FIRST, then consult cardano-haskell-oracle / fetch cardano-ledger source for exact constructor order + derived Ord of: Cardano.Ledger.Credential.Credential (ScriptHashObj vs KeyHashObj order), StakeCredential, Cardano.Ledger.Address.RewardAccount, Cardano.Ledger.Conway.Governance.Procedures.Voter, DRep, Committee credential. THEN enumerate EVERY place where a collection ordered by one of these (a Map/Set whose toList/CBOR-encoding order is observable) feeds a BYTE-EXACT output: (1) ScriptContext / TxInfo maps + lists (txInfoVotes, txInfoWdrl, txInfoSignatories, txInfoData, dRepDelegation, etc.) and the Plutus redeemer-pointer INDEX (ConwayPlutusPurpose Voting/Rewarding/Certifying via Map.toList), (2) ledger-state CBOR encodings keyed by credential (UMap/rewards/deposits, stake distribution, DRep delegation, pool params, gov state), (3) any sorted serialization. For each site: the exact Haskell Ord used and whether the credential TYPE (script vs key) actually breaks ties there (vs the 28-byte hash). Return a precise list with module/function names.`,
  },
  {
    key: 'dugite-head-ordering',
    prompt:
      `You are enumerating dugite's ACTUAL ordering at HEAD for every credential-keyed / credential-ordered structure that affects byte-exact output. Read the real code. Start: crates/dugite-primitives/src/credentials.rs (Credential enum = VerificationKey(0)|Script(1), derived Ord => Key<Script), transaction.rs (Voter Ord ~501-506, withdrawals BTreeMap<Vec<u8>,_> ~805), to_typed_hash32 (key=0x00/script=0x01 suffix). Then trace, for each of these, what ORDER dugite emits: txInfoVotes (uplc/src/script_context.rs ~849-865, populate_gov.rs ~119-132), Vote redeemer index (redeemer_resolve.rs ~318), withdrawals/txInfoWdrl (redeemer_resolve.rs ~256, populate_v3.rs ~107-124, tx_info_populate.rs ~569-583), txInfoSignatories (tx_info_populate.rs ~481-485), and the ledger-state maps keyed by credential (stake_map / rewards / deposits / DRep delegation in dugite-ledger/src/state). For EACH: is it sorted by (a) the enum-derived Credential Ord [Key<Script], (b) typed-hash32 bytes [key 0x00 < script 0x01], (c) raw 28/29-byte blob, or (d) insertion order? State which, with file:line. Flag every site whose order is INVERTED vs Haskell Script<Key.`,
  },
  {
    key: 'reward-stake-guard',
    prompt:
      `CRITICAL GUARD ANALYSIS. dugite's reward + stake-distribution byte-exactness is VALIDATED against Koios (preprod ep293, mainnet ep247) and must NOT regress. Yet dugite's Credential Ord is Key<Script while Haskell's is Script<Key. Resolve the apparent contradiction: WHY do the validated reward/stake outputs survive the inversion? Read dugite-ledger/src/{rewards.rs,epoch.rs,ledger_seq.rs,state/mod.rs} + ${REFS}/shelley-rewards.md. Determine: (1) Are the stake-distribution / reward / deposit maps that feed Koios-validated totals keyed by Credential-Ord, by typed-hash32, or by something whose ITERATION ORDER does not affect the totals (e.g. sums are order-independent)? (2) Does Haskell's reward/stake CBOR or fold order actually depend on script-vs-key tie-breaking, or only on the 28-byte hash (so type-ties effectively never change a total)? (3) Therefore: would flipping dugite's Credential enum Ord (or its Ord impl) to Script<Key REGRESS any Koios-validated reward/stake output, or is the inversion only observable in the phase-2 ScriptContext/redeemer-index paths (#26/#27)? Give a concrete verdict with evidence.`,
  },
]

phase('Research')
const research = await parallel(
  RESEARCH.map((r) => () => agent(r.prompt, { label: `research:${r.key}`, phase: 'Research', model: 'opus' }))
)

const RECO_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['fix_level', 'rationale', 'regression_risk', 'affected_sites', 'confidence'],
  properties: {
    fix_level: { type: 'string', enum: ['credential-ord', 'per-consumer', 'mixed', 'inconclusive'] },
    rationale: { type: 'string' },
    regression_risk: { type: 'string', description: 'whether a Credential-Ord flip would regress the validated reward/stake byte-exactness, with the decisive reason' },
    affected_sites: { type: 'array', items: { type: 'string' }, description: 'file:line sites whose order is inverted vs Haskell and must change' },
    confidence: { type: 'number' },
  },
}

phase('Synthesize')
const payload = research.filter(Boolean).map((r, i) => `### ${RESEARCH[i].key}\n${r}`).join('\n\n')
const reco = await agent(
  `You have three research reports on dugite's Credential-Ord inversion (backlog #26). Produce the decision artifact.\n\n`
  + `1. Run: mkdir -p scripts/prod-readiness/.audit\n`
  + `2. Write scripts/prod-readiness/.audit/credential-ord-analysis.md containing: (a) the Haskell-vs-dugite ordering table per site (Haskell Ord, dugite ordering, inverted? Y/N, byte-exact-observable? Y/N), (b) the reward/stake GUARD verdict (does a Credential-Ord flip regress the Koios-validated reward/stake outputs?), (c) the recommended fix level + exact sites + how_to_confirm (the byte-exact ScriptContext dump-diff / replay each fix must pass).\n`
  + `3. Return the StructuredOutput: fix_level (credential-ord = one Ord change is safe+sufficient; per-consumer = fix each phase-2 site individually because a global flip would regress reward/stake; mixed; inconclusive), rationale, regression_risk, affected_sites (file:line), confidence.\n\n`
  + `Be decisive and quote the load-bearing Haskell + dugite facts. The reward/stake regression guard is the crux.\n\nRESEARCH:\n${payload}`,
  { label: 'synthesize:reco', phase: 'Synthesize', schema: RECO_SCHEMA, model: 'opus' }
)

return { reco, research_len: research.map((r) => (r ? r.length : 0)) }
