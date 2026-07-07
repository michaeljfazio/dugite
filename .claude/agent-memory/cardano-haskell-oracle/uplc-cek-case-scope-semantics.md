---
name: uplc-cek-case-scope-semantics
description: checkScope eagerness/blind-spot, De Bruijn index-0 sentinel, Case/Constr CEK frame semantics, builtin-constant casing gate (vanRossemPV=PV11), MissingCaseBranch/NonConstrScrutinized errors
type: reference
---

Source: IntersectMBO/plutus @ master (fetched 2026-07-04). All line numbers refer to that snapshot.

## 1. Scope check (free-variable) eagerness — has a real Constr/Case blind spot

`plutus-ledger-api/src/PlutusLedgerApi/Common/Eval.hs` `mkTermToEvaluate` (~line 124):
```haskell
-- make sure that term is closed, i.e. well-scoped
through (liftEither . first DeBruijnError . UPLC.checkScope) appliedT
```
This runs ONCE, after args are applied (`mkIterAppNoAnn`), BEFORE `evaluateTerm`/CEK ever
runs. It is a distinct `EvaluationError` constructor `DeBruijnError !FreeVariableError`
(sibling to `CekError`), but both flow through the same `Either EvaluationError ExBudget`
that `evaluateScriptRestricting`/`evaluateScriptCounting` return — i.e. **on-chain this is
a phase-2 failure (collateral consumed), NOT a decode/phase-1 error**, despite a code
comment saying it "really" should be phase-1 and may move later:
> "Since long ago this check has been in `mkTermToEvaluate`, which makes it a phase 2
> failure... For now we keep it as it is, but we may try to move it later." (Eval.hs, Note
> [Checking the Plutus Core language version] — comment sits next to the LL-version check
> but the note about phase-2-vs-phase-1 applies to checkScope's placement too)

`checkScope` itself: `plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Check/Scope.hs`
(56 lines total, last logic change 2022 commit 8bd820c4be "SCP-3315", predates Constr/Case
which landed ~2023 for PlutusV3):
```haskell
checkScope = go 0
  where
    go !lvl = \case
      Var _ n -> do
        let i = n ^. index
        unless (i > 0 && fromIntegral i <= lvl) $ throwError $ FreeIndex i
      LamAbs _ binder t -> do
        let bIx = binder ^. index
        unless (bIx == 0) $ throwError $ FreeIndex bIx
        go (lvl + 1) t
      Apply _ t1 t2 -> go lvl t1 >> go lvl t2
      Force _ t -> go lvl t
      Delay _ t -> go lvl t
      _ -> pure ()   -- Constr, Case, Constant, Builtin, Error all fall here!
```
**CRITICAL FINDING**: the pattern match only descends into `Var`/`LamAbs`/`Apply`/`Force`/
`Delay`. `Constr` args and `Case` scrutinee+branches are NOT traversed — they silently hit
`_ -> pure ()`. So:
- For the user's literal question (free var inside an unforced `Delay` or unused lambda
  arg): YES, still eagerly caught — `go` recurses into `Delay` bodies and `LamAbs` bodies
  unconditionally regardless of whether ever forced/applied.
- BUT a free variable buried inside a `Constr` argument or a `Case` branch (including one
  nested under further Delay/Lambda) is NEVER checked by `checkScope`, and if that branch
  is never dynamically selected during CEK evaluation, it is **never detected at all** —
  the script just evaluates to success. If dugite recurses into Constr/Case when doing its
  own eager scope check (the "obviously more correct" thing to do), it will REJECT scripts
  that real cardano-node ACCEPTS — a hard consensus divergence risk. Byte-exact requires
  replicating the Haskell blind spot, not fixing it.
- If a free var inside Constr/Case IS dynamically reached by CEK, the runtime lookup
  (`lookupVarName` in Cek/Internal.hs ~line 1082) throws `OpenTermEvaluatedMachineError`
  (a `StructuralError`, i.e. `CekError`, not `DeBruijnError`) — different error constructor
  than the static check would have produced, though both are phase-2 failures either way.

Note: `Constr`'s CEK semantics (`computeCek` for `Constr _ i es`, Cek/Internal.hs ~864) is
STRICT/eager in ALL its immediate arguments (evaluates es left-to-right before returning
`VConstr`) — so a free var directly as a Constr arg (not nested under Delay/unreached Case
branch) WILL be dynamically hit regardless of the static blind spot. The truly-invisible
case requires the free var to be nested under something lazy (Delay, or an unreached Case
branch/lambda) that is itself inside a Constr/Case subtree.

## 2. De Bruijn index 0 is a sentinel, never a valid Var reference

