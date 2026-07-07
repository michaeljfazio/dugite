---
name: ed25519-verify-strict-vs-libsodium-ref10
description: verifyEd25519Signature semvar dispatch history (donna removed Feb 2025, now unconditional libsodium for ALL PVs incl. pre-Chang); ed25519-dalek verify_strict vs libsodium ref10 exact edge-case divergence (pk canonicity gap)
metadata:
  type: reference
---

# verifyEd25519Signature: semvar dispatch history + dalek/libsodium divergence (issue #825 research)

## 1. donna is DEAD CODE in current/any-recent cardano-node — do not reimplement it

`plutus-core/plutus-core/src/PlutusCore/Crypto/Ed25519.hs` pre-2025-02-18 had TWO
implementations dispatched by `BuiltinSemanticsVariant` in
`plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs`:

```haskell
toBuiltinMeaning semvar VerifyEd25519Signature =
    let verifyEd25519SignatureDenotation = case semvar of
          DefaultFunSemanticsVariantA -> verifyEd25519Signature_V1  -- ed25519-donna (crypton)
          DefaultFunSemanticsVariantB -> verifyEd25519Signature_V2  -- cardano-crypto-class/libsodium
          DefaultFunSemanticsVariantC -> verifyEd25519Signature_V2
```

Commit `7cc069b3f6352d153edf22f8075f84b817ebc2ae` ("Kwxm/crypto/remove ed25519-donna",
PR #6848, merged 2025-02-18) deleted `verifyEd25519Signature_V1` entirely and collapsed
the dispatch to `toBuiltinMeaning _semvar VerifyEd25519Signature = ... verifyEd25519Signature ...`
— i.e. **unconditional libsodium for every semvar**, including A. This was never reverted;
current master (checked 2026-07) still has the single unconditional implementation, and when
variants D/E (van Rossem) were added later the case was already gone (no dispatch was ever
reintroduced).

semvar-to-PV mapping (`PlutusLedgerApi.{V1,V2}.EvaluationContext`, from
`PlutusLedgerApi.Common.ProtocolVersions`: alonzoPV=5, vasilPV=7, valentinePV=8, changPV=9,
plominPV=10, vanRossemPV=11):
- V1/V2: `pv<changPV(9) -> A`, `pv<vanRossemPV(11) -> B`, `else -> D`
- V3: `pv<vanRossemPV(11) -> C`, `else -> E`

`VerifyEd25519Signature` is in `batch1`, introduced at `alonzoPV` for V1 / `vasilPV` for V2 —
so it WAS reachable historically under variant A on real mainnet blocks (Alonzo→pre-Chang
V1/V2 scripts). But since cardano-node 11.0.1 (dugite's current target) necessarily pins a
plutus-ledger-api release >= 1.51.0.0 (2025-07-30, first release exposing van-Rossem/PV11
builtins — see plutus-ledger-api CHANGELOG.md) which is far AFTER the Feb-2025 donna removal,
**any current cardano-node computes verifyEd25519Signature via libsodium for EVERY protocol
version including full-from-genesis replay of pre-Chang blocks.** Donna is unreachable in any
currently-supported cardano-node. dugite must match libsodium unconditionally across all PVs —
no per-(language,pv) algorithm dispatch needed (semvar still matters for costing, just not for
which crypto backend this one builtin uses).

Donna's own semantics (source-verified via `kazu-yamamoto/crypton` — the crypton/cryptonite
fork Plutus depended on — `cbits/ed25519/ed25519.c`, `ed25519_sign_open`, literally vendored
floodyberry ed25519-donna C): `if ((RS[63] & 224) || !ge25519_unpack_negative_vartime(&A, pk)) return -1;`
— only the WEAK top-3-bits scalar check (not full `S<L`), NO small-order/canonical checks on
A or R at all. Non-cofactored equation (`SB - hA`), same math family as libsodium/dalek, just
far more permissive input validation. Confirms donna is strictly weaker/more permissive than
both libsodium and dalek's `verify_strict` — consistent with "Taming the many EdDSAs" (Chalkias
et al.) literature categorization.

