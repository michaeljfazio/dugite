---
name: n2n-diffusionmode-dataflow-duplex-gating
description: N2N handshake DiffusionMode acceptableVersion (min rule), DataFlow derivation, and exactly what gates a node running Initiator-role mini-protocols (incl. KeepAlive client) on an ACCEPTED connection
type: reference
---

## Pin used (cn 11.0.1, CHaP index-state 2026-05-02T16:21:41Z)

cardano-node 11.0.1's `cardano-node.cabal` depends on BOTH, simultaneously (a mid-migration split of the old `ouroboros-network` monolith):
- `ouroboros-network ^>=1.1` → github:intersectmbo/ouroboros-network @ `a98c88583fa27ac4e567095f8766216442cbb74d` (still hosts `framework`, `api` DataFlow/ConnectionManager/InboundGovernor/PeerStateActions, `KeepAlive.hs`)
- `cardano-diffusion ^>=1.0` → github:intersectmbo/ouroboros-network @ `17525c337a6209011fabd6916fa757efd44df6f8` (new home of `Cardano.Network.NodeToNode`, `Cardano.Network.NodeToNode.Version`, `Cardano.Network.Diffusion`)

Both are subdir-pins of the SAME monorepo at DIFFERENT commits (CHaP pins each cabal component independently — resolve via `foliage/packages.json` in `IntersectMBO/cardano-haskell-packages` branch `repo`, filtering `timestamp <= index-state`, taking the latest). `ouroboros-consensus-diffusion 0.26.0.0` → github:IntersectMBO/ouroboros-consensus @ `7e5999e90b490d6ae26f7073578adca2f0cbf84e`.

Method: see [[chap-dependency-pinning-methodology]].

## 1. NodeToNodeVersionData / DiffusionMode

`cardano-diffusion/api/lib/Cardano/Network/NodeToNode/Version.hs`:
```haskell
data NodeToNodeVersionData = NodeToNodeVersionData
  { networkMagic  :: !NetworkMagic
  , diffusionMode :: !DiffusionMode
  , peerSharing   :: !PeerSharing
  , query         :: !Bool
  }

instance Acceptable NodeToNodeVersionData where
    acceptableVersion local remote
      | networkMagic local == networkMagic remote
      = let acceptedDiffusionMode = diffusionMode local `min` diffusionMode remote
         in Accept NodeToNodeVersionData
              { networkMagic  = networkMagic local
              , diffusionMode = acceptedDiffusionMode
              , peerSharing   = peerSharing local <> peerSharing remote
              , query         = query local || query remote
              }
      | otherwise = Refuse ...
```
`ouroboros-network/api/lib/Ouroboros/Network/DiffusionMode.hs`:
```haskell
data DiffusionMode = InitiatorOnlyDiffusionMode | InitiatorAndResponderDiffusionMode
  deriving (Eq, Ord, ...)
```
Ctor declaration order = derived `Ord` order, so `InitiatorOnlyDiffusionMode < InitiatorAndResponderDiffusionMode`. **`min` ⇒ if EITHER side declares InitiatorOnly, the accepted/negotiated value for that connection is InitiatorOnly.** Symmetric (both peers independently compute the identical accepted value).

Wire encoding (`nodeToNodeCodecCBORTerm`): `diffusionMode` is a CBOR bool — `True` = InitiatorOnly, `False` = InitiatorAndResponder — the 2nd element of the 4-element handshake term list `[networkMagic, diffusionModeBool, peerSharingInt, queryBool]`.

## 2. DataFlow — the connection-lifetime property derived from the NEGOTIATED diffusionMode

