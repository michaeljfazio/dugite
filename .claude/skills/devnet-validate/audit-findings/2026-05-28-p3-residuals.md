# P3 follow-ups from 2026-05-28 session

These items did not block Round 2 or Round 3 PASS but are worth a future
focused investigation. They are listed in priority order.

## P3-1: 22.14B-lovelace reserves diff at boundary 2→3

**Observation**: After the Conway-from-genesis RUPD fix (`037c464ea`),
boundary 0→1 and boundary 1→2 are byte-exact with Haskell on both
treasury and reserves. At boundary 2→3, treasury still matches
byte-exact (7,197,832,802,160 each), but reserves diverge by
22,140,531,700 lovelace (~22K ADA, 0.0004% of the pot).

**Math at boundary 2→3 (devnet k=40 f=0.5 epoch_len=400 rho=0.003 tau=0.2)**:
- reserves at start = 5,996,394,003,600,000
- expansion = floor(rho × reserves) = 17,989,182,010,800
- treasury_cut = floor(tau × expansion) = 3,597,836,402,160
- ssStakeGo at this RUPD: mempty (snapshot lineage hasn't rotated stake into go yet — first per-pool distribution lands at boundary 3→4)
- expected reserves delta = treasury_cut = 3,597,836,402,160
- dugite reserves delta = 3,597,836,402,160 ✓ matches formula
- Haskell reserves delta = 3,619,976,933,860 (22.14B more than expected)

**Hypothesis**: the 22.14B is some non-zero `frTotalUnregistered` or
"reward to credentials deregistered between snapshot and apply" path
that fires in Haskell at boundary 2→3 but not at 1→2. Possibly the
genesis stake delegations are being deregistered or recategorized at
the SNAP rotation that introduces the first non-mempty `set`, and
Haskell forwards those snapshot-time-only rewards to treasury (which
shows up as MORE reserves decrease).

**To resolve**:
1. Capture a Haskell `cardano-cli debug log-epoch-state` dump at
   boundary 2→3 of a Conway-from-genesis devnet
2. Diff `_rewardUpdate.deltaR.unCoin` + `_rewardUpdate.rs` against
   dugite's `compute_reward_update` outputs at the same boundary
3. Verify whether Haskell's per-pool `rewards` map is empty at 2→3 OR
   whether it contains entries for unregistered credentials
4. If the latter, find the snapshot-stale-cred handling and mirror it

**Magnitude**: 22.14B / 3,597,836,402,160 ≈ 0.6%. Does NOT compound
across boundaries based on the data we have — at boundary 2→3 only
this single 22B residual appears, not 44B at 3→4 then 66B at 4→5.
Need to run a longer soak (≥30 min for 5 boundaries) with epoch-state
dumps to confirm.

**Why not P2/P1**: this is below the SKILL.md Round 2 PASS criterion
boundary (1→2 byte-exact), Round 2 is the ONLY round that compares
pots vs Haskell, and Round 2 evidence already shows GREEN end-of-round
pots since the soak doesn't cross boundary 2→3 (15-min soak / 400-slot
epoch = 2 boundaries max).

## P3-2: ~25 🔍 entries in Conway-LEDGER-predicate audit doc

**Doc**: `audit-findings/2026-05-28-conway-ledger-predicate-audit.md`

**Status**: enumerated all ~88 Haskell Conway leaf predicate failures
across 10 enums; ~25 marked 🔍 for follow-up (mostly DELEG sub-predicates,
DRep cert sub-predicates, Pool/Gov sub-predicates).

**To resolve**: for each 🔍 entry:
- Identify the dugite code path that should produce the failure
- Construct a minimal devnet test case that triggers the condition
- Verify dugite rejects with the equivalent error (or document why it
  doesn't apply)

**Why not P0**: most are obscure predicates that have never fired in
the wild on preview/preprod. Surfacing them all would need a
property-based test generator that constructs txs designed to fail
each predicate.

## P3-3: Multi-BP forge attribution

Round 2/3 evidence p2:per-bp-attribution detects validator-only mode
(pool2 has no opcert) and special-cases the PASS criterion as
"pool1≥3 AND pool2=0". When the devnet topology is changed to add a
second forging pool, p2 reverts to "both pools ≥3" — but that criterion
is hard to meet in 15 min of soak because 5% stake takes a while to
hit a leader slot.

**To resolve**: add a third PASS branch for "multi-pool but small
stake-share pool zero forges within soak window" so future devnet
topology changes don't trip a false-fail.

## P3-4: Audit doc maintenance

The audit-findings/ dir now has multiple resolved findings without a
clear index. Future sessions need to know which ones to read first.

**To resolve**: add an INDEX.md to `audit-findings/` summarizing each
finding's status (RESOLVED / P2 / P3 / OPEN) with the resolving commit.

---

These are not goals for the next session unless explicitly prioritized.