From `checkScope`: `Var` requires `i > 0` (index 0 rejected as free/invalid); `LamAbs`
binder requires `bIx == 0` exactly (any other binder index value is invalid). Convention:
index 1 = nearest enclosing lambda, increasing outward. On the wire, binders (`LamAbs`)
carry NO index at all (Flat/CBOR only encodes indices for `Var` nodes) — decode always
synthesizes `FakeNamedDeBruijn`/binder with index 0 (see `PlutusCore/DeBruijn/Internal.hs`
`deBruijnInitIndex = 0`, and `toFake`/`fakeNameDeBruijn`). `unDeBruijnTermWithM` (the Name-
producing direction, used for pretty-printing/debug, NOT the on-chain path — CEK operates
directly on `NamedDeBruijn` terms) even forcibly overwrites the binder's stored index to 0
before converting (`set index deBruijnInitIndex n`), ignoring whatever was on the wire —
this is why `checkScope` is documented as "stricter" than `unDeBruijnTerm`'s own indirect
scope check (which doesn't care what the binder index actually was).
`UntypedPlutusCore/DeBruijn.hs` unDeBruijnTermWithM DOES recurse fully into Constr/Case
(unlike checkScope) — but this function is irrelevant to on-chain evaluation since the CEK
machine's `NTerm uni fun = Term NamedDeBruijn uni fun` never gets un-De-Bruijned.

## 3. Casing on builtin-constant (VCon) scrutinees — supported, PV11+ only

`returnCek (FrameCases env cs ctx) e` in Cek/Internal.hs (~line 931-950):
```haskell
returnCek (FrameCases env cs ctx) e = case e of
  (VConstr i _) | i > fromIntegral @Int @Word64 maxBound ->
      throwErrorDischarged (StructuralError (MissingCaseBranchMachineError i)) e
  (VConstr i args) -> case (V.!?) cs (fromIntegral i) of
    Just t -> case args of
      EmptyStack -> computeCek ctx env t
      MultiStack rest -> computeCek (FrameAwaitFunValueN rest ctx) env t
    Nothing -> throwErrorDischarged (StructuralError $ MissingCaseBranchMachineError i) e
  VCon val -> case unCaserBuiltin ?cekCaserBuiltin val cs of
    HeadError err -> throwErrorDischarged (OperationalError $ CekCaseBuiltinError err) e
    HeadOnly fX -> computeCek ctx env fX
    HeadSpine f xs -> computeCek (FrameAwaitFunConN xs ctx) env f
  _ -> throwErrorDischarged (StructuralError NonConstrScrutinizedMachineError) e
```
So `NonConstrScrutinizedMachineError` ("A non-constructor/non-builtin value was scrutinized
in a case expression" — Exception.hs line ~120) is thrown ONLY for `VDelay`/`VLamAbs`/
`VBuiltin` scrutinees — VCon (builtin constants) get a dedicated path via `CaserBuiltin`.

**PV gate**: `PlutusLedgerApi/V3/EvaluationContext.hs` `mkEvaluationContext`:
```haskell
( \pv -> if pv < vanRossemPV
          then unavailableCaserBuiltin $ getMajorProtocolVersion pv
          else CaserBuiltin caseBuiltin )
```
`vanRossemPV = MajorProtocolVersion 11` (`Common/ProtocolVersions.hs`; PV table: shelley=2,
allegra=3, mary=4, alonzo=5, vasil=7, valentine=8, chang=9, plomin=10, vanRossem=11=newestPV
as of this snapshot). Below PV11, `unavailableCaserBuiltin` makes EVERY builtin-constant
case fail with `HeadError "'case' on values of built-in types is not supported in protocol
version N"` regardless of scrutinee type. **At/above PV11 (vanRossem) it is live** — this
is the SAME threshold that flips `DefaultFunSemanticsVariantC` → `...VariantE` for V3. Per
dugite's CLAUDE.md, preview testnet is already at PV11, so this is a LIVE feature for
preview validation, not a future/master-only one. (Only checked for PlutusV3's
`EvaluationContext`; V1/V2 EvaluationContexts don't wire a caser at all since V1/V2 scripts
can't contain `Case`/`Constr` AST nodes until PV11 either — see plc-version gating below.)

Exact per-type projection (`instance CaseBuiltin DefaultUni` in
`plutus-core/plutus-core/src/PlutusCore/Default/Universe.hs` ~line 914-947):
- **Unit**: requires exactly 1 branch (else `HeadError`); `HeadOnly branches[0]` (no args).
- **Bool**: `False` → `HeadOnly branches[0]` if len∈{1,2}; `True` → `HeadOnly branches[1]`
  if len==2 (True with len==1 is `HeadError`). The len==1/False-only form is an intentional
  size optimization (comment: "as long as the scrutinee is False... to save size by not
  having the True branch if it was gonna be Error anyway").
- **Integer**: `HeadOnly branches[x]` requires `0 <= x < len`, direct 0-based index; else
  `HeadError` (out-of-bounds message via `outOfBoundsErr`).
- **List (ProtoList)**: len==1 (cons-only form): empty list → `HeadError` ("Expected
  non-empty list, got empty list for casing list"); non-empty `(y:ys)` →
  `headSpine branches[0] [someValueOf elemTy y, someValueOf listTy ys]` (branch applied to
  head then tail as 2 constant args). len==2 (cons+nil): empty → `HeadOnly branches[1]`;
  non-empty → same headSpine on branches[0]. Any other len → `HeadError`.
- **Pair (ProtoPair)**: requires exactly 1 branch; `(l,r)` →
  `headSpine branches[0] [someValueOf tyL l, someValueOf tyR r]`.
- Any other builtin type (ByteString, String, Data, BLS12-381 points, Array, ...) →
  `HeadError "<uni> isn't supported in 'case'"` — casing is ONLY defined for
  Unit/Bool/Integer/List/Pair.
- `HeadSpine`/`headSpine` (KnownType.hs): `HeadOnly a` = no extra application; `HeadSpine a
  (Spine b)` = branch term must be applied (via `FrameAwaitFunConN`) to the produced
  constant args in order — i.e. casing on a builtin constant is implemented as "select
  branch, then treat it as a function and apply constant args to it", NOT substitution.

plc language-version gate for Constr/Case AST nodes existing at all (`Common/Versions.hs`
`plcVersionsIntroducedIn`): `plcVersion110` (which is required for Constr/Case to even
decode) is available for PlutusV3 since `changPV` (PV9, Chang/Conway from genesis) but only
from `vanRossemPV` (PV11) for PlutusV1/V2. So V1/V2 scripts using Case/Constr are impossible
before PV11 regardless of the builtin-casing question.

## 4. Case branch indexing / MissingCaseBranch / no arity check

From the same `returnCek (FrameCases ...)` clause above: `(V.!?) cs (fromIntegral i)`
(`Data.Vector`'s safe/Maybe indexing) — if `i >= length cs`, returns `Nothing` →
`throwErrorDischarged (StructuralError $ MissingCaseBranchMachineError i) e`.
`MissingCaseBranchMachineError Word64` (`PlutusCore/Evaluation/Machine/Exception.hs` line
61): "An attempt to go into a non-existent case branch" / pretty: "Case expression missing
the branch required by the scrutinee tag: i". There is a separate defensive guard for
`i > fromIntegral @Int @Word64 maxBound` (Int64 overflow) that also throws the same error
constructor — comment notes this branch can never actually trigger since Word64 max wraps
to -1 as Int64, so no value can look "apparently good" after overflow.
**No arity check exists anywhere**: the runtime ONLY checks `i < length cs` (index in
range). There is no validation that `length cs` equals any constructor's "expected" arity,
nor that a `VConstr`'s stored arg-count matches what the selected branch function expects —
if the branch under-consumes or over-consumes the applied args (via `FrameAwaitFunValueN`),
that surfaces later as a generic `NonFunctionalApplicationMachineError` (over-application)
or a stuck partial application, not a dedicated arity-mismatch error.
Decode-time bound (NOT an arity check — bounds the Constr TAG value, not branch count or
arg count): `PlutusLedgerApi/Common/SerialisedScript.hs` `scriptCBORDecoder`'s `checkConstr
n | n <= maxBoundConstr = Nothing | otherwise = Just "..."`, where `maxBoundConstr =
mbConstr (maxBoundsByPV pv)` and `maxBoundsByPV pv = if pv >= vanRossemPV then MaxBounds
{mbHeader=32, mbConstr=1024} else MaxBounds {mbHeader=maxBound, mbConstr=maxBound}`
(Common/Versions.hs ~line 369). So pre-PV11 a `Constr`'s tag `i` is CBOR-decode-unbounded;
from PV11 on, any `Constr ann i es` with `i > 1024` fails to decode (phase-1/decode error,
`ScriptDecodeError`, not a MachineError). This is a separate mechanism from the CEK's
runtime `MissingCaseBranchMachineError` (which fires on `Case`'s branch-vector lookup, not
on `Constr`'s tag decode).

Flat wire tags confirmed in `UntypedPlutusCore/Core/Instance/Flat.hs`: `termTagWidth = 4`
bits; `Constr` = tag 8, `Case` = tag 9 (encoded as
`encodeTermTag 8 <> encode ann <> encode i <> encodeListWith encodeTerm es` /
`encodeTermTag 9 <> encode ann <> encodeTerm arg <> encodeListWith encodeTerm (V.toList cs)`).

## Dugite relevance
dugite-uplc's CEK machine and scope-checking pass (if any) must replicate the Constr/Case
blind spot in point 1 exactly, or it risks a consensus-critical false-rejection of a
mainnet-valid tx. Check `crates/dugite-uplc` for any eager well-scopedness pass and verify
it matches `checkScope`'s exact traversal (Var/LamAbs/Apply/Force/Delay only). Also verify
dugite's Case-on-VCon support is gated on major protocol version >= 11 (not on ledger
language alone), matching the exact per-type projection table above.
