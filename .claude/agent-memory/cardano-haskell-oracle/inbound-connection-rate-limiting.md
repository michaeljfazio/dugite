---
name: Inbound Connection Rate-Limiting Architecture
description: Complete reference for ouroboros-network's inbound connection limits: AcceptedConnectionsLimit, soft/hard limits, per-IP policy, rejection behaviour, trace events
type: reference
---

## Key Files

- `ouroboros-network/framework/lib/Ouroboros/Network/Server/RateLimiting.hs` — `AcceptedConnectionsLimit` type, `runConnectionRateLimits`, `getRateLimitDecision`, trace ADT
- `ouroboros-network/framework/lib/Ouroboros/Network/Server.hs` — accept loop wiring, `acceptLoop`, hard-limit check after accept
- `ouroboros-network/framework/lib/Ouroboros/Network/ConnectionManager/Core.hs` — `includeInboundConnectionImpl`, `countIncomingConnections`, `mkPruneAction`, `connectionStateToCounters`
- `ouroboros-network/framework/lib/Ouroboros/Network/ConnectionManager/Types.hs` — `PrunePolicy` type alias, `simplePrunePolicy`, `ConnectionManagerCounters`, `inboundConns`
- `ouroboros-network/lib/Ouroboros/Network/Diffusion/Configuration.hs` — `defaultAcceptedConnectionsLimit` (hard=512, soft=384, delay=5s)
- `ouroboros-network/lib/Ouroboros/Network/Diffusion/Policies.hs` — `prunePolicy` (sorts by upstream/inbound + random score)
- `ouroboros-network/framework/lib/Ouroboros/Network/Server/ConnectionTable.hs` — legacy per-subscription valency counter, NOT used in P2P accept path
- `cardano-node/src/Cardano/Node/Configuration/POM.hs` — JSON key `"AcceptedConnectionsLimit"` (optional, defaults to `defaultAcceptedConnectionsLimit`)

## Default Values

```haskell
defaultAcceptedConnectionsLimit :: AcceptedConnectionsLimit
defaultAcceptedConnectionsLimit =
  AcceptedConnectionsLimit {
    acceptedConnectionsHardLimit = 512,
    acceptedConnectionsSoftLimit = 384,
    acceptedConnectionsDelay     = 5   -- DiffTime, seconds
  }
```

## No Per-IP Cap

There is NO per-IP (per-remote-address) cap in the ouroboros-network P2P accept path.
`Server.ConnectionTable` tracks valency counters for subscription workers (legacy, outbound DNS subscriptions) — it is not used in the P2P `Server.with` / `InboundGovernor` path.
The only protection is the global `inboundConns` count.

## No Per-IP Rate Limiter (Token Bucket)

There is NO per-IP token bucket or leaky bucket rate limiter. `runConnectionRateLimits` operates on the *global* `numberOfConnections` STM counter, not per-address. A single IP can consume all 512 slots.

## Soft Limit: Linear Delay on Accept Loop

`getRateLimitDecision` uses three branches:
- `numberOfConnections < softLimit (384)` → `NoRateLimiting` — accept immediately
- `softLimit <= n < hardLimit (512)` → `SoftDelay d` where `d = (n - softLimit) * 5s / (hardLimit - softLimit)` — linear interpolation from 0 at soft limit to 5s at hard limit
- `n >= hardLimit` → `HardLimit 512` — block accept loop

`runConnectionRateLimits` is called BEFORE each `runAccept` call in the accept loop, checked against `numberOfConnections connectionManager` (STM).

## Hard Limit: Double-Checked in includeInboundConnection

After accepting a TCP socket AND after checking `runConnectionRateLimits`, the connection manager's `includeInboundConnectionImpl` performs a SECOND check inside the state TMVar:

```haskell
canAccept = numberOfCons + 1 <= fromIntegral hardLimit
if canAccept then ... else return (state, Left ReachedInboundConnectionHardLimit)
```

If this fails, `includeInboundConnection` returns `Disconnected connId ReachedInboundConnectionHardLimit` — the accept loop then calls `close snocket socket`, silently dropping the TCP connection.

## Pruning (not direct rejection)

When an outbound-duplex peer transitions to InboundIdleState and doing so would exceed the hard limit, `mkPruneAction` is called. It uses `prunePolicy` to select `numberToPrune` existing inbound connections to kill (by sending `AsyncCancelled` to their threads and marking their state as `TerminatedState Nothing`). This is a separate mechanism from the accept-loop hard limit.

`prunePolicy` (Diffusion.Policies) sorts by:
1. `isUpstream connType` — True (outbound) sorts LAST (i.e., inbound-only peers are pruned first)
2. random score (uniform Int from StdGen)
3. connType

## What counts as "inbound" for the limit

`countIncomingConnections` sums `inboundConns` from `connectionStateToCounters`. States that count:
- `UnnegotiatedState Inbound` → +1 inbound
- `InboundIdleState _ _ _ _` → +1 inbound (plus +1 duplex or unidirectional)
- `InboundState _ _ _ _` → +1 inbound
- `DuplexState` → +1 inbound (also +1 outbound, +1 duplex, +1 fullDuplex)

Outbound states (ReservedOutbound, OutboundUni, OutboundDup, OutboundIdle) do NOT count.
Terminating/Terminated do NOT count.

## Rejection Behaviour (what the remote sees)

Hard limit in accept loop: socket accepted at OS level then immediately closed — remote sees TCP RST or clean close (no Mux/handshake frame sent). No Ouroboros error message.

Hard limit in includeInboundConnectionImpl: same — socket closed, `Disconnected` returned.

HardLimit block in runConnectionRateLimits: accept loop BLOCKS (STM retry) until `numberOfConnections` drops below `hardLimit`, then delays `max(0, acceptedConnectionsDelay - waited)` before accepting the next connection. No rejection during that time — the listening socket's OS backlog accumulates.

## Trace Events

From `Server.Trace`:
- `TrAcceptPolicyTrace (ServerTraceAcceptConnectionRateLimiting delay n)` — emitted when soft limit active, with delay and current count
- `TrAcceptPolicyTrace (ServerTraceAcceptConnectionHardLimit limit)` — emitted when hard limit reached, waiting
- `TrAcceptPolicyTrace (ServerTraceAcceptConnectionResume n)` — emitted when hard limit clears

From `ConnectionManager.Core.Trace`:
- `TrPruneConnections pruneSet numberToPrune choiceSet` — emitted when existing connections are killed to make room

No explicit "TrRejectedDueToHardLimit" event from the connection manager — the `includeInboundConnection` path just returns `Disconnected` silently.

## N2C Local Connection Limits

The local (Node-to-Client) connection manager uses:
```haskell
localConnectionLimits = AcceptedConnectionsLimit maxBound maxBound 0
```
i.e., effectively unlimited connections for local socket (cardano-cli, wallet backends, etc.).

## JSON Config

Optional key `"AcceptedConnectionsLimit"` in node config JSON. Parsed as `.:?` so it defaults to `defaultAcceptedConnectionsLimit` if absent. Mainnet default config does NOT include this key → uses 512/384/5s defaults.

## ConnectionTable (legacy, NOT P2P)

`Server.ConnectionTable` is a map keyed by `(remoteAddr, ConnectionDirection)` tracking `ValencyCounter`s for DNS subscription workers (pre-P2P outbound-only code path). It is NOT wired into `Server.with` / `InboundGovernor` / `ConnectionManager.Core`. It cannot reject or rate-limit inbound connections in the P2P node.
