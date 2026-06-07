export const meta = {
  name: 'gauntlet-credential-ord',
  description: 'Tier-A\' refutation panel for the #26/#27 Credential-Ord per-consumer fix before commit',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default adversarial panel, phase-2 lenses' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/incomplete via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  `Fix under test (UNCOMMITTED in the main tree; backlog #26 + #27): a per-consumer ledger-ordered (Script<Key) comparator for `
  + `the dugite Cardano node's phase-2 ScriptContext. Read the actual changes: git diff on crates/dugite-primitives/src/{credentials.rs,transaction.rs} `
  + `+ crates/dugite-uplc/src/{populate_gov.rs,populate_v3.rs,redeemer_resolve.rs,tx_info_populate.rs}, and the analysis `
  + `scripts/prod-readiness/.audit/credential-ord-analysis.md.\n\n`
  + `WHAT IT DOES: dugite's Credential enum derives Ord Key<Script (correct for Plutus-Data-tag + canonical-CBOR-key roles); `
  + `Haskell LEDGER Credential is Script<Key. The fix ADDS Credential::cmp_ledger (Script rank 0 < Key rank 1, then 28-byte hash) `
  + `+ Voter::cmp_ledger (variant rank CC<DRep<SPO unchanged, inner cred via cmp_ledger), and applies them when building `
  + `txInfoVotes, txInfoWdrl (V1/V2/V3), and resolving the Vote/Reward redeemer-pointer index. The shared derived Ord is UNCHANGED. `
  + `The reward/stake conservation pipeline is untouched (credentials erased to typed-Hash32 at ingest).\n\n`
  + `ESTABLISHED FACTS the engine already verified (attack these if you can, do not just accept them): `
  + `(a) Per-entry transform is byte-identical old-vs-new for withdrawals: old withdrawal_to_plutus and new ledger_ordered_withdrawals `
  + `both do PrimAddress::from_bytes -> r.stake -> credential_to_plutus -> (StakingCredential::Hash(cred), BigInt); ONLY the order differs. `
  + `(b) Ordering is a no-op except for >=2 entries of MIXED key+script type in one collection (same-type multi-entry sorts by hash identically; single-entry trivial). `
  + `(c) Haskell refs: Voter = CommitteeVoter<DRepVoter<StakePoolVoter; Credential = ScriptHashObj<KeyHashObj (Script<Key); RewardAccount Ord = (Network, Credential). `
  + `(d) Targeted fmt+clippy+nextest 730/730 green (independently re-verified). `
  + `(e) 0/769 phase2-dumps-730val exercise the changed code; preprod frontier ep0-293 was byte-exact PRE-fix.`

const LENSES = [
  {
    key: 'per-entry-byte-identity',
    prompt: `LENS: per-entry byte-identity. Read the actual diff. Is the NEW per-entry ScriptContext value (for withdrawals AND votes) `
      + `byte-identical to the OLD for the common single-entry and same-type-multi-entry cases? Look for ANY subtle difference: a `
      + `changed credential wrapping, a Ptr-vs-Hash staking credential case, a V1/V2/V3 representation mismatch, BigInt encoding, `
      + `an amount type change, or the votes-map value shape. If the new code can produce a DIFFERENT byte output than pre-fix for `
      + `a tx that has NO mixed key+script multi-entry collection, refuted=true (that would regress the byte-exact-validated history).`,
  },
  {
    key: 'haskell-ord-correctness',
    prompt: `LENS: Haskell-Ord correctness across Plutus language versions. Is Script<Key the order cardano-ledger/plutus ACTUALLY uses `
      + `for txInfoVotes, txInfoWdrl, and the Vote/Reward redeemer-pointer index, at PlutusV1, V2, AND V3? Could any of these be built `
      + `from a different structure (e.g. a list preserving a different order, or a Set with a different Ord, or wire-tag order) such `
      + `that Script<Key is WRONG for that site/version? Also: is the redeemer-pointer index space really the ledger Map.toList/Set.elemAt `
      + `order (Script<Key) and not something else? Consult the analysis + Haskell source. If the fix applies Script<Key where Haskell `
      + `uses a different order at any site/version, refuted=true.`,
  },
  {
    key: 'completeness-consistency',
    prompt: `LENS: completeness + internal consistency. The fix deliberately EXCLUDED TreasuryWithdrawals + UpdateCommittee credential maps `
      + `in populate_gov.rs and the consensus-CBOR encode/governance.rs (deferred to #26b), and txInfoSignatories (#30). Does fixing `
      + `txInfoVotes/txInfoWdrl ordering while leaving those creates an INTERNAL INCONSISTENCY within a single ScriptContext that makes `
      + `the result WRONG (worse than before)? Or does the fix MISS a credential-ordered phase-2 site that IS in scope (e.g. a V3 `
      + `txInfo field, dRep delegation, or a redeemer map) that would now be inconsistent with the fixed ones? If the partial fix `
      + `introduces a new wrong-ordering or inconsistency, refuted=true. (A pre-existing still-inverted EXCLUDED site that is independently `
      + `tracked as #26b is NOT a refutation — only a NEW inconsistency or an in-scope miss is.)`,
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent(`Adversarially REFUTE the fix via this lens. Default refuted=true if uncertain. Read the real code before deciding.\n\n${CONTEXT}\n\n${l.prompt}`,
      { label: `refute:${l.key}`, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