## 2. libsodium ref10 exact check sequence (Cardano fork, IntersectMBO/libsodium)

`src/libsodium/crypto_sign/ed25519/ref10/open.c`, `_crypto_sign_ed25519_verify_detached`
(non-`ED25519_COMPAT` path — confirmed `ED25519_COMPAT` is never defined anywhere in
IntersectMBO/libsodium or IntersectMBO/cardano-base, so this strict path is always compiled in):

```c
if (sc25519_is_canonical(sig + 32) == 0 || ge25519_has_small_order(sig) != 0) return -1;
if (ge25519_is_canonical(pk) == 0 || ge25519_has_small_order(pk) != 0) return -1;
if (ge25519_frombytes_negate_vartime(&A, pk) != 0) return -1;
/* h = SHA512(R||A||M) mod L; R' = [h]A + [S]B (via negated A => non-cofactored SB=R+hA) */
/* accept iff bytewise R' == R */
```

`ge25519_is_canonical` (`crypto_core/ed25519/ref10/ed25519_ref10.c`) rejects pk iff its
255-bit magnitude (sign bit masked) is >= p = 2^255-19 (explicit, UNCONDITIONAL canonical
field-element check, independent of point order). `ge25519_has_small_order` is a static
7-entry byte blacklist (0, 1, two order-8 x-values, p-1, p, p+1 — covering both canonical and
"+p wraparound" non-canonical encodings of the 8-element torsion subgroup). `sc25519_is_canonical`
checks S < L = 2^252+27742317777372353535851937790883648493.

Call chain confirmed: `cardano-crypto-class/src/Cardano/Crypto/DSIGN/Ed25519.hs` `verifyDSIGN`
→ FFI `c_crypto_sign_ed25519_verify_detached` → exactly the function above.
`rawDeserialiseVerKeyDSIGN`/`rawDeserialiseSigDSIGN` only check byte LENGTH (32/64), all the
real checks live in verify_detached.

## 3. ed25519-dalek 2.2.0 `verify_strict` — confirmed DIVERGENCE on public-key canonicity

Source-read directly from `~/.cargo/registry/src/.../ed25519-dalek-2.2.0/src/{verifying,signature}.rs`
and `curve25519-dalek-4.1.3/src/edwards.rs`:

- **S canonicity: MATCHES.** Default (non-`legacy_compatibility`) `check_scalar` uses
  `Scalar::from_canonical_bytes` = full `S<L` check, same as `sc25519_is_canonical`. Dugite's
  Cargo.toml (`ed25519-dalek = { version="2", features=["serde","rand_core"] }`) does NOT
  enable `legacy_compatibility` — correct/already matches libsodium, NOT donna.
- **Small-order R: MATCHES in outcome.** `verify_strict` decompresses `signature.R` (ZIP-215
  permissive `CompressedEdwardsY::decompress`, no canonicity check) then calls
  `signature_R.is_small_order()` = `mul_by_cofactor().is_identity()` — an actual group-order
  computation, catches the same 8-torsion set (canonical AND non-canonical "+p" aliases, since
  ZIP-215 decompress reduces mod p first) as libsodium's blacklist.
- **Small-order A (pubkey): MATCHES in outcome** — same mechanism (`self.point.is_small_order()`
  post-decompression), correctly reduces mod p first so catches aliased encodings too.
- **Non-cofactored equation SB=?R+hA: MATCHES exactly.** `RCompute::finish()` computes
  `EdwardsPoint::vartime_double_scalar_mul_basepoint(&k, &(-A), &s)` = `-[k]A + [s]B`, same math
  as libsodium, no cofactor multiplication anywhere.
