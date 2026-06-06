# Engine State  (single source of truth — committed every wake)

## Control
- HALT: false
- refuter_N: 3
- daily_token_budget: 40000000
- cadence_floor_secs: 270
- cadence_ceiling_secs: 1800
- reference_node_socket: none        # Koios-first; set if a cn node is up

## Frontiers  (advance these; zero open divergence behind each)
- ledger.preprod:   BYTE-EXACT vs Koios at ep100/150/200/230 (Shelley→Conway) + ep57 per-cred exact + ep292/293 total active_stake HEAD-verified on the LIVE soak node (go(293)==Koios as(292), set(293)==as(293)) + ep293 reserves 13072484951876873 & treasury 1870588626354717 BOTH byte-exact (to the lovelace) vs Koios totals (wake63). So at ep293 the live HEAD node matches Koios on ALL three core accounting outputs (reserves+treasury+active_stake). NOTE: ep293 reserves/treasury are mithril-import-faithful + held; dugite's OWN reserves/treasury TRANSITION computation is covered separately by full-replay ep0-233. The ep292 -100 ADA candidate (2b) was a STALE dump — RESOLVED on HEAD. clean replay ep0-233 zero halts. Frontier HOLDS through ep293
- ledger.mainnet:   BYTE-EXACT vs Koios — reserves+treasury exact at ep212-221 (doc's +180.4B ep213 divergence GONE on HEAD); replay validating further
- sync.preprod:     from-genesis REPLAY clean + #9 snapshot-backend fix LANDED + LIVE-SOAK reached tip healthy (wake57): clone db-clones/preprod-soak fast-started via #9 Convertible mem->lsm path (NO genesis replay, utxo_count=4116338), caught up to live tip (node block 4793022 hash-matched koios, 1 block/28s behind live 4793023), 0 panic/0 OOM/0 wedge, RSS 4.8GB, CPU 1.5% idle-at-tip. *** SUSTAINED-WINDOW CONFIRMED wake60 ***: node tracks live tip in lockstep ~18min (block 4793042 == koios live 4793042, extends within seconds of each new block), 0 anomalies => GATE (2) live-sync-to-tip VALIDATED on preprod. Residual is GATE (3) only: #10 ref-script independent validation on the fast-start path (in FIXING via muscle we0nz74zr; node trusts consensus meanwhile, no wedge)
- sync.mainnet:     ~ep331 (last known good db-mainnet)
- phase2.preprod:   BYTE-EXACT (is_valid) on FULL-REPLAY — full preprod replay ep0-293 (V1/V2/V3, Alonzo->Babbage->Conway): 0 ValidationTagMismatch, 0 divergence dumps. #22 RESOLVED on HEAD. OPEN GAP on MITHRIL-FAST-START path: #10 (mod.rs:6411 drops reference-script bytes on import -> ref-input scripts unresolved at tip; ledger-exactness unaffected). Frontier holds for replay; fast-start ref-scripts blocked on #10 fix.
- phase2.mainnet:   inert until ep507 (V3)
- perf:             at-tip CPU bounded (15 hot peers); sync ~300 blk/s Byron

## Backlog  (ranked by impact; one advanced per wake)
0. [H][ledger][REAL-CURRENT, ROOT-CAUSED] mainnet ep246 reserves +82,270,482 / treasury -55,269. STRUCTURAL
   ROOT CAUSE (deep-dive wuc2kqb1z): the member-reward fold (rewards.rs:445-490) iterates go.delegations +
   separate go.stake_distribution.get(cred) lookup, whereas Haskell folds a SINGLE resolved active-stake VMap
   (resolveActiveInstantStakeCredentials) where swdStake = UTxO instant-stake <> reward-account balance and
   stake+delegation are bundled. When the two dugite maps disagree for a cred (e.g. stake_distribution per-cred
   value omits the reward-balance that pool_stake aggregated, or an ordering skew), dugite under-credits that
   member -> undistributed -> reserves (+82.27M; +55K treasury via shifted frTotalUnregistered partition).
   RULED OUT byte-exact: deltaR1/d/rho/tau, the prefilter (all drops correctly-deregistered), member_stake==0
   skip, leader/operator reward, SNAP-before-MIR ordering. FIX (discrete, careful, Tier A): build the member
   iteration from a single resolved active-stake source matching resolveActiveInstantStakeCredentials, then
   byte-exact verify reserves==Koios 12880948865137767 at ep246 + confirm ep209-245 unregressed. Haskell:
   RewardUpdate.hs:201-279 rewardStakePoolMember, Rewards.hs:261-282 rewardOnePoolMember, Stake.hs resolve...
   PARKED pending a focused fix session. state:PARKED-WITH-ROOT-CAUSE attempts:3
   PV3). FIRST divergence at ep246 (ep209-245 byte-exact); persists/amortizes ep246-250 (replay at ep262).
   dugite took ~82.27M LESS from reserves at the ep245->246 reward transition. Found by broad Koios sweep
   (NOT in the stale docs). state:ANALYZING. Likely: reward-pot/reserves-expansion (rho) / undistributed-
   rewards-return / MIR calc at the Allegra boundary. Dumps in epoch-dumps-engine/mainnet-ep213/.
1. [H][ledger] ep57 preprod stake-distribution -10 ADA. *** RESOLVED on clean HEAD (wake22-23): ***
   per-cred dump proves dugite ep57 = Koios 9957549164/9815680998 BYTE-EXACT; clean from-genesis replay
   crosses ep181 with NO WithdrawalAmountMismatch (verified to ep192+). Prior findings doc was STALE.
   state:DONE-on-clean-replay. RESIDUAL: the fork-induced variant is still real -> land #6 (apply_utxo_diff
   reconstruction fix) for production fork-robustness, with its own verification. Old context below:
   CONFIRMED so far: NOT reconstruction path (inert), NOT Dijkstra (inert/pre-era), reward_balance IS
   folded (epoch.rs:215). Measurement stalled: epoch.rs has MULTIPLE stake loops (live pool_stake ~205,
   per-cred ~267, snapshot pool_stake recompute ~852, mark construct ~326) and ad-hoc eprintlns kept
   hitting the wrong one. NEXT-ACTION (do NOT ad-hoc again): add per-credential stake_map values to the
   COMMITTED epoch-state-debug dump (epoch_state_debug.rs) so a replay yields per-cred JSON reliably; OR
   use the existing DUGITE_REWARD_DBG rewards.rs member-loop harness configured for these 2 creds. Then
   measure dugite ep57 utxo_stake for both vs Koios 9957549164/9815680998 to settle whether a clean
   immutable replay even reproduces the -5 ADA (0/931 diff suggests maybe NOT -> would re-open the
   fork/original-sync hypothesis).
2. [H][ledger] #11 mainnet stake-dereg residual (4 no-withdrawal cases diverge).
   state:NEW attempts:0  (replayable from db-mainnet; verify its epoch first)
2b. [ledger] preprod ep292 active_stake -100 ADA candidate *** RESOLVED-ON-HEAD wake62 (was STALE dump) ***.
   HEAD verification via the LIVE soak node (pid 99162, ep293) dugite-cli query stake-snapshot:
   Go=886,446,899 ADA == Koios as(292)=886,446,899.25 ✓ ; Set=912,041,407 ADA == Koios as(293)=912,041,407.32 ✓.
   Snapshot shift: HEAD go(293) IS the dump's set(292) one epoch later — HEAD shows it +100 ADA higher than the
   Jun-3 dump (886,346,899) and MATCHES Koios exactly. The -100 ADA was already fixed by code landed after Jun-3.
   The #481 lesson held again: a stale dump misled; regenerate/HEAD-verify before investigating any residual.
   ledger.preprod re-validated through ep293 (total active_stake byte-exact to ADA precision). state:DONE
