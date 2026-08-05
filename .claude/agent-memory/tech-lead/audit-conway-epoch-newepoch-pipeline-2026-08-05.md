---
name: audit-conway-epoch-newepoch-pipeline-2026-08-05
description: Full NEWEPOCH-pipeline audit (epoch.rs + rewards.rs + governance.rs epoch-boundary code) — 3 issues filed (#1015-#1017), 2 fixed same-session, extensive clean negatives
metadata:
  type: project
---

Ran a dedicated audit of `crates/dugite-ledger/src/state/epoch.rs` and the
surrounding reward/pot/governance-boundary machinery against Haskell's
NEWEPOCH pipeline (TICK -> RUPD -> NEWEPOCH -> MIR/EPOCH -> SNAP -> POOLREAP
-> RATIFY/ENACT), in worktree `audit-conway-epoch`. Two independent parallel
sibling audits ran concurrently (Phase-1 validation, LSQ query surface) — this
one stayed scoped to `state/` epoch machinery.

## Found and fixed (same session, RED-then-GREEN verified)

- **#1017** — `LedgerState::process_epoch_transition` (the `#[doc(hidden)]`
  test-only boundary helper) never called `prune_committee_state`, unlike
  production `ConwayEraRules::process_epoch_transition`
  (`eras/conway.rs:893`). No production impact (production already called
  it) — a test/production drift gap, the same N-copies shape as #977/#985.
  Fixed by adding the call + a regression test driven through
  `process_epoch_transition` itself (not the already-tested
  `prune_committee_state` directly).
- **#1016** — a `rewards.rs` comment above `total_active_stake` (1) wrongly
  attributed the `sumAllActiveStake`-alignment fix to issue #898 (that's the
  unrelated Mithril gov-roots bug — the fixing commit `5c9d833b52` itself
  says its test "ruled the reward formula out as the cause of #898") and (2)
  claimed POOLREAP leaves retiring pools' delegations dangling, which is
  false for cardano-node 11.0.1's actual POOLREAP (clears delegations in the
  SAME transition it removes the pool — see
  [[poolreap-active-purge-vs-dangling-11-0-1]]). Comment-only fix; the
  formula itself (`pool_stake` summed unfiltered by `pool_params`
  membership) is correct regardless, since a `StakeDelegation` cert is never
  validated against pool existence.

## Filed, not fixed

