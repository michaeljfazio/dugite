# Issue #760-A — Re-diagnosis: genesis-mode cold-restart sync wedge

Date: 2026-06-13. Based on live source reading of the working tree; all
file:line references verified against HEAD.

## Executive summary

The original A1 design (buffer beyond-horizon headers into the candidate
fragment before the forecast check) was **correctly rejected** by the oracle
correction. This document provides the definitive root-cause analysis,
alternative ranking, repro diagnostics, and a corrected fix design that
mirrors Haskell without buffering beyond-horizon headers.

**Primary root cause (confirmed by code): the #742 unproductive-dynamo
watchdog fires on the legitimately-parked dynamo during genesis bulk sync,
continuously rotating it and preventing the self-priming cycle from ever
completing.**

---

## A — Primary root cause

### The self-priming cycle (how it should work)

In Haskell, genesis bulk sync self-primes because `csCandidate` fills with
forecast-validated headers from the snapshot tip up to `snapshot_tip +
stability_window` (≈ 8 640 headers on mainnet at k=2160, f=0.05). BlockFetch
fetches those bodies, the ledger advances, the forecast horizon advances, the
next parked header unblocks, and so on. The LoE tip is the shared prefix of
all peers' `csCandidate`s, so as the dynamo's fragment fills the LoE tip
advances, selection advances, and the cycle is self-sustaining.

In dugite the analogous cycle is:
1. Dynamo streams headers. Each in-horizon header passes `forecast_park_or_disconnect`
   (`sync.rs:5247`), is pushed to `pending_headers` (`sync.rs:5372`), and
   appended to `peer_state` (`sync.rs:5396`).
2. BlockFetch sees `pending_headers` for the dynamo and fetches the bodies.
3. Applied blocks advance `ledger_tip_rx`, which wakes any parked
   `forecast_park_or_disconnect` loops.
4. The dynamo's `peer_state` fragment grows → LoE tip advances (Syncing
   branch of GSM actor, `gsm.rs:900-987`) → chain selection cap lifts →
   ledger tip advances further.

**The cycle CAN prime on a cold restart — the dynamo correctly intersects at
the snapshot tip (see section A.1) and begins streaming in-horizon headers.**
The wedge is upstream: the cycle IS priming, but it is being continuously
aborted by the watchdog before it accumulates enough headers to become
self-sustaining.

### The watchdog kill loop (the actual bug)

`connection_lifecycle.rs:2431-2484` — the "unproductive-claim" watchdog path:

```
if fetch_runs.is_empty() {           // <─── dynamo has nothing dispatchable for BlockFetch
    …
    if is_genesis_bulk_sync
        && now_ms.saturating_sub(since) >= watchdog_ms        // unproductive ≥ 3×grace (30s)
        && last_starvation_ms >= since.saturating_add(watchdog_ms)  // starvation concurrent
    {
        cs.rotate_dynamo(&addr);     // <─── KILL. The dynamo is rotated.
```

Definitions:
- `watchdog_ms = 3 * block_fetch_grace_period.as_millis()` = 3 × 10 000 ms = **30 s**
  (default `block_fetch_grace_period = 10.0s`, `config.rs:118`).
- `last_starvation_ms = if starv == 0 { now_ms } else { starv }`. When
  ChainSel queue is empty (starvation Ongoing, `starv == 0`), this equals
  `now_ms`, so condition 2 collapses to the same test as condition 1.
  Therefore the watchdog fires whenever: (a) in genesis bulk sync, (b) the
  dynamo has been claimed-but-unproductive for ≥ 30 s continuously, AND (c)
  ChainSel is starved (queue empty).

**What "unproductive" means here:** `fetch_runs.is_empty()` is true when the
dynamo's `pending_headers` is empty OR ALL headers in it already pass the
`has_block`/`fetched_hashes` filter. On cold restart the dynamo's
`pending_headers` IS empty — it has no headers yet because it is parked in
`forecast_park_or_disconnect` (`sync.rs:5247`) waiting for the first in-horizon
header. While it parks, its BlockFetch worker polls every 10 ms, claims the
active-fetcher slot, sees `fetch_runs.is_empty()`, and increments
`unproductive_since_ms`. After 30 s it rotates the dynamo.

