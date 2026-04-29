---
name: DuplexPeerConnection implementation
description: Full-duplex N2N: ConnectionId tuple keying + simultaneous-open Overwritten + bind-to-listen-port (2026-04-29)
type: project
---

## Issue #187: full-duplex N2N connections for mempool tx propagation

**Why:** Outbound N2N connections via `PipelinedPeerClient`/`PeerClient` advertise `initiator_only_diffusion_mode = true`.  The remote peer therefore never sends us TxSubmission2 requests, so our mempool txs are never propagated to peers we connect to outbound.

**What was built:** `crates/dugite-network/src/duplex.rs` — `DuplexPeerConnection` (Phase 1) and `serve_tx_submission` (Phase 2).

**Phase 3 (2026-04-29):** `ConnectionLifecycleManager` now keys connections by `ConnectionId { local, remote }` matching Haskell `Ouroboros.Network.ConnectionId`. Outbound + inbound to the same remote coexist when their (local, remote) tuples differ, and same-CID collisions follow Haskell's `Overwritten` rule (inbound wins, outbound yields). `set_local_listen_addr` is wired in `mod.rs` for InitiatorAndResponder mode and the N2N listener now binds with `SO_REUSEADDR + SO_REUSEPORT` (`bind_n2n_listener`) so outbound `connect_from(listen_addr)` can succeed on Linux/macOS. With this, a co-located cardano-node relay's REUSEPORT outbound to dugite at `127.0.0.1:listen` produces an inbound that does NOT collide with our outbound (different local ports) — block diffusion path is fully duplex symmetric.

## Key implementation details

### Pallas Plexer subscription semantics
- `subscribe_client(P)` → sends on P (bit-15=0), receives on P|0x8000 — use for protocols WE initiate
- `subscribe_server(P)` → sends on P|0x8000, receives on P — use for protocols THEY initiate

For a duplex outbound connection:
- ChainSync (2), BlockFetch (3), KeepAlive (8), PeerSharing (10) → `subscribe_client`
- TxSubmission2 (4) → `subscribe_server` (remote peer requests OUR txs)

### Handshake version table
Must use `initiator_only_diffusion_mode = false` (vs the `true` in `VersionTable::v7_and_above`).
Advertising versions 14 and 15 only (matching cardano-node 10.x).
The 4-element params array `[magic, initiator_only, peer_sharing, query]` requires version ≥ 13.

### TxSubmission2 server protocol flow
1. Wait for peer MsgInit [6] (TXSUB_INIT_TIMEOUT = 30s)
2. Reply MsgInit [6]
3. Loop:
   - MsgRequestTxIds [0, blocking, ack, req] → MsgReplyTxIds [1, [[hash, size], ...]]
   - MsgRequestTxs [2, [hash, ...]] → MsgReplyTxs [3, [cbor, ...]]
   - MsgDone [4] → exit

### Inflight cap
MAX_TX_INFLIGHT = 1000 (matches inbound server in n2n_server.rs).
When at cap, send empty reply to let peer ack before we push more IDs.

### Files changed
- `crates/dugite-network/src/duplex.rs` (new, 900 lines)
- `crates/dugite-network/src/lib.rs` (added `pub mod duplex`, re-exports)

### Phase 3 (TODO)
Wire `DuplexPeerConnection` into the governor connect loop in `governor.rs` or the node's peer connection management so hot peers use duplex connections instead of pipelined-only connections.
