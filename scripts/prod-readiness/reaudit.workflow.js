export const meta = {
  name: 'dugite-reaudit',
  description: 'Adversarial re-audit: surface NEW byte-exactness/correctness/security gaps at HEAD, refute-verify each, write ranked findings',
  phases: [
    { title: 'Find', detail: '6 parallel finders, one per subsystem dimension' },
    { title: 'Verify', detail: 'refute-by-default adversarial verification per candidate' },
    { title: 'Synthesize', detail: 'dedup + rank survivors, write durable findings file' },
  ],
}

// Re-audit of the dugite node at HEAD. Backlog is cleared; goal is to surface
// NEW real gaps (NOT re-tread the #541 audit's 91 findings — 80 fixed — nor this
// session's fixes #6/#7/#11/#16/#17/#20/#23). Every finder cross-checks the actual
// HEAD Rust against canonical Haskell behavior (the in-project refs first, oracle
// only if needed). Refute-by-default verify kills plausible-but-wrong findings.

const REFS = '.claude/skills/haskell-ledger-cross-validation/references/era-rules'
const EXCLUDE = `Do NOT report anything already fixed/known: the #541 security audit (silent-skip BootstrapWitness/vkey/nonce_vrf/kes — issues #541-#550, mostly fixed), this session's fixes (#6 apply_utxo_diff instant-stake, #7 Dijkstra sub-tx instant-stake, #11 Shelley->Allegra RUPD, #16 script-ref language-tag doc, #17 snapshot crcOfConcat, #20a varlen overflow / #20b definite-map exact-count / #20c backend dup-key first-wins, #23 txInfoData V1 dedup), the #438/#481/#624/#626 reward fixes, and the DEFERRED #24 (PlutusV2 inline-datum-spend ExUnit over-cost). Confirm the current HEAD code before claiming a gap — if HEAD already handles it, do NOT report it.`

const FINDING_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['title', 'location', 'suspected_divergence', 'haskell_reference', 'severity', 'confidence', 'how_to_confirm'],
        properties: {
          title: { type: 'string', description: 'one-line gap title' },
          location: { type: 'string', description: 'file:line(s) in HEAD' },
          suspected_divergence: { type: 'string', description: 'what dugite does vs what Haskell/the spec does, concretely' },
          haskell_reference: { type: 'string', description: 'canonical Haskell module/function or spec rule that defines correct behavior' },
          severity: { type: 'string', enum: ['H', 'M', 'L'] },
          confidence: { type: 'number', description: '0..1 that this is a REAL divergence (not already handled)' },
          how_to_confirm: { type: 'string', description: 'the byte-exact replay / dump-diff / test that would confirm or refute' },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['real', 'reason', 'adjusted_severity'],
  properties: {
    real: { type: 'boolean', description: 'true ONLY if, after trying to refute it, the gap survives as a genuine HEAD divergence' },
    reason: { type: 'string', description: 'the refutation attempt result: why it is real, or why it is actually already-correct/handled' },
    adjusted_severity: { type: 'string', enum: ['H', 'M', 'L'] },
  },
}

const DIMENSIONS = [
  {
    key: 'ledger-reward-epoch',
    prompt: `You are auditing dugite-ledger's reward + epoch-transition math for byte-exactness vs Haskell cardano-ledger, at HEAD. Read the actual code: crates/dugite-ledger/src/rewards.rs, epoch.rs, ledger_seq.rs, and eras/{shelley,allegra,alonzo,babbage,conway}.rs. Cross-check against ${REFS}/{shelley-rewards,shelley-certs,INDEX}.md. Hunt areas NOT covered by the already-fixed reward bugs: pool-deregistration deposit refunds + timing, key-deposit/pool-deposit accounting into the deposit pot, MIR cert application to treasury vs reserves at each era boundary, undistributed-reward return to reserves, reward-account filtering (deregistered creds), proposal/governance deposit refunds, the order of SNAP vs reward application. For each suspected divergence give exact file:line and the Haskell function it should match.`,
  },
  {
    key: 'conway-governance',
    prompt: `You are auditing dugite-ledger's Conway governance (CIP-1694) for byte-exactness vs Haskell cardano-ledger Conway, at HEAD. Find the governance code (search crates/dugite-ledger/src for governance/ratification/voting/drep/committee/proposal). Cross-check ${REFS}/conway.md. Hunt: ratification threshold math per governance-action type + per voter (DRep/SPO/CC), the dRepVotingThresholds/poolVotingThresholds application, ratification vs enactment ordering, treasury-withdrawal enactment, committee-update / no-confidence handling, DRep expiry + activity, proposal deposit return on enactment/expiry, the gov-action lifecycle (proposed->ratified->enacted) and how it touches the deposit pot + treasury. Exact file:line + the Haskell rule each should match.`,
  },
  {
    key: 'phase2-scriptcontext',
    prompt: `You are auditing dugite-uplc's Plutus ScriptContext / TxInfo builder for schema byte-exactness vs Haskell (cardano-ledger TxInfo + plutus ScriptContext), at HEAD. Read crates/dugite-uplc/src/{tx_info_populate.rs, populate_v1_v2.rs, redeemer_resolve.rs, eval_redeemer.rs} and any v3 scriptcontext builder. Hunt NEW schema gaps (NOT the deferred #24 inline-datum over-cost): V3 TxInfo field completeness + ordering (txInfoVotes, txInfoProposalProcedures, txInfoCurrentTreasuryAmount, txInfoTreasuryDonation, txInfoRedeemers map), V1/V2 vs V3 field differences, era-fallback cost-model selection, redeemer pointer/index construction (Set TxIn ordering), datum/redeemer Map vs List encoding, value/multiasset map ordering in the context. Exact file:line + the Haskell builder field each should match.`,
  },
  {
    key: 'cbor-strictness',
    prompt: `You are auditing dugite-serialization's CBOR decoders for adversarial-input strictness + byte-exact decode, at HEAD. Read crates/dugite-serialization/src/decode/ broadly. Hunt the #537/#539/#624 SILENT-SKIP class at NEW sites: any decoder that calls r.skip() / silently drops an unknown-or-unhandled field that Haskell would either reject or apply, any lenient length/tag acceptance, any place a malformed sub-structure is swallowed instead of erroring, missing canonical-form enforcement where Haskell is strict, integer/bignum bounds not checked, indefinite-vs-definite acceptance mismatches. dugite-node is adversarial-deployment software: default-to-reject. Exact file:line + what Haskell's decoder (cardano-ledger CBOR / cborg) does instead.`,
  },
  {
    key: 'consensus-header-vrf-kes',
    prompt: `You are auditing dugite-consensus header validation + crypto checks for byte-exactness vs Haskell ouroboros-consensus / cardano-ledger Praos, at HEAD. Read crates/dugite-consensus/src broadly (header validation, VRF leader check, KES, opcert, nonce evolution, chain selection/tiebreaker). Hunt: VRF leader-value threshold comparison (the certified natural vs f(sigma)), nonce (eta) evolution + epoch nonce computation + the 'stability window' boundary, opcert counter monotonicity + KES period bounds + KES signature window, header field size/range checks, chain-selection tiebreaker (RestrictedVRF at Conway, comparePraos), the slot->epoch + leadership-eligibility math. Exact file:line + the Haskell function each should match.`,
  },
  {
    key: 'epoch-snapshot-stake',
    prompt: `You are auditing dugite-ledger's mark/set/go snapshot model + instant-stake distribution for byte-exactness vs Haskell cardano-ledger, at HEAD (NOT the already-fixed #6/#7 instant-stake replay paths). Read crates/dugite-ledger/src/{epoch.rs, ledger_seq.rs, state/mod.rs} and snapshot logic. Cross-check ${REFS}/{shelley-core,shelley-rewards,conway}.md. Hunt: the snapshot rotation (mark<-set<-go) exact timing at the epoch boundary, pointer-stake inclusion (pre-Conway) vs exclusion (Conway), reward-account balance folding into stake, stake-distribution per-credential aggregation vs the resolved active-stake VMap, pool-stake aggregation + the pool->delegator mapping, deregistered-cred exclusion timing, the deposit pot in the snapshot. Exact file:line + the Haskell function each should match.`,
  },
]

