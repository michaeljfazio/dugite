export const meta = {
  name: 'gauntlet-31a-witsset',
  description: 'Refutation panel for #31-A (reject unknown witness-set keys) before commit: Haskell-reject-all-eras, over-strictness, completeness',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #31-A fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/over-strict/incomplete OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #31-A). Read the ACTUAL current code: git diff on '
  + 'crates/dugite-serialization/src/decode/{era_shelley,era_alonzo,era_babbage,era_conway}.rs.\n\n'
  + 'WHAT IT DOES: the tx WITNESS-SET map decoder default arm was changed from `_ => { r.skip()?; }` (silently skip unknown '
  + 'keys) to `_ => { return Err(SerializationError::CborDecode(format!("witness set: unknown key {key}"))); }` (REJECT) at 4 '
  + 'sites: era_shelley.rs:1094 (decode_shelley_witness_set, keys 0..2), era_alonzo.rs:1019 (decode_alonzo_witness_set, keys '
  + '0..5; Allegra+Mary reuse this), era_babbage.rs:910 (keys 0..6), era_conway.rs:2232 (keys 0..7). 3 skip-tests were flipped '
  + 'to expect Err (alonzo/babbage/shelley *_witness_set_unknown_key_rejected) + a new conway_witness_set_unknown_key_rejected.\n\n'
  + 'CANONICAL HASKELL (the fix agent permalink-pinned cardano-ledger commit cd8b7fab — RE-CONFIRM independently): Alonzo TxWits '
  + 'decodes via SparseKeyed with field-picker txWitnessField whose catch-all is `txWitnessField n = invalidField n`; '
  + 'Coders.hs invalidField -> Invalid n -> invalidKey; Plain.hs invalidKey = cborError. So an unknown witness-set map key '
  + 'HARD-FAILS decode in every era (Shelley/Allegra/Mary/Alonzo/Babbage/Conway), NOT version-gated. Witness-set unknown keys '
  + 'are a #539-class CONSENSUS admission gap (adversarial/latent — honest cardano-node never emits them).\n\n'
  + 'IN SCOPE = ONLY the witness-set arms. OUT OF SCOPE (must stay lenient/skip): tx-body unknown keys (#31-B, era-aware), '
  + 'CostModels unknown language keys (Haskell decodeCostModelsLenient RETAINS them), PParamUpdate unknown keys (lenient).\n\n'
  + 'GREEN STATUS (engine-verified): git diff = exactly 4 r.skip() removed + 4 rejects; tx-body skips (era_conway:667/671) '
  + 'UNTOUCHED; cost_models_unknown_keys_ignored + pparam_update_unknown_key_skipped tests STILL PASS; fmt+clippy+nextest '
  + '1176/1176.'

const LENSES = [
  {
    key: 'haskell-reject-all-eras',
    prompt: 'LENS: does Haskell REALLY reject unknown witness-set keys, in EVERY era, with NO forward-compat leniency? PERMALINK-'
      + 'RECONFIRM independently (do not just trust the agent): fetch cardano-ledger Alonzo TxWits.hs and confirm txWitnessField '
      + 'catch-all = invalidField n, and trace invalidField -> invalidKey -> cborError (Coders.hs / Plain.hs). CRITICAL subtlety '
      + '(this is exactly where #30 found a version-gate): is the witness-set field picker version-gated or forward-compat in ANY '
      + 'era — e.g. does some decoder version SKIP unknown keys for forward compatibility, or does the Shelley/Allegra/Mary '
      + 'witness decoder differ from Alonzo+? Check the ifDecoderVersionAtLeast branches and the Shelley TxWits decoder '
      + 'specifically. If Haskell is lenient (skips) for unknown witness-set keys in ANY era/version, then dugite now OVER-rejects '
      + 'there -> refuted=true. Only NOT refuted if reject is correct for every era dugite supports.',
  },
  {
    key: 'over-strictness',
    prompt: 'LENS: over-strictness — did the fix reject anything it should not? Read the git diff. Confirm: (a) EXACTLY the 4 '
      + 'witness-set default arms changed, nothing else; (b) the tx-body decoder default arms (era_conway.rs:667/671 '
      + 'decode_conway_tx_body + pre-Conway bodies) STILL skip (that is #31-B); (c) CostModels (cost_models_unknown_keys_ignored) '
      + 'and PParamUpdate (pparam_update_unknown_key_skipped) unknown-key handling STILL lenient and their tests pass; (d) no '
      + 'break-byte / known-but-unused r.skip()? was turned into a reject. Also: does the reject correctly PROPAGATE — the '
      + 'witness-set match is inside a for_each_map_entry closure, so `return Err(...)` must abort the whole decode (verify it '
      + 'is not swallowed). If any non-witness-set path now rejects, or the error does not propagate, refuted=true.',
  },
  {
    key: 'completeness',
    prompt: 'LENS: completeness + correctness. (a) Are there OTHER live witness-set decode paths NOT covered by the 4 sites — e.g. '
      + 'a separate Conway/Dijkstra witness-set decoder, or a witness set decoded inline somewhere else (grep for read_set / '
      + 'witness_set / for_each_map_entry over witness keys)? Dijkstra reuses Conway TxWits — confirm there is no extra Dijkstra '
      + 'witness key that Haskell knows but dugite would now wrongly reject. (b) Did the 3 flipped tests + the new Conway test '
      + 'actually change to assert an ERROR (not silently still asserting skip)? (c) Is the reject commit-safe — honest mainnet/'
      + 'testnet blocks never carry unknown witness-set keys (cardano-node encodes only known keys), so live re-sync is '
      + 'unaffected; the only behavior change is rejecting adversarial/crafted txs Haskell also rejects. If a real witness-set '
      + 'path is missed, a Dijkstra key is wrongly rejected, or a test is not genuinely flipped, refuted=true.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the #31-A fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
