---
name: nodepeermanager-orphaned-methods-upstream-audit
description: Commit-pinned verdicts for dugite #1003 (4 orphaned NodePeerManager methods) against ouroboros-network PeerMetric/InboundGovernor/PublicRootPeers/ConnMap - no BlockFetch success reward, eager inbound-maturity GC IS upstream pattern, no unified PeerCategory type, no IP-only connection lookup
type: reference
---

## Pins (same as [[haa-outbound-connections-state-verified]], reused here)
`IntersectMBO/ouroboros-network` monorepo:
- `ouroboros-network` subdir -> rev `a98c88583fa27ac4e567095f8766216442cbb74d`
- `cardano-diffusion` subdir -> rev `17525c337a6209011fabd6916fa757efd44df6f8`
Both are the exact CHaP-resolved deps for cardano-node 11.0.1.

## Q1: no BlockFetch-success reputation/reward mechanism anywhere
`ouroboros-network/lib/Ouroboros/Network/PeerSelection/PeerMetric.hs` exports exactly
4 metric fns: `joinedPeerMetricAt`, `upstreamyness`, `fetchynessBytes`, `fetchynessBlocks`.
No 5th "success count"/reputation field exists. All 4 are consumed ONLY in
`hotDemotionPolicy` (`Diffusion/Policies.hs` + `cardano-diffusion/.../Policies.hs`,
churn-mode variant) — used to pick which already-HOT peers to demote first
(lowest score = demoted first: `sortOn (\(peer,rn) -> (Map.findWithDefault (0,Nothing)
peer scores, rn))`). Every PROMOTION policy (cold->warm, warm->hot, inbound,
peer-share) is `simplePromotionPolicy` = pure random (`addRand rngVar available (,)`,
zero metric input).

`jobPromoteColdPeer`'s success branch (`Governor/EstablishedPeers.hs`) only sets two
booleans on success: `KnownPeers.setSuccessfulConnectionFlag` +
`KnownPeers.clearTepidFlag` — grepped ALL fetched Governor/State modules,
`resetFailCount` (State/KnownPeers.hs) has ZERO callers from any promotion-success
path; its only caller is internal to `KnownPeers.setCurrentTime`. Fail count decays
ONLY via a fixed 120s timer (`policyClearFailCountDelay`, `clearFailCountTimes` PSQ),
never via a subsequent success. Failure path: `KnownPeers.reportFailures` does
exponential backoff (5s * 2^(failCount-1), capped 2^5, +/-2s jitter), forgets peer
after `policyMaxConnectionRetries`.

**Verdict**: success = pure "absence of failure, decaying on a fixed clock." No
upstream analog for an incrementing, capped `reputation: f64` field feeding ANY
governor decision. dugite's `record_success`/`PeerInfo.reputation` -> DELETE, no
upstream mechanism shaped like it. dugite's already-wired `fetchyness_bytes` is the
correct `fetchynessBytes` analog (used in ChurnModeBulkSync demotion scoring).