3. [DONE-on-HEAD][ledger] mainnet ep213 reserves divergence (== #11): RESOLVED. dugite reserves+treasury
   BYTE-EXACT vs Koios at ep212-221 (the doc's +180.4B ep213 divergence is GONE; fix landed in last 2 days).
   Stake-dereg reward attribution correct (reserves exact). OLD (stale) context: mainnet ep213 reward divergence (== #11; doc dated 2026-06-04, RECENT -> likely REAL,
   not stale like ep57). 4 target creds: 53215c471b7ac752e3ddf8f2c4c1e6ed111857bfaa675d5e31ce8bce,
   6184f6e7229530a2d1f9f746112406100e2696dd7439ff8c52750700,
   af22f95915a19cd57adb14c558dcc4a175f60c6193dc23b8bd2d8beb,
   d9812f8d30b5db4b03e5b76cfd242db9cd2763da4671ed062be808a0. PLAN: mainnet from-genesis replay (db-mainnet,
   ep331; LONG - Byron+) with DUGITE_EPOCH_STATE_DUMP_CRED_FILTER=<4 creds> -> per-cred dump at ep213 ->
   compare vs koios.sh mainnet account_reward_history. NOTE: verify the finding isn't stale FIRST (re-read
   the doc's repro method; it bisected ep213 so may have a faster pre-ep213-snapshot path). state:NEW attempts:0
4. [DONE-on-HEAD][phase2] #22 CEK V1/V2 Babbage residual: RESOLVED. Full preprod replay ep0-293 with
   DUGITE_PHASE2_DUMP_DIR produced 0 phase-2 divergence dumps + 0 ValidationTagMismatch -> dugite's Plutus
   is_valid is byte-exact for every on-chain script across all eras. The stale 628-divergence buckets
   (398 budget/186 Error/44 unIData) are fixed on HEAD. (Caveat: this proves is_valid agreement, the
   chain-critical property; per-redeemer ExBudget exact-match isn't separately instrumented but no is_valid
   divergence means no script flipped validity.)
5. [L][phase2] #14 V3 TxInfo deferred fields (inert until mainnet ep507).
   state:NEW attempts:0
9. [DONE] Snapshot UTxO-backend mismatch: FIXED + operationally verified + committed. (was:)
   [M][sync/perf][REAL] Snapshot UTxO-backend mismatch: mithril-import/haskell-conversion saves the native
   ledger snapshot with backend `dugite-mem`, but `run` defaults to `dugite-lsm` -> "snapshot backend does not
   match configured backend" -> snapshot DISCARDED -> FULL genesis replay on every restart instead of
   fast-start. Real robustness/perf gap (not byte-exactness). Fix: save the snapshot in the configured backend
   (or auto-run the snapshot converter, or make mithril-import honor --utxo-backend). Found via the sync-gate
   live-node test (wake48). state:NEW attempts:0
10. [H][phase2/sync][REAL-NEW, wake57] Reference-script resolution fails at tip on the #9 fast-start clone.
   During the live-soak (db-clones/preprod-soak, fast-started via #9 Convertible mem->lsm), EVERY tip tx that
   spends a script-locked input or does a script-locked withdrawal via a REFERENCE script fails dugite's
   independent validation: phase-1 `MissingScriptWitness`/`MissingWithdrawalScriptWitness` + phase-2
   `script not found for redeemer purpose`. The node "trusts on-chain consensus" so it does NOT wedge, but
   dugite would REJECT these txs standalone. Same script hashes recur across many independent txs
   (ec80112317817fdf..., 744837b0a352566983276e1fb256e428d96eab87cc42972261e0c88b withdrawal,
   85e3bfa6b315ad81..., d55eb689d83301fb...), i.e. these are reference scripts held in old UTxOs.
   HYPOTHESIS: the #9 Convertible mem->lsm fast-start path (load_snapshot_with_backend_guard ->
   attach_utxo_store inline-UTxO migration) does NOT preserve the reference-script bytes/index on converted
   UTxO entries -> reference inputs resolve to a UTxO with no script -> resolution fails. Since phase2.preprod
   was locked BYTE-EXACT on a FULL-REPLAY sync (0 divergence), these WARNs appearing ONLY on the fast-started
   clone would be a REGRESSION IN THE #9 FIX (a real correctness gap I introduced), NOT a base phase-2 gap.
   FALSIFIABLE EXPERIMENT (next wake, Tier A'/B): run a node that did a FULL from-genesis (or non-converted)
   replay to the SAME preprod tip and check the SAME slots (125081911, 125081937, 125081958, 125082000,
   125082081) for the identical WARNs. NOT present on full-replay => #9 reference-script gap (fix: rebuild the
   reference-script resolution from converted UTxOs in the Convertible arm). Present on both => genuine
   reference-script-from-UTxO resolution gap independent of snapshot. Cheaper pre-check: dump whether the
   UTxO entry holding script 744837b0a3...  has its reference-script bytes after the mem->lsm conversion.
   *** ROOT-CAUSED wake58 (NOT a #9 regression — #9 EXPOSED a latent mithril-import gap) ***:
   Reproduced via Koios tx_info on failing tx 0d325a6e... — it supplies 5 scripts via REFERENCE INPUTS, and
   the exact hashes dugite reports Missing/not-found (744837b0a3=ref-UTxO f08f73509b0d3b4a#0, d55eb689d8=
   e2766b4eb2b8d4da#0, ec80112317=44cdaca0dd0fa27a#0, d0adeb2053=cbe7a6c5e9b92eb2#0) are reference scripts
   held in OLD (pre-snapshot) UTxOs. THE BUG: mithril/haskell-import UTxO loader at
   crates/dugite-node/src/node/mod.rs:6411 HARD-SETS `script_ref = None` for every output, on the FALSE premise
   (comment 6406-6410) that "reference scripts are only needed for Phase-2 ... during gap replay" — but
   ref-script UTxOs created LONG before the snapshot are never re-created in the bounded gap replay, so their
   script_ref stays None forever -> dugite can't resolve them as reference inputs at the live tip. Before #9 the
   snapshot was DISCARDED + full-replayed from genesis (rebuilding all script_refs from block data) so it never
   manifested; #9 made the node USE the snapshot, inheriting script_ref=None. SCOPE: ledger byte-exactness
   UNAFFECTED (UTxO membership + stake distribution don't need script_ref; phase2.preprod full-replay result
   stands). It's a phase-2 INDEPENDENT-validation gap SPECIFIC to mithril-fast-start nodes; masked by
   trust-on-consensus (no wedge), but a real correctness gap (esp. for a block producer / trustless validator).
   FIX (TWO-PART, exactly 2 crates — within commit limit): (1) dugite-serialization
   src/mempack/txout.rs::decode_tag5 currently dumps the datum+script tail into opaque_tail (line 466-467) and
   sets script_ref:None (479) — must PARSE the tail and populate script_ref:Some(raw_bytes) (struct already has
   the field, line 98); (2) dugite-node mod.rs:6411 — decode MemPackTxOut.script_ref raw bytes into a ScriptRef
   enum (CBOR script tag 0=Native/1=V1/2=V2/3=V3). VERIFY: re-run the mithril-fast-start soak; the failing slots
   125081911/125081937/125081958/125082000/125082081 must NO LONGER emit MissingScriptWitness / "script not
   found for redeemer purpose". Tier A' (phase-2). state:VERIFYING-PENDING attempts:1  FIX COMPLETE (muscle
   we0nz74zr, hash-oracle PASSED byte-exact, checks_green, 2 crates; patch + worktree wf_41bd7059-365-1). See
   In-progress for the VERIFYING plan (SIGTERM soak -> build -> re-soak -> WARNs-gone -> gauntlet -> commit).
16. [L][phase2][LATENT, from gauntlet wqwgen1p0] decode_imported_script_ref hard-codes Plutus language tag
   0->V1,1->V2,2->V3,3->V4 as 'global', but the MemPack PlutusScript tag is ERA-RELATIVE (per-era packTagM).
   Byte-exact for ALL CURRENT eras only because each era's language list is a strict PREFIX [V1,V2,V3,V4] (no
   reorder/removal), so era-relative index == fromEnum(language) today. Patch comments self-contradict
   ('era-relative' vs 'global'). NOT a current divergence. FIX: make the mapping era-aware (or assert the prefix
   invariant + comment) when a future era reorders/removes a language. state:NEW attempts:0 (follow-up after #10 lands)
15. [M][phase2][REAL-NEW wake86] Fast-start residual 277 "script returned Error term" — SEPARATE from #10's
   key/datum/refscript/multiasset (those are fixed: 549->277, not-found+budget zeroed). On the FULL-fix re-import
   re-soak (db-clones/preprod-verify10c retained), 277 DISTINCT txs each emit one "uplc fails but on-chain
   is_valid=true; trusting consensus" Error-term (e.g. 8b1a6a78 @slot125081937, ff9bc7d2, ff448523, fcee9506).
   DISCRIMINATOR: full-replay (phase2.preprod) is BYTE-EXACT for these -> still PURELY import-incompleteness, a
   field the import gets wrong that full-replay gets right. Already fixed+ruled-out: TxIx key, inline datum,
   reference script, multi-asset value. LEADING SUSPECT (to verify, not assume): compact-ADDRESS decode for the
   MemPack TxOut tags 2/3 Addr28Extra forms -> wrong txInfoInputs[].address in ScriptContext -> script returns
   Error. Spread across 277 txs (broad field issue, not a few scripts) supports an address-class cause. NON-chain-
   critical (trust-on-consensus, no wedge). NEXT: diagnose muscle comparing a failing tx's resolved-input
   address (dugite import vs Koios) using db-clones/preprod-verify10c. state:NEW attempts:0
6. [H][ledger] FORK-ROBUSTNESS (elevated M->H, now vindicated): apply_utxo_diff reconstruction didn't
   replay stake_map -> the FORK-INDUCED variant of the ep57 bug. Clean HEAD replay is correct, but a live
   sync hitting a rollback could still corrupt stake. The refuted gauntlet trusted a STALE doc; this IS a
   real fix. Patch: scripts/prod-readiness/candidate-latent-fix-apply_utxo_diff.patch + worktree
   wf_9be2125b-d01-1. Verify via a fork-exercising scenario, then land. state:NEW attempts:0
7. [M][ledger] LATENT Dijkstra SUBUTXO bug: apply_sub_transactions mutated utxo_set but NOT stake_map/
   ptr_stake (asymmetry). Valid fix + add_instant_stake/delete_instant_stake helper refactor preserved in
   scripts/prod-readiness/candidate-latent-fix-dijkstra-subutxo.patch + worktree wf_dcc190ba-a5c-1. Inert
   for ep57 (Dijkstra is post-Conway). Land separately after its own verification. state:NEW attempts:0

## In-progress
- item: #10 (now "fast-start phase-2 IMPORT COMPLETENESS") state:FIXING (multi-asset reconstruction bug). ***
  VERIFYING wake106: endianness CORE verified BUT thorough check found a multi-asset REGRESSION *** robust-fix
  re-soak (verify10e): TxIx auto-detect=Big correct (script-not-found 0, budget 0, safety-net sane), BUT
  MultiAssetNotConserved jumped 32->316 (ALL input_side:0 = imported multi-asset UTxO carries ZERO of the asset).
  Cause: keys now resolve idx>=1 inputs (previously InputNotFound, 600 baseline), EXPOSING that the multi-asset
  reconstruction stores empty/wrong assets despite the 10-NFT unit oracle passing (same decode-vs-real-data
  pattern). So the reconstruction has a real gap. Launched diagnose+fix muscle w34va8uxf: locate the failing rep
  layout via the real blob + Koios (sample tx 08e9548154... policy d8906ca5...), fix parse_multi_asset_rep and/or
  the node value fold, byte-exact oracle = reconstructed multi_asset == Koios asset_list. Main reset clean
  (ROBUST patch preserved as base). VERIFY: re-soak MultiAssetNotConserved back to ~baseline + endianness win
  kept. was: state:VERIFYING-RESOAK (ROBUST fix). Build DONE
  (BUILD_EXIT=0). DROVE re-verify: cloned db-preprod-sync -> verify10e, ran ROBUST binary (pid 47327, port 4208).
  *** AUTO-DETECT WORKS ON REAL BLOB ***: log "Auto-detected MemPack TxIx endianness from snapshot data
  txix_endianness=Big" (correct for preprod new format), safety-net distribution SANE (txix_low=3131782 vs
  txix_mult256=62 -> low>>mult256, net did NOT trip). utxo_count=4116338 skipped=0. Node syncing 124999169->tip.
  NEXT WAKE VERDICT: grep verify10e-resoak.log -> must MATCH 549->277 (not-found 0, budget 0, 4/5 slots clean);
  if so -> RE-GAUNTLET (version-independent, should clear) -> commit. was: state:VERIFYING-BUILDING (ROBUST fix). *** muscle
  w1m4bxztw COMPLETE wake103, checks_green, version/layout-INDEPENDENT, 2 crates (8 files +2080/-239) ***. DROPPED
  layout-conditional endianness entirely. REPLACED with: (1) documented snapshotTablesCodecVersion as upstream's
  authoritative disambiguator (not exposed to dugite import inputs today -> fall through); (2) EMPIRICAL
  AUTO-DETECT detect_txix_endianness — samples first 2000 keys via RawKeyWalker, decodes under BOTH endiannesses,
  picks the sane index distribution (dense [1,255], sparse at nonzero mult-256); TvarIterator::new auto-detects;
  (3) HARD SAFETY NET assert_txix_distribution_sane (low > mult256*8) accumulated during import, ERRORS LOUD on
  mis-key (no silent corruption). Multi-asset reconstruction (parse_multi_asset_rep) ported. ORACLES: legacy
  preview fixture auto-detects LE -> txix==1 (BE read TRIPS the net); synth BE blob auto-detects Big -> txix==1;
  deliberate mis-key TRIPS the net; both-endianness pinned. ROBUST patch saved
  candidate-fix-10-ROBUST-autodetect-endianness.patch (2724 lines, applies clean) + applied to MAIN + build pid
  46612 (.jobs/verify-build-10e.log). Handles ALL combos (flat/nested x LE/BE) by reading data = gauntlet-proof
  by construction. NEXT WAKE: BUILD_EXIT=0 -> fresh import from db-preprod-sync (auto-detects BE) -> re-soak ->
  KEEP 549->277 -> RE-GAUNTLET -> commit. was: state:FIXING (ROBUST endianness). *** RE-GAUNTLET
  wj0pzgzaq REFUTED 2/3 — layout-proxy is WRONG (gauntlet's 2nd correct catch) ***. Upstream history (2 refuters,
  convergent commit cites): flat-`tables` layout (~oc 0.25.0.0 Apr-2025) and the BE TxIx flip (BigEndianTxIn
  byteSwap16, commit 9ac9388 Aug-2025) and the flat-tables MOVE (286ad7ec8 Oct-2025) landed in DIFFERENT
  releases -> layout is NOT a proxy for endianness. Real shipped snapshots exist as flat+LE AND nested+BE, both
  mis-keyed by the conditional layout mapping (idx1<->256). BE-flip added no version byte; flat-LE vs flat-BE are
  byte+layout identical -> resolver CANNOT branch on layout. Authoritative disambiguator = snapshot CODEC VERSION
  (snapshotTablesCodecVersion/TablesCodecVersion1, da3934cf8), layout-independent. DID NOT COMMIT; main reset
  clean. Launched ROBUST fix-muscle w1m4bxztw: determine endianness by (1) authoritative codec version if
  accessible, else (2) EMPIRICAL auto-detect from index distribution (version/layout-independent: wrong
  endianness maps true 1..255 -> multiples of 256), + (3) HARD post-decode sanity assertion (error-loud on
  mis-key, never silent). Keep multi-asset+refscript+datum carry-over (gauntlet-approved). Oracles on BOTH
  fixtures + a mis-key-trips-safety-net test. was: state:GAUNTLET-PENDING (CONDITIONAL fix). *** VERIFYING
  PASS wake96 *** conditional-fix re-soak (verify10d, new format=Big) MATCHES the unconditional run: not-found 0,
  budget 0, MissingScriptWitness 0, ~279 Error-term (=#15 residual), 4/5 target slots clean. So conditional fix
  is chain-equivalent for the NEW format (BE) AND fixes the legacy regression (LE, unit-proven via
  test_legacy_fixture_first_entry_txix_le). Both formats correct. Launched RE-GAUNTLET wj0pzgzaq (refuterN=3) —
  this time probing layout=>endianness mapping completeness/reliability + carry-over integrity. On PASS -> commit
  the CONDITIONAL patch via gh/HTTPS. RESIDUAL ~277 Error-term = #15 (db-clones/preprod-verify10d kept for it).
  was: state:VERIFYING-RESOAK (CONDITIONAL fix). Build DONE
  (BUILD_EXIT=0, 1m39s). DROVE re-verify: cloned db-preprod-sync -> verify10d, ran CONDITIONAL-fix binary
  (pid 2797, port 4207). Import log CONFIRMS the conditional logic: "txix_endianness=Big" for the flat `tables`
  new-format snapshot (legacy/LE path covered by green unit test test_legacy_fixture_first_entry_txix_le).
  utxo_count=4116338 skipped=0. Node syncing 124999169 -> tip. NEXT WAKE VERDICT: grep verify10d-resoak.log —
  must MATCH verify10c (549->277, not-found 0, budget 0, 4/5 target slots clean); if so the conditional fix is
  chain-equivalent for the new format AND legacy is unit-proven -> RE-GAUNTLET -> commit. was:
  state:VERIFYING-BUILDING (CONDITIONAL fix). *** muscle
  wauynb0ku COMPLETE wake94, checks_green, addresses the gauntlet's legacy regression, 2 crates ***. Replaced
  unconditional-BE with snapshot-version-CONDITIONAL endianness: `enum TxIxEndianness{Little,Big}`;
  decode_mempack_txin(data, endianness); TvarIterator::new_with_endianness; resolve_inmemory_tables_path returns
  (PathBuf, TxIxEndianness) = flat `tables`->Big (BigEndianTxIn byteSwap16), nested `tables/tvar`->Little (raw
  MemPack Word16). Kept BOTH pinned tests (BE-new + LE-legacy) + test_legacy_fixture_first_entry_txix_le (legacy
  fixture first entry -> txix==1 under LE, 256 under BE = mirror-regression guard). Multi-asset+refscript+datum
  carried over. Both canonical Haskell sources quoted. CONDITIONAL patch saved
  candidate-fix-10-CONDITIONAL-endianness.patch (2395 lines, applies clean) + applied to MAIN + build pid 2336
  (.jobs/verify-build-10d.log). NEXT WAKE: BUILD_EXIT=0 -> fresh import from db-preprod-sync (NEW format=BE) ->
  re-soak -> KEEP 549->277 (not-found+budget still 0) -> RE-GAUNTLET (must now clear the legacy-LE dissent) ->
  commit. was: state:FIXING (endianness REFINEMENT). *** GAUNTLET
  wqwgen1p0 verdict wake89: passed 2-1 (refuteCount=1) BUT the dissent is EMPIRICALLY CORRECT -> DID NOT COMMIT
  (don't blindly trust majority) ***. The unconditional-BE TxIx fix is WRONG for legacy snapshots: TxIx
  endianness is SNAPSHOT-VERSION-DEPENDENT. I VERIFIED byte-by-byte: new flat `tables` (>=11.0.1) index1=`00 01`=BE
  (BigEndianTxIn byteSwap16); legacy nested `tables/tvar` (<=10.6.x) index1=`01 00`=LE (raw host MemPack). Both
  share envelope 81bf5822 (can't branch on content) but resolve_inmemory_tables_path branches by FILE PATH (flat
  file=new/BE; nested tvar=legacy/LE) — same oc-1.0.0.0/node-11.0.1 boundary flipped BOTH layout and endianness.
  Unconditional BE would silently corrupt every TxIx>=1 from legacy/preview imports (01 00 -> 256) = mirror-image
  regression the 2 pass-voters missed (they only tested new-format). FIX (muscle wauynb0ku): make endianness
  CONDITIONAL on layout (flat->BE, nested-tvar->LE), threaded from the import call site through TvarIterator to
  decode_mempack_txin; keep BOTH a LE pinned test (legacy fixture preview_tvar_head_64k.bin) AND a BE pinned test.
  Main reset CLEAN (FULL unconditional-BE patch NOT committed; superseded). VERIFY: both fixtures' index-1 ->
  txix==1 under their format; re-import re-soak keeps 549->277. was: state:GAUNTLET-PENDING (FULL fix). *** VERIFYING
  wake86: MAJOR chain-level SUCCESS (not a no-op this time) *** full-fix re-soak (verify10c) divergences
  549->277 with KEY-RESOLUTION classes ELIMINATED: "script not found" 11->0, "budget exhausted" 41->0,
  MissingScriptWitness 0; 4 of 5 original target slots now CLEAN. NO regression (counts only dropped). The
  BE-key fix made the imported refscript/datum data finally RESOLVE at phase-2. Launched GAUNTLET wqwgen1p0
  (refuterN=3) on the FULL fix; on PASS -> commit (FULL patch on main). RESIDUAL: 277 "script returned Error
  term" persist (down only 14) = SEPARATE import cause, filed as #15. was: state:VERIFYING-RESOAK (FULL fix). Build DONE
  (BUILD_EXIT=0, 1m39s). DROVE fresh import re-verify: cloned db-preprod-sync -> db-clones/preprod-verify10c, ran
  FULL-fix binary (pid 56221, /tmp/engine-verify10c.sock, port 4206). Import clean (utxo_count=4116338 skipped=0)
  with BE keys + datum + refscript + multi-asset. Node syncing 124999169 -> tip; re-processes the failing slots.
  NEXT WAKE VERDICT (the culmination of the whole #10 arc): grep verify10c-resoak.log for 291 "Error term" + 41
  "budget exhausted" + 11 "script not found" at slots 125081911..125082081 (and overall). DROP TO ~0 = #10
  end-to-end VERIFIED (BE-key fix lets the refscript+datum data finally resolve) -> gauntlet -> commit FULL fix.
  Any meaningful residual = not done. was: state:VERIFYING-BUILDING (FULL fix). *** fix-muscle
  wagcpug42 COMPLETE wake84, checks_green, KEY-correctness oracles PASS, 2 crates (8 files +1453/-166) ***.
  PRIMARY: mempack/mod.rs:68 from_le_bytes->from_be_bytes for the UTxO-HD ordered-store KEY's TxIx (#461
  reconciled: generic MemPack Word16 IS host-LE, but the on-disk tables KEY is BE so lexicographic order ==
  numeric TxIx order; repurposed pinned test ->_be_v11). SECONDARY: full multi-asset Value reconstruction
  (parse_multi_asset_rep, byte-exact port of Mary CompactValue 5-region rep; node import folds triples into a
  full Value instead of Value::lovelace). *** KEY-CORRECTNESS ORACLE (the anti-no-op proof) ***: new gated test
  asserts 00000c0c...#1 decodes to txix==1 (NOT 256) with coin 1_750_000 + smooth idx distribution; all 3
  real-blob oracles (txix-key, inline-datum bytes, refscript hashes) PASS vs the actual 885MB blob. FULL patch
  saved scripts/prod-readiness/candidate-fix-10-FULL-refscript-datum-endianness.patch (1972 lines, applies clean)
  + applied to MAIN + release build pid 55768 (.jobs/verify-build-10c.log). NEXT WAKE (chain-level proof): on
  BUILD_EXIT=0, fresh import from db-preprod-sync -> re-soak -> the 291/41/11 phase-2 divergences MUST drop to ~0
  (now keys are correct, the refscript+datum data finally resolves) -> gauntlet -> commit. Old notes below. was:
  state:FIXING (endianness). wake79: analyze muscle
  wxuwzffyl FULLY COMPLETE (rootcause confidence 0.96: "TxIx decoded LE not BE at mempack mod.rs:68"). Launched
  FIX muscle wagcpug42 (Opus, worktree, Tier 1) to: STEP0 apply candidate-fix-10-COMPLETE-refscript-datum.patch
  (base), then (1) mod.rs:68 from_le_bytes->from_be_bytes with #461 RECONCILIATION (determine decoder scope:
  tables-only=flip vs shared=tables-specific BE path; explain BE = ordered-store key sort-order) + fix the pinned
  test/fixture (0100 false-LE -> real 0001 BE), (2) multi-asset drop mod.rs:6435. Key-correctness oracle
  (a UNIT test is NOT enough — last fix passed unit oracles but was a runtime no-op): gated blob-decode test that
  a real idx-1 entry decodes to index==1 + store lookup of 00000c0c...#1 yields coin 1750000 (Koios). VERIFY
  (next, chain-level): re-import re-soak drops 291/41/11 to ~0. Once keys are correct the refscript+datum decode
  finally takes runtime effect. Old residual notes below. was: state:ROOT-CAUSED (residual). *** wake78: analyze
  muscle wxuwzffyl found THE BUG, triple-confirmed empirically *** ROOT CAUSE = crates/dugite-serialization/
  src/mempack/mod.rs:68 decodes TxIn output index (TxIx) as `u16::from_le_bytes` but the on-disk UTxO-HD tables
  KEY is BIG-ENDIAN -> every imported UTxO at output index >=1 stored under a CORRUPTED key (index 1->256,
  2->512). At phase-2, utxo_set.lookup(input) by the REAL index MISSES -> 11 "not found" (script-locked input
  at idx>=1) + 291 "Error term" (co-input/value at idx>=1 absent -> malformed ScriptContext txInfoInputs) + 41
  "budget" (same wrong context). THIS is why the byte-exact-PROVEN datum/refscript decode fix was a RUNTIME
  NO-OP: data decodes correctly but is filed under the WRONG KEY, so phase-2 lookup never finds it (the
  static-decode-vs-runtime disconnect). RECONCILES with ledger byte-exactness (wake62/63): stake/reserves
  summing ITERATES all entries (key-independent) so totals stayed exact; only LOOKUP-by-index broke. PROOF
  (triple): 3 Koios samples store at #256 not #1 (value matches); index-0 entries correct; raw-blob probe
  txix_bytes=0001 (BE=1, LE=256). CAUTION: flips a PINNED test test_mempack_txix_endianness_pinned_le_v11 tied
  to issue #461 — #461's LE assumption is contradicted by real preprod v11 data; the tables KEY is BE (likely
  for sort-order in the ordered store), distinct from generic MemPack Word16 host-native -> reconcile carefully,
  don't just delete the test. FIX (next wake, fix-muscle, builds on the refscript+datum base which becomes
  effective once keys are correct): (1) mod.rs:68 from_le_bytes->from_be_bytes; (2) fix test+fixture
  tests.rs:42-49,95-130 (fixture uses 0100 for "index 1"; real on-disk is 0001); (3) secondary multi-asset drop
  mod.rs:6435 Value::lovelace(coin) discards txout.multi_asset (decoder recovers it, txout.rs:94; only 1269
  UTxOs, not the #10 cause but fix same pass). Still 2 crates (dugite-serialization + dugite-node). VERIFY:
  re-import re-soak must drop 291/41/11 to ~0. Haskell: Cardano.Ledger.TxIn MemPack TxIn = TxId then TxIx
  (Word16). Was: state:DIAGNOSING (residual). *** VERIFYING VERDICT
  wake75: FAILED end-to-end — the divergence is NOT gone *** The complete-fix re-soak (verify10b) shows IDENTICAL
  counts to before the datum fix: 291 "script returned Error term" + 41 "budget exhausted" + 11 "script not
  found" (was 290/41/11). So the datum+refscript DECODE fix (unit-oracles PROVEN byte-exact) did NOT change
  runtime phase-2 behavior at all. The discipline caught what green tests would have hidden (had I committed on
  oracle-green, I'd have shipped a no-op-at-runtime fix). FALSIFIED multi-asset hypothesis (tx 578069c6 has 3
  ZERO-asset inputs). KEY PUZZLE: decode-oracle says 0/59,872 tag-5 lack script_ref, yet runtime STILL emits 11
  "not found" -> a STATIC-DECODE vs RUNTIME-RESOLUTION disconnect. HYPOTHESIS (to verify, not assume): the
  corrected import data (OutputDatum::InlineDatum + script_ref @mod.rs:6413-6445) is NOT reaching the phase-2
  ScriptContext/input-resolution path for confirmed-block validation (candidates: bincode roundtrip drops
  fields / ScriptContext reads datum from witness only / separate datum-table unfilled / resolution reads an
  unpopulated structure). Launched ANALYZE muscle wxuwzffyl to TRACE import->store->phase-2-resolution and pin
  the file:line where the data is lost. db-clones/preprod-verify10b retains the complete-fix imported state for
  the muscle. #10 fixes (refscript+datum) stay on MAIN as the base (correct, oracle-proven, no regression).
  NOTE (strategic, recorded): these divergences are NON-CHAIN-CRITICAL (node trusts on-chain consensus -> no
  wedge/no chain-divergence; like any snapshot-bootstrapped node). next: poll wxuwzffyl root-cause. was:
  state:VERIFYING-RESOAK (complete fix). Build DONE
  (BUILD_EXIT=0, 1m39s). DROVE fresh import re-verify: cloned db-preprod-sync -> db-clones/preprod-verify10b, ran
  COMPLETE-fix binary (pid 99008, /tmp/engine-verify10b.sock, port 4205). Import path ran WITH datum+refscript
  decode: "Loading UTxO set ...124999169/tables" -> "complete utxo_count=4116338 skipped=0" -> native snapshot
  1487.5 MB (vs 1161.9 MB refscript-only = the 778K inline datums + refscripts now included). Node syncing from
  124999169 toward tip; will RE-PROCESS the failing slots (>124999169). NEXT WAKE VERDICT: grep verify10b-resoak.log
  for the 290 "script returned Error term" + 41 "budget exhausted" + 11 "script not found" at slots
  125081911..125082081 (and broadly) — ALL GONE = #10 end-to-end VERIFIED -> gauntlet -> commit. ANY remaining
  eval-divergence on these txs (that full-replay validates) = still a gap -> back to FIXING. was VERIFYING-BUILDING. *** muscle
  wnqthg8c8 COMPLETE wake73, BOTH byte-exact oracles PASS, checks_green, 2 crates (7 files +1060/-115) ***.
  KEY DISCOVERY: the real datum bug was tag-4 CORRUPTION, not just a drop — BinaryData era = bare
  ShortByteString (VarLen||cbor), and the old decoder neither stripped the VarLen prefix (stored 1e581c... vs
  581c...) for ADA-only NOR kept the datum for multi-asset. Fixed: decode_tag4 uses decode_compact_value_exact
  (ADA-only+multi-asset) + new decode_binary_data() to yield bare Plutus Data CBOR; corrupt 132,148 ->
  correct 778,015 inline-datum outputs. era_conway.rs decode_plutus_data_cbor() (reuses the SAME read_plutus_data
  as tag-24 block decode); mod.rs import -> OutputDatum::InlineDatum{data,raw_cbor} (was None) = fixes the 290
  "Error term". GAP B residual-11: DIAGNOSED already-covered by the refscript base (all 4 scripts 7afbde08/
  23d3717e/bb4e5521/86820a34 decode from tag-5 MULTI-ASSET high-txix outputs, PlutusV3, hash byte-exact;
  tag5-without-script_ref = 0 across all 59,872 tag-5 entries) -> no further change, added oracle asserts. ORACLES
  (DUGITE_PREPROD_TABLES-gated, green vs real 885MB blob): refscript CIP-hash for all 4 gap-B + datum oracle
  (decoded tag-4 datums blake2b-256 == Koios on-chain datum hashes aafa39eb.../54cfb9d3...). COMPLETE patch saved
  scripts/prod-readiness/candidate-fix-10-COMPLETE-refscript-datum.patch (applies clean) + applied to MAIN
  (uncommitted) + release build pid 98554 (.jobs/verify-build-10b.log). NEXT WAKE: on BUILD_EXIT=0, fresh import
  from db-preprod-sync (NOT a reused db) -> re-soak -> the 290 Error-term + 41 budget + 11 not-found must ALL be
  GONE (full-replay byte-exact = oracle) -> gauntlet (refuterN=3) -> commit via gh/HTTPS. Old sub-state notes: *** VERIFYING VERDICT wake67
  (fix correct but INSUFFICIENT — gate correctly BLOCKS commit) ***: fresh fixed-binary re-import re-soak
  (db-clones/preprod-verify10) measured vs the OLD soak: "script not found for redeemer purpose" 379 -> 11
  (97% gone), MissingScriptWitness present -> 0, and the hash-oracle target txs (578069c6/0d325a6e/759eab17/
  dadf042b/8b1a6a78) now RESOLVE. NO regression (those txs were already divergent as "not found"). BUT resolving
  scripts EXPOSED a SIBLING import gap: 290 "script returned Error term" + 41 "budget exhausted" eval-divergences
  (all "uplc fails but on-chain is_valid=true; trusting consensus" -> no wedge). DISCRIMINATOR (decisive, no
  guess): full-replay is BYTE-EXACT for these same txs (phase2.preprod 0-divergence), so this is PURELY
  import-incompleteness -> mod.rs:6440-6444 DROPS INLINE DATUMS on import (OutputDatum::None, "Skip for now")
  -> resolved script + missing inline datum = wrong ScriptContext = Error term. Plus 11 RESIDUAL "not found"
  for OTHER scripts (7afbde08.../23d3717e.../bb4e5521.../86820a34..., slots 125009-125046) — a tag-5 sub-case
  the fix didn't cover (likely multi-asset tag-5 or another encoding). EXPANDED SCOPE (still 2 crates): (1) [DONE
  on main] decode_tag5 ref-script; (2) [TODO] mod.rs:6440 decode MemPackTxOut.datum (NOW populated by the fix,
  txout.rs:503/583) into OutputDatum::Inline(PlutusData) instead of None; (3) [TODO] residual-11 ref-script
  sub-case. RE-VERIFY target: Error-term + budget-exhausted + "not found" ALL clear on a fresh fast-start re-soak
  (full-replay byte-exact is the oracle). #10 patch stays on MAIN (correct base, no regression). next: expanded
  fix-muscle. *** wake68: expanded fix-muscle wnqthg8c8 LAUNCHED *** (Opus, worktree, Tier A'). Main reverted
  CLEAN (refscript changes removed; patch candidate-fix-10-mempack-refscript.patch is the base + applies clean to
  HEAD). Muscle instructed: STEP0 git-apply the refscript patch, THEN (A) decode MemPackTxOut.datum ->
  OutputDatum::Inline(PlutusData) at mod.rs:6440 (reuse existing PlutusData decoder; BinaryData=ShortByteString
  wrapping original CBOR), (B) cover the residual-11 refscript sub-case (script 7afbde08... etc — likely
  multi-asset tag-5). Given TWO oracles: extended refscript hash-oracle + NEW datum byte-oracle (decoded datum ==
  Koios inline_datum for an input of tx 578069c6). Verify target: fresh re-import re-soak clears 290 Error-term +
  41 budget + 11 not-found (full-replay = byte-exact oracle). next: poll wnqthg8c8 -> on pass, fresh re-import
  re-verify -> gauntlet -> commit complete fix. was: state:VERIFYING-RESOAK — build DONE (BUILD_EXIT=0,
  fixed binary target/release/dugite-node @01:37 w/ #10 patch on MAIN uncommitted). DROVE the fresh import with
  the FIXED binary: cloned db-preprod-sync (had haskell-ledger/ + immutable/, NO dugite snapshot = import state)
  -> db-clones/preprod-verify10, ran fixed binary (pid 46355, /tmp/engine-verify10.sock, port 4204). Log
  CONFIRMS the fixed import path ran: "Loading UTxO set from MemPack tables blob ...124999169/tables
  bytes=887932877" -> "UTxO loading complete utxo_count=4116338 skipped=0" -> "Haskell ledger import complete;
  native snapshot saved". So script_ref is now decoded via the fixed path. Node syncing from import point slot
  124999169 toward tip. *** NEXT-WAKE VERIFY (definitive) ***: the previously-failing slots 125081911/125081937/
  125081958/125082000/125082081 are AHEAD of 124999169, so the node RE-PROCESSES them on the way to tip ->
  grep verify10-resoak.log for MissingScriptWitness / "script not found for redeemer purpose" at those slots/
  their tx hashes (578069c6.../0d325a6e...). GONE = #10 end-to-end VERIFIED -> gauntlet -> commit. STILL present
  = fix incomplete (decode ok at unit level but mod.rs:6411 wiring or phase-2 resolution gap) -> back to FIXING.
  Direct alt-check: dugite-cli query the imported UTxO f08f73509b0d3b4a#0 for populated script_ref.
  *** HASH-ORACLE PASSED (chain-critical proof, not just green tests) ***: decoded the REAL on-disk preprod
  tables blob — input f08f73509b0d3b4a#0 -> Plutus(lang_tag2->global V3) -> blake2b_224(0x03||body) =
  744837b0a352566983276e1fb256e428d96eab87cc42972261e0c88b EXACT; e2766b4eb2b8d4da#0 -> d55eb689d8... EXACT.
  Fix: decode_tag5 now parses the exact CompactValue (new decode_compact_value_exact in compact.rs) then the
  MemPack Datum option + AlonzoScript blob -> script_ref:Some; mod.rs:6411 decode_imported_script_ref maps
  ScriptRefKind->ScriptRef (Plutus tag 0/1/2/3 monotonic across eras; native via decode_native_script_cbor).
  Quotes verbatim Haskell MemPack instances (BabbageTxOut/Datum/AlonzoScript/PlutusScript/CompactValue/
  ShortByteString/Hash; key subtlety: Datum's DatumHash is BE PackedBytes32 stored verbatim, UNLIKE
  DataHash32/Addr28Extra). PRESERVED: scripts/prod-readiness/candidate-fix-10-mempack-refscript.patch (923 lines)
  + worktree .claude/worktrees/wf_41bd7059-365-1. NOT committed (gated on VERIFYING + gauntlet).
  VERIFYING PLAN (next wake, heavy): RAM only 3GB free now -> (1) SIGTERM the current soak (gate-2 banked;
  frees ~4.4GB) NEVER pkill -9; (2) build fixed binary from the worktree (or apply patch to a build clone);
  (3) fresh mithril-fast-start re-soak on a clone; (4) CONFIRM the reference-script WARNs are GONE at slots
  125081911/125081937/125081958/125082000/125082081 (and ref-script txs validate independently) -> then gauntlet
  (refuterN=3, Aprime lenses) -> commit via gh/HTTPS on pass. Do NOT commit on green tests alone.
- item: #0 ep246 reserves +82,270,482 (Allegra/PV3) state:PARKED-WITH-ROOT-CAUSE — structural member-reward fold
- item: live soak (sync-gate) state:LIVE-SOAK VALIDATED — soak node tracks live preprod tip in LOCKSTEP
  (node block 4793042 slot 125082823 == koios live tip 4793042; extends within seconds of each new block,
  4793037->4793042 over ~2min). Continuously at/near tip since 16:55 (~18min), 0 panic/OOM/wedge/chain_diverged,
  RSS ~4.8GB, CPU 1.6% idle-at-tip. => readiness GATE (2) live-sync-to-tip VALIDATED on preprod (replay-clean +
  fast-start + sustained at-tip lockstep). NUANCE: this is a MITHRIL-FAST-START node, so gate (3) phase-2
  INDEPENDENT validation of ref-script txs is still gated on #10 (it trusts on-chain consensus meanwhile — no
  wedge). Gates (2) and (3) are separate; (2) is now met.
- attempts: 1
- ANALYZE RESULT (w6lsvu2p2, Opus): canonical Haskell active-stake =
  resolveActiveInstantStakeCredentials (Stake.hs @52ef3d5) — per registered+delegated
  credential, active_stake = (UTxO instant stake) <> (reward-account balance); spec
  stakeRelation = UTxO-stake ∪ rewards (Shelley epoch.tex stakeDistr). 3 merge branches.
- ROOT-CAUSE DISAMBIGUATION (engine): analyze guessed the missing term is the reward
  balance, but BOTH creds are short by EXACTLY 5,000,000 lovelace (630472f7 9952549164
  vs 9957549164; 7d3e2b31 9810680998 vs 9815680998). A reward BALANCE is never round
  5.000000 ADA -> this is a STRUCTURAL UTxO instant-stake attribution bug (prior finding),
  NOT the reward-balance term. Fix target = the UTxO->credential instant-stake routing
  (crates/dugite-ledger/src/eras/common.rs::apply_utxo_changes + stake_routing rebuild),
  matching the canonical per-credential UTxO aggregation. Tier A.
- FIX RESULT (whq17wl1f, Opus, Tier A, checks_green): ROOT CAUSE = apply_utxo_diff in
  crates/dugite-ledger/src/ledger_seq.rs (the state-RECONSTRUCTION path used by
  rollback_via_seq after FORK rollbacks) mutated only the UTxO set, never
  stake_distribution.stake_map/ptr_stake. The LIVE apply path (apply_utxo_changes) keeps
  per-cred instant-stake incrementally; reconstruction did not -> fork rollback dropped the
  per-cred stake while inverting UTxO -> exact 5-ADA add/subtract asymmetry. FIX: reconstruction
  now reuses the SAME stake_routing as the live path (parity by construction); inserts ADD,
  deletes SUBTRACT. Regression test apply_utxo_diff_replays_credential_stake_not_just_utxo_set.
  Haskell: resolveActiveInstantStakeCredentials (Stake.hs) active=instant_stake<>balance, deposit NOT included.
- VERIFICATION NUANCE: this is a FORK-ROLLBACK-path bug. A clean from-genesis IMMUTABLE replay
  has no forks, so it may NOT reproduce the -10 ADA (dump also lacks per-pool active_stake).
  PLAN: (1) fixed-binary from-genesis replay; diff ep57 dump vs the saved UNFIXED dump
  (epoch-dumps-engine/preprod-ep57). If different -> compare to Koios. If identical -> clean
  replay can't exercise the fork path; escalate to regression-test + gauntlet + a queued LIVE
  re-sync (true end-to-end: network forks, match Koios ep48-181). (2) COMMIT ONLY after byte-exact
  end-to-end confirms (honoring 'byte-exact or it isn't fixed'). Koios ref: ep57 active_stake 26538160048802.
- reproduced: YES, byte-exact vs AUTHORITATIVE preprod.koios.rest (ep293, real chain):
  ep57 pool1n84mel6 active_stake: Koios 26538160048802 vs dugite 26538150048802 = -10 ADA.
  Localized (prior + confirmed): 2 delegators each -5 ADA, creds 630472f7bfeb8fde...b40d
  and 7d3e2b319c66fe...64ca. Compounds -> +1 lovelace WithdrawalAmountMismatch at ep181.
- root-cause hypothesis (from diagnose, Opus): UTxO-set add/spend asymmetry in
  crates/dugite-ledger/src/eras/common.rs::apply_utxo_changes — some outputs are
  spend-subtracted from a credential they were never fully add-credited to. NOT the
  stake-snapshot rebuild. Tier A fix.
- GROUND-TRUTH FIX: the Koios *MCP* serves the WRONG network (Preview ep1320 vs preprod
  ep293) -> all ledger ground truth now goes through lib/koios.sh <net> <endpoint> REST.
- next: analyze muscle (research canonical Haskell UTxO->stake-credit attribution + spec)
  -> fix (worktree, Tier A) -> VERIFYING replay (reuse db-clones/preprod-ep57) -> gauntlet.

## Running jobs
- fix-muscle w34va8uxf (#10 multi-asset reconstruction bug: input_side:0 on imported multi-asset UTxOs, Opus,
  worktree, Tier A') — /workflows-visible. Poll next wake for FIX (byte-exact multi_asset==Koios oracle).
- verify10e-resoak — STOPPED CLEAN wake106 (VERIFYING: endianness verified [script-not-found 0/budget 0], but
  MultiAssetNotConserved 32->316 = multi-asset reconstruction bug). db-clones/preprod-verify10e RETAINED for the
  multi-asset diagnosis + #15.
- MAIN CLEAN. ROBUST patch (candidate-fix-10-ROBUST-autodetect-endianness.patch) = verified base + buggy multiasset.
- Patch history: COMPLETE / FULL(uncond-BE) / CONDITIONAL(layout) / ROBUST(autodetect, endianness-correct/multiasset-buggy).
- fix-muscle w1m4bxztw — COMPLETE (auto-detect, safety net, both-fixture oracles). patch
  candidate-fix-10-ROBUST-autodetect-endianness.patch + worktree wf_bdd8b73d-b58-1.
- import source db-preprod-sync/haskell-ledger/ INTACT; legacy fixture committed. db-clones/preprod-verify10d kept for #15.
- Patch history: COMPLETE(base) / FULL(uncond-BE WRONG) / CONDITIONAL(layout WRONG) / ROBUST(auto-detect, current).
- verify10d-resoak — STOPPED CLEAN wake96 (VERIFYING PASS: not-found 0, budget 0, ~279 Error-term=#15).
  db-clones/preprod-verify10d RETAINED (conditional-fix import state) for #15 (277 Error-term) diagnosis.
- verify10c GC'd. CONDITIONAL #10 fix on MAIN uncommitted (commit on re-gauntlet pass). 94GB disk free.
- fix-muscle wauynb0ku — COMPLETE (format-conditional, both pinned tests). patch
  candidate-fix-10-CONDITIONAL-endianness.patch + worktree wf_4e02da23-a01-1.
- import source db-preprod-sync/haskell-ledger/ INTACT; legacy fixture committed (test_fixtures/preview_tvar_head_64k.bin).
- db-clones/preprod-verify10c RETAINED for #15 (277 Error-term) diagnosis.
- Patch history: COMPLETE (base, no endianness), FULL (WRONG unconditional-BE — do NOT commit), CONDITIONAL (correct).
- fix-muscle wagcpug42 — COMPLETE (key-correctness oracles pass). FULL patch
  candidate-fix-10-FULL-refscript-datum-endianness.patch + worktree wf_843d9ff3-1d5-1.
- import source: db-preprod-sync/haskell-ledger/ INTACT for the re-verify.
- verify10b-resoak — STOPPED CLEAN wake75 (SIGTERM; verdict: 549 divergences UNCHANGED by datum fix). RAM freed.
  db-clones/preprod-verify10b retains the COMPLETE-fix imported state for the analyze muscle.
- COMPLETE #10 fix (refscript+datum) on MAIN uncommitted (correct oracle-proven base; commit gated on residual
  resolution + gauntlet). patch candidate-fix-10-COMPLETE-refscript-datum.patch.
- fix-muscle wnqthg8c8 — COMPLETE (both oracles pass). COMPLETE patch candidate-fix-10-COMPLETE-refscript-datum.patch
  + worktree wf_17ccaf91-99f-1. (Base refscript-only patch candidate-fix-10-mempack-refscript.patch also retained.)
- import source: db-preprod-sync/haskell-ledger/ INTACT (124995007+124999169) for the re-verify.
- fix-muscle we0nz74zr — COMPLETE (hash-oracle PASSED). Patch candidate-fix-10-mempack-refscript.patch + worktree.
- live-soak — STOPPED CLEAN wake65 (SIGTERM; ep293 snapshot saved; gate-2 VALIDATED banked).
- replay-measure  pid-file=.jobs/replay-measure.pid  (clean HEAD from-genesis replay climbing past ep93
  toward ep181). TEST: does it hit the original WithdrawalAmountMismatch halt at ep181? If it CROSSES
  ep181 cleanly -> ep57/ep181 RESOLVED on HEAD (prior finding stale); sync.preprod frontier unblocks. If
  it HALTS -> bug is fork-induced (vindicates refuted #6 apply_utxo_diff fix; gauntlet trusted stale doc).

## VERIFY FINDING (CORRECTED after gauntlet — my wake9 analysis was WRONG)
- The apply_utxo_diff fix is INERT for ep57. Gauntlet wm055td32 REFUTED 2/3 (haskell-semantics +
  edge-epoch): the ep57 -10 ADA is reproduced on a CLEAN from-genesis LOCAL replay (no forks) per
  REWARD-DIVERGENCE-FINDINGS.md, which applies every block via the LIVE path apply_utxo_changes and
  NEVER invokes rollback_via_seq -> apply_utxo_diff. So the 0/931 fixed-vs-unfixed diff is the
  SIGNATURE OF AN INERT FIX, not 'fork-path-only'.
- My 'set==Koios -> clean replay correct' inference was a SNAPSHOT ERROR: the -10 ADA is in the GO
  snapshot; I compared the SET total. The findings doc explicitly RULES OUT incremental/reconstruction
  upkeep ("the -5 ADA is in UTxO set content or address->credential attribution, NOT incremental upkeep").
- CORRECTED ROOT CAUSE: a LIVE-path apply_utxo_changes attribution bug (eras/common.rs). STAKE_CLAMP
  fired 6x in the live path; last-mile points at a BASE-SCRIPT address (addr_test1zpu3l06a...) / a
  non-Phase-5 output-creation path / an era-boundary path. The next ANALYZE/FIX targets THAT.

## Active job
- fix-muscle  workflow=wrpfacs13  (#9 snapshot backend mismatch -> mithril-import saves in configured backend)
  4-cred filter, DUGITE_EPOCH_STATE_DUMP=epoch-dumps-engine/mainnet-ep213). Climbing through Byron->ep214.
  CAUTION: free_ram=1GB at launch -> watch for OOM. When at ep213+: diff dugite reserves/treasury vs
  Koios totals (api.koios.rest) + per-cred reward vs account_reward_history. Confirm the +180.4B reserves.

## DB clones on disk
- db-clones/preprod-ep57         (unfixed-binary replay; baseline dump captured)
- db-clones/preprod-ep57-fixed   (fixed-binary replay, in progress)

## Gauntlet ledger  (passed/refuted approaches — never silently retry a REFUTED)
- REFUTED 2026-06-06 (w20c0k2qr, fix muscle self-refuted, NO code change): "fix the member-reward prefilter
  LOCATION (rewards.rs:461 / frozen addrsRew capture)". The prefilter is byte-exact correct: capture slot
  172800=ceil(4k/f) matches; set=reward_accounts.keys()==Haskell accounts domain; owner-exclusion, member_stake
  (utxo<>reward_balance), apply-time unregistered->treasury all match Haskell. The REAL bug: a credential is
  in Haskell's accounts at ep245 startStep but MISSING from dugite's reward_accounts (registration-tracking
  edge: reg/dereg/re-reg or MIR ordering). Treasury -55,269 = Haskell's frTotalUnregistered for it. DO NOT
  re-patch the prefilter location.
- REFUTED 2026-06-06 (whr4t971m, fix muscle self-refuted, NO code change): "ep246 reserves +82M is deltaR1
  too small from d-source". WRONG: RUPD at 245->246 uses prevPParams=ep244 d=0.26 (dugite correct);
  eta=1 (blocksMade>expectedBlocks) so deltaR1 identical for all candidate d; treasury -55K != 0.2*82M so
  NOT a pot/deltaR1 error. CORRECT cause: per-pool/per-member reward UNDER-distribution -> undistributed
  rewards return to reserves (treasury ~neutral). Same class as REWARD-DIVERGENCE-MAINNET-ep213.md's
  documented OPEN residual (whole-pool vs per-member prefilter). DO NOT retry a deltaR1/d fix.
- REFUTED 2026-06-06 (wm055td32, 2/3): "fix apply_utxo_diff reconstruction path to replay stake_map".
  *** REFUTAL PREMISE NOW DISPROVEN: the refuters (and the findings doc) claimed a clean local replay
  reproduces the ep57 -5 ADA. DIRECT MEASUREMENT (per-cred dump, wake22) proves clean HEAD replay is
  byte-exact CORRECT at ep57 (9957549164/9815680998 == Koios). So the bug is NOT in the live path and IS
  likely fork-induced -> the apply_utxo_diff reconstruction fix (#6) may be the REAL fix after all. The
  ep181-halt replay test decides. Lesson: the findings doc was stale; trust live measurement over docs. ***

## Token spend  (rolling; UTC-dated lines)
- 2026-06-06T11:41Z wake1 ~ build+mithril launch (assess+drive)
- 2026-06-06T11:52Z wake2 ~ replay reproduce + 2 launch-replay fixes + diagnose Workflow
- 2026-06-06T12:00Z wake3 ~ diagnose result + ground-truth fix (koios.sh) + byte-exact confirm
- 2026-06-06T12:25Z wake6 ~ analyze result + root-cause disambiguation + fix muscle launched

## Last node state
- sampled: 2026-06-06T17:01Z  node_pids="99162 99165" rss_mb=4798 free_disk_gb=143 free_ram_gb=4 jobs=9 halt=false
  AT-TIP: node block 4793022 slot 125082081 hash c8004a5b... == koios tip 4793022 (1 block/28s behind live 4793023); STAT SN, 1.5% CPU idle-at-tip; 0 panic/OOM/wedge

## Wake log
- wake1 2026-06-06T11:41Z: ASSESS found no db-preprod-sync (only db-mainnet + db-preprod-haskell
  [cardano-node format]). SCHEDULE picked #1 (top impact); its precondition is a preprod dugite
  db + the epoch-state-debug binary. DRIVE launched both as background jobs under the heavy-op
  lock (build=CPU, mithril=IO — recorded concurrency: different resource classes, both blocking
  prerequisites). RESCHEDULE ~1200s to let both finish.
- wake2 2026-06-06T11:52Z (early; prereqs finished fast): REPRODUCE. Found+fixed 2 launch-replay
  bugs (wipe must remove ledger-snapshot.bin AND haskell-ledger/ import dir, else node loads
  ep293 state and skips from-genesis replay). Clean from-genesis replay produced dumps ep0..62;
  ep56/57/58 captured. Fired diagnose muscle Workflow w2ci9weas (/workflows-visible) to localize
  the ep57 divergence vs Koios. Also committed observability change (diagnose mode routes all
  analytical work through the muscle for /workflows visibility).
- wake6 2026-06-06T12:25Z: analyze w6lsvu2p2 returned canonical Haskell active-stake formula
  (UTxO instant stake <> reward balance). Engine DISAMBIGUATED its root-cause: exact 5,000,000
  lovelace x2 rules out reward balance -> structural UTxO instant-stake attribution (prior finding).
  ANALYZING->FIXING; launched fix muscle whq17wl1f (Opus, worktree). Fix gated on VERIFYING replay
  reproducing 26538160048802 + gauntlet before any commit.
- wake9 2026-06-06T12:49Z: VERIFY decisive — fixed-vs-unfixed ep57 dump 0/931 diffs (fork-path bug,
  immutable replay can't exercise it); clean-replay SET total 254384027228099 == Koios byte-exact.
  Replay-gate ruled inapplicable; launched gauntlet wm055td32 with regression-test+parity evidence.
- wake10 2026-06-06T12:55Z: gauntlet wm055td32 REFUTED 2/3. CAUGHT MY OWN wake9 ERRORS: (a) 0/931
  diff = inert fix, not fork-path-only; (b) set-vs-go snapshot mistake in 'clean replay correct'.
  Real cause = live apply_utxo_changes base-script-address attribution (findings doc rules out
  reconstruction). Reverted inert fix from main; preserved as latent-fix patch (backlog #6).
  Item -> ANALYZING with corrected target. The gauntlet PREVENTED a #438-class wrong commit.
- wake11 2026-06-06T13:00Z: re-ANALYZE. Engine confirmed directly: common.rs:72 and mod.rs:2224
  stake_routing are identical (no routing discrepancy); snapshot is full UTxO-set rebuild -> bug is
  UTxO-set content (a ~5ADA UTxO missing/mis-credited per cred, STAKE_CLAMP underflow). Fired analyze
  wo8nuypp6 to pinpoint the exact live-path defect + enable STAKE_CLAMP logging for targeted re-replay.
- wake13 2026-06-06T13:15Z: analyze wo8nuypp6 returned a SELF-CONTRADICTORY result (research head:
  'snapshot omits reward_balance'; root-cause head: 'snapshot folds reward_balance, loss is stake_map
  clamp'). Engine DISAMBIGUATED from code: epoch.rs:215 total_stake = utxo_stake + reward_balance
  (reward_balance IS folded -> research head WRONG); snapshot uses the INCREMENTAL stake_map (epoch.rs:187,
  not rebuild_stake_distribution). So -5 ADA is in utxo_stake (stake_map), under-credited because a
  Phase-2 subtract clamped (common.rs:202-204 saturating_sub + skip-if-absent). Haskell instant-stake
  never underflows on a valid chain -> the REAL bug is a MISSING ADD in some output-creation path; the
  clamp is the symptom. Fired fix muscle with this confirmed non-conflicted target.
- wake15 2026-06-06T13:35Z: fix w01lpkz0o found+fixed a REAL ADD/SUB asymmetry but in the DIJKSTRA
  SUBUTXO path -> INERT for ep57 (Babbage/Alonzo, pre-Dijkstra). 2nd inert-but-valid fix (latent #7).
  KEY PROCESS INSIGHT: worktree muscles CANNOT run the replay harness, so they cannot pinpoint the exact
  ep57 tx; the ORCHESTRATOR must. Added STAKE_CLAMP/SKIP logging to common.rs Phase-2 (uncommitted),
  building node+epoch-state-debug; next wake replays to ep57 and greps the 2 creds to find the offender.
- wake16 2026-06-06T13:44Z: ran clamp-instrumented replay -> ZERO STAKE_CLAMP/SKIP events (any cred) through
  ep60. The Phase-2 subtract is perfectly balanced on the clean replay -> clamp/missing-ADD hypothesis
  REFUTED BY MEASUREMENT. Critical gap exposed: the engine never directly measured the per-cred stake_map
  value from its own replay (summary dump lacks it). Added per-cred STAKE_TRACE log in epoch.rs snapshot;
  rebuilding; next wake reads the actual ep57 value -> settles whether a clean immutable replay even
  reproduces the -5 ADA (mounting evidence — 0/931 diff, 0 clamps — suggests it may NOT, i.e. the bug is
  fork/original-sync-induced like the apply_utxo_diff path after all).
- wake17 2026-06-06T13:52Z: per-cred tracing.warn produced ZERO output (INFO=308, WARN=0) — node logging
  layers drop custom-target warns regardless of RUST_LOG. strings confirmed instrumentation IS in the
  binary, so this is a LOG-FILTER artifact -> wake16 '0 clamps' is UNCONFIRMED (logs were suppressed,
  not absent). Switched all instrumentation to eprintln (bypasses tracing, env-gated DUGITE_STAKE_TRACE);
  rebuilding. Next wake: replay with the env set, finally read the per-cred ep57 value.
- wake18 2026-06-06T13:58Z: eprintln instrumentation CONFIRMED in binary + env set, but ZERO output ->
  I instrumented the WRONG stake loop (epoch.rs has several; dump pool value comes from the snapshot
  delegation fold, not self.certs.delegations at ~205). After 8 wakes (11-18) of measurement plumbing
  with no ledger-logic progress, PARKING #1 per engineering judgment (don't tunnel). Banked: 2 valid
  latent fixes (#6/#7) + many refuted hypotheses. Reverted ad-hoc instrumentation; next wake ROTATES
  to a fresh backlog item (#11 mainnet stake-dereg or #3 ep213). #1 resumes later with PROPER committed
  per-cred dump instrumentation, not ad-hoc eprintlns.
- wake19 2026-06-06T14:00Z: SCHEDULE — instead of rotating into #11/#3 (same per-cred-divergence class
  -> same measurement wall that stalled #1), chose the dependency-aware move: build SHARED per-cred dump
  infra (reliable JSON, no log-filter/loop-guessing). Fired fix muscle whzzl2vls to add per-credential
  stake (utxo_stake/reward_balance/total/pool) to the epoch-state-debug dump gated on DUGITE_TRACE_CREDS.
  Once verified+committed, ALL ledger byte-exactness items become measurable from a replay's JSON. This
  is the engine investing in tooling to unblock a whole frontier rather than tunneling.
- wake21 2026-06-06T14:11Z: infra muscle whzzl2vls delivered per-credential dump (PerCredentialSummary +
  CredentialEntry joining GO-snapshot stake_distribution + delegations + rupd.rewards, env-filterable via
  DUGITE_EPOCH_STATE_DUMP_CRED_FILTER, top-200 + pinned-filter, 5 unit tests, fields aligned to cardano-cli
  debug log-epoch-state). Tier B, checks_green, 1 file (epoch_state_debug.rs, feature-gated, no ledger
  semantics). Applied to main (uncommitted); building. Next wake replays + reads per-cred JSON to settle
  ep57 and arm the same tooling for #3 ep213 / #11.
- wake22 2026-06-06T14:14Z: *** BREAKTHROUGH *** per-cred dump infra VERIFIED + COMMITTED (62db548471).
  Direct measurement: clean HEAD from-genesis replay computes ep57 per-cred stake BYTE-EXACT vs Koios
  (630472f7=9957549164, 7d3e2b31=9815680998; appear in ep58-dump GO snapshot due to mark/set/go lag).
  The -5 ADA is NOT reproduced on clean HEAD -> prior REWARD-DIVERGENCE-FINDINGS.md is STALE (old code or
  fork-affected db). 8 wakes of 'stalled measurement' actually produced the answer: dugite is already
  correct on clean replay. Left the replay climbing to ep181 to test the original halt (decides
  fork-induced-bug-real vs fully-resolved). Lesson banked: stale findings docs misled the gauntlet.
- wake23 2026-06-06T14:22Z: *** ITEM #1 RESOLVED *** clean HEAD replay reached ep192 having CROSSED
  ep181 with ZERO WithdrawalAmountMismatch (the original halt). With ep57 per-cred byte-exact (wake22),
  the ep57/ep181 divergence is RESOLVED on HEAD; prior findings doc confirmed STALE. ledger.preprod +
  sync.preprod frontiers UNBLOCKED. Residual: land #6 for fork-robustness (the fork-induced variant).
  Next wake: let replay finish to ep293 (lock frontier), then rotate to #3 ep213 / #11 — NOW ARMED with
  the per-cred dump tool that made this resolution possible.
- wake24 2026-06-06T14:27Z: BROADENED preprod validation (lock-free, using produced dumps + koios.sh):
  dugite total active_stake BYTE-EXACT vs Koios at ep100/150/200/230 (253512539651088 / 327288105772146 /
  381515925060861 / 380718162816350) spanning Shelley→Alonzo→Babbage→Conway. Clean replay ep0-233 has
  ZERO WithdrawalAmountMismatch/panic/ValidationTagMismatch/chain_diverged. The preprod ledger frontier is
  solidly byte-exact, not just ep57. Replay finishing to ep293; next wake rotates to mainnet (#3 ep213 /
  #11) armed with the per-cred dump.
- wake25 2026-06-06T14:33Z: SCHEDULE prep (lock-free; preprod replay at ep267 still holds heavy-op lock).
  db-mainnet usable (ep331 + ledger snapshots; Koios mainnet tip ep635). Mainnet ep213 finding is RECENT
  (2026-06-04) so unlike ep57 it's likely REAL -> warrants a mainnet replay. Extracted the 4 target creds
  for the per-cred filter. Next wake: when preprod replay (->ep293) frees the lock, START the mainnet
  investigation (either a from-genesis replay, or the doc's faster bisect path if it has one) OR pick
  phase2 #22 (doable on the already-present db-preprod-sync Babbage Plutus) if mainnet replay is too long.
- wake26 2026-06-06T14:39Z: ROTATE to mainnet #3/#11 (ep213 reserves divergence, recent+real). Doc method:
  from-genesis REPLAY is fast (~min, not hours; replay!=full-validation-sync). SIGTERMed preprod replay
  (frontier already established ep0-233 + spot-checks), cloned db-mainnet (CoW), wiped to immutable,
  launched mainnet from-genesis replay with the 4 stake-dereg creds filtered + epoch-state-debug. Byron
  climbing. Next wake: poll to ep214, diff reserves/treasury vs Koios to reproduce the +180.4B, then
  diagnose with the haskell-ledger-cross-validation skill. WATCH RAM (1GB free at launch).
- wake27 2026-06-06T14:42Z: *** mainnet ep213 RESOLVED on HEAD *** dugite reserves+treasury BYTE-EXACT vs
  Koios at ep212/213/214/220/221 (diff=0). The doc's +180.4B ep213 reserves divergence is GONE (fix landed
  2026-06-04..06). 4 stake-dereg creds absent from dump = deregistered; reserves-exact => their reward
  attribution is correct. KEY STRATEGIC FINDING: BOTH ep57 (preprod) and ep213 (mainnet) backlog items
  were STALE (already fixed on HEAD). The backlog is seeded from old findings docs describing fixed bugs.
  PIVOT: stop chasing stale per-finding items; do BROAD validation (sweep reserves/treasury/per-cred across
  many epochs both nets vs Koios) to find any REAL remaining divergence. Mainnet replay continues to validate
  more history. This is great production-readiness news: dugite is more correct than the docs suggested.
- wake28 2026-06-06T14:47Z: BROAD SWEEP found a REAL current divergence: mainnet reserves byte-exact
  ep209-245 then DIVERGES at ep246 (+82,270,482 reserves, -55,269 treasury), bisected (ep245 exact). This
  is the first genuine non-stale bug the engine has found -> the broad-validation pivot works. ep246=Allegra
  PV3. dugite under-removed ~82.27M from reserves at the 245->246 reward transition. Fired analyze muscle to
  diagnose via haskell-ledger-cross-validation (reward-pot / reserves-expansion / undistributed-return / MIR).
- wake29 2026-06-06T14:52Z: characterized the ep246 divergence trajectory (lock-free while analyze runs):
  reserves_diff ep246=+82.27M, ep250=+81.6M, ep255=+81.8M, ep260=+81.0M, ep265=+80.2M, ep270=+79.4M,
  ep275=+88.1M. So it is a DISCRETE one-time ~82.27M reserves error at the ep245->246 boundary (slowly
  self-amortizes as the extra reserves generate slightly more expansion), NOT a per-epoch systematic drift,
  PLUS a second discrete event near ep271-275 (+~9M). Narrows diagnosis to a specific reward/MIR/pot event
  at the ep246 Allegra boundary. analyze muscle w93yngoj8 still running; mainnet replay at ep275.
- wake30 2026-06-06T14:?? : analyze w93yngoj8 done — DIAGNOSIS: ep246 reserves+82.27M = deltaR1 too small in
  the reward update (startStep: deltaR1=floor(min(1,eta)*rho*reserves); rho/tau/d ALL from prevPParams in
  Haskell). Sign pattern (reserves high + treasury low) matches a small deltaR1 (rPot=ssFee+deltaR1 ->
  deltaT1=floor(tau*rPot) also shrinks). Engine pinpointing: rewards.rs:177 pp=params sources rho/tau; d
  uses separate prev_d (#629). RULED OUT rho/tau (mainnet ep245==ep246: rho 0.003, tau 0.2). CONFIRMED d
  changed 0.24->0.22 at ep246. So bug is in deltaR1's d-dependent eta/expected_blocks OR blocksMade source
  OR eta Rational arithmetic. Fired fix muscle to read the call site (which epoch's d/blocks passed),
  compute Haskell-correct eta from Koios ep244 d + ep245 pool_blocks, pinpoint the exact wrong input, fix
  (Tier A), and ADD RUPD-component dump (eta/expected_blocks/actual_blocks/deltaR1) for byte-exact verify.
- wake33 2026-06-06T15:?? : fix muscle whr4t971m SELF-REFUTED my deltaR1/d diagnosis (made NO code change,
  cited the #438 trap). Correct cause: ep246 +82.27M reserves = per-pool/per-member reward UNDER-distribution
  (undistributed pot returns to reserves; treasury near-neutral, NOT tau*X). This is the ep213-doc's OPEN
  residual class (per-member prefilter). NEXT: localize WHICH pool(s)/member(s) dugite under-distributes at
  ep246 by comparing the dump's rewards.per_pool_top20 + per_credential rewards vs Koios pool_history /
  account_reward_history (mind the mark/set/go reward-timing lag), then fix the prefilter/distribution.
  GREAT discipline: the muscle refused a speculative fix that would have regressed byte-exact ep209-245.
- wake34 2026-06-06T15:13Z: ep213 doc confirms the class precisely: dugite UNDER-distributes MEMBER rewards
  (treasury-neutral, undistributed->reserves). Drop site = member loop rewards.rs:427-476; the pv<=6
  registration prefilter rewards.rs:461 (if !registered_at_startstep(cred) -> skip member reward), mirroring
  Haskell rewardOnePoolMember.prefilter (hk in addrsRew). The +82M = dugite's frozen reward-account set
  (startstep_addrs_rew capture, rewards.rs:150-160) drops members Haskell pays — likely a capture-timing or
  de/re-registration edge. The 4 'creds' I filtered ARE the 4 top pools (af22f959/d9812f8d/53215c47/6184f6e7).
  Fired localize+fix muscle with strict pinpoint-first orders (find the exact dropped members via Koios
  account_reward_history before any fix).
- wake37 2026-06-06T15:?? : 2nd muscle (w20c0k2qr) again refused to patch (no change), byte-exact-refuted the
  prefilter-LOCATION hypothesis. Real bug: dugite's reward_accounts is MISSING a cred Haskell has at ep245
  startStep (treasury -55,269 = frTotalUnregistered for it). Needs instrumented replay (orchestrator-only).
  SIGTERMed the mainnet validation replay (job done: ep246 bisected, ep0-318 swept), added DROP_TRACE eprintln
  to the prefilter (rewards.rs:461), building. Next: replay db-mainnet to ep246 with DUGITE_DROP_TRACE=1,
  grep the dropped creds near ep245, cross-ref Koios to find the one(s) Koios pays -> trace why reward_accounts
  misses it -> byte-exact fix. NOTE: dugite SNAP(epoch.rs:333) runs BEFORE apply_pending_mir(479) vs Haskell
  applyRUpd->MIR->SNAP — flagged as a separate class to check.
- wake38 2026-06-06T15:?? : DROP_TRACE instrumentation WORKS — captures DROP_PREFILTER cred=<typedhash32> pool=<id>.
  Replay climbing (ep219); 43 drops for the 4 target pools so far (mostly pool 53215c47). NEXT (when replay
  passes ep246): isolate the ep245->246 RUPD drop burst (correlate with epoch markers in the log), and for
  each dropped cred cross-ref Koios pool_delegators_history/account_reward_history at ep244 -> the dropped
  cred(s) Koios PAYS (summing ~82.27M) are the bug = creds in Haskell accounts but missing from dugite
  reward_accounts at ep245 startStep. Then trace the registration-tracking gap (reg/dereg/re-reg or MIR order)
  -> byte-exact fix. Replay job: .jobs/replay-droptrace.{pid,log}.
- wake39 2026-06-06T15:?? : PINPOINT via DROP_TRACE replay: the 3 creds dugite prefilter-drops at the ep245
  RUPD for the 4 pools (36e9eb66/85545bae/cb303645) ALL deregistered at ep243/244 per Koios account_updates
  (registered ep211-218, deregistered ep243-244, zero reward) -> CORRECTLY dropped, NOT the bug. So dugite's
  reward_accounts is NOT missing a still-registered cred via the prefilter -> the +82.27M is NOT a prefilter
  drop (rules out the muscle's missing-cred hypothesis too, by measurement). PIVOT candidates: (a) member_stake==0
  skip rewards.rs:472 (member_stake from go.stake_distribution wrong -> member dropped); (b) leader/operator
  reward (rewards.rs:427-437) under-paid; (c) the SNAP-before-MIR ordering (epoch.rs:333 vs 479) the muscle
  flagged. NEXT: instrument the would-be-reward of EVERY dropped/zeroed member (Σ should ≈82.27M to confirm
  the path) OR compare per-pool leader+member totals dugite-vs-Koios pool_history at ep244 to find the pool
  whose total is short. All ep245-window drops saved /tmp/all_drops245.txt.
- wake40 2026-06-06T15:?? : per-pool comparison is reward-TIMING-tangled (dugite per_pool_top20 ep246 d9812f8d
  =2000768023438 vs Koios pool_history ep244 member=34060783216, ~60x off -> wrong epoch/semantic mapping).
  Bug resists shallow localization (prefilter ruled out by measurement; 619 drops; timing tangled). Firing
  ONE well-resourced deep-dive muscle with the per-cred dumps (epoch-dumps-engine/mainnet-droptrace/ has
  per_credential WITH reward per cred) + all ruled-out findings. If it can't crack it -> PARK ep246 as a
  thoroughly-characterized REAL open bug and BROADEN validation (later mainnet eras ep300+/Conway, phase2,
  sync, perf) to maximize bug-discovery coverage (higher production-readiness value than one 82-ADA bug).
- wake43 2026-06-06T15:?? : deep-dive wuc2kqb1z ROOT-CAUSED ep246 (structural: two-map keying in the member-
  reward fold; Haskell uses one resolved active-stake VMap with swdStake=UTxO<>reward_balance). Ruled out 5
  hypotheses byte-exact. This is the engine's deepest real find: a genuine byte-exactness bug root-caused to a
  precise mechanism + Haskell source, even if the structural fix is a discrete careful task. PARKED with full
  root cause; reverted DROP_TRACE diagnostic from main. BROADENING: next wakes validate other frontiers (later
  mainnet eras ep300+/Babbage/Conway, phase2 #22 on db-preprod-sync, sync soak) to maximize byte-exact coverage
  and check whether the two-map keying manifests elsewhere. NET so far: preprod byte-exact all eras; mainnet
  byte-exact ep209-245 + ep247-318 (the one ep246 divergence root-caused).
- wake44 2026-06-07: removed the ep246 DROP_TRACE diagnostic (cleanup, committed 4fd6ee4a2e; main clean).
  BROADENED to the phase-2 readiness gate (#22, untouched so far): launched a preprod from-genesis replay
  (Babbage/Conway Plutus-dense) with DUGITE_PHASE2_DUMP_DIR. PRIMARY signal = a clean replay with NO
  ValidationTagMismatch proves dugite's phase-2 is_valid is byte-exact with on-chain (no Plutus eval
  disagreement halts the chain). At ep55 so far, clean. The dumps capture finer ExBudget divergences (the
  #22 residual class). Next wake: poll the replay to Conway tip; if no ValidationTagMismatch -> phase-2
  is_valid byte-exact frontier locked; then bucket any ExBudget dumps via phase2_repro.
- wake46 2026-06-07: PHASE-2 FRONTIER LOCKED (preprod). Full from-genesis preprod replay (4,789,676 blocks,
  ep0-293) with DUGITE_PHASE2_DUMP_DIR: ZERO ValidationTagMismatch, ZERO phase-2 divergence dumps. Confirmed
  the dump fires ONLY on a phase-2 is_valid divergence (plutus.rs:260 maybe_dump_phase2_divergence), so 0
  dumps = 0 divergences. dugite evaluates every on-chain Plutus V1/V2/V3 script (Alonzo/Babbage/Conway) to
  the same validity verdict as the chain. #22 RESOLVED on HEAD (was 628 stale divergences). THIRD readiness
  gate in strong shape: ledger (preprod all-era + mainnet ep209-318 minus root-caused ep246) ✓, phase-2 ✓.
  Remaining: ep246 structural fix (parked, root-caused), sync/perf frontiers, mainnet-phase-2 V3 (inert til ep507).
- wake47 2026-06-07: BROADENED to the SYNC gate (4th frontier). Launched a LIVE preprod node (clone of
  db-preprod-sync, fast-start from ep293 native snapshot, NOT wiped) connecting to preprod peers to sync
  ~620 blocks to the live tip (ep293 b4792907) then SOAK at tip. Validates live-network sync + at-tip
  stability (no stall/wedge/chain_diverged, ledger_tip==immutable_tip). job .jobs/sync-soak.{pid,log}.
  CAUTION free_ram=1GB at launch (live node ~7GB) — watch for OOM. Next wakes: confirm it reaches tip and
  soaks clean for a sustained window. This rounds out the 4th readiness gate (correctness gates ledger+phase2
  already byte-exact).
- wake48 2026-06-07: SYNC-gate live-node test FOUND a real perf gap (#9): the node discards its ep293 native
  snapshot on `run` due to a UTxO-backend mismatch (snapshot saved as dugite-mem, run configured dugite-lsm)
  -> "snapshot backend does not match" -> FULL genesis replay instead of fast-start. Real robustness/perf
  issue (a mithril-import->run cycle always full-replays). The replay itself is clean (no stall/wedge/OOM,
  ~88% at slot 109M). Once it completes it syncs the ~620-block gap to live tip + soaks. Next wake: confirm
  tip + clean soak. The engine's broadening keeps finding REAL issues (ledger ep246, now sync/perf #9).
- wake49 2026-06-07: SYNC-gate live test, deeper findings: after the #9 full replay, the node does a 2nd
  replay of 2612 VOLATILE blocks emitting many "Block does not connect to tip" WARNs (recheck if benign or a
  volatile-set handling issue), then was I/O-wait (state UN) at 1GB free RAM (swapping) -> startup very slow,
  no N2C socket reached in the window. The from-genesis REPLAY path is proven clean (this + all prior replays:
  no stall/wedge/chain_diverged/OOM). The LIVE-NETWORK-SYNC + at-tip SOAK portion is not cleanly testable
  under 1GB free RAM -> SIGTERMed the node; flag for a clean-RAM re-run (or after the #9 backend fix lets it
  fast-start). Sync gate = replay-clean ✓ + live-soak DEFERRED (RAM-bound). Added watch: volatile connect-WARNs.
- wake50 2026-06-07: SCHEDULE picked #9 (tractable real perf fix; ep246 parked as hard/structural). PINPOINT:
  the haskell-import path saves the native snapshot via state.save_snapshot (node/mod.rs:6421) WITHOUT the
  configured utxo-backend -> tagged in-memory -> run --utxo-backend lsm rejects it (mod.rs:660) -> FULL
  genesis replay. Fix: persist+tag the snapshot in the CONFIGURED backend during mithril/haskell import so a
  mithril-import->run cycle fast-starts. Fired fix muscle. Verification = operational: after fix, run a
  mithril-imported db and confirm it loads the snapshot (no 'backend does not match' / no full replay).
- wake53 2026-06-07: #9 fix done (wrpfacs13, Tier B, checks_green, canonically grounded vs ouroboros-consensus
  LSM/InMemory backend guards + snapshot-converter). INSIGHT: the .bin payload is backend-AGNOSTIC (only inline
  vs empty utxo_set); a DugiteMem snapshot under an LSM node is CONVERTIBLE — attach_utxo_store already migrates
  inline UTxOs. Fix adds BackendCheckResult::Convertible (snapshot.rs) -> load_snapshot_with_backend_guard
  accepts+migrates instead of reject->replay (node/mod.rs); self-healing (next save re-tags lsm). 2 files/2
  crates, no ledger/byte-exact change. Applied to main (uncommitted), compiles clean, feature-build running.
  NEXT: operational verify — run a fresh clone of db-preprod-sync with the fixed binary, confirm it loads the
  mem snapshot (no 'backend does not match', NO full genesis replay) -> then COMMIT (first landed real fix).
- wake54 2026-06-07: *** #9 FIXED + OPERATIONALLY VERIFIED + COMMITTED (first landed real fix) ***. Ran the
  fixed binary on a fresh db-preprod-sync clone: log shows "Loaded in-memory snapshot under the LSM backend ...
  no from-genesis replay" (utxo_count=4116338), then live-synced 3319 Conway blocks. 0 panic/ERROR/backend-
  mismatch/full-replay/volatile-WARN. fmt+clippy clean. Committed (2 files, Tier B, no byte-exact change).
  mithril-imported nodes now fast-start instead of full-replaying. Remaining real work: #0 ep246 (hard
  structural, root-caused) + the RAM-clean live soak. ENGINE SCORECARD: ledger byte-exact (preprod all-era +
  mainnet ex-ep246), phase-2 byte-exact, sync replay-clean + #9 fixed; 2 real bugs found, 1 fixed, 1 root-caused.
- wake55 2026-06-07: housekeeping + sync-soak retry. GC'd 4 old preprod db-clones (kept 2 newest). free_ram
  recovered to 5GB (verify node exited). Launched a LIVE preprod soak with the #9-FIXED binary (fast-starts via
  Convertible snapshot load). Monitoring: reach tip + sustained at-tip soak (no stall/wedge/chain_diverged,
  ledger_tip==immutable_tip) -> would lock the sync gate's live-soak portion. job .jobs/live-soak.{pid,log}.
- wake109 2026-06-07: POLL #10 multi-asset fix-muscle w34va8uxf — RUNNING, healthy (4GB RAM, no nodes). *** META
  DISCOVERY ***: db/ledger/<slot>/meta contains {"backend":"utxohd-mem","checksum":...,"tablesCodecVersion":1} —
  the AUTHORITATIVE endianness disambiguator (tablesCodecVersion, the gauntlet's prescribed signal) IS accessible
  next to the tables blob. The robust fix used empirical auto-detect (worked, verified); reading tablesCodecVersion
  from meta is a cleaner principled upgrade -> note as #10 hardening follow-up (auto-detect + safety-net already
  correct, so not blocking). Muscle currently diagnosing multi-asset + reconciling gated-test state. Not disturbed.
  #10 stays FIXING; next: poll -> re-import re-verify (MultiAssetNotConserved -> baseline) -> re-gauntlet -> commit.
- wake108 2026-06-07: POLL #10 multi-asset fix-muscle w34va8uxf — RUNNING, healthy (4GB RAM, no nodes, 0
  completed). Building a diag_scan example to inspect the real blob + locate the reconstruction bug (diagnose-
  first, good). Not disturbed; no competing work. #10 stays FIXING; next: poll -> build+nextest -> re-import
  re-verify (MultiAssetNotConserved -> baseline) -> re-gauntlet -> commit.
- wake107 2026-06-07: POLL #10 multi-asset fix-muscle w34va8uxf — RUNNING, healthy (5GB RAM, no nodes, 0
  completed). Diagnosing: locating the failing UTxO to compare reconstructed multi_asset vs Koios. Not disturbed;
  no competing work. #10 stays FIXING; next: poll -> build+nextest -> re-import re-verify (MultiAssetNotConserved
  -> baseline) -> re-gauntlet -> commit.
- wake106 2026-06-07: #10 VERIFYING — endianness CORE verified, multi-asset REGRESSION found (thorough check).
  robust-fix re-soak: TxIx auto-detect=Big correct (script-not-found 0, budget 0, safety-net sane), but a full
  rejection-class scan found MultiAssetNotConserved 32->316 (all input_side:0). The key fix RESOLVES idx>=1 inputs
  (was 600 InputNotFound) which EXPOSED that the multi-asset reconstruction stores empty assets on real UTxOs
  (10-NFT unit oracle passes, real data fails — same decode-vs-real pattern as the whole #10 saga). Did NOT
  commit. SIGTERM'd verify10e (kept db), GC'd verify10d, reset main clean. Launched diagnose+fix muscle w34va8uxf
  for the multi-asset reconstruction (real-blob + Koios byte-exact oracle). #10 VERIFYING-RESOAK -> FIXING.
  LESSON: always scan ALL rejection classes in the verdict, not just the expected ones — a thorough grep caught a
  regression a narrow check would have shipped.
- wake105 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. Robust-fix build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10e, ran robust binary (pid 47327): AUTO-DETECT confirmed on real blob ("txix_endianness
  =Big") + safety-net distribution sane (low 3131782 vs mult256 62, no trip). utxo_count=4116338 skipped=0. Node
  syncing. Deferred verdict grep (one-step). next: grep verify10e-resoak.log -> must match 549->277 -> RE-GAUNTLET
  -> commit. 3GB RAM, 84GB disk.
- wake104 2026-06-07: POLL #10 robust-fix build verify-build-10e (pid 46612) — STILL RUNNING (final crate
  dugite-node compiling/linking). Not done; can't re-verify yet. No competing work during link. #10 stays
  VERIFYING-BUILDING; next: BUILD_EXIT=0 -> clone db-preprod-sync -> re-import re-soak (auto-detect BE, keep
  549->277) -> re-gauntlet -> commit.
- wake103 2026-06-07 (notification-triggered): #10 ROBUST endianness fix COMPLETE (muscle w1m4bxztw, Tier A',
  checks_green, 2 crates). Version/layout-INDEPENDENT: empirical auto-detect (detect_txix_endianness samples 2000
  keys, picks sane index distribution) + hard safety net (assert_txix_distribution_sane errors loud on mis-key) +
  multiasset+refscript+datum. Oracles: legacy fixture->LE/txix==1, synth BE->txix==1, mis-key trips net,
  both-endianness pinned. Saved ROBUST patch (2724 lines, applies clean), applied to main, launched build pid
  46612. Advanced #10 FIXING -> VERIFYING-BUILDING. next: BUILD_EXIT=0 -> fresh import re-soak (keep 549->277) ->
  RE-GAUNTLET (should find no uncovered combo — auto-detect reads data) -> commit. Did NOT commit.
- wake102 2026-06-07: POLL #10 robust fix-muscle w1m4bxztw — RUNNING, healthy (4GB RAM, no nodes, 0 completed).
  In build/clippy-fixup (observe_txix auto-detect helper; .is_multiple_of lint) -> approaching build+nextest. Not
  disturbed; no competing work. #10 stays FIXING; next: poll -> re-import re-verify -> re-gauntlet -> commit.
- wake101 2026-06-07: POLL #10 robust fix-muscle w1m4bxztw — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Implemented EMPIRICAL AUTO-DETECT: TvarIterator::new auto-detects endianness from data (new_with_endianness for
  explicit tests); now adding gated key-correctness + safety-net tests. Version/layout-independent = gauntlet-proof
  by construction. Not disturbed. #10 stays FIXING; next: poll -> build+nextest -> re-import re-verify -> re-gauntlet -> commit.
- wake100 2026-06-07: POLL #10 robust fix-muscle w1m4bxztw — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Mid-implementation: porting multi-asset num_assets into the tag decoders (txout.rs). Not disturbed; no competing
  work. #10 stays FIXING; next: poll -> build+nextest -> re-import re-verify -> re-gauntlet -> commit.
- wake99 2026-06-07: POLL #10 robust fix-muscle w1m4bxztw — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Applied COMPLETE base; porting num_assets/parse_multi_asset_rep from CONDITIONAL + implementing robust
  (codec-version/empirical-auto-detect) endianness + safety net. Not disturbed; no competing work. #10 stays
  FIXING; next: poll -> build+nextest -> re-import re-verify (both fixtures + keep 549->277) -> re-gauntlet -> commit.
- wake98 2026-06-07 (notification-triggered): *** RE-GAUNTLET REFUTED 2/3 — gauntlet's SECOND correct catch ***.
  wj0pzgzaq: 2 refuters (edge-epoch+compounding-feedback) proved layout!=endianness via upstream history (flat-
  tables and BE-flip in different oc releases months apart; intermediate flat-LE & nested-BE snapshots shipped).
  My conditional layout-proxy would mis-key those. 3rd refuter (haskell-semantics, refuted=false) corroborated the
  timeline (called it comment-only) but the 2 are right: intermediate combos exist. DID NOT COMMIT; reset main
  clean. Relaunched ROBUST fix w1m4bxztw: endianness via codec-version-if-available else empirical auto-detect
  (index distribution) + hard mis-key safety-net (error-loud, never silent). #10 GAUNTLET-PENDING -> FIXING.
  LESSON: serialization-format assumptions need upstream-history verification; the gauntlet's adversarial panel
  caught a subtle version-vs-layout conflation twice. #10 is deep (28 wakes) but every gauntlet catch was a REAL
  correctness bug avoided. Empirical auto-detect makes the fix version-independent -> gauntlet-proof by construction.
- wake97 2026-06-07: POLL #10 re-gauntlet wj0pzgzaq — RUNNING (3 refuters active, 0 completed; 5GB RAM, no
  nodes). Adversarially probing the CONDITIONAL fix (layout=>endianness mapping completeness/reliability,
  default-Big-on-legacy, carry-over integrity). Not disturbed; no competing work. #10 stays GAUNTLET-PENDING;
  next: poll -> pass -> COMMIT the conditional patch via gh/HTTPS.
- wake96 2026-06-07: #10 VERIFYING PASS for the CONDITIONAL fix. verify10d re-soak (new format=Big) matches the
  unconditional run: not-found 0, budget 0, MissingScriptWitness 0, ~279 Error-term (=#15), 4/5 target slots
  clean -> conditional fix is chain-equivalent for new format AND fixes the legacy regression (LE unit-proven).
  SIGTERM'd verify10d (kept its db for #15), GC'd verify10c. Launched RE-GAUNTLET wj0pzgzaq on the conditional
  fix. #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. next: poll -> on pass COMMIT the conditional patch (TxIxEndianness
  + multiasset + refscript + datum, 2 crates) via gh/HTTPS -> then #16 follow-up + #15 diagnosis.
- wake95 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. Conditional-fix build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10d, ran conditional binary (pid 2797): import log confirms "txix_endianness=Big" for
  the flat-tables new format (legacy/LE unit-proven). utxo_count=4116338 skipped=0. Node syncing. Deferred verdict
  grep (one-step). next: grep verify10d-resoak.log -> must match 549->277 -> RE-GAUNTLET -> commit. 3GB RAM,
  94GB disk (GC clones soon).
- wake94 2026-06-07 (notification-triggered): #10 CONDITIONAL endianness fix COMPLETE (muscle wauynb0ku, Tier A',
  checks_green, 2 crates). enum TxIxEndianness{Little,Big} branched at resolve_inmemory_tables_path (flat tables->
  Big, nested tvar->Little); decode_mempack_txin + TvarIterator take endianness; BOTH pinned tests kept + legacy-
  fixture LE oracle (first entry->txix==1 under LE, 256 under BE). Multi-asset+refscript+datum carried over. Both
  canonical Haskell sources quoted. Saved CONDITIONAL patch (2395 lines, applies clean), applied to main, launched
  build pid 2336. Advanced #10 FIXING -> VERIFYING-BUILDING. next: BUILD_EXIT=0 -> fresh import re-soak (keep
  549->277) -> RE-GAUNTLET (must clear the legacy dissent) -> commit. Did NOT commit (chain re-verify + re-gauntlet
  pending).
- wake93 2026-06-07: POLL #10 refinement muscle wauynb0ku — at FINAL gate (clippy clean; running nextest
  --workspace). Imminent completion. Not disturbed. Re-verify prep stands (db-preprod-sync import source intact +
  legacy fixture committed). #10 stays FIXING. next (notif/poll): read FIX -> fresh re-import re-soak (both formats:
  legacy LE + new BE, keep 549->277) -> re-gauntlet -> commit FULL conditional fix.
- wake92 2026-06-07: POLL #10 refinement muscle wauynb0ku — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Introduced a TxIxEndianness param, threaded through decode_mempack_txin + TvarIterator::new; now fixing the
  remaining call sites + tests for the new signature. Not disturbed; no competing work. #10 stays FIXING; next:
  poll -> build+nextest -> re-import re-verify (both formats correct, keep 549->277) -> re-gauntlet -> commit.
- wake91 2026-06-07: POLL #10 refinement muscle wauynb0ku — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Confirmed BOTH canonical sources (legacy/LE = generic MemPack Word16 host-native; new/BE = BigEndianTxIx
  byteSwap16 — matches my wake89 byte-by-byte finding) and is editing mempack/mod.rs to implement the
  format-conditional endianness. Not disturbed; no competing work. #10 stays FIXING; next: poll -> re-import re-verify.
- wake90 2026-06-07: POLL #10 refinement muscle wauynb0ku — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Applied the COMPLETE base (refscript+datum) in its worktree; now reworking the endianness as format-conditional
  (flat tables->BE, nested tvar->LE) + multi-asset delta. Not disturbed; no competing work. #10 stays FIXING;
  next: poll wauynb0ku -> re-import re-verify (both formats correct, keep 549->277) -> re-gauntlet -> commit.
- wake89 2026-06-07 (notification-triggered): *** GAUNTLET CAUGHT A REAL REGRESSION (passed 2-1 but I overrode
  the majority — verified the dissent) ***. wqwgen1p0: haskell-semantics + edge-epoch refuted=FALSE (found
  upstream BigEndianTxIn confirming BE for NEW format); compounding-feedback refuted=TRUE and CORRECT: TxIx
  endianness is snapshot-VERSION-dependent. I verified byte-by-byte: legacy tvar fixture index1=`01 00`=LE, new
  tables index1=`00 01`=BE, identical envelopes, import branches by file path. Unconditional BE would mirror-image-
  regress legacy/preview imports (01 00->256). DID NOT COMMIT. Reset main clean (FULL unconditional-BE patch NOT
  committed). Launched refinement fix-muscle wauynb0ku (format-conditional LE/BE branched at the import call site,
  BOTH LE+BE pinned tests). Filed latent #16 (era-relative plutus tag, byte-exact today). #10 GAUNTLET-PENDING ->
  FIXING. LESSON: a 2-1 gauntlet pass with a concrete empirical dissent is NOT an auto-commit — verify the dissent.
- wake88 2026-06-07: POLL #10 gauntlet wqwgen1p0 — 1/3 refuters DONE (edge-epoch: refuted=FALSE, DECISIVE
  validation). Found the canonical upstream source confirming the BE fix WORD-FOR-WORD: on-disk tables key is
  consensus-layer BigEndianTxIn, MemPack `packM (BigEndianTxIx (TxIx w)) = packM (byteSwap16 w)`
  (ouroboros-consensus Shelley/Ledger/Ledger.hs); #461 reconciliation matches upstream rationale verbatim.
  Cleared all angles: decode_mempack_txin has exactly ONE caller (TvarIterator, BE keys only); module is
  DECODE-ONLY (no encoder -> re-save concern moot); multi-asset rep uses host-LE cells (distinct from BE key)
  with correct name-length/dedup/empty/last edge cases; decode-failures degrade safely w/ warnings. LATENT
  FOLLOW-UP (not a current bug, didn't refute): decode_imported_script_ref hard-codes 0->V1,1->V2,2->V3,3->V4 as
  'global' but the MemPack PlutusScript tag is ERA-RELATIVE; byte-exact today only via the strict-prefix language
  property; patch comments self-contradict ('era-relative' vs 'global') -> file as #16 follow-up after commit.
  2/3 refuters still running -> aggregated verdict not final -> CANNOT commit yet. #10 stays GAUNTLET-PENDING.
- wake87 2026-06-07: POLL #10 gauntlet wqwgen1p0 — RUNNING (3 parallel refuters active, 0 completed; 5GB RAM,
  no nodes). Adversarially refuting the FULL fix (BE-key correctness across all callers, multi-asset rep edge
  cases, #461 LE-context, re-save roundtrip). Not disturbed; no competing work. #10 stays GAUNTLET-PENDING; next:
  poll -> pass (refute<2/3) -> COMMIT the FULL patch via gh/HTTPS; refuted -> address it.
- wake86 2026-06-07: *** #10 VERIFYING = MAJOR chain-level SUCCESS *** full-fix re-soak (verify10c) dropped
  phase-2 divergences 549->277 with the KEY-RESOLUTION classes ELIMINATED ("not found" 11->0, "budget" 41->0,
  MissingScriptWitness 0; 4/5 target slots CLEAN). NO regression. The BE-key fix (the diagnosed root cause) made
  the refscript+datum data finally resolve at runtime — this time NOT a no-op (contrast wake75). Launched GAUNTLET
  wqwgen1p0 (refuterN=3) on the FULL fix toward commit. Filed the residual 277 "Error term" (277 distinct txs,
  full-replay byte-exact so import-incompleteness; leading suspect compact-address decode tags 2/3) as new item
  #15 (non-chain-critical). SIGTERM'd verify10c (kept its db for #15 diagnosis). #10 VERIFYING-RESOAK ->
  GAUNTLET-PENDING. next: poll gauntlet -> on pass COMMIT the FULL patch (TxIx endianness + multiasset +
  refscript + datum, 2 crates) via gh/HTTPS. ENGINE WORKED: byte-exact discipline forced the deep diagnosis that
  found the real one-line endianness root cause two "correct" decode fixes had masked.
- wake85 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. Full-fix build BUILD_EXIT=0 (1m39s). Cloned
  db-preprod-sync -> verify10c, ran FULL-fix binary (pid 56221): clean import (utxo_count=4116338 skipped=0) with
  BE keys + datum + refscript + multi-asset. Node syncing to re-process the failing slots. Held one-step
  discipline (wake66/74 precedent): recorded the re-soak launch, deferred the verdict grep to next wake. 3GB RAM
  free. next wake: grep verify10c-resoak.log -> 291/41/11 must drop to ~0 -> #10 verified -> gauntlet -> commit.
- wake84 2026-06-07 (notification-triggered): #10 ENDIANNESS FIX COMPLETE (muscle wagcpug42, Tier A',
  checks_green, 2 crates, +1453/-166). mod.rs:68 from_le_bytes->from_be_bytes (#461 reconciled: tables KEY is BE
  for sort-order, distinct from host-LE MemPack Word16) + full multi-asset Value reconstruction (Mary
  CompactValue rep port). KEY-CORRECTNESS ORACLE passes (00000c0c#1 -> txix==1 not 256, coin 1750000) — the
  anti-no-op proof the last fix lacked; all 3 real-blob oracles green. Preserved FULL patch (1972 lines, applies
  clean), reset main + applied it, launched build pid 55768. Advanced #10 FIXING -> VERIFYING-BUILDING. next:
  BUILD_EXIT=0 -> fresh import from db-preprod-sync -> re-soak -> 291/41/11 MUST drop to ~0 -> gauntlet -> commit.
  Did NOT commit (chain-level divergence-gone not yet confirmed; unit key-oracle is necessary but the re-soak is
  the cardinal proof).
- wake83 2026-06-07: POLL #10 fix muscle wagcpug42 — at FINAL gate (clippy clean; running nextest --workspace).
  Imminent completion. Not disturbed. Re-verify prep stands (db-preprod-sync import source intact, disk
  headroom). #10 stays FIXING. next (on completion notif/poll): read FIX -> fresh re-import re-soak -> 291/41/11
  must drop to ~0 (chain proof) -> gauntlet -> commit COMPLETE fix (refscript+datum+endianness+multiasset, 2 crates).
- wake82 2026-06-07: POLL #10 fix muscle wagcpug42 — STILL RUNNING, healthy (4GB RAM, no nodes, building). In
  BUILD phase for the multi-asset reconstruction (PolicyId/AssetName imports added to mod.rs, compiling
  serialization+node); endianness flip + tag0/1 multi-asset done, build+nextest+gated-oracle still ahead. Not
  disturbed; no competing work. #10 stays FIXING; next: poll wagcpug42 -> re-import re-verify (291/41/11 -> ~0).
- wake81 2026-06-07: POLL #10 fix muscle wagcpug42 — STILL RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Implementing: extending decode_tag0/tag1 to decode_compact_value_exact so multi_asset is populated
  consistently across all tags (thorough secondary fix alongside the primary mod.rs:68 endianness flip). Not
  disturbed; no competing work. #10 stays FIXING; next: poll wagcpug42 -> re-import re-verify (291/41/11 -> ~0).
- wake80 2026-06-07: POLL #10 fix muscle wagcpug42 — STILL RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  Currently researching the canonical Haskell CompactValue multi-asset layout to correctly reconstruct
  multi-asset Values for the secondary fix (mod.rs:6435) — thorough (the primary endianness flip is the
  one-liner). Not disturbed; no competing heavy work. #10 stays FIXING; next: poll wagcpug42 -> re-import re-verify.
- wake79 2026-06-07: DRIVE #10 endianness FIX. Analyze muscle wxuwzffyl fully complete (rootcause confidence
  0.96). Launched fix muscle wagcpug42 (Tier 1, worktree): apply COMPLETE refscript+datum patch as base, then
  flip mod.rs:68 TxIx from_le_bytes->from_be_bytes (with #461 reconciliation: determine decoder scope + explain
  ordered-store BE key) + fix pinned test/fixture + multi-asset drop mod.rs:6435. Emphasized a KEY-CORRECTNESS
  oracle (not just unit test — the last fix passed units but was a runtime no-op): idx-1 entry must decode to
  index==1 and store-lookup 00000c0c...#1 -> coin 1750000. 5GB RAM, no nodes. #10 ROOT-CAUSED -> FIXING. next:
  poll wagcpug42 -> re-import re-soak (291/41/11 -> ~0) -> gauntlet -> commit.
- wake78 2026-06-07: *** #10 ROOT CAUSE FOUND (analyze muscle wxuwzffyl) *** = mempack/mod.rs:68 decodes TxIx
  little-endian but the on-disk UTxO-HD tables key is BIG-ENDIAN -> imported UTxOs at output index >=1 are
  mis-keyed (idx1->256) -> phase-2 lookup-by-real-index misses -> the 11 not-found + 291 Error-term + 41 budget.
  Explains why the byte-exact-proven datum/refscript decode was a RUNTIME NO-OP (right data, wrong key) and why
  ledger totals stayed exact (summing is key-independent; only lookup broke). Triple-confirmed: 3 Koios samples
  (#1 stored at #256), index-0 correct, raw-blob probe 0001=BE. The engine's discipline FORCED this: the datum
  fix not moving the counts is what drove the diagnosis to the real one-line endianness bug that two "correct"
  decode fixes had masked. Advanced #10 DIAGNOSING -> ROOT-CAUSED. Fix (next wake): from_le_bytes->from_be_bytes
  at mod.rs:68 + reconcile pinned #461 LE test (contradicted by real data) + secondary multi-asset drop
  mod.rs:6435. Did NOT launch fix muscle this wake (analyze muscle's agent2 still finalizing; avoid 2-muscle
  contention). NON-chain-critical (trust-on-consensus) but a real fast-start UTxO-key-integrity bug.
- wake77 2026-06-07: POLL #10 analyze muscle wxuwzffyl — STILL RUNNING, healthy (4GB RAM, no nodes, 0 completed).
  Significant interim lead: verify10b snapshot meta = backend dugite-lsm, utxo_count=0 (UTxOs in the LSM store
  not inline); muscle now verifying whether the LSM `active` store (2.7GB, has data) carries datum/script_ref at
  runtime READ time — i.e. the LSM bincode roundtrip / phase-2 resolution read path is the suspect for the
  decode-vs-runtime disconnect (recall utxo_store.rs uses bincode of the FULL TransactionOutput, so it SHOULD
  preserve them — muscle is confirming empirically). Not disturbed; no competing work. #10 stays DIAGNOSING.
- wake76 2026-06-07: POLL #10 analyze muscle wxuwzffyl — STILL RUNNING, healthy (5GB RAM, no nodes, 0 completed
  events). Interim trace findings worth noting: the snapshot ENCODE is faithful for datum/script_ref (post-Alonzo
  output keys 2/3 emitted) so the snapshot roundtrip is NOT the loss point; muscle now inspecting the actual
  phase-2 dumps' resolved output_cbor for tx 578069c6 to see if datum/script_ref are present at EVAL time (the
  real decode-vs-runtime disconnect). Not disturbed; no competing heavy work. #10 stays DIAGNOSING; next: poll.
- wake75 2026-06-07: #10 VERIFYING VERDICT = FAILED end-to-end (the gate did its job). Complete-fix re-soak
  (verify10b) shows IDENTICAL divergence counts to before the datum fix: 291 Error-term + 41 budget + 11
  not-found. So the byte-exact-PROVEN decode fix (datum+refscript) is a runtime NO-OP for these txs -> the
  corrected import data isn't reaching phase-2 eval. Falsified multi-asset (tx 578069c6 = 0-asset inputs).
  Puzzle: decode-oracle says all tag-5 have script_ref yet runtime still 11 not-found = static-vs-runtime
  disconnect. SIGTERM'd verify10b (RAM 5GB free). Launched ANALYZE muscle wxuwzffyl to trace import->store->
  phase-2-resolution and pin where datum/script_ref is lost. #10 -> DIAGNOSING. Did NOT commit (divergence not
  gone; refscript+datum fixes stay on main as oracle-proven base). Recorded that these divergences are
  NON-chain-critical (trust-on-consensus). LESSON re-affirmed: oracle-green unit tests != runtime byte-exact.
- wake74 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. Complete-fix build finished BUILD_EXIT=0 (1m39s;
  wake73's "finished" was a PID artifact). Cloned db-preprod-sync -> verify10b, ran the complete-fix binary
  (pid 99008): import path executed WITH datum+refscript decode (snapshot 1487.5MB vs 1161.9MB refscript-only =
  778K inline datums + refscripts now populated, utxo_count=4116338 skipped=0). Node syncing 124999169 -> tip to
  re-process the failing slots. Next wake greps verify10b-resoak.log: 290 Error-term + 41 budget + 11 not-found
  must ALL be gone -> #10 verified -> gauntlet -> commit. 3GB RAM free; no competing heavy work.
- wake73 2026-06-07 (notification-triggered): #10 EXPANDED FIX COMPLETE (muscle wnqthg8c8, Tier A', checks_green,
  2 crates). MAJOR: the datum bug was tag-4 CORRUPTION (BinaryData VarLen not stripped + multi-asset dropped),
  not just a drop -> 132,148 corrupt -> 778,015 correct inline-datum outputs. Fix = decode_binary_data +
  decode_plutus_data_cbor (reuses tag-24 block decoder) + OutputDatum::InlineDatum on import. GAP B residual-11
  diagnosed already-covered (tag-5 multi-asset, 0 missing script_ref, byte-exact hashes). BOTH byte-exact oracles
  pass (refscript CIP-hash + datum blake2b-256 == Koios datum hashes). Preserved COMPLETE patch (1365 lines, 2
  crates, applies clean), applied to main, launched release build pid 98554. Advanced #10 FIXING-EXPANDED ->
  VERIFYING-BUILDING. next: BUILD_EXIT=0 -> fresh import from db-preprod-sync -> re-soak -> 290+41+11 WARNs GONE
  -> gauntlet -> commit. Did NOT commit (end-to-end divergence not yet confirmed gone; unit oracles != chain proof).
- wake72 2026-06-07: POLL #10 muscle wnqthg8c8 — at FINAL verification (clippy CLIPPY_EXIT=0; running
  `cargo nextest run --workspace` now = last gate before it returns the FIX result). Imminent completion;
  not disturbed, no competing work. Verify-prep from wake71 stands (db-preprod-sync import source intact, disk
  headroom). #10 stays FIXING-EXPANDED. next (on completion notif or next poll): read FIX result -> fresh
  import re-verify from db-preprod-sync (must clear 290 Error-term + 41 budget + 11 not-found) -> gauntlet -> commit.
- wake71 2026-06-07: POLL #10 muscle wnqthg8c8 — STILL RUNNING, now in TEST/BUILD phase (nextest/cargo build,
  fixing its own datum-decode unit tests re BinaryData VarLen prefix) -> near completion. Not disturbed. Did
  non-contending VERIFY PREP to de-risk the upcoming re-verify: confirmed db-preprod-sync/haskell-ledger/
  (124995007 + 124999169) is INTACT = reusable fresh-import source; GC'd 4 SUPERSEDED clones (preprod-soak
  [gate-2 banked], preprod-verify10 [refscript-only verdict captured], preprod-9verify [#9 done], preprod-live
  [old]) -> disk 125->132GB free (CoW-shared so modest). KEPT db-clones/mainnet-ep213 (#0/#3) + db-preprod-sync.
  #10 stays FIXING-EXPANDED. next: poll wnqthg8c8 -> on FIX result, fresh import re-verify from db-preprod-sync. (active, 0 completed events, 4GB RAM free, no
  nodes). Captured a MATERIAL finding from its live log for continuity: decoding inline datums revealed the OLD
  tag-4 (datum-only) MemPack decode path was SILENTLY CORRUPTING datums — inline-datum-bearing outputs jumped
  132,148 -> 778,015 once decoded correctly (so ~646K outputs had mishandled datums, not just the import-drop).
  Muscle is validating decoded datums against real CBOR shapes (bstr, tag(121)/tag(122)/tag(1282) "output
  reference" datum) and Koios tx_info. So #10's datum half is BIGGER than "inline datums dropped" — it includes
  a tag-4 corruption bug; the muscle is fixing both. This raises #10's severity/value (correct phase-2 datums for
  778K outputs on any fast-start node). No competing heavy work. #10 stays FIXING-EXPANDED; next: poll wnqthg8c8.
- wake69 2026-06-07: POLL #10 muscle wnqthg8c8 — STILL RUNNING, healthy (actively building a real-blob
  diagnostic to locate the residual-11 refscript; no errors; 4GB RAM free). Not disturbed; no competing heavy
  work. Light lock-free support: Koios script_info confirms residual script 7afbde082796cfa6ed6ba87728c57f-
  111815340af03e1897094163b3 IS a real preprod plutusV3 reference script (size 4751, creation tx dca0e8d5...).
  SAME lang (V3) as the target 744837b0a3 that already resolves -> the residual failure is in the TxOut ENCODING
  holding it, NOT the script type — consistent with the multi-asset tag-5 sub-branch hypothesis given to the
  muscle. Validates the muscle is chasing a real target. #10 stays FIXING-EXPANDED. next: poll wnqthg8c8.
- wake68 2026-06-07: DRIVE #10 EXPANDED FIX. Reverted main's refscript changes CLEAN (patch preserved + applies
  clean to HEAD), then launched expanded fix-muscle wnqthg8c8 (Opus, worktree, Tier A') to: git-apply the
  refscript patch as the base, then (A) decode inline datums (MemPackTxOut.datum -> OutputDatum::Inline at
  mod.rs:6440, reusing the existing PlutusData decoder) + (B) cover the residual-11 refscript sub-case
  (7afbde08... etc, likely multi-asset tag-5). Gave it a NEW datum byte-oracle (decoded datum == Koios
  inline_datum) on top of the refscript hash-oracle. Did NOT block on the muscle (long build+nextest). State
  stays FIXING-EXPANDED. next: poll wnqthg8c8 -> fresh re-import re-verify (full-replay byte-exact oracle) ->
  gauntlet -> commit the COMPLETE fix (refscripts+datums+residual) as one focused 2-crate unit.
- wake67 2026-06-07: #10 VERIFYING VERDICT — fix CORRECT but INSUFFICIENT (the gate did its job: BLOCKS a
  premature commit). The fresh fixed-binary re-import re-soak cut "script not found" 379->11 (97%) and zeroed
  MissingScriptWitness; hash-oracle target txs now resolve; NO regression. But resolving scripts EXPOSED 290
  "Error term" + 41 "budget exhausted" eval-divergences. Decisive discriminator (NOT a guess): full-replay is
  byte-exact for these txs, so it's pure import-incompleteness -> mod.rs:6440 DROPS INLINE DATUMS ("Skip for
  now") -> resolved-script + missing-datum = wrong ScriptContext = Error term. Plus 11 residual "not found"
  (tag-5 sub-case uncovered). Confirmed the datum bytes ARE now available (txout.rs:503/583) so the fix is
  feasible in the same 2 crates. Advanced #10 VERIFYING-RESOAK -> FIXING-EXPANDED ("fast-start phase-2 import
  completeness" = refscripts[done] + inline-datums[todo] + residual-11[todo]). SIGTERM'd verify10 node (RAM
  freed), kept #10 refscript patch on main as the base. Did NOT commit (divergence not gone). next: expanded
  fix-muscle for the inline-datum decode + residual-11, then fresh re-import re-verify (full-replay = oracle).
