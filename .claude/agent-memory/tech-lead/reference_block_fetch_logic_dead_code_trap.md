---
name: block-fetch-logic-dead-code-trap
description: BlockFetchLogicTask (block_fetch_logic.rs) is spawned in production but functionally inert — the real BlockFetch path is the single-fetcher CAS-gated worker in connection_lifecycle.rs
metadata:
  type: reference
---

`crates/dugite-node/src/node/block_fetch_logic.rs` defines `BlockFetchLogicTask` — a decision
task that dispatches fetch ranges across multiple registered peers via `register_peer()` /
`fetch_senders`, and a standalone `blockfetch_worker()` free-for-all downloader. It IS
instantiated and spawned in `node/mod.rs` (`BlockFetchLogicTask::new_with_peer_manager(...)`,
tokio::spawn'd, ticks every 10ms/40ms).

**But it is a no-op in production**: `register_peer()` / `deregister_peer()` are never called
outside `block_fetch_logic.rs`'s own unit tests, and `blockfetch_worker()` is never called at all
outside tests. Since `fetch_senders` stays permanently empty, `evaluate_and_fetch()` returns
immediately on the `if self.fetch_senders.is_empty() { return; }` guard every tick, forever.

The REAL BlockFetch worker is `ConnectionLifecycleManager::make_blockfetch_task` in
`connection_lifecycle.rs`. It is single-fetcher: peer workers race a `compare_exchange` on a
shared `active_fetcher: Arc<AtomicU64>`, matching Haskell's `bfcMaxConcurrencyBulkSync = 1`. When
the slot is free, only the top `GSV_FETCH_TOP_K = 2` peers (ranked by measured EWMA fetch
bandwidth / "fetchyness" tracked in `PeerManager`) may claim it — a hot standby, not a fair race,
so a momentarily-busy best peer can't stall the slot. This confirms and explains the memory
[[project_sync_saturation_fixes_2026_06_17]] / [[project_fetch_path_gsv_2026_06_21]] finding that
concurrent multi-peer body fetch is a validated NEGATIVE — dugite's actual production
architecture never does concurrent multi-peer body fetch; the apparent multi-peer decision-task
code in `block_fetch_logic.rs` is vestigial (likely superseded during the single-fetcher rewrite
and never removed).

**Why this matters**: reading `block_fetch_logic.rs` in isolation gives a completely wrong mental
model of the fetch architecture. Always grep for `register_peer(` call sites before trusting that
file's doc comments — as of 2026-08-01 there are none outside its own tests.

Range sizing (the real path, `connection_lifecycle.rs`): adaptive byte budget
(`BLOCKFETCH_RANGE_BYTE_BUDGET = 8 MiB`) against a running average of recent block sizes, clamped
to `[BLOCKFETCH_MIN_RANGE=64, MAX_BLOCKS_PER_FETCH=2000]` blocks (operator override via
`DUGITE_BLOCKFETCH_MAX_RANGE`), with `BLOCKFETCH_PIPELINE_WINDOW = 2` in-flight `MsgRequestRange`
requests and a 10ms poll cadence per peer worker (`bfcDecisionLoopIntervalPraos` parity).

## RESOLVED (2026-08-01, v2.4.3)

`BlockFetchLogicTask` and its spawn site were **deleted** in #943 — the module
no longer exists, so the trap is gone rather than merely documented. Keep this
entry for the *shape* of the trap (a module that looks like the Haskell-parity
design, has passing tests, and is never wired to production), which recurred as
#941 and #942 in the same release. The live path remains
`ConnectionLifecycleManager::make_blockfetch_task`.
