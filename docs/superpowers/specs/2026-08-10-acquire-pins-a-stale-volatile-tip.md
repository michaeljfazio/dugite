# `MsgAcquire VolatileTip` can pin a point that is not the volatile tip

Found by devnet-validate Round 1 on `worktree-nonmyopic-1067`, 2026-08-10.
Non-consensus, N2C query path. **Blocks the v2.8.0 gate.**

## Symptom

14 tx-zoo scripts failed with `BadInputsUTxO`. Not a contiguous category —
06d, 06e and 06f **passed**, interleaved between failures. The real pattern is
two bounded time windows.

## Mechanism — measured, not inferred

dugite-relay served a UTxO view **older than its own applied chain**, while its
LocalTxSubmission admission validated against the live tip. Scripts selecting
"largest UTxO at the shared wallet" therefore re-selected an input the same
node had already seen spent, and the same node then correctly rejected the tx
it had just helped build.

From `dugite-relay.log`, block applications bounding each window:

| window | opens (block applied) | closes (next block) | gap |
|---|---|---|---|
| 1 | 09:04:10.219 | 09:04:13.218 | 3.0 s |
| 2 | 09:04:20.218 | 09:04:24.218 | 4.0 s |

All 22 `InputNotFound` rejections fall strictly inside those gaps and stop when
the next block lands:

```
window 1:  11.079  11.731  12.391  13.019      (closes 13.218)
window 2:  21.599  22.055  22.710  23.172  23.626  24.092   (closes 24.218)
```

The last rejection in each window precedes the closing block by 0.2 s and
0.13 s. That is the signature of a view that refreshes only on block apply.

`cardano-cli transaction build` performs its own LSQ lookup of `--tx-in` and
fails loudly when the input is absent, so a successful build proves the served
view still contained the spent input — in a *fresh* acquire session per script.

## Root cause — verified in code

Two facts compose:

1. `acquire` pins the `NodeStateSnapshot` most recently published by
   `Node::update_query_state()` (`crates/dugite-node/src/node/query.rs:274`).
2. That publish is **rate-limited and skippable**
   (`crates/dugite-node/src/node/mod.rs:8416`):

```rust
let query_state_ran =
    if self.last_query_state_update.elapsed() >= query_state_refresh_interval(at_tip) {
        self.update_query_state().await;
        ...
```

with `query_state_refresh_interval` = **1 s at tip, 30 s during catch-up**
(`mod.rs:9649-9655`). When two blocks land under a second apart the second
block's refresh is skipped, and the snapshot then stays at the older block
until the *next* block arrives. An empty-slot gap makes that arbitrarily long —
3 s and 4 s here, and up to 30 s while catching up.

So `MsgAcquire VolatileTip` can pin a point that is not the volatile tip.

The rate limit is not gratuitous: the comment at the call site records that the
rebuild is ~1.4 s at mainnet scale and runs synchronously on the apply task, so
the catch-up cadence directly bounds bulk-sync throughput. Any fix has to keep
that property.

## Why this is dugite's defect and not the harness's

Upstream cannot express this state. cardano-node instantiates its
LocalStateQuery server as

```haskell
hStateQueryServer = \reg ->
  localStateQueryServer (ExtLedgerCfg cfg) $ \target ->
    ChainDB.allocInRegistryReadOnlyForkerAtPoint getChainDB target reg
```

(`ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Network/NodeToClient.hs`,
`release-ouroboros-consensus-3.0.1.0`) — every `MsgAcquire` allocates a forker
against the **current** LedgerDB, which chain selection updates synchronously.
There is no cached query snapshot, so `VolatileTip` cannot lag the node's own
adopted chain.

The harness is genuinely fragile (shared wallet, largest-UTxO selection, no
`zoo_wait_mempool_quiet`), and a sub-second query→submit race exists against
any node — that is what the archived `01a-simple-pay` failures are. But the
harness's only assumption is that a node's query view is not *seconds* behind
its own admission view, and that holds for cardano-node by construction.

## Regression status

The staleness is **pre-existing**; its visibility through UTxO queries is
**new on this branch**.

`d24c5608b6` ("fix(n2c): answer UTxO queries from the acquisition's pinned
point", the #1068 fix) wired `handle_utxo_by_address` to the acquired point.
Before it, that function called `provider.utxos_at_address_bytes(addr)` with no
point — reading live, and therefore immune to snapshot staleness. Verified:

```
$ git show d24c5608b6~1:crates/dugite-node/src/node/n2c_query/utxo.rs | grep -n utxos_at_address_bytes
45:            all_utxos.extend(provider.utxos_at_address_bytes(addr));
```

The 2026-08-08 archived round, which passed these categories, predates
`d24c5608b6` (2026-08-09).

**Do not revert #1068.** It fixed a real defect — one `MsgAcquire..MsgRelease`
session answering from two ledger points. Reverting restores that. The
correction belongs at `acquire`, so the pinned point is actually current.

## The wider implication

Every *other* LSQ query has been served from this same up-to-1-s (up-to-30-s
during catch-up) stale view all along. #1068 did not create that; it made one
query family honest about which point it was answering from. Gates that sample
at quiescent moments cannot see it — the same epoch-phase blindness as #977 and
#1071.

## Secondary, and genuinely the harness's

The `GovActionsDoNotExist` arm of the 07-* failures is a state-hygiene
cascade: the 07 scripts read the proposal id from
`tx-zoo/state/built/gov-action-info.id`, which 06a writes only on PASS. 06a
failed, so the file still held an id from the **2026-08-08** devnet. Entirely
downstream of the primary failure — with a fresh view, 06a would have passed and
rewritten it — but the file surviving across devnet incarnations is its own bug.

## Cheapest discriminating measurement, if re-confirmation is wanted

Against a running relay, tail `Chain extended` alongside a 5 Hz
`query utxo` of a funded address, and submit two chained spends ~1 s apart.
A multi-second gap between the block landing and the spent input disappearing
from the served view, ending exactly at the next `Chain extended`, confirms it.
`RUST_LOG=dugite_node::node::serve=debug` prints the direct signal:
`"UTxO query: undoing blocks applied after the acquired point" blocks=N`.

## Provenance

Mechanism and regression attribution came from an independent analysis on the
FABLE model, given the evidence and deliberately not the investigator's
hypotheses. Every load-bearing claim above was then re-verified here against
the logs, `git show`, and the tree.