- wake66 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. Build finished BUILD_EXIT=0 (fixed binary).
  DROVE the fresh import with the fixed binary: db-preprod-sync was already in import-state (haskell-ledger/ +
  immutable/, no dugite snapshot), so APFS-cloned it -> db-clones/preprod-verify10 and ran the fixed binary
  (pid 46355). Log CONFIRMS the import path executed ("Loading UTxO set from MemPack tables blob ...124999169/
  tables 887932877 bytes" -> "complete utxo_count=4116338 skipped=0" -> "Haskell ledger import complete") —
  so script_ref is now decoded through the fixed decode_tag5 + decode_imported_script_ref. Node syncing from
  124999169 toward tip; it will RE-PROCESS the previously-failing slots (all > 124999169), which IS the
  reproduction. Next wake greps verify10-resoak.log for the ref-script WARNs at those slots: GONE = end-to-end
  verified -> gauntlet -> commit. No competing heavy work (only this node + idle build process exiting).
- wake65 2026-06-07: START VERIFYING #10. SIGTERM'd the soak cleanly (4s; "Snapshot saved epoch=293, 4118241
  UTxOs" + "Shutdown complete" — ImmutableDB flushed, no corruption); RAM freed. git-apply'd the #10 patch to
  MAIN (applies clean; uncommitted working-tree only — commit still gated on gauntlet). Launched bg release
  build pid 45661 (.jobs/verify-build-10.log; compiling downstream crates after dugite-serialization). Recorded
  the CRITICAL verification nuance: must RE-IMPORT from the raw Haskell tables with the fixed binary (the old
  soak db has script_ref=None baked in by the old-binary import -> reusing it = false negative). State
  VERIFYING-PENDING -> VERIFYING-BUILDING. Next wake: poll build -> on BUILD_EXIT=0, fresh import + check
  script_ref populated for f08f73509b0d3b4a#0 / ref-script WARNs gone -> gauntlet -> commit.
- wake64 2026-06-07 (notification-triggered): #10 fix-muscle we0nz74zr COMPLETED. Tier A', checks_green, EXACTLY
  2 crates (dugite-serialization + dugite-node, 7 files +732/-56). *** HASH-ORACLE PASSED ***: decoded the real
  on-disk preprod tables blob and the CIP-tagged blake2b-224 of the decoded ScriptRef for f08f73509b0d3b4a#0 ==
  744837b0a3... and e2766b4eb2b8d4da#0 == d55eb689d8... EXACTLY (chain-critical proof, not just green tests).
  The fix parses the MemPack Datum-option + AlonzoScript tail in decode_tag5 (was opaque_tail/None) and decodes
  ScriptRefKind->ScriptRef at mod.rs:6411; quotes verbatim Haskell MemPack instances. PRESERVED the fix as
  candidate-fix-10-mempack-refscript.patch (923 lines) + worktree wf_41bd7059-365-1; NOT committed (gated on
  VERIFYING+gauntlet). Advanced #10 ROOT-CAUSED -> VERIFYING-PENDING. Did NOT build now (only 3GB free RAM +
  soak running -> would risk swap/wedge). Set the VERIFYING plan for next wake: SIGTERM soak (gate-2 banked) ->
  build fixed binary -> fresh fast-start re-soak -> confirm ref-script WARNs GONE at the failing slots -> gauntlet
  -> commit. RAM-safe deferral, fix durably preserved.
