---
name: hardfork-pvcanfollow-exact-mechanics
description: Byte-exact, raw-source-verified (not model-paraphrased) mechanics of pvCanFollow, preceedingHardFork, and ProposalCantFollow in Conway GOV — corrects an imprecise hypothetical in conway-ratify-precision-facts.md #2
metadata:
  type: reference
---

Verified 2026-07-06 by curling the raw files directly (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, `eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs` at IntersectMBO/cardano-ledger master) and reading exact line ranges — not via a summarizing fetch. This supersedes the imprecise example in [[conway-ratify-precision-facts]] point 2.

## `pvCanFollow` — `Shelley/PParams.hs:299-307`, unchanged from prior verification

```haskell
pvCanFollow ::
  -- | Previous protocol version
  ProtVer ->
  -- | New protocol version
  ProtVer ->
  Bool
pvCanFollow (ProtVer curMajor curMinor) (ProtVer newMajor newMinor) =
  (succVersion curMajor, 0) == (Just newMajor, newMinor)
    || (curMajor, curMinor + 1) == (newMajor, newMinor)
```
Same-major bump requires `newMinor == curMinor + 1` EXACTLY (not `>`). Major bump requires `newMajor == succVersion curMajor` (i.e. curMajor+1, capped by the era's static max) AND `newMinor == 0` exactly.

## `preceedingHardFork` — `Conway/Rules/Gov.hs:673-694`, verbatim

```haskell
-- | If the GovAction is a HardFork, then return 3 things (if they exist)
-- 1) The (StrictMaybe GovPurposeId), pointed to by the HardFork proposal
-- 2) The proposed ProtVer
-- 3) The ProtVer of the preceeding HardFork
-- If it is not a HardFork, or the previous govActionId points to something other
-- than  HardFork, return Nothing. It will be verified with another predicate check.
preceedingHardFork ::
  EraPParams era =>
  PParams era ->
  GovRelation StrictMaybe ->
  Proposals era ->
  GovAction era ->
  Maybe (StrictMaybe (GovPurposeId 'HardForkPurpose), ProtVer, ProtVer)
preceedingHardFork pp pgaids ps = \case
  HardForkInitiation mPrev newProtVer
    | mPrev == pgaids ^. grHardForkL
        -- If major version is too high, then we need to compare it to the current protocol version,
        -- instead of the one in previous action, since major versions that are at least one higher
        -- than current are not allowed.
        --
        -- It is statically guaranteed for `succVersion` to produce `Just` in this case, since we
        -- always have next major version available for any `Era`.
        || Just (pvMajor newProtVer) > succVersion (pvMajor (pp ^. ppProtocolVersionL)) ->
        Just (mPrev, newProtVer, pp ^. ppProtocolVersionL)
    | otherwise -> do
        SJust (GovPurposeId prevGovActionId) <- Just mPrev
        HardForkInitiation _ prevProtVer <- gasAction <$> proposalsLookupId prevGovActionId ps
        Just (mPrev, newProtVer, prevProtVer)
  _ -> Nothing
```
(`_ -> Nothing` line for non-HardFork actions inferred from the Haddock comment and confirmed by call-site usage — the function is a `Maybe`-returning helper only invoked for the current action under validation.)

**Base resolution has THREE cases, not two:**
1. `mPrev == pgaids ^. grHardForkL` (matches the currently-ENACTED root for the HardFork purpose, i.e. no not-yet-enacted ancestor) → base = live `pp ^. ppProtocolVersionL` (current on-chain PParams).
2. `mPrev /= root` BUT `newMajor > succVersion(curMajor)` (proposed major already jumps more than one step past CURRENT) → base is STILL forced to live current PParams version, **not** the chain. This is a deliberate short-circuit: it guarantees `pvCanFollow` fails for any proposal attempting to leapfrog more than one major version ahead of the currently-enacted version, and it means Haskell does **not** allow compounding two separate major-version bumps by chaining a second HardForkInitiation onto a still-in-flight (unenacted) major-bump proposal within the same live proposal set.
3. Otherwise (`mPrev /= root` and the major-jump is plausible, i.e. `newMajor <= succVersion(curMajor)`) → do-block in the `Maybe` monad: look up `prevGovActionId` via `proposalsLookupId` in the live `Proposals` OMap; if that action exists AND is itself a `HardForkInitiation`, base = **that proposal's own target ProtVer**. If the lookup fails (id not found) or the referenced action is not a `HardForkInitiation` (wrong purpose), the pattern match fails in the `Maybe` monad → `preceedingHardFork` returns `Nothing` for the whole function → no `ProposalCantFollow` is raised from this check at all (see call site below); that malformed-ancestor case is instead caught by the separate, generic `proposalsAddAction`/`InvalidPrevGovActionId` structural check later in `processProposal` (Gov.hs:562-566).

### Worked examples (byte-verified against the guard above)
- Current `(9,0)`. Proposal A: `prevGovActionId=SNothing` (root), target `(10,0)`. Matches disjunct 1 (`mPrev==root`) → base=current `(9,0)` → `pvCanFollow (9,0) (10,0)` = True (major-bump branch) → **A is valid**.
- While A is still un-enacted/in-flight, Proposal B: `prevGovActionId=A`, target `(10,1)`. `mPrev/=root`. Is `newMajor(10) > succVersion(curMajor=9)=10`? No (not strictly greater) → disjunct 2 does NOT fire → falls to case 3 (chain lookup): finds A, A's action is `HardForkInitiation _ (10,0)` → base = `(10,0)` → `pvCanFollow (10,0) (10,1)` = True (minor-bump branch) → **B is valid** (legitimate minor-bump chained onto an in-flight major-bump).
- Same setup, but B instead targets `(11,0)` (attempting to chain a SECOND major bump onto A before A is enacted). `mPrev/=root`. Is `newMajor(11) > succVersion(9)=10`? **Yes** → disjunct 2 fires → base is forced to current `(9,0)`, chain lookup is bypassed entirely → `pvCanFollow (9,0) (11,0)` = False (major jumps by 2) → **B is rejected** with `ProposalCantFollow`.

This third example corrects an earlier, imprecise memory (see below) which claimed the opposite outcome for this exact shape of scenario.

## Predicate failure — `ConwayGovPredFailure` constructor, `Conway/Rules/Gov.hs:193-199` (verbatim)

```haskell
| InvalidPrevGovActionId (ProposalProcedure era)
| VotingOnExpiredGovAction (NonEmpty (Voter, GovActionId))
| ProposalCantFollow
    -- | The PrevGovActionId of the HardForkInitiation that fails
    (StrictMaybe (GovPurposeId 'HardForkPurpose))
    -- | Its protocol version and the protocal version of the previous gov-action pointed to by the proposal
    (Mismatch RelGT ProtVer)
```
`Mismatch RelGT ProtVer` has fields `mismatchSupplied :: ProtVer` (the new/proposed version) and `mismatchExpected :: ProtVer` (the resolved base/prev version per `preceedingHardFork`'s 3-way resolution above). CBOR: `ProposalCantFollow` is sum-tag **10** (`SumD ProposalCantFollow <! From <! FromGroup` / encode `Sum ProposalCantFollow 10 !> To prevgaid !> ToGroup mm`); `InvalidPrevGovActionId` is sum-tag **8**.

## Call site — `Conway/Rules/Gov.hs:483-499` (verbatim, inside `processProposal`)

```haskell
let processProposal proposals (idx, proposal@ProposalProcedure {..}) = do
      runTest $ checkBootstrapProposal pp proposal

      let newGaid = GovActionId txid idx

      -- In a HardFork, check that the ProtVer can follow
      let badHardFork = do
            (prevGaid, newProtVer, prevProtVer) <-
              preceedingHardFork @era pp prevGovActionIds proposals pProcGovAction
            guard (not (pvCanFollow prevProtVer newProtVer))
            Just $
              ProposalCantFollow @era prevGaid $
                Mismatch
                  { mismatchSupplied = newProtVer
                  , mismatchExpected = prevProtVer
                  }
      failOnJust badHardFork injectFailure

      -- PParamsUpdate well-formedness check
      runTest $ actionWellFormed (pp ^. ppProtocolVersionL) pProcGovAction
      ... -- (bootstrap-gated account/deposit/network-id/treasury/committee checks, unrelated)

      -- Ancestry checks and accept proposal
      let expiry = pp ^. ppGovActionLifetimeL
          actionState = mkGovActionState newGaid proposal expiry currentEpoch
       in case proposalsAddAction actionState proposals of
            Just updatedProposals -> pure updatedProposals
            Nothing -> proposals <$ failBecause (injectFailure $ InvalidPrevGovActionId proposal)
```

**Exact ordering within `processProposal` (one iteration per `ProposalProcedure` in the tx body, folded via `foldlM'` over `st` — i.e. per-transaction, GOV rule, BLOCK-APPLY time, not RATIFY/ENACT):**
1. `checkBootstrapProposal` — `DisallowedProposalDuringBootstrap` if PV<10 (bootstrap, major=9) and action type isn't one of `ParameterChange`/`HardForkInitiation`/`InfoAction`. HardForkInitiation is explicitly ALLOWED during bootstrap, and receives NO special-cased version-follow logic — `pvCanFollow`/`preceedingHardFork` are invoked identically in and out of bootstrap. This directly answers "any bootstrap interaction": **none, for the version-follow check itself.**
2. `badHardFork`/`ProposalCantFollow` — the HF-specific version-follow check (this is the ONLY place `pvCanFollow` is invoked for proposal submission — note `hasLegalProtVerUpdate` in `PParams.hs:311-316` is a different call site used for pre-Conway `Update`/PPUP-style protocol-version bumps, `AtMostEra "Babbage"`-gated, not used in Conway GOV).
3. `actionWellFormed` — `MalformedProposal` (PParamsUpdate structural well-formedness, unrelated to HF).
4. Bootstrap-gated return-account-registration + treasury/committee-specific checks (unrelated to HF).
5. Deposit check, network-id checks (unrelated to HF).
6. **LAST**: `proposalsAddAction` — the generic structural ancestry check (does `prevGovActionId`, for ANY purpose including HardFork, resolve to either the currently-enacted root OR an existing live node in that purpose's proposal-forest?). Raises `InvalidPrevGovActionId` on failure. This is INDEPENDENT of and runs AFTER the HF-specific check — a HardFork proposal whose `prevGovActionId` doesn't resolve via `preceedingHardFork`'s chain lookup (non-existent id, or id of a non-HardFork action) produces `Nothing` from `badHardFork` (no `ProposalCantFollow`), and is instead caught here by `InvalidPrevGovActionId`.

## Answers to the 5 numbered questions (for cross-reference)
1. `pvCanFollow` (Shelley/PParams.hs:299-307, quoted above) called from `badHardFork` inside `processProposal` (Gov.hs:483-499).
2. Base version resolution: root-match → live current PParams; non-root AND major-jump already implausible (`>succVersion(cur)`) → ALSO live current PParams (guard/short-circuit, NOT the in-flight parent); non-root AND major-jump plausible → in-flight parent's own target ProtVer via `proposalsLookupId`. Non-HardFork prevGovActionId reference or SNothing-but-mismatched-root cases fall through to `InvalidPrevGovActionId` via the separate `proposalsAddAction` structural check, not `ProposalCantFollow`.
3. `ProposalCantFollow (StrictMaybe (GovPurposeId 'HardForkPurpose)) (Mismatch RelGT ProtVer)` — carries the prevGovActionId AND both ProtVers (`mismatchSupplied`=new, `mismatchExpected`=resolved base). CBOR sum-tag 10.
4. GOV rule, per-transaction, BLOCK-APPLY/proposal-submission time. Confirmed — not RATIFY/ENACT.
5. No special-casing: HardForkInitiation is allowed during bootstrap (PV9) by `checkBootstrapProposal`, and once past that gate, `pvCanFollow`/`preceedingHardFork` run unchanged.
6. Version-follow rule is exactly `(curMajor+1, 0)` OR `(curMajor, curMinor+1)` relative to the resolved base — never `newMinor > curMinor` (any gap).

## Related
- [[conway-ratify-precision-facts]] point 2 — superseded by this file's more precise 3-way base resolution and corrected worked example.
- [[project_dugite_ratify_audit_divergences_2026_07_04]] divergence 2 — the illustrative example there ("A targets (10,0), B chains off A targeting (11,0), passes") is WRONG per this verification; that exact scenario is REJECTED by Haskell too (via the `succVersion` short-circuit). The real, still-valid form of divergence 2 is: dugite has no chain-lookup at all, so a legitimate minor-bump-chained-onto-an-in-flight-major-bump (e.g. A:(9,0)->(10,0) root-anchored valid, B chains off A targeting (10,1)) is INCORRECTLY REJECTED by dugite (which always compares against live current PParams (9,0), not A's target (10,0)) even though Haskell ACCEPTS it. Divergence 1 (minor-value laxity, `tgt_minor > cur_minor` vs exact `+1`) is unaffected by this correction and remains accurate.