`cardano-diffusion/lib/Cardano/Network/NodeToNode.hs`:
```haskell
-- | Node-To-Node protocol connections which negotiated
-- `InitiatorAndResponderDiffusionMode` are `Duplex`.
ntnDataFlow :: NodeToNodeVersionData -> DataFlow
ntnDataFlow NodeToNodeVersionData { diffusionMode } =
  case diffusionMode of
    InitiatorAndResponderDiffusionMode -> Duplex
    InitiatorOnlyDiffusionMode         -> Unidirectional
```
`ouroboros-network/framework/lib/Ouroboros/Network/ConnectionManager/Types.hs`:
```haskell
-- | Each connection negotiates if it is uni- or bi-directional. 'DataFlow'
-- is a life time property of a connection, once negotiated it never changes.
data DataFlow = Unidirectional | Duplex
```
`ntnDataFlow` is threaded as `daNtnDataFlow` → `CM.connectionDataFlow` (a `versionData -> DataFlow` field of `ConnectionManager.Arguments`, `ConnectionManager/Core.hs:140`). On BOTH inbound and outbound handshake completion, `ConnectionManager/Core.hs:978`/`:1648`: `let dataFlow = connectionDataFlow versionData` where `versionData` is the ACCEPTED/negotiated `NodeToNodeVersionData` (post-`acceptableVersion`, i.e. already `min`'d) — NOT the raw remote-declared value. This `dataFlow` is stored permanently in the connection's state (`InboundIdleState connId thread handle dataFlow`, etc.) and never recomputed.

## 3. What actually gates running the client (Initiator) role on an ACCEPTED connection

Three independent layers, all must line up:

**(a) Bundle construction — decided ONCE per node PROCESS from LOCAL config only, not per-connection, not from handshake data.** `ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Node.hs:727-756` (`mkDiffusionApplications`) builds BOTH bundles unconditionally at startup:
- `daApplicationInitiatorMode` = `NTN.initiator ...` → every protocol incl. KeepAlive is `InitiatorProtocolOnly (clientFn)` — **no server/responder function exists at all**.
- `daApplicationInitiatorResponderMode` = `NTN.initiatorAndResponder ...` → every protocol incl. KeepAlive is `InitiatorAndResponderProtocol clientFn serverFn` — both exist, always, for every connection this process ever handles.

`ouroboros-network/lib/Ouroboros/Network/Diffusion.hs:755` (`case dcMode of`, `dcMode = diffusionMode` at line 217, i.e. the NODE'S OWN local `DiffusionMode` config — cardano-node's `--diffusion-mode`/topology default) picks between `withConnectionManagerInitiatorOnlyMode` (uses `MuxInitiatorConnectionHandler`, mux type `Mx.InitiatorMode`) and `withConnectionManagerInitiatorAndResponderMode` (uses `MuxInitiatorResponderConnectionHandler daNtnDataFlow`, mux type `Mx.InitiatorResponderMode`) for the WHOLE diffusion instance. A relay/BP running the normal `InitiatorAndResponderDiffusionMode` config therefore has BOTH functions compiled into every connection's mux bundle — this alone does NOT mean the client function runs.

**(b) Per-connection eligibility — DataFlow must be Duplex.** `ouroboros-network/framework/lib/Ouroboros/Network/InboundGovernor.hs`: on an INBOUND connection, `tDataFlow = connectionDataFlow csVersionData` (line 667); only `Duplex` connections are added to `freshDuplexPeers`/eventually `matureDuplexPeers` (lines 387-391 — the `Unidirectional` case is a no-op, the peer is simply never tracked). This fresh/mature-duplex tracking is what makes an inbound connection VISIBLE to the P2P peer-selection governor as a candidate for "promotion" (running our own Initiator-role protocols toward it). A `Unidirectional` inbound connection NEVER enters this set.

**(c) Activation — `PeerStateActions.hs` `activatePeerConnection`/`establishPeerConnection` toggles per-temperature `ControlMessage` TVars** (`ahControlVar`, `Continue`/`Terminate`) that gate whether a spawned mini-protocol thread actually proceeds. `establishPeerConnection` sets `Warm=Continue, Established=Continue` (KeepAlive lives in the `WithEstablished` tier per `nodeToNodeProtocols`, so it starts as soon as a peer reaches WARM, before HOT); `activatePeerConnection` additionally sets `Hot=Continue` (unlocking ChainSync/BlockFetch/TxSubmission, the `WithHot` tier). This machinery is invoked EITHER for a genuinely dialed outbound peer, OR — for an already-established Duplex inbound connection — via `ConnectionManager/Core.hs`'s reuse ("Awake") transition: `requestOutboundConnection` on an address already in `InboundIdleState _ _ _ Duplex` (lines ~1398, ~1747, comment `Awake^{Duplex}_{Local} : InboundIdleState Duplex → OutboundState Duplex`/`OutboundDupState`) re-purposes the SAME physical TCP connection for our own Initiator-role traffic — **no new socket, no new dial**. This reuse path is unreachable for `Unidirectional` connections (the code only pattern-matches `Duplex`).

**KeepAlive is NOT exempted from any of this.** It goes through the identical `InitiatorAndResponderProtocol clientFn serverFn` construction and the identical Duplex+activation gating as ChainSync/BlockFetch/TxSubmission — the only difference is which TemperatureBundle tier it's in (`WithEstablished`, starts at "warm" not "hot") and its mux-level start policy is `StartOnDemandAny` (network-mux/src/Network/Mux/Types.hs:166-175 — "like StartOnDemand, but also start if data is received for ANY `StartOnDemand` protocol", a piggyback optimization, not a duplex-bypass).

## 4. Proof of intent: `cardano-ping` (backs `cardano-cli ping`) hardcodes InitiatorOnly

`cardano-ping/src/Cardano/Network/Ping.hs:135-143`: every proposed `NodeToNodeVersion` is paired with the literal `InitiatorOnly` (`modeToBool InitiatorOnly = True`). So any real cardano-node — and dugite, if fixed — computes `acceptedDiffusionMode = min(local, InitiatorOnly) = InitiatorOnly` ⇒ `DataFlow = Unidirectional` ⇒ never enters `freshDuplexPeers` ⇒ `activatePeerConnection`/KeepAlive-client is NEVER invoked toward it. This is why real cardano-node never sends an unsolicited `MsgKeepAlive` to `cardano-cli ping`.

## 5. BP→relay duplex pull (why it does NOT collide with the fix)

Real secure-BP topologies need the connection the BP dials OUT to its relay to be **Duplex**, not InitiatorOnly — Duplex is what lets the relay reuse that inbound connection (§3c "Awake" transition) to run ITS OWN ChainSync/BlockFetch/TxSubmission/KeepAlive CLIENT toward the BP (pulling new blocks), even though the BP was the TCP dialer. `InitiatorOnly` at the diffusion-mode level would make this impossible (Unidirectional connections cannot be "awakened" — code only matches `Duplex`).

The per-peer knob is `topology.json`'s local-roots `"diffusionMode"` field (`"InitiatorOnly"` / `"InitiatorAndResponder"`, **default `InitiatorAndResponderDiffusionMode`** if omitted — `ouroboros-network/orphan-instances/.../OrphanInstances.hs:132`: `o .:? "diffusionMode" .!= InitiatorAndResponderDiffusionMode`), living in `LocalRootPeersGroup.rootDiffusionMode` (`ouroboros-network/lib/Ouroboros/Network/Diffusion/Topology.hs:39`). A `"behindFirewall": true` flag on that same local-root entry sets `Provenance = Inbound` instead of the default `Outbound` (`OrphanInstances.hs:130`) — this is the flag that tells the RELAY's local-root governor (`EstablishedPeers.hs` `jobPromoteColdPeer`) "this peer will dial ME; don't try to dial it, just recognize/reuse its inbound connection." `InitiatorOnly` is reserved for genuinely one-way clients that never want anything pulled back from them (wallets, `cardano-cli ping`); it is NOT the mechanism a firewalled BP uses toward its own relay.

## Fix implication for dugite (#TBD, inbound-accept KeepAlive-client bug)

dugite's inbound-accept path must NOT unconditionally spawn a KeepAlive-client task. It must compute the connection's DataFlow from the ACCEPTED `NodeToNodeVersionData.diffusionMode` (= `min(local_declared, remote_declared)`, i.e. actually apply the min-rule during handshake accept, not just echo the remote's raw value or the local config) and only start Initiator-role protocol instances (KeepAlive client, and — if/when dugite ever runs BP-relay style duplex pulls — ChainSync/BlockFetch/TxSubmission clients) toward that peer when `DataFlow == Duplex`. This will not break BP→relay duplex pulls because that scenario legitimately negotiates Duplex (default `InitiatorAndResponder` on the local-root entry), while `cardano-cli ping` legitimately negotiates Unidirectional (hardcoded `InitiatorOnly`).