- wake63 2026-06-07: #10 fix-muscle we0nz74zr STILL RUNNING (actively editing decode_tag5 in the worktree —
  iterating, likely refining after the hash-oracle check; not disturbed, no competing heavy work). Advanced
  gate (1) via a zero-cost HEAD spot-check: the soak log's "Building LedgerState from Haskell snapshot
  epoch=293" line shows reserves=13072484951876873 treasury=1870588626354717, and Koios totals(293) gives the
  SAME to the lovelace -> mithril-import reserves/treasury byte-exact + held. Combined with wake62's
  dugite-recomputed active_stake match, the live HEAD node matches Koios on ALL 3 core accounting outputs at
  ep293. Recorded honestly that reserves/treasury here are import-faithful (transition-computation is covered by
  full-replay ep0-233). #10 stays FIXING. Idea banked: if the soak crosses ep293->294, that live-tests dugite's
  OWN reserves/treasury transition on HEAD — watch for it.
- wake62 2026-06-07: #10 fix-muscle we0nz74zr STILL RUNNING (now at its hash-oracle verification step —
  reading the real 885MB preprod tables blobs to validate its MemPack decoding; not disturbed). Advanced
  candidate 2b to RESOLVED-ON-HEAD via a LIGHT live-node query (no heavy work): dugite-cli query stake-snapshot
  against the at-tip HEAD soak node (/tmp/engine-soak.sock) -> Go=886,446,899 ADA == Koios as(292), Set=
  912,041,407 == as(293). HEAD go(293) is the dump's set(292) one epoch on, and it's +100 ADA above the Jun-3
  dump, matching Koios EXACTLY -> the -100 ADA was a STALE dump (already fixed post-Jun-3). #481 lesson held
  again. ledger.preprod frontier re-validated through ep293. Net: a candidate divergence investigated and
  CLOSED without a heavy replay, by querying the already-running HEAD node. #10 stays FIXING.
