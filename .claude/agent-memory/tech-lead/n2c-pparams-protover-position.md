---
name: N2C Conway PParams protocolVersion position
description: protocolVersion sits at index 12 in Conway PParams array(31), between tau and minPoolCost — NOT at index 30. Verified empirically against cardano-cli 10.15 (issue #434).
type: reference
---

# Conway PParams Positional Encoding

## Invariant (current ledger master, verified 2026-05-12 with cardano-cli 10.15)

Conway PParams CBOR is a **positional `array(31)`** with the following order
from `eraPParams @ConwayEra`:

| Idx | Field                          | CBOR shape           |
|----:|--------------------------------|----------------------|
|   0 | txFeePerByte                   | uint                 |
|   1 | txFeeFixed                     | uint                 |
|   2 | maxBBSize                      | uint                 |
|   3 | maxTxSize                      | uint                 |
|   4 | maxBHSize                      | uint                 |
|   5 | keyDeposit                     | uint                 |
|   6 | poolDeposit                    | uint                 |
|   7 | eMax                           | uint                 |
|   8 | nOpt                           | uint                 |
|   9 | a0                             | tag(30)[num, den]    |
|  10 | rho                            | tag(30)[num, den]    |
|  11 | tau                            | tag(30)[num, den]    |
|  12 | **protocolVersion**            | array(2)[maj, min]   |
|  13 | minPoolCost                    | uint                 |
|  14 | coinsPerUTxOByte               | uint                 |
|  15 | costModels                     | map                  |
|  16 | prices                         | array(2)[tag30, tag30]|
|  17 | maxTxExUnits                   | array(2)[mem, steps] |
|  18 | maxBlockExUnits                | array(2)[mem, steps] |
|  19 | maxValSize                     | uint                 |
|  20 | collateralPercentage           | uint                 |
|  21 | maxCollateralInputs            | uint                 |
|  22 | poolVotingThresholds           | array(5) of tag30    |
|  23 | drepVotingThresholds           | array(10) of tag30   |
|  24 | committeeMinSize               | uint                 |
|  25 | committeeMaxTermLength         | uint                 |
|  26 | govActionLifetime              | uint                 |
|  27 | govActionDeposit               | uint                 |
|  28 | drepDeposit                    | uint                 |
|  29 | drepActivity                   | uint                 |
|  30 | minFeeRefScriptCostPerByte     | tag(30)[num, den]    |

Source of truth (master HEAD): `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs`,
in the `EraPParams ConwayEra` instance:

```haskell
eraPParams =
  [ ppTxFeePerByte, ppTxFeeFixed, ppMaxBBSize, ppMaxTxSize, ppMaxBHSize
  , ppKeyDeposit, ppPoolDeposit, ppEMax, ppNOpt, ppA0, ppRho, ppTau
  , ppGovProtocolVersion        -- INDEX 12
  , ppMinPoolCost, ppCoinsPerUTxOByte, ppCostModels, ppPrices
  , ppMaxTxExUnits, ppMaxBlockExUnits, ppMaxValSize
  , ppCollateralPercentage, ppMaxCollateralInputs
  , ppPoolVotingThresholds, ppDRepVotingThresholds
  , ppCommitteeMinSize, ppCommitteeMaxTermLength
  , ppGovActionLifetime, ppGovActionDeposit
  , ppDRepDeposit, ppDRepActivity
  , ppMinFeeRefScriptCostPerByte
  ]
```

## Prior incorrect note (superseded by issue #434)

An earlier version of this memory and the oracle file
`cardano-ledger-types-wire-format.md` claimed protocolVersion was index **30
(LAST)** with `minFeeRefScriptCostPerByte` at index 29. That ordering was
incompatible with cardano-cli 10.15 against current ledger master — the
gov-state query fails with
`Final number of elements: 24 does not match the total count that was decoded: 31`
because protocolVersion's `array(2)` is read as `minPoolCost`'s uint and
every subsequent type-check propagates.

Verified empirically on 2026-05-12 by running `cardano-cli conway query
protocol-parameters --testnet-magic 2` against a dugite node with each
ordering: the `protocolVersion` field decodes to the snapshot's true value
(major 8, minor 0) only when emitted at index 12.

If the ledger ever moves protocolVersion to the end again, the test
`test_pparams_conway_positional_order_issue_434` (in
`crates/dugite-node/src/node/n2c_query/encoding.rs`) will fail and the
oracle file should be re-read.

## Where this lives in dugite
- Encoder: `crates/dugite-node/src/node/n2c_query/encoding.rs::encode_protocol_params_cbor`
- Callers: GetCurrentPParams (tag 3), GovState (tag 24) cur/prev pparams +
  EnactState cur/prev pparams (4 emissions per gov-state response)
- Golden test: `test_pparams_conway_positional_order_issue_434`

## Other PParams callers to check when fields shift again
If the layout shifts again, every consumer that overlays a positional
decoder on top will silently misread. Always ship a golden test that asserts
the **value** at each index, not just the count.
