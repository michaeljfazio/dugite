---
name: constrdata-word64-gate-version-boundary
description: Data type def (Constr Integer [Data]) + CBOR/Flat codec fully quoted; CRITICAL correction to plutus-builtins-adversarial-audit.md - the ConstrData Word64 gate does NOT exist in the plutus-ledger-api version cardano-node 11.0.1 actually bundles
type: reference
---

Source-verified 2026-07-06 by direct fetch + diff of IntersectMBO/plutus tags
1.62.0.0 / 1.63.0.0 / 1.65.0.0 (raw.githubusercontent.com, not just grep of a
single tag). Triggered by a dugite-uplc byte-exact bug investigation
(`Data::Constr(tag, fields)` uses `u64` in Rust vs Haskell's `Constr Integer`).

## 1. `Data` type definition (unconditional, all eras/PVs)

`plutus-core/plutus-core/src/PlutusCore/Data.hs` lines 42-49:
```haskell
data Data
  = Constr Integer [Data]
  | Map [(Data, Data)]
  | List [Data]
  | I Integer
  | B BS.ByteString
  deriving stock (Show, Read, Eq, Ord, Generic, Data.Data.Data)
  deriving anyclass (Hashable, NFData, NoThunks)
```
Confirmed: tag is a raw, unbounded, signed `Integer` at the TYPE level, always
(this part has never changed across any plutus version checked).

## 2. CBOR (`Serialise Data`) codec — compact + general forms

Same file, `encodeData` (145-166) / `decodeConstr` (285-307). Verbatim scheme
comment (`Note [CBOR alternative tags]`, lines 73-86):
```
Alternatives 0-6 -> tags 121-127, followed by the arguments in a list
Alternatives 7-127 -> tags 1280-1400, followed by the arguments in a list
Any alternatives, including those that don't fit in the above -> tag 102
  followed by a list containing an unsigned integer for the actual
  alternative, and then the arguments in a (nested!) list.
```
`encodeData` for the tag-102 general form:
```haskell
Constr i ds
  | otherwise ->
      let tagEncoding =
            if fromIntegral (minBound @Word64) <= i && i <= fromIntegral (maxBound @Word64)
              then CBOR.encodeWord64 (fromIntegral i)
              -- This is a "correct"-ish encoding of the tag, but it will *not* deserialise, since
              -- we insist on a 'Word64' when we deserialise.
              -- So this is really a "soft" failure.
              else CBOR.encodeInteger i
       in CBOR.encodeTag 102 <> CBOR.encodeListLen 2 <> tagEncoding <> encode ds
```
`decodeConstrExtended` (298-307):
```haskell
decodeConstrExtended = do
  len <- CBOR.decodeListLenOrIndef
  i <- CBOR.decodeWord64          -- negative or >2^64-1 tag: DECODE FAILS, unconditionally
  args <- decodeListOf decodeData
  ...
  pure $ Constr (fromIntegral i) args
```
So: CBOR decode of `Data` has ALWAYS required the Constr tag to fit `Word64`
in every plutus version checked (this is NOT new/gated — it's the on-chain
datum/redeemer decode path, PV-independent, shared by
`cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Data.hs`'s
`PlutusData era` wrapper). Only the CEK-builtin-runtime path (`constrData`,
see below) is where the "arbitrary Integer" case actually differs by version.

## 3. Flat encoding of `Data` constants — reuses the SAME CBOR bytes

`plutus-core/plutus-core/src/PlutusCore/FlatInstances.hs` line 126:
```haskell
deriving via FlatViaSerialise Data instance Flat Data
```
`plutus-core/plutus-core/src/Codec/Extras/FlatViaSerialise.hs`:
```haskell
instance Serialise a => Flat (FlatViaSerialise a) where
  encode = encode . BSL.toStrict . serialise . unFlatViaSerialise
  decode = do
    errOrX <- deserialiseOrFail <$> decode
    case errOrX of
      Left err -> fail $ show err
      Right x -> pure $ FlatViaSerialise x
```
I.e. a `Data`-typed constant embedded literally in a serialized UPLC program
(`con data #...`) is flat-encoded as a length-chunked strict bytestring
containing the FULL CBOR bytes (`encodeData`/`decodeData` above) — not a
bespoke bit-packed format. So the Word64 tag bound on decode applies
identically whether the bytes arrive via a datum/redeemer OR via a literal
`Data` constant inside a script body (both go through `decodeConstrExtended`).
A negative/huge-tag `Data` value can therefore never be embedded as a
DECODABLE literal constant in an on-chain script — `encodeData`'s own
"soft failure" comment applies: it happily EMITS such bytes (via
`CBOR.encodeInteger i`) but its own decoder then rejects them.

## 4. `constrData` builtin — genuinely version-gated, and NOT where you'd expect

`plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs`. Confirmed by
DIRECT SOURCE DIFF across three tags (not just grepping one):

**Tag 1.62.0.0** (GitHub-released 2026-04-24), line ~1478 — **NO gate at all**:
```haskell
toBuiltinMeaning _semvar ConstrData =
  let constrDataDenotation :: Integer -> [Data] -> Data
      constrDataDenotation = Constr
   in makeBuiltinMeaning constrDataDenotation (runCostingFunTwoArguments . paramConstrData)
```
`_semvar` is unused (underscore-prefixed) — `constrData` accepts ANY Integer
(negative, >2^64-1) unconditionally, for every `BuiltinSemanticsVariant`
including D/E (which already exist as an enum and are ALREADY selected for
PV>=11 by `machineParametersFor` at this same tag — variant D/E in 1.62.0.0
is only wired up for `ConsByteString`'s V1/V2 dispatch, nothing else).

**Tag 1.63.0.0** (GitHub-released 2026-05-06), line ~1737 — **gate added**:
```haskell
toBuiltinMeaning semvar ConstrData
  | ensurable semvar =
      let constrDataD :: Word64 -> [Data] -> Data
          constrDataD = Constr . toInteger
       in makeBuiltinMeaning constrDataD (runCostingFunTwoArguments . paramConstrData)
  | otherwise =
      let constrDataD :: Integer -> [Data] -> Data
          constrDataD = Constr
       in makeBuiltinMeaning constrDataD (runCostingFunTwoArguments . paramConstrData)
```
`ensurable :: BuiltinSemanticsVariant DefaultFun -> Bool; ensurable = \case DefaultFunSemanticsVariantD -> True; DefaultFunSemanticsVariantE -> True; _ -> False`
(line ~2725-2730). This is part of a MUCH bigger diff introduced by
**PR #7754** ("update default universe plumbing and tidy builtin handling",
merged 2026-05-01T17:31:01Z, first released as tag 1.63.0.0 on 2026-05-06) —
the same commit ALSO introduces the `CInteger`/`CByteString` newtype-wrapper
perf swap for AddInteger/SubtractInteger/MultiplyInteger/Divide/Quotient/etc.
(dozens of builtins get an `ensurable`-gated branch for the first time here;
this matches/completes what `plutus-builtins-adversarial-audit.md` §0
describes as "variant D/E ensurable dispatch" — that memory's SOURCE TAG was
1.65.0.0, i.e. already-post-1.63.0.0, so it only ever saw the COMPLETED
picture).

**`machineParametersFor` (`plutus-ledger-api/src/PlutusLedgerApi/MachineParameters.hs`) is IDENTICAL between 1.62.0.0 and 1.63.0.0** — `majorPV >= vanRossemPV(11)` selects variant D (V1/V2) or E (V3) in BOTH. So this is not a case of the PV threshold changing; it's `ConstrData`'s OWN denotation code failing to check `ensurable semvar` in 1.62.0.0 despite variant D/E already being the selected variant for PV>=11 — i.e. 1.62.0.0 has an INCOMPLETE/buggy implementation of variant D/E's intended semantics, silently fixed in 1.63.0.0. Undocumented in either package's CHANGELOG.md (grepped both, no mention of ConstrData/ensurable/Word64/vanRossem) — only found via commit-range diff + `gh api compare`.

## 5. THE version-boundary problem for cardano-node 11.0.1 (dugite's actual target)

- `cardano-node` tag `11.0.1`: GitHub release published **2026-05-05T16:47:12Z**.
  Its `cabal.project` pins `cardano-haskell-packages 2026-05-02T16:21:41Z`
  as the CHaP index-state (no `source-repository-package` stanza for either
  `cardano-ledger` or `plutus*` — both resolved purely through CHaP at that
  index-state).
- plutus tag `1.62.0.0` GH-released 2026-04-24 (well before the cutoff) —
  ungated `constrData`.
- plutus tag `1.63.0.0` GH-released 2026-05-06 (ONE DAY AFTER cardano-node
  11.0.1's own release, and after its CHaP index-state cutoff) — gated
  `constrData`. CHaP publish lag means 1.63.0.0 could not possibly have been
  resolvable at the 2026-05-02 index-state even though PR #7754 had already
  merged to plutus `master` on 2026-05-01 (merge-to-master and
  tag-then-CHaP-publish are different events with real lag between them).

**Conclusion: cardano-node 11.0.1 — the exact binary dugite's own CLAUDE.md
names as the minimum version for preview/PV11 connectivity — almost
certainly bundles plutus-ledger-api 1.62.0.0 or older, and therefore
`constrData` is UNGATED (plain unbounded `Integer`, no Word64 check) in that
binary EVEN AT PV11/vanRossem.** The Word64 restriction is real upstream
plutus behavior but is not yet what dugite's actual live preview/mainnet
peers enforce as of cardano-node 11.0.1. It will land whenever a later
cardano-node point release (11.0.2 / 11.1.0 / etc.) bumps its
plutus-ledger-api dependency past 1.63.0.0.

This was NOT verified against a live node (no cabal.project.freeze/plan.json
was available in the git tag to give 100% certainty of the exact resolved
version) — it's inferred from GH release-date ordering + CHaP index-state
semantics, which is strong but not airtight evidence. If it ever matters for
an actual consensus-critical fix, the decisive test is to run
`cardano-cli conway transaction evaluate` (or equivalent phase-2 eval) against
a real cardano-node 11.0.1 binary with a script that calls
`constrData (-1) []`, or grep the binary's embedded package DB /
`cardano-node --version` build info for the linked `plutus-ledger-api`
version.

## 6. `unConstrData` — NEVER gated, in either version

`toBuiltinMeaning _semvar UnConstrData` (both 1.62.0.0 line ~1513 and
1.63.0.0+ line ~1780, `_semvar` unused in BOTH):
```haskell
unConstrDataDenotation :: Data -> BuiltinResult (Integer, [Data])
unConstrDataDenotation = \case
  Constr i ds -> pure (i, ds)
  _ -> fail "Expected the Constr constructor but got a different one"
```
Tag flows back out as a plain, unbounded `Integer` unconditionally — a
negative/huge tag (however it got into a `Data` value — only possible via the
ungated `constrData` builtin, since CBOR/Flat decode always requires Word64
per §2-3) round-trips losslessly through `unConstrData`, `equalsData`
(structural `(==)` on `Data`, no bound anywhere), in every plutus version and
every protocol version. The Word64 restriction, when/where it applies, is
ONE-DIRECTIONAL: it only gates construction, never deconstruction.

## 7. Unlifting-failure classification for the Word64 gate (when it IS active)

`Word64`'s `ReadKnownIn` instance = `readKnownAsInteger`
(`PlutusCore/Default/Universe.hs`), bounds-checks against
`(minBound::Word64=0, maxBound::Word64=2^64-1)`; out-of-range throws
`operationalUnliftingError` (`PlutusCore/Builtin/Result.hs`):
```haskell
operationalUnliftingError = BuiltinUnliftingEvaluationError . MkUnliftingEvaluationError . OperationalError . MkUnliftingError
```
— classified `OperationalError` (not `StructuralError`), same
`BuiltinUnliftingEvaluationError` family as other bound-checked-primitive
unlifting failures (Int/Int8../Word/Word8..). Since `constrData` takes
exactly 2 args, unlifting happens as soon as it's fully saturated (deferred/
call-by-name unlifting per `Meaning.hs`) — a script calling
`constrData (con integer -1) (con (list data) [])` where the gate IS active
is a full CEK evaluation failure (phase-2, script aborts, not catchable),
exactly like any other builtin unlifting failure.

## Dugite translation notes (issue likely adjacent to #821/ConstrData work)

- Rust `Data::Constr(u64, Vec<Data>)` is CORRECT and lossless for anything
  that ever reaches the chain via CBOR (datums/redeemers) or Flat (embedded
  script literals) — §2/§3 confirm the Word64 bound is unconditional there,
  in every plutus version ever checked.
- The ONLY place a `u64` representation would clip Haskell's real in-memory
  value is: (a) a bare-metal CEK evaluation of `constrData` with an
  out-of-Word64 Integer tag, on a plutus-ledger-api build OLDER than 1.63.0.0
  (§5) — where Haskell would SUCCEED and build `Constr <huge-or-negative> []`
  in memory; a Rust `u64`-tagged `Constr` cannot represent that value at all
  and must not silently wrap/truncate it (`Data::Constr` should probably be
  `Integer`-backed, or minimum: `i128`/`BigInt`-backed, if dugite wants to
  replicate pre-1.63 constrData semantics byte-exactly for the actual
  cardano-node 11.0.1 binary it targets).
- Practical recommendation given the ambiguity: dugite's own conformance
  corpus (`tests/conformance/upstream/sources.toml`, `[plutus] tag =
  "1.65.0.0"`) is pinned to the GATED version — so dugite's CEK should
  implement the Word64-gate-at-PV>=11 behavior to pass its own corpus and to
  match where cardano-node itself is clearly heading; but be aware this is
  presently AHEAD of (not identical to) the exact cardano-node 11.0.1 binary
  actually running on preview/mainnet peers right now. Flag this explicitly
  if a real byte-exact divergence investigation (issue/PR) hinges on it —
  don't assume the gate is "obviously" live on the network just because PV=11
  is active; PV number and plutus-ledger-api patch version are two
  independently-versioned things that can (and did, here) drift out of sync.
- See also `plutus-builtins-adversarial-audit.md` §3 (older note, was correct
  about 1.65.0.0 behavior but did not know about the 1.62/1.63 version-
  boundary gap — read this file alongside it, this one supersedes it on the
  "is the gate actually live in 11.0.1" question).
