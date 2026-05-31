---
name: multi-peer-blockfetch-wiring
description: Multi-peer BlockFetch wiring — per-peer dispatch invariant, BfPeerCmd channel, devnet validation results
type: project
---

# Multi-peer BlockFetch — wiring + correctness findings

## What was wired

**`BfPeerCmd` channel** (`block_fetch_logic.rs`):
- Added `BfPeerCmd` enum: `Register(SocketAddr, mpsc::Sender<Vec<FetchRange>>)` / `Deregister(SocketAddr)`
- Added `peer_cmd_rx: Option<mpsc::Receiver<BfPeerCmd>>` to `BlockFetchLogicTask`
- `drain_peer_cmds()` called at start of each decision tick (collect-then-process pattern for borrow checker)

**`ConnectionLifecycleManager`** (`connection_lifecycle.rs`):
- Added `bf_peer_cmd_tx: Option<mpsc::Sender<BfPeerCmd>>`
- `set_bf_peer_cmd_tx()` called from `Node::run()` after decision task is spawned
- `send_bf_cmd()` helper uses `try_send` (fire-and-forget, 64-slot buffer)
- `make_blockfetch_task` now creates per-peer `(fetch_tx, fetch_rx)` channel, sends `BfPeerCmd::Register`, and runs `blockfetch_worker` as the `ProtocolTaskFn` body
- Removed `active_fetcher` and `max_fetched_slot` atomic fields (single-fetcher era)
- `demote_to_warm`, `demote_to_cold`, `cleanup_dead_connections`: each sends `BfPeerCmd::Deregister` alongside `candidate_chains.remove()`

**`Node::run()`** (`mod.rs`):
- Creates `(bf_peer_cmd_tx, bf_peer_cmd_rx)` channel before spawning `BlockFetchLogicTask`
- Calls `bf_task.set_peer_cmd_rx(bf_peer_cmd_rx)`
- Calls `lc.set_bf_peer_cmd_tx(bf_peer_cmd_tx)` on the `ConnectionLifecycleManager`

## Critical correctness invariant (discovered during devnet)

**NEVER cross-peer dispatch**: Each fetch range must be sent to the peer that ADVERTISED those headers via ChainSync.

**Root cause of original stall**: The old `evaluate_and_fetch` collected headers from ALL peers, sorted them by slot, and used `BlockFetchDecision::select_peer()` which assigned ranges to the lowest-latency peer regardless of who advertised the block. Peer B (which hadn't forged/received those blocks) would return `MsgNoBlocks`. The `in_flight` entries for those blocks would timeout after 60s, and the relay stalled.

**Fix**: `evaluate_and_fetch` now groups `new_headers` by advertising peer (`per_peer_headers: HashMap<SocketAddr, Vec<&PendingHeader>>`), sorts each peer's headers by slot, batches them with `batch_peer_headers_into_ranges`, and dispatches each peer's ranges ONLY to that peer's worker. Cross-peer dedup is handled by the `in_flight` hash map filtering.

**Haskell alignment**: `fetchDecisions` in `Ouroboros.Network.BlockFetch.Decision` maintains per-peer candidate chains and dispatches fetch requests only to the advertising peer. "Best peer" selection is for choosing among multiple peers that all have the same block, not for cross-peer range dispatch.

## Remaining known issues (non-blocking)

**Stale in-flight warnings** (`purged stale in-flight blocks`): The `in_flight` map entries are never cleared on successful delivery (no `mark_received` call from the worker). They timeout after 60s. Benign — `has_block` skips them in subsequent ticks. Fix: add a delivery notification back from the worker to the decision task (requires additional channel or callback). Left as follow-up.

## Devnet validation results

- 25-minute soak, epoch 0→1→2 crossing
- BP forged 457 blocks; relay applied 457 blocks (1:1)
- **p4 tip-parity: 100% (168/168 ticks)**
- max tip-age: 6s (p5 pass)
- 2 blockfetch workers running concurrently throughout
- verify.sh: all 5 predicates PASS

## `FetchRange.slot_bounds()` 

Added to `dugite_network::protocol::blockfetch::decision::FetchRange` to compute `(from_slot, to_slot)` tuple for in-flight hash marking.
