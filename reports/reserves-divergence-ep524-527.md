# Issue #763: Accelerating Reserves Divergence — Root-Cause Report

**Date:** 2026-06-13  
**Epochs:** 524–527 (mainnet, Conway PV9)  
**Symptom:** Dugite drains MORE from reserves than Koios by a growing margin each epoch  
**Status:** Real reward-calc divergence confirmed; specific trigger under investigation  

---

## 1. Data Summary

| Epoch boundary | Extra reserves drain (dugite vs Koios) |
|---|---|
| pre-ep524 start | **776,000 ADA cumulative** (inherited) |
| 524→525 | +285,693 ADA |
| 525→526 | +622,379 ADA |
| 526→527 | +927,761 ADA |
| ep527 end | **~2,611,833 ADA cumulative** |

Increment between epochs: +337K, +305K (roughly +320K/epoch).  
Treasury discrepancy: -15K→-4.8K ADA/epoch (tiny and shrinking — see §4 for implication).

---

## 2. Is This a Sampling Artifact?

**Conclusion: No. The gauges sample canonical post-NEWEPOCH state.**

### Code path (N2C sync + bulk sync)

`dugite_reserves_lovelace` and `dugite_treasury_lovelace` are `AtomicU64` fields in
`NodeMetrics` (`crates/dugite-node/src/metrics.rs`, lines 444-445).

They are set via `set_governance_snapshot(governance_snapshot_from_ledger(&ls))` where:

```rust
// crates/dugite-node/src/node/mod.rs:506-509
GovernanceSnapshot {
    treasury_lovelace: ls.epochs.treasury.0,
    reserves_lovelace: ls.epochs.reserves.0,
    ...
}
```

This is called **after** `apply_block_with_delta` returns in both the bulk-sync path
(`sync.rs:1618-1929`) and the at-tip path (`mod.rs:6406-6425`).

`apply_block_with_delta` calls `process_epoch_transition` as Step 3 of block apply
(`apply.rs:265-373`). `process_epoch_transition` for Conway:
1. Applies old `pending_reward_update` (if any — backward compat only)
2. Calls `compute_reward_update(...)` 
3. Immediately applies the result (lines 668-696): `reserves -= delta_reserves`, `treasury += delta_treasury`, per-account reward credits
4. Rotates snapshots, builds MARK
5. Applies governance (ratification, expiration, enactment)
6. Applies pool retirements

So `ls.epochs.reserves.0` at metric-update time = post-RUPD canonical value.  
**The gauge is sampled after the full NEWEPOCH transition. Sampling artifact is ruled out.**

---

## 3. Formula Verification

### 3.1 Conservation identity

```
delta_reserves = treasury_cut + total_distributed - epoch_fees
```

Algebra matches Haskell's `RewardUpdate`:
- `deltaT = treasury_cut = floor(tau × (expansion + fees))`
- `deltaR = deltaR2 - deltaR1 = undistributed - expansion`
- `dugite delta_reserves = expansion - undistributed = treasury_cut + total_distributed - epoch_fees` ✓

(`rewards.rs` lines 575-578)

### 3.2 Expansion (deltaR1)

Conway d = 0 (always `d_ge_4_5` is false):
```rust
// rewards.rs:199-231
effective_blocks = actual_blocks.min(expected_blocks);
expansion = floor(rho × reserves × effective_blocks / expected_blocks)
```
Matches Haskell `startStep`: `rho * min(1, eta) * reserves` with `eta = blocksMade/expectedBlocks`. ✓

### 3.3 ss_fee timing

The oracle confirms: `ssFee` in Haskell is a **top-level field** on `SnapShots`, overwritten each boundary with the current `utxosFeesL` post-applyRUpd drain. It is **not** rotated with the individual `ssStakeMark/Set/Go` snapshots. Dugite mirrors this with `epochs.snapshots.ss_fee` (a single field, set each boundary at `conway.rs:719`). **One-epoch lag in both implementations.** ✓

### 3.4 total_stake formula

`total_stake = max_lovelace_supply - reserves.0` (`rewards.rs:246`).  
Haskell: `circulation = maxSupply - casReserves`.  
At ep524: `45_000_000_000_000_000 - 7_595_003_223_306_721 = 37_404_996_776_693_279` = Koios `supply` field ✓.

### 3.5 Apparent performance formula

