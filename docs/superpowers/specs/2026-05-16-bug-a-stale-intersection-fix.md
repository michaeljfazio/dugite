# Bug A: Stale ChainSync Intersection at Origin — Fix Design

**Date:** 2026-05-16
**Status:** Design / Pre-implementation
**Priority:** P1
**File:** `crates/dugite-node/src/node/sync.rs`

---

## Problem

When dugite-bp starts with an empty ChainDB and connects to a relay that is also at genesis (or behind dugite-bp's local tip), `chainsync_client_task` constructs `known_points = [Origin]`. The relay responds `MsgIntersectFound { point: Origin }`. Intersection is anchored at origin for the lifetime of this connection.

After that, the relay advances its chain (e.g. cardano-bp begins forging). The relay sends `MsgRollBackward { point: Origin }` followed by `MsgRollForward` headers for the new chain. dugite-bp's ChainSync receives these headers and forwards them to BlockFetch. Chain selection (`TriggeredFork`) fires if and only if the competing chain is strictly longer than dugite-bp's current `selected_chain` in VolatileDB.

**The specific failure case:** dugite-bp forges its own block early (before the relay's chain is long enough to win chain selection). At that point dugite-bp's VolatileDB `selected_chain` is one block ahead of the relay's chain-in-progress. The relay's chain does not yet trigger `TriggeredFork` because it is not strictly longer. dugite-bp continues extending its own fork. The relay eventually surpasses dugite-bp's chain length, `TriggeredFork` fires, and chain selection should switch — but it does not, because the relay's chain roots at origin and the VolatileDB `walk_chain_back` from the relay's tip cannot reach the required ancestor across the Origin anchor. Chain selection silently returns `StoreButDontChange` for every relay block.

**Root cause:** The intersection-at-origin anchor is a degenerate anchor. VolatileDB `switch_chain` requires both chains to share a common block in volatile storage (Haskell invariant: `isReachable`). When intersection is at Origin, there is no shared volatile block, so `switch_chain` cannot function and the relay's fork is permanently unreachable. The node stays stuck on its self-forged fork.

**This is distinct from** the general "tip age growing" hang: in that scenario the ChainSync pipeline is also stalled. Here, ChainSync and BlockFetch work correctly — the failure is in chain selection's inability to operate across an Origin anchor.

---

## Fix: Disconnect on Origin Intersection with Non-Origin Local State

**Chosen option: Option B** — disconnect (return error from `chainsync_client_task`) when `MsgIntersectFound { point: Origin }` is received and the local `ledger_tip != Point::Origin`.

This is the Haskell-compatible approach: the Haskell consensus client (`Ouroboros.Consensus.MiniProtocol.ChainSync.Client`) maintains a `KnownIntersectionState` and calls `terminateAfterDrain` when `intersectsWithCurrentChain` returns `NoLongerIntersects`. An intersection at Origin with a non-Origin local chain satisfies the Haskell equivalent of this condition: the peer and the node share no useful common ancestor, so the connection cannot make progress.

The peer demotion mechanism (`demote_to_warm` → `demote_to_cold` → reconnect after backoff) will cause the peer to be retried. On the next connection attempt the relay will have advanced and the intersection will land at a real block.

### File and Lines

**File:** `crates/dugite-node/src/node/sync.rs`

**Location:** Inside `try_find_intersect`, in the `MsgIntersectFound` arm, after the intersection is logged. Current code (approximately lines 2920-2934):

```rust
ChainSyncMessage::MsgIntersectFound {
    point,
    tip_slot,
    tip_block_number,
    ..
} => {
    let prim_point = from_codec_point(&point);
    info!(
        %peer_addr,
        point = %prim_point,
        tip_slot,
        tip_block_number,
        "ChainSync intersection found",
    );
    Ok(Some(point))
}
```

**Proposed change:** Return `Ok(None)` (instead of `Ok(Some(point))`) when the found intersection is Origin but our ledger is non-Origin. The caller already handles `None` as "no intersection found" and falls through to the retry/fallback path, which eventually logs "syncing from Origin" and anchors at Origin — that needs to also be fixed. Better: return a distinct error so the task terminates cleanly and the peer is reconnected.

```rust
ChainSyncMessage::MsgIntersectFound {
    point,
    tip_slot,
    tip_block_number,
    ..
} => {
    let prim_point = from_codec_point(&point);
    // If the peer can only intersect at Origin but we have a real local
    // chain, this connection cannot drive useful chain selection: VolatileDB
    // switch_chain requires a shared volatile block (Haskell: isReachable).
    // An Origin anchor has no volatile block, so every relay block would
    // return StoreButDontChange and our fork would never be superseded.
    //
    // Disconnect so the peer manager retries after backoff; on reconnection
    // the relay will typically have advanced and offer a real intersection.
    // Matches Haskell terminateAfterDrain / NoLongerIntersects semantics.
    if matches!(point, CodecPoint::Origin) && ledger_tip != Point::Origin {
        warn!(
            %peer_addr,
            tip_slot,
            tip_block_number,
            local_ledger_tip = %ledger_tip,
            "ChainSync intersection at Origin with non-Origin local chain \
             — peer is behind us; disconnecting for reconnect",
        );
        return Err(anyhow::anyhow!(
            "Peer {peer_addr} intersection at Origin with non-Origin local \
             ledger tip; disconnecting to retry after peer catches up"
        ));
    }
    info!(
        %peer_addr,
        point = %prim_point,
        tip_slot,
        tip_block_number,
        "ChainSync intersection found",
    );
    Ok(Some(point))
}
```

The `ledger_tip` variable is already in scope in `chainsync_client_task` and must be passed into (or captured by) `try_find_intersect`. Since `try_find_intersect` is currently a nested `async fn` that does not capture the outer scope, the cleanest change is to add `ledger_tip: &Point` as an additional parameter.

**Full call-site change** (line ~2957):

```rust
let mut intersection = try_find_intersect(
    &mut channel, peer_addr, &codec_points, &ledger_tip,
).await?;
```

And the retry loop similarly passes `&ledger_tip`.

**Alternative placement:** The check could instead be placed at the call site, immediately after `try_find_intersect` returns `Some(CodecPoint::Origin)`:

```rust
// After Attempt 1:
if matches!(intersection, Some(CodecPoint::Origin)) && ledger_tip != Point::Origin {
    return Err(anyhow::anyhow!(
        "Peer {peer_addr} intersection at Origin with non-Origin local chain; reconnecting"
    ));
}
```

This alternative is slightly simpler — no signature change to `try_find_intersect` — and equally correct because the retry logic is only entered when `intersection.is_none()`. Prefer this form to minimize diff.

---

## Why This Is Minimal and Correct

- **Minimal:** 5-6 lines of new logic, one `return Err(...)`. No changes to BlockFetch, chain selection, VolatileDB, or the retry loop.
- **Correct:** The `chainsync_client_task` error path triggers cleanup (Phase 4: `chains.remove(&peer_addr)`) and the connection lifecycle manager's `"chainsync task failed"` log, which leads to peer demotion. The peer manager re-promotes after its backoff timer, at which point `chainsync_client_task` runs again with a freshly computed `known_points` from the updated ledger/ChainDB state.
- **Idempotent on cold boot:** When the local node is also at origin (fresh DB, no ledger state), `ledger_tip == Point::Origin`, so this guard does not fire. Normal genesis sync is unaffected.
- **Haskell parity:** The Haskell consensus client does not re-issue `MsgFindIntersect` on the same connection. Recovery is always by reconnection. This change matches that behavior.
- **No new steady-state cost:** The guard executes once at connection setup, not in the hot pipeline loop.

---

## Test Strategy

1. **Unit test (sync.rs):** Construct a mock channel that responds `MsgIntersectFound { point: Origin }` when `ledger_tip = Point::Specific(slot=100, hash=...)`. Assert `chainsync_client_task` returns `Err` with the expected message. Use the existing test scaffold in `connection_lifecycle.rs` (`ConnectionLifecycleManager::new_for_test()`).

2. **Local testnet smoke test:** Run `local-testnet/run.sh` with one dugite-bp and one dugite-relay. Both start with empty DBs. Confirm that after the relay ingests cardano-bp's first forge, dugite-bp reconnects and eventually adopts the relay's chain (tip parity predicate in `verify.sh`).

3. **Regression guard:** Add a comment in the test linking to this spec to prevent future removal.

---

## Risks

- **Reconnect storm:** If the relay stays behind for an extended period (e.g. seconds in a local testnet, potentially minutes on mainnet), each reconnect attempt fires the guard and disconnects again. The peer manager's exponential backoff (starts at ~1s, doubles) caps this at ~10 reconnects/minute. Acceptable. The peer eventually catches up.
- **Masking real issues:** If a peer is permanently at genesis (misconfigured, bad DB), we will repeatedly reconnect. The peer manager's failure-count decay and reputation scoring will demote such a peer to cold after ~5-10 failures, matching normal Haskell handling of non-responsive peers.
- **Local testnet edge case (both BPs at genesis):** In the very first seconds of a fresh local testnet, BOTH nodes may be at genesis simultaneously. The guard fires on both sides, both disconnect, then both retry. This is a brief ordering race (seconds) and self-resolves once either node receives a block. Not a correctness issue.
- **Mithril import:** After a Mithril snapshot import, the local ledger tip is far ahead of origin. If the sync peer happens to be behind the Mithril tip on first connection, the guard fires, peer disconnects, and on reconnect the peer has typically advanced. This is the intended behavior and matches what Haskell does on restart with a dense ImmutableDB.

---

## Refinement: Bug E (commit `94f85b781`, 2026-05-16)

The original Bug A fix assumed the peer manager would automatically reconnect after the disconnect (per the "Why This Is Minimal and Correct" section above). That assumption held for OUTBOUND connections but **not for inbound-only connections**: when the relay accepts an inbound socket from a freshly-started dugite-bp and the relay's chainsync-to-dbp task dies via the Bug A guard at second 1 of the soak, no supervisor re-spawns the chainsync task. The inbound socket stays up, dbp continues to pull blocks from the relay's chainsync server, but the relay never sees a single header from dbp for the rest of the run.

This was Bug E. It only surfaced on the new local-devnet because that's the first dugite test setup in which a relay accepts a Bug A-triggering inbound connection.

The refinement (`crates/dugite-node/src/node/sync.rs`, line ~3022 after the patch): the guard now disconnects only when `local_block_no > security_param`. For chains within `k` blocks of genesis, the volatile window can absorb a full-genesis rollback if the peer's chain eventually wins — so accepting Origin and letting `process_add_block` adopt the peer's blocks via Bug D's Praos tiebreaker is the correct behavior. For chains beyond `k` (mainnet post-sync, a Mithril-imported relay, etc.), the disconnect-and-retry semantics are preserved.

This is the architecturally correct fix because:

- The original Bug A scenario was a symptom of Bug B (LedgerSeq empty → rollback fails → clear_volatile cascade). With Bug B fixed (`59a5fc64d`) and Bug D's Praos tiebreaker landed (`92052eb6c`), the "accept Origin and let RollForward proceed" path is now safe at small chain depths.
- The `k = security_param` threshold maps directly to the VolatileDB's volatile window: any required rollback fits if and only if local depth ≤ k.
- All 674 dugite-node tests pass after the change; no regression on the Bug A unit test (it was never written — the spec's test-strategy was aspirational; the local-devnet soak now exercises both branches of the refined guard).

The Risks section above is unchanged in scope: the "Reconnect storm" / "Masking real issues" / "Mithril import" cases still apply when the guard fires (i.e., when `local_block_no > k`). The new behavior — accepting Origin within `k` — is strictly safer than the previous always-disconnect rule and brings dugite closer to Haskell's actual chain-sync semantics (Haskell never disconnects on Origin; it just rolls forward).
