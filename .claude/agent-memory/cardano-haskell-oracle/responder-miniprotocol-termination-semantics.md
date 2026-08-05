---
name: responder-miniprotocol-termination-semantics
description: Haskell mux/InboundGovernor behavior when a responder mini-protocol (e.g. ChainSync server) exits cleanly vs throws; ChainSync StMustReply timeout is client-only; "silent orphaned responder" is not upstream-representable
type: reference
---

## Pinned source
`IntersectMBO/ouroboros-network@main`, commit `c45735a56c567fa977969173d18943bac6bb3821`
(no `release-ouroboros-consensus-3.0.1.0` tag exists in this repo — that tag is in the
separate `ouroboros-consensus` repo). Fetched 2026-08-04.

## Q1: exception in a responder mini-protocol thread — fatal to the WHOLE mux
`network-mux/src/Network/Mux.hs`:
- `miniProtocolJob`'s `jobHandler` (L357-363) catches `SomeException`, evaluates to
  WHNF, does `tryPutTMVar completionVar (Left e)`, returns
  `MiniProtocolException miniProtocolNum miniProtocolDirEnum e`.
- `JobResult` doc (L649-663): *"A mini-protocol thread terminated with an exception.
  We always respond by terminating the whole mux."* / MuxerException and
  DemuxerException are *"Always fatal."*
- `monitor`'s `EventJobResult (MiniProtocolException pnum pmode e)` (L458-463):
  sets `muxStatus` to `Failed e`, traces `TraceExceptionExit`+`TraceState Dead`,
  then **`throwIO e`** — propagates out of `monitor`, out of `JobPool.withJobPool`,
  out of `run` (only `SomeAsyncException` is specially caught at L274-277, and even
  that re-throws after marking `Failed`). The mux's `run` action itself terminates
  with an exception → caller (connection handler) tears down the bearer/socket.
- One exception anywhere = the ENTIRE connection (all mini-protocols, both
  directions) dies. There is no "isolate the failure to one mini-protocol" mode.

## Q2: clean responder completion — Mux itself does NOT restart; InboundGovernor DOES
- `Mux.hs` `monitor`'s `EventJobResult (MiniProtocolShutdown pnum pmode)` (L452-456):
  comment *"Protocols that runs to completion are not automatically restarted."*
  Just traces `TraceCleanExit` and loops — mux stays up, this protocol is now idle
  (`miniProtocolStatusVar` reset to `StatusIdle` in `jobAction`, L342) and will
  never run again unless something external calls `runMiniProtocol`/`runResponder`.
- `ouroboros-network/framework/lib/Ouroboros/Network/InboundGovernor.hs` is that
  "something external" for N2N inbound duplex connections. `MiniProtocolTerminated`
  handling (L410-443):
  - `Left e` (exception) → `TrResponderErrored`, comment: *"a mini-protocol errored.
    In this case mux will shutdown, and the connection manager will tear down the
    socket."* No restart.
  - `Right _` (clean completion) → calls `runResponder tMux mpd` AGAIN with
    `StartOnDemand`, traces `TrResponderRestarted`. `Terminated` type's doc (L897-899):
    *"This is just enough to decide if we need to restart a mini-protocol and to do
    the restart."*
- So: exception = whole connection dies. Clean exit (only reachable via the
  protocol's own `MsgDone`/terminal state) = InboundGovernor immediately re-arms
  `StartOnDemand` so the NEXT byte on that (num,dir) spawns a fresh instance from
  scratch. `NewConnectionInfo` doc confirms: *"Inbound protocol governor will start
  responder protocols using 'StartOnDemand' strategy."*
- `StartOnDemand` vs `StartEagerly` only affects WHEN the thread first spawns
  (on first ingress bytes vs immediately); restartability after completion is
  governed by InboundGovernor's watch loop, not by which start mode was used.

## Q3: is a silent, permanent, connection-alive stall of one responder representable? NO.
Two independent reasons, both load-bearing:
1. **Type-level**: `ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/ChainSync/Server.hs`
   `chainSyncServerPeer` interprets a `ChainSyncServer` into `Server ... StIdle m a`.
   The ONLY path to `Done` is `MsgDone -> Effect $ Done <$> recvMsgDoneClient` (L119),
   itself only reachable when the CLIENT sends `MsgDone` (`Type.hs`: `MsgDone ::
   Message ... StIdle StDone`, `StateAgency StDone = NobodyAgency`). It is a
   **compile-time impossibility** to write server code that returns/exits mid-protocol
   without either throwing or following the client's MsgDone to StDone. There is no
   Haskell equivalent of "the async task just returns Ok(())" from the middle of
   serving ChainSync.
2. **Runtime backstop**: even if a responder job were somehow orphaned (no listener,
   connection alive), the demuxer (`network-mux/src/Network/Mux/Ingress.hs` L100-132)
   keeps writing inbound bytes into that mini-protocol's ingress `TVar` regardless of
   whether a job is draining it — dispatch is by static (num,dir) table, not by job
   liveness. Once buffered bytes exceed `maximumIngressQueue` for that protocol it
   throws `IngressQueueOverRun` (`Trace.hs` L64-66), a `DemuxerException`, which per
   `Mux.hs` L477-487 is `Failed` (not the `BearerClosed`-with-zero-jobs "clean" case)
   → kills the whole mux. So even a hypothetical orphan converts to a hard,
   observable failure once the peer sends enough more data — it cannot stay silent
   forever.
- Net: dugite's actual bug (#980-adjacent: responder task returns and is never
  polled again, mux/TCP stay up, inbound frames silently dropped, no restart, no
  teardown) has **no upstream analogue**. Haskell's two only terminal outcomes are
  "restart on demand" (clean) or "kill the connection" (exception) — a third
  "quietly orphaned forever" state doesn't exist in the model.

