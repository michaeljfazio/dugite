---
name: conway-futurepparams-lifecycle-and-json
description: FuturePParams (ConwayGovState.cgsFuturePParams / query gov-state "futurePParams" key) - exact 3-constructor type, EncCBOR/DecCBOR Sum tags 0/1/2, aeson JSON shape, and the full per-block/per-epoch-boundary lifecycle (predictFuturePParams vs solidifyFuturePParams, point-of-no-return = firstSlotNextEpoch - 2*stabilityWindow). Live-verified 2026-08-02 at HEAD 4f7cb2d6874df70561e32147084ed82cee773e8a.
metadata:
  type: reference
---

Triggered by a dugite devnet-validate divergence: dugite's `GetGovState` N2C encoder hardcodes
`FuturePParams = NoPParamsUpdate` unconditionally, and a live cardano-node 11.0.1 comparison showed a
different value for that key after a TreasuryWithdrawals action enacted.

Files: `libs/cardano-ledger-core/src/Cardano/Ledger/State/Governance.hs` (type + instances),
`eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs` (`ConwayGovState`, `predictFuturePParams`),
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs` (epoch-boundary reset),
`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs` (per-block re-predict + fresh-pulser
call site), `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Tick.hs` (`solidifyNextEpochPParams`),
`libs/cardano-ledger-core/src/Cardano/Ledger/Slot.hs` (`getTheSlotOfNoReturn`),
`libs/cardano-ledger-api/golden/conway/json/queryGovState.json` (golden JSON fixture — this file
literally IS the ground truth cardano-cli output shape, use it directly for any future JSON-parity
question, don't just trust the derived-Generic reasoning).

## 1. Type + CBOR (verbatim, `State/Governance.hs:90-166`)

```haskell
data FuturePParams era
  = NoPParamsUpdate
  | DefinitePParamsUpdate !(PParams era)
  | PotentialPParamsUpdate (Maybe (PParams era))   -- field is LAZY on purpose
  deriving (Generic)

instance Default (FuturePParams era) where
  def = NoPParamsUpdate

instance EncCBOR (PParams era) => EncCBOR (FuturePParams era) where
  encCBOR =
    encode . \case
      NoPParamsUpdate -> Sum NoPParamsUpdate 0
      DefinitePParamsUpdate pp -> Sum DefinitePParamsUpdate 1 !> To pp
      PotentialPParamsUpdate pp -> Sum PotentialPParamsUpdate 2 !> To pp

instance (Typeable era, DecCBOR (PParams era)) => DecCBOR (FuturePParams era) where
  decCBOR = decode . Summands "FuturePParams" $ \case
    0 -> SumD NoPParamsUpdate
    1 -> SumD DefinitePParamsUpdate <! From
    2 -> SumD PotentialPParamsUpdate <! From
    k -> Invalid k

solidifyFuturePParams :: FuturePParams era -> FuturePParams era
solidifyFuturePParams = \case
  PotentialPParamsUpdate Nothing -> NoPParamsUpdate
  PotentialPParamsUpdate (Just pp) -> DefinitePParamsUpdate pp
  fpp -> fpp   -- No/Definite are already-terminal, idempotent
