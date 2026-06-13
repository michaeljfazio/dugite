# Adversarial Review: 369f0d4ca3 — OutsideForecast Deferral in `classify_eager_validation_result`

**Branch:** fix/760-genesis-csj-wedge  
**Date:** 2026-06-13  
**Reviewer:** Tech Lead (adversarial mode)  
**Verdict: SHIP** — no safety hole found; one minor test gap noted.

---

## 1. Summary of Change

`classify_eager_validation_result` (sync.rs:3635) now maps
`ConsensusError::OutsideForecast` to `Ok(false)` (deliberate skip / defer to
body apply) instead of `Err` (disconnect peer).

The motivation: `eager_validate_header` uses `ledger_view.last_applied_slot`
(the lock-free, throttled ArcSwap) as the `ledger_tip_slot` argument to
`validate_header_full_with_counters`. `publish_ledger_view` is gated (#698)
and freezes entirely when the apply loop stalls. `forecast_park_or_disconnect`
uses the FRESH `tip_rx` watch channel (updated on every apply, even under the
throttle gate). Both run in the same `MsgRollForward` arm, but a view that
froze ~551k slots behind the live applied tip caused `OutsideForecast` inside
eager even for headers that were within the fresh forecast window — producing a
false negative that disconnected every peer and permanently wedged the node at
the Babbage→Conway era boundary.

---

## 2. Attack Surface Analysis

### Finding 1 — SAFETY: Can a genuinely-too-far-future header bypass the deferral and get admitted?

**Verdict: REFUTED.**

The call sequence in the `MsgRollForward` arm (sync.rs ~5252–5325) is strictly
ordered:

1. **`forecast_park_or_disconnect`** (line 5260) — runs FIRST, for all
   non-Byron headers. Uses `tip_rx.borrow()` (the FRESH per-block watch) as
   the forecast anchor. If `slot >= fresh_tip + 1 + stability_window` the
   function parks indefinitely during genesis bulk sync, or returns `Err` after
   60s otherwise. The `park_result?` on line 5270 propagates the error
   immediately, returning from the entire task and disconnecting the peer.
   Control flow cannot reach eager validation until this gate passes.

2. **`eager_validate_header`** (line 5288) — only reachable if
   `forecast_park_or_disconnect` returned `Ok(())`. It reads the stale
   `view_arc.last_applied_slot` which, if frozen, causes `OutsideForecast`
   again. With the fix, this becomes `Ok(false)` — header goes to
   `pending_headers`, body is fetched, and body-apply re-validates with the
   live ledger.

There is no code path from `MsgRollForward` to `eager_validate_header` that
bypasses `forecast_park_or_disconnect`. The function is only called in one
place (`MsgRollBackward` trims `pending_headers` and re-fills the pipeline via
the same `MsgRollForward` arm; after a rollback, new headers come through the
same gate).

The only headers for which `forecast_park` is skipped are Byron-wrapped headers
(`is_byron_wrapped_header`, line 5252). Byron (PBFT) has no Praos forecast
semantics, is completed in the first ~30 hours of mainnet sync, and is also
skipped by `eager_validate_header` at the era gate (line 3499). No change.

**Conclusion:** A genuinely too-far-future header that bypasses `forecast_park`
is not possible through this code path. The deferral only fires for headers that
the FRESH tip watch has already admitted.

---

### Finding 2 — SAFETY: Does body-apply re-validate the forecast authoritatively?

**Verdict: CONFIRMED — body apply is a complete authoritative re-check.**

`apply_fetched_block` (mod.rs:5856) calls `validate_peer_header_full`
(mod.rs:5369) only AFTER the `connects_to_tip` gate (line 6134-6148). At that
point the block's `prev_hash` equals the live ledger tip hash, so the block's
slot is exactly one chain step ahead of the live tip. `validate_peer_header_full`
at line 5480 passes `tip_slot = ls.tip.point.slot()` — the LIVE ledger tip —
to `validate_header_full`. The forecast check inside (praos.rs:659–662) computes
`max_for = tip + 1 + stability_window`. Since the block connects to the tip, its
slot is at most `max_for - stability_window` slots ahead — trivially within
range. An `OutsideForecast` from body-apply would indicate a pathological state
(ledger tip somehow ahead of the block being applied) which is structurally
prevented by the `connects_to_tip` guard.

Additionally, body-apply uses `ValidationMode::Full` (not Replay), performing
VRF proof, nonce VRF, opcert Ed25519, and KES signature verification. No safety
is lost by deferral.

---

### Finding 3 — RESOURCE: Bandwidth/disk amplification via `pending_headers`?

**Verdict: PLAUSIBLE but bounded and not worse than status quo.**

With the fix, a peer that streams headers the stale view cannot forecast gets
them recorded to `pending_headers` and body-fetched. The bound is
`PENDING_HEADERS_PAUSE = 10_000` headers (sync.rs:4148), enforced by
`should_refill_pipeline` (line 4185). At 8 KB maximum per header (the
`MAX_HEADER_CBOR_BYTES` cap, line 4172), that is 80 MB peak pending-header
memory — unchanged from before the fix. Bodies fetched by BlockFetch that fail
body-apply are rejected at the `validate_peer_header_full` step (mod.rs:6166)
and the apply loop does not advance the ledger tip, so the peer does not
progress.

**Pre-fix behavior was strictly worse:** the false-disconnect churned ALL 40
peers simultaneously and the node made zero progress. The fix at worst causes
some body fetches that body-apply rejects (a small amplification); the pre-fix
behavior was a permanent wedge.

A malicious peer CANNOT exploit this to cause unbounded fetch: the
`PENDING_HEADERS_PAUSE` cap stops pipeline refill. The peer can fill at most
10,000 headers × average body size. This is the same amplification budget that
existed before the fix.

---

### Finding 4 — SAFETY: Is the genuine-adversarial-forecast defense fully preserved?

**Verdict: CONFIRMED — `forecast_park` (fresh tip) is the real enforcement line.**

The pre-fix code provided TWO layers:
- `forecast_park_or_disconnect` (fresh tip, 60s timeout) — the primary gate
- `classify_eager_validation_result` mapping `OutsideForecast` → `Err` (disconnect) — now removed

The removed layer was redundant for genuinely too-far-future headers because
`forecast_park` already catches them with a fresh tip, parks, and disconnects
at 60s. The only case the removed layer could fire that `forecast_park` would
NOT catch is the stale-view false negative — which is exactly the bug being
fixed.

The FORECAST_PARK_TIMEOUT (60s, line 3387) remains intact. A peer sending
headers beyond the FRESH forecast window for 60s (non-genesis) or indefinitely
(genesis bulk sync) still disconnects via `forecast_park`. The fix does not
change `forecast_park`'s behavior at all.

---

### Finding 5 — SOUNDNESS: Does a stale view produce wrong non-OutsideForecast results?

**Verdict: CONFIRMED MINOR — the stale view causes incorrect stale-nonce VRF, but Replay mode does not execute VRF crypto.**

The stale `LedgerView` affects several fields passed to eager validation:
- `last_applied_slot` — forecast horizon (the fixed issue)
- `epoch_nonce` (from `LedgerView`) — NOT passed to eager; `eager_validate_header` does not inject the epoch nonce into the decoded header

In `eager_validate_header`, `decode_wire_wrapped_block_header` sets
`epoch_nonce: Hash32::ZERO` (era_conway.rs:374). This ZERO nonce is passed to
`validate_header_full_with_counters`, which uses `ValidationMode::Replay`
(sync.rs:3595). In Replay mode, the VRF proof verification and KES signature
are SKIPPED entirely (praos.rs:541–547). The zero nonce is therefore never
used in a crypto check.

Checks that DO run in Replay mode against the stale view:
- **Structural field sizes** — pure header intrinsic, unaffected by staleness.
- **Forecast horizon** — the stale `last_applied_slot` produces the false
  OutsideForecast; the fix maps this to `Ok(false)`.
- **FutureBlock** — `current_slot = header.slot` is passed (line 3561), so
  `header.slot > current_slot` is always false. Unchanged; body-apply uses the
  real wall-clock slot.
- **ObsoleteNode** (PV check) — uses `view.protocol_params.protocol_version_major`.
  If the view is stale, an in-range PV might be checked against an old value.
  However: (a) PV only increases monotonically, so a stale PV can only be
  LOWER than the true value — a lower PV makes the `pv > max_major_prot_ver`
  check more lenient, never more strict; (b) body-apply re-checks with the
  live ledger PV. No false accept of an ObsoleteNode header.
- **Opcert counter** — uses `peer_counters` (per-peer, seeded lazily from the
  global snapshot). Already handled with separate `OpcertCounterOverIncremented
  → Ok(false)` deferral.
- **KES period** — uses `consensus_seed.slots_per_kes_period` (a fixed genesis
  parameter, not from the view). Unaffected by staleness.
- **Unregistered pool** — uses `set_snap.pool_stake.is_empty()` guard (line 3529);
  an incomplete view returns `Ok(false)` (early skip), not a false accept.

**No stale-view condition causes a false-accept (Ok(true)) for a genuinely
invalid header.** The stale view can only produce Ok(false) (skip) or false
negatives (incorrect Err). The only false-negative remaining after the fix is
ObsoleteNode under a permanently-stale view at an era transition — and that is
also addressed by body-apply.

---

### Finding 6 — TEST COVERAGE: Is the unit test adequate?

**Verdict: ADEQUATE with one missing test case.**

The new test at sync.rs:6108–6118 verifies that `classify_eager_validation_result`
maps the specific `OutsideForecastRange` values from the real incident to `Ok(false)`.
It also checks `Ok(true)`, `OpcertCounterOverIncremented → Ok(false)`,
`OpcertSequenceRegression → Err`, and `InvalidBlock → Err` in the same function.

**Missing test:** There is no test covering the INTERACTION of the two gatekeepers.
Specifically: a test that demonstrates a header that passes `forecast_park` (because
`tip_rx` is current) but would have been false-disconnected by the old
`classify_eager_validation_result` (because `view.last_applied_slot` is stale).
This is the exact scenario the fix corrects. The scenario can be constructed with:

- `tip_rx` sender updated to slot X (fresh)
- `view_arc.last_applied_slot` = X - 600,000 (frozen / stale)
- header at slot X - 55 (within FRESH window, outside STALE window)

Without such a test, future refactors could accidentally re-couple the two
checks (e.g., by having `forecast_park` use the stale view again) without
failing the unit test. This is a test-quality issue, NOT a functional bug.
Severity: LOW — the existing regression event covers this in integration.

---

## 3. Haskell Model Comparison

The Haskell reference (`Ouroboros.Consensus.MiniProtocol.ChainSync.Client`) does not
treat an `OutsideForecastRange` from the ChainSync header-receive path as a peer
fault. In the Haskell model, the forecast check at the ChainSync layer is
advisory: the client parks (`BlockedOnForecast`) and waits for the ledger
to advance, then retries. An `OutsideForecastRange` is a property of the
NODE'S LOCAL LEDGER STATE, not of the peer. Disconnecting on it is
architecturally incorrect and has never been Haskell's behavior. The fix
brings dugite's eager validation into alignment with this model. The
authoritative check at apply time (`updateChainDepState`) is preserved.

---

## 4. Summary Table

| Attack / Question | Verdict | Confidence |
|---|---|---|
| 1. Too-far-future header admitted after deferral | REFUTED | High — `forecast_park` using fresh `tip_rx` is the binding gate; `park_result?` propagates before eager runs |
| 2. Body-apply re-validates forecast authoritatively | CONFIRMED | High — `connects_to_tip` + `validate_peer_header_full` with live `ls.tip.point.slot()` |
| 3. Resource amplification via deferred body-fetches | PLAUSIBLE but bounded | Medium — capped at `PENDING_HEADERS_PAUSE × MAX_BODY_SIZE`; strictly better than the pre-fix permanent wedge |
| 4. Genuine-adversarial forecast defense preserved | CONFIRMED | High — `forecast_park` (fresh tip, 60s timeout) is unchanged and is the real enforcement line |
| 5. Stale view causes wrong non-OutsideForecast result (false-accept) | REFUTED | High — Replay mode skips all crypto; stale PV is only more lenient on ObsoleteNode; no path to false `Ok(true)` |
| 6. Unit test adequacy | MINOR GAP | Low severity — missing an integration-level test for the two-gatekeeper interaction |

---

## 5. Recommended Follow-up (Non-blocking)

Add a unit test for the stale-view / fresh-tip divergence scenario:
- `tip_rx` at slot N (current)
- `view_arc.last_applied_slot` at slot N − 600_000 (stale, simulating the incident)
- header at slot N − 55 (within fresh window, outside stale window)

Expected: `forecast_park_or_disconnect` returns `Ok(())`; `eager_validate_header`
returns `Ok(false)` (OutsideForecast from stale view → deferred); header enters
`pending_headers` normally. This pins the two-layer guarantee so that any future
change that recouples the checks fails loudly.

This can be filed as a follow-up and does not block shipping 369f0d4ca3.
