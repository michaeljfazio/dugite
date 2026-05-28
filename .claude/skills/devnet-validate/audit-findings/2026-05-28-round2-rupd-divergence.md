# Round 2 — P0 RUPD divergence at boundary 1→2 (devnet k=40)

**Status: RESOLVED 2026-05-28 in commit `037c464ea`**

Root cause was two latent bugs in the Conway-from-genesis init path:
1. `LedgerState::new` defaults `prev_d=1/1` (Shelley overlay convention) — Conway has d=0, so the overlay branch fired and mis-attributed 3.6T at boundary 0→1
2. `finalize_genesis_state` pre-fills `snapshots.mark/set` with genesis stake — that snapshot rotates into `ssStakeGo` at boundary 1→2 and mis-distributes ~22 ADA in per-pool rewards

Fix applied in `Node::init_ledger_state` (Conway-from-genesis only, PV >= 9):
- override `prev_d = 0/1`, `prev_protocol_version_major = PV`
- clear pre-filled `snapshots.mark`/`set` back to None
- `pool_distribution_for_slot` now falls back to live-state when snapshots are None (so forge eligibility still works in epoch 0)
- Forge path now refreshes governance metrics (was the cause of stale `dugite_treasury_lovelace`)

Final Round 2 verification: byte-exact treasury + reserves parity vs Haskell at boundaries -1→0, 0→1, 1→2. All Round 2 PASS criteria green (verify.sh 5/5, analyze-evidence NO ANOMALIES, metric-audit 30/0 consistent, health-probe HEALTHY).

---


**Surfaced**: 2026-05-28 by Round 2 with `securityParam = 40` (so the Praos RUPD pulser `4k/f=320` fits the 400-slot epoch).