- **#1015** — `babbage.rs::process_epoch_transition` delegates wholesale to
  `ShelleyRules::process_epoch_transition`, which folds `extraEntropy` into
  the epoch-boundary nonce combine (3-term TPraos TICKN formula). The real
  Praos formula (`tickChainDepState`, `Praos.hs`, used by
  Babbage/Conway/Dijkstra) is 2-term (`candidate ⭒ lastEpochBlockNonce`) and
  never references extraEntropy — `ppExtraEntropy` is structurally absent
  from Babbage+ `PParams` (`notSupportedInThisEraL`). Conway's own
  hand-written implementation already has the correct 2-term formula;
  Babbage's does not, because it reuses Shelley's function unmodified.
  Currently dormant on every live network (extra_entropy can only be set by
  a legacy pre-Conway PPU, and the one historical mainnet occurrence, epoch
  259, reset long before any network's Babbage HF) — but latent and
  consensus-critical (VRF leader schedule) if a chain ever carries a
  non-neutral value across the Alonzo->Babbage boundary. Needs either
  extracting the nonce-combine into an overridable hook or a post-hoc
  recompute in babbage.rs — out of "small/safe" bar for a same-session fix.

## Extensively verified as CORRECT (clean negatives, oracle cross-checked)

- Reward formula chain: `maxPool'` (byte-exact match to Haskell f1-f4),
  sigma vs sigmaA distinction, pledge gate (`<=` on go-snapshot pledge vs
  self-delegated stake), 3-stage floor chain, leader-vs-member asymmetry at
  `poolR <= cost`, pv<=2 Shelley single-reward-selection vs pv>2 aggregation.
- Zero-block-pool gate: dugite's early `continue` on
  `bprev_blocks_by_pool[pool_id]==0` is NOT a divergence — Haskell's
  `mkPoolRewardInfo` has the identical gate one level deeper
  (`Map.lookup ... Nothing -> Left`), so a zero-block pool gets nothing even
  under `d>=0.8` (appPerf=1 is genuinely unreachable for it). Also confirmed
  dugite's overlay-slot exclusion in `epoch_blocks_by_pool` accumulation
  (`eras/common.rs::compute_shelley_nonce`) matches Haskell `incrBlocks`.
- RATIFY: expiry-vs-threshold ordering (#990 — threshold check first, expiry
  recorded only in the not-ratified branch) confirmed byte-exact against
  `ratifyTransition`. DRep voting-power composition (#949/#991 —
  InstantStake+ProposalDeposits+AccountBalance, counted once) confirmed.
  `check_cc_approval` zero-threshold/min-size ordering confirmed. SPO stake
  source (the `set<-mark` rotation trick in `compute_pulsed_ratify_state`,
  reproducing Haskell's `ssStakeMarkPoolDistr snapshots1`) confirmed correct.
- DRep pulser: single `Option<DRepPulsingState>` field (#988 consolidation)
  confirmed still holds structurally — no torn/independent fields reappeared.
- `updateNumDormantEpochs` (dugite: `proposals.is_empty()`) reasoned
  equivalent to Haskell's fuller `OMap.filter (expiresAfter>=currentEpoch)`
  check, GIVEN dugite's pruning (`proposals_apply_enactment`) already runs
  earlier in the same boundary — no proposal present at the check point can
  already be expired. DRep dormant-epoch "bump-all + reset" mechanism
  (`updateDormantDRepExpiry`, fires on any tx with proposal-procedures)
  confirmed implemented in `eras/conway.rs` (already fixed a prior
  unbounded-growth bug per its own comments).
- POOLREAP: SNAP-before-POOLREAP ordering, unregistered-refund->treasury,
  future-pool-params merge precedence, deposit-at-registration-time
  immutability (re-registration does NOT touch `pool_deposits`) — all
  confirmed against cardano-node 11.0.1's actual pinned commit (traced via
  CHaP: cardano-ledger-shelley 1.18.1.0 @ `b7c17cf3...`), not just master
  HEAD.
- Deposits/obligation: `totalObligation` = stake+pool+drep+proposal (no
  committee-deposit category exists), proposal-deposit refund destination
  (registered account vs treasury on unregistered), and the
  expiry-vs-ratification off-by-one (#990) all confirmed byte-exact against
  the real `returnProposalDeposits`/`ratifyTransition` source.
- MIR pot-transfer (#803), snapshot mark/set/go rotation order and timing,
  prevPParams capture-before-enactment timing (#685) — all re-verified
  correct, no new issues.

## Methodology notes worth keeping

- **Version-pin discipline mattered here.** A fresh oracle query against
  cardano-ledger `master` HEAD gave a DIFFERENT (and, it turned out,
  correct-for-11.0.1) answer than what an older, pre-refactor-era memory
  note assumed (`StakePoolSnapShot`/`spssStake` UMap-era naming vs
  `StakePoolState`/`spsDelegators` post-refactor naming). Don't trust
  "master HEAD" as a proxy for "what real nodes run" without checking —
  resolved two independent ways: (1) this repo's OWN
  `tests/conformance/upstream/sources.toml` cardano-ledger pin
  (`a88b60bdcf3248dfe5a2f9372c188c399233f479`, fetched directly — already
  has the refactor), and (2) an oracle CHaP-trace from cardano-node 11.0.1's
  actual release tag through `cardano-testnet.cabal` ->
  `cardano-api` -> `cardano-ledger-shelley` index-state resolution (see
  cardano-haskell-oracle's `chap-dependency-pinning-methodology.md`). Both
  agreed. When a finding's correctness genuinely hinges on which upstream
  commit is "real," check the project's own conformance-corpus pin first —
  it's usually the fastest way to get an answer that's actually gated on
  something (CI downloads that exact tag).
- A git commit message can directly refute a code comment written in the
  same commit's diff, if the comment was later hand-edited to sound more
  certain than the commit intended (#1016 was found exactly this way — `git
  log -S` on the defining symbol, then reading the ORIGINAL commit message
  side by side with the CURRENT comment text).