- wake61 2026-06-07: #10 fix-muscle we0nz74zr STILL RUNNING (agent mid-rewrite of decode_tag5; not disturbed,
  no competing heavy work launched — 4GB free RAM during its compile). Advanced a DIFFERENT item via a lock-free
  spot-check: discovered NEW ledger candidate 2b — preprod ep292 active_stake -100 ADA vs Koios, CONSTANT across
  all 3 snapshot phases (mapping pinned: go/set/mark(292) == Koios as(291/292/293), each exactly -100,000,000
  lovelace). Found in epoch-dumps-dugite/epoch_000292.json. Flagged the PROVENANCE caveat (Jun-3 stale dump,
  #481 lesson) — MUST reproduce on HEAD (query live soak node @ep293 or HEAD replay) before treating as real;
  likely same instant-stake-attribution class as ep57 if it survives. Recorded item 2b + annotated
  ledger.preprod frontier (holds ep0-230; ep231-293 not yet HEAD-verified). #10 stays FIXING.
- wake60 2026-06-07: POLL #10 muscle (we0nz74zr STILL RUNNING — agent jsonl live, journal shows started not
  completed; mid two-part MemPack fix + nextest --workspace). Did NOT disturb it / did NOT launch competing
  heavy work. Advanced the OTHER in-progress item instead: sync-gate live-soak -> VALIDATED. Soak node tracks
  the live preprod tip in LOCKSTEP (block 4793042 slot 125082823 == koios live 4793042; extends within seconds
  of each new block, 4793037->4793042 over ~2min), continuously at/near tip since 16:55 (~18min), 0
  panic/OOM/wedge/chain_diverged. => readiness GATE (2) live-sync-to-tip VALIDATED on preprod. Recorded the
  nuance that gate (3) phase-2 independent ref-script validation on the fast-start path remains blocked on #10
  (node trusts consensus meanwhile; gates 2 and 3 are separate). #10 stays FIXING. Next wake: re-poll we0nz74zr.
