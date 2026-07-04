# ISSUE-4: Plutus budget-accounting divergence (live preview, 2026-06-18)

## Symptom (from running BP soak)
```
WARN dugite_ledger::state::apply: Plutus evaluation divergence (parallel):
uplc says scripts fail but block is_valid=true on-chain — trusting on-chain consensus
tx_hash=b8e75c708a3827e6809ee397ec6dd60d83b228349652e54b84c6f6e4a5971f31
slot=115145170
error=Plutus evaluation failed: eval_phase_two_raw error: script evaluation failed:
budget exhausted: cpu_remaining=14547, mem_remaining=109
```
- Haskell network accepted the tx (`is_valid=true`); dugite's parallel UPLC eval FAILED (budget exhausted).
- Node's safety valve ("trust on-chain consensus") kept it byte-exact at block level — but internal phase-2 eval diverged.
- **mem_remaining=109** at exhaustion → dugite consumed ~109 mem MORE than Haskell over the run. Tiny, precise over-charge → likely a narrow per-builtin/per-step mem-accounting diff (or one builtin's mem cost slightly high), NOT a wholesale cost-model mismatch. cpu had 14547 to spare (cpu fine; mem is the divergent dimension).

## Tx facts (Koios, preview)
- block 4395048, slot 115145170, epoch 1332. tx_size 1705, fee 718220. invalid_after 115146008.
- **PlutusV2** DeFi tx. 3 reference scripts: `3922229b…`/1570B, `5a71ae99…`/8734B, `a7913bb6…`/5088B.
- Complex context: multiple reference inputs with oracle/AMM inline datums (rationals, price feeds, pool params). Collateral present (5.39 ADA). Mint: policy `45df5f27…` token `55534472` (USDr?).
- Protocol/AMM contracts (Indigo/Minswap-class). Heavy script → near the mem budget.

## Diagnosis plan
1. **Re-evaluate this exact tx through dugite's UPLC with per-builtin budget tracing** (need: tx CBOR + resolved inputs/datums/ref-scripts + epoch-1332 PV2 cost model). Find the operation(s) where dugite's mem charge exceeds Haskell's.
2. **Cost-model check**: confirm dugite's PlutusV2 cost model == preview epoch-1332 on-chain params (rule out #764-class default-fallback). Koios `koios_cli_protocol_params` / dugite inspect_costmodels.
3. **Per-builtin mem accounting cross-check vs Haskell plutus** (cardano-haskell-oracle): which ops charge mem, constant vs size-based, any subtlety (e.g. builtin result mem, constant node mem, CASE/CONSTR mem in PV2) dugite might mis-account.
4. Confirm fix byte-exactly (re-eval passes within budget), add conformance vector.

## Notes
- dugite-uplc CEK declared "100% conformant" (CLAUDE.md) — but conformance corpus didn't cover this. Live DeFi tx is a new edge case.
- Related history: #764 (PV3 budget = default-fallback-from-PPUP-wipe, NOT CEK over-count); #761 (BLS flat wire-id). This is PV2 + mem-dimension → likely distinct.
- Tools: DUGITE_DUMP_FAILED_ONLY (not enabled on current run), inspect_costmodels, release-prof unstripped binary.
- Re-fetch tx: `koios_tx_cbor` for b8e75c70…; resolve inputs/ref-inputs for the script context.

## Status: FIXED + committed `c1d0eb7f7a` (cost_apply.rs).
CANONICAL-confirmed (cardano-haskell-oracle: builtinCostModel{B,D}.json + ProtocolVersions.hs + CostingFun/Core.hs): apply_v1/v2 hardcoded VariantB division cost shapes for ALL PVs (no is_variant_d gate). At PV11/VariantD: modInteger/remainderInteger mem subtracted_sizes→linear_in_y2(=size_y); modInteger/divideInteger cpu const_above_diagonal→above_and_below_diagonal (==MultipliedSizes, since max*min==x*y). dugite DEFAULT table already had the correct VariantD shapes — only the on-chain apply path was wrong.
Fix: is_variant_d(pv>=11) + variant-aware divmod_cost() at all 8 V1/V2 division sites. Byte-identical at PV<11 (480 uplc tests incl. PV8 phase2_onchain pass). TDD variant-boundary test. just check green. Upstream golden conformance running (b17fxiicc).
DEPLOY: batched with ISSUE-3 — rebuild + restart with DUGITE_PHASE2_DUMP_DIR set (auto-capture future divergences + confirm b8e75c70-class no longer recurs).
FOLLOW-UP: capture b8e75c70 as a PV11 V2 phase2_onchain regression fixture (all existing fixtures are PV8 — why this shipped).
  CAPTURE BLOCKED (2026-06-18): clean offline capture not achievable —
  (a) startup replay (replay_immutable_gap startup.rs:429 + volatile replay :591) uses BlockValidationMode::ApplyOnly, and apply.rs:1439 gates the phase-2 divergence-dump path on ValidateAll → no dump on replay.
  (b) apply_bench (the ValidateAll re-apply tool) predates UTxO-HD: dugite-lsm snapshot has 0 in-memory UTxO (UTxO lives in the LSM store it doesn't load) + reads immutable-only while block 4395048 is volatile (immutable tip ~115141668 < 115145170). → 0 blocks, can't validate.
  Only remaining capture = run the PRE-FIX node LIVE with --validate-all-blocks after deleting volatile-wal so it re-fetches+ValidateAll-applies 4395048 → dump; but that needs the fixed node stopped (can't run 2 nodes on one DB) + a multi-min re-sync + mutates db-preview. DECISION (user, 2026-06-18): accept the fix as complete; fixture deferred as a documented follow-up (the fix is already byte-exact-proven; a proper fixture needs a dedicated re-eval-tx-from-chain tool).
  NOTE: fix is already byte-exact-proven without the fixture (canonical builtinCostModelD.json citation == dugite DEFAULT table; variant-boundary unit test asserts exact shapes+coeffs; 6027 upstream golden conformance; PV<11 byte-identity). The live soak's empty phase2-dump dir is an ongoing real-tx regression signal. A proper fixture really wants a dedicated "re-eval tx from local chain (LSM+volatile)" tool — separate engineering task.