**What happens on rotation:** `rotate_dynamo` (csj.rs:491-529) demotes the
dynamo to a fresh jumper (`Happy { fresh: true }`) and elects the next peer
via `backfill_dynamo` → `promote_to_dynamo` (`csj.rs:568-649`). The new dynamo
either exits `run_csj_jumper_loop` with `JumperExit::StreamReintersect` (if
its server cursor jumped) or `JumperExit::Stream`. Either path calls
`reintersect_promoted_peer` (`sync.rs:4348-4415`), which sends
`MsgFindIntersect` with the current chain tip (still the snapshot tip, since
no progress was made), gets `MsgIntersectFound`, anchors at the snapshot tip,
and begins streaming. The new dynamo immediately hits `forecast_park_or_disconnect`
for the same reason (all headers are beyond the horizon from the snapshot),
parks for 30 s, gets rotated again.

**Net result: each dynamo gets 30 s to prime, then rotates.** On mainnet with
40 peers and a 138-epoch gap (~11M slots), stability_window ≈ 8640 slots.
A full stability window of blocks at roughly 20 blk/s (rough mainnet throughput
on a fast sync) would take ~7 min. The 30 s window expires before enough
headers accumulate, the dynamo rotates, the new dynamo re-intersects at the
same frozen tip, and the cycle repeats. Each rotation produces at most a handful
of blocks: ~30 s × (blocks receivable before hitting the horizon) ≈ the
headers that arrive within the forecast window before parking. With a far-below
snapshot, the FIRST header is already beyond the horizon (see section A.1), so
the cycle yields exactly 0 blocks per 30 s rotation → ~1 blk/min observed.

### A.1 — Does the dynamo intersect at the snapshot tip?

`reintersect_promoted_peer` (`sync.rs:4348-4415`) and the initial
`build_known_points` path both use:
1. `chain_db.get_tip_info()` — the selected chain tip (snapshot tip on cold restart)
2. `db.get_chain_points(VOLATILE_POINTS_DEPTH)` — recent volatile points
3. `db.get_immutable_tip_point()` — immutable tip

The peer (a mainnet relay) is well ahead (~epoch 636 vs snapshot epoch 498),
but it still holds our chain history, so `MsgIntersectFound` at our snapshot
tip is expected and is confirmed by the "CSJ: promoted peer re-intersected at
frontier" log. The first header streamed is therefore the block immediately
AFTER the snapshot tip.

On mainnet mainnet the snapshot tip is epoch ~498 (slot ~126M), live tip is
epoch ~636 (slot ~162M), gap is ~36M slots. Stability window is
`3k/f = 3×2160/0.05 = 129 600 slots`. The first header from the snapshot tip
is at slot `snapshot_tip + 1`, which is far inside the forecast horizon
(`snapshot_tip + 1 <= snapshot_tip + 129 600`). **The first header is
in-horizon.** So the dynamo DOES start streaming in-horizon headers — the wedge
is NOT the initial beyond-horizon hit. It is the watchdog firing before enough
in-horizon headers accumulate to make BlockFetch productive.

### A.2 — Why does BlockFetch show `fetch_runs.is_empty()` even when the dynamo has in-horizon headers?

Two sub-cases:

**Sub-case A.2a (first seconds):** The dynamo has just re-intersected and its
`pending_headers` is empty. Its ChainSync task is in the pipeline loop sending
`MsgRequestNext`s and awaiting responses. The first responses arrive, pass
`forecast_park_or_disconnect`, and are pushed to `pending_headers`. This takes
a few ms per header. Meanwhile BlockFetch polls every 10 ms. The first few
polls may see an empty queue; after 50-100 ms headers accumulate.