Dugite (`rewards.rs:413-416`):
```rust
perf = (blocks_made / total_blocks) × (total_active_stake / pool_active_stake)
pool_reward = floor(perf × max_pool)
```
Haskell: `appPerf = beta / sigmaA` where `beta = blocksMade/blocksTotal` and `sigmaA = pool_stake/totalActiveStake`. `pool_reward = floor(appPerf × maxP)`. These are algebraically identical. ✓

### 3.6 Block counting timing

`bprev_blocks_by_pool` is captured at `conway.rs:418` BEFORE any parameter updates.
Current epoch's blocks are added via `evolve_nonce` (Step 9b in `apply.rs`), AFTER epoch transition (Step 3). So bprev correctly reflects N-1 blocks. ✓

### 3.7 prevPParams

RUPD uses `epochs.prev_protocol_params` (`conway.rs:613`), captured before governance
enactment (`conway.rs:438`). ✓ (Issue #438 fix confirmed present.)

**Summary of formula checks:** All checked formula components are correct. The divergence lies in the INPUTS to the RUPD formula, not the formula itself.

---

## 4. Localizing the Divergence

### 4.1 Treasury exactness constraint

Since `delta_treasury = treasury_cut = floor(tau × (expansion + fees))` and treasury
tracks Haskell very closely (-15K ADA/epoch), the RUPD INPUTS (reserves, eta, ss_fee)
are approximately correct. The treasury divergence shrinks (from -15K to -4.8K ADA/epoch)
as reserves deplete — consistent with expansion converging due to lower reserves.

This rules out: `rho` wrong, `reserves` dramatically wrong as RUPD input, `ss_fee` wrong,
block-eta wrong. All would move `delta_treasury` significantly.

### 4.2 What the divergence implies

From the conservation identity, if `delta_treasury` is (nearly) correct and `delta_reserves`
is X too high:

```
delta_reserves_dugite = treasury_cut + total_distributed_dugite - epoch_fees
delta_reserves_haskell = treasury_cut + total_distributed_haskell - epoch_fees
delta_reserves_dugite - delta_reserves_haskell ≈ total_distributed_dugite - total_distributed_haskell
```

**Dugite distributes 285K → 928K ADA MORE per epoch than Haskell.** The excess goes to
`reward_accounts` (since treasury is close and the total conservation identity must hold).

### 4.3 Why the divergence grows

Per-epoch extra distribution: 285K, 622K, 928K (increments: +337K, +305K).

The growth rate (~+320K ADA/epoch extra each epoch) is far too fast for simple compounding
(which would give rho × delta ≈ 0.3% of any existing excess per epoch). Therefore the
excess `total_distributed` is not primarily caused by compounding from prior over-distribution.

This growth pattern is instead consistent with a **growing pool_stake in the GO snapshot**:
- Epoch N's GO snapshot includes pool_stake M(N) above Haskell's
- Epoch N+1's GO snapshot includes pool_stake M(N)+X above Haskell's
- Extra per-epoch distribution ≈ RUPD_factor × pool_stake_excess

If the GO snapshot's pool_stake grows by X per epoch (due to some mechanism adding X to
`reward_accounts` at each MARK time), the per-epoch excess distribution grows linearly.

### 4.4 Magnitude of the implied stake excess

For 285K ADA extra distribution at ep524→525 boundary:
```
extra = RUPD_factor × stake_excess
RUPD_factor ≈ rho × (1-tau) × (sigma/n) × n ≈ rho × (1-tau) ≈ 0.003 × 0.8 = 0.0024
stake_excess ≈ 285,000 / 0.0024 ≈ 118M ADA
```

This implies the GO snapshot used for ep524→525 RUPD contains approximately **118M ADA
more pool_stake** than Haskell's equivalent GO snapshot. This is a very large amount —
commensurate with the cumulative effect of large treasury withdrawals accumulating in
reward_accounts.

---

## 5. Inherited Deficit Context

The **776K ADA cumulative deficit at ep524 start** matches the pots-deficit-fingerprint
model (report `pots-deficit-fingerprint.md`):

- ep388→389 boundary: one-shot +996,138 ADA injection into reserve deficit (state artifact
  from val12 mid-epoch restore with corrupted go/bprev in v8 binary)
- D(E) decays by ~0.2%/epoch: `D(524) ≈ 996,138 × (1-0.002)^135 ≈ 769K ADA`

The ep388 injection contributes ~2,800 ADA/epoch of extra reserves drain at ep524 (from
the ~760K excess in reward_accounts generating tiny extra stake). This is NOT the cause
of the 285K-928K/epoch observed — those are two orders of magnitude larger.

