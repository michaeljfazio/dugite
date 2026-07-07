---
name: ppup-json-field-names-debug-dump
description: Exact aeson ToJSON field names for pre-Conway PPUPState/ProposedPPUpdates/PParamsUpdate as emitted by cardano-cli debug log-epoch-state, plus the Conway-era replacement shape under the same "ppups" key
metadata:
  type: reference
---

Live-verified 2026-07-06 via cardano-haskell-oracle (GitHub source read, not inferred). This
covers the aeson JSON debug-dump path (`cardano-cli debug log-epoch-state`), which is a
**separate code path from CBOR wire format** — do not conflate with
[[conway-pparams-field-order]] (that's the CBOR array(31) encoding, unrelated key names).

## 1. ShelleyGovState -> "ppups" key (pre-Conway eras)

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Governance.hs` (type ~L63-72, ToKeyValuePairs
~L174-180):

```haskell
data ShelleyGovState era = ShelleyGovState
  { sgsCurProposals    :: !(ProposedPPUpdates era)
  , sgsFutureProposals :: !(ProposedPPUpdates era)
  , sgsCurPParams      :: !(PParams era)
  , sgsPrevPParams     :: !(PParams era)
  , sgsFuturePParams   :: !(FuturePParams era)   -- SILENTLY EXCLUDED from JSON
  }

toKeyValuePairs ShelleyGovState {..} =
  [ "proposals" .= sgsCurProposals
  , "futureProposals" .= sgsFutureProposals
  , "curPParams" .= sgsCurPParams
  , "prevPParams" .= sgsPrevPParams
  ]
```

The `"ppups"` wrapper key comes from `UTxOState`'s instance, era-polymorphic:
`eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs:297`
(`"ppups" .= utxosGovState`, where `utxosGovState :: GovState era`).

## 2. ProposedPPUpdates is an ARRAY OF PAIRS, not a JSON object

`eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs` (~L270-271, ~L290-292):

```haskell
newtype ProposedPPUpdates era = ProposedPPUpdates (Map (KeyHash GenesisRole) (PParamsUpdate era))

instance ToJSON (ProposedPPUpdates era) where
  toJSON (ProposedPPUpdates ppUpdates) = toJSON $ Map.toList ppUpdates
```

`Map.toList` runs BEFORE `toJSON`, so aeson's tuple instance fires (2-element array), NOT the
`ToJSONKey`/object-keyed-by-hash-string instance one would expect from a `Map`. Real shape:

```json
"proposals": [
  ["a1b2c3...28byteHexHash", { "txFeePerByte": 44, ... }],
  ["d4e5f6...28byteHexHash", { ... }]
]
```

`KeyHash` renders as a plain hex string (newtype-derived ToJSON over `Hash`,
`libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs:162-179`). `futureProposals` has the
identical array-of-pairs shape. **A normalizer expecting `{"<hash>": {...}}` will silently
read zero proposals — this is the most likely bug source if hand-rolling a Python parser.**

## 3. PParamsUpdate JSON field names — data-driven from `ppName`, NOT Haskell record names

Keys come from each era's `eraPParams` list feeding a generic `ToKeyValuePairs (PParamsUpdate
era)` (`libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs:733-736`). Do not guess
from the abbreviated Shelley-paper record field names (minFeeA, a0, rho, tau, eMax, nOpt,
etc.) — those are NOT the JSON keys.

| Shelley-paper name | Actual JSON key | Era scope |
|---|---|---|
| minFeeA | `txFeePerByte` | all |
| minFeeB | `txFeeFixed` | all |
| maxBBSize | `maxBlockBodySize` | all |
| maxTxSize | `maxTxSize` | all (unchanged) |
| maxBHSize | `maxBlockHeaderSize` | all |
| keyDeposit | `stakeAddressDeposit` | all |
| poolDeposit | `stakePoolDeposit` | all |
| eMax | `poolRetireMaxEpoch` | all |
| nOpt | `stakePoolTargetNum` | all |
| a0 | `poolPledgeInfluence` | all |
| rho | `monetaryExpansion` | all |
| tau | `treasuryCut` | all |
| decentralisationParam | `decentralization` | Shelley-Mary only, gone Alonzo+ |
| (extraEntropy) | `extraPraosEntropy` | Shelley-Mary only, gone Alonzo+ |
| minUTxOValue | `minUTxOValue` | Shelley-Mary only, dropped Alonzo+ |
| coinsPerUTxOWord/Byte | `utxoCostPerByte` | Alonzo+ — SAME key both eras despite unit changing (per-word Alonzo -> per-byte Babbage+); known cross-era naming quirk |
| costmdls | `costModels` | Alonzo+ |
| prices | `executionUnitPrices` | Alonzo+ |
| maxTxExUnits | `maxTxExecutionUnits` | Alonzo+ |
| maxBlockExUnits | `maxBlockExecutionUnits` | Alonzo+ |
| maxValSize | `maxValueSize` | Alonzo+ |
| collateralPercentage | `collateralPercentage` | Babbage+ (unchanged) |
| maxCollateralInputs | `maxCollateralInputs` | Babbage+ (unchanged) |
| minPoolCost | `minPoolCost` | all (unchanged) |
| protocolVersion | `protocolVersion` -> nested `{"major": N, "minor": N}` | Shelley-Babbage only (`BaseTypes.hs:224-228`) |

Sources: Shelley `eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs:340-493`; Alonzo
`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/PParams.hs:657-724`.

## 4. Conway — "ppups" key survives but is a STRUCTURALLY DIFFERENT type

`ConwayGovState` (`eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs:243-251`, ToJSON
~L391-402) is what renders under the same `"ppups"` wrapper key on an 11.0.1 Conway dump
(because the wrapper is generic over `GovState era`):

```haskell
toKeyValuePairs cg =
  [ "proposals" .= cgsProposals              -- CIP-1694 GovActionState list, NOT a PPUpdates map
  , "nextRatifyState" .= extractDRepPulsingState cgsDRepPulsingState
  , "committee" .= cgsCommittee
  , "constitution" .= cgsConstitution
  , "currentPParams" .= cgsCurPParams        -- RENAMED from curPParams
  , "previousPParams" .= cgsPrevPParams      -- RENAMED from prevPParams
  , "futurePParams" .= cgsFuturePParams
  ]
```

Key implications for a normalizer targeting cardano-node 11.0.1 (Conway):
- `esLState.utxoState.ppups` IS present, same wrapper key name as pre-Conway.
- `"proposals"` is now the governance-action list (GovActionState/Proposals), unrelated shape
  to the Shelley `ProposedPPUpdates` array-of-pairs.
- There is **no `futureProposals` key** in Conway at all (name doesn't survive).
- `curPParams`/`prevPParams` become `currentPParams`/`previousPParams` (renamed, not aliased).
- Legacy PPUPState/ProposedPPUpdates JSON literally cannot appear once the chain is on Conway
  — it was fully replaced, not merely left perpetually empty. A normalizer must branch on
  era (or on which key set is present under `ppups`) rather than assume one fixed schema.
