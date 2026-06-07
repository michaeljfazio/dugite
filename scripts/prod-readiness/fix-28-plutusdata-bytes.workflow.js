export const meta = {
  name: 'fix-28-plutusdata-bytes',
  description: 'FIXING #28: bound PlutusData leaf bytestrings at 64 bytes (decodeBoundedBytes parity), scoped to PlutusData decode arms only',
  phases: [{ title: 'Fix', detail: 'add read_bounded_plutus_bytes + apply at PlutusData leaves; defensive tests; over-strictness guard' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['files_changed', 'diff_summary', 'bignum_handling', 'scope_guard', 'tests_added', 'checks', 'caveats', 'completed'],
  properties: {
    files_changed: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    bignum_handling: { type: 'string', description: 'how the tag-2/3 mantissa bound was applied; whether read_bigint is PlutusData-only or shared, and how that was handled without over-stricting non-Plutus callers' },
    scope_guard: { type: 'string', description: 'evidence that generic read_bytes_owned/read_indef_bytes (vkeys/scripts/addresses/asset-names) were NOT bounded' },
    tests_added: { type: 'string' },
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
  `Implement dugite backlog #28 in the MAIN working tree (do NOT create a worktree; do NOT git commit). Single crate: `
  + `dugite-serialization.\n\n`
  + `GOAL (byte-exact parity with plutus PlutusCore.Data.decodeData, Note [The 64-byte limit]): every PlutusData LEAF bytestring `
  + `must be capped at 64 bytes at DECODE and REJECTED above. Specifically — mirror Haskell decodeBoundedBytes / `
  + `decodeBoundedBytesIndefLen:\n`
  + `  - A single DEFINITE-length bytestring leaf > 64 bytes => Err.\n`
  + `  - The INDEFINITE chunked form: EACH single chunk must be <= 64 bytes (reject any one chunk > 64); the CONCATENATED TOTAL `
  + `MAY exceed 64 across multiple <=64 chunks (do NOT bound the total). A 0-length chunk is allowed.\n`
  + `  - The BIGNUM (CBOR tag 2 / tag 3) MANTISSA bytestring is also a leaf: bound it the same way (definite >64 => Err; each `
  + `indef chunk >64 => Err).\n\n`
  + `EXACT SITES (read them first): crates/dugite-serialization/src/decode/era_alonzo.rs read_plutus_data_depth Type::Bytes `
  + `(~:1283, read_bytes_owned) + Type::BytesIndef (~:1287, read_indef_bytes) + the bignum tag-2/3 mantissa reads (~:1224/:1230); `
  + `crates/dugite-serialization/src/decode/era_conway.rs the Type::Bytes|Type::BytesIndef arm (~:2576-2578) + the bignum path `
  + `(~:2514, currently via reader.rs read_bigint). Underlying primitives: reader.rs read_bytes_owned (~:399), read_indef_bytes `
  + `(~:426, chunk loop ~:446-449), read_bigint (~:507, mantissa via read_bytes_owned ~:520/:524).\n\n`
  + `IMPLEMENTATION:\n`
  + `  1. Add a helper (in era_alonzo.rs, shared by Conway, or a small module) read_bounded_plutus_bytes(r) that: peeks the type; `
  + `for a definite bytestring reads it and returns Err if len > 64; for the indefinite form walks chunks via the reader and `
  + `returns Err if ANY single chunk len > 64, concatenating otherwise (total unbounded). Use a clear error like `
  + `"PlutusData ByteString leaf exceeds 64 bytes".\n`
  + `  2. Replace the PlutusData Type::Bytes / Type::BytesIndef arms in BOTH era_alonzo.rs and era_conway.rs read_plutus_data `
  + `paths to call read_bounded_plutus_bytes.\n`
  + `  3. BIGNUM mantissa: bound the tag-2/3 mantissa leaf the same way in the PlutusData Integer path for BOTH eras. FIRST `
  + `CHECK whether reader.rs read_bigint is used ONLY for PlutusData or ALSO by non-Plutus callers (grep usages). If read_bigint `
  + `is shared with non-Plutus callers, DO NOT add the 64-byte bound inside read_bigint; instead, in the PlutusData decode path, `
  + `consume the bignum tag locally and read the mantissa via read_bounded_plutus_bytes (or add a separate read_bounded_plutus_`
  + `bigint used ONLY by the PlutusData arms). If read_bigint is PlutusData-only, bounding it directly is acceptable — but state `
  + `which, with the grep evidence.\n\n`
  + `*** SCOPE GUARD (CRITICAL — over-strictness is a REGRESSION): do NOT add the 64-byte bound to the generic reader.rs `
  + `read_bytes_owned / read_indef_bytes themselves — they serve NON-Plutus callers (Ed25519 vkeys 32B, KES/VRF, NATIVE+Plutus `
  + `SCRIPT bytes which exceed 64B, addresses, asset names up to 32B, metadata, etc.) that are NOT subject to the plutus 64-byte `
  + `rule. Only the PlutusData leaf arms get bounded. In scope_guard, give grep evidence that those generic readers are unchanged `
  + `and still used by the non-Plutus sites.\n\n`
  + `TESTS (defensive, #538/#539 pattern): add unit + a length-lattice proptest covering: PlutusData definite bytes len 64 => Ok, `
  + `65 => Err; indef single chunk 64 => Ok, 65 => Err; TWO 64-byte chunks (total 128) => Ok (total unbounded); 0-length chunk `
  + `=> Ok; bignum mantissa 64 => Ok, 65 (definite and indef chunk) => Err. AND an OVER-STRICTNESS GUARD test: a >64-byte NON-`
  + `Plutus bytestring (e.g. a Plutus script blob, or a generic read_bytes_owned call) is STILL accepted (decode succeeds). If a `
  + `fuzz target for plutus_data decode exists, extend it; else add a small one if the crate has a fuzz harness.\n\n`
  + `BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-serialization --all-targets -- -D warnings ; cargo nextest run `
  + `-p dugite-serialization. Report each pass/fail. Remember: green tests are NOT byte-exact proof — a separate gauntlet (incl. `
  + `an over-strictness lens) follows; your job is a correct, scoped, test-covered implementation matching decodeBoundedBytes. `
  + `completed=true ONLY if applied at all PlutusData leaf sites (both eras, incl. bignum mantissa), scope guard verified, tests `
  + `added, and fmt+clippy+nextest green. Do NOT commit.`,
  { label: 'fix:28', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