phase('Find')
const results = await pipeline(
  DIMENSIONS,
  (d) => agent(d.prompt + '\n\n' + EXCLUDE, { label: `find:${d.key}`, phase: 'Find', schema: FINDING_SCHEMA }),
  (res, d) => {
    const cands = (res && res.findings ? res.findings : []).filter((f) => f.confidence >= 0.45 && f.severity !== 'L')
    if (!cands.length) return { key: d.key, verified: [] }
    return parallel(
      cands.map((f) => () =>
        agent(
          `Adversarially REFUTE this suspected dugite-vs-Haskell divergence. Default to real=false unless you can confirm, by reading the actual HEAD code at the cited location AND the canonical Haskell behavior, that dugite genuinely diverges and it is NOT already handled elsewhere. Be a skeptic.\n\nTitle: ${f.title}\nLocation: ${f.location}\nSuspected divergence: ${f.suspected_divergence}\nHaskell reference: ${f.haskell_reference}\nHow to confirm: ${f.how_to_confirm}\n\nRead the cited file:line in the repo, trace whether HEAD already handles it (guards, callers, era gates), and either confirm (real=true) or refute (real=false) with a concrete reason.`,
          { label: `verify:${d.key}:${(f.title || '').slice(0, 28)}`, phase: 'Verify', schema: VERDICT_SCHEMA }
        ).then((v) => ({ ...f, verdict: v }))
      )
    ).then((vs) => ({ key: d.key, verified: vs.filter(Boolean) }))
  }
)

phase('Synthesize')
const confirmed = results
  .filter(Boolean)
  .flatMap((r) => (r.verified || []))
  .filter((f) => f.verdict && f.verdict.real)
  .map((f) => ({ ...f, severity: f.verdict.adjusted_severity || f.severity }))

const payload = JSON.stringify({ confirmed, by_dimension: results.map((r) => r && r.key) }, null, 2)
const summary = await agent(
  `Below is JSON of adversarially-CONFIRMED re-audit findings for the dugite Cardano node (each survived refute-by-default verification). Write them to disk as a durable, ranked backlog-ready file.\n\nSteps:\n1. Run: mkdir -p scripts/prod-readiness/.audit\n2. Write the file scripts/prod-readiness/.audit/reaudit-findings.md with: a one-line header, then the findings ranked by severity (H first) then confidence desc. For EACH finding include: a proposed backlog rank/impact tag [H|M|L], the title, location (file:line), the suspected divergence, the Haskell reference, confidence, the verifier's reason it is real, and how_to_confirm (the byte-exact replay/dump-diff that a fix-wake must use — tests-green is NOT proof).\n3. De-duplicate findings that point at the same root cause/location (merge them).\n4. Return (as your final text) a concise plain-text ranked list: "N confirmed findings: [H] title (file) — one-line; ..." so the engine can paste them into the backlog. If there are ZERO confirmed findings, say exactly "ZERO confirmed findings" and still write the file noting that.\n\nJSON:\n${payload}`,
  { label: 'synthesize:write-findings', phase: 'Synthesize' }
)

return { confirmed_count: confirmed.length, summary }
