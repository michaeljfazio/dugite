export const meta = {
  name: 'gauntlet-31b-body',
  description: 'Refutation panel for #31-B (era-aware tx-body unknown-key reject) before commit: per-era key-set match, over-rejection, commit-safety',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #31-B fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/over-rejecting/incomplete OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #31-B). Read the ACTUAL current code: git diff on '
  + 'crates/dugite-serialization/src/decode/era_conway.rs (decode_conway_tx_body).\n\n'
  + 'WHAT IT DOES: dugite has ONE decode_conway_tx_body serving BOTH Conway and Dijkstra. It now takes an `era: Era` param; the '
  + 'lenient default `_ => { r.skip()?; }` and the `6 => { r.skip()?; }` arm were replaced with an ERA-AWARE REJECT '
  + '(`return Err(CborDecode(format!("{era:?} tx body: unknown/invalid key {key}")))`), and the Dijkstra-only arms 23/25/26 were '
  + 'guarded with `if era == Era::Dijkstra` so Conway falls through to reject them. era is threaded from the block decoder '
  + '(KeepRaw::parse_with closure), the conway/dijkstra standalone decoders (dijkstra passes Era::Dijkstra), and test callers.\n\n'
  + 'CANONICAL HASKELL (diagnose w075p3s3n, conf 0.95, permalink-pinned cardano-ledger cd8b7fab — RE-CONFIRM independently): tx '
  + 'body decoded via SparseKeyed with field-picker bodyFields; catch-all hard-fails unknown keys (Conway bodyFields n = '
  + 'invalidField n -> cborError; Dijkstra v12+ decoderByKey _ -> Nothing -> failMsg, NO version-gate). EACH ERA knows only its '
  + 'OWN keys. EXACT known top-level body keys: CONWAY = {0,1,2,3,4,5,7,8,9,11,13,14,15,16,17,18,19,20,21,22} (gaps 6,10,12,>=23 '
  + 'rejected — key 6 pre-Conway update is HARD-REJECTED not skipped); DIJKSTRA = Conway plus {23,25,26}.\n\n'
  + 'GREEN STATUS (engine-verified): only era_conway.rs changed; fmt+clippy+nextest 1179/1179; real-blocks suite passes '
  + '(test_conway/alonzo/babbage/mary/shelley_block + test_decode_block_dijkstra_native_dispatch); new tests '
  + '(conway_tx_body_rejects_dijkstra_only_keys, dijkstra_tx_body_accepts_23_25_26, conway_tx_body_rejects_key6, '
  + 'dijkstra_unknown_key99_rejected) pass; cost_models_unknown_keys_ignored + pparam_update_unknown_key_skipped untouched.'

const LENSES = [
  {
    key: 'per-era-key-set-match',
    prompt: 'LENS: are the EXACT per-era body-key sets correct vs Haskell? PERMALINK-RECONFIRM independently (raw source — #31-A '
      + 'caught a WebFetch hallucination, so READ the actual cardano-ledger Conway TxBody.hs bodyFields AND Dijkstra TxBody.hs '
      + 'decoderByKey). Verify: (a) Conway known keys = EXACTLY {0,1,2,3,4,5,7,8,9,11,13..22}, nothing more/less; (b) Dijkstra adds '
      + 'EXACTLY {23,25,26} (and key 24 is SubTx-level only, not top-level); (c) key 6 is genuinely NOT in Conway/Dijkstra '
      + 'bodyFields and is HARD-REJECTED (the fix deletes the skip) — confirm against raw source; (d) the catch-all hard-fails '
      + '(invalidField->cborError / decoderByKey Nothing->failMsg) with NO version-gate / forward-compat in any era (re-read '
      + 'decodeSparseKeyed Decoder.hs). If any key is in/out of the wrong era set, or key 6 is actually accepted somewhere, or '
      + 'there is a forward-compat skip, refuted=true.',
  },
  {
    key: 'over-rejection',
    prompt: 'LENS: over-rejection (a consensus BREAK if any VALID key is now rejected). Read the diff. (a) Confirm the explicit '
      + 'accept-arms cover EXACTLY the Conway set {0,1,2,3,4,5,7,8,9,11,13..22} unconditionally + 23/25/26 guarded by '
      + 'era==Dijkstra — no valid key dropped. (b) The match-guards: for Conway, do 23/25/26 correctly FALL THROUGH to the '
      + 'rejecting default (not silently matched/skipped)? for Dijkstra, are 23/25/26 ACCEPTED? (c) Is `era` threaded at EVERY '
      + 'caller — could any path call the body decoder with the WRONG era (e.g. a Conway-era block decoded with Era::Dijkstra or '
      + 'vice versa, leading to wrong accept/reject)? Check decode_conway_block_mode passes the right era, and that no caller was '
      + 'missed (un-threaded -> compile error, but verify the threaded value is correct per path). (d) Real blocks decode '
      + 'unchanged (the real-blocks suite passes). If any valid key is over-rejected, a guard is wrong, or a caller passes the '
      + 'wrong era, refuted=true.',
  },
  {
    key: 'commit-safety',
    prompt: 'LENS: is committing #31-B safe? (a) key-6 reject: does ANY real Conway or Dijkstra block/tx ever carry body key 6? '
      + '(key 6 = pre-Conway `update`; Conway/Dijkstra encoders never emit it, so no honest block has it -> rejecting it only '
      + 'affects adversarial txs Haskell also rejects). Confirm no honest-chain impact. (b) Dijkstra is UNRELEASED (pre-PV12) — '
      + 'its TxBody key set {+23,25,26} could change before activation. Is committing the Dijkstra reject safe NOW? (Dijkstra is '
      + 'not active on any network, so there is zero live block-ingestion impact; if the key set changes pre-activation dugite '
      + 'updates then. Conway is mainnet-live and stable — the higher-stakes path is correct.) (c) Is the fix a strict '
      + 'improvement — it only rejects unknown/out-of-era keys that Haskell ALSO rejects at decode (a #539-class consensus gap '
      + 'closure), with honest blocks decoding identically? Refuted=true ONLY if committing breaks honest-chain decode or the '
      + 'Dijkstra-unreleased risk is actually a present danger; a correctly-tracked future-key risk that cannot fire today is NOT '
      + 'a refutation.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the #31-B fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
