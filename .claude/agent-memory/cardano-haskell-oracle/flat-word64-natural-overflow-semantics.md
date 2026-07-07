---
name: flat-word64-natural-overflow-semantics
description: Definitive proof that IntersectMBO/plutus's vendored `flat` library REJECTS (never truncates) Word64 varints that exceed 2^64-1, while Natural varints are truly unbounded; identifies which UPLC fields use which decoder
type: reference
---

Resolved for dugite issue #842 (2026-07-06). IntersectMBO/plutus vendors its own
fork of the `flat` library in-tree at `plutus-core/flat/src/PlutusCore/Flat/*`
(namespace `PlutusCore.Flat.*`, not the upstream Hackage `flat` package). All
citations below are against `master` @ `266f6a028daa` (2026-07-06).

## The core fact: Word64 decode REJECTS overflow, does not truncate

`plutus-core/flat/src/PlutusCore/Flat/Decoder/Strict.hs:163-236` — `dWord64` is
a fixed chain of 9 `wordStep` continuations (shl = 0,7,...,56) followed by one
`lastStep 63` for the 10th/final chunk:

```haskell
dWord64 :: Get Word64
dWord64 = wordStep 0 (wordStep 7 (... (wordStep 56 (lastStep 63)) ...)) 0

wordStep shl k n = do
  tw <- fromIntegral <$> dWord8
  let w = tw .&. 127
  let v = n .|. w `shift` shl
  if tw == w then return v else k v          -- tw==w  <=>  continuation bit clear

lastStep shl n = do                            -- shl = 63 for Word64
  tw <- fromIntegral <$> dWord8
  let w = tw .&. 127
  let v = n .|. w `shift` shl
  if tw == w
    then if countLeadingZeros w < shl then wordErr v else return v
    else wordErr v
 where wordErr v = fail $ "Unexpected extra byte in unsigned integer" ++ show v
```

At the 10th chunk (shl=63) only 1 more bit of a 64-bit word is free (9*7=63
bits already consumed), so only chunk values 0 and 1 are representable without
loss. `countLeadingZeros (w::Word64) < 63` is true exactly when `w >= 2`. So
Haskell's rule is precisely: **reject iff the final (shift=63) chunk's 7-bit
value is > 1** — an exact match for the fix dugite issue #842 proposed
("reject when shift==63 && chunk>1"). This is `fail`, i.e. `Left (BadEncoding
...)`, never a silent wraparound.

