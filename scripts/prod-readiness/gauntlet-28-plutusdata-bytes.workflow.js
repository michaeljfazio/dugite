export const meta = {
  name: 'gauntlet-28-plutusdata-bytes',
  description: 'Refutation panel for the #28 PlutusData 64-byte leaf cap before commit (exact-match, over-strictness/completeness, encoder-consistency)',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #28 decode-bound fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong/incomplete/over-strict OR commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  `Fix under test (UNCOMMITTED in the main tree; dugite backlog #28). Read the ACTUAL current code: git diff on `
  + `crates/dugite-serialization/src/decode/{reader.rs,era_alonzo.rs,era_conway.rs}, and the diagnose artifact context below.\n\n`
  + `WHAT IT DOES: caps every PlutusData LEAF bytestring at 64 bytes at CBOR DECODE, matching Haskell plutus PlutusCore.Data `
  + `decodeBoundedBytes (Note [The 64-byte limit]). reader.rs ADDS (additive, 0 removals) read_bounded_plutus_bytes (definite `
  + `>64 => Err; indefinite: EACH single chunk must be <=64 => Err on any chunk >64, but the concatenated TOTAL is UNBOUNDED; `
  + `0-length chunk OK) and read_bounded_plutus_bigint (PlutusData-only tag-2/3 bignum: mantissa via the bounded reader). The `
  + `generic Reader::read_bytes_owned / read_indef_bytes / read_bigint are UNCHANGED and still serve non-Plutus callers `
  + `(Ed25519/KES/VRF vkeys, addresses, asset names, native+Plutus SCRIPT blobs >64B, metadata). era_alonzo.rs + era_conway.rs `
  + `read_plutus_data_depth route their Bytes/BytesIndef + bignum-mantissa arms through the bounded helpers. Babbage reuses `
  + `Alonzo's read_plutus_data (no own copy). 23 defensive tests pass; fmt+clippy+nextest 1175/1175 green.\n\n`
  + `CANONICAL HASKELL (source-confirmed, conf 0.95; attack if you can): decodeData dispatches TypeBytes -> decodeBoundedBytes `
  + `(unless BS.length b <= 64 then fail), TypeBytesIndef -> decodeBoundedBytesIndef (decodeBoundedBytes PER CHUNK; total may `
  + `exceed 64), bignum via decodeBoundedBigInteger -> decodeBoundedBytes. Enforced at CBOR decode (DecCBOR (PlutusData era), `
  + `inline BinaryData makeBinaryData, redeemers) BEFORE any ledger rule. KNOWN FOLLOW-UP (tracked as #28b, NOT this fix): the `
  + `dugite ENCODER emits a single definite bstr for >64B leaves (no chunking like encodeBoundedBytes).`

const LENSES = [
  {
    key: 'haskell-exact-match',
    prompt: `LENS: exact decodeBoundedBytes match. Read read_bounded_plutus_bytes + read_bounded_plutus_bigint. Does the bound match `
      + `Haskell BYTE-FOR-BYTE? Check: (a) boundary is INCLUSIVE — len==64 accepted, len==65 rejected (not off-by-one, not <64); `
      + `(b) indefinite form rejects ANY single chunk >64 anywhere in the stream (not just the first), and ACCEPTS a multi-chunk `
      + `total >64 (e.g. two 64-chunks = 128); (c) 0-length chunk accepted; (d) bignum tag-2 AND tag-3 mantissa both bounded, `
      + `definite and per-indef-chunk; (e) does dugite accept a non-canonical encoding Haskell would reject, or reject one Haskell `
      + `accepts (e.g. a definite bstr <=64 must still be accepted; does the indef reader wrongly accept a definite >64 nested `
      + `somewhere, or wrongly reject the canonical chunked form)? If any deviation from decodeBoundedBytes, refuted=true.`,
  },
  {
    key: 'overstrict-completeness',
    prompt: `LENS: over-strictness + completeness across ALL eras + carriers. (1) OVER-STRICTNESS: confirm NO generic/non-Plutus byte `
      + `reader got bounded — grep that read_bytes_owned/read_indef_bytes/read_bigint bodies are unchanged and still used for `
      + `vkeys/KES/VRF/addresses/asset-names/SCRIPT blobs (which legitimately exceed 64B). A >64B native/Plutus script or a >64B `
      + `metadata bytestring MUST still decode. If the fix over-rejects any non-Plutus >64B bytestring, refuted=true. (2) `
      + `COMPLETENESS: are ALL PlutusData leaf decode paths bounded? Check every era's read_plutus_data entry — Alonzo, Babbage `
      + `(reuse), Conway, AND any DIJKSTRA / newest-era decoder (does it have its OWN read_plutus_data that is still unbounded?). `
      + `Check every CARRIER: witness-set datums, inline datums (Babbage/Conway), redeemer data/redeemer-map, auxiliary-data/`
      + `metadata-embedded Data. If any PlutusData leaf carrier in any era still decodes via an UNBOUNDED path, refuted=true.`,
  },
  {
    key: 'encoder-consistency-commit-safety',
    prompt: `LENS: is committing the DECODE bound ALONE (without the #28b encoder-chunking fix) SAFE, or does it break a real dugite `
      + `self-path? The dugite encoder emits a single definite bstr for >64B PlutusData leaves. Trace whether dugite ever DECODES `
      + `its OWN re-encoded PlutusData in a hot path that would now fail: block forging (does the forge path re-decode the block it `
      + `built before broadcast?), mempool admission of a locally-built tx, ledger-snapshot serialization round-trip, or any `
      + `encode-then-decode loop. Distinguish: (a) a >64B PlutusData leaf can only arise from a USER-submitted datum/redeemer (and `
      + `Haskell would reject it at mempool too) OR a dugite-internal re-encode. If committing #28 alone breaks an honest dugite `
      + `self-operation (e.g. forging a normal block, or round-tripping a <=64B-leaf datum), refuted=true. If it only affects a `
      + `>64B-leaf datum (adversarial / requires the #28b encoder feature, correctly tracked), NOT refuted — but say so explicitly. `
      + `Also confirm a <=64B-leaf datum (the overwhelmingly common case) still round-trips encode->decode fine.`,
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent(`Adversarially REFUTE the #28 fix via this lens. Default refuted=true if uncertain. Read the real current code before deciding.\n\n${CONTEXT}\n\n${l.prompt}`,
      { label: `refute:${l.key}`, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