**Latent**: this bug has existed in dugite for an unknown time. Previously hidden because:
- devnet had `k=60` → `4k/f=480 > epoch_len=400` → pulser never started → Haskell also produced T=0 → "byte-exact parity"
- Preview/preprod ledger-replay validation (issue #438) covered boundaries **3–500**, not the first two

## Failing predicate (SKILL.md Round 2 PASS criteria)

> `dugite_treasury_lovelace` **byte-exactly equals** `cardano-bp.esChainAccountState.treasury` AND `dugite_reserves_lovelace` **byte-exactly equals** `cardano-bp.esChainAccountState.reserves`

## Observation

| Anchor | Slot | Block | Dugite T | Dugite R | Haskell T | Haskell R | Parity |
|---|---|---|---|---|---|---|---|
| epoch 0 init | 37 | 14 | 0 | 5,999,994,000,000,000 | 0 | 5,999,994,000,000,000 | OK (genesis) |
| boundary 0→1 | 420 | 198 | 0 | 5,999,994,000,000,000 | 0 | 5,999,994,000,000,000 | **OK** (RUPD doesn't fire here in either impl) |
| boundary 1→2 | 812 | 389 | **0** | **5,999,994,000,000,000** | **3,437,997,599,402** | **5,996,556,007,587,608** | **DIVERGE** |
| post-soak | 973 | 474 | 0 | 5,999,994,000,000,000 | 3,437,997,599,402 | 5,996,556,007,587,608 | DIVERGE (no further boundary) |

**Δtreasury** = +3,437,997,599,402 lovelace (~3.44M ADA) at Haskell
**Δreserves** = −3,437,992,412,392 lovelace (~3.44M ADA) at Haskell
Difference ~5,187,010 lovelace = the per-pool reward distributed inside Haskell (consistent with one pool collecting a tiny payout based on the GO snapshot's stake).

Expected from the math (params: rho=0.003, tau=0.2, reserves≈6e15, prev_d=0, f=0.5, epoch_len=400, actual_blocks=198):
- `effective_blocks = min(198, 200) = 198`
- `expansion = floor(rho * reserves * effective_blocks / expected_blocks) = floor(0.003 × 6e15 × 198/200) = 17.82e12`
- `treasury_cut = floor(tau × expansion) = floor(0.2 × 17.82e12) = 3.564e12`

So dugite *should* have produced `delta_treasury ≈ 3.564e12` at boundary 1→2 (within Haskell rounding noise).

## Other round criteria

Round 2 otherwise passes (these are all GREEN despite the pot divergence):
- `verify.sh`: 5/5 PASS (forge cross-check 431 canonical / 100% tip-parity 171/171 / max tip-age 6s)
- `analyze-evidence.sh`: NO ANOMALIES (0 ERROR per node, chain density 0.524)
- `TraceForgedInvalidBlock`: 0 in cardano-bp log → chain remains structurally valid

This is the key insight: **the blocks dugite forges are accepted by Haskell even though dugite's local ledger state diverges**. The chain block CBOR carries transactions but not derived ledger state (treasury/reserves), so the divergence is purely on the ledger-side and won't immediately fork the chain. Over many epochs it would compound and eventually break things like withdrawal correctness, reward-account state, and any debug-CBOR cross-validation.

## Hypothesis (untested)

`compute_reward_update` in `crates/dugite-ledger/src/state/rewards.rs:136` produces `PendingRewardUpdate::default()` (deltas = 0) under one of these conditions. Tracing through what *should* be the values at devnet boundary 1→2:

- `prev_protocol_params` should be {rho=0.003, tau=0.2} — set at genesis init (`state/mod.rs:649`) to the genesis `params`
- `prev_d` should be {0,1} — Conway always sets d=0 (`eras/conway.rs:433-436`)
- `bprev_blocks_by_pool` should be `{pool1: 198}` — captured at boundary 0→1 and stored in `epochs.snapshots.bprev_blocks_by_pool`
- `go_snapshot` may be `None` at boundary 1→2 (snapshots are: mark→set at 0→1, set→go at 1→2; the first non-trivial GO appears at boundary 2→3)
- `reserves = 5.999994e15`, `treasury = 0` at this boundary

If `go=None` at boundary 1→2, the `compute_reward_update` code path at line 258-269 returns `delta_treasury = treasury_cut, delta_reserves = treasury_cut - epoch_fees` — so it *should* produce non-zero treasury even with empty GO.

So **one of the inputs (rho/tau/d/reserves/blocks) must be wrong, OR the wrapper code in `Conway::on_epoch_transition` is reading from the wrong field**. Need to either:
1. Add a `tracing::info!` at compute_reward_update entry+exit and re-run devnet
2. Write a `tests::on_epoch_transition` unit test for boundary 1→2 (Conway-from-genesis genesis) and assert `delta_treasury > 0`

## Evidence

- Evidence dir: `testnet/local-devnet/evidence/20260528T081344Z/`
- dugite-bp log: `testnet/local-devnet/logs/dugite-bp.log` (no RUPD-level traces; would need DEBUG)
- HEAD commit: `34427a267 devnet+skill: drop k from 60 to 40 so RUPD pulser fits the epoch`
- Devnet specs: `testnet/local-devnet/config/spec/shelley-spec.json` (k=40, f=0.5, epoch=400, rho=0.003, tau=0.2)

## Severity / blast radius

- **P0** — ledger state divergence from canonical Haskell semantics
- **Not immediately chain-breaking** — block CBOR is fine, chain continues to validate; tip-parity stays at 100%
- **Cumulative** — every subsequent epoch boundary widens the gap; over hundreds of epochs the dugite ledger state would be wildly wrong vs network truth
- **Hidden until now** — preview/preprod cross-validation suites started at boundary 3 (issue #438) and never exercised 1→2 against Haskell, only against Koios snapshots taken from a different starting condition
- **Caught by**: this Round 2 enhancement (k=40 + byte-exact Haskell-pot comparison)

## Stop point

Per the original autonomous goal's stop conditions ("new bug class needing architectural decision: stop+ask"), the user must decide:

(a) **Fix now** — instrument `compute_reward_update`, write a Conway-from-genesis boundary 1→2 unit test, root-cause, fix, re-validate Round 2, then proceed to Round 3.
(b) **File as P0 issue, proceed** — log this as a known divergence (new GitHub issue), run Round 3, finish the validation report. Fix this in a focused follow-up session.

I cannot reasonably estimate the fix time without instrumenting first. The "wrong input field" class would be a 1-line fix once identified (similar to issue #685 prevPParams capture timing). A deeper bug could take 4-8h.
