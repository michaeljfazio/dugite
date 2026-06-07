export const meta = {
  name: 'fix-31a-witsset-reject',
  description: 'FIXING #31-A: reject unknown WITNESS-SET map keys at decode (all eras), matching Haskell SparseKeyed invalidField -> cborError',
  phases: [{ title: 'Fix', detail: 'reject unknown witness-set keys (all eras); flip skip-tests; over-strictness guard' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['haskell_confirmed', 'haskell_source', 'sites_changed', 'tests_flipped', 'over_strictness_guard', 'checks', 'caveats', 'completed'],
  properties: {
    haskell_confirmed: { type: 'boolean', description: 'true if cardano-ledger txWitnessField n = invalidField n -> cborError (reject unknown wits key) is permalink-reconfirmed for all eras' },
    haskell_source: { type: 'string', description: 'permalink-pinned cardano-ledger source (Alonzo TxWits txWitnessField, the SparseKeyed/invalidField/invalidKey/cborError machinery), confirming not-version-gated reject' },
    sites_changed: { type: 'array', items: { type: 'string' }, description: 'the witness-set default arms changed from skip to reject (file:line, per era incl. Shelley)' },
    tests_flipped: { type: 'string', description: 'which *_witness_set_unknown_key_skipped tests were flipped to expect an error + any new Conway reject test' },
    over_strictness_guard: { type: 'string', description: 'evidence that ONLY witness-set arms changed — tx-body decoders, CostModels (cost_models_unknown_keys), PParamUpdate (pparam_update_unknown_key), and unrelated r.skip() (break bytes / known-unused) are UNTOUCHED' },
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
  'Implement dugite backlog #31-A (the witness-set part) in the MAIN working tree (no worktree; do NOT git commit). Single '
  + 'crate: dugite-serialization.\n\n'
  + 'GOAL (match Haskell byte-exact): cardano-ledger decodes the tx WITNESS SET via SparseKeyed with field-picker '
  + '`txWitnessField n = invalidField n`, so an UNKNOWN witness-set map key (anything outside the known set) HARD-FAILS the '
  + 'decode (invalidField -> Invalid n -> invalidKey -> cborError). This is NOT version-gated — it rejects in every era '
  + '(Shelley/Allegra/Mary/Alonzo/Babbage/Conway, all share the strict SparseKeyed behavior). dugite currently SILENTLY SKIPS '
  + 'unknown witness-set keys via `_ => { r.skip()?; }`, accepting txs/blocks Haskell rejects (a #539-class consensus '
  + 'admission gap). Fix: REJECT unknown witness-set keys.\n\n'
  + 'STEP 1 — PERMALINK-RECONFIRM (hard gate): WebFetch the actual cardano-ledger Alonzo TxWits.hs to confirm '
  + '`txWitnessField n = invalidField n` (the catch-all) and that the SparseKeyed/invalidField machinery yields cborError on an '
  + 'unknown key, NOT a skip — and that this is the decoder for Alonzo/Babbage/Conway TxWits and the Shelley-style witness '
  + 'decoder. Set haskell_confirmed=true ONLY if confirmed; if the source shows witness-set unknown keys are SKIPPED (lenient), '
  + 'set haskell_confirmed=false, change NO code, and report.\n\n'
  + 'STEP 2 — REJECT (only if confirmed): change the WITNESS-SET decoder default arm from `_ => { r.skip()?; }` to '
  + '`_ => { return Err(SerializationError::CborDecode(format!("witness set: unknown key {key}"))); }` (use the keyword/style '
  + 'already in the file; the witness-set match binds the key as e.g. `key`). The witness-set decoders are at: era_conway.rs '
  + '~:2232 (keys 0..7), era_babbage.rs ~:910 (keys 0..6), era_alonzo.rs ~:1019 (keys 0..5), AND the Shelley witness-set decoder '
  + '(find it in era_shelley.rs — keys 0..5/6; Allegra/Mary reuse the Shelley decoder, so fixing Shelley covers them — verify). '
  + 'Change ONLY these witness-set default arms.\n\n'
  + '*** OVER-STRICTNESS GUARD (CRITICAL — this is the #1 risk): DO NOT touch (leave lenient/skip):\n'
  + '  - the TX-BODY decoder default arms (that is #31-B, era-aware, separate — e.g. era_conway.rs ~:669 decode_conway_tx_body);\n'
  + '  - CostModels unknown-language-key handling (Haskell decodeCostModelsLenient RETAINS unknown keys — test '
  + 'cost_models_unknown_keys_ignored must stay);\n'
  + '  - PParamUpdate unknown keys (Haskell lenient — test pparam_update_unknown_key_skipped must stay);\n'
  + '  - any other r.skip()? that consumes break bytes / known-but-unused fields.\n'
  + 'Grep to be sure you changed ONLY the 3-4 witness-set default arms and nothing else.\n\n'
  + 'STEP 3 — FLIP TESTS: the existing tests asserting the lenient skip must now expect an ERROR: '
  + 'alonzo_witness_set_unknown_key_skipped (era_alonzo.rs:2188), babbage_witness_set_unknown_key_skipped (era_babbage.rs:1581), '
  + 'shelley_witness_set_unknown_key_skipped (era_shelley.rs:2442) — rename to *_unknown_key_rejected and assert the decode '
  + 'returns Err (CborDecode). Add a conway_witness_set_unknown_key_rejected test (append key=8 to a valid Conway witness set -> '
  + 'decode Err). If Allegra/Mary have their own such tests, flip them too. Do NOT touch cost_models_unknown_keys_ignored or '
  + 'pparam_update_unknown_key_skipped.\n\n'
  + 'BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-serialization --all-targets -- -D warnings ; cargo nextest run '
  + '-p dugite-serialization. Report each pass/fail. completed=true ONLY if haskell_confirmed, the witness-set arms reject, the '
  + 'skip-tests are flipped, the lenient cases are untouched, and fmt+clippy+nextest are green. Green tests are NOT byte-exact '
  + 'proof — a gauntlet follows. Do NOT commit.',
  { label: 'fix:31a', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