```
Wire CBOR (`Cardano.Ledger.Binary.Coders` `Sum`/`SumD` convention): a 2-element array
`[tag, payload...]` for non-nullary, bare `[tag]` (single-elem array, just the constructor-tag int)
for `NoPParamsUpdate`. Tags 0/1/2 exactly as above — no other constructors exist, `k -> Invalid k` for
anything else.

`instance ToJSON (PParams era) => ToJSON (FuturePParams era)` has NO explicit method body — it uses
aeson's `default toJSON = genericToJSON defaultOptions` via the `deriving (Generic)` on the type.
`defaultOptions.sumEncoding = TaggedObject {tagFieldName="tag", contentsFieldName="contents"}`, and
since NOT all constructors are nullary, `allNullaryToStringTag` doesn't collapse anything. Confirmed
byte-exact against `libs/cardano-ledger-api/golden/conway/json/queryGovState.json`:
- `NoPParamsUpdate` -> `{"tag":"NoPParamsUpdate"}` (no `"contents"` key at all — TaggedObject omits it
  for zero-field constructors).
- `DefinitePParamsUpdate pp` -> `{"tag":"DefinitePParamsUpdate","contents":{...full PParams object...}}`.
- `PotentialPParamsUpdate Nothing` -> `{"tag":"PotentialPParamsUpdate","contents":null}` (not in the
  golden fixture verbatim but structurally certain: `Maybe`'s `ToJSON` is `Nothing -> Null`).
- `PotentialPParamsUpdate (Just pp)` -> `{"tag":"PotentialPParamsUpdate","contents":{...PParams...}}`.

## 2. Field position in `ConwayGovState` (`Conway/Governance.hs:243-259`, 7-field record, CBOR array(7))

```haskell
data ConwayGovState era = ConwayGovState
  { cgsProposals :: !(Proposals era)                  -- field 0 / CBOR index 0
  , cgsCommittee :: !(StrictMaybe (Committee era))     -- field 1
  , cgsConstitution :: !(Constitution era)             -- field 2
  , cgsCurPParams :: !(PParams era)                    -- field 3
  , cgsPrevPParams :: !(PParams era)                   -- field 4
  , cgsFuturePParams :: !(FuturePParams era)            -- field 5  <-- this one
  , cgsDRepPulsingState :: !(DRepPulsingState era)     -- field 6
  }
```
`EncCBOR`/`DecShareCBOR` (`Conway/Governance.hs:337-369`) confirm the record order IS the CBOR field
order — `decodeRecordNamedT "ConwayGovState" (const 7)`, fields decoded/encoded in exactly the above
sequence (`cgsFuturePParams` is the 6th of 7 array elements, 0-indexed slot 5).

JSON rendering is a HAND-WRITTEN `ToKeyValuePairs` instance, NOT auto-derived from record order —
key ORDER in the JSON object differs from CBOR field order (`Conway/Governance.hs:393-403`):
```haskell
toKeyValuePairs cg@(ConwayGovState _ _ _ _ _ _ _) =
  let ConwayGovState {..} = cg
   in [ "proposals" .= cgsProposals
      , "nextRatifyState" .= extractDRepPulsingState cgsDRepPulsingState
      , "committee" .= cgsCommittee
      , "constitution" .= cgsConstitution
      , "currentPParams" .= cgsCurPParams
      , "previousPParams" .= cgsPrevPParams
      , "futurePParams" .= cgsFuturePParams
      ]
```
JSON key is exactly `"futurePParams"` (matches the field the user is diffing against). Note also:
`"nextRatifyState"` is a SEPARATE key computed by force-completing the pulser
(`extractDRepPulsingState`) — do not confuse the two; `futurePParams` is the lazy
predict/solidify-driven value, `nextRatifyState` is always-fully-forced.

## 3. Full lifecycle (the part that matters for the bug)

**Epoch-boundary reset — unconditional, regardless of WHICH action type enacted**
(`Conway/Rules/Epoch.hs:296-325`, inside `EPOCH`'s `epochTransition`):
```haskell
ratifyState@RatifyState {rsEnactState, rsEnacted, rsExpired} = extractDRepPulsingState pulsingState
...
govState1 =
  govState0
    & cgsProposalsL .~ newProposals
    & cgsCommitteeL .~ ensCommittee
    & cgsConstitutionL .~ ensConstitution
    & cgsCurPParamsL .~ nextEpochPParams govState0     -- reads OLD (pre-reset) futurePParams
    & cgsPrevPParamsL .~ curPParams
    & cgsFuturePParamsL .~ PotentialPParamsUpdate Nothing   -- <-- ALWAYS this, every boundary
```
This line fires at EVERY epoch boundary no matter what enacted (TreasuryWithdrawals, ParameterChange,
NoConfidence, or nothing at all) — it is not conditioned on the action type. `hasChangesToPParams`
(next point) is what later decides whether it turns into `Just pp`.

**Per-block re-prediction during the epoch** (`Conway/Rules/NewEpoch.hs:154-166`, `NEWEPOCH`'s
`eNo /= succ eL` / non-boundary branch, runs on literally every tick within the epoch):
```haskell
if eNo /= succ eL
  then pure $
    nes
      & newEpochStateDRepPulsingStateL %~ pulseDRepPulsingState
      & newEpochStateGovStateL %~ predictFuturePParams
  else ...