- wake59 2026-06-07: DRIVE #10 FIX. Soak still EXACTLY at tip (block 4793035 == koios live 4793035, ~17min,
  0 panic/OOM/wedge, 1.7% CPU) — sync-gate live-soak holding. Cheap pre-check already satisfied last wake
  (root-cause is definitive), so advanced #10 ROOT-CAUSED -> FIXING: launched fix-muscle we0nz74zr (Opus,
  isolated worktree, Tier A') for the two-part fix (decode_tag5 tail-parse populating script_ref:Some +
  mod.rs:6411 ScriptRef decode). Gave the agent a built-in HASH ORACLE (decoded refscript for UTxO
  f08f73509b0d3b4a#0 must hash to 744837b0a3...; e2766b4eb2b8d4da#0 -> d55eb689d8...) so it self-verifies the
  MemPack Script decoding instead of guessing — flagged the tvar blob is MemPack-encoded (not CBOR) and to
  reuse existing ScriptRef/compute_script_ref_hash. Did NOT block the wake on the long build+nextest; next wake
  polls we0nz74zr. Commit ONLY after re-soak proves the failing slots stop WARNing (green tests != proof).
- wake58 2026-06-07: *** #10 ROOT-CAUSED (deviated from stale standing-prompt's #1 ep57, which is RESOLVED per
  engine-state) ***. (a) Soak node now EXACTLY at tip (block 4793026 == koios live 4793026, sustained ~12min,
  0 panic/OOM/wedge, 1.9% CPU). (b) Reproduced #10 via Koios tx_info: failing tx 0d325a6e... supplies 5 scripts
  via REFERENCE INPUTS; the hashes dugite reports Missing (744837b0a3/d55eb689d8/ec80112317/d0adeb2053) are the
  reference scripts on pre-snapshot UTxOs. ROOT CAUSE = mithril/haskell-import UTxO loader (mod.rs:6411) hard-
  sets script_ref=None on a FALSE 'only-needed-in-gap-replay' premise; #9 EXPOSED this latent gap (pre-#9 the
  snapshot was discarded + full-replayed, rebuilding script_refs). Localized the fix: decode_tag5
  (mempack/txout.rs) dumps the script tail into opaque_tail instead of populating script_ref:Some; + mod.rs:6411
  must decode those bytes into ScriptRef. Two-part, 2 crates, Tier A'. Ledger byte-exactness UNAFFECTED (membership/
  stake don't need script_ref). Updated backlog #10 (NEW->ROOT-CAUSED), In-progress, phase2.preprod frontier
  (annotated mithril-fast-start gap). next wake: FIX via fix-muscle then re-soak to confirm the WARNs are gone.
