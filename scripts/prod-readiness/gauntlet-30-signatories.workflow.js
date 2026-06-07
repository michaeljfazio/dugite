export const meta = {
  name: 'gauntlet-30-signatories',
  description: 'Refutation panel for the #30 txInfoSignatories sort+dedup fix before commit (Set.toList match, over-canonicalization, completeness)',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #30 fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/incomplete/over-canonicalizing OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #30). Read the ACTUAL current code: git diff on '
  + 'crates/dugite-uplc/src/tx_info_populate.rs.\n\n'
  + 'WHAT IT DOES: required_signers_to_plutus_padded now does `out.sort(); out.dedup();` on the Vec<PubKeyHash> it builds '
  + '(PubKeyHash = [u8; 28], derived lexicographic Ord), so dugite emits txInfoSignatories in ascending byte order with '
  + 'duplicates removed instead of on-wire order. This one helper feeds txInfoSignatories for PlutusV1 (populate_v1_v2.rs:62), '
  + 'V2 (:112) and V3 (populate_v3.rs:91). Added a unit test (wire order [3,1,2,1] -> [1,2,3]; canonical/empty/single unchanged). '
  + 'The sibling required_signers_to_plutus (Hash28 variant) is TEST-ONLY (sole caller is a test).\n\n'
  + 'CANONICAL HASKELL (source-confirmed conf 0.9, but the quotes were WebFetch-sourced and line numbers may drift — '
  + 'PERMALINK-RECONFIRM is part of your job): cardano-ledger builds txInfoSignatories (identically for V1/V2/V3 via the shared '
  + 'Alonzo helper transTxBodyReqSignerHashes) as `transKeyHash <$> Set.toList (txBody ^. reqSignerHashesTxBodyG)` where '
  + 'reqSignerHashes (body key 14) is a Set (KeyHash Witness). Set.toList = ascending Ord (28-byte lexicographic) + deduped. '
  + 'transKeyHash = pure 28-byte copy. Decode: Alonzo/Babbage decodeSet is lenient (re-sort+dedup); Conway PV9+ rejects '
  + 'duplicates at decode (a SEPARATE strictness gap tracked with #31 as fix (B) — NOT this fix).\n\n'
  + 'CONTEXT: dugite already canonicalizes its OTHER Set-like TxInfo fields the same way (sort_inputs+dedup at '
  + 'tx_info_populate.rs:429, withdrawals sort_by cmp_ledger, datums sort_by_key, voters sort_by cmp_ledger) — required_signers '
  + 'was the lone wire-order one.\n\n'
  + 'GREEN STATUS (engine-verified): fmt+clippy+nextest 448/448 incl. the new canonicalisation test + the real '
  + 'onchain_babbage_scripts_validate_within_declared_budget test (no regression).'

const LENSES = [
  {
    key: 'set-tolist-exact-match',
    prompt: 'LENS: does sort()+dedup() on PubKeyHash exactly reproduce Haskell Set.toList(Set KeyHash) for V1/V2/V3? '
      + 'PERMALINK-RECONFIRM (do not just trust the summary): fetch the actual cardano-ledger Alonzo/Babbage/Conway TxInfo.hs and '
      + 'verify transTxBodyReqSignerHashes = transKeyHash <$> Set.toList(reqSignerHashes) and that V1/V2/V3 all use it (no '
      + 'era-specific signatories path). Then verify the Rust: (a) PubKeyHash = [u8;28] derived Ord is lexicographic over 28 '
      + 'bytes == Haskell Ord(KeyHash); (b) Rust slice sort() is ascending lexicographic == Set ascending; (c) Vec::dedup AFTER '
      + 'sort removes ALL duplicates (not just consecutive in an unsorted vec) == Set uniqueness — confirm sort happens BEFORE '
      + 'dedup in the code; (d) the 28-byte unpadding happens correctly (padded_signer_to_pubkeyhash copies the first 28 of the '
      + '32-byte form; the 4 pad bytes are constant zero so they cannot affect the relative order). If sort+dedup diverges from '
      + 'Set.toList in any way, refuted=true.',
  },
  {
    key: 'over-canonicalization',
    prompt: 'LENS: over-canonicalization. Does Haskell REALLY sort AND dedup txInfoSignatories for ALL of V1, V2, V3 — or is there '
      + 'an era / Plutus version where Haskell preserves wire order or does NOT dedup (in which case dugite now WRONGLY '
      + 'canonicalizes)? Confirm reqSignerHashes is genuinely a Set (not a list) in the TxBody for every relevant era, and that '
      + 'the same transTxBodyReqSignerHashes is the txInfoSignatories source in all three Plutus TxInfo instances. Also confirm '
      + 'dugite did NOT accidentally change any OTHER field or path by this edit. If dugite now canonicalizes where Haskell '
      + 'preserves wire order, refuted=true.',
  },
  {
    key: 'completeness-commit-safety',
    prompt: 'LENS: completeness + is (A) alone commit-safe? (a) Are there OTHER live txInfoSignatories build paths besides '
      + 'required_signers_to_plutus_padded (grep the crate)? Confirm the Hash28 variant required_signers_to_plutus is truly '
      + 'test-only (its only non-def caller is a #[test]). (b) Does committing fix (A) alone — without the (B) Conway-PV9+ '
      + 'duplicate-reject-at-decode — leave the #30 fix WRONG or commit-unsafe? Note: (A) makes the txInfoSignatories CONTENT '
      + 'match Haskell (sorted+deduped) for any tx that reaches phase-2; (B) is a separate phase-1/admission asymmetry (dugite '
      + 'accepts a duplicate-bearing Conway tx Haskell rejects at decode) correctly tracked with #31. Is committing (A) a strict '
      + 'improvement that breaks nothing (honest canonical txs unchanged; non-canonical txs now get the Haskell-correct '
      + 'signatories content)? Refuted=true ONLY if (A) alone is wrong/unsafe to commit; a correctly-tracked separate (B) is NOT '
      + 'a refutation.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the #30 fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
