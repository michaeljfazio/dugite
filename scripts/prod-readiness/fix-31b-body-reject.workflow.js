export const meta = {
  name: 'fix-31b-body-reject',
  description: 'FIXING #31-B: era-aware reject of unknown tx-body keys (Conway vs Dijkstra known-key sets), matching Haskell bodyFields invalidField',
  phases: [{ title: 'Fix', detail: 'thread era; guard Dijkstra-only keys; delete key-6 skip; era-aware reject; tests' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['sites_changed', 'era_threaded', 'guards_added', 'key6_deleted', 'tests', 'over_rejection_guard', 'checks', 'caveats', 'completed'],
  properties: {
    sites_changed: { type: 'array', items: { type: 'string' } },
    era_threaded: { type: 'string', description: 'how the era param was added + threaded from every caller (block decoder KeepRaw::parse_with closure, standalone, dijkstra standalone, test callers)' },
    guards_added: { type: 'string', description: 'the 23/25/26 arms guarded with `if era == Era::Dijkstra` so Conway falls through to reject' },
    key6_deleted: { type: 'boolean', description: 'the `6 => r.skip()` arm was DELETED (key 6 now hard-rejected by both eras)' },
    tests: { type: 'string', description: 'flipped test_dijkstra_unknown_tx_body_key_skipped (key99->reject) + added Conway-rejects-23/25/26 + Dijkstra-accepts-23/25/26 + Conway-rejects-key6 tests' },
    over_rejection_guard: { type: 'string', description: 'evidence NO valid key is over-rejected: Conway accepts exactly {0,1,2,3,4,5,7,8,9,11,13..22}; Dijkstra adds {23,25,26}; honest blocks decode unchanged' },
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
  'Implement dugite backlog #31-B (era-aware tx-BODY unknown-key reject) in the MAIN working tree (no worktree; do NOT git '
  + 'commit). Single crate: dugite-serialization, single file crates/dugite-serialization/src/decode/era_conway.rs '
  + '(decode_conway_tx_body, ~line 448). #31-A (witness-set) is already committed; this is the SEPARATE tx-body part.\n\n'
  + 'SOURCE-CONFIRMED (diagnose w075p3s3n, conf 0.95, permalink-pinned cardano-ledger cd8b7fab): Haskell decodes the tx body via '
  + 'SparseKeyed with field-picker bodyFields; the catch-all hard-fails unknown keys (Conway: bodyFields n = invalidField n -> '
  + 'cborError; Dijkstra v12+: decoderByKey _ -> Nothing -> failMsg, NO version-gate). EACH ERA knows only its OWN keys. EXACT '
  + 'known top-level tx-body keys:\n'
  + '  CONWAY = {0,1,2,3,4,5,7,8,9,11,13,14,15,16,17,18,19,20,21,22}. REJECTED gaps: 6,10,12, and everything >=23.\n'
  + '  DIJKSTRA = CONWAY plus {23,25,26} (23=sub_transactions, 25=direct_deposits, 26=account_balance_intervals). Still rejects '
  + '6,10,12,24,99. (Key 14 is accepted by both — Conway reqSignerHashes, Dijkstra guards — same key number, no guard needed.)\n'
  + 'So a CONWAY body must REJECT 6,10,12,23,25,26,...; a DIJKSTRA body must REJECT 6,10,12,24,...(>=27),99.\n\n'
  + 'dugite currently: decode_conway_tx_body (no era param) handles keys including 5 (account_balance_intervals) and explicit '
  + 'arms for 23/25/26, with `6 => { r.skip()?; }` (~:665-668) and a lenient default `_ => { r.skip()?; }` (~:669-672, comment '
  + '"skip for forward compatibility (Dijkstra may add keys)"). The ONE decoder serves BOTH Conway and Dijkstra.\n\n'
  + 'THE FIX (exactly this):\n'
  + '  1. Add an era param: change `fn decode_conway_tx_body(r: &mut Reader<\'_>)` to '
  + '`fn decode_conway_tx_body(r: &mut Reader<\'_>, era: Era)` (Era is dugite_primitives::era::Era, already imported; ensure it '
  + 'derives PartialEq for the == compare — it does). Thread `era` from EVERY caller:\n'
  + '     - the block decoder ~:177 `KeepRaw::parse_with(r, decode_conway_tx_body)` -> '
  + '`KeepRaw::parse_with(r, |r| decode_conway_tx_body(r, era))` (era is in scope from decode_conway_block_mode);\n'
  + '     - decode_conway_tx_standalone ~:2793 -> pass its `era`;\n'
  + '     - decode_dijkstra_tx_standalone ~:2891 -> pass `Era::Dijkstra`;\n'
  + '     - test call sites (~:3305, ~:3449) -> pass the era the test intends (Era::Conway unless it is a Dijkstra test).\n'
  + '  2. GUARD the Dijkstra-only arms so Conway rejects them: change the `23 => ...`, `25 => ...`, `26 => ...` match arms to '
  + '`23 if era == Era::Dijkstra => ...`, `25 if era == Era::Dijkstra => ...`, `26 if era == Era::Dijkstra => ...`. (For Conway '
  + 'these now fall through to the rejecting default.)\n'
  + '  3. DELETE the `6 => { r.skip()?; }` arm entirely — key 6 (pre-Conway `update`) is NOT in Conway OR Dijkstra bodyFields and '
  + 'is HARD-REJECTED by both (the comment "skip in Conway" is WRONG per raw Haskell; let key 6 fall through to the rejecting '
  + 'default).\n'
  + '  4. Replace the lenient default `_ => { r.skip()?; }` with an era-aware REJECT: '
  + '`_ => { return Err(SerializationError::CborDecode(format!("{era:?} tx body: unknown/invalid key {key}"))); }`. Because the '
  + '23/25/26 arms are now Dijkstra-guarded, this default correctly rejects 23/25/26 for Conway AND 6/10/12/24/99/etc for both.\n\n'
  + '*** OVER-REJECTION GUARD (CRITICAL — over-rejecting a valid key is a CONSENSUS BREAK): verify the explicit accept-arms cover '
  + 'EXACTLY the Conway set {0,1,2,3,4,5,7,8,9,11,13..22} (shared) plus the Dijkstra-guarded {23,25,26}. Honest Conway/Dijkstra '
  + 'blocks must still decode. Confirm no currently-handled VALID key was turned into a reject.\n\n'
  + 'TESTS: (a) FLIP test_dijkstra_unknown_tx_body_key_skipped (~:3440, uses key 99) -> rename *_rejected, call '
  + 'decode_conway_tx_body(&mut r, Era::Dijkstra), assert result.is_err(). (b) ADD: conway_tx_body_rejects_dijkstra_only_keys '
  + '(a Conway body with key 23/25/26 -> Err); dijkstra_tx_body_accepts_23_25_26 (a Dijkstra body with those -> Ok); '
  + 'conway_tx_body_rejects_key6 (key 6 -> Err in both eras). Keep cost_models_unknown_keys_ignored + pparam_update_unknown_key_'
  + 'skipped UNTOUCHED.\n\n'
  + 'BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-serialization --all-targets -- -D warnings ; cargo nextest run '
  + '-p dugite-serialization. Report each pass/fail. completed=true ONLY if era threaded everywhere, guards added, key-6 deleted, '
  + 'default rejects era-aware, tests added/flipped, and fmt+clippy+nextest green. Green tests are NOT byte-exact proof — a '
  + 'gauntlet follows. Do NOT commit.',
  { label: 'fix:31b', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