## Q4: ChainSync StMustReply timeout is CLIENT-side (initiator) ONLY
Confirmed via `ouroboros-network/framework/lib/Ouroboros/Network/Driver/Limits.hs`
`mkDriverWithLimits`'s `recvMessage` (L120-141): takes a `TheyHaveAgencyProof pr st`
— i.e. `recvMessage`/the timeout applies ONLY on the side that does NOT have agency
in that state (the side blocked waiting to receive). `ChainSync/Type.hs`:
`StateAgency StIdle = ClientAgency`, `StateAgency (StNext _) = ServerAgency`,
`StateAgency StIntersect = ServerAgency`. So:
- **StIdle** (client has agency): the SERVER calls `recvMessage` → SERVER applies
  this timeout. = `ChainSyncIdleTimeout`, default 3373s
  (`cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs`
  `defaultChainSyncIdleTimeout = ChainSyncIdleTimeout 3373`).
- **StNext StCanAwait / StNext StMustReply / StIntersect** (server has agency): the
  CLIENT calls `recvMessage` → CLIENT applies these. Per
  `cardano-diffusion/protocols/lib/Cardano/Network/Protocol/ChainSync/Codec/TimeLimits.hs`
  (`timeLimitsChainSync`, L43-88): `StNext StCanAwait` = `shortWait` (10s,
  `Ouroboros/Network/Protocol/Limits.hs`), `StIntersect` = `shortWait` (10s),
  `StNext StMustReply` = trustable peer → `waitForever`(`Nothing`); non-trustable →
  uniform random 135-269s (comment: "corresponds to 99.9% to 99.9999% thresholds").
- **The server has NO protocol-level timeout while producing a StMustReply reply.**
  It never calls `recvMessage` in that state (it's the one with agency, doing
  `sendMessage`/computing). Nothing in `Network.Mux`/`typed-protocols` bounds how
  long a responder may internally block before yielding `MsgRollForward`/
  `MsgRollBackward`. Server-side "am I taking too long" is purely an application
  concern (in ouroboros-consensus's ChainSync server, this manifests as blocking on
  ChainDB follower STM `retry`, unbounded).

## Q5: observable failure mode at the downstream (initiator) peer when a responder dies
- If the responder mini-protocol THROWS: per Q1, that side's whole mux dies →
  `run` throws → caller closes the bearer/socket (FIN/RST). Downstream peer's own
  demuxer eventually hits `BearerClosed` reading the socket → `DemuxerException` in
  its `monitor` (Mux.hs L477-487): if `JobPool.readGroupSize jobpool
  MiniProtocolJob == 0` (all their own jobs already idle) it's treated as `Stopped`
  (graceful); otherwise `Failed e` → `throwIO e` → their mux dies too. Either way it
  is OBSERVABLE: a torn-down TCP connection, not silence.
- Absent an immediate bearer close (e.g. the failure is the server internally
  hanging rather than throwing — not representable per Q3, but hypothetically), the
  downstream client's OWN `ProtocolLimitFailure(ExceededTimeLimit)` fires per Q4's
  table (10s in StCanAwait/StIntersect, 135-269s in StMustReply for untrusted
  peers) and the CLIENT disconnects on its own initiative.
- So the "the peer receives no further data and no error" failure mode dugite
  exhibits is not just unimplemented upstream — it contradicts the invariant that
  every code path here ends in either an explicit restart or an explicit,
  observable teardown.

## Relevant to dugite
CLAUDE.md "Current Focus" describes exactly the class of bug this maps to: an N2N
ChainSync server task returning (`Ok(())` or `Err(_)`) while mux/TCP stay alive and
inbound frames are silently dropped. The Haskell-correct fix shape is InboundGovernor's,
not Mux's: on a *clean* return, immediately re-register/restart the responder route
(equivalent of `StartOnDemand` re-arm) so new inbound bytes spawn a fresh handler; on
an *error* return, tear down the whole connection (abort the mux/close the socket),
never leave it half-alive. See also [[mux-connection-architecture]],
[[n2n-connection-architecture]], [[chainsync-at-tip]].