**Sub-case A.2b (after first stability window worth of headers):** The dynamo
streams ≈ stability_window headers (in-horizon), then the NEXT header is at
`snapshot_tip + stability_window + 1`, which is beyond the horizon. It parks in
`forecast_park_or_disconnect` (`sync.rs:5247`), blocking further `pending_headers`
growth. At this point `pending_headers` has ≈ stability_window headers. If
BlockFetch is slow enough (or the contiguity guard at `connection_lifecycle.rs:2534`
rejects some ranges), `pending_headers` may still contain headers. But if
BlockFetch is FAST (it usually is; at 20 blk/s and 1 Hz ledger publish, a 10s
burst fetches ~200 blocks), `pending_headers` drains before the ledger tip
advances enough to unpark the dynamo. The dynamo parks with an empty
`pending_headers`, BlockFetch sees `fetch_runs.is_empty()` for 30 s, rotates.

In both sub-cases the rotate happens. Sub-case A.2b is the dominant steady-state
failure mode once the cycle is running (i.e. a partial rotation, not a total
freeze); the observed "~1 blk/min" is consistent with 30 s/rotation × N headers
fetched before draining per rotation.

### A.3 — The #735 contiguity guard interaction

`connection_lifecycle.rs:2534-2613` — after promotion via
`JumperExit::StreamReintersect`, the promoted dynamo re-intersects at the
frontier. Its `pending_headers` are fresh (cleared at reintersect, new headers
start from the frontier). The #735 guard checks `prev_hash` of the first
pending header; since the dynamo streams from the frontier, this should match
a stored block. However, on rotation under the watchdog, `pending_headers` from
the previous (rotated) dynamo may not be cleared. The CandidateChainState entry
persists in `candidate_chains` across dynamo rotations — it is only reset when
the peer disconnects (`chains.remove(&peer_addr)` on task exit, not on role
change). Therefore the NEW dynamo may inherit `pending_headers` from a
partially-completed previous cycle, and the #735 contiguity guard may accept or
reject those headers correctly; this is not the source of the wedge, but it
does mean the "unproductive" diagnosis is accurate: after a rotation, the new
dynamo briefly has stale headers, then fresh headers append. The key stall is
the 30 s park window.

---

## B — Ranked alternative causes

### B.1 — LoE pinned at immutable tip because jumpers' fragments are empty

**Status: confirmed as a CONTRIBUTING factor, not the primary cause.**

When the dynamo's `peer_state` fragment is empty or shallow (e.g., during the
first seconds after re-intersection), the LoE computation at `gsm.rs:900-987`:

```rust
let sp = crate::genesis_governor::shared_candidate_prefix(selection_tip, &frags);
```

`shared_candidate_prefix` (`genesis_governor.rs:113-154`) takes the LONGEST
COMMON PREFIX of ALL peers' fragments. Jumper fragments are advanced only when
they accept a jump (`replace_fragment` called from `run_csj_jumper_loop:4325`
on `JumpInfo` acceptance). On cold restart, the dynamo has just re-intersected
and has 0 headers in its fragment. The jumpers have their `next_jump` pre-loaded
from the dynamo's `jump_info` (`csj.rs:194-197`), but `jump_info` starts at
`None` for a freshly-elected dynamo. The dynamo's `jump_info` is only set by
`update_jump_info` (`csj.rs:377-391`), which is called at `sync.rs:5408` AFTER
the header passes `forecast_park_or_disconnect`. Since all headers park
immediately, `jump_info` is never set, no jumps are ever broadcast, and jumpers
stay `Happy { fresh: true }` forever.

In this state `shared_candidate_prefix` has:
- Dynamo fragment: anchor = snapshot_tip, entries = [] (empty)
- 39 jumper fragments: each has whatever `replace_fragment` set last — on fresh
  start, their anchor is the initial intersection (`set_anchor` at `sync.rs:4870`)
  = snapshot tip or Origin, entries = []

All fragments empty → `suffix_at_imm_tip` returns empty for all → `shared_candidate_prefix` returns empty prefix → LoE tip = volatile_window tail (= snapshot_tip) → LoE = selection_tip → no advancement beyond the current selection.

This means the LoE IS pinned at the snapshot tip, and chain selection will not
advance past it. This IS a circular dependency contributing to the freeze, but
it is secondary to the watchdog: even if we fixed the LoE pinning by other
means (e.g. having the dynamo push headers before the forecast check), the
watchdog would still rotate the dynamo every 30 s, preventing the fragment from
ever growing enough to move the LoE.

