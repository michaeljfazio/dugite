# Protocol Parameters Reference

Cardano protocol parameters control fees, block sizes, staking mechanics,
script execution budgets, and governance. Every one of them is mutable through
a `ParameterChange` governance action, so the values below are not constants —
always query a node for truth.

> **Mainnet values in this page were read from mainnet epoch 646.** They are
> included to give a sense of scale, not as defaults. Re-read them from a node
> before relying on any of them.

## Querying Parameters

```bash
dugite-cli query protocol-parameters \
  --socket-path ./node.sock \
  --out-file protocol-params.json
```

The JSON key names in the tables below are the `cardano-cli`-compatible names
that `dugite-cli` emits and accepts. They are **not** the Rust field names and
they are not produced by serde — they are written by the N2C client decoder in
`crates/dugite-network/src/n2c_client.rs`. Where `cardano-cli` accepts an older
alias, both are listed.

## Fee Parameters

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| Min fee coefficient | `txFeePerByte` / `minFeeA` | Fee per byte of transaction size | 44 |
| Min fee constant | `txFeeFixed` / `minFeeB` | Fixed fee component | 155381 |
| Min UTxO cost per byte | `utxoCostPerByte` / `coinsPerUTxOByte` | Minimum lovelace per byte of UTxO (Babbage+) | 4310 |
| Reference script fee | `minFeeRefScriptCostPerByte` | Tiered fee per byte of reference script (Conway) | 15 |

The base transaction fee formula is:

```
fee = txFeePerByte * tx_size_in_bytes + txFeeFixed
```

Conway adds a **tiered** reference-script surcharge on top, growing
geometrically with total reference-script size.