- wake57 2026-06-07: *** LIVE-SOAK REACHED TIP HEALTHY (#9 fast-start validated end-to-end) ***. The soak node
  (pid 99162, clone db-clones/preprod-soak) fast-started via the #9 Convertible mem->lsm path (NO genesis
  replay, utxo_count=4116338, 8min elapsed) and caught up to the live preprod tip: node block 4793022 slot
  125082081 hash c8004a5b... == koios tip 4793022, 1 block/28s behind live 4793023. STAT SN, 1.5% CPU
  idle-at-tip, RSS 4.8GB. 0 panic/0 OOM/0 wedge/0 real-ERROR (the "chain_diverged" matches were benign
  ChainSync-intersection status lines; rollbacks were normal initial peer intersections). Sync-gate live-soak
  portion SUCCEEDING — needs only a sustained-window confirm (next wake). *** NEW REAL FINDING #10 ***: at tip,
  EVERY reference-script-spending/withdrawing tx fails dugite's INDEPENDENT validation (phase-1
  MissingScriptWitness + phase-2 'script not found for redeemer purpose'); node trusts on-chain so no wedge.
  Same script hashes recur (744837b0a3... withdrawal, ec80112317..., 85e3bfa6b3...) => reference scripts in old
  UTxOs. HYPOTHESIS: the #9 Convertible fast-start path doesn't preserve reference-script bytes/index on
  converted UTxO entries — a REGRESSION in my own #9 fix (phase2.preprod was byte-exact on FULL replay). Logged
  as backlog #10 with a falsifiable experiment (full-replay node to same tip — same WARNs?); did NOT guess a
  fix (the #438-class trap). Next wake: (a) sustained at-tip confirm, (b) run experiment #10.