```
`predictFuturePParams` (`Conway/Governance.hs:296-323`):
```haskell
predictFuturePParams govState =
  case cgsFuturePParams govState of
    NoPParamsUpdate -> govState              -- already solidified this epoch, no-op
    DefinitePParamsUpdate _ -> govState      -- already solidified this epoch, no-op
    _ -> govState { cgsFuturePParams = PotentialPParamsUpdate newFuturePParams }
  where
    newFuturePParams = do   -- LAZY: not forced until solidifyFuturePParams
      guard (any hasChangesToPParams (rsEnacted ratifyState))
      pure (ensCurPParams (rsEnactState ratifyState))
    ratifyState = extractDRepPulsingState (cgsDRepPulsingState govState)
    hasChangesToPParams gas = case pProcGovAction (gasProposalProcedure gas) of
      ParameterChange {} -> True
      HardForkInitiation {} -> True
      _ -> False           -- TreasuryWithdrawals/NoConfidence/UpdateCommittee/NewConstitution/InfoAction: False
```
**Key fact for the user's exact scenario**: `hasChangesToPParams` is `True` ONLY for
`ParameterChange`/`HardForkInitiation`. A `TreasuryWithdrawals` enactment does NOT itself flip
`futurePParams` to `Just pp` — it stays `PotentialPParamsUpdate Nothing`. The divergence the user is
almost certainly seeing after their TreasuryWithdrawals test is simply **tag 2 (`Potential`, `contents:
null`) vs dugite's hardcoded tag 0 (`No`, no `contents` key)** — a shape mismatch that happens after
*every* epoch boundary regardless of action type, not something TreasuryWithdrawals's payload
specifically changes.

**Solidify at the "point of no return"** = 2 stability windows before the end of the epoch
(`Slot.hs:106-114`):
```haskell
getTheSlotOfNoReturn slot = do
  globals@Globals {stabilityWindow} <- ask
  let epochNo = epochInfoEpoch epochInfo slot
      nextEpochNo = succ epochNo
      firstSlotNextEpoch = epochInfoFirst epochInfo nextEpochNo
      pointOfNoReturn = firstSlotNextEpoch *- Duration (2 * stabilityWindow)
  pure (epochNo, pointOfNoReturn, nextEpochNo)
```
called from `solidifyNextEpochPParams` (`Shelley/Rules/Tick.hs:144-156`), invoked on EVERY block's TICK
(`Shelley/Rules/Tick.hs:176`, Conway's `TICKF` equivalent at `Conway/Rules/Tickf.hs:42`), BEFORE the
`NEWEPOCH` trans call each time:
```haskell
solidifyNextEpochPParams nes slot = do
  (curEpochNo, slotOfNoReturn, _) <- getTheSlotOfNoReturn slot
  pure (curEpochNo,
        if slot < slotOfNoReturn then nes
        else nes & newEpochStateGovStateL . futurePParamsGovStateL %~ solidifyFuturePParams)