> **Minimum-UTxO is era-dispatched, not one formula.** Dugite selects the
> per-era Haskell calculation from the protocol version in force
> (issue #919): flat `minUTxOValue` at PV ≤ 3; Mary `scaledMinDeposit` at PV 4;
> Alonzo `(27 + size + dataHashSize) × coinsPerUTxOWord` at PV 5–6; Babbage
> `(160 + size) × coinsPerUTxOByte` at PV ≥ 7. Key 17 therefore means
> `coinsPerUTxOWord` before Babbage and `coinsPerUTxOByte` from Babbage on.

## Block Size Parameters

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| Max block body size | `maxBlockBodySize` | Maximum block body size in bytes | 90112 |
| Max transaction size | `maxTxSize` | Maximum transaction size in bytes | 16384 |
| Max block header size | `maxBlockHeaderSize` | Maximum block header size in bytes | 1100 |

## Staking Parameters

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| Stake address deposit | `stakeAddressDeposit` / `keyDeposit` | Deposit for stake key registration (lovelace) | 2000000 |
| Pool deposit | `stakePoolDeposit` / `poolDeposit` | Deposit for pool registration (lovelace) | 500000000 |
| Pool retire max epoch | `poolRetireMaxEpoch` / `eMax` | Maximum future epochs for pool retirement | 18 |
| Pool target count | `stakePoolTargetNum` / `nOpt` | Target number of pools (k parameter) | 500 |
| Min pool cost | `minPoolCost` | Minimum fixed pool cost (lovelace) | 170000000 |

## Monetary Policy

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| Monetary expansion (rho) | `monetaryExpansion` | Rate of new ADA created from reserves per epoch | 0.003 |
| Treasury cut (tau) | `treasuryCut` | Fraction of rewards directed to the treasury | 0.20 |
| Pledge influence (a0) | `poolPledgeInfluence` | How pledge affects reward calculation | 0.3 |

These three are **exact rationals** on the wire (CBOR tag 30), not floats.
Dugite parses genesis values as exact `Scientific` and never through an
`f64` intermediate — a lossy parse here diverges the reward calculation.

## Plutus Execution Parameters

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| Execution unit prices | `executionUnitPrices` | `{priceMemory, priceSteps}` rationals | `{0.0577, 0.0000721}` |
| Max tx execution units | `maxTxExecutionUnits` | `{memory, steps}` per transaction | `{16500000, 10000000000}` |
| Max block execution units | `maxBlockExecutionUnits` | `{memory, steps}` per block | `{72000000, 20000000000}` |
| Max value size | `maxValueSize` | Maximum serialized value size in bytes | 5000 |
| Collateral percentage | `collateralPercentage` | Collateral % of total tx fee for Plutus txs | 150 |
| Max collateral inputs | `maxCollateralInputs` | Maximum collateral inputs per tx | 3 |
| Cost models | `costModels` | Per-language builtin cost vectors (`PlutusV1`–`PlutusV3`) | — |

> `maxValueSize` is compared with a strict `>`, against a size computed with
> Haskell `encodeMap` semantics — indefinite-length CBOR map headers above 23
> entries, definite at or below. Getting that wrong over-counts by one byte on
> large asset maps and produces a false Phase-1 rejection (issue #930).

## Governance Parameters (Conway)

| Parameter | JSON Key | Description | Mainnet value |
|-----------|----------|-------------|---------------|
| DRep deposit | `dRepDeposit` | Deposit for DRep registration (lovelace) | 500000000 |
| DRep activity | `dRepActivity` | Epochs of inactivity before a DRep goes dormant | 20 |
| Gov action deposit | `govActionDeposit` | Deposit for governance action submission (lovelace) | 100000000000 |
| Gov action lifetime | `govActionLifetime` | Governance action expiry (epochs) | 6 |
| Committee min size | `committeeMinSize` | Minimum constitutional committee size | 5 |
| Committee max term | `committeeMaxTermLength` | Maximum committee member term (epochs) | 146 |

### Voting Thresholds

The threshold parameters travel on the wire as **two fixed-order arrays**, not
as individual keys: `poolVotingThresholds` (5 entries, CBOR key 25) and
`drepVotingThresholds` (10 entries, CBOR key 26).

**`poolVotingThresholds` — array order:**

| Position | JSON Key |
|---|---|
| 0 | `pvtMotionNoConfidence` |
| 1 | `pvtCommitteeNormal` |
| 2 | `pvtCommitteeNoConfidence` |
| 3 | `pvtHardForkInitiation` |
| 4 | `pvtPPSecurityGroup` |

**`drepVotingThresholds` — array order:**

| Position | JSON Key |
|---|---|
| 0 | `dvtMotionNoConfidence` |
| 1 | `dvtCommitteeNormal` |
| 2 | `dvtCommitteeNoConfidence` |
| 3 | `dvtUpdateToConstitution` |
| 4 | `dvtHardForkInitiation` |
| 5 | `dvtPPNetworkGroup` |
| 6 | `dvtPPEconomicGroup` |
| 7 | `dvtPPTechnicalGroup` |
| 8 | `dvtPPGovGroup` |
| 9 | `dvtTreasuryWithdrawal` |

### Which body votes on what

This is the ratification matrix Dugite implements, matching Haskell
`Conway.Rules.Ratify`. "—" means that body does not vote on the action at all
(it is not an abstention — the check is simply absent).

| Action type | DRep threshold | SPO threshold | Constitutional Committee |
|---|---|---|---|
| No Confidence | `dvtMotionNoConfidence` | `pvtMotionNoConfidence` | — |
| Update Committee (normal) | `dvtCommitteeNormal` | `pvtCommitteeNormal` | — |
| Update Committee (under no-confidence) | `dvtCommitteeNoConfidence` | `pvtCommitteeNoConfidence` | — |
| New Constitution | `dvtUpdateToConstitution` | — | votes |
| Hard Fork Initiation | `dvtHardForkInitiation` | `pvtHardForkInitiation` | votes |
| Parameter Change | every affected group's `dvtPP*Group` must pass independently | `pvtPPSecurityGroup`, **only if** a security-tagged parameter is touched | votes |
| Treasury Withdrawal | `dvtTreasuryWithdrawal` | — | votes |
| Info | never ratifies (informational only) | — | — |

Two consequences worth internalising:

- There is **no** `pvtPPEconomicGroup`. The only SPO threshold for parameter
  changes is `pvtPPSecurityGroup`, and SPOs are excluded entirely from a
  parameter change that touches no security-tagged parameter.
- A parameter change spanning several groups must clear **each** group's DRep
  threshold, not just the highest one.

During the Conway bootstrap phase, DRep thresholds are treated as zero (always
met) and only the SPO and committee checks bind.

## CBOR keys for a `ProtocolParamUpdate`

Governance actions carry parameter changes as a sparse CBOR **map** keyed by
integer. This is the complete table Dugite encodes and decodes.

| Key | Parameter | Notes |
|-----|-----------|-------|
| 0 | `txFeePerByte` / `minFeeA` | |
| 1 | `txFeeFixed` / `minFeeB` | |
| 2 | `maxBlockBodySize` | |
| 3 | `maxTxSize` | |
| 4 | `maxBlockHeaderSize` | |
| 5 | `stakeAddressDeposit` / `keyDeposit` | |
| 6 | `stakePoolDeposit` / `poolDeposit` | |
| 7 | `poolRetireMaxEpoch` / `eMax` | |
| 8 | `stakePoolTargetNum` / `nOpt` | |
| 9 | `poolPledgeInfluence` (a0) | CBOR tag 30 rational |
| 10 | `monetaryExpansion` (rho) | CBOR tag 30 rational |
| 11 | `treasuryCut` (tau) | CBOR tag 30 rational |
| 12 | `decentralization` (d) | **pre-Conway only** — decode only |
| 13 | `extraEntropy` | **pre-Conway only** — decode only |
| 14 | `protocolVersion` | **pre-Conway only** — `[major, minor]`, decode only |
| 15 | `minUTxOValue` | **pre-Conway only** — decode only (restored in #919) |
| 16 | `minPoolCost` | |
| 17 | `coinsPerUTxOWord` / `utxoCostPerByte` | Meaning depends on the PV in force *before* this update's own PV bump |
| 18 | `costModels` | map `{0: PlutusV1, 1: PlutusV2, 2: PlutusV3, 3: PlutusV4}` |
| 19 | `executionUnitPrices` | `[memPrice, stepPrice]` |
| 20 | `maxTxExecutionUnits` | `[mem, steps]` |
| 21 | `maxBlockExecutionUnits` | `[mem, steps]` |
| 22 | `maxValueSize` | |
| 23 | `collateralPercentage` | |
| 24 | `maxCollateralInputs` | |
| 25 | `poolVotingThresholds` | array of 5 rationals, order above |
| 26 | `drepVotingThresholds` | array of 10 rationals, order above |
| 27 | `committeeMinSize` | |
| 28 | `committeeMaxTermLength` | |
| 29 | `govActionLifetime` | |
| 30 | `govActionDeposit` | |
| 31 | `dRepDeposit` | |
| 32 | `dRepActivity` | |
| 33 | `minFeeRefScriptCostPerByte` | CBOR tag 30 rational |
| 34 | `maxRefScriptSizePerBlock` | Dijkstra — decode only |
| 35 | `maxRefScriptSizePerTx` | Dijkstra — decode only |
| 36 | `refScriptCostStride` | Dijkstra — decode only |
| 37 | `refScriptCostMultiplier` | Dijkstra — decode only |

Notes on asymmetry, because it bites:

- Keys **12–15** exist only in the pre-Conway (Shelley → Babbage) update
  shape. Dugite **decodes** them for historical replay; the Conway encoder
  never emits them, and the Conway decoder skips them.
- Keys **34–37** are the unreleased Dijkstra era. They decode but are not
  encoded.
- Key **30 is `govActionDeposit` and key 31 is `dRepDeposit`** — the order is
  the opposite of what the alphabetical reading suggests, and swapping them
  silently produces a valid-looking but wrong proposal.

Encoder: `crates/dugite-serialization/src/encode/protocol_params.rs`.
Decoders: `crates/dugite-serialization/src/decode/era_conway.rs` (Conway and
later) and `.../era_shelley.rs` (pre-Conway).

## N2C wire encoding of the current parameters

The `GetCurrentPParams` LocalStateQuery reply is **not** the sparse
integer-keyed map above. It is a **positional CBOR `array(31)`, indices 0–30**,
matching Haskell's `EncCBOR (ConwayPParams Identity ConwayEra)`. The two layouts
are different and are not interchangeable.

| Index | Parameter | Index | Parameter |
|---|---|---|---|
| 0 | `txFeePerByte` | 16 | `executionUnitPrices` |
| 1 | `txFeeFixed` | 17 | `maxTxExecutionUnits` |
| 2 | `maxBlockBodySize` | 18 | `maxBlockExecutionUnits` |
| 3 | `maxTxSize` | 19 | `maxValueSize` |
| 4 | `maxBlockHeaderSize` | 20 | `collateralPercentage` |
| 5 | `stakeAddressDeposit` | 21 | `maxCollateralInputs` |
| 6 | `stakePoolDeposit` | 22 | `poolVotingThresholds` |
| 7 | `poolRetireMaxEpoch` | 23 | `drepVotingThresholds` |
| 8 | `stakePoolTargetNum` | 24 | `committeeMinSize` |
| 9 | `poolPledgeInfluence` | 25 | `committeeMaxTermLength` |
| 10 | `monetaryExpansion` | 26 | `govActionLifetime` |
| 11 | `treasuryCut` | 27 | `govActionDeposit` |
| 12 | `protocolVersion` (`[major, minor]`) | 28 | `dRepDeposit` |
| 13 | `minPoolCost` | 29 | `dRepActivity` |
| 14 | `utxoCostPerByte` | 30 | `minFeeRefScriptCostPerByte` |
| 15 | `costModels` | | |

Every index 0–30 is populated; there are no gaps and no Dijkstra slots.
`GetGenesisConfig` uses a different, legacy Shelley-era layout —
`array(18)` on N2C v16–v20 and `array(17)` on v21+.

Encoder: `crates/dugite-node/src/node/n2c_query/encoding.rs`
(`encode_protocol_params_cbor`). Client-side decoder to `cardano-cli` JSON:
`crates/dugite-network/src/n2c_client.rs` (`parse_protocol_params_cbor`).