**Refute evidence for B.1 as the PRIMARY cause:** If LoE pinning were the
primary cause, we would see the dynamo accumulate headers in `pending_headers`
and `peer_state` up to stability_window, then stall on LoE not advancing. The
reported symptom is ~1 blk/min, which corresponds to periodic 30 s bursts, not
a clean plateau at stability_window blocks. The watchdog rotation period (30 s)
matches the observed rate.

### B.2 — First header beyond forecast horizon (original "far-ahead" hypothesis)

**Status: REFUTED for the cold-restart case.**

As shown in A.1, the first header is at snapshot_tip+1, which is within the
forecast window of `snapshot_tip + stability_window`. The dynamo does stream
in-horizon headers. The original design report's claim that "the first received
header is already beyond the forecast horizon" was incorrect for the cold-restart
case; it would only be true if the dynamo intersected at Origin (not the case
after `reintersect_promoted_peer`).

However, a secondary instance occurs after the dynamo has consumed all in-horizon
headers and hits slot `snapshot_tip + stability_window + 1`: the first BEYOND-HORIZON
header correctly parks. This is not the cause of ~1 blk/min; it is the natural
steady-state behavior that should self-resolve as BlockFetch fetches the
in-horizon batch and the ledger advances.

### B.3 — stale LedgerView (#742) — tip-watch not updated after snapshot restore

**Status: ALREADY FIXED (`forecast_park_or_disconnect` uses `ledger_tip_rx`,
not `view.last_applied_slot`).** The `tip_rx` watch channel is updated
separately on each block apply (`sync.rs:3733-3738`). Not a current regression.

### B.4 — GDD disconnects the dynamo as low-density

**Status: UNLIKELY but not fully excludable.**

During the freeze, the dynamo's candidate suffix beyond the LoE tip is empty
(LoE = snapshot tip, fragment empty). GDD Guard 1: `idling || lower_bound > 0 ||
has_block_after`. The dynamo is not idling (it is streaming), `lower_bound = 0`
(empty suffix), `has_block_after = max(suffix_head_slot, latest_slot) >= first_slot_after_window`.
`latest_slot` is set in `peer_state.on_roll_forward` BEFORE the fragment is
appended (`genesis_peer_state.rs:231`), but only for headers that pass
`forecast_park_or_disconnect`. If the dynamo parks before the first `on_roll_forward`
call (i.e. it parks before the first header), `latest_slot = None` and GDD
Gate 0 excludes the dynamo entirely — no disconnect. So GDD should not be
killing the dynamo during the freeze. Can still cause intermittent disconnects
once a few headers arrive but are below the genesis window.

### B.5 — `chainsel_starvation_ms` semantics incorrect under heavy apply load

**Status: UNLIKELY — edge semantics are correct per code review.**

`chainsel_dequeued` does a CAS from 0 (Ongoing) to EndedAt only once per
starvation period (`connection_lifecycle.rs:1180`). Long apply times do not
falsely look like starvation because `chainsel_queue_empty` sets Ongoing only
when the queue goes EMPTY (apply is blocked waiting). During normal bulk sync
the queue stays full (multiple blocks fetched per apply cycle), so starvation
stays EndedAt from the last epoch-boundary apply. The watchdog's condition
`last_starvation_ms >= since + watchdog_ms` is then false (old EndedAt is in
the past), so it does NOT fire — the watchdog only fires when the queue is
actively empty AND has been empty throughout the unproductive window. This is
exactly the cold-restart scenario.

---

## C — Live-repro diagnostics plan

### Existing metrics to watch (no code change)

On the Prometheus endpoint (port 12798) during cold genesis restart from epoch ~498:

1. **`peers_connected` and `sync_progress_percent`** — should show steady
   progress if priming works; flatline at ~1 blk/min indicates the wedge.
