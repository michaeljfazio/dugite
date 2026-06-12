# Issue #757: Genesis Mode PreSyncing Stall — Root Cause & Fix

**Status**: Fixed (working tree only — not committed)
**Affected mode**: `--consensus-mode genesis` bootstrapped from a Mithril snapshot
**Symptom**: `dugite_gsm_state = 0` (PreSyncing), `dugite_blocks_applied_total = 2160` (= k), node frozen at `snapshot_tip + k` blocks, never advances to live tip

---

## Root Cause

The bug is a combination of two conditions that together create a permanent deadlock:

### Condition 1 — LoE caps selection at exactly k blocks in PreSyncing

In `GenesisStateMachine::new()` (`crates/dugite-node/src/gsm.rs`) when there is no `caught_up.marker` file, the GSM starts in `PreSyncing`. In PreSyncing the Limit on Eagerness (LoE) is represented as:

```
LoeState::Fragment { anchor = immutable_tip, entries = [], k = 2160 }
```

`loe_allows_extension()` in `crates/dugite-storage/src/loe_trim.rs` returns `false` when:
```rust
// entries is empty → min-fragment head == anchor
depth_past_loe_tip == k  // i.e., 2160 == 2160 → NOT < k → false
```

The node applies the k blocks between the snapshot tip and the live tip in seconds (the bulk-sync pipeline runs at 100–300 blk/s). At exactly block `snapshot_tip + k` the `(k+1)`-th block is gated: `add_block_gated(allow_extend = false)` stores the block in VolatileDB but does not extend selection. From this point, the node fetches blocks but can never apply any of them. Every block is stored as a deferred leaf.

The smoking gun is `dugite_blocks_applied_total = 2160 = k` — this is not coincidence.

### Condition 2 — PreSyncing → Syncing transition never fires

The transition requires `active_blp_count >= min_active_blp` (default 5). Big Ledger Peers can come from two sources:

1. **`peerSnapshotFile`** (cardano-node topology field) — absent in the default preprod topology (`config/preprod/topology.json`)
2. **Ledger peer discovery** (`run_ledger_peer_discovery_task`) — fires every **5 minutes** with the **first tick skipped**

The local-roots trust path in `haa_satisfied()` also doesn't fire because `preprod/topology.json` has `localRoots: [{accessPoints: [], ...}]` (empty).

Timeline on a typical Mithril snapshot bootstrap:
- `t=0s`: node starts, GSM in PreSyncing
- `t=7-21s`: node applies k=2160 blocks, LoE fires, selection frozen
- `t=5m+`: ledger peer discovery fires for the first time — too late

The node is already permanently stalled before the first BLP is discovered.

### Why PRAOS works on the same DB

PRAOS mode sets `LoeState::Disabled`. `loe_allows_extension()` returns `true` unconditionally. No k-block cap exists. The PRAOS path is entirely unaffected.

---

## Files Changed

### `crates/dugite-node/src/gsm.rs`

**1. New field in `GsmConfig`** (after `marker_path`):
```rust
/// Stability window in seconds (`sgen × slot_length_secs`).
/// When the node starts without a `caught_up.marker` and the tip age at
/// startup is less than this threshold, it transitions directly to Syncing.
/// A value of 0 disables the optimisation (keeps strict Haskell semantics).
pub syncing_startup_threshold_secs: u64,
```

**Default**: `129_600` (= `sgen = ceil(3k/f) = ceil(3×2160/0.05)` slots, each 1 s on mainnet/preprod). This is the Ouroboros stability window — one full window of "recent" history.

**2. `GenesisStateMachine::new()` — no-marker branch**:

Before the fix, the no-marker branch always returned `GenesisSyncState::PreSyncing`. After the fix:

```rust
let recent = matches!(
    initial_tip_age_secs,
    Some(age) if config.syncing_startup_threshold_secs > 0
        && age < config.syncing_startup_threshold_secs
);
if recent {
    info!(..., "Genesis: tip is recent (no marker) — starting in \
         Syncing (Mithril snapshot / fast-restart path, issue #757)");
    GenesisSyncState::Syncing
} else {
    GenesisSyncState::PreSyncing
}
```

