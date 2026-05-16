---
name: forge-connectivity-gate-bug-c
description: Bug C fix — forge loop gated on hot-peer + ChainSync intersection to prevent self-forked fork cascade
metadata:
  type: project
---

## Forge Connectivity Gate (Bug C) — commit 9d30beaf2

**Root cause:** `try_forge_block_at` ran as soon as VRF leader check passed, regardless
of peer state or ChainSync intersection status. On fresh local-devnet startup, dugite-bp
forged block 0 at slot 10 before any peer connected. After the self-forge, the relay's
chain shared no volatile block with the bp's chain. Bug A's guard then disconnected on
every reconnect attempt (Origin intersection with non-Origin local tip), permanently
stalling the node.

**Fix:** Added two conditions that must both be true before forge proceeds:
1. `peer_manager.hot_peer_count() > 0` — at least one hot peer (RwLock read)
2. `peer_intersection_established.load(Relaxed)` — AtomicBool set by chainsync

**Key design decision on when to set the flag:**
The flag is set by `chainsync_client_task` for ANY valid intersection that survives the
Bug-A guard. Two valid cases:
- `Specific` intersection (non-Origin) — normal case, peer shares our chain
- `Origin` intersection with `Origin` local ledger — fresh genesis start, both nodes at
  genesis; also valid, forging can proceed

The Bug-A guard already filters out the dangerous case (Origin intersection with
non-Origin local ledger = self-forged fork). So we just set the flag unconditionally
after the guard.

**Important:** Originally tried setting flag only for `Specific` intersections, which
broke the fresh-genesis devnet scenario (both nodes at genesis, intersection at Origin,
flag never set, forge deferred forever). The "Deferring forge: has_intersection=false"
log at slot=60 revealed this regression.

**Files changed:**
- `crates/dugite-node/src/node/mod.rs` — `Node.peer_intersection_established` field,
  gate in `try_forge_block_at`, 5 unit tests
- `crates/dugite-node/src/node/connection_lifecycle.rs` — `ConnectionLifecycleManager`
  field + `new()` + `new_for_test()` + `make_chainsync_task()` plumbing
- `crates/dugite-node/src/node/sync.rs` — `chainsync_client_task` new parameter,
  flag set after Bug-A guard

**Validation (local-devnet, 2026-05-16):**
- ChainSync intersection found (08:18:22) → Chain extended slot=2 (08:18:24) →
  TraceForgedBlock slot=12 (08:18:29). Ordering correct.
- p2 (per-BP attribution): PASS — pool1=27, pool2=23 forges
- p4 (tip parity): PASS
- 673/673 tests pass

**Why:** Without gate, self-forged fork + Bug-A guard = permanent stall on local-devnet.
**How to apply:** The gate is transparent in production (peers connect within ~2s, well
before the first leadership slot).
