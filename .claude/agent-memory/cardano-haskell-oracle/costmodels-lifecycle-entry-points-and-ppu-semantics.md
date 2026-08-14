---
name: costmodels-lifecycle-entry-points-and-ppu-semantics
description: Where each PlutusVn cost model enters PParams (era translations only), pre-Conway PPU whole-field REPLACE vs Conway per-language MERGE, and why the HFC initial state is always Byron (translations at slot 0 for epoch-0 forks)
metadata:
  type: reference
---

# Cost-model lifecycle in cardano-ledger / consensus (verified at ledger `faa7a9dc`, oc `release-ouroboros-consensus-3.0.1.0`)

## Entry points — cost models ONLY enter PParams via era translations (or on-chain PPUs)

| model | enters at | mechanism |
|---|---|---|
| PlutusV1 | Mary→Alonzo translation | `upgradeAlonzoPParams`: `appCostModels = uappCostModels` — the WHOLE field comes from AlonzoGenesis (`Alonzo/Translation.hs:64`: `translateEra (AlonzoGenesisWrapper upgradeArgs) = pure . upgradePParams upgradeArgs`) |
| PlutusV2 | on-chain PPU only | `upgradeBabbagePParams`: `bppCostModels = appCostModels` — Alonzo→Babbage is a PASSTHROUGH; no genesis injects V2 anywhere (confirms #1046) |
| PlutusV3 | Babbage→Conway translation | `upgradeConwayPParams` (`Conway/PParams.hs:1107-1117`), source comment: "We add the PlutusV3 CostModel from ConwayGenesis to the ConwayPParams here" — `updateCostModels bppCostModels (mkCostModels (Map.singleton PlutusV3 ucppPlutusV3CostModel))`, per-language union, new (genesis V3) wins |

There is NO upstream path that puts ConwayGenesis's `plutusV3CostModel` into
PParams before the Babbage→Conway `translateEra`. A node showing V3 during
Alonzo/Babbage has invented an entry.

## PPU application: pre-Conway REPLACE, Conway MERGE

- Generic `applyPPUpdates` (`Core/PParams.hs:274-277`, the `Updatable (K1 t x a) (K1 t (StrictMaybe x) u)` instance):
  `SJust x -> x` — WHOLE-FIELD replace. Shelley/Allegra/Mary/Alonzo/Babbage all
  use the default (no override in their `EraPParams` instances).
  So a Babbage PPU carrying `costmdls {V1,V2}` REPLACES the entire map —
  any pre-existing entry not in the update (e.g. a wrongly-held V3) is WIPED.
- Conway OVERRIDES it (`Conway/PParams.hs:805-806` → `conwayApplyPPUpdates:1195-1199`):
  `THKD (SJust costModelUpdate) -> THKD $ updateCostModels (old) costModelUpdate`
  — per-language MERGE, new wins. Doc comment at :1170: "`CostModels` update
  differs form other protocol parameters". (`protocolVersion` also becomes
  un-updatable via PPU in Conway: `cppProtocolVersion = cppProtocolVersion pp`.)

## HFC initial state is ALWAYS Byron; epoch-0 forks are still translations

- `protocolInfoCardano` (`Cardano/Node.hs:603,940-954`): `pInfoInitLedger =
  initExtLedgerStateCardano` = `injectInitialExtLedgerState cfg
  initExtLedgerStateByron`, then per-era `injectIntoTestState` (initial
  funds/staking) applied only to the era the telescope LANDS in.
- `injectInitialExtLedgerState` (`HardFork/Combinator/Embed/Nary.hs:256-303`):
  docstring "Performs any hard forks scheduled via 'TriggerHardForkAtEpoch'";
  calls `State.extendToSlot (configLedger cfg) (SlotNo 0)` — for all-forks-at-
  epoch-0 devnets the FULL translation chain Byron→…→Conway runs inside the
  initial-state computation, each era's `translateEra` with its own genesis as
  context. Conway upgrade params therefore DO arrive via translation on such
  networks. Note in source: "we can translate across multiple eras when
  computing the initial ledger state, but we do not support translation across
  multiple eras in general" (applyChainTick crosses at most one).
- For a Byron-first network with non-zero fork epochs (mainnet/preprod),
  `extendToSlot 0` translates nothing: `pInfoInitLedger` is pure Byron state;
  Shelley/Alonzo/Conway genesis live only in the ledger CONFIG as translation
  contexts, consumed at each fork.
- `tcInitialPParamsG` / `createInitialState` (`Shelley/Transition.hs:149-168,
  330-386`) are explicitly "/Warning/ - Should only be used in testing and
  benchmarking" and `protectMainnet`-guarded — they compose the same
  `upgradePParams` chain, so using them to seed a REAL network's initial state
  front-loads every era's genesis params (incl. V3) — the defect shape.

## Why an extra (unused-language) cost model is reporting-only pre-Conway

- Script integrity hash: `mkScriptIntegrity` (`Alonzo/Tx.hs:301-310`) —
  `langs = plutusScriptLanguage <$> mapMaybe toPlutusScript scriptsUsed` where
  `scriptsUsed = restrictKeys scriptsProvided scriptsNeeded`; langViews cover
  only languages of Plutus scripts ACTUALLY USED. Unused map entries never
  enter the hash.
- V3 unrepresentable pre-Conway: `PlutusScript BabbageEra = BabbagePlutusV1 |
  BabbagePlutusV2`, `eraMaxLanguage = PlutusV2` (`Babbage/Scripts.hs:57-63`);
  Alonzo is V1-only.
- Phase-2 cost-model access is per-script `Map.lookup` (NoCostModel), never
  the whole map ([[nocostmodel-collecterror-native-script-exclusion]]).
- The whole map IS observable via LSQ (`GetCurrentPParams` etc.) and state
  dumps — reporting divergence only.
- CAVEAT: benignity rests on the extra key being an UNUSABLE language. The
  same defect with V1/V2 *values* differing is CONSENSUS: UTXOW
  `checkScriptIntegrityHash` compares supplied vs locally-computed hash →
  false Phase-1 rejects.
- Self-healing: any pre-Conway PPU carrying costmdls wipes the extra entry
  (whole-field replace); and at the Conway fork `updateCostModels` puts
  genesis V3 on the WINNING side of the union regardless.

See [[babbage-conway-hf-ppup-order]] for exact updateCostModels semantics and
unknown-language handling (a V3 key in a Babbage PPU lands in the UNKNOWN
map, not valid models).