2. **Log grep for `rotating (#742)`** — both watchdog paths log at INFO:
   - `"BlockFetch: dynamo unproductive past watchdog"` (`connection_lifecycle.rs:2469`)
   - `"BlockFetch: dynamo declined far-ahead ranges past watchdog"` (`connection_lifecycle.rs:2601`)
   - `"BlockFetch: peer starved ChainSel past grace period — rotating CSJ dynamo"` (`connection_lifecycle.rs:2662`)

   If these appear at ~30 s intervals, the primary root cause is confirmed.

3. **Log grep for `"CSJ: promoted peer re-intersected at frontier"`** 
   (`sync.rs:4395`) — each re-intersection fires at INFO. On a healthy sync
   this should appear once per dynamo per many minutes; on the wedge it fires
   every ~30 s.

4. **Log grep for `"ChainSync: peer has been parked on forecast horizon"`**
   (`sync.rs:3763`) — WARN at 60 s intervals per parked peer. These will show
   which dynamo is parked and for how long.

### Temporary debug logs to add (targeted; one cold-restart repro disambiguates)

**File: `crates/dugite-node/src/node/connection_lifecycle.rs`**

At line 2449, just inside the `if fetch_runs.is_empty()` branch, add:
```rust
debug!(%addr,
    unproductive_secs = unproductive_since_ms.map(|s| now_ms.saturating_sub(s) / 1000).unwrap_or(0),
    watchdog_threshold_secs = 3 * block_fetch_grace_period.as_secs(),
    chainsel_starved = (chainsel_starvation_ms.load(…) == 0),
    "BlockFetch: dynamo unproductive — pending_headers empty, checking watchdog"
);
```

This fires every 10 ms (BlockFetch poll cadence) while the dynamo is
unproductive, so gate it on `unproductive_since_ms.is_some()` to reduce
volume. Watching the `unproductive_secs` field confirms it hits 30 before
rotating.

**File: `crates/dugite-node/src/node/sync.rs`**

At line 5247, just before the `forecast_park_or_disconnect` call, add:
```rust
debug!(%peer_addr, slot, pending_headers_before_park = pending_count,
    "ChainSync: header approaching forecast check");
```

This shows whether `pending_headers` is filling before the park. If it reads
0 every time, the BlockFetch cycle has no headers to work with.

**File: `crates/dugite-node/src/genesis_peer_state.rs`**

The `fragment_snapshot()` function is O(1) (imbl clone). In the GSM actor
loop, after computing `loe_tip` at `gsm.rs:937`, add a debug log:
```rust
debug!(loe_tip_slot = ?loe_tip, dynamo_frag_len = frags.first().map(|(_, f)| f.len()),
    "GSM: LoE computed");
```

This confirms whether the LoE tip is advancing between rotations.

**Disambiguation criteria from ONE cold-restart run:**
- If the `rotating (#742)` log appears at ~30 s intervals → Primary root cause
  confirmed.
- If `pending_headers_before_park` is > 0 most of the time but `fetch_runs`
  is still empty → #735 contiguity guard is blocking BlockFetch; secondary cause.
- If `dynamo_frag_len` stays at 0 throughout → jumpers never get jump info;
  LoE pins at snapshot_tip; secondary cause B.1 also active.
- If none of the above and the node IS making progress → the wedge is not
  reproducible at this epoch gap; try a larger gap or check for recent fix.

---

## D — Corrected Haskell-faithful fix design

### Rejected approach (oracle-confirmed)

A1 as described in `reports/issue-760-genesis-csj-wedge-design.md` — buffer
beyond-horizon headers into `pending_headers` and the candidate fragment
BEFORE `forecast_park_or_disconnect` — is **rejected**. Haskell does NOT put
beyond-horizon headers into `csCandidate`. The Haskell `checkTime` step BLOCKS
at the forecast horizon; headers before it fill `csCandidate`; headers at or
past it park. The candidate never holds beyond-horizon headers.

### Corrected approach: fix the watchdog exemption

**The problem is the watchdog's definition of "unproductive."** A dynamo that
is legitimately parked on the forecast horizon while BlockFetch drains its
in-horizon headers is NOT unproductive — it is working exactly as Haskell
intends. The watchdog should fire only when the dynamo has NO in-horizon headers
queued AND is parked.