## Q2: inbound "maturation" IS eagerly, actively pruned upstream — not read-filter-only
`InboundGovernor/State.hs`: TWO structures, `matureDuplexPeers :: Map peerAddr
versionData` and `freshDuplexPeers :: OrdPSQ peerAddr Time versionData` (ordered by
arrival time). `InboundGovernor.hs`: `inboundMaturePeerDelay = 15*60` (matches
dugite's `INBOUND_MATURE_PEER_DELAY`). The transition is wired as its OWN wake arm
in the main governor loop's STM race:
```haskell
FirstToFinish do
  case maturedPeers time (freshDuplexPeers state) of
    (as, _)     | Map.null as -> retry
    (as, fresh) -> (MaturedDuplexPeers as fresh :) <$> InfoChannel.readMessages infoChannel
```
```haskell
maturedPeers time freshPeers =
      first (Map.fromList . map (\(addr,_p,v) -> (addr,v)))
    $ OrdPSQ.atMostView ((-inboundMaturePeerDelay) `addTime` time) freshPeers
```
`OrdPSQ.atMostView` actively POPS every matured entry out of the fresh PSQ; result
merged into `matureDuplexPeers`. `mkPublicState`/`readInboundPeers` just reads the
already-clean `matureDuplexPeers` field — no filtering happens at read time. On
disconnect, `unregisterConnection` deletes from BOTH maps.

**Verdict**: opposite of read-time-filter-only. Upstream eagerly transitions entries
out of a time-ordered PSQ on a schedule tied into the SAME loop that's already
running. dugite's `gc_fresh_inbound()` -> WIRE it, called every tick of whatever loop
already calls `fresh_inbound_set` (mirror `maturedPeers` riding the governor tick),
not left as a standalone dead helper.

## Q3: no unified PeerCategory sum type — independent Set-membership checks throughout
`PublicRootPeers.hs`:
```haskell
data PublicRootPeers extraPeers peeraddr =
  PublicRootPeers { getLedgerPeers :: !(Set peeraddr), getBigLedgerPeers :: !(Set peeraddr)
                   , getExtraPeers :: !extraPeers }
member p memberExtraPeers (PublicRootPeers lp blp ep) =
     memberExtraPeers p ep || Set.member p lp || Set.member p blp
```
`PeerSelectionState` holds `localRootPeers`/`publicRootPeers`/`knownPeers`/
`establishedPeers`/`activePeers` as 5 separate structures. The canonical
"classify this peer" site, `peerSelectionStateToView` (`Governor/Types.hs`), computes
every (category x lifecycle-state) via ad-hoc `Set.intersection`/`Set.\\` against
separately-held sets (`bigLedgerSet = PublicRootPeers.getBigLedgerPeers publicRootPeers`,
etc) — one line per combination, no classifier function. `PeerSource`
(LocalRoot/PublicRoot/PeerShare, `PeerSelection/Types.hs`) is a DIFFERENT axis
(discovery origin, set once in KnownPeerInfo) — not a live big-ledger-peer classifier.

**Verdict**: scattered independent Set-membership checks ARE the upstream-aligned
shape. dugite's `peer_category()` -> DELETE; dugite's real production code (separate
`local_root_groups.iter().any(...)` + `big_ledger_peers.contains(addr)` at each call
site) already matches upstream's architecture.

## Q4: no IP-only connection lookup anywhere — ConnMap always keys by full address
(First-pass hypothesis WRONG on verification: expected simultaneous-open handling
might need port-agnostic identity since an outbound dial's local ephemeral port
differs from the listen port. Checked `ConnMap.hs` directly — false.)

`ConnectionId.hs`:
```haskell
data ConnectionId addr = ConnectionId { localAddress :: !addr, remoteAddress :: !addr }
instance Ord addr => Ord (ConnectionId addr) where
    conn `compare` conn' = remoteAddress conn `compare` remoteAddress conn'
                         <> localAddress conn `compare` localAddress conn'
```
`ConnectionManager/ConnMap.hs`:
```haskell
-- | The outer map keys are remote addresses, the internal ones are local addresses.
newtype ConnMap peerAddr a = ConnMap { getConnMap :: Map peerAddr (Map (LocalAddr peerAddr) a) }
lookup ConnectionId { remoteAddress, localAddress } (ConnMap st) =
   case remoteAddress `Map.lookup` st of
     Nothing  -> Nothing
     Just st' -> LocalAddr localAddress `Map.lookup` st'
```
`peerAddr` = full `SockAddr` (IP+port) at the real N2N instantiation, same type as
`KnownPeers`/`readInboundPeers :: m (Map peeraddr PeerSharing)` everywhere else in
the governor. The ONE relaxed-match fn, `lookupByRemoteAddr`, STILL requires an
exact full `remoteAddress`; it only relaxes on which LOCAL entry answers (handles the
`ReservedOutboundState`/`UnknownLocalAddr` transient race window during outbound
connect setup) — a completely different axis from "same IP, different remote port".
`Core.hs` outbound-connect: `remoteAddress = peerAddr` is the literal dial target;
`localAddress` is provisional until `getLocalAddr` resolves post-`connect()`.

**Verdict**: no legitimate upstream precedent for IP-only/port-agnostic lookup,
anywhere in ConnectionManager.Core/ConnMap. Same #996-shaped mistake (see
[[project_996_mempool_revalidation_checklist]] for the rate-limiter instance),
now confirmed at the connection-manager layer too. dugite's
`find_inbound_duplex_by_ip(ip: IpAddr)` -> DELETE; dugite's real wired
`has_any_to(remote: SocketAddr)` (full ConnectionId matching) is already the
upstream-aligned shape.

## Net verdicts for dugite #1003
- `record_success`/`reputation` (Q1) -> DELETE
- `gc_fresh_inbound()` (Q2) -> WIRE (call from the same tick loop as `fresh_inbound_set`)
- `peer_category()` (Q3) -> DELETE
- `find_inbound_duplex_by_ip()` (Q4) -> DELETE

See also [[haa-outbound-connections-state-verified]], [[p2p-governor-architecture]],
[[n2n-connection-architecture]], [[inbound-connection-rate-limiting]],
[[responder-miniprotocol-termination-semantics]].
