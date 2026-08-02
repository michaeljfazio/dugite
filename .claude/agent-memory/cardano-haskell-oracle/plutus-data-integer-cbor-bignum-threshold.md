---
name: plutus-data-integer-cbor-bignum-threshold
description: Exact word64-based threshold where PlutusCore.Data's I (integer) leaf switches from plain CBOR int to tag-2/3 bignum; decode leniency; found dugite bug (i128 gate + truncating cast)
metadata:
  type: reference
---

## Source

`plutus-core/plutus-core/src/PlutusCore/Data.hs` (IntersectMBO/plutus, master @ 3e257708aea5705074ae5a9687e0d97d66a954f2). Threshold logic last touched structurally by "Reuse bitwise primitives for Data" (#7618, commit d91c23ded9, 2026-02-24) — that commit ONLY swapped the hand-rolled `integerToBytes` for `PlutusCore.Bitwise.integerToBytesBE`; the two `encodeInteger` guard clauses (the actual threshold) are byte-for-byte unchanged from the original `-- Taken exactly from Codec.CBOR.Write` version. This is old, stable, foundational code — safe to treat as current for any recent cardano-node release.

## `encodeData`'s `I` arm (line 165)

```haskell
encodeData = \case
  ...
  I i -> encodeInteger i
  B b -> encodeBs b
```

## `encodeInteger` (lines 170-182) — THE THRESHOLD

```haskell
encodeInteger :: Integer -> Encoding
encodeInteger i
  | i >= 0, i <= fromIntegral (maxBound :: Word64) = CBOR.encodeInteger i
encodeInteger i
  | i < 0, i >= -1 - fromIntegral (maxBound :: Word64) = CBOR.encodeInteger i
encodeInteger i
  | i >= 0 = CBOR.encodeTag 2 <> encodeBs (integerToBytesBE i)
  | otherwise = CBOR.encodeTag 3 <> encodeBs (integerToBytesBE (-1 - i))
```

**Exact inclusive plain-integer range: `[-(2^64) .. 2^64 - 1]`.**

- Upper bound: `maxBound :: Word64` = `2^64 - 1`.
- Lower bound: `-1 - (2^64 - 1)` = **`-2^64`** exactly (NOT `-2^64 - 1`). Confirms the commonly-assumed bound precisely — the arithmetic literally is `-1 - maxWord64`, which equals `-(2^64)`, not one further out.
- This matches CBOR's own native range for a `Word64`-argument major-type-0/1 integer (major type 1 argument `n` encodes value `-(1+n)`, and `n` maxes out at `2^64-1`, giving `-(2^64)`).

Outside that range: bignum path.
- `i >= 2^64` → tag **2**, magnitude = `integerToBytesBE i` (i.e. `i` itself, big-endian, no sign).
- `i <= -(2^64) - 1` → tag **3**, magnitude = `integerToBytesBE (-1 - i)` (matches the general CBOR negative-bignum rule `value = -(1 + magnitude)`).

`integerToBytesBE` (`PlutusCore/Bitwise.hs:304-307`): `integerToBytesBE 0 = BS.pack [0]`; otherwise `unsafeIntegerToByteString BigEndian 0 n` — minimal-length big-endian, no padding (width arg `0` = "as many bytes as needed"). Irrelevant here in practice since bignum-path magnitudes are always `>= 2^64`, never zero.

## 64-byte chunking (Note "Evading the 64-byte limit")

The bignum magnitude is fed through the exact same `encodeBs` used for `Data`'s `B` (ByteString) leaf — **not a separate/parallel implementation**:

```haskell
encodeInteger i
  | otherwise = CBOR.encodeTag 3 <> encodeBs (integerToBytesBE (-1 - i))
...
encodeBs :: BS.ByteString -> Encoding
encodeBs b | BS.length b <= 64 = CBOR.encodeBytes b
encodeBs b = CBOR.encodeBytesIndef <> foldMap encode (to64ByteChunks b) <> CBOR.encodeBreak
```

So magnitudes ≤64 bytes → plain definite bytestring; >64 bytes → indefinite-length bytestring, 64-byte chunks (`to64ByteChunks`, lines 193-198). A magnitude that size is astronomically larger than any real Plutus value (`2^512`+), but the rule is unconditional and shared code, not special-cased.

## Decode leniency (non-canonical bignum acceptance)

`decodeData`'s outer dispatch (`CBOR.peekTokenType`) routes CBOR items tagged 2/3 to the cborg-classified `TypeInteger` pseudo-type, which calls `decodeBoundedBigInteger` (lines 232-245):

```haskell
decodeBoundedBigInteger = do
  tag <- CBOR.decodeTag
  bs <- ... decodeBoundedBytes / decodeBoundedBytesIndef ...
  case tag of
    2 -> pure $ CBOR.uintegerFromBytes bs
    3 -> pure $ CBOR.nintegerFromBytes bs
    t -> fail ("Bignum tag must be one of 2 or 3, got: " ++ show t)
```

**No minimality/canonicality check exists anywhere in this path.** The only constraint enforced is the 64-byte-per-chunk bound (`decodeBoundedBytes`/`decodeBoundedBytesIndef`). There is no check that the decoded magnitude is `>= 2^64` (i.e. that a bignum encoding was actually *necessary*). **Conclusion: `decodeData` ACCEPTS a tag-2/tag-3 bignum encoding of a value that would have fit in the plain range** (e.g. tag `2` + 1-byte payload `0x05` decodes to `I 5` without error) — decode is lenient/non-canonical-accepting even though `encodeData` itself is always canonical (deterministic, single encoding per value). This asymmetry matters for anyone doing hash-consing or canonical-form assumptions on decoded `Data` from untrusted input (e.g. redeemer/datum bytes arriving over the wire) — decode does NOT reject non-canonical bignum framing, so byte-exact round-trip (decode→re-encode) is not guaranteed to reproduce the original bytes, but Cardano's `script_data_hash` hashes the ORIGINAL received bytes anyway (see [[variable-length-cbor-framing-and-blockbody-hash-over-original-bytes]] pattern), so this is not itself a validation hazard for dugite as long as dugite also hashes original bytes rather than re-encoding.

## Dugite bug found during this consult (2026-08-01, unfixed as of this memory)

`crates/dugite-serialization/src/cbor.rs`:
- `encode_plutus_int` (line ~124) gates the bignum path on `value.to_i128()` succeeding (i128 range ±~1.7e38) — far wider than the true Haskell threshold (`u64`-based, ±~1.8e19 magnitude).
- Values with `to_i128()` = `Some` but magnitude `> u64::MAX` (e.g. `123456789012345678901234567890`, ~1.2e29) incorrectly take the "plain int" branch, calling `encode_int(v: i128)` (line 148), which does `encode_uint(value as u64)` — an unchecked truncating cast. Result: silent mod-2^64 wraparound (`123456789012345678901234567890` → `14083847773837265618`) instead of the required tag-2 bignum encoding.
- The 64-byte chunking helper (`encode_bounded_plutus_bytes`, line 356, `PLUTUS_DATA_BYTES_LEAF_MAX = 64` in `decode/reader.rs:687`) IS already correct and already shared with the `B`-leaf path — only the plain-vs-bignum threshold gate is wrong.
- Correct fix: gate on the BigInt directly against the u64-based bounds (mirroring Haskell's two guards exactly), not `to_i128()`:
  - non-negative plain iff `value.to_u64().is_some()` (i.e. `value <= u64::MAX`)
  - negative plain iff `(-value - 1).to_u64().is_some()` (i.e. `value >= -(2^64)`)
  - otherwise bignum (tag 2/3), unchanged.
- This feeds `script_data_hash` — any transaction with a Datum/Redeemer integer in `(2^64, i128::MAX]` or `[i128::MIN, -(2^64)-1)` produces a wrong hash today. Not yet fixed at time of writing.
- Worth auditing: `crates/dugite-uplc/src/builtin/denotations.rs` (lines ~1350, 1385, 1496) and `syn/parser.rs:699` also gate on `BigInt::to_i128()` — check whether any of those paths similarly need bignum-vs-plain CBOR framing (most look like arithmetic/ExUnits contexts, not CBOR encoding, but not exhaustively checked in this consult).