Confirmed by doctest in `plutus-core/flat/src/PlutusCore/Flat/Instances/Base.hs`
(search "Word/Int decoders return an error if the encoded value is outside
their valid range"):
```
>>> unflat @Word64 (flat @Natural $ fromIntegral @Word64 maxBound)
Right 18446744073709551615
>>> unflat @Word64 (flat @Natural $ fromIntegral @Word64 maxBound + 1)
Left (BadEncoding ...
```

## Natural (arbitrary precision) is NOT bounded at all — no truncation, no reject

`Decoder/Strict.hs:106-108`: `dNatural = dUnsigned`. `dUnsigned` (`Strict.hs:240-259`)
loops accumulating 7-bit chunks into any `(Num b, Bits b)` with no chunk-count
limit; the only post-hoc check is `case bitSizeMaybe v of Nothing -> return v;
Just s -> ...` — and `Natural`'s `Bits` instance returns `bitSizeMaybe = Nothing`
(unbounded), so the bound check is dead code for `Natural`. A `Natural` varint
of any length (limited only by remaining input / overall 16KiB script-size cap)
decodes successfully into an arbitrary-precision bignum. Confirmed by doctest:
`test (2^120::Natural)` round-trips fine (`Instances/Base.hs`).

**This means "reject at u64 boundary" is the CORRECT behavior only for fields
Haskell types as `Word64`, not for fields Haskell types as `Natural`.** A
single shared decoder function that always rejects at 2^64 is right for the
former and wrong (over-strict vs Haskell) for the latter.

## Which UPLC fields use which decoder (verified by type + Flat instance)

- **De Bruijn `Index`** (used for every `Var` term / lambda binder count):
  `plutus-core/plutus-core/src/PlutusCore/DeBruijn/Internal.hs` defines
  `Index` as a `Word64` newtype. Its *current* Flat instance in
  `plutus-core/plutus-core/src/PlutusCore/FlatInstances.hs:362`:
  `deriving newtype instance Flat Index -- via word64` — i.e. decode = `dWord64`
  (the bounded/rejecting decoder above), confirmed reject semantics.
  `FlatInstances.hs:84` `Note [DeBruijn Index serialization]` documents the
  history explicitly: Index used to be `Natural`-encoded, was switched to a
  *custom* Word64 decoder to work around a bug in `flat<0.5.2`'s stock Word64
  decoder, and — now that the vendored `flat>=0.6` fork has the bug fixed —
  switched to the **non-custom, fixed** `Word64` decoder (i.e. exactly `dWord64`
  above). This note is itself proof the "reject on Word64 overflow" behavior is
  intentional, tested, and load-bearing for consensus.
  Property-tested explicitly in
  `plutus-core/untyped-plutus-core/testlib/DeBruijn/FlatNatWord.hs`:
  `prop_DecLarger` (line 40-42) asserts `isLeft $ unflat @Word64 $ flat @Natural n`
  for `n` beyond `maxBound::Word64`; `prop_OldVsNewIndex` (line 65+) asserts the
  old custom decoder and the new `deriving via word64` decoder agree
  `Left`-for-`Left` on all out-of-range naturals.

- **`Constr` tag** (UPLC sum-type discriminant, PlutusV3/Conway `plcVersion110+`
  only): `plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Core/Type.hs:101`
  — `Constr !ann !Word64 ![Term ...]` (comment right above at line ~99: "TODO:
  worry about overflow, maybe use an Integer -- See Note [Constr tag type]" —
  the Plutus team itself flagged this as an open concern, but the field type
  today is `Word64`). Decode site
  `plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Core/Instance/Flat.hs:167`
  (`handleTerm 8 = ... Constr <$> decode <*> decode <*> ...`) dispatches the tag
  field's `decode` on its static type `Word64` → `dWord64` → same reject
  semantics as Index.

- **Program `Version` triple** (`(major,minor,patch)` header of every flat
  program): `plutus-core/plutus-core/src/PlutusCore/Version.hs:44` —
  `data Version = Version {_versionMajor :: Natural, _versionMinor :: Natural,
  _versionPatch :: Natural}` — all three fields are `Natural`, not `Word64`.
  `FlatInstances.hs:176` `instance Flat Version where decode = Version <$>
  decode <*> decode <*> decode` dispatches each field's `decode` on `Natural`
  → `dUnsigned`, the **unbounded** decoder. Haskell places **no cap at all**
  on the version components; dugite capping them at u64 (`read_natural_u64` in
  `program.rs:49-51`) is a real, if practically-irrelevant, "too strict vs
  Haskell" divergence, separate from and NOT fixed by the Index/Constr
  overflow-reject fix. See [[plutus-flat-wire-format-defaultfun]] for the
  broader Flat wire-format DefaultFun/Program map.

## Practical takeaway for dugite

`crates/dugite-uplc/src/flat/bits.rs` `read_natural_u64`'s overflow guard
(`if shift >= u64::BITS`) fires one 7-bit chunk too late: at `shift == 63` the
unguarded `chunk << shift` in Rust silently drops bits 1..6 of `chunk` (`<<` by
an in-range amount just discards the shifted-out high bits, it does not panic
or wrap onto bit 0), so a 10-chunk varint whose final chunk is >= 2 is
silently truncated to a valid-looking `u64` instead of erroring — i.e. dugite
is currently **too permissive** relative to Haskell (accepts adversarial input
Haskell's node would reject at phase-1 deserialization). The fix (`reject when
shift==63 && chunk>1`) is an exact match for Haskell's `lastStep` and should be
applied at the `Var`-index (`term.rs:292`) and `Constr`-tag (`term.rs:333`)
call sites. It should NOT be extrapolated to also start rejecting the
version-triple decode in `program.rs:49-51` — that field is genuinely
unbounded in Haskell (see above); that mismatch is real but separate and
already tracked (issue #842 item 3) as low-priority/practically-irrelevant
since no real compiler emits version numbers anywhere near 2^64.

Reachability: the *decode path* (`dWord64`/`read_natural_u64`) runs on every
single `Var` node of every real script (all scripts have variables) and on
every `Constr` node of PlutusV3 sum-type-using scripts — hot path, not
adversarial-only. The *specific 10-chunk-with-final-chunk>=2 overflow trigger*
requires an index/tag value >= 2^63, which no real compiler (plutus-tx, Aiken,
Helios) would ever emit (that would imply billions of nested binders/branches)
— reaching the exact edge case is adversarial-only, but dugite's own project
stance (dugite treats all wire input as adversarial; see project memory
`feedback_dugite_node_hostile_environment.md`) is precisely the threat model a
consensus-critical phase-1 decoder must be hardened against; "no real script
hits it" does not make the accept-when-Haskell-rejects direction safe to leave
unfixed.