**Fix A: Widen the watchdog exemption in `connection_lifecycle.rs`**

The `fetch_runs.is_empty()` check at `connection_lifecycle.rs:2431` correctly
detects nothing-dispatchable-from-this-peer, but the watchdog then fires after
just 30 s. In genesis bulk sync, the correct grace is 3k/f × slot_length =
129 600 × 1 s ≈ 36 h — effectively unlimited. The watchdog should be
**disabled for genesis bulk sync**.

Specifically, at line 2464:
```rust
// BEFORE (fires in genesis bulk sync):
if is_genesis_bulk_sync
    && now_ms.saturating_sub(since) >= watchdog_ms
    && last_starvation_ms >= since.saturating_add(watchdog_ms)
{
    cs.rotate_dynamo(&addr);

// AFTER (exempt in genesis bulk sync):
if !is_genesis_bulk_sync
    && now_ms.saturating_sub(since) >= watchdog_ms
    && last_starvation_ms >= since.saturating_add(watchdog_ms)
{
    cs.rotate_dynamo(&addr);
```

Haskell parity: In Haskell, `checkLastChainSelStarvation` (`BlockFetch/Decision/Genesis.hs`)
runs only in `GenesisFetchMode` (genesis AND not caught up), firing when
`lastStarvationTime >= peersOrderStart(p) + gracePeriod`. The
`peersOrderStart` is the timestamp when the peer was promoted to active; the
grace period is `bfcBlockFetchGracePeriod` (default 10 s). But Haskell's
starvation is measured from `ChainSel` being EMPTY, which during genesis bulk
sync only occurs between block-apply cycles — roughly once per block. A peer
that causes no blocks to be applied (truly stuck) WILL be rotated; a peer
whose blocks are being applied fast enough is NOT rotated.

In dugite's current code, `chainsel_starvation_ms = 0` (Ongoing) when the
apply queue is empty. This is correct. But the `unproductive-claim` watchdog
at line 2431-2484 fires based on `fetch_runs.is_empty()`, which is TRUE when
the dynamo is legitimately parked (no headers yet). This second watchdog path
is NOT in Haskell — it was added as a conservative safeguard for CSJ dynamos
that are silent without the LoP waking them. But in genesis bulk sync it fires
on the wrong condition. The fix is to gate this second watchdog on
`!is_genesis_bulk_sync` (like the Haskell path, which relies on starvation
NOT on "no ranges to dispatch").

**Fix B: Also widen the "far-ahead" watchdog at line 2592**

The second instance of the watchdog (`connection_lifecycle.rs:2570-2613`) fires
when BlockFetch has pending headers but they are ALL declined by the #735
contiguity guard. This IS a real failure mode (post-jump far-ahead headers
on a just-promoted dynamo). However, the same 30 s threshold is too short for
genesis bulk sync; it should also be gated on `!is_genesis_bulk_sync`.

```rust
// Line 2592:
if !is_genesis_bulk_sync           // <─── add this gate
    && is_genesis_bulk_sync        // <─── REMOVE the existing is_genesis_bulk_sync check
```

Wait — actually the existing code at 2592 ALREADY requires `is_genesis_bulk_sync`.
The "far-ahead" watchdog is intentionally genesis-only, because in Praos mode
a peer with far-ahead headers would simply be disconnected by the forecast
timeout (no CSJ). This watchdog is correct for the post-jump case where the
promoted dynamo serves headers from its jump point. However, after
`reintersect_promoted_peer`, the dynamo re-anchors at the frontier; headers
that arrive after reintersect are in-horizon (from the frontier). If the
watchdog fires here it means `reintersect_promoted_peer` failed to bring the
dynamo back far enough. This is a secondary concern; Fix A above addresses the
primary wedge.

**Fix C: Broadcast an initial jump immediately after the first in-horizon batch**

