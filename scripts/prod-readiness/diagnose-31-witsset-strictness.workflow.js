export const meta = {
  name: 'diagnose-31-witsset-strictness',
  description: 'DIAGNOSE #31: does cardano-ledger TxWits/TxBody SparseKeyed REJECT unknown keys? + the Conway set duplicate-reject (folded #30 fix-B); consensus impact',
  phases: [{ title: 'Diagnose', detail: 'source-confirm SparseKeyed unknown-key strictness + Conway decodeSet dup-reject; consensus vs mempool; fix scope' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['witsset_unknown_key', 'txbody_unknown_key', 'conway_set_duplicate', 'haskell_source', 'dugite_sites', 'consensus_impact', 'is_real_gap', 'fix_plan', 'confidence', 'caveats'],
  properties: {
    witsset_unknown_key: { type: 'string', description: 'does cardano-ledger TxWits (SparseKeyed/decodeKeyedSparse) REJECT an unknown map key at decode, or SKIP it (forward-compat)? quote the decoder. is it era/version-gated?' },
    txbody_unknown_key: { type: 'string', description: 'same question for the tx BODY sparse decoder — reject or skip unknown keys? (related; dugite skips there too)' },
    conway_set_duplicate: { type: 'string', description: 'confirm the folded #30 fix-B: Conway PV9+ decodeSetEnforceNoDuplicates rejects DUPLICATES at decode for tag-258 Set fields; which dugite read_set sites are affected; is ordering ever enforced (no)' },
    haskell_source: { type: 'string', description: 'canonical modules/functions: TxWits decCBOR (SparseKeyed/witnessField), the sparse decoder unknown-key behavior (decodeKeyedSparse / Field / the catch-all), TxBody decoder, decodeSetEnforceNoDuplicates' },
    dugite_sites: { type: 'string', description: 'dugite witness-set _ => r.skip() sites (era_alonzo.rs:1019-1021, era_babbage ~910-912, era_conway.rs:2232-2234) + tx-body equivalents + the read_set sites (no dedup/dup-reject)' },
    consensus_impact: { type: 'string', description: 'is unknown-key-skip a CONSENSUS divergence (Haskell rejects the block/tx at decode so dugite adopting it splits the chain) or only a mempool/admission asymmetry? could such a tx ever be on-chain (Haskell rejects so no)? severity = adversarial/latent or live?' },
    is_real_gap: { type: 'boolean', description: 'true ONLY if Haskell genuinely REJECTS what dugite SKIPS (i.e. dugite is too lenient). If Haskell ALSO skips unknown keys, this is a FALSE candidate — say so.' },
    fix_plan: { type: 'string', description: 'precise fix IF real: reject unknown witness-set keys (which eras), reject unknown tx-body keys (if Haskell does), Conway set dup-reject. Scope + which existing tests assert the skip and must flip. If FALSE candidate, say no fix.' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  'DIAGNOSE dugite backlog #31 (re-audit candidate conf 0.55 — the LOWEST confidence of the batch, so it may be a FALSE '
  + 'candidate; CONFIRM before any fix). The engine HEAD-verified the dugite side: the Conway witness-set map decoder has '
  + '`_ => { r.skip()?; }` for keys outside 0..7 (crates/dugite-serialization/src/decode/era_conway.rs:2232-2234; same pattern '
  + 'era_alonzo.rs:1019-1021, era_babbage.rs ~910-912) — it SILENTLY SKIPS unknown witness-set map keys.\n\n'
  + 'CLAIM: Haskell decodes TxWits via SparseKeyed ("TxWits", decodeKeyedSparse) which HARD-FAILS on an unknown key, so dugite '
  + 'admits a tx (and thus a block) Haskell rejects at decode.\n\n'
  + 'THE CRUX (source-confirm, do NOT guess — this is the make-or-break for is_real_gap):\n'
  + '  (1) Read IntersectMBO/cardano-ledger Cardano.Ledger.Alonzo.TxWits / Babbage / Conway TxWits decCBOR and the underlying '
  + 'sparse decoder (Cardano.Ledger.Binary.Decoding — decodeKeyedSparse / decodeSparse / the Field machinery). When the decoder '
  + 'encounters a map key NOT in its known field set, does it (a) cborError/fail (STRICT — dugite must reject), or (b) silently '
  + 'skip the value and continue (LENIENT — dugite already matches, FALSE candidate)? Quote the catch-all / default behavior. '
  + 'Is it the same for Alonzo/Babbage/Conway?\n'
  + '  (2) Same question for the tx BODY sparse decoder (dugite also has `_ => r.skip()` for unknown body keys) — reject or skip?\n'
  + '  (3) FOLDED #30 fix-B: confirm Conway PV9+ decodeSetEnforceNoDuplicates REJECTS DUPLICATES at decode for tag-258 Set fields '
  + '(the #30 diagnose found this at conf 0.9); enumerate the dugite read_set sites (inputs/collateral/certs/reference_inputs/'
  + 'vkey_witnesses/required_signers/plutus_scripts) that currently accept duplicates; confirm ordering is NEVER enforced.\n'
  + 'Use the in-project refs .claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md FIRST, then '
  + 'cardano-haskell-oracle / WebFetch the cardano-ledger source (permalink-pin the decisive lines).\n\n'
  + 'Then: CONSENSUS IMPACT — if Haskell rejects unknown keys / duplicates at DECODE, a block carrying such a tx is rejected by '
  + 'Haskell nodes, so dugite accepting it is a chain-split/partition surface (but adversarial — no honest tx has them). '
  + 'Distinguish consensus (block-level) vs mempool-only. is_real_gap=true ONLY where Haskell genuinely rejects what dugite '
  + 'accepts. Give the precise fix (reject unknown wits keys / unknown body keys / Conway set dups — be explicit which are real '
  + 'and which eras), the dugite sites, and note the existing tests asserting the skip that must flip to expect-rejection. '
  + 'Return the StructuredOutput.',
  { label: 'diagnose:31', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
