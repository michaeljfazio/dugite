---
name: plutus-builtins-adversarial-audit
description: Byte-exact contracts for Trace/SliceByteString/ConstrData/VerifyEd25519/VerifyEcdsaSecp256k1/ExpModInteger/DropList vs adversarial inputs, with BuiltinSemanticsVariant PV gating
type: reference
---

Source-verified 2026-07-04 against IntersectMBO/plutus master + IntersectMBO/cardano-base master + bitcoin-core/secp256k1 master + IntersectMBO/libsodium master. All file paths relative to repo root.

## BuiltinSemanticsVariant <-> protocol-version mapping (THE key table)
File: `plutus-ledger-api/src/PlutusLedgerApi/Common/ProtocolVersions.hs` (constants) +
`plutus-ledger-api/src/PlutusLedgerApi/MachineParameters.hs` (`machineParametersFor`).

```
pv:        pre-Conway(<9)  post-Conway,pre-vanRossem(9,10)  post-vanRossem(>=11)
PlutusV1:       A                    B                              D
PlutusV2:       A                    B                              D
PlutusV3:      (n/a)                 C                              E
```
- `changPV = 9` (Chang HF, Conway + PlutusV3 introduced)
- `plominPV = 10` (intra-Conway HF, new builtins in V2/V3)
- `vanRossemPV = 11` (intra-Conway HF) — **dugite's preview testnet is ALREADY at PV11 (per CLAUDE.md), meaning variant D/E semantics are LIVE now, not future.**
- `ensurable :: BuiltinSemanticsVariant DefaultFun -> Bool` (Builtins.hs ~L2725) = `True` only for D/E. Gates two DIFFERENT kinds of things: (a) pure perf/representation swap (`CInteger`/`CByteString` newtype wrappers used by AddInteger/SubtractInteger/MultiplyInteger/ConsByteString/SliceByteString/IndexByteString/Blake2b_256/VerifyEd25519Signature/VerifySchnorrSecp256k1Signature — NO bounds/semantic change, same Int/Integer bound checks either branch); (b) **genuine semantic tightening for ConstrData only** (see below).

## 1. Trace — non-Text first arg
`toBuiltinMeaning _semvar Trace` (Builtins.hs ~L1622): `Text -> a -> BuiltinResult a`, denotation `\text a -> a <$ emit text`. Single impl, no semvar dispatch ("constant cost, no variant dispatch needed").
Unlifting is **deferred to full saturation** (Note in `plutus-core/plutus-core/src/PlutusCore/Builtin/Meaning.hs` ~L232-333: "operationally deferred unlifting"/"call-by-name unlifting" — `ReadKnownM = Either BuiltinError`, the whole arg-unlift chain only runs once ALL args of a builtin are supplied). Since `trace` takes exactly 2 args, `trace (con integer 5) x` IS fully saturated, so unlifting runs immediately: `readKnownConstant` (`PlutusCore/Builtin/KnownType.hs` ~L310-323) compares the constant's universe tag (`DefaultUniInteger`) against the expected tag (`DefaultUniString`) via `geqL`; mismatch -> `throwError . BuiltinUnliftingEvaluationError $ typeMismatchError uniExp uniAct`, classified as **StructuralError**. Net: **`trace (con integer 5) x` is an evaluation FAILURE** (script fails), it does NOT fall through and return `x`. (Opaque/polymorphic 2nd arg has a no-op ReadKnown so it never masks the 1st-arg failure — the `Either` monad short-circuits on `x1 <- readKnown arg1` before ever touching arg2.)

## 2. SliceByteString / IndexByteString — Int, not Integer, at Haskell level
Denotation type is literally `Int -> Int -> ByteString -> ByteString` / `... -> Int -> BuiltinResult Word8` (Builtins.hs ~L1301-1342), a **pure, non-failing** function for SliceByteString (`BS.take n (BS.drop start xs)` — Haskell `Data.ByteString.take/drop` themselves clamp negative/overlong indices, no exception).
BUT: the PLC-visible argument type is `integer` (DefaultUni has no Int). The `Int` Haskell type is unlifted from an `Integer` constant via `PlutusCore/Default/Universe.hs` ~L576-585: on 64-bit (`WORD_SIZE_IN_BITS==64`, the only supported node target) `readKnown @Int` = `readKnownAsInteger` bounds-checked against `(minBound::Int64, maxBound::Int64)` (~L553-574), throwing `operationalUnliftingError` ("... is not within the bounds of Int") if out of `[-2^63, 2^63-1]`. **So `sliceByteString (2^64) 1 bs` FAILS at the unlifting stage (evaluation failure) — it never reaches the pure `take`/`drop` call, and does NOT silently clamp/wrap.** Same Int-bound logic applies uniformly to IndexByteString's second arg, unconditional on semvar (`ensurable` only swaps the ByteString wrapper, not the Int bound-check).

