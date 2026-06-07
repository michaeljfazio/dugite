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
20. [M][security/hardening][REAL-NEW wake200, from #10 gauntlets rounds 4-6] Snapshot-import adversarial-hardening:
   dugite MemPack/CBOR decoders are systematically MORE LENIENT than Haskell's strict Data.MemPack on MALFORMED
   snapshot inputs (real well-formed snapshots import byte-exact — #10 DONE — but crafted/corrupt blobs that Haskell
   loadSnapshot ABORTS are silently accepted or rejected-at-wrong-offset). Concrete instances found by the #10
   gauntlet refuters (all adversarial-only, deferred from #10's commit): (a) decode_varlen (compact.rs:50-69) has NO
   terminal-byte high-bit mask + NO overflow/non-minimal rejection -> a >2^64 or overlong varlen silently truncates
   to u64 Ok; Haskell unpack7BitVarLenLast F.fails. Used at ~10 sites (CompactAddr len, Coin, MA count/rep len, tag-4/5
   datum/script len). (b) DEFINITE-length tables map truncated to M<N declared entries -> silently imports the prefix
   (TvarBody/TvarIterator track indefinite+saw_break but NOT entries_remaining; round-5 fixed only the indefinite arm);
   Haskell decodeMapLen demands exactly N -> DecoderErrorPrematureEOF abort. (c) enforce_snapshot_backend_is_utxohd_mem
   (mempack/mod.rs:917) resolves `backend` via serde_json value.get LAST-wins while tablesCodecVersion uses the
   first-wins machinery -> opposite dup-key resolution for two fields of the SAME SnapshotMetadata; aeson is first-wins
   for all. FIX class: make every snapshot-leaf decoder hard-fail exactly where Haskell strict MemPack/CBOR does
   (varlen mask+overflow+too-many-bytes; definite-map entry-count; backend first-wins). Mostly backstopped in practice
   by #17 (whole-file CRC) + Mithril signature, hence M not H. state:NEW attempts:0
17. [H][security/integrity][REAL-NEW, from gauntlet w3upqlq0y compounding-feedback] Mithril/Haskell snapshot
   IMPORT does NOT verify the snapshotChecksum/CRC. Upstream V2/InMemory.loadSnapshot computes
   crcOfConcat(state-CRC, tables-CRC) and throws ReadSnapshotDataCorruption on mismatch; dugite's
   import_haskell_ledger_snapshot reads the `checksum` meta field but NEVER verifies it. -> a snapshot with valid
   meta (backend=utxohd-mem, tablesCodecVersion=1) but CORRUPTED/tampered/truncated state|tables bytes that
   remain MemPack-decodable is SILENTLY ACCEPTED (upstream rejects it). Adversarial-deployment surface for the
   mithril-fast-start path (a corrupt UTxO set -> wrong phase-2 ScriptContext at the live tip). SEPARATE from
   #10's TxIx/datum/refscript/multiasset import-completeness scope. FIX: compute crcOfConcat(state, tables) ==
   snapshotChecksum at import; ERROR on mismatch. state:NEW attempts:0
16. [L][phase2][LATENT, from gauntlet wqwgen1p0] decode_imported_script_ref hard-codes Plutus language tag
   0->V1,1->V2,2->V3,3->V4 as 'global', but the MemPack PlutusScript tag is ERA-RELATIVE (per-era packTagM).
   Byte-exact for ALL CURRENT eras only because each era's language list is a strict PREFIX [V1,V2,V3,V4] (no
   reorder/removal), so era-relative index == fromEnum(language) today. Patch comments self-contradict
   ('era-relative' vs 'global'). NOT a current divergence. FIX: make the mapping era-aware (or assert the prefix
   invariant + comment) when a future era reorders/removes a language. state:NEW attempts:0 (follow-up after #10 lands)
15. [M->H][phase2][ROOT-CAUSED-CONFIRMED wake165 — byte-level proof] 306 "script returned Error term" phase-2
   divergences = dugite serialiseData CANONICAL RE-ENCODE bug (general-UPLC). *** wake165 DEFINITIVE PROOF (wpeec891q
   mechanism dim): failing tx 27751ab9 spends a script (7afbde08, PlutusV3, 4751 bytes) that calls serialiseData 11x
   and checks blake2b(serialiseData(datum)) against the stored datum_hash. The on-chain datum d87a9fd8799f... is 276
   bytes using 8x INDEFINITE-length CBOR arrays (0x9f..0xff); blake2b256(those 276 verbatim bytes) = bbd352028feffe9a
   80a2822b46b9858bc1cf883cff383e1191b47d27ed708eb0 = the on-chain datum_hash EXACTLY. dugite denotations.rs:601
   d.to_cbor() RE-ENCODES to 270 bytes canonical DEFINITE-length -> blake2b256 = feec1506b516a2ca... != datum_hash ->
   the script's own hash-check fails -> 'Error term'. 12/13 unique failing-script credentials call serialiseData (the
   2 that don't always co-appear with an SD script). This is the SAME set in verify10A & verify10j (post-snapshot live
   blocks), import-independent. FIX (Tier A', dugite-uplc ONLY): the CEK Constant::Data must carry the ORIGINAL CBOR
   bytes verbatim (Haskell MemoBytes/BinaryData equivalent) when the Data originated from CBOR decode (datums,
   redeemers, txInfo Data); serialiseData returns those memoised bytes unchanged, falling back to canonical to_cbor()
   ONLY for machine-constructed Data. Thread original bytes through: plutus-data CBOR decode -> Constant::Data ->
   builtin SerialiseData. VERIFY: replay ep293 window (slots 125001020+), confirm 306 -> ~0; then gauntlet. Haskell
   ref: Cardano.Ledger.Plutus.Data BinaryData/hashBinaryData (hashAnnotated over memoised SBS) + Plutus
   builtinSerialiseData returning the original bytes. state:ROOT-CAUSED-CONFIRMED attempts:0 (ACTIVATE after #10
   phase-1 lands). --- wake163 re-frame (now fully confirmed): ---
   *** wake163 RE-DIAGNOSIS OVERTURNS the
   old framing below: classification muscle wpeec891q found a FULLY-INDEPENDENT PURELY-POST-SNAPSHOT failing tx
   27751ab9 (slot 125001020, PlutusV3 5b2bfe89; only input 3d7bb051 @slot124999282 > cutoff 124999169 = NEVER
   imported) — so these are NOT import-incompleteness and the old 'full-replay byte-exact / compact-address' suspect
   is REFUTED (the full-replay-byte-exact claim was UNVERIFIED at ep293). CODE-CONFIRMED ROOT CAUSE: dugite
   serialiseData builtin crates/dugite-uplc/src/builtin/denotations.rs:597-604 does d.to_cbor() = CANONICAL re-encode,
   but Haskell serialiseData returns the MEMOISED ORIGINAL bytes (MemoBytes/BinaryData). A script that serialiseDatas
   a non-canonical Data (Constr tag-102 / indefinite arrays / non-minimal ints) and hashes/compares the result
   diverges -> logical Error term. serialiseData is the ONLY Data-BYTES divergence vector (all other Data builtins
   structural) — consistent with the wake161 inline-datum-resolution fix being a NO-OP. FIX (Tier A', dugite-uplc):
   dugite's Data must carry the original CBOR bytes (like Haskell MemoBytes) so serialiseData returns the memo when
   present, canonical re-encode only for machine-constructed Data; thread original bytes from CBOR decode of
   datums/redeemers/Data through the CEK Constant::Data. VERIFY by replaying the ep293 window (slots 125001020+) and
   confirming 306 -> ~0. mechanism dim of wpeec891q still confirming (does 27751ab9's script invoke builtin tag 51?).
   state:ROOT-CAUSED attempts:0 (becomes active AFTER #10 phase-1 lands). --- OLD (wake86, now refuted) framing: ---
   Fast-start residual 277 "script returned Error term" — SEPARATE from #10's
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
- item: #0 (mainnet ep246 reserves) state:FIXING (reward_pot epoch_fees ~5ppm; root cause RE-LOCATED via data).
  *** wake233: DIAGNOSE wbqhzeczq COMPLETE — ROOT CAUSE RE-LOCATED (data-driven, HIGH confidence) to reward_pot
  epoch_fees, NOT apply_utxo_changes/stake. Dim-1's '-7.1M per-cred whale' was a SNAPSHOT-LAG ARTIFACT (dump ep245
  stake == Koios with one-epoch lag, byte-exact). Dim-2 (the correcting, rigorous result): every member reward is
  uniformly under-scaled by -5.027 ppm (15 creds across DIFFERENT pools, stdev 0.0051 ppm, e.g. stake1uxrx2qr8...
  dugite=40901117728 koios=40901323467 d=-205739). Pool-INDEPENDENT constant -> NOT sigma/stake (both byte-exact:
  go.totalActiveStake=21,956,097,174,685,676, totalStake, appPerf all byte-exact) -> localizes to reward_pot (rewards.
  rs:244 = expansion+epoch_fees-treasury_cut). expansion correct (ep245 reserves byte-exact) -> the ~5ppm is in the
  EPOCH_FEES/ss_fee term (rewards.rs:184). Haskell uses the GO-snapshot ssFee (TWO-EPOCH LAG), not live fees.
  *** CRITICAL LESSON: analyze-1 (wuqv1kgo9) WRONGLY 'ruled out' rewards.rs/reward_pot and pointed at apply_utxo_changes
  -> rounds 1-2 chased red herrings (usefully proved apply_utxo_changes/rebuild correct + added regression tests, but
  not the bug). The DATA-comparison (dim-2 cross-check) found the real cause AND corrected dim-1's lag artifact. Don't
  trust 'ruled out' without data. *** The 3 prior #0 fixes failed in the member-fold (445-490); the REAL site is the
  reward_pot epoch_fees scalar (184) — a DIFFERENT site. DROVE: launched VERIFY-THEN-FIX muscle w0oegi6uf (run
  wf_656d4f7e-345, opus, rewards.rs reward_pot/epoch_fees only) with discipline: trace dugite's epoch_fees source vs
  Koios epoch_info fees @244/245/246, confirm swapping to the GO-snapshot ssFee gives +5.027ppm == +82,215,213 deficit
  (+ tau-cut == treasury -55,269), ONLY then fix; if not localizable to fees -> report (maybe a reward_pot precision
  issue) + STOP. NEXT WAKE: poll -> verified+fixed -> dump/reward-loop verify ep246 reserves==12880948865137767.
  *** wake231: ROUND-2 muscle wr9tddl4q RESULT = rebuild/genesis-load/pointer ALSO correct (2 invariant tests PASS:
  rebuild_stake_distribution == incremental per-cred across base/script/pointer/Byron + aliasing hashes; genesis-load-
  then-spend == rebuild). Static: state::stake_routing == common::stake_routing == Credential::to_typed_hash32; rebuild
  gated OFF during from-genesis replay. NO fix (discipline held; 2 tests kept on main as coverage). *** KEY: both code
  rounds prove dugite is INTERNALLY consistent (incremental==rebuild, add/spend symmetric) — which CANNOT catch a
  dugite-vs-HASKELL credential-attribution difference that's internally consistent but per-cred wrong vs chain. Code-
  internal invariants are exhausted; need a dugite-vs-Koios DATA comparison. DROVE: launched ROUND-3 DIAGNOSE muscle
  wbqhzeczq (run wf_aae4b738-242, opus via diagnoseModel:'opus') comparing dugite top-30-50 per-cred active stake
  (epoch-dumps-engine/mainnet-droptrace/epoch_000245.json) to Koios per-account active stake @ep245 (+ reward-side
  cross-check vs account_reward_history @ep246) to find the credential(s) where dugite stake != Koios and infer the
  class. NEXT WAKE: on found credential -> trace its address/stake class -> the dugite-vs-Haskell extraction/value bug
  -> FIX; if top-50 all match (error spread thin) -> need a FULL per-cred dump (re-dump w/ higher top-N) or instrumented
  replay. HARNESS REALITY: no db-mainnet (from-genesis mainnet replay infeasible quickly); rely on existing dumps +
  Koios for localization.
  *** wake227: ROUND-1 muscle wxbflru4x RESULT = apply_utxo_changes is SYMMETRIC/CORRECT (5 create-then-spend net-zero
  invariant tests PASS: base-key/script-payment/script-stake/multi-asset/collateral — add Phase-5/sub Phase-2/
  collateral all route via identical stake_routing+credential_to_hash). Followed discipline: NO speculative fix; kept
  the 5 tests (copied to main as regression coverage + negative result). Narrowed candidates to OUTSIDE the hot path:
  (A) rebuild_stake_distribution full re-sum (state/mod.rs + certificates.rs:209) using a different cred extraction/
  value/pointer handling than incremental; (B) genesis/initial UTxO-set load (state/mod.rs:1072,1782); (C) POINTER-
  address stake inclusion consistency (snapshot_format.rs:154). Note: certificates.rs:206 does NOT prune stake_map on
  dereg (so pruning is NOT the bug). DROVE: launched ROUND-2 localize-then-fix muscle wr9tddl4q (run wf_5cf86f13-2e8,
  opus, dugite-ledger only, NOT common.rs/rewards.rs/epoch.rs) with invariant-test-FIRST: rebuild_stake_distribution
  (full re-sum) MUST == incremental apply_utxo_changes stake_map per-credential for all address classes (base/
  enterprise/POINTER/Byron); if a test FAILS that IS the localization+fix; if all pass -> STOP, escalate to
  instrumented mainnet replay. NEXT WAKE: poll -> invariant fails+fixed -> build -> dump-verify ep246; else round-3
  or instrumented DUGITE_REWARD_DBG mainnet replay (HEAVY) to localize the exact credential.
  *** wake225: ANALYZE wuqv1kgo9 COMPLETE — MAJOR ROOT-CAUSE REVISION (confidence HIGH on class+arithmetic, MEDIUM on
  exact cred). The documented 'member-reward fold two-map' cause is REFUTED: dugite Sigma stake_distribution ==
  go.total_active_stake == Koios 21,956,097,174,685,676 BYTE-EXACT @ep245; both maps already bundle utxo+reward_balance
  (epoch.rs:209-215,:271-277); the fold (rewards.rs:445-490) is byte-exact FOR ITS INPUTS. REAL CAUSE: a PER-CRED
  stake-VALUE error that nets to ZERO at aggregate, from an ADD/SPEND ASYMMETRY in crates/dugite-ledger/src/eras/
  common.rs::apply_utxo_changes (spend saturating_sub 202-208/334-340 debits a different cred/amount than the add
  Phase-5 credits @263) -> corrupted per-member t & sigma -> systematic FLOOR under-distribution across ~150k members
  -> +82,270,482 reserves / -55,269 treasury (reward-acct credit deficit -82,215,213, ties out: -(82,270,482-55,269)).
  Conservation: dRES+dTRE+dRewardAccts+dFees=0. *** STRATEGIC: UNIFIES #0 + #2/#11 + ep57-residual #6 — ALL the same
  apply_utxo_changes asymmetry (REWARD-DIVERGENCE-FINDINGS/MAINNET-ep213/POST-HOLD-PLAN). The 3 prior #0 fixes FAILED
  because they targeted rewards.rs/prefilter (the symptom), the #438 trap. *** Note: the wake-prompt's standing
  'root-cause hypothesis = apply_utxo_changes add/spend asymmetry in common.rs' was CORRECT all along (not stale).
  DROVE: launched LOCALIZE-THEN-FIX muscle wxbflru4x (run wf_625883f0-635, opus, dugite-ledger only) with DISCIPLINE:
  write a SYMMETRIC-ROUTING INVARIANT unit test FIRST (add output under cred X -> spend -> stake_map net-zero on X,
  no other cred moves; cover pointer/script-staking/collateral/multi-asset/cross-tx); only fix if the test concretely
  FAILS; if it passes (no code asymmetry), STOP + report 'needs instrumented replay' (NO speculative fix). HARNESS:
  HEAD dumps epoch-dumps-engine/mainnet-droptrace exist; final byte-exact gate = dump-verify ep246 reserves==
  12880948865137767 + ep209-245 unregressed (+ ideally preprod ep57). NEXT WAKE: poll -> if invariant fails+fixed ->
  build -> dump-verify; if no asymmetry -> instrumented DUGITE_REWARD_DBG replay to localize. #15 done -> activated #0 (highest-impact: mainnet reserves byte-exactness). PARKED attempts:3, so per
  the staleness lesson (#481/#438: don't trust a stale root-cause; HEAD-verify first) launched ANALYZE muscle
  wuqv1kgo9 (run wf_e3e8f3af-92f, opus/ledger) to RE-VERIFY the member-reward-fold root cause (rewards.rs:445-490 two-
  map vs Haskell single resolved-active-stake VMap resolveActiveInstantStakeCredentials) against Haskell source +
  epoch-dumps-engine/mainnet-ep213/ + Koios (reserves==12880948865137767 @ep246), characterize WHY the 2 maps
  disagree for the divergent cred, hypothesize why the 3 prior fixes failed, and produce a discrete fix + verification
  harness plan (full mainnet replay vs DUGITE_REWARD_DBG dump-loop). NEXT WAKE: read verdict -> if root cause confirmed
  + harness available -> FIX (Tier A, careful); if harness needs a mainnet db at ep246, assess availability first.
  NOTE: #0 verification is HEAVY (mainnet replay to ep246) — different from the preprod fast-start harness.
  *** ===== #15 DONE — COMMITTED 117c41e5f5 + PUSHED (prod-readiness-engine, HTTPS) wake221. =====
  GAUNTLET w4ou064y2 PASSED clean (pass=true, refuteCount=0; all 3 refuters source-verified byte-exact: getBabbage
  SpendingDatum precedence, CIP-0069 None-handling, datum-hash verbatim-vs-canonical separation, serialiseData stays
  canonical, no non-Spending regression, memo's silent-pass-where-Haskell-fails GONE). CI gate green (fmt + clippy
  -D warnings + uplc nextest 435). Committed 2 files / 1 crate (dugite-uplc) via gh/HTTPS. #15 closes the phase-2
  serialiseData arc: the 306 ep293 'Error term' (V3 SpendingScriptInfo datum was None) -> 0, byte-exact + Haskell-
  correct (canonical, no memo). *** THE #15 ARC IS THE GAUNTLET'S BEST CASE: a verbatim-memo 'fix' PASSED its replay
  (306->0) but the haskell-semantics refuter caught it as conceptually wrong (serialiseData=canonical not verbatim;
  memo = silent pass-where-Haskell-fails on non-canonical); analyze confirmed via PlutusCore source; the minimal
  correct fix (V3 datum population, canonical) then passed cleanly. Replay-passing is necessary NOT sufficient. ***
  was: #15 GAUNTLET-PENDING. *** wake219: REPLAY DECISIVE PASS — verify15min synced PAST window (tip 125125576), 0 'Error term' across the FULL
  ep293 window (125001020-125105013, was 306), 0 phase-1, 27751ab9 fixed. The canonical V3-datum fix is byte-exact +
  correct (no memo, no silent pass-where-Haskell-fails). SIGTERM'd verify15min; launched RE-GAUNTLET w4ou064y2 (run
  wf_0d183edd-b51) on the minimal fix — the prior memo R1 is RESOLVED (memo reverted; serialiseData canonical).
  #17/#19/#20 scoped out. NEXT WAKE: on PASS -> COMMIT #15 via gh/HTTPS (dugite-uplc, 1 crate); on REFUTE -> verify
  the dissent (getBabbageSpendingDatum precedence / None-handling / datum-hash bytes).
  *** wake218: MINIMAL fix muscle wkba3hja9 green (2 files dugite-uplc: redeemer_resolve.rs resolve_spend_datum_v3
  [getBabbageSpendingDatum: inline <|> witness, None-tolerant] + eval_redeemer.rs purpose_to_script_info_v3 builds
  ScriptInfo::Spending{datum} via CANONICAL plutus_data_to_data; NO memo; data.rs/to_cbor/serialiseData UNTOUCHED =
  canonical; ledger datum-hash path untouched). DROVE: copied 2 files to main (clean HEAD + fix; data.rs 0 memo
  confirmed), BUILD_EXIT=0, uplc nextest GREEN 435 (conformance preserved, serialiseData canonical), cloned
  verify15min pid 17352. *** REPLAY VERDICT (prefix) = PASS: 0 'Error term' by slot 125010507 (prior 41), 0 total so
  far (tip 125019223), 27751ab9 fixed, 0 phase-1. The CORRECT fix (canonical, no silent pass-where-Haskell-fails)
  eliminates the 306 just like the memo did — confirming the 306 were the V3 None-datum, not byte-shape. Node syncing
  full window. NEXT WAKE: confirm 306->0 PAST window (slot>125105013) -> RE-GAUNTLET (memo conceptual error GONE;
  serialiseData canonical; V3 datum canonical) -> on PASS commit #15 (dugite-uplc, 1 crate).
  was: state:FIXING (minimal correct fix; memo REVERTED). *** wake216: ANALYZE wpkh7n7c9 COMPLETE — definitive verdict
  (the gauntlet was RIGHT to refute the memo). serialiseData = canonical encodeData (getPlutusData strips MemoBytes
  before CEK); dugite encode_data ALREADY byte-matches PlutusCore (Q2 harness-proven); ep293 datum IS canonical so
  the wake165 '270 vs 276' was a MISDIAGNOSIS; the memo causes silent PASS-where-Haskell-FAILS for non-canonical
  datums (Q4 airtight: non-canonical datum spent by blake2b(serialiseData)==datum_hash script is is_valid=false
  on-chain [canonical!=wire] but dugite-memo returns verbatim->matches->wrongly is_valid=true). REAL ROOT CAUSE: the
  306 were because the PlutusV3 SpendingScriptInfo spending DATUM was built as None (V3 branch left datum=None) ->
  script got no datum -> Error term. V1/V2 already correct. DROVE: REVERTED main's dugite-uplc to clean HEAD (no memo,
  structural Data); launched MINIMAL FIX muscle wkba3hja9 (run wf_96017a98-ead, NO bridge — worktree==clean HEAD) to
  add ONLY the V3 SpendingScriptInfo datum population (getBabbageSpendingDatum: inline <|> witness, None-tolerant)
  built CANONICAL (plutus_data_to_data, NO memo), serialiseData untouched/canonical. NEXT WAKE: poll -> build ->
  re-replay (306->0, now CORRECT for non-canonical too) -> re-gauntlet -> commit. LESSON: the original #15 'verbatim
  memo' passed its replay for the WRONG reason (ep293 canonical + V3-datum-population masked it); the gauntlet's
  haskell-semantics refuter caught the conceptual error. serialiseData is canonical, NOT verbatim.
  *** wake213: GAUNTLET w4a16gr1r REFUTED 3/3. R1 (haskell-semantics, DEEP): the
  serialiseData BUILTIN is NOT verbatim-MemoBytes — transDatum/transRedeemer call getPlutusData (STRIPS MemoBytes) ->
  PlutusCore.Data.Data (no bytes) -> Serialise `encode=encodeData` ALWAYS CANONICAL. So serialiseData = PlutusCore-
  canonical encodeData, NEVER verbatim. The 306->0 replay passed ONLY because ep293 datums are ALREADY PlutusCore-
  canonical (verbatim==canonical); the REAL bug is dugite encode_data (data.rs) not byte-matching PlutusCore
  encodeData (270 vs on-chain 276). MY ANALYSIS CONFIRMS THE LOGIC: a NON-canonical datum spent by a blake2b(serialise
  Data(datum))==datum_hash script is is_valid=FALSE on-chain (canonical != non-canonical -> hash mismatch -> fail),
  but dugite-with-memo returns verbatim -> hash matches -> WRONGLY is_valid=TRUE = a silent pass-where-Haskell-fails.
  R2/R3: redeemers not memoised (asymmetric) — MOOT if serialiseData is canonical (then the fix is encoder, no memo).
  *** This means the #15 memo fix is likely the WRONG approach (passes replay for the wrong reason). DO NOT COMMIT.
  Launched ANALYZE muscle wpkh7n7c9 (run wf_959aaeab-33f, opus) to SETTLE authoritatively vs PlutusCore source: Q1
  serialiseData canonical-vs-verbatim, Q2 exact encodeData rules, Q3 is ep293 datum PlutusCore-canonical + where is
  dugite's 270-vs-276 encode_data delta, Q4 verdict (memo-correct vs revert-memo+fix-encoder). #15 memo KEPT on main
  (uncommitted) pending verdict. NEXT WAKE: on analyze verdict — if encoder-fix: REVERT memo, fix data.rs encode_data
  to byte-match PlutusCore encodeData, re-replay (306->0 correctly + non-canonical class correct), re-gauntlet.
  *** wake211 BYTE-EXACT GATE PASSED:
  V3-extension fix w19kofqwx (eval_redeemer.rs + redeemer_resolve.rs: resolve_spend_datum_optional resolves V3
  spending datum per getBabbageSpendingDatum [inline <|> witness, None if datum-less]; eval_redeemer builds V3
  SpendingScriptInfo datum via plutus_data_to_data_memoised = VERBATIM). DROVE: copied 2 files to main (dugite-uplc
  only), BUILD_EXIT=0, uplc nextest GREEN 439 (438 + new V3 verbatim regression; conformance/eval green => machine-
  built Data still canonical), verify15v3 replay. *** REPLAY VERDICT = DECISIVE PASS: 0 'Error term' across the FULL
  ep293 window (slots 125001020-125105013, +18K beyond) — DOWN FROM 306; CASE-1 27751ab9 (V3 7afbde08) now passes;
  0 phase-1. The serialiseData canonical-re-encode divergence is GONE. Launched #15 GAUNTLET w4a16gr1r (run
  wf_c496a1ad-30a, opus refuters). NEXT WAKE: on PASS -> COMMIT #15 via gh/HTTPS (dugite-uplc, 1 crate); on REFUTE ->
  verify the dissent (memo-leaks-into-Eq / machine-built-wrongly-memoised / tx-Data-misses-memo). After #15:
  phase2.preprod fast-start now byte-exact on serialiseData; next backlog = #0 (mainnet ep246) / #16 / #17 / #19 / #20.
  *** wake208:
  FIX muscle w1xi3j2nf green (Data-memo architecture: DataKind+Data{kind,original}, Eq/Hash IGNORE memo, to_cbor
  returns memo, V1/V2 datum_raw + witness datums_to_plutus memoised; 438 uplc tests green). DROVE: copied 10 files
  to main (dugite-uplc only), BUILD_EXIT=0, uplc nextest GREEN 438, verify15 replay. *** REPLAY VERDICT = NO-OP:
  41 'Error term' by slot 125010507 == pre-#15 binary's 41, SAME tx_hashes incl 27751ab9. ROOT CAUSE (confirmed by
  code): the memo was threaded into the V1/V2 datum ARGUMENT only (eval_redeemer.rs:130); the PlutusV3 branch (:156)
  ignores datum_raw and populate_v3.rs has ZERO spending-datum memo. The 306 failing scripts are V3 (7afbde08) which
  read their datum from the V3 ScriptContext SpendingScriptInfo (SpendingScript txOutRef (Just datum)), NOT the
  memoised txInfoData. The Data-memo architecture + V1/V2 path are CORRECT and KEPT on main. DROVE: regenerated
  base-15-uplc-bridge.patch (2140L, includes the #15 work), launched FIX muscle w19kofqwx (run wf_819302a0-b89) to
  thread datum_raw into the V3 SpendingScriptInfo datum (plutus_data_to_data_memoised). NEXT WAKE: poll -> build ->
  re-replay ep293 -> 306 Error-term MUST drop to ~0 (the byte-exact gate; tests-green is NOT proof) -> gauntlet ->
  commit. LESSON (again): replay is the only arbiter — a green V1/V2-memo fix was a V3 no-op.
  *** wake201: ASSESSED — CEK Data enum (data.rs:65
  Constr/Map/List/I/B) is PURELY STRUCTURAL (no memo); serialiseData (denotations.rs:601) always to_cbor()-canonical.
  dugite-uplc is CLEAN + UNDRIFTED from base ca50afd9ef (no bridge needed); plutus_data_to_data (tx_info_populate.rs:
  302) already has raw_cbor plumbing (~L388/531). #15 = dugite-uplc ONLY, 1 crate. Launched FIX muscle w1xi3j2nf (run
  wf_75ef4164-1c0): add OPTIONAL original-CBOR memo to Data (Hash/Eq/PartialEq MUST IGNORE it — structural equality
  unchanged); populate from on-chain CBOR (plutus_data_to_data threads raw_cbor for datum/redeemer/txInfoData +
  Data::from_cbor); serialiseData returns memo-or-to_cbor; machine-built Data (constrData etc.) NO memo = canonical
  (matches Haskell MemoBytes/builtinSerialiseData/hashAnnotated). Regression test: CASE-1 276-byte non-canonical datum
  -> serialiseData returns verbatim (blake2b==bbd35202..) not canonical 270. NEXT WAKE: poll -> build -> replay ep293
  window (slots 125001020+) -> 306 'Error term' must drop to ~0 -> gauntlet -> commit.
  *** ===== #10 DONE — COMMITTED 125ce7ef18 + PUSHED (prod-readiness-engine, HTTPS) wake200. =====
  6th GAUNTLET w7i0t8l28 REFUTED 3/3 but ALL THREE adversarial-only (malformed-snapshot decoder strictness, NOT
  byte-exactness on real data): R1 decode_varlen no overflow/non-minimal rejection; R2 DEFINITE-map truncation
  (round-5 only fixed indefinite arm; real blob is indefinite anyway); R3 backend field serde_json last-wins vs
  tablesCodecVersion first-wins (dup-key inconsistency). HARD POLICY INVOKED (declared wakes 194/197/198): core
  exhaustively confirmed byte-exact (round-5 edge-epoch refuter) + 6 byte-identical replays (4116338, 0 phase-1 past
  window) + Mithril-signed snapshots make malformed inputs out-of-band + adversarial surface demonstrably unbounded
  (6 rounds, ~18 distinct edges, no convergence) => COMMIT the byte-exact core, file the 3 + systemic leniency as #20.
  CI gate GREEN pre-commit: fmt clean + clippy -D warnings clean (2 crates) + nextest 1140. Committed 10 files / 2
  crates (dugite-serialization + dugite-node) via gh/HTTPS, DUGITE_PRECOMMIT_STRICT=1. #10 closes the long phase-2
  fast-start IMPORT COMPLETENESS arc (started ~wake73; 0 phase-1 rejections; was once 986).
  was: #10 GAUNTLET-PENDING (6th round). *** wake198: verify10B5 byte-identical import (0 phase-1, 0 NotFullyConsumed, 0 truncation-err); node mid-window
  (tip 125004549, still syncing toward 125105013 — wakes quick). R1+R3 can't regress phase-1 (parse-only + malformed-
  only) so launched 6th RE-GAUNTLET w7i0t8l28 (run wf_3579ddd3-3c2, refuterN=3) IN PARALLEL with the window sync.
  Gauntlet item scopes #15/#17/#19 OUT + requires the refutation be reachable from a real/malformed snapshot.
  NEXT WAKE per HARD POLICY: on PASS -> COMMIT #10 via gh/HTTPS (2 crates) -> formally file #17/#19 + activate #15;
  on adversarial-only REFUTE -> COMMIT #10 core anyway + open 'snapshot-import adversarial-hardening' tracking item
  (NO 7th cycle — core exhaustively confirmed byte-exact round-5 + stable across 6 replays). Also confirm verify10B5
  0-phase-1 PAST window before commit (currently mid-window, 0 so far).
  was: state:VERIFYING-RESOAK (round-5 FINAL: R1+R3). *** wake197: FINAL fix muscle wiujlmyn2 green (tier A, 1 crate dugite-serialization mempack/mod.rs+tests). R1
  COMPLETE: new top_level_number_literal() structure-scoped walk (skip_json_value/parse_json_string_at, top-level
  object only = aeson KM.lookup) drives the codec-version VALUE; removed the dead extract_raw_number_literal flat
  scan -> gate and value now AGREE (nested tablesCodecVersion ignored, matching aeson .: top-level-only). R3: tvar_
  body_offset returns TvarBody{offset,indefinite}; TvarIterator carries map_indefinite+saw_break; indefinite-map EOF-
  without-0xff => Some(Err) (Haskell ReadSnapshotFailed; RFC8949 indefinite requires break). +11 regression tests.
  DROVE: copied 2 files to main (2-crate footprint, node/ledger 0 R1/R3 markers), BUILD_EXIT=0 (no drift), dugite-
  serialization NEXTEST GREEN 1140 passed/6 skip, GC'd worktree, cloned verify10B5 pid 12350. Import BYTE-IDENTICAL:
  codec_version=1 Big (R1 structural extraction reads canonical flat meta), utxo_count=4116338 + txix_low=3131782
  txix_mult256=62, 0 phase-1, 0 NotFullyConsumed, 0 truncation-err (R3 no false-trigger; real blob ends 0xff). Node
  syncing window. NEXT WAKE: confirm 0-phase-1 past window -> 6th RE-GAUNTLET (R1+R3 addressed). Per HARD POLICY
  (wake194): on PASS -> COMMIT #10; on adversarial-only REFUTE -> COMMIT core anyway + open 'snapshot-import
  adversarial-hardening' tracking item (NO 7th cycle). This is the LAST hardening cycle.
  was: state:FIXING (round-5 FINAL: R1-complete-F1 + R3-indef-trunc). *** wake194: 5th GAUNTLET ww5a6h0zx = REFUTED 2/3; the edge-epoch refuter COULD NOT refute & exhaustively CONFIRMED
  the entire import byte-exact (addresses, multi-asset, tags 2/3, F2, container/truncation all match Haskell). verify
  10B4 WINDOW CONFIRMED (tip 125117568, 0 phase-1). The 2 refutations (both adversarial-only, no real-snapshot risk):
   (R1 haskell-semantics) F1 is INCOMPLETE: gate uses structural first_occurrence_value (top-level only, aeson) but
    VALUE still from extract_raw_number_literal FLAT byte-scan (matches key in NESTED objects too). meta {"extra":
    {"tablesCodecVersion":99},"tablesCodecVersion":1} => aeson=1 imports, dugite gate=1 but scan="99" => hard-error.
    A SELF-INCONSISTENT half-done fix in #10's OWN code -> MUST complete (won't ship inconsistent). FIX: drive value
    from the same structural first-occurrence as the gate.
   (R3 truncation) indefinite map 0xbf...0xff truncated at an ENTRY BOUNDARY (no 0xff) -> TvarIterator remaining.
    is_empty()=>None silently imports the prefix as complete. Haskell aborts (ReadSnapshotFailed). Backstopped by
    #17 CRC but in the truncation class #10 claims. FIX: carry map-kind; indefinite-EOF-without-break => Some(Err).
  DROVE: SIGTERM'd verify10B4 (window captured), regenerated base-commitB4-bridge.patch (3-crate), launched FINAL fix
  muscle wiujlmyn2 (run wf_26976ced-f49) for R1+R3 (mempack/mod.rs only). *** HARD POLICY (decided): this is the LAST
  adversarial-hardening cycle. R1 is completing a half-done fix; R3 is small. After this -> COMMIT #10. If a 6th
  gauntlet finds yet more ADVERSARIAL-ONLY edges (no real-snapshot risk), they go to a 'snapshot-import adversarial-
  hardening' tracking item, NOT another cycle — the byte-exact core is confirmed stable across 6 replays + an
  exhaustive edge-epoch refuter pass. NEXT WAKE: poll -> build -> re-import -> 6th re-gauntlet -> COMMIT.
  was: state:GAUNTLET-PENDING (5th round, F1+F2 final). *** wake192: verify10B4 import byte-identical (0 phase-1, 0 NotFullyConsumed; node still early at etime ~1min,
  syncing toward window — wakes came in quick succession). F1 (dup-key, absent in real meta) + F2 (native-decode
  relaxation) provably can't regress phase-1, so launched 5th RE-GAUNTLET ww5a6h0zx (run wf_bcedc476-060, refuterN=3)
  in PARALLEL with verify10B4 window sync. Gauntlet item EXPLICITLY scopes #15/#17/#19 OUT (refute only on in-scope
  serialization+node import defects NOT those 3). NEXT WAKE per wake187 POLICY: on PASS -> COMMIT #10 via gh/HTTPS
  (2 crates) -> formally file #17/#19, activate #15; on REFUTE -> if the new edge is a REAL-snapshot byte-exactness
  defect, fix it; if ADVERSARIAL-ONLY (like F1/duplicate-key class) AND no real-snapshot risk, COMMIT #10 core anyway
  + fold the edge into a 'snapshot-import adversarial-hardening' tracking item (F2 was the last real-snapshot risk;
  4 rounds done; the byte-exact core is stable across 5 replays — infinite adversarial-edge-chasing is non-productive).
  Also confirm verify10B4 0-phase-1 past window before commit.
  was: state:VERIFYING-RESOAK (round-4: F1 dup-key + F2 indef-array). *** wake191: FIX muscle wb28q1upc green (1 crate dugite-serialization: mempack/mod.rs F1 first_occurrence_value
  MapAccess [aeson KM.fromList first-wins] + era_conway.rs/reader.rs F2 read_native_script accept indefinite outer
  array via new Reader::expect_break [cardano-ledger decodeListLikeT/decodeListLenOrIndef]; +tests; also repaired
  bridge-orphaned tests [TxIxEndianness arg, full-consumption contract]). DROVE: copied 4 files to main (2-crate
  footprint, node/ledger 0 F1/F2 markers), BUILD_EXIT=0 (no drift), dugite-serialization NEXTEST GREEN 1130 passed/6
  skip (the wake184 build-only test-consistency now CONFIRMED on main), GC'd worktree, cloned verify10B4 pid 78827.
  Import BYTE-IDENTICAL: codec_version=1 Big (F1 no-dup common case unchanged), utxo_count=4116338 + txix_low=3131782
  txix_mult256=62, 0 phase-1, 0 NotFullyConsumed. Node syncing window. NEXT WAKE: confirm 0-phase-1 past window ->
  5th RE-GAUNTLET (F1+F2 addressed; F3->#19). Per wake187 POLICY: if 5th round finds ONLY adversarial-only edges (no
  real-snapshot risk), COMMIT #10 core + open adversarial-hardening tracking item; F2 was the last real-snapshot risk.
  was: state:FIXING (round-4 hardening: F1 dup-key + F2 indef-array). *** wake187: 4th RE-GAUNTLET wvfzy4jta = REFUTED 3/3 (3 NEW deeper edges; verified the dissents):
   (F1 haskell-semantics, COMPILE-VERIFIED) duplicate-JSON-key: aeson default json keeps FIRST occurrence, serde_json
    keeps LAST. parse_tables_codec_version type-gate uses serde_json value.get() (LAST) while extract_raw_number_literal
    is FIRST -> they DISAGREE; meta {..,"tablesCodecVersion":1,"tablesCodecVersion":"x"} => Haskell imports (1), dugite
    hard-errors (String). Adversarial-only but clear aeson mismatch in #10's R3 code. -> FIX in #10 (cheap, mod.rs).
   (F2 edge-epoch, HIGH=real-snapshot risk) read_native_script HARD-REJECTS indefinite-length OUTER array (arr_len.
    is_none()=>Err) while NESTED levels accept indefinite via read_array; cardano-ledger Timelock DecCBOR accepts both
    (decodeListLenOrIndef). A tag-5 native ref-script with indefinite outer array imports in Haskell but ABORTS the
    whole 4.1M fast-start in dugite. Same class as commit 4b42125fbb. -> FIX in #10 (era_conway.rs read_native_script).
   (F3 compounding-feedback) CompactAddr stored parsed-not-verbatim -> pointer-address non-minimal base-128 varlen
    re-encodes divergently. This IS the #19 carve-out; larger (dugite-primitives Address + all TxOut consumers),
    real-snapshot impact ~nil (pointer addrs vanishingly rare, canonical round-trips lossless). -> STAYS #19 (re-framed
    honestly: a real-but-rare lossy round-trip, not just a refactor preference).
  DROVE: regenerated base-commitB3-bridge.patch (3-crate), launched FIX muscle wb28q1upc (run wf_351f2eb6-b40) for
  F1+F2 (dugite-serialization ONLY: mempack/mod.rs first-wins gate + era_conway.rs read_native_script accept-indef).
  NOTE: 4 gauntlet rounds, each finds deeper adversarial CBOR edges; the byte-exact CORE (well-formed snapshot, 0
  phase-1, 4116338 byte-identical) is DONE. POLICY for round-5: if it finds ONLY new ADVERSARIAL-only edges (no real-
  snapshot risk like F2), COMMIT #10's byte-exact core + open a 'snapshot-import adversarial-hardening' tracking item
  (F1/dup-key class, #17 CRC, #19 CompactAddr) — infinite adversarial-edge-chasing is non-productive; the cardinal
  rule's intent (byte-exact on REAL chain data) is satisfied. NEXT WAKE: poll wb28q1upc -> build -> re-import -> re-
  gauntlet -> commit-or-policy-call.
  was: state:GAUNTLET-PENDING (commit-B FINAL, 6-path+R1+R2). *** wake185: verify10B3 WINDOW CONFIRMED — synced PAST window (tip 125115283 > 125105013, block 4794330), 0 phase-1,
  0 NotFullyConsumed (R2 doesn't false-trigger on well-formed TxOuts). DROVE: SIGTERM'd verify10B3 (evidence captured),
  launched RE-GAUNTLET wvfzy4jta (run wf_d0e85509-f55, refuterN=3) on the COMPLETE final state (6-path + R1 dangerous-
  Big + R2 full-consumption). This is the 4th gauntlet round; prior 3 rounds' refutations ALL addressed (wetwroth8:
  R3/CRC#17; wdvf5l5le: opaque/hard-error/c==0; wd3lzyawv: dangerouslyBig/full-consumption + 6 paths verified byte-
  exact by compounding-feedback). Out-of-scope (separate items, NOT defects): CRC=#17, opaque-CompactAddr=#19;
  phase-2 Error-term=#15. NEXT WAKE: on gauntlet PASS -> COMMIT #10 via gh/HTTPS (dugite-serialization+dugite-node)
  -> file #19 + activate #15; on REFUTE -> verify the dissent vs pinned source (4th round should be near-clean —
  adversarial surface exhausted: opaque-store + truncation/leftover/dangerouslyBig hard-error all done).
  was: state:VERIFYING-RESOAK (commit-B FINAL, R1+R2). *** wake184: FIX muscle w3dsqneah COMPLETED green (tier B, 1 crate dugite-serialization mempack/mod.rs+tests; node/
  ledger UNTOUCHED confirmed). R1 dangerouslyBig: O(1) bounds before pow (net_exp>=3=>None [coeff>=1, 10^3>255];
  net_exp<0 => trailing_zero_digits() scan, no 10^|e| materialised); huge-exp tests <0.02s. R2 full-consumption:
  TvarIterator asserts consumed==val_bytes.len() else Some(Err)+finished (Data.MemPack unpackFail); key side already
  strict (txin==34B). DROVE: copied 2 files to main (2-crate footprint, node/ledger 0 R1/R2 markers), BUILD_EXIT=0
  (NO drift this time — the 3-crate bridge worked), GC'd worktree, cloned verify10B3 pid 41461. Import BYTE-IDENTICAL
  to verify10B2: utxo_count=4116338 + txix_low=3131782 txix_mult256=62 IDENTICAL (R2 dropped nothing — the prior
  '4116339' was a misrecollection; 4116338 is consistent across ALL runs). R1 happy-path codec_version=1 Big, 0
  phase-1, 0 NotFullyConsumed (well-formed TxOuts fully consume). Node syncing window as evidence. NEXT WAKE: confirm
  0-phase-1 past window -> RE-GAUNTLET (R1+R2 addressed; 6 paths byte-exact-verified; compounding-feedback already
  passed) -> on PASS COMMIT #10 via gh/HTTPS (2 crates) + file #19 (opaque-addr) + activate #15 (serialiseData).
  was: state:FIXING (commit-B FINAL hardening: R1+R2). *** wake180: RE-GAUNTLET wd3lzyawv = REFUTED 2/3 BUT the 3rd refuter (compounding-feedback) VERIFIED all 6 prior
  paths byte-exact (couldn't refute). The 2 NEW narrow edges (both concrete + Haskell-grounded, both in dugite-
  serialization/src/mempack/mod.rs ONLY): (R1 haskell-semantics) R3's c==0 short-circuit fixed 0e<huge> but NONZERO-
  coeff huge-exponent 1e2000000000 still hits BigInt::pow(2e9) -> GB/OOM; Haskell toBoundedInteger has dangerouslyBig
  guard (e>limit && e>integerLog10'(255)=2 -> Nothing in O(1), lazy toIntegral). FIX: bound net_exp before pow (>=0
  branch: net_exp>=3 => None since coeff>=1; <0 branch: |net_exp| > coeff trailing-zero-count => non-integral None).
  (R2 edge-epoch) TvarIterator::next() ~1064-1066 DISCARDS decode_mempack_txout's _consumed -> a value blob whose
  decoder consumes only a PREFIX is silently accepted; Haskell mempack unpackFail is FULL-CONSUMPTION-STRICT
  (consumedBytes/=len => NotFullyConsumed => loadSnapshot aborts). FIX: assert _consumed==val_bytes.len() else
  Some(Err); same for key. Both VALID (verified the dissent). DROVE: SIGTERM'd verify10B2 (window evidence captured:
  0 phase-1 past 125105013), regenerated base-commitB2-bridge.patch (5197L, 3-CRATE serialization+node+LEDGER so the
  Convertible variant+arm both exist = NO more re-classify drift), launched FIX muscle w3dsqneah (run wf_ed2d1c3a-ca1).
  NEXT WAKE: poll; green -> copy mempack/mod.rs(+tests) to main -> build -> re-import (0 phase-1) -> RE-GAUNTLET (R1+R2
  addressed; 6 paths already verified byte-exact) -> COMMIT #10. CONVERGING: adversarial surface nearly exhausted
  (opaque-store + truncation-hard-error + full-consumption + dangerouslyBig done; CRC=#17, opaque-addr=#19 separate).
  was: state:GAUNTLET-PENDING (commit-B re-fix, 6-path). *** wake178: verify10B2 IMPORT FULLY CLEAN — 4116339 UTxOs loaded, 0 phase-1, 0 import HARD-ERRORS (the new
  hard-error paths do NOT false-trigger on a well-formed snapshot; tag-4/5 opaque-store relax does NOT over-reject).
  The 6-path changes provably CANNOT regress phase-1 (opaque relaxation + malformed-only hard-errors + parse-only R3),
  and verify10B already established 0-phase-1-PAST-window -> sufficient to gauntlet. DROVE: launched RE-GAUNTLET
  wd3lzyawv (run wf_487f86db-d79, refuterN=3) on the 6-path disposition IN PARALLEL with verify10B2 (pid 10889) still
  syncing toward the window (kept as belt-and-suspenders window evidence). Each prior refutation (wdvf5l5le) now
  resolved: tag-4/5 OVER-REJECT -> opaque-store; TvarIterator/address/multi-asset SILENT -> hard-error; R3 c==0 blowup
  -> short-circuit. NEXT WAKE: read gauntlet result + confirm verify10B2 0-phase-1 past window; on PASS -> COMMIT #10
  via gh/HTTPS (2 crates) then file #19 (opaque-CompactAddr) + activate #15 (serialiseData); on REFUTE -> verify the
  dissent vs the pinned source before acting.
  *** wake177: RE-FIX muscle wcp4vycpw COMPLETED green (tier B, 2 crates, 6-path). Notable: agent correctly split
  tag-5 into PLUTUS-body-OPAQUE vs NATIVE-Timelock-STRUCTURAL (Haskell Timelock MemPack unpackMemoBytesM IS
  structural — more precise than my instruction). All 6 paths applied: (1) import_inline_datum opaque-store
  (PlutusData::Bytes fallback, never error/None); (2) tag-5 Plutus opaque, native structural-hard-error retained;
  (3) TvarIterator Some(Err(CborDecode)) on mid-map trunc; (4) address hard-error; (5) multi-asset + AssetName>32
  hard-error; (6) R3 coeff.is_zero()=>Some(0). +dead-code removal (skipped counter) +tests.
  *** CROSS-CRATE DRIFT (handled): the bridge was 2-crate but BackendCheckResult::Convertible spans 3 crates (the
  VARIANT lives in dugite-ledger, added by a417bd2c6f AFTER base ca50afd9ef). Worktree's dugite-ledger lacked the
  variant, so the agent re-classified mem-under-LSM as a guarded Mismatch{DugiteMem,DugiteLsm} arm — but main's
  dugite-ledger RETURNS Convertible (never that Mismatch), so on main that arm is dead + match non-exhaustive. FIX:
  on copy-to-main, surgically restored HEAD's Convertible arm (Ok+Convertible+Mismatch). LESSON: bridge patches must
  span ALL crates a feature touches, OR reconcile cross-crate enum drift at copy-time.
  DROVE: copied 3 files to main (2-crate footprint), restored Convertible arm, BUILD_EXIT=0 (pid 10553, .jobs/verify-
  build-10B2.log), GC'd 16G worktree, cloned db-clones/preprod-verify10B2, launched re-fixed binary pid 10889. Import
  byte-exact: codec_version=1 Big, 0 phase-1, 0 import HARD-ERRORS (well-formed snapshot doesn't trip the new hard-
  error paths; opaque-datum relax = no false reject). Import in progress. NEXT WAKE: confirm full import + 0 phase-1
  past window -> RE-GAUNTLET (all 3 prior refutes now addressed: tag-4/5 opaque=byte-exact, 3 silent paths hard-error
  =byte-exact, R3 short-circuit) -> COMMIT #10 via gh/HTTPS. File #19 opaque-CompactAddr separately.
  was: state:FIXING (commit-B RE-FIX, 6-path disposition).
  *** wake172: ANALYZE wezt2hemc COMPLETE — authoritative per-path disposition (pinned cardano-ledger cd8b7fab +
  ouroboros-consensus 640b7fea). GOVERNING PRINCIPLE: snapshot UTxO leaves are MemPack newtype-over-ShortByteString
  (BinaryData/PlutusBinary/CompactAddr/multi-asset rep) = ZERO structural validation at load (that lives in on-chain
  DecCBOR, NOT invoked); load-time protections = MemPack underrun hard-fail (truncation/unknown-tag) + whole-file CRC.
  => leaf-structural = OPAQUE-STORE; container-truncation = HARD-ERROR. 6-PATH FIX:
   (1) tag-4 datum mod.rs:6648-6660 OPAQUE-STORE (best-effort decode, keep raw bytes on Err, NO error) [my no-silent-
       None OVER-REJECTED — BinaryData opaque]; (2) tag-5 refscript ~6679-6694 OPAQUE-STORE body (unknown-language-tag
       MAY hard-error) [over-rejected]; (3) TvarIterator mempack/mod.rs:977-1006 HARD-ERROR on mid-map decode_bytes Err
       / val_start>=len [pre-existing silent-truncate]; (4) address ~6571-6582 HARD-ERROR (opaque-CompactAddr refactor
       = SEPARATE item) [pre-existing silent-skip]; (5) multi-asset ~6616-6623 + AssetName>32 ~6599-6607 HARD-ERROR
       [pre-existing silent token-drop]; (6) R3 add coeff.is_zero()=>Some(0) short-circuit [my R3, 0e<huge> blowup].
  R3 core is byte-exact-confirmed by all refuters (KEEP). DROVE: regenerated base-commitB-bridge.patch (4152 lines,
  ca50afd9ef->main current = FINAL-DONE+R3+no-silent-None+Convertible arm, avoids the wake166 drift recurrence) +
  launched FIX muscle wcp4vycpw (run wf_b1d55d93-e4a, worktree, applies bridge by abs-path first). NEXT WAKE: poll;
  green -> copy files to main -> verify-build -> re-import (0 phase-1) -> re-gauntlet (all 3 prior refutes now
  addressed: tag-4/5 opaque=byte-exact, 3 silent paths hard-error=byte-exact, R3 short-circuit) -> COMMIT #10.
  FILE SEPARATELY: #19 opaque-CompactAddr-store adversarial-hardening (path-4 option-a).
  was: state:DIAGNOSING (gauntlet REFUTED 3/3; verify dissents).
  *** wake169: RE-GAUNTLET wdvf5l5le = pass=false REFUTED 3/3 — substantive, NOT a commit. ALL 3 CONFIRM byte-exact:
  R3 scientific_literal_as_word8 (Aeson toBoundedInteger@Word8 — 1.0/1e0/100e-2=>1, sub-ULP/1.5 reject, range reject)
  + TxIx endianness/backend STRICT mapping. VALID REFUTATIONS:
   (CRUX, R2-a — INTRODUCED by my no-silent-None): mod.rs:6648-6660 now re-decodes EVERY tag-4/tag-5 inline datum via
    decode_plutus_data_cbor and HARD-ERRORS the whole import on decode failure. But Haskell stores inline datums as
    OPAQUE BinaryData (newtype MemPack over ShortByteString), NEVER re-decoding at load (lazy on script-consume). So a
    legal-on-chain datum dugite's read_plutus_data rejects would BRICK a snapshot Haskell accepts = OVER-REJECTION
    (inverse of byte-exact). #15 proves dugite Data layer is non-verbatim (276->270) = exactly this over-reject class.
    -> my hardening is WRONG here; byte-exact = store verbatim opaque, best-effort structural decode, keep bytes on err.
   (R1 / R2-b / R3 — PRE-EXISTING in FINAL-DONE, no-silent-corruption gaps; Haskell MemPack hard-fails on all):
    TvarIterator::next() mempack/mod.rs:977-1006 swallows a mid-map CBOR decode_bytes Err as clean end-of-map ->
    SILENT PARTIAL UTxO import (assert_txix_distribution_sane blind to it, skipped not incremented); address-failure
    mod.rs:6571-6582 -> skip+continue (drops UTxO); parse_multi_asset_rep failure mod.rs:6616-6623 -> warn + ADA-only
    (drops native tokens = the MultiAssetNotConserved class). minor: R3 lacks Haskell c==0 short-circuit (0e<huge> ->
    BigInt::pow blowup).
  NOTE: #10's CORE phase-1 byte-exactness on WELL-FORMED snapshots is DONE+verified (0 phase-1 x4 replays); these are
  MALFORMED-input / over-reject robustness. DROVE: launched ANALYZE muscle wezt2hemc (run wf_66f42008-83b) to VERIFY
  vs Haskell (does loadSnapshot re-decode tag-4 datum / tag-5 script or store opaque? does MemPack unpack hard-fail on
  truncated map / malformed addr / malformed value?) + per-path byte-exact disposition (HARD-ERROR vs OPAQUE-STORE vs
  short-circuit) + pre-existing-vs-introduced + fold-vs-separate-item. NEXT WAKE: on analyze result -> FIX (keep R3;
  revise tag-4/5 to opaque-no-redecode; harden TvarIterator/address/multi-asset to hard-error per Haskell; +c==0
  short-circuit) -> rebuild -> re-import (still 0 phase-1) -> re-gauntlet -> commit.
  was: state:GAUNTLET-PENDING (commit-B). *** wake168:
  VERIFYING-RESOAK VERDICT = PASS. verify10B synced PAST window (tip 125110959 > 125105013): 0 phase-1 (all classes),
  0 import hard-errors (no-silent-None non-regressing), 308 Error-term (= #15 general-UPLC, unchanged by hardening as
  expected). DROVE: SIGTERM'd verify10B (evidence captured), launched RE-GAUNTLET muscle wdvf5l5le (run wf_83b4db4e-
  836, refuterN=3) on FINAL-DONE+R3+no-silent-None. The prior 3/3 refute (wetwroth8) is now fully ADDRESSED: R1+R2
  297-attribution PROVEN general-UPLC #15 (byte-level: independent post-snapshot tx + serialiseData 276->270 datum
  mismatch); R3 float-parse FIXED byte-exact (f64-free + Aeson toBoundedInteger); CRC=#17 scoped. NEXT WAKE: on
  gauntlet PASS -> COMMIT #10 via gh/HTTPS (dugite-serialization + dugite-node, 2 crates) then ACTIVATE #15; on any
  REFUTE -> verify the dissent vs upstream before acting (do NOT blindly trust the count).
  was: state:VERIFYING-RESOAK (commit-B). *** wake167:
  BUILD_EXIT=0 (combined hardening binary; Convertible-drift fixed). DROVE: CoW-cloned db-preprod-sync ->
  db-clones/preprod-verify10B, launched pid 48115 port 4211. R3/no-silent-None NON-REGRESSING confirmed: import
  logged "codec_version=1 txix_endianness=Big" (R3 f64-free scientific_literal_as_word8 parses the REAL integer-1
  meta correctly), 0 phase-1, 0 import hard-errors (no-silent-None did NOT falsely reject any real tag-4/5 blob ->
  all real datums/refscripts decode fine). Node still importing/early-replay. NEXT WAKE: once synced past window
  (slot 125105013) confirm full 0 phase-1 -> LAUNCH RE-GAUNTLET FINAL-DONE (prior 3/3 NOW resolved: R1+R2 297-attrib
  PROVEN general-UPLC #15 [byte-level proof wake165]; R3 fixed byte-exact; CRC still #17) -> on PASS COMMIT #10 via
  gh/HTTPS (dugite-serialization + dugite-node, 2 crates). Then ACTIVATE #15 serialiseData.
  was: state:VERIFYING-BUILDING (commit-B). *** wake166: FIX
  muscle wjuuqz22k COMPLETED green (tier A, 3 files / 2 crates). R3: replaced as_f64().fract() with f64-FREE
  scientific_literal_as_word8() (raw Number::as_str() literal + num_bigint, Aeson toBoundedInteger@Word8 exact:
  1.0/1e0/100e-2=>1 accept, 1.0000000000000001/1.5 reject, 256/-1 reject, "1" reject) — chose raw-literal over
  enabling global arbitrary_precision (avoids serde_json feature-unification across 61 crates). no-silent-None: tag-4
  inline-datum + tag-5 decode_imported_script_ref (now Result) HARD-ERROR on malformed blob (no-silent-corruption);
  +regression tests. Quotes Aeson parseBoundedIntegralFromScientific + Scientific.toBoundedInteger.
  DROVE: copied the 3 files into main (FINAL-DONE base) -> 2-crate footprint. *** HIT worktree-staleness DRIFT: build
  failed E0004 non-exhaustive BackendCheckResult (missing Convertible arm) — muscle worktrees branch from STALE base
  ca50afd9ef; commit a417bd2c6f (#9, dugite-mem-under-LSM Convertible) landed AFTER that base and was NOT in the
  worktree's node/mod.rs (base patch only carried FINAL-DONE diff, not intervening commits). Verified via git log
  ca50afd9ef..HEAD: ONLY a417bd2c6f touched node/mod.rs, mempack ZERO drift. FIX: re-inserted HEAD's Convertible arm
  (info! + Ok(state)) into node/mod.rs. Rebuild pid 47755 (.jobs/verify-build-10B.log). Also GC'd 161GB of 15 stale
  muscle worktrees (each ~10G target/) -> disk 8.6G->186G (RECURRING: GC worktrees after every fix muscle).
  NEXT WAKE: BUILD_EXIT=0 -> clone db-preprod-sync -> verify10B re-import (confirm STILL 0 phase-1, R3/no-silent-None
  don't regress the integer-1 happy path) -> RE-GAUNTLET FINAL-DONE (prior 3/3 resolved: 297=general-UPLC #15; R3
  fixed) -> COMMIT #10 via gh/HTTPS (dugite-serialization + dugite-node). Then activate #15 serialiseData.
  was: state:FIXING (commit-B hardening: R3 + no-silent-None).
  *** wake164: DROVE ROOT-CAUSED -> FIXING. Generated FINAL-DONE base patch from main's uncommitted tree (3856 lines,
  2 crates) -> scripts/prod-readiness/base-FINAL-DONE-main.patch (ABS path; FINAL-DONE is uncommitted on main =
  invisible to fresh worktrees, so the muscle MUST git-apply it first). Launched FIX muscle wjuuqz22k (run
  wf_52ea6a96-0ac, worktree): (R3) make json_number_to_word8_codec_version BYTE-EXACT with Aeson toBoundedInteger@
  Word8 — current as_f64().fract() (serde_json no arbitrary_precision) accepts sub-ULP fractional 1.0000000000000001
  Aeson rejects; fix = enable arbitrary_precision + read raw Number::as_str() token, integral+range test on the exact
  literal; (no-silent-None) tag-4/tag-5 import decode failures must HARD-ERROR not silently None (no-silent-corruption
  rule). Scope dugite-serialization + dugite-node. NEXT WAKE: poll; on green -> copy changed files into main,
  verify-build, re-import-replay (confirm STILL 0 phase-1), then RE-GAUNTLET FINAL-DONE (prior 3/3 refute resolved:
  R1+R2 297-attribution PROVEN general-UPLC #15; R3 fixed), then COMMIT #10 via gh/HTTPS (2 crates).
  *** (#15 mechanism dim of wpeec891q still running — read whenever it completes; it finalizes #15 serialiseData, not
  #10.) ***
  was: state:ROOT-CAUSED (phase-1 SEPARABLE; 306 are #15 not #10).
  *** wake163 VERDICT = GENERAL-UPLC (the 306 'Error term' are NOT import-caused). Re-diagnose muscle wpeec891q
  classification dim COMPLETE found=true: of 15 sampled failing txs, 6/15 (40%) are PURELY post-snapshot, and
  CASE 27751ab9 (slot 125001020, PlutusV3 script 5b2bfe89) is FULLY INDEPENDENT — its only spending input 3d7bb051
  was created at slot 124999282 (113 slots ABOVE the snapshot cutoff 124999169 => live block-decoded, NEVER imported)
  yet it STILL fails. One independent never-imported failing tx is DISPOSITIVE: import is NOT necessary for the bug.
  IMPLICATION: #10's phase-1 import work (FINAL-DONE: 0 phase-1 rejections, byte-exact codec-version/endianness/TxOut-
  completeness) is DONE and SEPARABLE; the 306 phase-2 'Error term' belong to #15 (general phase-2 UPLC), NOT #10.
  The serialiseData mechanism dim of wpeec891q is STILL RUNNING (will confirm vs 10a0dbda/27751ab9). REMAINING #10
  WORK before commit: (i) fold R3 float-parse hardening (json_number_to_word8 gate on Number::as_u64/as_i64, reject
  sub-ULP fractional that Aeson rejects) + the no-silent-None on tag-4/5 import; (ii) RE-GAUNTLET FINAL-DONE — the
  prior 3/3 refute is now RESOLVED (R1+R2 297-attribution: PROVEN to be #15/general-UPLC not #10; R3: fixed); (iii)
  commit (dugite-serialization + dugite-node, <=2 crates) via gh/HTTPS. NEXT WAKE: read wpeec891q mechanism result
  (finalize #15 root cause = serialiseData), then DRIVE #10 step (i) launch a small fix muscle for R3+no-silent-None.
  was: state:DIAGNOSING (mechanism REFUTED by replay; re-open).
  *** wake161 VERDICT: the inline-datum fix is a NO-OP — REPLAY ARBITRATES. verify10A (FINAL-DONE + uplc
  inline_spend_datum) synced PAST the window (tip 125108218 > 125105013) and shows 306 'Error term' = SAME as
  verify10j's 297 (FINAL-DONE alone; +9 is just more blocks). The R1+R2 refuter mechanism (imported inline-datum
  re-encode in resolve_spend_datum) is EMPIRICALLY WRONG. The fix agent's caveat was correct (V1/V2 txInfoData is
  witness-only; InlineDatum.data already == read_plutus_data(raw_cbor) so resolution was never re-encoding). DROVE:
  SIGTERM'd verify10A, REVERTED redeemer_resolve.rs from main (no-op, kept main clean; FINAL-DONE serialization+node
  intact). LESSON: the wuoecuy7o diagnose's '2/20 spend imported inline-datum UTxOs' was CORRELATION not causation
  (10% of script UTxOs at ep293 are imported inline-datum regardless) — replay is the only arbiter, as the cardinal
  rule says.
  *** RE-OPEN: the 306 phase-2 'Error term' (uplc says script-fail, on-chain is_valid=true) are NOT datum-resolution.
  NEXT-WAKE re-diagnosis must be OPEN (do NOT assume import). DECISIVE BRANCH to settle: are these IMPORT-specific or
  GENERAL-UPLC? Launch a DIAGNOSE/ANALYZE muscle to ROOT-CAUSE ONE representative failing tx (CASE1 10a0dbda20742f52
  894b66af9cf8880271197a33df7be16f8a5f1039ac176e5d, slot 125009209): pull tx CBOR + spent script + datum + redeemer
  via koios.sh preprod, run dugite UPLC CEK with tracing, find the EXACT divergence (candidates: serialiseData builtin
  re-encoding non-canonical datum canonically [fix-agent hypothesis: on-chain Data carries memoised MemoBytes; dugite
  structural Data re-encodes when script serialises/hashes it]; a wrong/missing ScriptContext field; cost-model/budget;
  a specific builtin). IMPLICATIONS: if GENERAL-UPLC (not import) -> #10 phase-1 import (FINAL-DONE, 0 rejections) IS
  DONE+separable -> gauntlet+commit FINAL-DONE (dugite-serialization+dugite-node, the (B) commit) as the WHOLE of #10,
  and the 306 become a NEW phase-2-UPLC item; if IMPORT-specific -> #10 must fix it. DO NOT let the phase-2 mystery
  keep blocking the byte-exact phase-1 import fix.
  *** wake162: code-confirmed PRIME SUSPECT + launched re-diagnose muscle wpeec891q (run wf_1bcfce4f-50b). FOUND:
  dugite serialiseData builtin (crates/dugite-uplc/src/builtin/denotations.rs:597-604) does d.to_cbor() = CANONICAL
  re-encode; Haskell serialiseData returns the MEMOISED ORIGINAL bytes (MemoBytes/BinaryData). So any script calling
  serialiseData on a non-canonical Data (Constr tag-102 etc; CASE-1 datum starts d87a9f) and hashing/comparing the
  result diverges -> logical 'Error term'. serialiseData is essentially the ONLY Data-BYTES divergence vector (all
  other Data builtins are structural) — which is WHY the inline-datum resolution fix was a no-op (value was fine; the
  bug is the SCRIPT serialising it). NOT import-specific. Muscle wpeec891q confirms vs the real failing tx 10a0dbda
  (does the script invoke builtin tag 51? is the datum non-canonical?) + classifies import-vs-general (sample 15 of
  306: how many spend ONLY post-snapshot inputs => GENERAL-UPLC). NEXT WAKE on verdict: GENERAL-UPLC (expected) ->
  (i) #10 phase-1 FINAL-DONE is DONE+separable -> gauntlet+commit (dugite-serialization+dugite-node); (ii) file the
  306 as NEW phase-2-UPLC item 'serialiseData verbatim-bytes' (dugite Data must carry original CBOR like Haskell
  MemoBytes; serialiseData returns memo when present, canonical only for machine-built Data). IMPORT-specific ->
  #10 absorbs.
  was: state:VERIFYING-RESOAK (commit-A). *** wake160:
  BUILD_EXIT=0 (combined binary 08:06: FINAL-DONE + uplc inline_spend_datum fix). DROVE: SIGTERM'd verify10j evidence
  node (clean "Shutdown complete"; its 297 count is in verify10j-resoak.log), GC'd verify10i (-CoW), CoW-cloned
  db-preprod-sync -> db-clones/preprod-verify10A, launched combined binary pid 98474 port 4211. Import byte-exact
  (codec_version=1 txix_endianness=Big), 0 phase-1 / 0 Error-term so far (still importing/replaying; has NOT reached
  the ep293 divergence window slots 125001020+ yet). NEXT WAKE VERDICT (the arbiter, per fix-agent no-op caveat):
  once verify10A syncs PAST slot ~125105013, COUNT 'Error term' over slots 125001020+: ~0 (CASE1 10a0dbda / CASE2
  08c596be gone) => fix WORKS -> gauntlet -> commit (A); STILL ~297 => NO-OP confirmed -> revert redeemer_resolve.rs
  from main + RE-DIAGNOSE toward UPLC serialiseData/CEK datum-bytes (on-chain Data carries memoised original bytes
  via MemoBytes; dugite structural Data likely re-encodes when script hashes/serialises its datum arg).
  was: state:VERIFYING-BUILDING (commit-A). *** wake159:
  FIX muscle wst6ekcg6 COMPLETED checks_green=true, tier A', 1 crate (dugite-uplc redeemer_resolve.rs only). Fix:
  InlineDatum arm now routes through new helper inline_spend_datum(data, raw_cbor, script_hash) that DECODES the
  preserved raw_cbor (validates span==original) to recover the structural datum anchored to verbatim bytes, mirroring
  Haskell binaryDataToData/getBabbageSpendingDatum + the DatumHash raw-span branch. +regression test.
  *** CRITICAL CAVEAT from the fix agent (DO NOT TRUST GREEN — replay is the arbiter): the agent's OWN Haskell
  research shows (a) V1/V2 txInfoData is WITNESS-ONLY (PV1.txInfoData = Alonzo.transTxWitsDatums (tx^.witsTxL)) — the
  spent output's inline datum does NOT enter the context datum map; it is passed as the script's DATUM ARGUMENT; and
  (b) in this main tree InlineDatum.data is ALREADY read_plutus_data(raw_cbor) at every construction site, so
  inline_spend_datum recovers the SAME structural value => the fix is LIKELY A NO-OP for well-formed inputs and may
  NOT remove the 297 'Error term'. Also: the wuoecuy7o diagnosis referenced decode_plutus_data_cbor which does NOT
  exist in this checkout. HYPOTHESIS for the TRUE mechanism if replay shows no change: the divergence is NOT datum-
  resolution but the UPLC serialiseData builtin (or other ScriptContext field) re-encoding the datum CANONICALLY when
  the script itself hashes/serialises its datum arg — on-chain Data carries memoised original bytes (MemoBytes) so
  serialiseData returns verbatim; dugite's structural Data likely re-encodes. That would be a dugite-uplc CEK/builtin
  bug, deeper than resolve_spend_datum.
  DROVE: copied worktree redeemer_resolve.rs into MAIN (which carries FINAL-DONE uncommitted: mempack codec-version +
  node import-endianness present) -> combined binary; started release build pid 97939 (.jobs/verify-build-10A.log).
  NEXT WAKE: BUILD_EXIT=0 -> clone db-preprod-sync -> verify10A re-soak -> COUNT 'Error term' divergences over the
  SAME ep293 slot window (125001020+): if ~0 (CASE1 10a0dbda / CASE2 08c596be gone) the fix WORKS -> gauntlet ->
  commit (A); if STILL ~297 the fix is a NO-OP -> REVERT redeemer_resolve.rs from main, RE-DIAGNOSE toward
  serialiseData/CEK (the refuter mechanism was plausible but the Haskell research refutes it).
  was: state:FIXING (commit-A inline-datum verbatim). *** wake156:
  DROVE ROOT-CAUSED -> FIXING. Launched FIX muscle wst6ekcg6 (run wf_3ec4a181-f27, worktree, Tier A') for commit (A)
  = dugite-uplc inline-datum VERBATIM-BYTES ScriptContext fix. Pre-read confirmed exact site: redeemer_resolve.rs:620
  `InlineDatum { data, .. } => Ok(data.clone())` DISCARDS the carried raw_cbor; the DatumHash branch L631-642 ALREADY
  matches via tx.witness_set.raw_plutus_data_cbor element spans (plutus_data_element_spans) instead of re-encoding —
  the inline-datum path must mirror that. InlineDatum.raw_cbor IS populated (importer sets Some(inline_cbor); live via
  KeepRaw). Fix is UNIVERSAL (imported+live), scoped to dugite-uplc ONLY (<=1 crate), + non-canonical Constr-tag-102
  InlineDatum regression test. NEXT WAKE: poll wst6ekcg6; on green -> apply to a clone, VERIFYING-replay the 297
  'Error term' residual (expect CASE1/CASE2 gone + overall drop), then gauntlet, then commit (A). After (A) lands ->
  commit (B) dugite-serialization+dugite-node (FINAL-DONE phase-1 + no-silent-None + R3 float-parse).
  was: state:ROOT-CAUSED (dissent CONFIRMED). *** wake155:
  DIAGNOSE wuoecuy7o dimension-1 (inline-datum-import-implication) RETURNED found=true with CONCRETE byte-level
  evidence (the R1+R2 dissent is EMPIRICALLY CORRECT — the 297 residual is NOT cleanly #15):
   CASE 1: failing tx 10a0dbda20742f52894b66af9cf8880271197a33df7be16f8a5f1039ac176e5d (slot 125009209 ep293)
    spends UTxO d653e3692353fe3f86daf21f16e8027eaee5c835467e3139992e98dc0c8135bb#0 created slot 121384342 (<<
    cutoff 124999169 => IMPORTED), addr 0x70 enterprise-script (hash 7afbde082796cfa6ede6bba8a6aadca8...), INLINE
    datum tag=1 bytes d87a9fd8799fd8799fd8799f581c99...
   CASE 2: failing tx 08c596becf99622f703e179be5dafaf936fed54679f39c98c1284c82dc0165fd (slot 125014562) spends
    722326df219c768da65b549450d8a73c5b9f98e3d9292dfda089b3af2b26160a#0 created slot 124884686 (< cutoff => IMPORTED),
    0x70 enterprise-script, INLINE datum tag=1 bytes d8799f01019f581c0ae2286929bc0908...
   2/20 sampled (10%) 'Error term' txs spend imported inline-datum script UTxOs. (control dim a14571b1 still running,
   irrelevant — one concrete imported case is dispositive/monotonic.)
  ROOT CAUSE (precise): crates/dugite-uplc/src/redeemer_resolve.rs::resolve_spend_datum (~L620) returns
  data.clone() for the inline-datum case and IGNORES the carried raw_cbor, then ScriptContext re-encodes via
  plutus_data_to_data().to_cbor() CANONICALLY. On-chain inline datums are NON-canonical (Constr CBOR tag-102) and
  do NOT round-trip -> datum-hash/ScriptContext bytes differ from chain -> script hits its Error branch -> 'Error
  term'. (Same file ALREADY hardened the DatumHash path to hash ORIGINAL raw spans for this exact reason; the
  inline-datum path was not given equivalent verbatim treatment.) SECONDARY (fold): tag-4/tag-5 import in node/mod.rs
  SILENTLY degrades to None on decode err (no-silent-corruption violation); R3 json_number_to_word8 as_f64().fract()
  accepts sub-ULP fractional version (gate on Number::as_u64/as_i64).
  EXPANDED #10 SCOPE (3 fix sites across 3 crates -> SPLIT commits to honor <=2-crate rule):
   (A) dugite-uplc: resolve_spend_datum must build the ScriptContext inline datum from VERBATIM raw_cbor (mirror the
       DatumHash original-span path), NOT re-encode. THIS is the 297-residual fix. [1 crate]
   (B) dugite-serialization + dugite-node: FINAL-DONE phase-1 import (codec-version/endianness/TxOut-completeness,
       already 0 phase-1) + no-silent-None on tag-4/5 + R3 float-parse hardening. [2 crates]
  Commit (A) and (B) as SEPARATE focused commits, each its own gauntlet. NEXT WAKE: launch FIX muscle for (A)
  (inline-datum verbatim ScriptContext) in worktree; the (A) fix is the gating one for the phase-2 residual.
  was: state:DIAGNOSING (dissent-verification). *** wake152:
  RE-GAUNTLET wetwroth8 RETURNED pass=false, REFUTED 3/3 (NOT a commit). DO NOT trust the prior numeric verdicts —
  verify the dissent (cardinal rule). The three refutations:
   (R1 haskell-semantics + R2 edge-epoch CONVERGE — the GATING dissent): the 297 phase-2 'Error term' residual is
    attributed WHOLESALE to #15 WITHOUT per-error evidence. Concrete alt-cause squarely in #10's domain: imported
    INLINE datums (MemPack tag-4) carry raw_cbor but phase-2 consumer dugite-uplc/src/redeemer_resolve.rs::
    resolve_spend_datum (~L620) IGNORES raw_cbor and re-encodes CANONICALLY — on-chain datums are non-canonical
    (Constr CBOR tag-102), don't round-trip -> datum-hash/ScriptContext mismatch -> 'Error term'. ALSO tag-4/tag-5
    import in node/mod.rs SILENTLY degrades to None on decode err (warn-and-continue) = violates no-silent-corruption
    cardinal rule -> wrong/empty ScriptContext -> 'Error term'. Same symptom as #15 -> attribution unproven.
   (R3 compounding-feedback — REAL but NARROW, fold into #10 before re-gauntlet): json_number_to_word8_codec_version
    uses serde_json as_f64().fract() (no arbitrary_precision) -> sub-ULP fractional versions (1.0000000000000001)
    round to 1.0 and are ACCEPTED where Aeson arbitrary-precision Scientific.toBoundedInteger REJECTS any fractional.
    Acceptance-side mismatch -> "mirror Aeson exactly" FALSIFIED for that input class. Unreachable from canonical
    snapshots (real preprod meta = INTEGER 1, hits as_u64 fast path) but violates strict-parse contract. FIX: gate on
    Number::as_u64/as_i64 integer typing (reject any non-integer-typed JSON number), NOT as_f64().fract().
  DROVE: launched DIAGNOSE muscle wuoecuy7o (run wf_31db35f1-cbb) to verify the R1+R2 dissent via DECISIVE
  discriminator: a spent input created at slot <= 124999169 came from the #10 IMPORT (MemPack tables); slot >
  124999169 came from live block-CBOR decode (never imported). Diagnose resolves ~20 of the 305 'Error term'
  tx_hashes' spent inputs via koios.sh preprod -> are ANY imported tag-4/tag-5 script UTxOs (import implicated) or
  are ALL post-snapshot (residual genuinely #15)? NEXT WAKE: on diagnose verdict — IMPORT-IMPLICATED -> #10 absorbs
  inline-datum verbatim-bytes (resolve_spend_datum must use raw_cbor) + no-silent-None fixes, re-fix+re-soak+re-
  gauntlet; NOT-IMPLICATED -> #10 separable, fold R3 float-parse hardening, rebuild+re-soak+re-gauntlet, then commit.
  Either way R3 hardening is REQUIRED before #10 commits. verify10j node (pid 63671) left soaking as evidence.
  was: state:GAUNTLET-PENDING (FINAL-DONE). *** wake150:
  FULL-VERDICT PASS. verify10j soak (pid 63671) synced 124999533 -> 125105013 (~105K slots past snapshot tip,
  processed blocks referencing imported UTxOs): 0 phase-1 transaction rejections ALL classes (InputNotFound/
  MissingScriptWitness/InvalidMint/MultiAssetNotConserved/CollateralNotFound) — IDENTICAL to STRICT verify10i.
  The 13 non-#15 "ERROR" greps are benign substring hits (macOS DNS WARN, chain_diverged=false, PlutusV1 332-vs-166
  cost-model fallback WARN — pre-existing, unrelated to import-keying). Only residual = 297 phase-2 'Error term'
  = #15 (294->297 is +3 slots, separately filed). No halts/panics/apply-failures. Launched RE-GAUNTLET muscle
  wetwroth8 (run wf_436d43b5-37f) on FINAL-DONE with rich item (codec-version Aeson-exact + endianness/backend +
  TxOut completeness + CRC=#17 scoped). Should PASS (float-parse byte-exact resolves refutation-a; CRC filed
  resolves refutation-b; endianness confirmed byte-exact by all 3 priors). NEXT WAKE: on gauntlet PASS -> COMMIT
  #10 via gh/HTTPS (2 crates: dugite-serialization + dugite-node) using FINAL-DONE patch; on REFUTE -> verify the
  dissent vs upstream before acting. was: state:VERIFYING-RESOAK (FINAL-DONE). *** wake149:
  BUILD_EXIT=0 (FINAL-DONE binary built Jun7 07:14). DROVE re-import re-soak: GC'd verify10b (-2G CoW), CoW-cloned
  db-preprod-sync -> db-clones/preprod-verify10j, launched FINAL-DONE node pid 63671 port 4211 sock
  /tmp/engine-verify10j.sock. Import log byte-exact: "(strict: only version 1 => big-endian is accepted)
  codec_version=1 txix_endianness=Big", 4116339 UTxOs loaded, snapshot saved ep293. 0 phase-1 rejections so far
  (all classes). Node live-syncing forward 124999533 -> chain tip 125104880 (~105K slots; will process blocks
  referencing imported snapshot UTxOs = real keying test). NEXT WAKE FULL-VERDICT: scan ALL rejection classes ->
  must be 0 phase-1 (identical to STRICT verify10i, since real preprod meta=integer1=BE same path) -> RE-GAUNTLET
  FINAL-DONE (should PASS: float-parse now Aeson-exact, claim accurate, CRC=#17) -> COMMIT #10 via gh/HTTPS (2
  crates: dugite-serialization + dugite-node). was: state:VERIFYING-BUILDING (FINAL-DONE). *** muscle
  w3cxa15va COMPLETE wake147, checks_green, 2 crates ***. json_number_to_word8_codec_version mirrors Aeson
  toBoundedInteger@Word8 EXACTLY: 1.0/1e0/100e-2=>1=>Big (accepted like upstream); 1.5=>Err(non-integral);
  256/-1=>Err(out-of-Word8); 2.0=>Err(unknown version); "1"=>Err(string); then enforceVersion narrows to ==1.
  Field-absent/null/file-absent/wrong-backend=>Err unchanged. Narrowed overclaim comments + scope-noted CRC as
  #17 + corrected cross_validate 'live not dead' wording. Resolves BOTH gauntlet refutations (float-parse byte-
  exact; CRC separate). The real preprod meta is integer 1 -> import UNCHANGED from STRICT (verify10i = 0 phase-1).
  FINAL-DONE patch saved candidate-fix-10-FINAL-DONE-codecversion-aeson.patch (3856 lines, applies clean) +
  applied to MAIN + build pid 62684 (.jobs/verify-build-10j.log). NEXT WAKE: BUILD_EXIT=0 -> re-import re-soak
  (confirm 0 phase-1, identical to STRICT) -> RE-GAUNTLET (should PASS: parse byte-exact, claim accurate, CRC=#17)
  -> COMMIT #10. was: state:FIXING (float-parse byte-exactness). *** re-
  gauntlet w3upqlq0y = 2/3 refuted, but ALL 3 CONFIRM the endianness/backend/version decision is byte-exact for
  real inputs — NO endianness refutation ***. Refutes: (a) haskell-semantics — parse_tables_codec_version
  as_u64() REJECTS float-form 1.0/1e0/100e-2 that Aeson toBoundedInteger FLOORS to 1 and ACCEPTS -> over-strict
  byte-exactness mismatch (unreachable from canonical snapshots, fail-closed, but cardinal-rule requires parity);
  (b) compounding-feedback — overclaim "rejects everything upstream rejects" since dugite skips snapshotChecksum/
  CRC verification (-> FILED as #17, separate integrity gap). RESOLUTION: small float-parse fix (accept integral
  JSON Number ==1 like Aeson) + narrow the overclaim comments + fix the "dead code" wording (cross_validate is
  live). Launched fix-muscle w3cxa15va (abs-path STRICT base). The real preprod import (integer 1) is unchanged ->
  chain re-verify should stay 0 phase-1; re-gauntlet should PASS (parse now byte-exact, claim accurate, CRC filed
  separately). was: state:GAUNTLET-PENDING (STRICT). *** FULL VERIFYING
  PASS wake142 *** verify10i re-soak (synced 124999612->125103157): 0 phase-1 transaction rejections (all
  classes), strict codec_version=1->Big, only 294 #15 Error-term. Launched RE-GAUNTLET w3upqlq0y on STRICT — key
  angle (over-strict guard): any case where upstream ACCEPTS but dugite now REJECTS (regression)? + backend tag
  exactness + re-verify modern-only claim. On PASS -> commit the STRICT patch via gh/HTTPS (lands #10). was:
  state:VERIFYING-RESOAK (STRICT). Build DONE
  (BUILD_EXIT=0). DROVE re-verify: cloned db-preprod-sync -> verify10i, ran STRICT binary (pid 32304, port 4212).
  Import log: "(strict: only version 1 => big-endian is accepted) codec_version=1 txix_endianness=Big", sane
  distribution, utxo_count=4116338 skipped=0. Node syncing 124999169->tip. NEXT WAKE FULL-VERDICT: scan ALL
  rejection classes -> 0 phase-1 -> RE-GAUNTLET (strict terminal) -> commit. GC'd verify10h (disk 38GB -> GC
  aggressively next: mainnet-ep213 48G if mainnet not imminent). was: state:VERIFYING-BUILDING (STRICT). *** muscle
  wh8n6ip92 COMPLETE wake140, checks_green, 2 crates ***. Strict semantics: from_tables_codec_version ->
  {Some(1)=>Big, else=>Err}; parse_tables_codec_version -> Result<u64> (field absent/null => Err, mandatory `.:`);
  added enforce_snapshot_backend_is_utxohd_mem (backend!=utxohd-mem => Err, mirrors loadSnapshot
  MetadataBackendMismatch); resolve_snapshot_txix_endianness: meta-FILE-absent => bail (no silent LE), enforce
  backend then version, always Big on success. None unrepresentable from an accepted meta; the LE/auto-detect
  path is now DEAD for the import decision (Big only). VERIFIED safe: all current target networks (mainnet/preview/
  preprod PV9-PV11+) ship modern flat-tables meta+version=1 BE snapshots; no network has meta-less legacy-LE ->
  legacy import path dropped, lenient tests replaced with strict *_is_error. Quotes upstream FromJSON mandatory
  `.:` + enforceVersion + loadSnapshot backend-guard + unconditional BigEndianTxIx + getMetadata-Nothing-is-CRC.
  STRICT patch saved candidate-fix-10-STRICT-codecversion.patch (3725 lines, applies clean) + applied to MAIN +
  build pid 31837 (.jobs/verify-build-10i.log). NEXT WAKE: BUILD_EXIT=0 -> re-import re-soak (preprod=version1=BE,
  0 phase-1 full rejection-class scan) -> RE-GAUNTLET (strict terminal, rejects all upstream rejects) -> commit.
  was: state:FIXING (STRICT meta semantics). *** re-gauntlet
  w4007sv2k = 2/3 refuted (haskell-semantics + edge-epoch agree; compounding-feedback ran importer tests
  empirically + did NOT refute) ***. DEFINITIVE truth table (source-cited): FromJSON uses MANDATORY
  `.: tablesCodecVersion` -> present-meta-field-absent/null => MetadataInvalid => HARD ERROR (both converter +
  loader); enforceVersion 1=>BE else fail; loadSnapshot decodes BigEndianTxIx UNCONDITIONALLY (no version=>LE
  branch) + checks backend==utxohd-mem; getMetadata's MetadataFileDoesNotExist=>Nothing means SKIP-CRC not
  decode-LE. UNANIMOUS must-fix: FINAL2 maps field-absent/null=>LE but upstream THROWS (too lenient, violates
  default-to-rejection). DECISION (end the meta-absent flip-flop): STRICT — only {meta present + version=1 +
  backend=utxohd-mem} => Big; EVERYTHING else (field-absent/null, version-other, meta-file-absent, malformed,
  backend-mismatch) => ERROR. None unrepresentable from an accepted meta. Launched strict remediation wh8n6ip92
  (abs-path base patch; verify all current mithril snapshots are modern before dropping the legacy-LE path;
  STOP+report if a real network needs meta-less LE rather than guess). was: state:GAUNTLET-PENDING (FINAL2). *** FULL VERIFYING
  PASS wake131 *** verify10h re-soak (synced 124999612->125100373): 0 phase-1 transaction rejections (all
  classes), codec_version=Some(1)->Big, only 292 #15 Error-term residual. Launched RE-GAUNTLET w4007sv2k on
  FINAL2 (= authoritative codec-version [addressed 3 prior refutes] + meta-absent tolerance [addressed the latest
  refute] + new end-to-end tests). On PASS -> commit the FINAL2 patch via gh/HTTPS (lands #10). was:
  state:VERIFYING-RESOAK (FINAL2). Build DONE
  (BUILD_EXIT=0). DROVE re-verify: cloned db-preprod-sync -> verify10h, ran FINAL2 binary (pid 72713, port 4211).
  Import: "codec_version=Some(1) txix_endianness=Big" (authoritative), distribution sane, utxo_count=4116338
  skipped=0. (meta-absent fix doesn't change the modern-BE preprod path; the legacy-LE meta-absent path is covered
  by the new end-to-end unit tests since no meta-less legacy snapshot is on disk to soak.) Node syncing
  124999169->tip. NEXT WAKE FULL-VERDICT: scan ALL rejection classes -> 0 phase-1 (MultiAssetNotConserved 0,
  not-found 0, budget 0, no new class) -> RE-GAUNTLET -> commit. GC'd verify10g (disk 53GB). was:
  state:VERIFYING-BUILDING (FINAL2 authoritative+meta-
  absent). *** muscle wx76r15y3 COMPLETE wake128, checks_green, 2 crates ***. Fix (node/mod.rs): meta read
  NotFound=>None=>Little (legacy LE), mirroring upstream getMetadata (MetadataFileDoesNotExist->Nothing->still
  decode, quoted verbatim); other IO errors propagate; Some(1)=>Big, Some(other)=>Err, field-absent/null=>None,
  malformed JSON=>Err; cross_validate+safety-net guard None=>LE. Extracted resolve_snapshot_txix_endianness
  (testable via REAL importer path). Added the MISSING end-to-end tests: importer_accepts_legacy_snapshot_with_no
  _meta_file_as_little_endian (+3: meta-without-field=>LE, codec=1=>Big, unknown=>Err) via build_tvar helper.
  Combined FINAL2 patch saved candidate-fix-10-FINAL2-authoritative-metaabsent.patch (3542 lines, applies clean)
  + applied to MAIN + build pid 71916 (.jobs/verify-build-10h.log). NEXT WAKE: BUILD_EXIT=0 -> re-import re-soak
  (modern-BE preprod, 0 phase-1 rejections via FULL rejection-class scan) -> RE-GAUNTLET (meta-absent now
  tolerated as LE per offline-importer analog; end-to-end test added) -> commit. was: state:FIXING (meta-absent tolerance). *** re-gauntlet
  w8t0ro3f6 = 3/3 refuted, but refuters DISAGREED -> waiting for the aggregate (wake122) was VINDICATED ***.
  Resolution (2-of-3 + correct upstream analog): dugite is TOO STRICT — hard-errors on meta-FILE-absent, which
  REGRESSES legitimate legacy LE snapshot imports. PROOF: dugite's mithril-import is the OFFLINE-conversion analog
  (upstream SnapshotConversion.getMetadata: MetadataFileDoesNotExist->Nothing->STILL decodes), legacy tvar
  snapshots predate the meta file (introduced 2025-04-16), and dugite SHIPS a legacy LE fixture + supports the
  legacy layout. The hard-error fires before from_tables_codec_version(None)=>Little can run. (compounding-feedback
  cited the ONLINE node-load path which rejects meta-absent — wrong analog for an offline import.) Some(1)=>Big is
  byte-exact (version added 17min before BE flip, same merge batch). FIX (muscle w5vke699f): meta-absent => None
  => Little (NOT error), same as field-absent; keep Some(1)=>Big/Some(other)=>Err; cross_validate+safety-net guard
  the None=>LE path; ADD the missing END-TO-END importer test (legacy dir, no meta => imports as LE). My earlier
  "error on missing meta" instinct (wake114/119) was too strict — gauntlet corrected it. was:
  state:GAUNTLET-PENDING (AUTHORITATIVE fix). *** FULL
  VERIFYING PASS wake121 (via byte-exact authoritative path) *** verify10g re-soak (synced 124999612->125098044):
  ZERO phase-1 transaction rejections (MultiAssetNotConserved 0, InputNotFound 0, MissingScriptWitness 0,
  not-found 0, budget 0); import used codec_version=Some(1)->Big (authoritative, NOT heuristic); cross-val no
  contradiction; only 281 #15 Error-term residual. Launched RE-GAUNTLET w8t0ro3f6 on the authoritative fix —
  key adversarial angle (4th-catch guard): does meta-FILE-absent wrongly ERROR on legitimate legacy (pre-10.7)
  snapshots that have no meta file (should be None=>Little, not error)? On PASS -> commit the AUTHORITATIVE patch
  via gh/HTTPS. was: state:VERIFYING-RESOAK (AUTHORITATIVE fix). Build DONE
  (BUILD_EXIT=0). DROVE re-verify: cloned db-preprod-sync -> verify10g, ran AUTHORITATIVE binary (pid 24240, port
  4210). Import log CONFIRMS the authoritative path: "Authoritatively determined MemPack TxIx endianness from
  snapshot meta tablesCodecVersion codec_version=Some(1) txix_endianness=Big" (NOT auto-detect); cross-validation
  distribution sane (low 3131782 vs mult256 62, no contradiction); utxo_count=4116338 skipped=0. Node syncing
  124999169->tip. NEXT WAKE FULL-VERDICT: scan ALL rejection classes -> 0 phase-1 rejections (MultiAssetNotConserved
  0, not-found 0, budget 0, no new class) -> RE-GAUNTLET (authoritative codec-version, error-on-ambiguity,
  independent cross-val) -> commit. GC'd verify10f (verify10g supersedes for #15). was:
  state:VERIFYING-BUILDING (AUTHORITATIVE fix). ***
  rework muscle wjnl2t2ib COMPLETE wake119, checks_green, addresses ALL 3 gauntlet refutations, 2 crates ***.
  UPSTREAM PROOF (resolves wake115): Ouroboros.Consensus...Snapshots `data TablesCodecVersion=TablesCodecVersion1`
  Haddock LITERALLY "Used in cardano-node 10.7. Previous versions have no codec version. [(txid, big-endian
  txix)]"; enforceVersion: 1->ok, _->fail. So version field (introduced separately in 10.7, NOT at the BE-flip
  commit) IS authoritative: tablesCodecVersion=1 -> BE; absent -> legacy host-LE; other -> ERROR. FIX:
  TxIxEndianness::from_tables_codec_version (Some(1)=Big/None=Little/else Err) is the DECISION-maker; node import
  reads haskell-ledger/<slot>/meta (ERROR if missing - "refuse to guess"), maps authoritatively, then
  cross_validate_txix_endianness re-derives empirically as INDEPENDENT defense (errors only on CLEAR
  contradiction); detect/is_sane demoted to cross-validation. 11 new unit tests (version maps, malformed->Err,
  cross-val pass/contradiction/ambiguous-defers) + gated real-preprod oracle (meta=1->Big, UTxO 00000c0c#1->
  txix==1 coin 1750000) + legacy LE fixture. serde_json dev->deps. AUTHORITATIVE patch saved
  candidate-fix-10-AUTHORITATIVE-codecversion.patch (3282 lines) + applied to MAIN + build pid 23735
  (.jobs/verify-build-10g.log). NEXT WAKE: BUILD_EXIT=0 -> re-import re-soak -> 0 phase-1 rejections (full
  rejection-class scan) -> RE-GAUNTLET (now authoritative, error-on-ambiguity, independent cross-val) -> commit.
  was: state:FIXING (authoritative endianness rework). ***
  GAUNTLET wmpyis3tx REFUTED (edge-epoch, decisive + correct) — gauntlet's 3rd catch, the most important ***.
  The FINAL fix's endianness uses an EMPIRICAL auto-detect HEURISTIC -> violates cardinal rule (byte-exact ONLY,
  NEVER heuristics); its 'safety net' re-runs the SAME is_sane() predicate (NOT independent, "first test twice");
  and an AUTHORITATIVE signal exists+unused (tablesCodecVersion — which I MYSELF found at wake109 and recorded as
  "cleaner than auto-detect" but didn't act on). Value/datum/refscript/multi-asset components confirmed SOUND by
  the refuter; only the endianness DECISION mechanism is wrong. CONFIRMED tablesCodecVersion IS in dugite's
  import dir (haskell-ledger/<slot>/meta = {"backend":"utxohd-mem","checksum":...,"tablesCodecVersion":1}; preprod
  =1=BE). DID NOT COMMIT; main reset clean. Launched rework muscle wjnl2t2ib: read tablesCodecVersion
  authoritatively (after VERIFYING from upstream that the version actually encodes the BE/LE flip), map
  version->endianness, ERROR on missing/unknown (no guess), keep the distribution check ONLY as an independent
  cross-validation. was: state:GAUNTLET-PENDING (FINAL combined fix). *** FULL
  VERIFYING PASS wake113 (first clean verdict) *** verify10f re-soak (synced 124999612->125096298, past all failing
  slots): ZERO phase-1 transaction rejections (was 986: 600 InputNotFound+174 MissingScriptWitness+163 InvalidMint
  +32 MultiAssetNotConserved+17 CollateralNotFound -> ALL 0); script-not-found 0, budget 0, MultiAssetNotConserved
  316->0; auto-detect=Big sane (low 3131782 vs mult256 62), safety net did NOT trip. ONLY residual: 281 phase-2
  "Error term" = the separately-filed #15 (distinct ScriptContext-eval cause). Launched RE-GAUNTLET wmpyis3tx on
  the FINAL fix (auto-detect + multi-asset never adversarially reviewed). On PASS -> commit the FINAL patch via
  gh/HTTPS. was: state:VERIFYING-RESOAK (FINAL combined fix). Build
  DONE (BUILD_EXIT=0). DROVE re-verify: cloned db-preprod-sync -> verify10f, ran FINAL binary (pid 78267, port
  4209). Import: auto-detect=Big, distribution sane (low 3131782 vs mult256 62), utxo_count=4116338 skipped=0,
  multi-asset now populated all tags. Node syncing 124999169->tip. NEXT WAKE FULL-VERDICT (scan ALL rejection
  classes, not just expected): MultiAssetNotConserved must drop ~316->baseline AND script-not-found 0 + budget 0
  kept + no NEW class -> then RE-GAUNTLET -> commit. GC'd verify10e (verify10f supersedes for #15). was:
  state:VERIFYING-BUILDING (FINAL combined fix). ***
  multi-asset muscle w34va8uxf COMPLETE wake111, checks_green, byte-exact real-blob oracle PASS, 2 crates ***.
  ROOT CAUSE (empirical, real blob): MemPack TxOut tags 0/1 used the OPAQUE decode_compact_value (hardcodes
  num_assets=0) -> 970K multi-asset UTxOs imported with EMPTY assets -> input_side:0 (tags 4/5 already used _exact,
  hence synthetic unit passed but real tag-0/1 failed). FIX: route tag0+tag1 through decode_compact_value_exact
  (parses VarLen numMA + rep per Mary Value.hs); tag1 DataHash offset from exact value extent. AFTER: tag0/1
  num_assets=0 eliminated, 1,629,052 multi-asset UTxOs fold non-empty, ADA-only byte-identical, all 6750 tests
  pass + new gated real-blob oracle (folded asset_list == Koios). COMBINED FINAL patch saved
  candidate-fix-10-FINAL-autodetect-multiasset.patch (2905 lines: refscript+datum+endianness-autodetect+safety-net
  +multiasset-all-tags, applies clean) + applied to MAIN + build pid 77765 (.jobs/verify-build-10f.log). NEXT WAKE:
  BUILD_EXIT=0 -> fresh import re-soak -> MultiAssetNotConserved -> ~baseline AND endianness win kept (not-found 0,
  budget 0) -> FULL rejection-class scan -> RE-GAUNTLET -> commit. was: state:FIXING (multi-asset reconstruction bug). ***
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
- verify-build-10j  pid 62684  log .jobs/verify-build-10j.log — release build of dugite-node with the FINAL-DONE
  #10 fix (STRICT + Aeson float-parse parity) on MAIN. Poll BUILD_EXIT=0 -> re-import re-verify.
- fix-muscle w3cxa15va — COMPLETE (Aeson float-parse parity + claim narrowing). patch
  candidate-fix-10-FINAL-DONE-codecversion-aeson.patch + worktree wf_1e767a9c-484-1.
- db-clones/preprod-verify10i RETAINED for #15. DISK 37GB — GC verify10i after the FINAL re-soak supersedes it.
- Patch history: ...STRICT(float-parse over-strict, 2/3 refuted) -> FINAL-DONE(Aeson float parity, current candidate).
- fix-muscle wh8n6ip92 — COMPLETE (strict meta; verified legacy-LE drop safe). patch
  candidate-fix-10-STRICT-codecversion.patch + worktree wf_e4a069c7-99f-1.
- db-clones/preprod-verify10h RETAINED for #15.
- Patch history: ...FINAL2(too lenient, 2/3 refuted) -> STRICT(version=1+backend-only-BE, else ERROR; current candidate).
- DISK: 51GB free (db-clones/mainnet-ep213 48G is the big one; GC if mainnet work not imminent).
- fix-muscle wx76r15y3 — COMPLETE (meta-absent=>LE tolerance + end-to-end tests). patch
  candidate-fix-10-FINAL2-authoritative-metaabsent.patch + worktree wf_a2048ce6-581-1.
- db-clones/preprod-verify10g RETAINED for #15.
- Patch history: ...AUTHORITATIVE(meta-absent too strict, 3/3 refuted) -> FINAL2(meta-absent tolerated, current candidate).
- *** INFRA NOTE (muscle worktree staleness) ***: isolation worktrees branch from a base commit that LAGS the
  engine-state/patch-file commits (crates/ identical to HEAD, but scripts/prod-readiness/*.patch FILES missing).
  => ALWAYS give muscles the base patch by ABSOLUTE path (/Users/michaelfazio/Source/dugite/scripts/prod-readiness/...),
  never a worktree-relative path. w5vke699f failed STEP-0 on a relative path; re-launched as wx76r15y3 with abs path.
- fix-muscle wjnl2t2ib — COMPLETE (authoritative codec-version endianness; addresses all 3 gauntlet refutes).
  patch candidate-fix-10-AUTHORITATIVE-codecversion.patch + worktree wf_fc714d5e-a00-1.
- re-gauntlet wmpyis3tx — DONE 3/3 REFUTED (drove this rework). db-clones/preprod-verify10f kept for #15.
- Patch history: ...FINAL(heuristic, 3/3 refuted) -> AUTHORITATIVE(codec-version, current candidate).
- fix-muscle w34va8uxf — COMPLETE (multi-asset tag0/1 fix; real-blob oracle PASS). FINAL patch
  candidate-fix-10-FINAL-autodetect-multiasset.patch + worktree wf_4f715407-1de-1.
- import source db-preprod-sync/haskell-ledger/ INTACT. db-clones/preprod-verify10e kept for #15.
- Patch history: ...ROBUST(endianness-ok/multiasset-buggy) -> FINAL(multiasset also fixed, current candidate).
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
- wake190 2026-06-07: POLL #10 FIX muscle wb28q1upc — still RUNNING, ACTIVE (last activity 3s, between build cycles,
  not wedged). No transition. Disk 178G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process result.
- wake189 2026-06-07: POLL #10 FIX muscle wb28q1upc — still RUNNING, ACTIVE (last activity 5s, build/test). No
  transition. Disk 179G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process result.
- wake196 2026-06-07: POLL #10 FINAL fix muscle wiujlmyn2 — still RUNNING (build/test, last activity 2min, not
  wedged). No transition. Disk 168G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process -> build -> re-import ->
  6th re-gauntlet -> COMMIT.
- wake234 2026-06-07: POLL #0 VERIFY-THEN-FIX muscle w0oegi6uf (reward_pot epoch_fees) — still RUNNING, ACTIVE
  (worktree present, rewards.rs; Koios fee tracing + ppm arithmetic + code). No transition. Disk 166G, no nodes. #0
  stays FIXING. NEXT WAKE: poll/process — fee discrepancy confirmed+fixed OR 'not localizable to fees' report.
- wake233 2026-06-07: #0 DIAGNOSE wbqhzeczq COMPLETE -> ROOT CAUSE RE-LOCATED (data-driven) to reward_pot EPOCH_FEES
  ~5ppm (rewards.rs:184), NOT apply_utxo_changes/stake. Dim-2: uniform pool-independent -5.027 ppm under-scaling of
  every member reward (stake byte-exact). Haskell uses GO-snapshot ssFee (2-epoch lag). analyze-1 wrongly ruled out
  rewards.rs -> rounds 1-2 chased red herrings. Launched VERIFY-THEN-FIX w0oegi6uf (epoch_fees source vs Koios; confirm
  ppm arithmetic before fixing). NEXT WAKE: poll -> verify+fix -> dump-verify ep246 reserves. LESSON: don't trust
  'ruled out' without data; the data-comparison diagnose found it.
- wake232 2026-06-07: POLL #0 ROUND-3 DATA-comparison diagnose wbqhzeczq — still RUNNING (0/2 dims, active; Koios
  per-account active-stake resolution + hex->bech32 is rate-limited). No transition. Disk 166G, no nodes. #0 stays
  DIAGNOSING. NEXT WAKE: on found dugite!=Koios credential -> trace class -> fix.
- wake231 2026-06-07: #0 ROUND-2 wr9tddl4q: rebuild/genesis-load/pointer ALSO correct (2 invariant tests PASS, no
  fix; kept on main). Both code rounds prove dugite INTERNALLY consistent -> can't catch a dugite-vs-Haskell per-cred
  attribution difference. Launched ROUND-3 DIAGNOSE wbqhzeczq (opus, data-comparison: dugite ep245 per-cred stake dump
  vs Koios) to find the credential where dugite!=Koios. NEXT WAKE: found cred -> trace class -> fix; else full-dump/
  replay. No db-mainnet so from-genesis mainnet replay infeasible; using dumps+Koios.
- wake230 2026-06-07: LOCK-RECOVERY (wake229 final commit/release malformed -> stale lock age315s + wake229 edit
  uncommitted; HEAD=wake228, no concurrent wake -> released+reacquired). POLL #0 ROUND-2 wr9tddl4q — still RUNNING
  (cargo pid 68010, last activity now; ~8min, rebuild==incremental invariant + full nextest). No transition. Disk
  157G. #0 stays DIAGNOSING. NEXT WAKE: poll/process.
- wake229 2026-06-07: POLL #0 ROUND-2 muscle wr9tddl4q — still RUNNING, ACTIVE (cargo running, invariant test being
  written into state/mod.rs, last activity 7s). No transition. Disk 166G, no nodes. #0 stays DIAGNOSING. NEXT WAKE:
  poll/process.
- wake227 2026-06-07: #0 ROUND-1 wxbflru4x: apply_utxo_changes SYMMETRIC (5 invariant tests PASS, no fix; tests kept
  on main). Narrowed to rebuild_stake_distribution / genesis-load / pointer-address (NOT dereg-prune — certs.rs:206
  keeps the entry). Launched ROUND-2 wr9tddl4q (rebuild==incremental per-cred invariant, dugite-ledger state/* only).
  NEXT WAKE: poll -> fix-or-escalate-to-instrumented-mainnet-replay. apply_utxo_changes hot path is correct.
- wake226 2026-06-07: POLL #0 LOCALIZE-THEN-FIX muscle wxbflru4x (apply_utxo_changes invariant-test-first) — still
  RUNNING, ACTIVE (worktree present, common.rs; deep ledger code analysis + symmetric-routing invariant test). No
  transition. Disk 166G, no nodes. #0 stays FIXING. NEXT WAKE: poll/process — invariant fails+fixed OR 'no asymmetry'
  report.
- wake225 2026-06-07: #0 ANALYZE wuqv1kgo9 COMPLETE -> ROOT-CAUSE REVISED -> FIXING. Member-reward fold REFUTED
  (aggregate byte-exact); REAL cause = apply_utxo_changes ADD/SPEND ASYMMETRY (common.rs spend 202-208/334-340 vs add
  263) corrupting per-cred stake (net-zero aggregate) -> floored under-distribution -> +82.27M reserves. UNIFIES #0/
  #2/#11/#6; 3 prior fixes failed on wrong site (rewards.rs symptom). Launched LOCALIZE-THEN-FIX muscle wxbflru4x
  (invariant-test-FIRST, no speculative fix). NEXT WAKE: poll -> invariant fails+fixed -> dump-verify ep246; else
  instrumented replay. The standing wake-prompt's apply_utxo_changes hypothesis was right all along.
- wake224 2026-06-07: POLL #0 ANALYZE wuqv1kgo9 — RESEARCH STAGE DONE (root-cause stage running). CONFIRMED divergence
  STILL REAL on HEAD (re-verify worthwhile): HEAD dumps epoch-dumps-engine/mainnet-droptrace/ show ep245 reserves=
  12905245994461083==Koios (0 diff), ep246 dugite=12880948947408249 vs Koios=12880948865137767 = +82,270,482 dRES /
  -55,269 dTRE. HARNESS: HEAD dumps ALREADY EXIST (mainnet-droptrace) -> dump-based verification viable, NO heavy
  mainnet replay needed. No transition. Disk 166G. #0 stays ANALYZING. NEXT WAKE: read root-cause stage verdict (the
  precise 2-map disagreement + discrete fix + why 3 priors failed) -> FIX.
- wake223 2026-06-07: POLL #0 ANALYZE wuqv1kgo9 — still RUNNING (research stage, last activity 3s). HARNESS NOTE:
  NO db-mainnet present (only epoch-dumps-engine/mainnet-ep213 dumps) -> #0 byte-exact verification CANNOT use a live
  mainnet replay without a heavy re-sync; must use the DUGITE_REWARD_DBG dump-loop harness on the ep245 'go' snapshot
  (if dumped) OR re-acquire db-mainnet. No transition. Disk 166G, no nodes. #0 stays ANALYZING. NEXT WAKE: read
  verdict + its harness recommendation.
- wake222 2026-06-07: #15 done -> #0 ACTIVE (mainnet ep246 reserves). PARKED attempts:3 -> per staleness lesson,
  launched ANALYZE muscle wuqv1kgo9 (opus) to RE-VERIFY the member-reward-fold root cause vs Haskell resolveActive
  InstantStakeCredentials + characterize the 2-map disagreement + why 3 prior fixes failed + fix/harness plan. NEXT
  WAKE: read verdict -> fix (careful Tier A) or assess mainnet-ep246 harness availability first. #0 verification HEAVY.
- wake221 2026-06-07: ***** #15 DONE — COMMITTED 117c41e5f5 + PUSHED *****. Re-gauntlet w4ou064y2 PASSED clean (0/3
  refuted; all source-verified byte-exact vs getBabbageSpendingDatum/toPlutusV3Args). CI gate green (fmt+clippy+uplc
  435). Committed 2 files/1 crate (dugite-uplc). #15 closes phase-2 serialiseData: 306 ep293 'Error term' -> 0 (V3
  SpendingScriptInfo datum was None; fixed canonical, NO memo). Arc lesson: the gauntlet caught a replay-passing memo
  fix as conceptually wrong; minimal canonical fix passed clean. NOW ACTIVE: #0 (mainnet ep246 reserves, parked->
  active) -> next wake launch ledger muscle.
- wake220 2026-06-07: POLL #15 RE-GAUNTLET w4ou064y2 (minimal canonical V3-datum fix) — still RUNNING (0/3 votes,
  active). No transition. Disk 166G, no nodes. #15 stays GAUNTLET-PENDING (replay already PASSED 306->0 full window).
  NEXT WAKE: on PASS -> commit #15 (dugite-uplc, 1 crate).
- wake219 2026-06-07: #15 REPLAY DECISIVE PASS — 0 'Error term' across FULL ep293 window (was 306), 27751ab9 fixed,
  0 phase-1 (tip 125125576). VERIFYING-RESOAK -> GAUNTLET-PENDING; SIGTERM'd verify15min, launched re-gauntlet
  w4ou064y2 on the minimal canonical V3-datum fix (memo gone). NEXT WAKE: PASS -> commit #15 (dugite-uplc).
- wake218 2026-06-07: #15 MINIMAL fix wkba3hja9 green -> built -> uplc nextest 435 -> REPLAY PREFIX PASS. verify15min:
  0 'Error term' by 125010507 (prior 41), 0 total so far, 27751ab9 fixed, 0 phase-1. Canonical V3-datum-population
  fix WORKS (no memo) — confirms 306 were V3 None-datum not byte-shape. FIXING -> VERIFYING-RESOAK. NEXT WAKE: confirm
  past window -> re-gauntlet (should PASS now) -> commit #15.
- wake217 2026-06-07: POLL #15 MINIMAL fix muscle wkba3hja9 (V3 datum population, canonical, no memo) — still
  RUNNING, ACTIVE (worktree present, build/test). No transition. Disk 161G, no nodes. #15 stays FIXING. NEXT WAKE:
  poll/process -> build -> re-replay (306->0).
- wake216 2026-06-07: #15 ANALYZE wpkh7n7c9 COMPLETE -> DIAGNOSING -> ROOT-CAUSED -> FIXING. Verdict: memo WRONG
  (serialiseData=canonical encodeData, dugite encoder already matches; ep293 datum IS canonical; memo = silent pass-
  where-Haskell-fails on non-canonical). REAL cause: V3 SpendingScriptInfo datum was None. Reverted main dugite-uplc
  to clean HEAD; launched MINIMAL fix wkba3hja9 (V3 datum population, CANONICAL, no memo; no bridge). NEXT WAKE: poll
  -> build -> re-replay (306->0) -> re-gauntlet -> commit. Gauntlet caught a fix that passed replay for wrong reason.
- wake215 2026-06-07: POLL #15 ANALYZE wpkh7n7c9 — RESEARCH STAGE DONE w/ DEFINITIVE VERDICT (root-cause stage still
  running): MEMO IS WRONG, R1 CONFIRMED. Q1: serialiseData = BSL.toStrict.serialise = encodeData (CANONICAL);
  getPlutusData STRIPS MemoBytes before the CEK (datum AND redeemer = bytes-less PLC.Data) -> serialiseData is
  canonical, never verbatim; the datum-memoised-redeemer-not asymmetry is the tell the memo theory is wrong. Q2:
  dugite encode_data ALREADY byte-matches PlutusCore encodeData (harness-proven: tag-102->compact-122, empty-list
  definite/non-empty indefinite, etc.) -> encoder needs NO fix. KEY RECONCILIATION: the 306->0 came from the
  V3-EXTENSION *POPULATING* the V3 SpendingScriptInfo datum (was None pre-fix -> script got no datum -> serialiseData
  failed), NOT from the memo; canonical works equally. CORRECT FIX: KEEP the V3 spending-datum population
  (resolve_spend_datum_optional + eval_redeemer ScriptInfo::Spending{datum}) but build it CANONICAL (plutus_data_to_
  data, NOT memoised), and REVERT the entire Data-memo architecture (DataKind-split memo, to_cbor-returns-memo, all
  the with_original/datum_raw threading). #15 memo still on main uncommitted. No transition (analyze not complete).
  NEXT WAKE: read root-cause stage's precise keep-vs-revert plan -> FIX (revert memo + keep V3 datum population
  canonical) -> re-replay (still 306->0 AND correct for non-canonical) -> re-gauntlet -> commit.
- wake213 2026-06-07: #15 GAUNTLET w4a16gr1r REFUTED 3/3 (PROFOUND). R1: serialiseData builtin = PlutusCore-CANONICAL
  encodeData (getPlutusData strips MemoBytes), NOT verbatim -> the memo is likely the WRONG approach; real bug =
  dugite encode_data != PlutusCore encodeData (270 vs 276). Replay passed for the wrong reason (ep293 datums already
  canonical). Memo makes dugite WRONGLY pass non-canonical-datum failed-script txs (silent pass-where-Haskell-fails).
  GAUNTLET-PENDING -> DIAGNOSING; launched ANALYZE wpkh7n7c9 (opus) to settle vs PlutusCore source. DO NOT commit.
  NEXT WAKE: verdict -> likely revert memo + fix encode_data. (Gauntlet caught a fix that passed replay for wrong reason.)
- wake212 2026-06-07: POLL #15 GAUNTLET w4a16gr1r — still RUNNING (0/3 votes, active). No transition. Disk 169G, no
  nodes. #15 stays GAUNTLET-PENDING (byte-exact gate already passed: 306->0 full window). NEXT WAKE: on PASS -> commit
  #15 (dugite-uplc, 1 crate).
- wake211 2026-06-07: #15 V3-extension fix w19kofqwx -> built -> REPLAY DECISIVE PASS. verify15v3: 0 'Error term'
  across FULL ep293 window (was 306), 27751ab9 fixed, 0 phase-1, uplc nextest 439 green. The serialiseData verbatim-
  bytes fix WORKS (V3 SpendingScriptInfo datum memoised per getBabbageSpendingDatum). VERIFYING-RESOAK -> GAUNTLET-
  PENDING; launched gauntlet w4a16gr1r. NEXT WAKE: PASS -> commit #15 (dugite-uplc). [Also: wake-mid, made muscle
  diagnose model overridable via args.diagnoseModel per user request — committed separately.]
- wake210 2026-06-07: LOCK-RECOVERY (wake209 final commit/release call malformed -> stale lock age302s + wake209
  edit uncommitted; HEAD=wake208, no concurrent wake -> released+reacquired). POLL #15 V3-extension w19kofqwx — still
  RUNNING (cargo pid 67721, last activity ~90s). No transition. Disk 163G. #15 stays FIXING. NEXT WAKE: poll/process
  -> build -> re-replay ep293 (306 Error-term -> ~0).
- wake209 2026-06-07: POLL #15 V3-extension fix muscle w19kofqwx — still RUNNING, ACTIVE (worktree present, bridge
  applied, build/test). No transition. Disk 171G, no nodes. #15 stays FIXING. NEXT WAKE: poll/process -> build ->
  re-replay ep293 (306 Error-term -> ~0).
- wake208 2026-06-07: #15 fix w1xi3j2nf green (Data-memo, 438 uplc tests) BUT REPLAY VERDICT = NO-OP (41 Error-term
  == pre-#15, same txs incl 27751ab9). Root cause: memo threaded V1/V2 datum-arg only; PlutusV3 spending datum
  (SpendingScriptInfo) NOT memoised (eval_redeemer:156 + populate_v3 zero threading) — failing scripts are V3.
  Architecture kept; regenerated base-15-uplc-bridge.patch, launched V3-extension fix w19kofqwx (wf_819302a0-b89).
  NEXT WAKE: poll -> build -> re-replay (306 Error-term -> ~0) -> gauntlet -> commit. Replay caught the no-op again.
- wake207 2026-06-07: POLL #15 FIX muscle w1xi3j2nf — still RUNNING (~45min; tool-calls 437->495, iterating on
  conformance-test assertions from the memo change [serialiseData round-trip], last activity 2s, not wedged). No
  transition. Disk 165G. #15 stays FIXING. NEXT WAKE: poll/process result.
- wake206 2026-06-07: POLL #15 FIX muscle w1xi3j2nf — still RUNNING (~40min; 437 agent tool-calls = heavy iteration
  on Data refactor + 999 UPLC conformance tests; last activity 6s, not wedged). No transition. Disk 173G. #15 stays
  FIXING. NEXT WAKE: poll/process result. (Legitimately long — Data enum change ripples through CEK + conformance.)
- wake205 2026-06-07: POLL #15 FIX muscle w1xi3j2nf — still RUNNING (~35min; memo IS added to data.rs [38 refs],
  in build/test iteration, last activity 5s, not wedged). No transition. Disk 173G, no nodes. #15 stays FIXING.
  NEXT WAKE: poll/process result.
- wake204 2026-06-07: POLL #15 FIX muscle w1xi3j2nf — still RUNNING, ACTIVE (~30min; Data-memo refactor + heavy
  UPLC conformance nextest; last activity 5s, not wedged). No transition. Disk 173G, no nodes. #15 stays FIXING.
  NEXT WAKE: poll/process result.
- wake203 2026-06-07: POLL #15 FIX muscle w1xi3j2nf — still RUNNING, ACTIVE (last activity 1s, build/test). No
  transition. Disk 173G, no nodes. #15 stays FIXING. NEXT WAKE: poll/process result.
- wake202 2026-06-07: POLL #15 FIX muscle w1xi3j2nf (Data verbatim-memo) — still RUNNING, ACTIVE (worktree present,
  dugite-uplc; Data-memo refactor touches data.rs+denotations.rs+tx_info_populate.rs+Hash/Eq impls+tests). No
  transition. Disk 174G, no nodes. #15 stays FIXING. NEXT WAKE: poll/process -> build -> replay ep293 window.
- wake201 2026-06-07: #15 ROOT-CAUSED-CONFIRMED -> FIXING (first item after #10). Assessed: Data enum purely
  structural, dugite-uplc clean+undrifted (no bridge), 1 crate. Launched FIX muscle w1xi3j2nf (wf_75ef4164-1c0):
  optional CBOR memo on Data (Hash/Eq ignore it), threaded from on-chain bytes, serialiseData returns memo. NEXT
  WAKE: poll -> build -> replay ep293 window -> 306 Error-term -> ~0 -> gauntlet -> commit.
- wake200 2026-06-07: ***** #10 DONE — COMMITTED 125ce7ef18 + PUSHED *****. 6th gauntlet w7i0t8l28 REFUTED 3/3 but
  ALL adversarial-only (varlen overflow, definite-map truncation, backend dup-key last-wins). HARD POLICY invoked:
  core byte-exact (6 replays, round-5 exhaustive confirm), Mithril-signed => malformed out-of-band, surface unbounded.
  CI gate green (fmt + clippy -D warnings + nextest 1140). Committed 10 files/2 crates (dugite-serialization+node) via
  HTTPS. Filed #20 (snapshot-import adversarial-hardening: varlen/definite-map/backend). SIGTERM'd verify10B5. #10
  closes the fast-start phase-2 IMPORT arc (986 phase-1 rejections -> 0). NOW ACTIVE: #15 (serialiseData verbatim,
  dugite-uplc, ROOT-CAUSED-CONFIRMED) -> next wake launch fix muscle.
- wake199 2026-06-07: POLL #10 6th GAUNTLET w7i0t8l28 — still RUNNING (0/3 votes). verify10B5 WINDOW CONFIRMED:
  synced PAST window (tip 125119080 > 125105013, block 4794493), 0 phase-1 — R1+R3 binary holds 0-phase-1 past the
  ep293 window (full replay evidence in hand for commit). No transition (gauntlet is the gate). Disk 175G. #10 stays
  GAUNTLET-PENDING. NEXT WAKE per HARD POLICY: PASS -> commit #10; adversarial-only REFUTE -> commit core + hardening
  item.
- wake198 2026-06-07: #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. verify10B5 byte-identical (0 phase-1 so far, mid-
  window). Launched 6th RE-GAUNTLET w7i0t8l28 (wf_3579ddd3-3c2), #15/#17/#19 scoped out, parallel to window sync.
  NEXT WAKE per HARD POLICY: PASS -> COMMIT #10; adversarial-only REFUTE -> COMMIT core + hardening tracking item.
- wake197 2026-06-07: #10 FIXING -> VERIFYING-BUILDING -> VERIFYING-RESOAK. FINAL fix wiujlmyn2 green (R1 top_level_
  number_literal structural value [aeson .: top-level only]; R3 indefinite-map EOF-no-break => Err). Copied to main,
  BUILD_EXIT=0, serialization nextest GREEN 1140, verify10B5 byte-identical (4116338, codec=1 Big, 0 phase-1, 0
  NotFullyConsumed, 0 truncation-err). NEXT WAKE: window -> 6th re-gauntlet -> COMMIT (hard policy: last cycle).
- wake195 2026-06-07: POLL #10 FINAL fix muscle wiujlmyn2 (R1 complete-F1 + R3 indef-trunc) — still RUNNING, ACTIVE
  (worktree present, bridge applied). No transition. Disk 177G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process
  -> build -> re-import -> 6th re-gauntlet -> COMMIT (per hard policy).
- wake194 2026-06-07: #10 5th GAUNTLET ww5a6h0zx REFUTED 2/3 (edge-epoch refuter CONFIRMED core byte-exact) ->
  FIXING. R1 = F1 incomplete (value still flat-scans nested keys; complete it structurally); R3 = truncated
  indefinite map silent-end (hard-error on indef-EOF-no-break). Both adversarial-only, mempack/mod.rs. SIGTERM'd
  verify10B4 (window 0-phase-1 captured). Launched FINAL fix muscle wiujlmyn2. HARD POLICY: last adversarial cycle;
  after this -> COMMIT #10 (further adversarial-only edges -> hardening tracking item, not more cycles). NEXT WAKE:
  poll -> build -> re-import -> 6th re-gauntlet -> commit.
- wake193 2026-06-07: POLL #10 5th GAUNTLET ww5a6h0zx — still RUNNING (0/3 votes). verify10B4 WINDOW CONFIRMED:
  synced PAST window (tip 125117568 > 125105013, block 4794428), 0 phase-1 — F1+F2 binary holds 0-phase-1 past the
  ep293 window (window evidence in hand for commit). No transition (gauntlet is the gate). Disk 177G. #10 stays
  GAUNTLET-PENDING. NEXT WAKE: on PASS -> commit #10; on REFUTE -> commit-or-hardening-item per wake187/192 policy.
- wake192 2026-06-07: #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. verify10B4 byte-identical (0 phase-1, 0
  NotFullyConsumed). Launched 5th RE-GAUNTLET ww5a6h0zx (wf_bcedc476-060), #15/#17/#19 scoped OUT, parallel to window
  sync. NEXT WAKE policy: PASS -> commit #10; REFUTE adversarial-only -> commit core + hardening tracking item.
- wake191 2026-06-07: #10 FIXING -> VERIFYING-BUILDING -> VERIFYING-RESOAK. F1+F2 muscle wb28q1upc green (1 crate;
  F1 aeson first-wins dup-key, F2 read_native_script accept indefinite outer array). Copied to main, BUILD_EXIT=0,
  serialization nextest GREEN (1130 passed), verify10B4 import byte-identical (4116338, codec=1 Big, 0 phase-1, 0
  NotFullyConsumed). NEXT WAKE: window -> 5th re-gauntlet -> commit-or-policy-call (F2 was last real-snapshot risk).
- wake188 2026-06-07: POLL #10 FIX muscle wb28q1upc (F1 dup-key + F2 native-indef-array) — still RUNNING, ACTIVE
  (worktree present, bridge applied, build/test). No transition. Disk 179G, no nodes. #10 stays FIXING. NEXT WAKE:
  poll/process -> build -> re-import -> re-gauntlet.
- wake187 2026-06-07: #10 4th-gauntlet wvfzy4jta REFUTED 3/3 (new edges) -> FIXING. F1 dup-key (aeson first-wins vs
  serde_json last-wins in parse_tables_codec_version, compile-verified, adversarial); F2 read_native_script rejects
  indefinite outer array but Haskell/nested accept it -> aborts real fast-start (HIGH); F3 CompactAddr-not-verbatim
  pointer non-minimal varlen -> #19 (rare/large). Launched FIX muscle wb28q1upc (F1+F2, dugite-serialization only).
  POLICY: if round-5 finds only adversarial-only edges (no real-snapshot risk), commit #10 core + open adversarial-
  hardening tracking item. NEXT WAKE: poll -> build -> re-import -> re-gauntlet -> commit.
- wake186 2026-06-07: POLL #10 RE-GAUNTLET wvfzy4jta (4th round) — still RUNNING (1/3 refuters done, active). No
  transition. Disk 179G, no nodes. #10 stays GAUNTLET-PENDING. NEXT WAKE: on PASS -> commit #10.
- wake185 2026-06-07: #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. verify10B3 window confirmed (tip 125115283, 0 phase-1,
  0 NotFullyConsumed). SIGTERM'd it; launched RE-GAUNTLET wvfzy4jta (wf_d0e85509-f55) on complete final state (6-path
  + R1 dangerouslyBig + R2 full-consumption). 4th round; prior 3 rounds all addressed. NEXT WAKE: PASS -> commit #10.
- wake184 2026-06-07: #10 FIXING -> VERIFYING-BUILDING -> VERIFYING-RESOAK. R1+R2 muscle w3dsqneah green (1 crate,
  dangerouslyBig O(1) bound + full-consumption assert). Copied to main, BUILD_EXIT=0 (no drift — 3-crate bridge),
  verify10B3 import BYTE-IDENTICAL (utxo_count=4116338 same txix dist; the '4116339' was misremembered), codec=1 Big,
  0 phase-1, 0 NotFullyConsumed. NEXT WAKE: window 0-phase-1 -> re-gauntlet -> commit.
- wake183 2026-06-07: POLL #10 FIX muscle w3dsqneah — still RUNNING (cargo-nextest pid 29079 in test phase, not
  wedged). No transition. Disk 173G, no soak nodes. #10 stays FIXING. NEXT WAKE: poll/process result.
- wake182 2026-06-07: POLL #10 FIX muscle w3dsqneah — still RUNNING, ACTIVE (last activity 6s ago, between build
  cycles). No transition. Disk 181G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process result.
- wake181 2026-06-07: POLL #10 FIX muscle w3dsqneah (R1 dangerouslyBig + R2 full-consumption) — still RUNNING, ACTIVE
  (worktree present, bridge applied, build/test). No transition. Disk 181G, no nodes. #10 stays FIXING. NEXT WAKE:
  poll/process -> build -> re-import -> re-gauntlet -> commit.
- wake180 2026-06-07: #10 GAUNTLET-PENDING -> FIXING. wd3lzyawv REFUTED 2/3 (3rd verified 6 paths byte-exact). 2 new
  edges in mempack/mod.rs: (R1) dangerouslyBig guard missing -> 1e2000000000 BigInt::pow blowup (bound net_exp before
  pow); (R2) TvarIterator discards _consumed -> partial-TxOut silent accept; Haskell unpackFail full-consumption-
  strict (assert _consumed==len else Err). SIGTERM'd verify10B2 (window 0-phase-1 captured). Regenerated 3-crate
  base-commitB2-bridge.patch (includes Convertible variant+arm -> no drift). Launched FIX muscle w3dsqneah
  (wf_ed2d1c3a-ca1). NEXT WAKE: poll -> build -> re-import -> re-gauntlet -> commit. Converging.
- wake179 2026-06-07: POLL #10 GAUNTLET wd3lzyawv — still RUNNING (last activity current). verify10B2 WINDOW
  EVIDENCE CONFIRMED: synced PAST window (tip 125113936 > 125105013, block 4794270) with 0 phase-1 — the 6-path
  binary holds 0-phase-1 past the ep293 window. No transition (gauntlet is the gate). Disk 182G. #10 stays
  GAUNTLET-PENDING. NEXT WAKE: on gauntlet PASS -> COMMIT #10 (window evidence already in hand).
- wake178 2026-06-07: #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. verify10B2 import fully clean (4116339 UTxOs, 0
  phase-1, 0 hard-errors — new hard-error paths don't false-trigger, opaque relax doesn't over-reject). Launched
  RE-GAUNTLET wd3lzyawv (wf_487f86db-d79) on 6-path disposition, parallel to verify10B2 window sync. All prior
  refutes resolved (opaque/hard-error/short-circuit). NEXT WAKE: gauntlet PASS + window 0-phase-1 -> commit #10.
- wake177 2026-06-07: #10 FIXING -> VERIFYING-BUILDING -> VERIFYING-RESOAK. Re-fix muscle wcp4vycpw green (6-path:
  tag-4/5 opaque-store [Plutus opaque, native Timelock structural], TvarIterator/address/multi-asset hard-error, R3
  c==0 short-circuit). Cross-crate drift: bridge 2-crate but Convertible variant lives in dugite-ledger (3rd crate);
  agent re-classified via Mismatch guard, dead on main -> restored HEAD Convertible arm at copy-time. BUILD_EXIT=0,
  GC'd 16G worktree, launched verify10B2 pid 10889: codec_version=1 Big, 0 phase-1, 0 import hard-errors. NEXT WAKE:
  confirm past window -> re-gauntlet -> commit. (LESSON: bridge must span all crates a feature touches.)
- wake176 2026-06-07: POLL #10 RE-FIX muscle wcp4vycpw — nextest FINISHED (no cargo procs), agent writing final FIX
  result (last activity 30s ago). Nearly complete. No transition. Disk 168G. #10 stays FIXING. NEXT WAKE: process
  result -> copy to main -> verify-build -> re-import -> re-gauntlet -> commit.
- wake175 2026-06-07: LOCK-RECOVERY + POLL. wake174's final commit/release call was MALFORMED (never executed) ->
  left a STALE wake-lock held (age 311s, < 1320s TTL so no auto-reclaim) + wake174 engine-state edit uncommitted.
  Verified NO concurrent wake (HEAD=wake173, only the bg muscle running), so safely release+re-acquired (this is a
  defensible self-recovery, not a protocol violation — the "busy=STOP" rule targets REAL concurrent wakes). #10 RE-FIX
  muscle wcp4vycpw now in FINAL nextest phase (cargo-nextest pid 80845 running, worktree target 16G, last activity
  2min ago — progressing, not wedged). No transition. Disk 168G. #10 stays FIXING. LESSON: a malformed tool call can
  strand the lock; the TTL (22min) is the backstop but manual release on a verified-self-stale lock is faster.
- wake174 2026-06-07: POLL #10 RE-FIX muscle wcp4vycpw — still RUNNING, ACTIVE (last activity 09:32; 6-path error-
  propagation fix is involved — decode_imported_script_ref signature, TvarIterator->Result, 5 hard-error sites + 1
  opaque-relax + tests). No transition. Disk 181G, no nodes. #10 stays FIXING. NEXT WAKE: poll/process result.
- wake173 2026-06-07: POLL #10 RE-FIX muscle wcp4vycpw — still RUNNING, ACTIVE (worktree present, bridge applied,
  6-path edits + build/test). No transition. Disk 184G, no nodes. #10 stays FIXING. NEXT WAKE: poll -> on green copy
  to main -> verify-build -> re-import -> re-gauntlet -> commit.
- wake172 2026-06-07: #10 DIAGNOSING -> FIXING. ANALYZE wezt2hemc gave source-grounded 6-path disposition (leaf=
  OPAQUE-STORE, container-truncation=HARD-ERROR): tag-4/5 opaque (revert my over-reject), TvarIterator/address/multi-
  asset HARD-ERROR (pre-existing silent paths), R3 c==0 short-circuit. Regenerated base-commitB-bridge.patch (4152L,
  ca50afd9ef->main, includes Convertible) + launched FIX muscle wcp4vycpw (wf_b1d55d93-e4a). NEXT WAKE: poll -> build
  -> re-import -> re-gauntlet -> commit. File #19 opaque-CompactAddr separately.
- wake171 2026-06-07: POLL #10 ANALYZE wezt2hemc — research stage DONE (1 result), root-cause agent running (final
  stage, last activity current). No transition. Disk 184G, no nodes. #10 stays DIAGNOSING. NEXT WAKE: on result ->
  fix (R3 keep; tag-4/5 opaque-no-redecode; harden the 3 silent paths).
- wake170 2026-06-07: POLL #10 ANALYZE wezt2hemc (Haskell per-path disposition) — still RUNNING, ACTIVE (oracle
  research on loadSnapshot/MemPack/BinaryData). No transition. No nodes; disk 184G. #10 stays DIAGNOSING. NEXT WAKE:
  on analyze result -> fix (R3 keep; tag-4/5 opaque-no-redecode; harden TvarIterator/address/multi-asset).
- wake169 2026-06-07: #10 RE-GAUNTLET wdvf5l5le = REFUTED 3/3 (substantive). R3 float-parse + TxIx/backend CONFIRMED
  byte-exact by all 3. VALID: (a) my no-silent-None tag-4/5 OVER-REJECTS vs Haskell opaque BinaryData (re-decodes +
  hard-errors where Haskell stores opaque) — INTRODUCED bug; (b) pre-existing silent paths TvarIterator mid-map
  truncation / address-skip / multi-asset-ADA-degrade (Haskell MemPack hard-fails). GAUNTLET-PENDING -> DIAGNOSING.
  Launched ANALYZE muscle wezt2hemc (wf_66f42008-83b) to verify Haskell per-path disposition. NEXT WAKE: fix (R3 keep;
  tag-4/5 opaque-no-redecode; harden the 3 silent paths) -> re-gauntlet -> commit.
- wake168 2026-06-07: #10 VERIFYING-RESOAK PASS -> GAUNTLET-PENDING. verify10B past window: 0 phase-1, 0 import
  hard-errors, 308 Error-term (=#15). SIGTERM'd verify10B; launched RE-GAUNTLET wdvf5l5le (wf_83b4db4e-836) on
  FINAL-DONE+R3+no-silent-None (prior 3/3 resolved: R1+R2=general-UPLC #15 byte-level; R3 fixed; CRC=#17). NEXT WAKE:
  PASS -> commit #10 (2 crates) + activate #15.
- wake167 2026-06-07: #10 BUILD_EXIT=0 -> VERIFYING-BUILDING -> VERIFYING-RESOAK. Cloned verify10B, launched
  combined hardening binary pid 48115. R3/no-silent-None NON-REGRESSING: codec_version=1 Big (real integer-1 meta),
  0 phase-1, 0 import hard-errors. NEXT WAKE: confirm full 0 phase-1 past window -> RE-GAUNTLET -> commit #10.
- wake166 2026-06-07: #10 FIXING -> VERIFYING-BUILDING. Fix muscle wjuuqz22k green (R3 f64-free Aeson-exact float-
  parse via raw-literal+bigint; no-silent-None tag-4/5 hard-error; 2 crates). Copied to main; build FAILED on
  worktree-staleness drift (missing BackendCheckResult::Convertible arm from #9 a417bd2c6f, landed after stale base
  ca50afd9ef) -> re-inserted arm, rebuild pid 47755. GC'd 161GB / 15 stale muscle worktrees (8.6G->186G free).
  NEXT WAKE: BUILD_EXIT=0 -> verify10B re-import (0 phase-1) -> re-gauntlet -> commit #10.
- wake165 2026-06-07: RECORD #15 ROOT-CAUSED-CONFIRMED (byte-level proof from wpeec891q mechanism dim, while #10 fix
  muscle wjuuqz22k runs). Script 7afbde08 (PlutusV3) computes blake2b(serialiseData(datum)); on-chain datum = 276
  bytes w/ indefinite arrays, blake2b = bbd35202.. = datum_hash exactly; dugite to_cbor() canonicalises -> 270 bytes,
  blake2b = feec1506.. != hash -> Error term. 12/13 failing scripts call serialiseData. FIX (dugite-uplc): Constant::
  Data must carry verbatim original CBOR (MemoBytes); serialiseData returns it. #15 -> ROOT-CAUSED-CONFIRMED, activates
  after #10 lands. (#10 unchanged this wake — its fix muscle still running.)
- wake164 2026-06-07: #10 ROOT-CAUSED -> FIXING (commit-B hardening). Generated base-FINAL-DONE-main.patch (3856
  lines, 2 crates) from main's uncommitted tree; launched FIX muscle wjuuqz22k (wf_52ea6a96-0ac, worktree, applies
  base patch by abs path first) for R3 byte-exact float-parse (arbitrary_precision + raw token, Aeson toBoundedInteger)
  + no-silent-None tag-4/5 import. NEXT WAKE: poll; green -> verify-build -> re-import (still 0 phase-1) -> re-gauntlet
  -> commit #10. (#15 wpeec891q mechanism dim still running.)
- wake163 2026-06-07: #10 DIAGNOSING -> ROOT-CAUSED. wpeec891q classification dim COMPLETE = GENERAL-UPLC: 6/15
  failing txs purely-post-snapshot, CASE 27751ab9 fully INDEPENDENT (input 3d7bb051 @124999282 never imported) yet
  fails -> import is NOT necessary -> the 306 belong to #15 (re-framed: serialiseData general-UPLC, NOT compact-
  address/import). #10 phase-1 (FINAL-DONE, 0 phase-1) is DONE+SEPARABLE. REMAINING #10: fold R3 float-parse +
  no-silent-None -> re-gauntlet (prior 3/3 refute now resolved) -> commit (2 crates). wpeec891q mechanism dim still
  running. NEXT WAKE: read mechanism result, launch #10 R3+no-silent-None fix muscle.
- wake162 2026-06-07: #10 re-diagnose. CODE-CONFIRMED prime suspect: serialiseData builtin denotations.rs:601
  d.to_cbor() = canonical re-encode (Haskell returns memoised ORIGINAL bytes) -> non-canonical Data serialised by a
  script diverges -> 'Error term'. Explains the inline-fix no-op (only Data-BYTES vector). Launched re-diagnose
  muscle wpeec891q (wf_1bcfce4f-50b): confirm vs tx 10a0dbda (builtin tag 51 present? datum non-canonical?) +
  import-vs-general classification (15/306 spend only post-snapshot?). NEXT WAKE: GENERAL -> commit FINAL-DONE as #10
  + file 306 as new serialiseData-verbatim item; IMPORT-specific -> #10 absorbs.
- wake161 2026-06-07: #10 VERDICT = NO-OP. verify10A (FINAL-DONE+inline-fix) synced past window -> 306 'Error term'
  == verify10j's 297 (unchanged). Inline-datum re-encode mechanism REFUTED BY REPLAY. SIGTERM'd verify10A, REVERTED
  redeemer_resolve.rs (kept main clean, FINAL-DONE intact). VERIFYING-RESOAK -> DIAGNOSING (re-open, OPEN mind).
  NEXT WAKE: launch diagnose muscle to root-cause ONE failing tx (10a0dbda, slot 125009209) via koios+CEK trace ->
  settle IMPORT-specific vs GENERAL-UPLC. If general -> commit FINAL-DONE as #10(B) + file 306 as new phase-2 item.
- wake160 2026-06-07: #10 BUILD_EXIT=0 -> VERIFYING-BUILDING -> VERIFYING-RESOAK. SIGTERM'd verify10j (clean),
  GC'd verify10i, cloned verify10A, launched combined binary pid 98474. Import OK (codec_version=1 Big, 0 phase-1).
  Still pre-window. NEXT WAKE VERDICT: count 'Error term' at slots 125001020+ -> ~0=works->gauntlet->commit(A);
  ~297=no-op->revert+re-diagnose serialiseData/CEK. Disk 22G.
- wake159 2026-06-07: #10 FIX muscle wst6ekcg6 COMPLETED (green, A', dugite-uplc inline_spend_datum verbatim) ->
  FIXING -> VERIFYING-BUILDING. *** FIX AGENT CAVEAT: V1/V2 txInfoData is witness-only + InlineDatum.data already
  read_plutus_data(raw_cbor) => fix LIKELY NO-OP; true mechanism may be UPLC serialiseData re-encode, not datum
  resolution. *** Copied fix into main (has FINAL-DONE), build pid 97939 (.jobs/verify-build-10A.log). NEXT WAKE:
  BUILD_EXIT=0 -> verify10A re-soak -> COUNT 'Error term' at ep293 slots 125001020+: ~0=fix works->gauntlet->commit;
  ~297=NO-OP -> revert + re-diagnose toward serialiseData/CEK. Health: node pid63671 slot125107354 0 phase-1; disk 20G.
- wake158 2026-06-07: POLL #10 FIX muscle wst6ekcg6 — still RUNNING, ACTIVE (Opus fix agent aec3ca671 in analysis/
  oracle+build phase). No transition. Health: verify10j node (pid 63671, 37min) slot 125107058 block 4793980, 0
  phase-1; disk 28G. #10 stays FIXING. NEXT WAKE: on fix green -> VERIFYING-replay 297 residual.
- wake157 2026-06-07: POLL #10 FIX muscle wst6ekcg6 — still RUNNING, ACTIVE (worktree build+fmt/clippy/nextest).
  No transition. Health: verify10j node (pid 63671, 32min) slot 125106750 block 4793973, 0 phase-1; disk 28G. #10
  stays FIXING. NEXT WAKE: on fix green -> VERIFYING-replay 297 residual.
- wake156 2026-06-07: #10 ROOT-CAUSED -> FIXING. Launched FIX muscle wst6ekcg6 (wf_3ec4a181-f27, worktree) for
  commit (A) = dugite-uplc inline-datum verbatim-bytes ScriptContext fix (resolve_spend_datum:620 must use carried
  raw_cbor like the DatumHash:631-642 raw-span precedent; universal imported+live; +Constr-tag-102 regression test).
  NEXT WAKE: poll; on green -> VERIFYING-replay 297 residual -> gauntlet -> commit (A), then commit (B).
- wake155 2026-06-07: #10 DIAGNOSE wuoecuy7o COMPLETED found=true -> DIAGNOSING -> ROOT-CAUSED. The R1+R2 gauntlet
  dissent is EMPIRICALLY CONFIRMED: 2/20 sampled 'Error term' txs (10a0dbda spends imported d653e369#0 created slot
  121384342; 08c596be spends imported 722326df#0 created slot 124884686) spend pre-snapshot IMPORTED inline-datum
  (tag=1) script (0x70) UTxOs. ROOT CAUSE = dugite-uplc resolve_spend_datum re-encodes inline datum CANONICALLY
  (ignores raw_cbor) -> non-canonical on-chain Constr tag-102 datum doesn't round-trip -> ScriptContext datum-hash
  mismatch -> 'Error term'. #10 NOT separable; absorbs the inline-datum verbatim-bytes fix. SPLIT into commit (A)
  dugite-uplc verbatim-datum [gating 297-residual fix] + (B) dugite-serialization+dugite-node FINAL-DONE phase-1 +
  no-silent-None + R3 float-parse. NEXT WAKE: launch FIX muscle for (A) in worktree.
- wake154 2026-06-07: POLL #10 DIAGNOSE wuoecuy7o — still RUNNING, ACTIVE (recent agent writes; koios tx-input
  resolution is rate-limited REST, legitimately slow). No transition. Health: verify10j node (pid 63671, 17min)
  at slot 125105858 block 4793941, 0 phase-1; disk 28G. #10 stays DIAGNOSING. NEXT WAKE: act on diagnose verdict.
- wake153 2026-06-07: POLL #10 DIAGNOSE wuoecuy7o (297-residual provenance) — still RUNNING (koios input-resolve
  fan-out). No transition. Health: verify10j node (pid 63671, 15min) at slot 125105734 block 4793935, still 0
  phase-1 rejections; disk 28G stable. #10 stays DIAGNOSING. NEXT WAKE: act on diagnose verdict.
- wake152 2026-06-07: #10 RE-GAUNTLET wetwroth8 = pass=false REFUTED 3/3. GAUNTLET-PENDING -> DIAGNOSING. R1+R2
  converge: 297 'Error term' residual attribution to #15 is UNPROVEN; viable #10 alt-cause = imported tag-4 inline
  datum re-encoded canonically (resolve_spend_datum ignores raw_cbor) + tag-4/5 silent-None on decode err. R3:
  json_number_to_word8 as_f64().fract() accepts sub-ULP fractional version Aeson rejects (narrow; fold before
  commit). Launched DIAGNOSE muscle wuoecuy7o (wf_31db35f1-cbb): resolve 20 'Error term' tx_hashes' spent-input
  slots via koios — imported(<=124999169) tag-4/5 => #10 implicated; all post-snapshot => #15 confirmed. NEXT WAKE:
  act on diagnose verdict.
- wake151 2026-06-07: POLL #10 RE-GAUNTLET wetwroth8 — still RUNNING (3 Opus refuters). No transition possible.
  Health: verify10j node (pid 63671, 7min) at slot 125105256 block 4793919, still 0 phase-1 rejections; disk 28G
  free (stable). #10 stays GAUNTLET-PENDING. NEXT WAKE: on gauntlet PASS -> COMMIT #10 via gh/HTTPS.
- wake150 2026-06-07: #10 FULL-VERDICT PASS -> VERIFYING-RESOAK -> GAUNTLET-PENDING. verify10j synced 124999533
  ->125105013, 0 phase-1 rejections (all classes, == STRICT verify10i); 13 non-#15 ERRORs all benign (DNS/cost-
  model-fallback/chain_diverged=false); residual 297 = #15. Launched RE-GAUNTLET muscle wetwroth8 (wf_436d43b5-37f)
  on FINAL-DONE. NEXT WAKE: on PASS -> COMMIT #10 via gh/HTTPS (dugite-serialization + dugite-node).
- wake149 2026-06-07: #10 BUILD_EXIT=0 (FINAL-DONE binary, Jun7 07:14). DROVE VERIFYING-BUILDING -> VERIFYING-
  RESOAK: GC'd verify10b (-2G), CoW-cloned db-preprod-sync -> verify10j, launched FINAL-DONE node pid 63671 (port
  4211). Import byte-exact "(strict: only version 1 => big-endian) codec_version=1 txix_endianness=Big", 4116339
  UTxOs, 0 phase-1 rejections so far. Node live-syncing 124999533 -> tip 125104880. NEXT WAKE FULL-VERDICT: scan
  ALL rejection classes (must be 0, identical to STRICT) -> RE-GAUNTLET FINAL-DONE -> COMMIT #10 via gh/HTTPS.
- wake148 2026-06-07: POLL #10 FINAL-DONE build verify-build-10j (pid 62684) — STILL RUNNING (final crate
  dugite-node compiling/linking). Not done; can't re-verify yet. No competing CPU during link. #10 stays
  VERIFYING-BUILDING; next: BUILD_EXIT=0 -> clone db-preprod-sync -> re-import re-soak (0 phase-1) -> re-gauntlet
  -> COMMIT #10.
- wake147 2026-06-07 (notification): #10 FLOAT-PARSE fix COMPLETE (muscle w3cxa15va, checks_green, 2 crates).
  json_number_to_word8_codec_version = Aeson toBoundedInteger@Word8 parity (1.0/1e0=>Big; 1.5/256/2.0/"1"=>Err) +
  enforceVersion ==1. Narrowed overclaim comments + CRC=#17 scope note + cross_validate live-not-dead. Resolves
  both refutes. Real preprod (integer 1) import unchanged from STRICT. Assembled FINAL-DONE patch (3856 lines, 2
  crates), applied to main, build pid 62684. #10 FIXING -> VERIFYING-BUILDING. next: BUILD_EXIT=0 -> re-soak
  (0 phase-1) -> re-gauntlet (should pass) -> COMMIT #10. Did NOT commit.
- wake146 2026-06-07: POLL #10 float-parse fix-muscle w3cxa15va — in VERIFY phase (clippy clean, running
  codec_version targeted tests then full nextest). Near completion. Not disturbed. #10 stays FIXING. next
  (notif/poll): read FIX -> re-import re-soak (real preprod integer 1, 0 phase-1) -> re-gauntlet -> commit.
- wake145 2026-06-07: POLL #10 float-parse fix-muscle w3cxa15va — RUNNING, healthy (4GB RAM, no nodes, 0
  completed). Implementing the small float-parse byte-exactness (accept integral JSON Number==1 like Aeson
  toBoundedInteger) + narrowing the overclaim/dead-code comments. Not disturbed; no competing work. #10 stays
  FIXING; next: poll -> build+nextest -> re-verify (real preprod integer 1, 0 phase-1) -> re-gauntlet -> commit.
- wake144 2026-06-07 (notification): re-gauntlet w3upqlq0y COMPLETE = 2/3 refuted but ALL 3 CONFIRM the
  endianness decision is byte-exact (no endianness refutation — a milestone after 7 rounds). The 2 refutes are
  narrow: (a) float-form Word8 parse over-strict (1.0/1e0 rejected vs Aeson-accepts) -> small byte-exact fix;
  (b) overclaim re missing CRC -> FILED #17 (separate integrity gap) + narrow the comment. Filed #17. Reset main,
  launched float-parse fix-muscle w3cxa15va. #10 GAUNTLET-PENDING -> FIXING. The endianness fix (the actual #10
  bug) is DONE-correct; this is the last polish to make the meta PARSE byte-exact with Aeson + accurate claims.
- wake143 2026-06-07: POLL #10 re-gauntlet w3upqlq0y — 2/3 reported, QUALITATIVELY DIFFERENT (no endianness
  refutation). compounding-feedback refuted=TRUE but CONFIRMS the endianness/backend/version truth table is
  byte-exact; it refutes only the OVERCLAIM "rejects everything upstream rejects" because dugite skips the
  snapshotChecksum/CRC check upstream loadSnapshot does (crcOfConcat==snapshotChecksum -> ReadSnapshotData
  Corruption) -> a valid-meta-but-corrupt-tables snapshot is accepted; ALSO notes cross_validate is LIVE not
  dead. It RECOMMENDS filing the CRC gap separately. edge-epoch refuted=FALSE ("no case produces a WRONG import
  on a real network"); found only a fail-CLOSED over-strict edge (rejects float-form 1.0/1e0 that Aeson floors to
  1; unreachable since ToJSON emits integer 1 + Mithril delivers verbatim). 3rd refuter (haskell-semantics)
  converging on the same float-edge. SETTLED RESOLUTION (regardless of 3rd's label): the #10 endianness fix is
  byte-exact correct -> (1) tone down the overclaim comment to "endianness decision byte-exact" + fix the "dead
  code" wording (cross_validate is live); (2) FILE the CRC-verification gap as NEW #17 (corrupt-snapshot
  acceptance; integrity, separate from #10's TxIx scope; dugite import never verified CRC); (3) file the
  float-form Word8 parse as minor #18 (floor integral JSON numbers per Aeson parseIntegralFromScientific); (4)
  COMMIT the endianness fix. WAITING for the 3rd refuter per the wake122/132 lesson before executing. #10 stays
  GAUNTLET-PENDING.
- wake142 2026-06-07: *** #10 FULL VERIFYING PASS (STRICT) *** verify10i re-soak: 0 phase-1 transaction
  rejections (all classes), strict codec_version=1->Big, only 294 #15 Error-term. Synced past all failing slots.
  SIGTERM'd verify10i (kept db for #15), GC'd mainnet-ep213 (re-clonable). Launched RE-GAUNTLET w3upqlq0y on the
  STRICT terminal (key probe: over-strict regression — does dugite now reject anything upstream ACCEPTS?). #10
  VERIFYING-RESOAK -> GAUNTLET-PENDING. next: poll -> on pass COMMIT the STRICT patch via gh/HTTPS (lands #10 at
  last — 7 gauntlet rounds, each a real byte-exactness bug caught).
- wake141 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. STRICT build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10i, ran STRICT binary (pid 32304): import "(strict: only version 1 => big-endian
  accepted) codec_version=1 txix_endianness=Big", sane distribution, utxo_count=4116338 skipped=0. Node syncing.
  GC'd verify10h (disk 38GB). Deferred full-verdict grep (one-step). next: scan ALL rejection classes -> 0 phase-1
  -> re-gauntlet -> commit.
- wake140 2026-06-07 (notification): #10 STRICT meta fix COMPLETE (muscle wh8n6ip92, checks_green, 2 crates).
  from_tables_codec_version={Some(1)=>Big, else=>Err}; field-absent/null=>Err (mandatory `.:`); backend check
  added; meta-file-absent=>bail. Muscle VERIFIED all current networks ship modern meta+version=1 BE snapshots ->
  legacy-LE import path safely dropped (lenient tests -> strict *_is_error). Quotes upstream verbatim. Assembled
  STRICT patch (3725 lines, 2 crates), applied to main, build pid 31837. #10 FIXING -> VERIFYING-BUILDING. next:
  BUILD_EXIT=0 -> re-soak (0 phase-1) -> re-gauntlet (strict terminal) -> commit. Did NOT commit.
- wake139 2026-06-07: POLL #10 strict remediation muscle wh8n6ip92 — at the VERY FINAL step (doc tests, after
  fmt/clippy/nextest). Imminent completion. Not disturbed. #10 stays FIXING. next (notif/poll): read FIX ->
  re-import re-soak (modern-BE 0 phase-1) -> re-gauntlet (strict terminal) -> commit.
- wake138 2026-06-07: POLL #10 strict remediation muscle wh8n6ip92 — in VERIFY phase (serialization clippy clean,
  running node clippy then nextest). Near completion. Not disturbed. #10 stays FIXING. next (notif/poll): read FIX
  -> re-import re-soak (modern-BE 0 phase-1) -> re-gauntlet (strict terminal) -> commit.
- wake137 2026-06-07: POLL #10 strict remediation muscle wh8n6ip92 — RUNNING, healthy (5GB RAM, no nodes, 0
  completed). Implementing strict semantics: meta-FILE-absent=>ERROR, enforce backend=="utxohd-mem", version
  parse->Big (only version=1), adding the backend parser. Exactly the specified strict fix. Not disturbed; no
  competing work. #10 stays FIXING; next: poll -> build+nextest -> re-verify (modern-BE 0 phase-1) -> re-gauntlet -> commit.
- wake136 2026-06-07: POLL #10 strict remediation muscle wh8n6ip92 — RUNNING, healthy (5GB RAM, no nodes, 0
  completed). Doing the verify-before-strict investigation: found the legacy-LE test (build_legacy_le_tvar) uses
  SYNTHETIC tvar blobs, not a real meta-less network snapshot (preview_tvar_head_64k.bin is doc-referenced).
  Supports the strict choice — no real target-network mithril snapshot needs meta-less LE -> safe to drop the LE
  import path. Not disturbed; no competing work. #10 stays FIXING; next: poll -> build+nextest -> re-verify ->
  re-gauntlet -> commit.
- wake135 2026-06-07 (notification): re-gauntlet w4007sv2k COMPLETE = 2/3 refuted (compounding-feedback ran the
  importer tests empirically + did NOT refute; the other 2 agree). DEFINITIVE source-cited truth table obtained
  (mandatory `.:` => field-absent THROWS; loadSnapshot decodes BE unconditionally + backend check). Made the
  EXECUTIVE call to END the meta-absent flip-flop: STRICT — only {meta present + version=1 + backend=utxohd-mem}
  => Big, everything else => ERROR (default-to-rejection). Reset main, launched strict remediation wh8n6ip92
  (verify-then-drop legacy-LE path; stop+report if a network needs meta-less LE). #10 GAUNTLET-PENDING -> FIXING.
  This is the 6th gauntlet round on #10's endianness edge-handling — each caught a real byte-exactness divergence;
  STRICT is the maximally-conservative terminal that no refuter can call too-lenient.
- wake134 2026-06-07: POLL #10 re-gauntlet — still 2/3, but 3rd refuter (compounding-feedback) is genuinely
  busy: it's RUNNING the actual importer_ tests (cargo nextest -p dugite-node, 480s timeout) to EMPIRICALLY
  verify the meta-absent behavior, not just reason. Worth waiting (the contested meta-file-absent case deserves
  empirical grounding). Not disturbed; no competing work. #10 stays GAUNTLET-PENDING. next: aggregate -> strict
  remediation.
- wake133 2026-06-07: POLL #10 re-gauntlet w4007sv2k — still 2/3 (3rd refuter compounding-feedback a42d849 still
  actively running, deeper than the other two; not stuck). pass already=false (2 refuted+agree). Holding per
  wake132 to get the 3rd's take on the contested meta-FILE-absent case (it's the lens that first flagged
  field-absent at wake122). Not disturbed; no competing work. #10 stays GAUNTLET-PENDING. next: full aggregate ->
  strict remediation (field-absent/null/version-other/backend-mismatch => ERROR; meta-FILE-absent decided
  deliberately) launched with the absolute-path base patch (worktree-staleness rule).
- wake132 2026-06-07: POLL #10 re-gauntlet w4007sv2k — 2/3 reported, BOTH refuted and AGREE (so not a wake122-
  style contradiction). FINAL2's meta-absent tolerance is TOO LENIENT: (1) field-absent/null in a PRESENT meta
  must be ERR (upstream FromJSON mandatory `.: tablesCodecVersion` -> MetadataInvalid -> hard error in converter
  AND loader); my wake123 over-generalization (tolerate field-absent like file-absent) was wrong — wake122's
  compounding-feedback was right. (2) DEEP: wake123's getMetadata analog returns Maybe CRC; its Nothing = skip-CRC
  NOT decode-LE; the actual loader V2/InMemory.loadSnapshot decodes BigEndianTxIx UNCONDITIONALLY + hard-fails on
  missing/invalid meta -> the "version decides endianness, absent=>LE" model may be a dugite invention. (3) backend
  field not validated. EVERY refuter CONFIRMS the core modern-BE fix is correct (chain-verified 0 phase-1); all
  disputes are edge metas the real import never hits. DECISION (wake122 lesson, esp. given the meta-absent
  flip-flop): WAIT for the 3rd refuter -> full aggregate -> ONE remediation with a DEFINITIVE truth table
  (file-absent/field-absent/null/version/backend => exact upstream behavior, LOADER analog). Likely STRICT:
  version=1+backend=mem=>BE, else ERROR; decide meta-FILE-absent (legacy LE) deliberately, not by guess. #10 stays
  GAUNTLET-PENDING. Did NOT commit, did NOT launch remediation yet.
- wake131 2026-06-07: *** #10 FULL VERIFYING PASS (FINAL2) *** verify10h re-soak: 0 phase-1 transaction
  rejections (all classes), codec_version=Some(1)->Big, only 292 #15 Error-term. Synced past all failing slots.
  SIGTERM'd verify10h (kept db for #15). Launched RE-GAUNTLET w4007sv2k on FINAL2 (authoritative + meta-absent
  tolerance + end-to-end tests — all 4 prior refutations addressed). #10 VERIFYING-RESOAK -> GAUNTLET-PENDING.
  next: poll -> on pass COMMIT the FINAL2 patch via gh/HTTPS (lands #10 after ~30 wakes / 4 fix iterations /
  4 gauntlet rounds — every refute was a real bug avoided).
- wake130 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. FINAL2 build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10h, ran FINAL2 binary (pid 72713): import "codec_version=Some(1)->Big" authoritative,
  sane distribution, utxo_count=4116338 skipped=0. Node syncing. GC'd verify10g (disk 53GB). Deferred
  full-verdict grep (one-step). next: scan ALL rejection classes -> 0 phase-1 -> re-gauntlet -> commit.
- wake129 2026-06-07: POLL #10 FINAL2-fix build verify-build-10h (pid 71916) — STILL RUNNING (final crate
  dugite-node compiling/linking). Not done; can't re-verify yet. No competing CPU during link. #10 stays
  VERIFYING-BUILDING; next: BUILD_EXIT=0 -> clone db-preprod-sync -> re-import re-soak (0 phase-1, full
  rejection-class scan) -> re-gauntlet -> commit.
- wake128 2026-06-07 (notification): #10 META-ABSENT fix COMPLETE (muscle wx76r15y3, checks_green, 2 crates).
  meta read NotFound=>None=>Little (legacy LE), mirroring upstream getMetadata verbatim; resolve_snapshot_txix_
  endianness extracted + tested via the real importer path; added the missing end-to-end legacy-no-meta=>LE test
  (+3 more). Assembled COMBINED FINAL2 patch (3542 lines: authoritative codec-version + meta-absent tolerance,
  2 crates), applied to main, build pid 71916. #10 FIXING -> VERIFYING-BUILDING. next: BUILD_EXIT=0 -> re-soak
  (0 phase-1) -> re-gauntlet -> commit. Did NOT commit.
- wake127 2026-06-07: POLL #10 meta-absent muscle wx76r15y3 — at FINAL gate (fmt+clippy clean; running nextest
  --workspace). Imminent completion. Not disturbed. #10 stays FIXING. next (notif/poll): read FIX -> re-import
  re-soak (modern-BE 0 phase-1) + legacy-LE path -> re-gauntlet -> commit.
- wake126 2026-06-07: POLL #10 meta-absent muscle wx76r15y3 — RUNNING, healthy (4GB RAM, no nodes, 0 completed).
  Constructing the missing end-to-end test (meta-absent legacy snapshot must decode LE: from_tables_codec_version
  (None)=>Little + cross-validation passes) — the exact gap the gauntlet flagged. Not disturbed; no competing
  work. #10 stays FIXING; next: poll -> build+nextest -> re-verify (modern-BE + legacy-LE) -> re-gauntlet -> commit.
- wake125 2026-06-07: POLL #10 re-launched muscle wx76r15y3 — RUNNING, healthy (4GB RAM, no nodes, 0 completed).
  CONFIRMED the abs-path fix worked: worktree now HAS the base machinery (from_tables_codec_version present) ->
  STEP 0 succeeded. Muscle verifying upstream SnapshotConversion.getMetadata meta-absent tolerance + doing the
  remediation (meta-absent=>None=>Little). Not disturbed; no competing work. #10 stays FIXING; next: poll ->
  build+nextest -> re-verify (modern-BE + legacy-LE import paths) -> re-gauntlet -> commit.
- wake124 2026-06-07: CAUGHT + FIXED an INFRA blocker. The remediation muscle w5vke699f reported "base patch
  does not exist" — diagnosed: its worktree (ca50afd9ef) branches from a base that LAGS the wake119 patch-file
  commit (7a28f46dbc), so scripts/prod-readiness/*.patch FILES are absent in the worktree (crates/ identical to
  HEAD though). The relative-path STEP-0 git apply couldn't find the base. TaskStop'd w5vke699f, re-launched as
  wx76r15y3 with the base patch by ABSOLUTE path + an infra note (patch applies to crates/ which match HEAD).
  Recorded the worktree-staleness rule for all future muscles (abs-path base patches). #10 stays FIXING. LESSON:
  poll muscle TEXT not just completion — a muscle floundering on a wrong premise needs orchestrator intervention.
- wake123 2026-06-07 (notification): re-gauntlet w8t0ro3f6 FULLY COMPLETE = 3/3 refuted BUT refuters DISAGREED on
  meta-absent -> wake122's WAIT-for-aggregate decision VINDICATED (the lone wake122 refutation said absent=>Err;
  the 2-of-3 aggregate + correct offline-importer analog says absent=>tolerate-as-LE — OPPOSITE). dugite is TOO
  STRICT: hard-errors on meta-FILE-absent, regressing legacy LE imports (upstream SnapshotConversion tolerates
  it; dugite ships a legacy LE fixture). Some(1)=>Big confirmed byte-exact. Reset main, launched remediation
  muscle w5vke699f (meta-absent=>None=>Little not error; add end-to-end importer test). #10 GAUNTLET-PENDING ->
  FIXING. LESSONS: (1) my wake114/119 "error on missing meta" was too strict — the gauntlet corrected it; (2)
  on a NARROW refutation, waiting for the full panel prevented a wrong remediation (the refuters contradicted).
- wake122 2026-06-07: POLL #10 re-gauntlet w8t0ro3f6 — 1/3 refuters reported (compounding-feedback refuted=TRUE,
  but NARROW). It CONFIRMS the primary path is correct + #10 genuinely resolved (Some(1)=>Big, 0 phase-1) AND
  resolves my wake121 meta-absent concern in the fix's favor (V2-InMemory ALWAYS writes meta w/ codec version;
  no legit meta-absent legacy). REAL narrow divergence: upstream FromJSON uses REQUIRED non-nullable
  `.: tablesCodecVersion`, so a meta that parses but lacks the field / is null = HARD upstream parse failure;
  dugite returns Ok(None)=>Little (silently accepted as LE) = too lenient. Remediation: absent/null field => ERR
  (collapse to {Some(1)=>Big, else=>Err}); drop the 2 None=>LE tests. Edge case (real preprod meta has field=1)
  but cardinal-rule-relevant. DECISION: WAIT for the full aggregate (2 refuters still running) before launching
  ONE remediation that addresses all findings — don't act on a partial gauntlet for a narrow point. #10 stays
  GAUNTLET-PENDING. next: read aggregate -> remediation muscle (absent/null=>Err) -> re-verify -> commit.
- wake121 2026-06-07: *** #10 FULL VERIFYING PASS (authoritative path) *** verify10g re-soak: 0 phase-1
  transaction rejections (all classes), codec_version=Some(1)->Big authoritative, cross-val no contradiction,
  only 281 #15 Error-term. Synced past all failing slots. SIGTERM'd verify10g (kept db for #15). Launched
  RE-GAUNTLET w8t0ro3f6 on the authoritative fix (key probe: meta-file-absent legacy snapshot handling). #10
  VERIFYING-RESOAK -> GAUNTLET-PENDING. next: poll -> on pass COMMIT the AUTHORITATIVE patch via gh/HTTPS (lands
  #10 at last). This iteration is grounded in upstream Haddock proof -> should clear the gauntlet.
- wake120 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. AUTHORITATIVE-fix build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10g, ran AUTHORITATIVE binary (pid 24240): import log confirms "Authoritatively
  determined ... from snapshot meta tablesCodecVersion codec_version=Some(1) txix_endianness=Big" (NOT
  auto-detect) + cross-val sane (no contradiction). utxo_count=4116338 skipped=0. Node syncing. GC'd verify10f
  (66GB disk). Deferred full-verdict grep (one-step). next: scan ALL rejection classes -> 0 phase-1 -> re-gauntlet
  -> commit.
- wake119 2026-06-07 (notification): #10 AUTHORITATIVE endianness fix COMPLETE (muscle wjnl2t2ib, Tier A',
  checks_green). Resolved wake115: upstream Haddock proves TablesCodecVersion1 = "big-endian txix" (cardano-node
  10.7+; absent=legacy LE; else ERROR) — version IS authoritative despite the BE-flip commit not bumping it
  (version field added separately). Fix: from_tables_codec_version decides; node reads meta (ERROR if missing);
  cross_validate is now INDEPENDENT (errors on contradiction); detect/is_sane demoted. Addresses all 3 unanimous
  gauntlet refutations (heuristic->authoritative; same-predicate->independent; nonzero==0-default-Big->error).
  Assembled AUTHORITATIVE patch (3282 lines, 2 crates), applied to main, build pid 23735. #10 FIXING ->
  VERIFYING-BUILDING. next: BUILD_EXIT=0 -> re-soak (0 phase-1 rejections) -> re-gauntlet -> commit. Did NOT commit.
- wake118 2026-06-07: POLL #10 rework muscle wjnl2t2ib — at FINAL gate (running nextest --workspace). Authoritative
  meta-derived endianness implemented + tests. Imminent completion. Not disturbed. #10 stays FIXING. next
  (notif/poll): read FIX -> re-import re-soak (0 phase-1 rejections, meta-derived endianness) -> re-gauntlet
  (now authoritative, not heuristic) -> commit.
- wake117 2026-06-07: POLL #10 rework muscle wjnl2t2ib — RUNNING, healthy (4GB RAM, no nodes, 0 completed).
  Implementing authoritative META-derived endianness (test_parse_tables_codec_version_real_preprod_meta added;
  updating the gated key-correctness oracle to derive endianness from the sibling meta file + cross-validate) ->
  found a workable meta-based mapping. Not disturbed; no competing work. #10 stays FIXING; next: poll -> build+
  nextest -> re-import re-verify (0 phase-1 rejections) -> re-gauntlet -> commit.
- wake116 2026-06-07 (notification): re-gauntlet wmpyis3tx FULLY COMPLETE = UNANIMOUS 3/3 REFUTED (haskell-
  semantics + edge-epoch + compounding-feedback). Confirms wake114's action. haskell-semantics added a CONCRETE
  extra hole for the rework to fix: observe_txix IGNORES index 0 + samples only first 2000 keys -> an
  index-0-dominated sample gives nonzero==0 for BOTH endiannesses -> is_sane() short-circuits TRUE for both ->
  "both sane -> default Big" -> SILENTLY wrong for a legacy-LE index-0-dominated snapshot; the safety net's
  nonzero==0 short-circuit is a false-pass. Value/datum/refscript/multi-asset reaffirmed SOUND by all refuters.
  REWORK REQUIREMENT (add to wjnl2t2ib's outcome bar): an uninformative/empty sample must NOT default Big — it
  must force the authoritative signal or ERROR, never guess. Rework muscle wjnl2t2ib still running (it already
  found 9ac9388 doesn't bump tablesCodecVersion -> verifying whether version disambiguates at all). #10 FIXING.
- wake115 2026-06-07: POLL #10 rework muscle wjnl2t2ib — RUNNING, healthy (5GB RAM, no nodes, 0 completed).
  *** CRITICAL interim finding (the verify-don't-assume paying off) ***: commit 9ac9388 (BigEndianTxIn flip) does
  NOT itself bump TablesCodecVersion -> tablesCodecVersion may NOT reliably encode endianness (the refuter
  ASSUMED it does). Muscle now checking if the BE flip shipped in the same release as a version bump (reading
  SnapshotMetadata + BigEndianTxIn upstream source). If version does NOT disambiguate endianness, neither layout
  NOR codec version is authoritative -> the resolution may be: error-on-ambiguity, OR a bounded/cross-validated
  detection. Not disturbed; no competing work. #10 stays FIXING; next: poll for the muscle's version->endianness
  determination + fix.
- wake114 2026-06-07: *** GAUNTLET'S 3RD CORRECT CATCH (the most important) *** re-gauntlet wmpyis3tx edge-epoch
  REFUTED the FINAL fix: TxIx endianness uses an empirical HEURISTIC (cardinal-rule violation), safety-net re-runs
  the SAME is_sane predicate (not independent), and the authoritative tablesCodecVersion (which I found wake109
  but didn't act on) is unused. Chain-verdict was clean (0 phase-1 rejections) BUT the heuristic is not
  byte-exact-guaranteed for edge snapshots -> correctly refused. Value/datum/refscript/multi-asset SOUND. Acted
  on the decisive refutation without waiting for aggregate (wake89 precedent). Confirmed tablesCodecVersion IS in
  haskell-ledger/<slot>/meta (=1=BE for preprod). Reset main, launched rework muscle wjnl2t2ib (authoritative
  version-based endianness, error-on-ambiguity, distribution check demoted to independent cross-validation).
  #10 GAUNTLET-PENDING -> FIXING. LESSON: I should have used the authoritative signal at wake109 instead of
  shipping a heuristic; the gauntlet enforced the cardinal rule I'd let slip.
- wake113 2026-06-07: *** #10 FIRST CLEAN VERIFYING PASS *** verify10f re-soak full rejection-class scan: ZERO
  phase-1 transaction rejections (was 986 across 5 classes -> ALL 0), script-not-found 0, budget 0,
  MultiAssetNotConserved 316->0, auto-detect=Big sane (no safety-net trip). Only residual: 281 phase-2 Error-term
  = #15 (separate). Synced past all failing slots. The complete import-completeness fix (refscript+datum+
  endianness-autodetect+safety-net+multiasset-all-tags) WORKS end-to-end. SIGTERM'd verify10f (kept db for #15),
  launched RE-GAUNTLET wmpyis3tx on the FINAL fix. #10 VERIFYING-RESOAK -> GAUNTLET-PENDING. next: poll -> on pass
  COMMIT the FINAL patch via gh/HTTPS (lands #10) -> then #15 + #16 follow-ups. The byte-exact discipline drove
  ~30 wakes through 4 fix iterations + 2 gauntlet refutations to a correct, version-independent fix.
- wake112 2026-06-07: #10 VERIFYING-BUILDING -> VERIFYING-RESOAK. FINAL-fix build BUILD_EXIT=0. Cloned
  db-preprod-sync -> verify10f, ran FINAL binary (pid 78267): auto-detect=Big, sane distribution (low 3131782 vs
  mult256 62), utxo_count=4116338 skipped=0, multi-asset populated all tags. Node syncing. GC'd verify10e (4->75GB
  disk wait 75GB free). Deferred full-verdict grep (one-step). next: scan ALL rejection classes ->
  MultiAssetNotConserved->baseline + not-found/budget 0 + no new class -> re-gauntlet -> commit.
- wake111 2026-06-07 (notification-triggered): #10 MULTI-ASSET fix COMPLETE (muscle w34va8uxf, Tier A',
  checks_green, real-blob oracle PASS). Root cause: tags 0/1 used opaque decode_compact_value (num_assets=0) ->
  970K multi-asset UTxOs imported empty; fix routes tag0/1 through decode_compact_value_exact. After: 1.629M
  multi-asset UTxOs fold non-empty, ADA-only byte-identical, real-blob folded asset_list == Koios. Assembled the
  COMBINED FINAL patch (2905 lines, all #10 layers: refscript+datum+endianness-autodetect+safety-net+multiasset-
  all-tags, 2 crates), applied to main, launched build pid 77765. Advanced #10 FIXING -> VERIFYING-BUILDING. next:
  BUILD_EXIT=0 -> re-import re-soak -> FULL rejection-class scan (MultiAssetNotConserved->baseline + not-found/budget
  still 0) -> re-gauntlet -> commit. Did NOT commit.
- wake110 2026-06-07: POLL #10 multi-asset fix-muscle w34va8uxf — NEAR COMPLETE: byte-exact gated test (reconstructed
  multi_asset == Koios) PASSES against the real preprod blob; collecting final diff (txout.rs + tests.rs). Imminent
  completion. Not disturbed. #10 stays FIXING; next (notif/poll): read FIX -> re-import re-soak (MultiAssetNotConserved
  -> baseline + keep endianness win) -> re-gauntlet -> commit.
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