**Decision table** (updated docstring):

| marker  | tip age                                  | initial state                |
|---------|------------------------------------------|------------------------------|
| absent  | `None` or ≥ `syncing_startup_threshold`  | PreSyncing                   |
| absent  | < `syncing_startup_threshold` (recent)   | Syncing  (Dugite ext. #757)  |
| present | `None` (no age limit)                    | CaughtUp                     |
| present | young enough                             | CaughtUp                     |
| present | already too old                          | PreSyncing + marker DELETED  |

**3. All existing `GsmConfig` struct literals in tests** that were missing the new field got `..Default::default()` added. Files: `gsm.rs` tests + `tests/genesis_integration.rs`.

### `crates/dugite-node/src/node/mod.rs`

**1. Compute `syncing_startup_threshold_secs` from genesis params** (inserted before `gsm_config` construction, ~line 2174):

```rust
let syncing_startup_threshold_secs =
    (genesis_params.sgen_slots as f64 * genesis_params.slot_length_secs) as u64;
let gsm_config = crate::gsm::GsmConfig {
    ...
    syncing_startup_threshold_secs,
    ..Default::default()
};
```

This is network-aware: preprod/mainnet = 129,600 s, preview = 25,920 s, devnet = 300 s.

**2. `initial_gsm_state` pre-computation** (~line 2268): the initial snapshot now mirrors the GSM actor's startup decision. It computes the tip age from `initial_tip_slot` (available from ledger state) and `shelley_genesis.system_start + slot_length`:

```rust
let tip_age_secs = shelley_genesis.as_ref().and_then(|sg| {
    chrono::DateTime::parse_from_rfc3339(&sg.system_start).ok().map(|t| {
        let tip_wallclock_ms = t.timestamp_millis().max(0) as u64
            + initial_tip_slot * sg.slot_length.saturating_mul(1000);
        let now_ms = now.as_millis() as u64;
        now_ms.saturating_sub(tip_wallclock_ms) / 1000
    })
});
let recent = matches!(tip_age_secs, Some(age) if threshold > 0 && age < threshold);
```

This ensures the LoE snapshot published for downstream consumers (`loe_slot = None` vs `Some(0)`) is correct from the very start, not just after the GSM actor starts.

---

## Unit Tests Added (`crates/dugite-node/src/gsm.rs`)

Five new tests under the `// ── Issue #757: Mithril snapshot bootstrap startup state ─────` section:

| Test | What it verifies |
|------|-----------------|
| `test_mithril_snapshot_bootstrap_starts_in_syncing` | Recent tip (7200s = 2h, << 129600s threshold) + no marker → `Syncing` |
| `test_stale_tip_no_marker_starts_in_presyncing` | Stale tip (200000s > threshold) + no marker → `PreSyncing` |
| `test_unknown_tip_age_no_marker_starts_in_presyncing` | `initial_tip_age_secs = None` + no marker → `PreSyncing` (safe default) |
| `test_threshold_zero_disables_syncing_startup` | `syncing_startup_threshold_secs = 0` → always `PreSyncing` (strict Haskell semantics) |
| `test_tip_age_at_threshold_boundary_is_presyncing` | `age == threshold` → `PreSyncing` (exclusive boundary) |

All 1510 tests pass; clippy clean.

---

## Safety Analysis

**Mithril snapshot security**: The Mithril certificate chain cryptographically guarantees the snapshot was produced by ≥ `n_signers` stake-weighted SPOs. The snapshot tip is already trusted. Starting in Syncing skips the HAA pre-check for that checkpoint, which is safe because Mithril provides an equivalent (and stronger) guarantee — the snapshot IS the honest availability checkpoint.

**Long-idle fast-restart**: If a node has been offline for > stability window (> 36h on mainnet), `tip_age_secs >= syncing_startup_threshold_secs` → starts in `PreSyncing` exactly as before. The historical security guarantee is preserved.

**Threshold = 0 escape hatch**: Setting `syncing_startup_threshold_secs = 0` in the `LowLevelGenesisOptions` disables the optimisation entirely and restores strict Haskell semantics (always PreSyncing without a marker). The field flows through `genesis_params.sgen_slots * genesis_params.slot_length_secs`, but operators can override via `GsmConfig::syncing_startup_threshold_secs = 0` if needed.

**PRAOS unchanged**: The entire new code path is guarded by `genesis_enabled` checks at every callsite. `LoeState::Disabled` is unaffected.

---

## Corrected fix (live-disproven v1)

**Date**: 2026-06-13
**Status**: Working-tree fix; NOT committed. Ready for live revalidation on db-preprod-757.

### What the live test proved

The v1 fix (startup state → Syncing) fired correctly ("Genesis: tip is recent (no marker) — starting in Syncing"). But 10 s later `evaluate()` was called with `active_blp_count = 0` and `tip_age_secs ≈ 7 300` (still recent). The old `Syncing` arm regressed unconditionally on `active_blp_count < min_active_blp`, writing `PreSyncing` back. The node re-entered the k-block stall.

**Root cause of v1 insufficiency**: `evaluate()` at line 473 always regressed `Syncing → PreSyncing` when `active_blp_count < min`, with no knowledge of tip age. The HAA-emitter (`mod.rs ~4411`) computes `active_blp` as `pm.haa_satisfied(min_blp) ? min_blp : real_blp_count`. On preprod (0 BLPs, 0 local roots, only bootstrap peers), `haa_satisfied` returns `false` and `real_blp_count = 0` → first `SyncStatus` tick delivers `active_blp_count = 0` → immediate regression.

### Haskell-oracle answer

Ouroboros Consensus (`GSM.hs`, `NodeKernel.hs`, `ouroboros-network/cardano-diffusion/.../Governor/Types.hs`):

- `isHaaSatisfied` reads a `TVar OutboundConnectionsState`.
- Under **`UseBootstrapPeers` mode**: `TrustedStateWithExternalPeers` = ≥ 1 active bootstrap peer AND every established peer is bootstrap-or-trusted-local-root. Big-ledger peers are **irrelevant**.
- Under **GenesisMode + DontUseBootstrapPeers**: `TrustedStateWithExternalPeers` = `activeNumBigLedgerPeers >= minNumberOfBigLedgerPeers`.
- **Syncing always regresses to PreSyncing** the moment `isHaaSatisfied` returns false (the `orElse` in `enterSyncing'`). There is **no runtime "recent tip" bypass**.
- The only bypass is at **startup**: `initializationGsmState` (marker present + tip fresh) → directly `CaughtUp`, which then uses `blockWhileCaughtUp` (no HAA check at all).

**Why Haskell never hits this on preprod**: preprod topology includes `bootstrapPeers`. The peer-selection governor promotes one bootstrap peer to Hot within a few seconds → `TrustedStateWithExternalPeers = true` → `active_blp` read by the status task returns the synthetic `min` → `evaluate()` stays in Syncing.

**Why dugite hits it**: `haa_satisfied()` only knows about big-ledger peers and trusted local roots. Bootstrap peers are not tracked. On preprod with empty `localRoots` and no `peerSnapshotFile`, `haa_satisfied` always returns `false`, so the SyncStatus emitter always reports `active_blp_count = 0`.

### The correct fix (Option B — evaluate() suppression)

**File**: `crates/dugite-node/src/gsm.rs`, `GenesisStateMachine::evaluate()`, the `Syncing` arm.

**Old rule** (v1):
```
if active_blp_count < min_active_blp → regress unconditionally
```

**New rule** (v2):
```
haa_lost = active_blp_count < min_active_blp
tip_stale = syncing_startup_threshold_secs == 0 || tip_age_secs >= syncing_startup_threshold_secs

if haa_lost && tip_stale  → regress to PreSyncing (tip is in from-genesis-vulnerable regime)
if haa_lost && !tip_stale → suppress (tip is certified-recent; GDD protects selection; log DEBUG)
```

**Why this is safe**:
- When `syncing_startup_threshold_secs > 0` and the tip entered the Syncing state because it was recent, the tip STAYS recent until the node catches up. A recent tip means the Mithril certificate (or a prior CaughtUp run) already validated the chain up to that point. GDD protects against density-sparse adversarial peers during the bounded gap to the live tip.
- Once the node catches up, `caught_up_predicate` fires → CaughtUp → marker written → the HAA concern is gone.
- If the tip goes stale while HAA is still unmet (e.g. node offline for > 36h), `tip_stale = true` and regression fires exactly as before.
- `syncing_startup_threshold_secs = 0` completely disables the bypass (strict Haskell semantics).
- `enabled = false` (PRAOS): `evaluate()` returns `None` immediately; this code is unreachable.

### Three invariants — verified

1. **PRAOS byte-identical**: `if !self.enabled { return None; }` at line 452 is hit before any new code.
2. **From-genesis cold start gated on real HAA**: when tip age ≥ threshold (`tip_stale = true`), HAA-loss regression fires normally.
3. **Recent-tip Mithril node stays in Syncing**: when `active_blp_count = 0` and `tip_age_secs = 7_300 < 129_600`, `tip_stale = false` → no regression.

### Tests added (64/64 pass)

| Test name | What it pins |
|---|---|
| `test_recent_tip_syncing_haa_loss_suppressed` | `active_blp=0, tip=7300s < threshold` → stays Syncing; multiple ticks |
| `test_syncing_haa_loss_with_stale_tip_regresses` | `active_blp=0, tip=130_000s > threshold` → PreSyncing |
| `test_praos_evaluate_noop_with_recent_tip` | `enabled=false` → `evaluate()` always `None`, state always CaughtUp |

Existing test `test_state_haa_loss_regresses_to_presyncing` updated to use `tip_age_secs=200_000` (stale) to remain consistent with new semantics.

Additionally, the incomplete call site in `crates/dugite-ledger/src/validation/mod.rs:3728` (left broken by the in-tree #758 work — `check_extra_redeemers` signature gained a `script_versions` arg but the call wasn't updated) was fixed by threading the already-computed `script_versions_for_redeemers` variable. This was necessary to compile the workspace at all; it does not change #758 logic.

### Haskell Reference (full picture)

---

## Haskell Reference

Haskell (`ouroboros-consensus`, `Ouroboros.Consensus.Genesis.Governor`):

```haskell
initializationGsmState :: GsmView m blk -> m GsmState
initializationGsmState gsmView = do
  markerWritten <- candidateReadCaughtUpPersistentState gsmView
  case markerWritten of
    Nothing -> pure PreSyncing
    Just CaughtUpState{..} -> do
      age <- durationUntilTooOld gsmView
      case age of
        Nothing  -> pure CaughtUp
        Just age -> if age <= 0 then do
          candidateRemoveCaughtUpPersistentState gsmView
          pure PreSyncing
        else pure CaughtUp
```

In Haskell, no-marker always starts `PreSyncing`. Haskell relies on `peerSnapshotFile` being present in the topology so that BLPs are available immediately at startup. Dugite's `config/preprod/topology.json` does not include `peerSnapshotFile` (a field that requires operator setup), so the strict Haskell path permanently deadlocks on Mithril bootstrap.

The Dugite extension is justified by the Mithril trust model: the Mithril certificate IS the HAA checkpoint. The stability-window threshold bounds the extension to nodes that are genuinely near the tip, preserving the security parameter for nodes that are genuinely far behind.

## Live validation of v2 fix (2026-06-13, preprod, db-preprod-757)

Restarted the FROZEN db-preprod-757 (stuck at slot 125,586,355 = snapshot_tip+k
under v1) with the v2.0.6-rc2 binary (v2 fix). Result:

- boot: `Genesis: tip is recent (no marker) — starting in Syncing (issue #757)`
- every 10s tick: `Genesis: HAA transiently lost but tip is recent — staying in
  Syncing (UseBootstrapPeers path, issue #757)` — the v2 suppression fires; the
  node does NOT regress to PreSyncing (v1 regressed here).
- **slot advanced 125,586,355 → 125,600,418 = live preprod tip (gap=0)**,
  gsm_state=1 (Syncing), blocks_applied climbing.

The v1 freeze is gone: the node unsticks past snapshot_tip+k and reaches the
live tip. PROVEN.
