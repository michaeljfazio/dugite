---
name: uplc-bls-unlifting-and-hardening-816-827-839-843
description: BLS12-381 builtin fixes — ByteString-as-element unlifting laxity, MSM empty-list elem_type check, redundant subgroup-recheck removal, final_verify aliasing/hardening nits
metadata:
  type: reference
---

Branch `fix/ledger-review-2026-07-04`, all changes confined to
`crates/dugite-uplc/src/builtin/bls.rs` plus 3 small necessary touches
(`term.rs` doc comment, `syn/parser.rs`/`syn/mod.rs` mlresult literal).
Not committed as part of this pass — see git log for the eventual commit.

**#816 (consensus, P0):** `unwrap_g1_bytes`/`unwrap_g2_bytes` had a
`Constant::ByteString` fall-through arm — any raw bytestring holding a
valid compressed point was silently accepted where a typed G1/G2
element was required. A cheap on-chain script (`bls12_381_G1_add (con
bytestring 0x<point>) ...`) would evaluate on dugite but fail Haskell's
`readKnownConstant` tag check. Fix: delete the ByteString arms; only
`Constant::Bls12_381G1Element`/`G2Element` unlift.

**#827 (P1):** `denote_multi_scalar_mul` matched `ProtoList { elements,
.. }` discarding `elem_type`, so an empty wrong-typed list (e.g. `(list
bool) []` fed as the scalars) unlifted successfully instead of failing.
cardano-haskell-oracle confirmed against source (`KnownType.hs` `geqL`
+ `PlutusCore.Crypto.BLS12_381.G1/G2.multiScalarMul`):
1. `readKnownConstant`'s `geqL` check is purely on the GADT type-tag
   witness embedded at parse time — it never inspects list contents,
   so it fires identically on empty and non-empty mismatched lists.
2. `multiScalarMul` is a bare `zip ss ps` with **no length-equality
   check anywhere** — mismatched lengths truncate to the shorter list,
   never error.
3. `zip [] [] = []` → `blsMSM` returns `blsZero` (identity) — empty×empty
   succeeds.
   Decisive conformance vectors (corrected from an earlier
   mis-citation): `multiScalarMul-08` (literal `[] []` → identity, NOT
   `05a` which is the general recursive case) and `09a`/`10a` ("extra
   entries ... ignored", NOT `12a` which is scalar-independent since
   all scalars are zero there). Fix: bind `elem_type` in all three
   `ProtoList` matches (scalars, G1 points, G2 points) and reject on
   mismatch before iterating, including for empty lists. Length
   mismatch / zip-truncation behavior was already correct in dugite —
   only the doc comment claiming "equal length required" was wrong.

**#839 (perf/DoS, P2, partial fix by design):** every BLS op
re-decompresses + re-subgroup-checks its compressed-byte inputs on
every call, but is charged Haskell's in-memory-point cost. Full fix
(cache the decompressed+validated `blst_p1`/`blst_p2` alongside the
compressed bytes) requires touching `term.rs` (`Constant` shape),
`flat/term.rs` (decode/encode), and machine/data.rs call sites — out of
the bls.rs-only scope for this pass and explicitly called out in the
issue's own fix guidance as droppable rather than cacheable once #816
lands. Applied the narrower, bls.rs-only mitigation instead: once
`unwrap_g1_bytes`/`unwrap_g2_bytes` only ever return bytes extracted
from an already-typed `Bls12_381G1Element`/`G2Element` (post-#816), and
flat decode rejects raw BLS constant literals outright (`flat/term.rs`
line ~747: `TypeTag::Bls12_381G1Element | G2Element | MlResult => Err
("not yet wired")`), every `Bls12_381G1Element`/`G2Element` that can
ever exist in the machine is provably already subgroup-validated (by
hashToGroup construction, prior uncompress, textual-parser validation,
or closure of group arithmetic on already-valid points). Added
`decompress_g1_trusted`/`decompress_g2_trusted` (decode only, skip
`blst_p1_in_g1`/`blst_p2_in_g2`) and rewired every *internal* consumer
(`take_one_g1`/`take_two_g1`/`g1_scalar_mul`/`miller_loop`/MSM point
loops, and G2 equivalents) to use them. The two true untrusted-input
entry points, `g1_uncompress`/`g2_uncompress` (raw external
`ByteString` via `unwrap_bytes`), keep the full checked `uncompress_g1`/
`uncompress_g2`. Honest limitation: this removes the subgroup-check
cost but NOT the decompression cost (unavoidable without the point
cache, since `Constant` only stores compressed bytes) — so it is a
partial, not full, closure of the ~100-400x actual-vs-charged gap in
the issue. Full closure remains a cross-file, out-of-scope change.

**#843 (hardening nits, all applied):**
- `final_verify`: replaced manual `inverse + mul + blst_final_exp(&mut
  combined, &combined) + is_equal` (the last step aliased `&mut`/`&`
  to the same place — Stacked-Borrows UB) with a direct
  `blst_fp12_finalverify(&a_fp, &b_fp)` call (exists in blst 0.3.16
  bindings) — same result, no aliasing, no manual inverse.
- Added `const _: () = assert!(size_of::<blst_fp12>() == FP12_BYTES);`
  pinning the 576-byte memcpy-via-pointer-cast assumption in
  `fp12_to_bytes`/`fp12_from_bytes`.
- `bls_scalar_r()`: replaced
  `BigInt::parse_bytes(HEX).unwrap_or_else(|| BigInt::from(1))` (r=1
  fallback would silently zero every scalar mod-reduction) with a
  fixed 32-byte-array `BigInt::from_bytes_be` construction — can't fail
  at all, so no fallback path exists to get wrong. NOTE: the crate
  denies `clippy::expect_used`/`unwrap_used` outside `cfg(test)`
  (`lib.rs:42`), so `.expect(...)` was not an option here even though
  the verdict's exact_fix text suggested it — the byte-array
  reconstruction sidesteps the lint entirely by having no fallible step.
- `g1_uncompress`/`g2_uncompress`: return the original validated input
  bytes directly instead of `compress_g1(&p)`/`compress_g2(&p)` — blst's
  compressed form is canonical so they're byte-identical; saves a
  redundant re-compression on a hot builtin.
- `syn/parser.rs` `TypeTag::Bls12_381MlResult` literal parsing: was
  silently accepting `(con bls12_381_mlresult 0x...)` with zero
  validation (576 raw bytes straight into `blst_fp12` arithmetic).
  Haskell has no `Parsable`/`Read` instance for `MlResult` — rejected
  outright now (`ParseError`). Textual-only; flat decode already
  rejected this. No existing test/corpus vector used the literal.

**Gate results:** `cargo build -p dugite-uplc --all-targets` clean;
`cargo nextest run -p dugite-uplc --features conformance` 1526/1526
(999/999 conformance corpus byte-identical, confirmed via `grep -c
"dugite-uplc::conformance "`); `cargo clippy -p dugite-uplc --all-targets
--features conformance -- -D warnings` clean; `cargo fmt -p dugite-uplc
--check` clean; `cargo build --workspace --all-targets` clean. Added 15
new tests (11 in `bls.rs`, 1 in `syn/mod.rs`, plus the pre-existing
final_verify positive case now has a negative-case sibling) covering:
each G1/G2 group-consuming builtin rejecting a ByteString-typed valid
point; MSM rejecting wrong-typed empty scalar/point lists for both
groups; MSM empty×empty identity; MSM length-mismatch truncation
equals a direct scalarMul; `bls_scalar_r` exact decimal value; fp12
size assert; final_verify false-case; mlresult literal parse rejection.

See also [[uplc-builtin-flat-id-mismatch]] for a prior BLS wire-format
bug class (flat ID misalignment, different root cause — this batch is
all semantic/unlifting, not wire-encoding).