This is the dual fix to ensure the jumpers' fragments advance once the dynamo
gets its first batch of headers. Currently, `on_roll_forward` (`csj.rs:289-320`)
only broadcasts when `slot > last_jump_slot + jump_size`. For a freshly-elected
dynamo, `last_jump_slot = candidate_anchor_slot` (the snapshot tip, set at
`csj.rs:186`). `jump_size = 4320` (mainnet default, `csj.rs:138`). So the
first broadcast happens when the dynamo's slot exceeds `snapshot_tip + 4320`.
At 20 blk/s (rough header throughput on good peers) this takes ~216 s = 3.6 min.
That is within the first rotation window IF Fix A removes the 30 s limit.
With Fix A alone, the cycle should self-prime within the first stability_window
pass. Fix C would reduce the first-jump latency from 4320 headers to a smaller
configurable threshold, but it is not required for correctness.

### Exact functions to change

**Primary (must change):**

`crates/dugite-node/src/node/connection_lifecycle.rs`

- Line 2464: Change the `if is_genesis_bulk_sync` condition of the
  "unproductive-claim" watchdog to `if !is_genesis_bulk_sync`. This disables
  the false-trigger that rotates a legitimately-parked genesis dynamo.
- The ChainSel-starvation rotation at line 2661 is CORRECT and should remain:
  it only fires when `last_starvation_ms >= claim_ms + grace_ms`, which in
  genesis bulk sync means ChainSel has been empty (no blocks applied) for the
  entire time the peer held the active-fetcher slot. A truly silent dynamo
  (no blocks flowing at all) will still be rotated via this path. This
  preserves Haskell's rotation-on-starvation semantics.

**Secondary (consider, not strictly required):**

`crates/dugite-node/src/node/connection_lifecycle.rs`

- Line 2592: The "far-ahead-ranges" watchdog already gates on
  `is_genesis_bulk_sync`, which is correct — this path is only reached in
  genesis mode and only when pending_headers exist but are all declined by the
  contiguity guard (the post-jump far-ahead scenario). Its 30 s threshold could
  be increased if live testing shows it fires too eagerly, but it is not the
  primary wedge for the cold-restart scenario.

### Why this is praos-byte-identical

Praos mode: `is_genesis_bulk_sync` is false (GSM is `CaughtUp` in Praos mode,
or CSJ is disabled entirely). The changed condition `!is_genesis_bulk_sync`
evaluates to `true` in Praos, preserving the existing behavior for non-genesis
mode exactly. The condition flip does not touch any data path; it changes only
the watchdog trigger guard. Zero risk of Praos regression.

### Why this does NOT buffer beyond-horizon headers

This fix changes only the watchdog gate condition in the BlockFetch worker. It
does not touch `forecast_park_or_disconnect`, `pending_headers`, or the
candidate fragment at all. The ChainSync task continues to block at the forecast
horizon exactly as Haskell does. The only change is that the BlockFetch worker
no longer rotates the dynamo for being parked, allowing the in-horizon headers
batch to build up, be fetched, advance the ledger, unpark the dynamo, and prime
the cycle.

---

## Summary table

| Question | Answer | File:line |
|---|---|---|
| Does dynamo intersect at snapshot tip? | Yes, via `reintersect_promoted_peer` | `sync.rs:4348-4415` |
| First headers in-horizon? | Yes (snapshot_tip+1 < snapshot_tip+stability_window) | `forecast_park_or_disconnect` |
| Does `peer_state` fill before rotation? | Partially — 0 to stability_window headers, then drains and parks | `sync.rs:5396` |
| LoE pinned? | Yes, secondary — jumpers never receive jump because dynamo's `jump_info = None` | `csj.rs:194-197`, `gsm.rs:923` |
| Watchdog fires during park? | **YES — primary root cause** — 30 s × `is_genesis_bulk_sync` condition fires on legitimately-parked dynamo | `connection_lifecycle.rs:2464` |
| GDD kills dynamo? | No — Gate 0 excludes dynamo with `latest_slot = None` | `genesis_governor.rs:224` |
| GDD reads csLatestSlot? | Yes — `st.as_ref().and_then(|s| s.latest_slot())` | `gsm.rs:1005` |
| Fix is praos-byte-identical? | Yes — `!is_genesis_bulk_sync` guard preserves praos path | `connection_lifecycle.rs:2464` |
