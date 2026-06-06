# Engine State  (single source of truth — committed every wake)

## Control
- HALT: false
- refuter_N: 3
- daily_token_budget: 40000000
- cadence_floor_secs: 270
- cadence_ceiling_secs: 1800
- reference_node_socket: none        # Koios-first; set if a cn node is up

## Frontiers  (advance these; zero open divergence behind each)
- ledger.preprod:   BYTE-EXACT vs Koios — total active_stake matches at ep100/150/200/230 (Shelley→Conway) + ep57 per-cred exact; clean replay ep0-233 zero halts; finishing to ep293
- ledger.mainnet:   BYTE-EXACT vs Koios — reserves+treasury exact at ep212-221 (doc's +180.4B ep213 divergence GONE on HEAD); replay validating further
- sync.preprod:     from-genesis REPLAY clean + #9 snapshot-backend fix LANDED + LIVE-SOAK reached tip healthy (wake57): clone db-clones/preprod-soak fast-started via #9 Convertible mem->lsm path (NO genesis replay, utxo_count=4116338), caught up to live tip (node block 4793022 hash-matched koios, 1 block/28s behind live 4793023), 0 panic/0 OOM/0 wedge, RSS 4.8GB, CPU 1.5% idle-at-tip. REMAINING: sustained-window confirm (next wake) + investigate #10 reference-script WARNs (below) which may be a regression in the #9 fast-start path
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
   found for redeemer purpose". Tier A' (phase-2). state:ROOT-CAUSED attempts:0  next:FIX via fix-muscle
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
- item: #10 (real, phase-2 mithril-fast-start ref-script gap) state:FIXING — fix-muscle we0nz74zr launched
  (Opus, isolated worktree, Tier A'). Two-part fix: (1) decode_tag5 tail-parse -> script_ref:Some;
  (2) mod.rs:6411 decode bytes -> ScriptRef. Given a BUILT-IN HASH ORACLE (decoded refscript for UTxO
  f08f73509b0d3b4a#0 must hash to 744837b0a3...) so the agent self-verifies its MemPack Script decoding, not
  guesses. next:poll muscle -> if checks_green + hash-oracle PASSES, VERIFYING (re-soak: failing slots must
  stop WARNing) -> gauntlet -> commit. Do NOT commit on green tests alone (hash match is the proof).
- item: #0 ep246 reserves +82,270,482 (Allegra/PV3) state:PARKED-WITH-ROOT-CAUSE — structural member-reward fold
- item: live soak (sync-gate) state:AT-TIP CONFIRMED — soak node block 4793035 == koios live tip 4793035,
  sustained ~17min, 0 panic/OOM/wedge, RSS 4.8GB, CPU 1.7% idle-at-tip. Sync-gate live-soak portion HOLDING.
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
- fix-muscle we0nz74zr (#10 phase-2 ref-script fix, Opus, worktree) — /workflows-visible. Poll next wake;
  on completion read FIX result (files/tier/checks_green/hash-oracle). If hash-oracle PASSES -> VERIFYING re-soak.
- live-soak pid 99162 (db-clones/preprod-soak) — at-tip soak, .jobs/live-soak.{pid,log}. SIGTERM-only to stop.
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