- **CANONICAL PUBLIC KEY (A) ENCODING: DIVERGES — CONFIRMED, source-quoted.**
  `VerifyingKey::from_bytes` doc comment (verbatim, `src/verifying.rs`): *"Verifies the point is
  valid under ZIP-215 rules. RFC 8032 / NIST point validation criteria are currently unsupported
  (see dalek-cryptography/curve25519-dalek#626)."* `CompressedEdwardsY::decompress()` →
  `decompress::step_1` uses `FieldElement::from_bytes(repr.as_bytes())` with NO canonicity check
  at all — accepts ANY y in [0,2^255) (silently reduces mod p through lazy field arithmetic) as
  long as a valid x exists, and `verify_strict` only rejects the resulting point if
  `is_small_order()`. There is NO equivalent of libsodium's unconditional
  `ge25519_is_canonical(pk)` gate anywhere in ed25519-dalek's verify path.
  **Concrete gap:** a pk encoded with a non-canonical byte pattern (y_encoded in [p, 2^255),
  i.e. y_encoded = y_actual + p for y_actual in 0..18 — only 19 such raw byte-patterns exist at
  all, since p = 2^255-19) that decodes to an ORDINARY (non-small-order) point would be REJECTED
  outright by libsodium/cardano-node but potentially ACCEPTED by dalek's `verify_strict` if the
  signature is otherwise valid against the reduced point. y_actual=0 and y_actual=1 are already
  small-order (caught by both regardless of the canonicity gap); only y_actual in 2..18 (17
  candidates, few if any of which are likely valid ordinary curve points) are the actual gap
  surface. Practical forgery difficulty is high (classic "small-order lets you forge for any
  message" trick does NOT extend to large-order points reached via this alias — you'd need to
  actually possess/derive a keypair whose y-coordinate happens to be one of these 17 tiny
  values, as hard as generic discrete-log), but per dugite's byte-exact-on-ALL-inputs mandate
  (see [[feedback_haskell_byte_exact_only]] in cardano-ledger-oracle memory) this is still a
  real, provable divergence that must be closed, not dismissed as impractical.

## 4. Minimal fix (NOT a full ref10 reimplementation)

`verify_strict` already correctly handles S-canonicity, small-order-A, small-order-R, and the
non-cofactored equation. The ONLY missing piece is an explicit canonical-pk-encoding pre-check
mirroring `ge25519_is_canonical`: reject pk if its 255-bit magnitude (mask top bit of byte 31)
>= p = 2^255-19. ~10-line self-contained routine, no crypto library changes needed. Apply once,
unconditionally, for ALL protocol versions (no semvar/PV gating — matches what upstream plutus
itself does today). Current dugite code:
`crates/dugite-uplc/src/builtin/denotations.rs:322-359` (`VerifyEd25519Signature` arm) — insert
the canonical-pk check between the length check and `VerifyingKey::from_bytes`, returning
`Bool(false)` on failure (matching the existing "malformed key -> False not crash" pattern
already used a few lines below for `from_bytes` failure).

## 5. Test vectors — upstream does NOT already have ready-made adversarial vectors

Checked `plutus-core/untyped-plutus-core/testlib/Evaluation/Builtins/SignatureVerification.hs`
(`genEd25519Case`/`genEd25519ErrorCase`/`genBadVerKey`/`genBadSig`) — these are generic
valid/malformed-length property generators, NOT targeted small-order/non-canonical vectors.
Searched cardano-base's test suite for Ed25519 small-order/canonical test content — no hits.
**Upstream has no ready-made oracle vectors for this edge-case class.** Recommended path:
(a) Google/Wycheproof `eddsa_test.json` for the standard non-canonical-S / small-order-A /
small-order-R / non-canonical-A test groups (cross-language reference, though check its
`flags` field per-case since not all libraries test the same axes); (b) hand-construct the 19
"p+k" alias byte-patterns directly (cheap, deterministic — just check which of y_actual=0..18
decode to valid curve points via the curve equation) and confirm dugite's new check rejects all
of them while establishing ground truth for what libsodium does via a small GHC harness calling
`Cardano.Crypto.DSIGN.Ed25519.verifyDSIGN` directly (no ready-made upstream fixture exists).

See also [[plutus-builtin-availability-gate]] for the batch1-6 builtin-availability table this
finding builds on (VerifyEd25519Signature confirmed in batch1 = alonzoPV/vasilPV).