## 3. ConstrData tag range — genuinely PV-gated, not just perf
Builtins.hs ~L1737-1751:
```haskell
toBuiltinMeaning semvar ConstrData
  | ensurable semvar =              -- variants D, E i.e. PV >= 11 (vanRossem)
      let constrDataD :: Word64 -> [Data] -> Data
          constrDataD = Constr . toInteger
  | otherwise =                     -- variants A, B, C i.e. PV < 11
      let constrDataD :: Integer -> [Data] -> Data
          constrDataD = Constr
```
- **Pre-vanRossem (PV<11, variants A/B/C): `constrData (-1) []` and `constrData (2^80) []` BOTH SUCCEED**, producing `Constr (-1) []` / `Constr (2^80) []` — no bound at all (raw `Integer`).
- **Post-vanRossem (PV>=11, variants D/E): tag is unlifted as `Word64`** via the same `AsInteger`-derived bounds check (`readKnownAsInteger` against `(0, 2^64-1)`) — out-of-range tag is an evaluation FAILURE, not a clamp.

CBOR decode of `Data` (used for on-chain datums/redeemers) is a **separate, PV-independent code path** that has ALWAYS required the tag to fit `Word64`, regardless of BuiltinSemanticsVariant:
`plutus-core/plutus-core/src/PlutusCore/Data.hs`:
- `decodeConstrExtended` (~L298-306): `i <- CBOR.decodeWord64` for the general/tag-102 form. A negative (CBOR major-type-1 nint) or >2^64-1 tag simply **fails to decode** — the `Data`/datum/redeemer can never exist on-chain with an out-of-range Constr tag, in ANY era.
- `encodeData` (~L145-160) has an explicit code comment: for `i` outside `Word64` range it still emits `CBOR.encodeInteger i` inside the tag-102 payload, annotated *"This is a 'correct'-ish encoding of the tag, but it will *not* deserialise, since we insist on a Word64 when we deserialise. So this is really a 'soft' failure."* I.e. Haskell's own encoder can produce bytes its own decoder rejects — an intentional, documented asymmetry, only reachable pre-vanRossem via unrestricted `constrData` + e.g. `serialiseData`.
- cardano-ledger's on-chain `Data era` (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Data.hs` ~L85-95, `newtype PlutusData era = PlutusData PV1.Data`) delegates its `Serialise`/CBOR instance straight to this same `PlutusCore.Data` codec — so the Word64 tag bound applies to every on-chain datum/redeemer unconditionally, confirming a Rust `Constr(u64, ...)` representation for **decoded/on-chain** Data is correct and lossless; the only place a wider (possibly negative) tag can transiently exist is inside CEK evaluation on pre-PV11 protocol versions via the raw-`Integer` `ConstrData` builtin.

## 4. VerifyEd25519Signature — CORRECTED 2026-07-06 (issue #825 research superseded the below in several places; see [[ed25519-verify-strict-vs-libsodium-ref10]] for full detail)
donna WAS dispatched via semvar A pre-#6848 (`7cc069b3f6352d153edf22f8075f84b817ebc2ae`, PR #6848, merged 2025-02-18), exactly as originally noted below. **CORRECTION: donna is DEAD CODE in any current/future cardano-node, including for pre-Chang replay.** The #6848 dispatch removal was never reverted and D/E variants were added later with NO dispatch reintroduced (confirmed against current master 2026-07). cardano-node 11.0.1 (dugite's target) necessarily pins plutus-ledger-api >= 1.51.0.0 (2025-07-30, first van-Rossem/PV11 release) — far after the donna removal. **So a fresh from-genesis full-validation sync by ANY current cardano-node uses libsodium unconditionally for verifyEd25519Signature on EVERY protocol version, including full re-validation of pre-Chang (PV<9) Alonzo/Babbage-era V1/V2 scripts.** dugite must do the same (single unconditional implementation, no PV/semvar gating of the algorithm) — do NOT reimplement donna. (The earlier version of this note incorrectly said the opposite — that pre-Chang replay needs donna semantics — that is now refuted.)