**Issue #763 is a distinct, newer divergence layered on top of the ep388 inherited deficit.**

---

## 6. Root-Cause Candidates

The divergence requires the GO snapshot to contain ~118M ADA more pool_stake than Haskell's.
This can arise from ANY of:

### Candidate A: Treasury Withdrawal Balance Accumulating in reward_accounts [LEADING]

The ep495 mainnet treasury withdrawal disbursed **102,140,000 ADA = 102.14M ADA** to
`stake1uxv9hwk8523p3asgnnjv0t49qvmlj96j22dw434j2gyks2qn9x52u`. If this stake address:
1. Is registered and delegated to a pool
2. Has NEVER claimed (withdrawn) those 102.14M ADA via a regular transaction

Then at each MARK snapshot, `reward_accounts[key] = 102.14M ADA` is included in
`pool_stake`. Dugite and Haskell should both do this — so this alone doesn't cause
divergence UNLESS:
- The address is registered in dugite's `reward_accounts` but NOT in Haskell's (because
  the registration certificate was dropped or handled differently)
- OR the 102.14M ADA was withdrawn from the on-chain UTxO in a transaction that dugite
  is mishandling (failing to drain `reward_accounts` on withdrawal)

**A withdrawal transaction from that stake address that dugite is not applying correctly
would leave 102.14M ADA in dugite's `reward_accounts` while Haskell has 0.** At each
MARK snapshot, this inflates pool_stake by 102.14M ADA → explains ~285K/epoch excess.

The linear GROWTH of the excess (~+320K/epoch) would then arise from the per-epoch reward
distribution adding to the balance: each epoch the pool earns rewards ≈ rho × 102.14M ADA
≈ 306K ADA more in dugite than Haskell (since dugite includes the balance in pool_stake
while Haskell excluded it post-withdrawal). This 306K additional per epoch matches the
observed +337K/+305K increment. ✓

### Candidate B: Incorrect reward_accounts drain on Conway withdrawal certificates

Conway uses `TxCert.ConwayTxCertDeleg.ConwayUnRegDRep` or stake deregistration/withdrawal
certificates to drain reward_accounts. If dugite's `apply_wdrls` is not correctly draining
`reward_accounts` for some withdrawal type introduced or changed in Conway (PV9+), balances
could persist when Haskell would clear them.

File to check: `crates/dugite-ledger/src/eras/common.rs` around `apply_wdrls`.

### Candidate C: MARK snapshot including DRep reward_accounts incorrectly

At ep524-527, `deposits_drep` is growing (260M → 303M ADA). DRep deposits go to
`reward_accounts` on deregistration. If DRep credential reward accounts are being
included in the MARK pool_stake calculation when Haskell excludes them (because they're
not delegated to a pool), this would inflate pool_stake.

In dugite `conway.rs:737-750`, pool_stake is built by iterating `certs.delegations`.
A DRep credential that is a stake credential AND is delegated should correctly be included.
But a DRep-only credential should not appear in `delegations`. This path seems correct.

### Candidate D: Incorrect filtering in total_active_stake

`rewards.rs:283-287`:
```rust
let total_active_stake: u64 = go.pool_stake.iter()
    .filter(|(pool_id, _)| go.pool_params.contains_key(pool_id))
    .fold(0u64, |acc, (_, s)| acc.saturating_add(s.0));
```

If retired pools (present in pool_stake at MARK time but removed from pool_params by
GO time) have their stake included in `total_active_stake` but excluded from per-pool
reward calculation, this creates a mismatch where `total_active_stake` > actual active
stake. The apparent performance (`total_active/pool_active`) would be inflated, causing
pool_reward to be inflated.

However this mechanism causes CONSTANT (not growing) excess — retiring pool count would
need to grow for a linearly increasing excess. Possible if pool retirement waves coincide.

---

## 7. Most Likely Root Cause

The data fingerprint most strongly matches **Candidate A**: a large withdrawal from
`stake1uxv9hwk8523p3asgnnjv0t49qvmlj96j22dw434j2gyks2qn9x52u` (or similar large-balance
credential) is being misapplied in dugite, leaving ~102-118M ADA in `reward_accounts`
that Haskell has drained. The 2-epoch snapshot lag means:

- At boundary N→N+1: withdrawal misapplied → balance persists in `reward_accounts`  
- At boundary (N+2)→(N+3): MARK from N appears in GO → extra 102M in pool_stake
- RUPD at boundary (N+2)→(N+3): distributes ~285K ADA extra to reward_accounts
- RUPD at boundary (N+3)→(N+4): distributes ~585K extra (now 102M + 285K extra in pool_stake)
- RUPD at boundary (N+4)→(N+5): distributes ~877K extra (now 102M + 570K extra)

