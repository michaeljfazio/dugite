export const meta = {
  name: 'diagnose-30-signatories',
  description: 'DIAGNOSE #30: confirm Haskell txInfoSignatories = Set.toList (sorted+deduped) + the required_signers decode strictness + the exact dugite fix',
  phases: [{ title: 'Diagnose', detail: 'source-confirm txInfoSignatories ordering + decode-set strictness; localize fix' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['is_real_gap', 'haskell_signatories_rule', 'haskell_decode_strictness', 'haskell_source', 'dugite_chain', 'fix_plan', 'observability', 'confidence', 'caveats'],
  properties: {
    is_real_gap: { type: 'boolean' },
    haskell_signatories_rule: { type: 'string', description: 'how cardano-ledger builds txInfoSignatories for V1/V2/V3: is it Set.toList over reqSignerHashesTxBodyL (ascending byte order + deduped)? same for all 3 Plutus versions?' },
    haskell_decode_strictness: { type: 'string', description: 'how is the tx-body required_signers (key 14) decoded? as a strict Set that REJECTS non-canonical (out-of-order / duplicate) at CBOR decode, or lenient (accept then Set re-sorts/dedups)? This decides whether the fix is sort-in-txInfoSignatories, reject-at-decode, or both.' },
    haskell_source: { type: 'string', description: 'canonical module/function + verbatim/paraphrased (Alonzo/Babbage/Conway TxInfo.hs txInfoSignatories; the TxBody reqSignerHashes decoder / decodeSet / its CDDL nonempty_set strictness)' },
    dugite_chain: { type: 'string', description: 'the dugite decode→txInfoSignatories chain: how is required_signers decoded (Vec? Set? read_set strip tag-258 only?), and required_signers_to_plutus_padded (tx_info_populate.rs:481-485) maps Vec in wire order with no sort/dedup; used by populate_tx_info_v1/v2/v3' },
    fix_plan: { type: 'string', description: 'the precise byte-exact fix: sort+dedup the keyhashes (by full byte order) in required_signers_to_plutus_padded? AND/OR reject non-canonical at decode (if Haskell is strict)? which sites; how to dedup (28-byte keyhash); does it interact with #26 cmp_ledger or the padded-hash form?' },
    observability: { type: 'string', description: 'is this byte-exact-observable on a real chain (are on-chain required_signers already canonically sorted so honest txs match), or only adversarial/non-canonical? does a script reading txInfoSignatories change result/ExUnits?' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  'DIAGNOSE dugite backlog #30 (re-audit candidate conf 0.6; the engine has HEAD-VERIFIED the dugite side: '
  + 'required_signers_to_plutus_padded at crates/dugite-uplc/src/tx_info_populate.rs:481-485 does '
  + 'signers.iter().map(...).collect() — NO sort, NO dedup, preserving on-wire order, used by populate_tx_info_v1/v2/v3). '
  + 'Your job: source-confirm the canonical Haskell behavior + decide the byte-exact fix.\n\n'
  + 'CLAIM: Haskell txInfoSignatories = Set.toList (reqSignerHashesTxBodyL :: Set (KeyHash Witness)) = ascending byte order + '
  + 'deduped, whereas dugite emits the required_signers in WIRE order with possible duplicates → a non-canonically-ordered or '
  + 'duplicate-bearing required_signers field yields a different ScriptContext + different ExUnit cost vs cardano-node.\n\n'
  + 'THE CRUX (source-confirm, do NOT guess) — answer BOTH:\n'
  + '  (1) txInfoSignatories construction: Read IntersectMBO/cardano-ledger Alonzo/Babbage/Conway TxInfo.hs. Is txInfoSignatories '
  + 'built as `Set.toList (txBody ^. reqSignerHashesTxBodyL)` mapped to PubKeyHash, i.e. ASCENDING byte order + DEDUPED, for '
  + 'PlutusV1, V2 AND V3? Quote it.\n'
  + '  (2) required_signers DECODE strictness: How is the tx-body required_signers (body key 14) decoded? Is it a STRICT Set that '
  + 'REJECTS a non-canonical CBOR encoding (out-of-order or duplicate elements) at decode (CDDL nonempty_set / decodeSet with a '
  + 'canonicity/strict-dedup check), or LENIENT (decodes any array then inserts into a Set which silently re-sorts+dedups)? '
  + 'This is decisive: if Haskell REJECTS non-canonical at decode, the byte-exact dugite fix must ALSO reject at decode (a CBOR '
  + 'strictness gap, like #28/#31) — and then txInfoSignatories is moot because such a tx never reaches phase-2; if Haskell is '
  + 'LENIENT + re-sorts, the fix is to sort+dedup in txInfoSignatories (the required_signers_to_plutus_padded builder). Read the '
  + 'cardano-ledger Conway TxBody decoder for reqSignerHashes + the relevant decodeSet/decodeNonEmptySet semantics; also check '
  + 'whether this changed across eras (Alonzo array vs Conway tag-258 set). Use the in-project refs '
  + '.claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md FIRST, then cardano-haskell-oracle / WebFetch.\n\n'
  + 'Then: is_real_gap=true ONLY if dugite genuinely diverges from Haskell (in txInfoSignatories content and/or decode '
  + 'acceptance). Give the precise byte-exact fix (sort+dedup in the builder, and/or reject-non-canonical-at-decode — be explicit '
  + 'which, per the strictness finding), the dugite sites, and the observability (do on-chain txs already have canonical '
  + 'required_signers so honest sync is unaffected — adversarial only — or is it a live divergence?). Note any interaction with '
  + 'the #26 cmp_ledger work or the 28-byte-keyhash-padded-to-32 form. Return the StructuredOutput.',
  { label: 'diagnose:30', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
