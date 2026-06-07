export const meta = {
  name: 'diagnose-28-plutusdata-bytes',
  description: 'DIAGNOSE #28: does cardano-ledger/plutus reject >64-byte definite PlutusData bytestrings? Confirm Haskell + localize dugite fix',
  phases: [{ title: 'Diagnose', detail: 'source-confirm Haskell decodeData bytes rule + dugite gap + exact fix plan' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['is_real_gap', 'haskell_rule', 'haskell_source', 'dugite_gap', 'fix_plan', 'hash_consensus_impact', 'confidence', 'caveats'],
  properties: {
    is_real_gap: { type: 'boolean', description: 'true ONLY if Haskell genuinely REJECTS at decode what dugite ACCEPTS (a real strictness asymmetry)' },
    haskell_rule: { type: 'string', description: 'the exact decode rule: is a single definite-length bytestring >64 bytes accepted or rejected for PlutusData B? what about indef chunks >64? bignum mantissa?' },
    haskell_source: { type: 'string', description: 'canonical module/function + verbatim/paraphrased snippet (cardano-ledger Cardano.Ledger.Plutus.Data and/or plutus PlutusCore.Data decodeData / Codec.CBOR.Decoding decodeBytesIndefLen + the 64-byte bound)' },
    dugite_gap: { type: 'string', description: 'exactly where dugite is too lenient (file:line for Type::Bytes / BytesIndef / bignum mantissa across era_alonzo.rs + era_conway.rs + read_indef_bytes chunk validation)' },
    fix_plan: { type: 'string', description: 'the precise minimal change to match Haskell: cap definite bytes at 64? reject? validate indef chunk size? which sites?' },
    hash_consensus_impact: { type: 'string', description: 'does accepting the malformed form cause a real consensus/phase-1 asymmetry (dugite adopts a block/tx Haskell rejects)? or is it inert (hash over raw bytes unchanged)?' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  `DIAGNOSE dugite backlog #28 (a re-audit candidate, conf 0.6 — CONFIRM it is real before any fix; it may be a FALSE candidate).\n\n`
  + `CLAIM: dugite's PlutusData decoder accepts a single DEFINITE-length CBOR bytestring of >64 bytes (and possibly indef chunks `
  + `>64 bytes, and bignum tag-2/3 mantissa >64 bytes) with NO bound, whereas cardano-ledger/plutus REJECT such non-canonical `
  + `encodings at deserialization (CDDL plutus_data: bounded_bytes = bytes .size (0..64); the encoder chunks >64-byte bytestrings `
  + `into 64-byte indefinite chunks). If true, a crafted tx/datum is ACCEPTED by dugite but REJECTED by Haskell → phase-1 / `
  + `consensus acceptance asymmetry (dugite-node is adversarial-deployment software, default-to-reject).\n\n`
  + `dugite side (READ the real code): crates/dugite-serialization/src/decode/era_alonzo.rs:1282-1288 (Type::Bytes -> `
  + `read_bytes_owned, Type::BytesIndef -> read_indef_bytes, NO length check), era_conway.rs:2576-2579, bignum mantissa `
  + `era_alonzo.rs:1224/1230 + era_conway.rs:2514 via read_bigint, and the read_indef_bytes / read_bytes_owned implementations `
  + `(does read_indef_bytes validate each chunk <= 64 bytes?). Confirm the gap precisely with file:line.\n\n`
  + `Haskell side (THE CRUX — source-confirm, do NOT guess): determine how cardano-ledger decodes a PlutusData/Data 'B' `
  + `bytestring. Read IntersectMBO/cardano-ledger Cardano.Ledger.Plutus.Data (the Data / PlutusData decCBOR instance) AND `
  + `IntersectMBO/plutus PlutusCore.Data (decodeData) + how it uses Codec.CBOR.Decoding (decodeBytes / decodeBytesIndefLen / a `
  + `bounded variant). Decisively answer: (1) Is a SINGLE definite-length bytestring of >64 bytes ACCEPTED or REJECTED for a `
  + `PlutusData B node at decode? (2) For the indefinite chunked form, is each chunk required to be <= 64 bytes (and is a single `
  + `0-length or >64 chunk rejected)? (3) Same questions for the bignum (tag 2/3) mantissa bytestring. Use WebFetch on the `
  + `IntersectMBO repos + the in-project refs .claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md. `
  + `Quote the exact decoder.\n\n`
  + `Then: is_real_gap = true ONLY if Haskell genuinely rejects what dugite accepts. Give the minimal byte-exact fix plan + the `
  + `consensus/hash impact (note: the datum/script_data_hash is computed over the ORIGINAL wire bytes, so the hash itself is `
  + `unchanged — the issue is pure ACCEPTANCE asymmetry; confirm whether that actually causes a consensus split or is inert). `
  + `Return the StructuredOutput.`,
  { label: 'diagnose:28', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