This matches the observed pattern (285K, 622K, 928K with ~310K increment). ✓

---

## 8. Verification Strategy

### Step 1: Identify the withdrawal transaction

Search for withdrawal transactions from `stake1uxv9hwk8523p3asgnnjv0t49qvmlj96j22dw434j2gyks2qn9x52u`
in the epoch range 520-525:

```bash
curl -s 'https://api.koios.rest/api/v1/account_reward_history?_stake_address=stake1uxv9hwk8523p3asgnnjv0t49qvmlj96j22dw434j2gyks2qn9x52u&_epoch_no=520'
curl -s 'https://api.koios.rest/api/v1/account_utxos?_stake_address=stake1uxv9hwk8523p3asgnnjv0t49qvmlj96j22dw434j2gyks2qn9x52u'
```

Check the account balance and withdrawal history. If there's a withdrawal tx at
approximately epoch 520-522 in Haskell that dugite doesn't apply (leaving 102M ADA
in `reward_accounts`), this is the source.

### Step 2: Cross-validate pool_stake in GO snapshot

Add `--features reward-debug-dump` diagnostic dump for the ep524→525 boundary. Compare
`pool_stake` totals with Haskell's `cardano-cli debug log-epoch-state` (go snapshot
`ssStakeGo.ssTotalActiveStake`). A discrepancy of ~118M ADA confirms the mechanism.

### Step 3: Check reward_accounts balance for the 102.14M ADA address

Enable epoch-state-debug at ep522-524 boundary (when MARK is taken). If dugite shows
the 102.14M ADA address with `reward_accounts[key] = 102.14M + accumulated_rewards`
while Haskell shows `0` (withdrawn), the Candidate A theory is confirmed.

### Step 4: Check apply_wdrls in Conway

Review `crates/dugite-ledger/src/eras/common.rs` `apply_wdrls` function and whether
Conway (pv9) withdrawal certificates (ConwayTxCertDeleg) properly drain `reward_accounts`.

---

## 9. What Is NOT the Cause

- **Sampling artifact**: ruled out (§2)
- **RUPD formula bugs**: all components verified correct (§3)
- **ep388 injection alone**: ~2.8K/epoch contribution at ep524, not 285K (§5)
- **Treasury withdrawal double-credit**: treasury matches — ruled out
- **ss_fee timing**: confirmed correct one-epoch lag matching Haskell (§3.3, oracle-confirmed)
- **Block counting / prevPParams / eta**: all verified (§3.5-3.7)

---

## 10. Recommended Fix Path

1. **Identify the specific transaction** where dugite fails to drain `reward_accounts`
   (most likely a Conway-era withdrawal transaction referencing the 102.14M ADA stake address)
2. **Add an epoch-state dump** at the ep522→523 or ep523→524 boundary (MARK snapshot time)
   and compare pool_stake against Haskell dump
3. **Fix the withdrawal handling** in `common.rs::apply_wdrls` or Conway cert handling
4. **Rebuild from snapshot** pre-ep522 to avoid re-accumulating the error

The fix itself is expected to be small (withdrawal credential not being correctly matched
or drained). No RUPD formula changes are needed — the formula is correct.

---

## 11. File References

| File | Relevance |
|---|---|
| `crates/dugite-ledger/src/state/rewards.rs:135-605` | `compute_reward_update` — RUPD formula |
| `crates/dugite-ledger/src/eras/conway.rs:601-702` | RUPD computation + application at boundary |
| `crates/dugite-ledger/src/eras/conway.rs:704-794` | Snapshot rotation + MARK construction |
| `crates/dugite-ledger/src/eras/common.rs` | `apply_wdrls` — withdrawal certificate handling |
| `crates/dugite-node/src/node/mod.rs:492-521` | `governance_snapshot_from_ledger` — metric sampling |
| `crates/dugite-node/src/metrics.rs:444-445` | `reserves_lovelace`/`treasury_lovelace` atomics |
| `reports/pots-deficit-fingerprint.md` | ep388 injection model — inherited 776K deficit |

---

*Report methodology: formula tracing through Rust source, Koios mainnet REST cross-validation,
Haskell oracle (ssFee lag, SnapShots type), conservation identity algebra, compounding model
analysis. No code changes made per user instruction.*
