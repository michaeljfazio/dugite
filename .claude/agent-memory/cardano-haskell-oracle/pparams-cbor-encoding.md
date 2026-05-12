# Conway PParams CBOR Encoding Reference

## Key Distinction: PParams vs PParamsUpdate
- **PParams** (GetCurrentPParams query response): CBOR **array** of length 31, fields in fixed order
- **PParamsUpdate** (governance proposals): CBOR **map** with integer keys 0-33, only changed fields present

## Source Files
- EncCBOR PParams: `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs`
- Conway fields: `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs`
- CDDL spec: `cardano-ledger/eras/conway/impl/cddl/data/conway.cddl`
- Rational encoding: `cardano-ledger/libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Plain.hs`
- CostModels: `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/CostModels.hs`
- ExUnits/Prices: `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/ExUnits.hs`

## PParams Array Order (31 fields) — V21+ corrected (issue #336, 2026-05-12)

**CORRECTED**: `protocolVersion` is at index **30 (LAST)** in Conway, not index 12.
Conway moved it out of the updatable PParamsUpdate map and appends it via
`ppGovProtocolVersion` in the eraPParams lens list.

| Idx | Field | CBOR Type | ppuTag (map key) |
|-----|-------|-----------|------------------|
| 0 | txFeePerByte | uint | 0 |
| 1 | txFeeFixed | uint | 1 |
| 2 | maxBBSize | uint | 2 |
| 3 | maxTxSize | uint | 3 |
| 4 | maxBHSize | uint | 4 |
| 5 | keyDeposit | uint | 5 |
| 6 | poolDeposit | uint | 6 |
| 7 | eMax | uint | 7 |
| 8 | nOpt | uint | 8 |
| 9 | a0 | Tag(30)[num,den] | 9 |
| 10 | rho | Tag(30)[num,den] | 10 |
| 11 | tau | Tag(30)[num,den] | 11 |
| 12 | minPoolCost | uint | 16 |
| 13 | coinsPerUTxOByte | uint | 17 |
| 14 | costModels | map{0:[i64],1:[i64],2:[i64]} | 18 |
| 15 | prices | [Tag30,Tag30] | 19 |
| 16 | maxTxExUnits | [mem,steps] | 20 |
| 17 | maxBlockExUnits | [mem,steps] | 21 |
| 18 | maxValSize | uint | 22 |
| 19 | collateralPercentage | uint | 23 |
| 20 | maxCollateralInputs | uint | 24 |
| 21 | poolVotingThresholds | array(5) of Tag30 | 25 |
| 22 | drepVotingThresholds | array(10) of Tag30 | 26 |
| 23 | committeeMinSize | uint | 27 |
| 24 | committeeMaxTermLength | uint | 28 |
| 25 | govActionLifetime | uint | 29 |
| 26 | govActionDeposit | uint | 30 |
| 27 | drepDeposit | uint | 31 |
| 28 | drepActivity | uint | 32 |
| 29 | minFeeRefScriptCostPerByte | Tag(30)[num,den] | 33 |
| 30 | protocolVersion | array(2)[major,minor] | N/A (no update in Conway) |

## Note: Array index != ppuTag
Keys 12-15 were Shelley's ppD/extraEntropy/protVer/minUTxOValue.
Babbage removed ppD(12) and extraEntropy(13), so array positions shifted
but ppuTag numbers in PParamsUpdate map stayed the same.

## Nested Type Encodings
- **Rational**: `Tag(30) [numerator: uint, denominator: positive_uint]`
- **ExUnits**: `[mem: uint, steps: uint]`
- **Prices**: `[mem_price: Tag30[n,d], step_price: Tag30[n,d]]`
- **CostModels**: `{0: [i64...], 1: [i64...], 2: [i64...]}` (PlutusV1=0, V2=1, V3=2)
- **ProtocolVersion**: `[major: uint, minor: uint]`
- **PoolVotingThresholds**: array(5) Tag30 rationals: [motionNoConfidence, committeeNormal, committeeNoConfidence, hardForkInitiation, ppSecurityGroup]
- **DRepVotingThresholds**: array(10) Tag30 rationals: [motionNoConfidence, committeeNormal, committeeNoConfidence, updateConstitution, hardForkInitiation, ppNetworkGroup, ppEconomicGroup, ppTechnicalGroup, ppGovGroup, treasuryWithdrawal]

## Known Dugite Bugs (as of 2026-03-09)
1. ~~encode_protocol_params_cbor uses map encoding, should be array(31)~~ FIXED
2. ~~DRep voting thresholds reuse dvt_p_param_change for all 4 PP group thresholds~~ FIXED
3. ~~ProtocolParamsSnapshot missing separate dvt_pp_network/economic/technical/governance fields~~ FIXED
4. ~~protocolVersion encoded at index 12, must be at index 30 (LAST)~~ FIXED 2026-05-12 (issue #336)
