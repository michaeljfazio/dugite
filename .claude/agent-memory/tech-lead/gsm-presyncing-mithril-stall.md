---
name: gsm-presyncing-mithril-stall
description: Issue #757 — genesis-mode PreSyncing LoE stalls exactly k=2160 blocks past Mithril snapshot tip; v1 fix (startup→Syncing) was INSUFFICIENT; v2 fix suppresses evaluate() HAA-loss regression when tip is recent
metadata:
  type: project
---

Mithril-bootstrapped genesis node stalls at `snapshot_tip + k` blocks.

**Root cause (v1 diagnosis — still correct)**: no `caught_up.marker` → GSM starts in `PreSyncing`. PreSyncing LoE is `Fragment { anchor=immutable_tip, entries=[], k=2160 }`. `loe_allows_extension()` returns false at depth == k. BLPs never arrive in time (ledger discovery fires every 5 min, first tick skipped; preprod topology has no `peerSnapshotFile`).

**v1 fix (INSUFFICIENT — live-disproven 2026-06-13)**: `GenesisStateMachine::new()` → no-marker + recent tip → start in Syncing. This FIRED correctly (log: "Genesis: tip is recent (no marker) — starting in Syncing"). But 10 seconds later `evaluate()` was called with `active_blp_count=0`, and it regressed BACK to PreSyncing unconditionally. Re-entered the k-block stall.

**v1 failure root cause**: `evaluate()` Syncing arm regressed on `active_blp_count < min` with no knowledge of tip age. The SyncStatus emitter reports `active_blp_count=0` on preprod (0 BLPs, 0 local roots, only bootstrap peers). `haa_satisfied()` only knows about BLPs and local roots — bootstrap peers are not tracked.

**Haskell oracle findings**:
- Under `UseBootstrapPeers` mode, Haskell HAA = ≥1 active bootstrap peer (NOT BLPs). Dugite doesn't track bootstrap peers at all.
- Syncing ALWAYS regresses to PreSyncing in Haskell when HAA is lost — no runtime tip-age bypass exists.
- The only Haskell bypass is at startup (marker present + fresh tip → CaughtUp directly, no HAA).
- Haskell avoids the stall because preprod topology includes `bootstrapPeers`, which connect in seconds → HAA satisfied before first evaluate tick.

**v2 fix (correct)**: in `evaluate()` Syncing arm, suppress the HAA-loss regression when the tip is still recent:
```
haa_lost = active_blp_count < min_active_blp
tip_stale = threshold == 0 || tip_age_secs >= threshold

if haa_lost && tip_stale  → regress (from-genesis-vulnerable; needs real HAA)
if haa_lost && !tip_stale → suppress (Mithril-certified; GDD protects; log DEBUG)
```

**Files changed (v2)**:
- `crates/dugite-node/src/gsm.rs` — `evaluate()` Syncing arm rewritten; 3 new tests; 1 existing test updated to use stale tip
- `crates/dugite-ledger/src/validation/mod.rs:3728` — threaded `script_versions_for_redeemers` into broken `check_extra_redeemers` call (left broken by in-tree #758 work; needed to compile)

**Three invariants preserved**:
1. PRAOS: `!self.enabled` → early return, unchanged
2. From-genesis cold start: tip stale → regression fires as before
3. Recent/Mithril: tip fresh + HAA lost → stays Syncing

**New tests (64/64 pass)**:
- `test_recent_tip_syncing_haa_loss_suppressed` — pins invariant 3
- `test_syncing_haa_loss_with_stale_tip_regresses` — pins invariant 2
- `test_praos_evaluate_noop_with_recent_tip` — pins invariant 1

**Smoking gun**: `gsm_state=0 + blocks_applied=2160=k` exactly → LoE cap fingerprint. Always check this first for genesis-mode stalls.

**Why:** v2 is a Dugite extension; Haskell avoids this by having bootstrap peers tracked in the diffusion layer's `OutboundConnectionsState` TVar. Full fix would be to track bootstrap peers in `haa_satisfied()` (mirroring the `UseBootstrapPeers` case-split in Haskell `outboundConnectionsState`), but the tip-staleness gate is simpler and equally safe given Mithril's trust model.

**How to apply:** Every genesis-mode startup stall investigation should check `gsm_state=0 + blocks_applied=k` first. If the node starts in Syncing but immediately regresses, the v1 failure mode is active — check if `evaluate()` tip_stale gate is in place.