libsodium acceptance rules — corrected quote (`IntersectMBO/libsodium src/libsodium/crypto_sign/ed25519/ref10/open.c` `_crypto_sign_ed25519_verify_detached`, non-`ED25519_COMPAT` branch — confirmed `ED25519_COMPAT` is never defined anywhere in IntersectMBO/libsodium or cardano-base, so this is always the compiled-in branch):
```c
if (sc25519_is_canonical(sig + 32) == 0 || ge25519_has_small_order(sig) != 0) return -1;   // reject non-canonical S (S>=L) OR small-order R
if (ge25519_is_canonical(pk) == 0 || ge25519_has_small_order(pk) != 0) return -1;           // reject non-canonical OR small-order pubkey A
... ge25519_double_scalarmult_vartime(&R, h, &A, sig+32); compare rcheck to given R bytes directly (NO x8 cofactor multiplication)
```
**CORRECTION on dalek**: source-read `ed25519-dalek` 2.2.0 `verify_strict` directly (`RCompute::finish()`) — it uses `EdwardsPoint::vartime_double_scalar_mul_basepoint(&k, &(-A), &s)` = **non-cofactored** `-[k]A+[s]B`, i.e. the SAME equation family as libsodium, NOT a ZIP-215/RFC8032 cofactored (x8) equation as this note previously speculated. S-canonicity and small-order-R/A checks in `verify_strict` ALSO match libsodium in outcome. **The one CONFIRMED real divergence**: `VerifyingKey::from_bytes`/`CompressedEdwardsY::decompress()` use ZIP-215-permissive decompression (dalek's own doc: "RFC 8032 / NIST point validation criteria are currently unsupported", citing curve25519-dalek#626) — NO equivalent of libsodium's unconditional `ge25519_is_canonical(pk)` gate exists in dalek. A non-canonically-encoded pk (19 possible raw byte patterns total, y_encoded=y_actual+p for y_actual in 0..18) that reduces to an ORDINARY (non-small-order) point would be accepted by dalek but rejected by libsodium. Minimal fix = add that one explicit pk-canonicity pre-check (~10 lines, mirroring `ge25519_is_canonical`'s bit-twiddling), not a full ref10 reimplementation.
Deserialization (`rawDeserialiseVerKeyDSIGN`/`rawDeserialiseSigDSIGN`) is a bare fixed-length check only (32B/64B) — all crypto validity checks above happen inside the C call, not in Haskell.

## 5. VerifyEcdsaSecp256k1Signature — zero r/s, high-S, pubkey format
`PlutusCore/Crypto/Secp256k1.hs::verifyEcdsaSecp256k1Signature`, single impl, NOT semvar-gated (`toBuiltinMeaning _semvar VerifyEcdsaSecp256k1Signature`). Wraps `Cardano.Crypto.DSIGN.EcdsaSecp256k1` (`cardano-crypto-class/src/Cardano/Crypto/DSIGN/EcdsaSecp256k1.hs`).
- **Pubkey**: `SECP256K1_ECDSA_PUBKEY_BYTES = 33` (`cardano-crypto-class/src/Cardano/Crypto/SECP256K1/Constants.hsc`). `rawDeserialiseVerKeyDSIGN` (~L262-275) does a **fixed 33-byte** `psbFromByteStringCheck` BEFORE ever calling `secp256k1_ec_pubkey_parse` — so **a 65-byte uncompressed key is rejected at the Haskell length gate and never reaches libsecp256k1's parser** (which itself, per bitcoin-core/secp256k1 `secp256k1_ec_pubkey_parse`, WOULD accept 65-byte uncompressed if given the chance — Cardano just never allows it). Wrong-length key/sig -> `failWithMessage "Invalid verification key."` / `"Invalid signature."` -> **evaluation FAILURE** (not `False`).
- **r=0 or s=0**: `secp256k1_ecdsa_signature_parse_compact` (bitcoin-core/secp256k1 `src/secp256k1.c` ~L412-431) only fails on scalar **overflow** (r or s >= curve order n); zero is not an overflow, so **parse SUCCEEDS**. The zero-scalar rejection happens later, inside `secp256k1_ecdsa_sig_verify` (`src/ecdsa_impl.h` ~L205-207): `if (secp256k1_scalar_is_zero(sigr) || secp256k1_scalar_is_zero(sigs)) return 0;`. Net: **r=0/s=0 signature parses fine and `verifyEcdsaSecp256k1Signature` returns `Right False`** (successful BuiltinResult, Boolean False) — NOT an evaluation failure.
- **High-S (s > n/2)**: `secp256k1_ecdsa_verify` (`src/secp256k1.c` ~L477-491) checks `!secp256k1_scalar_is_high(&s)` FIRST, short-circuiting to `False` — confirmed both by the C source and by Plutus's own doc comment in Builtins.hs ~L1455-1469 ("returning `false` immediately if that's not the case... this restriction is peculiar to Bitcoin"). **No normalization is performed — high-S returns `False`, same successful-result-not-failure semantics as zero r/s.**
- Bottom line pattern across both Ed25519 and ECDSA/Schnorr secp256k1: **wrong LENGTH -> evaluation failure (aborts the whole script unconditionally, no try/catch exists in UPLC); wrong-but-well-formed-length crypto (bad sig, zero scalar, high-S, wrong message) -> `False` (script can still branch on it).** Getting this length-vs-value distinction backwards is exactly the kind of thing that flips a phase-2 verdict.

## 6. ExpModInteger bounds — complete contract
Two-layer check. Outer, `PlutusCore/Default/Builtins.hs` ~L2306-2319:
```haskell
expModIntegerDenotation a b m =
  if m < 0 then fail "expModInteger: negative modulus"
  else ExpMod.expMod a b (naturalFromInteger m)
```
Inner, `PlutusCore/Crypto/ExpMod.hs`:
```haskell
expMod b e m
  | m <= 0 || m > maxBoundN = fail "expMod: invalid modulus"     -- maxBoundN = 2^8191 - 1
  | m == 1 = pure 0                                                -- special-cased (integerPowMod# bug workaround)
  | b == 0 && e < 0 = failNonInvertible 0 m                        -- 0 has no inverse
  | oob b || oob e = fail "expMod: out of bounds"                  -- |b| or |e| outside [-2^8191, 2^8191-1]
  | otherwise = case integerPowMod# b e m of
      (# n | #)   -> pure n
      (# | () #)  -> failNonInvertible b m                         -- gcd(b,m)!=1 with e<0
```
Full contract: m<0 -> fail (msg "negative modulus"); m==0 -> fail ("invalid modulus"); m==1 -> succeeds, returns 0 (even though 0 has no real inverse, this is a deliberate special case); m>2^8191-1 -> fail; |base| or |exp| outside [-2^8191, 2^8191-1] -> fail ("out of bounds"), checked BEFORE calling `integerPowMod#`; base non-invertible mod m with negative exponent -> fail ("... is not invertible modulo ..."); otherwise succeeds with the modular result.

## 7. DropList negative count — clamps to whole-list-unchanged, never fails
`PlutusCore/Default/Builtins.hs` ~L2320-2369, uses raw GMP `Integer` constructors (`IS`/`IN`/`IP` from `GHC.Num.Integer`) on the count arg (`IntegerCostedLiterally`):
- `IS i#` (fits machine Int#, incl. negative) -> `drop (I# i#) xs`. GHC's stdlib `Prelude.drop` for negative `n` returns `xs` unchanged (documented `drop` semantics: n<=0 -> whole list).
- `IN _` (bignum more negative than fits in Int#) -> `pure xs` directly (same "whole list unchanged" result, special-cased since it can't be coerced to `Int`).
- `IP _` (bignum bigger than `maxBound::Int`) -> `drop maxBound xs`, defensively guarded (throws "Panic: unreachable clause executed" if somehow non-empty, which cost-model exhaustion makes practically unreachable).
**So a negative count for DropList NEVER fails — it always returns the original list unchanged**, regardless of magnitude (small or huge negative). Not semvar-gated (`_semvar`).

## Key files for quick re-fetch
- `plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs` (main denotations, ~2800 lines)
- `plutus-core/plutus-core/src/PlutusCore/Builtin/{Meaning,KnownType,Runtime,HasConstant}.hs` (unlifting machinery)
- `plutus-core/plutus-core/src/PlutusCore/Default/Universe.hs` (Int/Word <-> Integer bound-checked instances, `AsInteger`)
- `plutus-core/plutus-core/src/PlutusCore/Crypto/{Ed25519,Secp256k1,ExpMod,Utils}.hs`
- `plutus-core/plutus-core/src/PlutusCore/Data.hs` (Data CBOR codec, decodeWord64-bounded Constr tag)
- `plutus-ledger-api/src/PlutusLedgerApi/Common/ProtocolVersions.hs` + `.../MachineParameters.hs` (PV<->semvar table)
- `cardano-base/cardano-crypto-class/src/Cardano/Crypto/DSIGN/{Ed25519,EcdsaSecp256k1}.hs`
- `cardano-base/cardano-crypto-class/src/Cardano/Crypto/SECP256K1/Constants.hsc`
- `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Data.hs`
- External (not IntersectMBO, but load-bearing): `bitcoin-core/secp256k1/src/{secp256k1.c,ecdsa_impl.h}`, `IntersectMBO/libsodium/src/libsodium/crypto_sign/ed25519/ref10/open.c`
