---
name: chainsync-intersection-vs-rollback-distinction
description: MsgIntersectFound is NOT a wire-level MsgRollBackward - the two are structurally distinct protocol transitions; RolledBackPastIntersection vs InvalidIntersection semantics; point-selection (mkOffsets) algorithm
type: reference
---

# ChainSync: intersection-finding is not "rollback" (Question that keeps recurring)

Sources (read 2026-07-30, `main` branch):
- `IntersectMBO/ouroboros-network`:
  `ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/ChainSync/Type.hs`
- `IntersectMBO/ouroboros-consensus`:
  `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/MiniProtocol/ChainSync/Client.hs`

## Key correction to a common misreading of the protocol

There is **no wire-level "initial MsgRollBackward" sent by the server after
MsgIntersectFound.** The typed-protocol state machine keeps intersection
lookup and steady-state sync in genuinely separate states (Type.hs:107-169):

```haskell
data Message (ChainSync header point tip) from to where
  MsgRequestNext      :: Message ... StIdle (StNext StCanAwait)
  MsgAwaitReply       :: Message ... (StNext StCanAwait) (StNext StMustReply)
  MsgRollForward      :: header -> tip -> Message ... (StNext any) StIdle
  MsgRollBackward     :: point  -> tip -> Message ... (StNext any) StIdle
  -- | ... The list of points should be ordered by the preference, e.g.
  -- highest slot number to lowest.
  MsgFindIntersect    :: [point] -> Message ... StIdle StIntersect
  MsgIntersectFound   :: point -> tip -> Message ... StIntersect StIdle
  MsgIntersectNotFound:: tip           -> Message ... StIntersect StIdle
  MsgDone             :: Message ... StIdle StDone
```

`MsgRollBackward` only ever exists as a response from the `StNext` state
(i.e. only ever follows a `MsgRequestNext`). `MsgIntersectFound` /
`MsgIntersectNotFound` transition `StIntersect -> StIdle` directly — this is
a completely different arrow in the state machine, never `MsgRollBackward`.
**A conforming server cannot and does not send `MsgRollBackward` as part of
intersection negotiation.** Any implementation that models "found
intersection at point P" as an internal/wire rollback-to-P message is
diverging from the actual protocol shape, even if it's semantically similar.

## How the consensus ChainSync client actually establishes the starting candidate fragment

`Client.hs`, function `intersectFound` (called from `recvMsgIntersectFound`,
NOT from the rollback path):
```haskell
intersectFound intersection theirTip = Stateful $ \uis -> do
  ...
  (theirFrag, theirHeaderStateHistory) <- do
    case attemptRollback intersection (ourFrag `withTime` ourHeaderStateHistory, ourHeaderStateHistory) of
      Just (c, d, _oldestRewound) -> return (c, d)
      Nothing ->
        -- The @intersection@ is not on our fragment, even though we sent
        -- only points from our fragment to find an intersection with. The
        -- node must have sent us an invalid intersection point.
        disconnect $ InvalidIntersection intersection (ourTipFromChain ourFrag) theirTip
```
This calls `attemptRollback` on **our own fragment** (`ourFrag`), not on any
in-progress candidate fragment (there isn't one yet — this IS the
initialization of `theirFrag`). Because the points offered in
`MsgFindIntersect` were themselves selected from `ourFrag` (see below), the
server's answer — if honest — is guaranteed to be one of those points, so
`attemptRollback` here always succeeds. **This is why "the initial rollback
to the negotiated intersection" is never flagged as adversarial: it isn't
running the same check as steady-state rollback at all**, and by
construction of the offered point-set it cannot fail unless the server lies
about which point it found (which routes to `InvalidIntersection`, a
`ChainSyncClientException` → hard `disconnect` → network layer blacklists
the peer, per the module's top docstring: "the corresponding peer will never
be chosen again").

## Steady-state rollback check (the real "past-immutable-tip" guard)

`rollBackward` (only reachable via `recvMsgRollBackward` inside `StNext`,
i.e. real wire `MsgRollBackward` messages during ongoing sync):
```haskell
case attemptRollback rollBackPoint (theirFrag, theirHeaderStateHistory) of
  Nothing ->
    -- ... it rolled back to some point that is not on the fragment, which
    -- means that it tried to roll back to some point before one of the
    -- last @k@ headers we initially started from. We could never switch
    -- to this fork anyway, so just disconnect.
    terminateAfterDrain n $ RolledBackPastIntersection rollBackPoint (ourTipFromChain ourFrag) theirTip
  Just (theirFrag', theirHeaderStateHistory', mOldestRewound) -> ...
```
`RolledBackPastIntersection`'s own doc comment (`ChainSyncClientResult`):
> We were asked to roll back past the anchor point of the candidate's
> fragment. This means the candidate chain no longer forks off within @k@,
> making it impossible to switch to.

Crucially this is a `ChainSyncClientResult` handled via `terminate` /
`terminateAfterDrain` ("**Gracefully** terminate the connection with the
upstream node with the given result") — NOT a `ChainSyncClientException`
thrown via `disconnect`. So a legitimate deep rollback (peer's chain forked
off more than k blocks ago relative to our candidate fragment) ends the
mini-protocol session cleanly; it is treated as "this peer's chain isn't
useful to us right now", not as "this peer is malicious". Only
`InvalidIntersection` (server names an intersection point that was never
offered) and similar `ChainSyncClientException` constructors are the
adversarial/protocol-violation bucket that gets a peer permanently
blacklisted for the connection's lifetime.

## Point-selection order for MsgFindIntersect (`findIntersectionTop` / `mkOffsets`)

```haskell
let maxOffset = fromIntegral (AF.length ourFrag)
    k = protocolSecurityParam (configConsensus cfg)
    offsets = mkOffsets k maxOffset
    points = map castPoint $ AF.selectPoints (map fromIntegral offsets) ourFrag

mkOffsets :: SecurityParam -> Word64 -> [Word64]
mkOffsets (SecurityParam k) maxOffset =
  [0] ++ takeWhile (< l) [fib n | n <- [2 ..]] ++ [l]
 where l = unNonZero k `min` maxOffset
```
Doc comment: "We use the fibonacci sequence to try blocks closer to our tip,
and fewer blocks further down the chain. It is important that this sequence
contains at least a point @k@ back: if no intersection can be found at most
@k@ back, then this is not a peer that we can sync with." Example for k=2160:
`[0,1,2,3,5,8,13,21,34,55,89,144,233,377,610,987,1597,2160]`. Offset 0 =
our current tip; each successive offset counts further back from the tip
along `ourFrag`, capped at `min(k, fragment length)` so the last point sent
is always the oldest point our fragment can offer (at most k back). This
list of points, ordered tip-to-oldest, is exactly what `SendMsgFindIntersect
points` transmits — matching the wire protocol's own doc requirement that
points be "ordered by preference, e.g. highest slot number to lowest."

## Rust translation notes for dugite

If dugite's ChainSync client currently treats "found intersection" as
"apply a rollback and validate it against the immutable tip / RolledBack-
PastIntersection-style check", that is very likely the #929-class bug: the
initial-intersection path should never run the steady-state
past-the-fragment-anchor check at all — it's building the FIRST candidate
fragment, not truncating an existing one. The steady-state guard belongs
exclusively on real `MsgRollBackward` messages received while already in
`StNext`/known-intersection state. Keep the two code paths (and their two
distinct outcomes — graceful session-end vs. hard peer-ban) separate,
mirroring `intersectFound` vs `rollBackward` in Client.hs.
