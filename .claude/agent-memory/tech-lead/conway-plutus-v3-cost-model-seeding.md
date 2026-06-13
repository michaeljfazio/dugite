# Conway PlutusV3 cost-model must be seeded at the hard fork + snapshot restore

**Symptom (mainnet epoch 507+):** every PlutusV3 transaction diverges two ways —
`ScriptDataHashMismatch` (phase-1) and `budget exhausted` (phase-2). Root cause:
`protocol_params.cost_models.plutus_v3 == None` in Conway ledger state, so
`encode_language_views(has_v3=true)` emits an empty map `0xa0` → wrong
`script_data_hash`, and V3 eval falls back to a DEFAULT cost model → spurious
budget exhaustion. Byte-exact proof: tx `31b6732d…` (ep507) wrong hash =
`blake2b256(redeemers || 0xa0)`.

**Why None:** the V3 cost model originates from `conway-genesis.json`
(`plutusV3CostModel`, 251 entries), NOT from an on-chain ParameterChange — so no
replay event populates it. Haskell `upgradeConwayPParams`
(eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs) seeds it at the
Babbage→Conway hard fork as a **per-language INSERT** over the Babbage {V1,V2}
map: `updateCostModels bppCostModels (mkCostModels {V3 -> ucppPlutusV3CostModel})`
= `Map.union` → {V1,V2,V3}. V1/V2 carried unchanged.

**Fix (commit 00a1a3ac8b):**
- `ConwayGenesisInit` gains `plutus_v3_cost_model: Option<Vec<i64>>`, populated
  from the loaded Conway genesis in BOTH startup paths (node/mod.rs + main.rs).
- `conway.rs on_era_transition` seeds `epochs.protocol_params.cost_models.plutus_v3`
  when `None` (per-language insert; preserves V1/V2 and any governance-updated V3).
  Covers from-genesis + pre-Conway-snapshot replay.
- `node/mod.rs` post-init guard: `if pv>=9 && plutus_v3.is_none()` seed from genesis.
  Covers resuming from a Conway snapshot (era transition won't re-fire). This is
  what fixes an already-running deployment on restart.

**Cost-model PParamUpdate semantics (verified via oracle, same source):**
- Conway `conwayApplyPPUpdates` cost_models = `updateCostModels` = per-language
  MERGE (`Map.union modValid oldValid`). A {V1,V2}-only update PRESERVES V3.
  dugite's governance.rs already does this (correct).
- PRE-Conway (Alonzo/Babbage) generic `applyPPUpdates` REPLACES the cost-models
  field wholesale (the PPU carries the full set). dugite `shelley.rs:apply_pp_update`
  (shelley.rs:780, conway.rs:507 era-crossing edge) is wholesale replace — and
  that is CORRECT for the pre-Conway path. **Do NOT change it to merge** — that
  would introduce a divergence. (A Sonnet investigation mis-flagged it; verified
  it's the pre-Conway path only.)

**Lesson:** any era-boundary PParams upgrade field that comes from genesis (not
on-chain) needs explicit seeding in `on_era_transition` AND a post-snapshot guard
— the snapshot-restore path skips the era transition entirely.

Regression: `test_on_era_transition_seeds_plutus_v3_cost_model` (conway.rs).
See [[conway-cert-redeemer-witnessing]].
