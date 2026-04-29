---
name: ConnectionId tuple keying for full-duplex N2N
description: ConnectionLifecycleManager keys by (local, remote) — matches Haskell ConnMap; enables co-located BP+relay diffusion
type: project
---

# ConnectionId tuple keying — 2026-04-29

## Why

Before this change, `ConnectionLifecycleManager.connections: HashMap<SocketAddr, PeerConnection>` used the *remote* SocketAddr alone as the key. When a co-located cardano-node relay running with SO_REUSEPORT dialed dugite (relay TCP src = its listen port = 3002, dst = dugite:3001), dugite's listener accepted with peer_addr = 127.0.0.1:3002 — colliding with dugite's existing outbound connection key (also 127.0.0.1:3002). `register_inbound_connection` rejected the inbound. Forged blocks reached dugite's ChainDB but never propagated.

## Fix

Keyed by `ConnectionId { local: SocketAddr, remote: SocketAddr }` matching Haskell `Ouroboros.Network.ConnectionId`. Outbound (ephemeral source) and inbound (our listen port) now produce DIFFERENT ConnectionIds and coexist. Full Haskell `Overwritten` semantic is also implemented for true same-CID collisions: inbound wins, outbound yields. `Ord` instance sorts remote-first then local — matches Haskell's load-bearing ordering used in `ConnMap.toMap`.

## Files changed

- `crates/dugite-network/src/bearer/tcp.rs` — added `TcpBearer::local_addr()`.
- `crates/dugite-node/src/node/peer_connection.rs` — `PeerConnection` now records `local_addr` and `direction: PeerConnectionDirection`.
- `crates/dugite-node/src/node/connection_lifecycle.rs` — `ConnectionId` struct + helpers (`find_outbound_cid`, `find_any_cid`, `has_outbound_to`, `has_any_to`); all `connections` accesses keyed by ConnectionId; `register_inbound_connection` is async (it shuts down displaced entries on collision); `register_warm_connection` and `promote_to_warm` yield (drop outbound) on same-CID collision.
- `crates/dugite-node/src/node/mod.rs` — `bind_n2n_listener()` uses socket2 with `SO_REUSEADDR + SO_REUSEPORT`; `lifecycle.set_local_listen_addr(self.listen_addr)` is now called when `DiffusionMode::InitiatorAndResponder`.
- `crates/dugite-node/Cargo.toml` — added `socket2` dep for the listener helper.

## Behavior consequences

- Demote-to-cold closes ALL connections to a remote (not just one) — matches Haskell `unregisterPeerConnection`.
- `peer_connected` is only invoked on the FIRST physical connection to a remote so the logical OutboundIdle state is not overwritten by an inbound. The inbound calls `mark_peer_duplex` instead.
- `cleanup_dead_connections` only emits `peer_disconnected` when the LAST surviving connection to a remote dies.
- `connect_from(local_listen_addr)` on outbound falls back to ephemeral on bind failure — safe across platforms with varying REUSEPORT semantics.
- Hot promotion picks the OUTBOUND connection of a duplex pair (initiator-side hot protocols). Server protocols run on every connection regardless of direction.

## How to apply

Block diffusion (`block_announcement_tx` broadcast) and tip-change re-diffusion automatically reach every connection, including the inbound of a duplex pair. No change needed in the forge or sync paths — they already publish to the broadcast channel.

The unit tests in `connection_lifecycle.rs::tests` (`duplex_pair_coexists_under_distinct_local_addrs`, `cleanup_dead_keeps_peer_when_other_connection_alive`, `connection_id_orders_by_remote_then_local`, `same_connection_id_hashmap_replaces_existing_entry`) are the regression net. Use `insert_fake_duplex_for_test` to construct duplex pair fixtures.