```
On mainnet (`k=2160, f=0.05` -> `stabilityWindow = 3k/f = 129600` slots, epoch = 432000 slots): the
"Potential" window is the FIRST ~172800 slots of the epoch (40%), the solidified (`No`/`Definite`)
window is the LAST ~259200 slots (60%). Ratio scales with `k`/`f`/epoch-length per network (devnet/
preview/preprod configs differ) — don't assume the mainnet 40/60 split applies verbatim to dugite's
devnet.

**`TICKF` also explicitly force-resets to `NoPParamsUpdate` on its OWN internal forecast boundary
crossing** (`Conway/Rules/Tickf.hs:62-66`, the ouroboros-consensus ledger-view-forecast path):
```haskell
pure $! nes {nesPd = pd'}
  & newEpochStateGovStateL . curPParamsGovStateL .~ nextEpochPParams govState
  & newEpochStateGovStateL . prevPParamsGovStateL .~ (govState ^. curPParamsGovStateL)
  & newEpochStateGovStateL . futurePParamsGovStateL .~ NoPParamsUpdate
```

## 4. Verdict on "is NoPParamsUpdate ever correct steady state"

Nuanced yes, but narrower than it looks:
- **Fresh chain, never had any governance action, before the very first epoch boundary ever
  processed**: `NoPParamsUpdate` IS correct — `ConwayGovState`'s `Default` instance
  (`Conway/Governance.hs:378`, `def = ConwayGovState def def def def def def (DRComplete def def)`)
  uses field-wise `def`, and `FuturePParams`'s own `Default` is `NoPParamsUpdate`
  (`State/Governance.hs:104-105`). This is the ONLY window where a permanently-hardcoded value is
  right by construction rather than by coincidence.
- **Last ~2 stability windows of ANY epoch (post point-of-no-return) in which no
  ParameterChange/HardForkInitiation is due to enact next boundary**: `NoPParamsUpdate` is correct —
  this is the common case for most ordinary epochs, and is a REAL, frequently-occurring steady state,
  not a corner case.
- **First ~(epoch length - 2*stabilityWindow) of literally every single epoch after the first
  boundary**: value is ALWAYS `PotentialPParamsUpdate` (tag 2), `Nothing` or `Just pp` depending on
  whether a PParamUpdate/HF action is currently accepted-so-far by the pulser. Hardcoded
  `NoPParamsUpdate` (tag 0) is wrong here — different tag, different JSON shape (no `contents` key vs
  a `contents` key present).
- **Last ~2 stability windows of an epoch following a boundary where ParameterChange or
  HardForkInitiation actually enacted**: value is `DefinitePParamsUpdate pp` (tag 1) carrying the real
  next PParams. Hardcoded `NoPParamsUpdate` is wrong here too, and arguably the most user-visible case
  (a real, non-null PParams object cardano-cli would show that dugite omits entirely).

**Answer: (b) — wrong only after certain events, but "certain events" here means "most of every
epoch, on every running chain with any governance activity at all"**, i.e. in practice a live/soaked
chain spends the plurality of its time in a state dugite's hardcode cannot represent. It's only right
in the narrow original-genesis window and in the tail of otherwise-quiet epochs.

## 5. What it's FOR / consumers / severity

`nextEpochPParams`/`nextEpochUpdatedPParams` (`State/Governance.hs:121-133`) are the only readers of
this field inside cardano-ledger itself, and both are consumed by:
- `Conway/Rules/Epoch.hs:323` (`cgsCurPParamsL .~ nextEpochPParams govState0`) — the ACTUAL epoch-
  boundary PParams swap reads this (falls back to `curPParamsGovStateL` if `knownFuturePParams` is
  `Nothing`, i.e., functionally correct either way for that ONE call site since it's evaluated post-
  solidify).
- `Conway/Rules/Tickf.hs:64` (`newEpochStateGovStateL . curPParamsGovStateL .~ nextEpochPParams
  govState`) — **this is ouroboros-consensus's ledger-view-forecast (HFC) path**, used to validate
  headers/build ledger views for slots ahead of the current tip, specifically so the Hard-Fork
  Combinator knows the upcoming protocol version *before* the real boundary is crossed. This is the
  reason `solidifyFuturePParams` must run a full `2*stabilityWindow` early — per the doc comment in
  `setFreshDRepPulsingState`, "we must ensure... HFC has the new EnactState available `6k/f` slots
  before the end of the epoch."

So the FIELD ITSELF is consensus-relevant (HFC forecast timing), not merely a query cosmetic — but
that is a **separate code path** from an N2C `GetGovState` query-response encoder. A hardcoded
constant in the LSQ encoder alone (as described) is, on its own, an **observability/query-parity bug**
(`query gov-state` JSON diverges from cardano-cli byte-for-byte), not automatically proof that dugite's
internal TICKF/forecast-equivalent is also broken. **Whether dugite's own HFC/forecast logic
independently tracks (or needs) an equivalent internal field is a separate question this Haskell-source
research cannot answer from the ledger side alone — it requires checking dugite's own consensus/ledger
code**, which is out of this oracle's scope (recommend `tech-lead` or `architect` agent for that half).

## Related
[[conway-gov-enactment-effects-and-timing]] — sibling investigation same day, covers the rest of
`Rules/Epoch.hs`'s epoch-boundary step (committee/constitution/treasury), same `govState1` binding.
