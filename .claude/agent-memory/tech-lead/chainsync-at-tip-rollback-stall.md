---
name: ChainSync at_tip rollback stall
description: at_tip flag not reset on MsgRollBackward causes pipeline freeze at live tip after a fork
type: reference
---

## Bug Pattern

When the ChainSync pipelined client reaches live tip and receives MsgAwaitReply,
`at_tip` is set to `true`.  The pipeline refill condition is:

    if !at_tip && outstanding <= low_mark { refill }

If the peer subsequently sends MsgRollBackward (a fork at tip), `at_tip` is still
`true`, so the refill branch is never taken.  If `outstanding` is also low (or 0
— all pipelined requests consumed before MsgAwaitReply), no new MsgRequestNext
is ever sent.  The peer has nothing to respond to and the TCP connection eventually
times out with "bearer closed".

## Symptom

- All peers find intersection at the same slot
- Peers send MsgRollBackward to that slot
- ChainSync tasks either crash with "bearer closed" or restart and loop FindIntersect
- Node never receives MsgRollForward headers; BlockFetch never fires
- Node stays stuck at the rollback slot indefinitely
- The stall does NOT occur on the FIRST rollback (initial_rollback=true path does not
  set at_tip=true), only on SUBSEQUENT rollbacks after the node has been at tip

## Fix

In `crates/dugite-node/src/node/sync.rs`, `chainsync_client_task()`, inside the
`MsgRollBackward` arm, add `at_tip = false;` immediately after the `outstanding`
decrement — before the depth check and refill logic.

Commit: 5abaf2687

## When to Watch For This

Any time the node is at live tip and experiences a fork (competitive chain switch).
Preview testnet has more frequent forks than mainnet (~5-minute epochs, less
finality buffer).  The bug manifests on the SECOND rollback to the same slot
(e.g., a competing chain branch causes two consecutive rollbacks to the same
anchor).
