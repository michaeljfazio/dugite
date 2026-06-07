export const meta = {
  name: 'diagnose-31b-body-keys',
  description: 'DIAGNOSE #31-B: exact Conway vs Dijkstra known tx-body key sets (cardano-ledger bodyFields) + dugite era-threading + era-aware reject fix',
  phases: [{ title: 'Diagnose', detail: 'pin the per-era known-body-key sets + the dugite era thread + the fix' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['conway_known_keys', 'dijkstra_known_keys', 'reject_confirmed', 'haskell_source', 'dugite_era_thread', 'fix_plan', 'tests', 'confidence', 'caveats'],
  properties: {
    conway_known_keys: { type: 'string', description: 'the EXACT set of tx-body map keys cardano-ledger Conway bodyFields recognizes (e.g. 0..8,9,10,11,13,14,15,16,17,18,19,20,21,22). list them; note any gaps (e.g. key 12 unused)' },
    dijkstra_known_keys: { type: 'string', description: 'the EXACT set for the Dijkstra (next) era bodyFields — which keys it ADDS over Conway (e.g. 5=account_balance_intervals, 23/25/26, key-14 guards) and whether it removes/changes any' },
    reject_confirmed: { type: 'string', description: 'confirm Haskell bodyFields catch-all = invalidField n -> cborError (hard-fail unknown body key), per-era (Conway rejects a Dijkstra-only key; each era knows only its own keys), NOT forward-compat — and re-check the version-gate (decodeSparseKeyed _ -> Nothing -> failMsg) like #31-A' },
    haskell_source: { type: 'string', description: 'cardano-ledger Conway/Dijkstra TxBody bodyFields (the known-key match + the invalidField catch-all); permalink-pin' },
    dugite_era_thread: { type: 'string', description: 'how dugite decode_conway_tx_body knows whether it is decoding a Conway or Dijkstra body — is the active Era threaded in (decode_conway_block_mode passes Era::Conway/Dijkstra)? where; or does it need a new param?' },
    fix_plan: { type: 'string', description: 'the era-aware fix: the default arm must reject keys outside the ACTIVE eras known set (Conway set for Conway blocks, Dijkstra set for Dijkstra) — NOT blindly reject >22; keep key-6 (pre-Conway update) handling; how to thread era; whether read_set/other decoders need the same' },
    tests: { type: 'string', description: 'which existing tests to flip (shelley_body_unknown_key_skipped) + keep lenient (test_dijkstra_unknown_tx_body_key_skipped — only if its key is a genuinely-future key Dijkstra does NOT yet know); add a Conway-rejects-Dijkstra-only-key test + Dijkstra-accepts-its-own-key test' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  'DIAGNOSE dugite backlog #31-B (the TX-BODY part; #31-A witness-set is already DONE+committed). This is the ERA-AWARE part — '
  + 'more delicate than #31-A because dugite has ONE decode_conway_tx_body that serves BOTH Conway and Dijkstra (which have '
  + 'DIFFERENT known-body-key sets), and its default arm currently `_ => { r.skip()?; }` (era_conway.rs:671-674, comment '
  + '"Unknown key — skip for forward compatibility (Dijkstra may add keys)").\n\n'
  + 'Haskell decodes the tx body via SparseKeyed with field-picker bodyFields whose catch-all is bodyFields n = invalidField n '
  + '-> cborError, so an unknown body key HARD-FAILS — BUT each era knows only its OWN keys, so the Conway decoder rejects a '
  + 'Dijkstra-only key and vice versa. To match byte-exact, dugite must reject keys outside the ACTIVE eras known set (NOT '
  + 'blindly reject everything >22).\n\n'
  + 'RESOLVE (source-confirm, permalink-pin — do NOT guess; #31-A caught a WebFetch hallucination on the v12+ version-gate, so '
  + 'read RAW source and re-check the version-gate the same way):\n'
  + '  (1) The EXACT set of tx-body map keys cardano-ledger CONWAY bodyFields recognizes (Cardano.Ledger.Conway.TxBody '
  + 'bodyFields / the From-field list). List every known key 0..N and any gaps.\n'
  + '  (2) The EXACT set for the DIJKSTRA (next) era bodyFields — which keys it ADDS over Conway and whether it changes any. '
  + '(dugite already handles key 5 = account_balance_intervals as a Dijkstra field, key 6 = pre-Conway update skipped.) Read the '
  + 'Dijkstra TxBody source.\n'
  + '  (3) Confirm bodyFields catch-all rejects (invalidField -> cborError) and re-check the decodeSparseKeyed v12+ path '
  + '(_ -> Nothing -> failMsg, NOT a forward-compat skip) — same trap as #31-A.\n'
  + 'Use the in-project refs .claude/skills/haskell-ledger-cross-validation/references/era-rules/{conway,dijkstra}.md FIRST, then '
  + 'WebFetch the cardano-ledger Conway + Dijkstra TxBody source (read raw, permalink-pin).\n\n'
  + 'Then: dugite era-threading — read crates/dugite-serialization/src/decode/era_conway.rs decode_conway_tx_body + its callers '
  + '(decode_conway_block_mode passes Era::Conway / Era::Dijkstra; decode_dijkstra_block at ~:94). Does the active Era reach the '
  + 'body decoder, or must it be threaded in as a new param? Give the precise era-aware fix plan (reject keys outside the active '
  + 'eras known set), which tests to flip vs keep-lenient (test_dijkstra_unknown_tx_body_key_skipped uses key 99 — keep lenient '
  + 'ONLY if 99 is a genuinely-future key Dijkstra bodyFields does NOT know; if Dijkstra also rejects 99, flip it), and confidence. '
  + 'Return the StructuredOutput.',
  { label: 'diagnose:31b', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
