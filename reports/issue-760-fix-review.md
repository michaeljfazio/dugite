# Adversarial Review — Issue #760-A Fix (commit 6212c2895b)

**Date:** 2026-06-13  
**Reviewer:** Tech-Lead agent (adversarial mode)  
**Branch:** fix/760-genesis-csj-wedge  
**Verdict:** SHIP (with one observability note — not a correctness blocker)

---

## Executive Summary

The fix is structurally sound. None of the six attack surfaces yield a confirmed
correctness defect that would re-open #742 or introduce a new permanent wedge
under realistic operating conditions. Two findings merit attention: one plausible
scenario that is bounded and self-correcting (not a blocker), and one unpatched
sibling watchdog path that is safe by analysis (the parked-dynamo scenario cannot
reach it). A missing observability log is the only actionable item.

---

## Finding 1 — CONFIRMED-REAL (LOW SEVERITY, SELF-CORRECTING): Legitimately-dead dynamo with stale far-ahead fragment can defer rotation by up to one block-drain cycle

**Risk category:** Attack #1 — genuinely-stuck dynamo escaping rotation  
**Severity:** Low — bounded, self-correcting; does not produce a permanent wedge

**Scenario:**

A dynamo streams headers from intersection slot X to forecast horizon X+W (where
W = 3k/f — ~25,920 on preview, ~129,600 on mainnet). Immediately after delivering
the last header it goes half-open (TCP alive, application dead). At this point:

- `fragment_head_slot` = X+W (last delivered header, recorded in `jump_info`)
- `pending_headers` contains all W slots worth of headers
- BlockFetch begins draining them, applying blocks, advancing `chain_db.get_tip()`
- While `chain_tip_slot < (X+W) - 2000`, the watchdog sees gap > 2000 → `rotate=false` → dynamo is kept

During the drain, the node is NOT wedged — BlockFetch is actively fetching from
the dynamo's headers and applying blocks. The "protection" merely keeps the dead
dynamo in its slot. When `pending_headers` finally empties:

- `fetch_runs.is_empty()` = true → watchdog fires
- `chain_tip_slot` ≈ X+W (all blocks applied) → gap ≈ 0 → `rotate=true` → rotation fires

**Therefore:** the only consequence is that rotation is deferred until the
dynamo's headers are fully consumed. During this window the node is making forward
progress (applying blocks). Once the headers are exhausted, the gap collapses and
rotation fires correctly on the next 30s watchdog cycle.

**One sub-case that is still bounded but slower:** If LoE is pinned (a lagging
jumper's `csCandidate` is empty), `chain_tip_slot` via `chain_db.get_tip()` may
not advance to X+W even after BlockFetch stores all the blocks, because chain
selection is LoE-gated. In this case rotation is deferred until either:
(a) the lagging jumper catches up (unlocking LoE → chain selection → `chain_db`
tip advances → gap collapses), or (b) the `#735` gross-request watchdog at line
2658 fires for a subsequent attempt by the dead peer.

In the LoE-pinned sub-case, rotating the dead dynamo would not help either —
the new dynamo would also park at the same LoE-constrained horizon. The real fix
is advancing the lagging jumper, which the existing LoP/jump machinery handles.

**Verdict:** Plausible deferral, not a re-opening of #742 (which was a permanent
wedge). The fix does not introduce infinite protection for a dead dynamo; it only
delays the inevitable rotation by at most one block-drain cycle.

---

## Finding 2 — REFUTED: Unpatched sibling watchdog at line 2658 could fire on parked dynamo

**Risk category:** Second watchdog path (`#735` gross-request invariant)  
**File:line:** `connection_lifecycle.rs:2658`

**Analysis:**

The `#735` path is entered when `fetch_runs.is_empty() == false` (headers exist)
but the FIRST header's `prev_hash` does not connect to any stored block. This
fires for a CSJ-promoted jumper whose far-ahead headers land in `pending_headers`
before the chain catches up.

A legitimately parked dynamo's `pending_headers` starts from the intersection
point (contiguous from the stored chain) because the dynamo begins streaming from
the negotiated intersection, not from a jump target. These headers DO connect to
stored blocks, so `connects = true` and the `!connects` branch at line 2619 is
never entered. The `#735` watchdog cannot fire on a parked dynamo that fed
contiguous in-horizon headers.

The unpatched path at 2658 is therefore safe for the #760-A scenario.

**Verdict:** Refuted as a defect for the #760-A case.

---

## Finding 3 — REFUTED: `jump_info` staleness / race producing wrong fragment_head_slot

**Risk category:** Attack #3 — `jump_info` source correctness

**Analysis:**

`update_jump_info` (csj.rs:401) is called exclusively on `MsgRollForward`
(sync.rs:5408), which calls `peer_state.fragment_snapshot()` AFTER
`on_roll_forward` appends the new entry. The resulting `CandidateFragment`
always has at least one entry before `update_jump_info` writes to
`peer.jump_info`. A newly registered dynamo with no headers yet has
`jump_info = None` (csj.rs:230), so `fragment_head_slot` correctly returns
`None` → `should_rotate = true`.

Both `update_jump_info` and `fragment_head_slot` take the same
`std::sync::Mutex<Inner>`. There is no race — they serialize.

A disengaged peer has `jump_info = None` (cleared by `disengage_peer` at
csj.rs:580), so it also returns `None` → `should_rotate = true`.

A rotated-to-Jumper peer retains its last `jump_info` (not cleared by
`rotate_dynamo`), but the watchdog only fires when `cs.rotate_dynamo(&addr)`
would succeed — which requires `addr` to still be the dynamo. After rotation,
`rotate_dynamo(&addr)` returns false, so the retained stale `jump_info` of the
former-dynamo-now-jumper does not cause any incorrect behavior.

**Verdict:** Refuted.

---

## Finding 4 — REFUTED: Lock-ordering hazard / hot-path contention

**Risk category:** Attack #4 — concurrency

**`chain_db.read()` usage:**

The new `chain_db.read().await` at line 2521 is inside the
`if fetch_runs.is_empty()` → `if is_genesis_bulk_sync && watchdog fires`
triple-gated branch. It fires at most once per watchdog window (every ~30s per
peer), never on the hot 10ms tick path. The comment at lines 2517-2519 is
accurate.

**Lock ordering:**

The `chain_db` RwLock (tokio) and the `csj` Mutex (`std::sync`) are never held
simultaneously. The chain_db read lock is taken in its own `let` block (lines
2520-2523), dropped before `cs.fragment_head_slot(&addr)` takes the csj Mutex.
No deadlock risk.

**`unproductive_since_ms` reset:**

Line 2538 resets `unproductive_since_ms = None` unconditionally inside the
`if watchdog fires` block, regardless of whether `rotate` was true or false.
This is intentional — the 30s watchdog window restarts fresh after each
evaluation. On the next `fetch_runs.is_empty()` tick, `unproductive_since_ms`
is set again (line 2487) and the new 30s clock begins. This means a parked
dynamo is re-evaluated every ~30s until the gap closes. Correct.

**Verdict:** Refuted.

---

## Finding 5 — CONFIRMED: Praos byte-identical claim is correct

**Risk category:** Attack #5 — Praos reachability

The new code path is doubly gated:

1. `if let (Some(ref cs), ...) = (&csj, ...)` — `csj` is `None` when genesis is
   disabled (mod.rs:2245: `if genesis_enabled && genesis_params.options.enable_csj`).
   In Praos mode, `csj = None`, so the entire inner block is unreachable.

2. `if is_genesis_bulk_sync` — when genesis mode is disabled, the GSM
   immediately enters `CaughtUp` (gsm.rs line 303: "If not enabled, it
   immediately enters CaughtUp"). `is_genesis_bulk_sync = state != CaughtUp`
   would be false even if the first gate were somehow bypassed.

Neither `fragment_head_slot` nor the new `chain_db.read()` is called in Praos
mode. Praos byte-identical claim holds.

**Verdict:** Confirmed correct.

---

## Finding 6 — REFUTED: Non-dynamo peer addr making discriminator meaningless

**Risk category:** Attack #6 — addr not dynamo

`cs.rotate_dynamo(&addr)` is a no-op (returns false) when `addr` is not the
current dynamo (csj.rs:521). The discriminator check at line 2524 reads
`cs.fragment_head_slot(&addr)` which returns whatever that peer's `jump_info`
holds, or `None`. If `rotate=true` but `rotate_dynamo` is a no-op (because
`addr` is not the dynamo), nothing happens. If `rotate=false` for a non-dynamo,
nothing happens either. The discriminator only has operational effect when `addr`
IS the dynamo; for all other peers both branches result in the same no-op outcome.

**Verdict:** Refuted.

---

## Finding 7 — OBSERVABILITY GAP (NOT A DEFECT): Silent skip has no log

When `rotate=false` (parked dynamo kept), neither a log line nor a metric is
emitted. An operator watching the logs during a stuck sync cannot distinguish
"watchdog fired and correctly held the parked dynamo" from "watchdog never
fired." This makes diagnosing future regressions harder.

**Suggested correction (optional, not blocking):**

```rust
if rotate {
    if cs.rotate_dynamo(&addr) {
        info!( ... "rotating (#742/#760-A)");
    }
} else {
    debug!(
        %addr,
        fragment_head_slot,
        chain_tip_slot,
        "BlockFetch: unproductive-dynamo watchdog: dynamo KEPT \
         (parked on forecast horizon, fragment {}s ahead — #760-A)",
        fragment_head_slot.unwrap_or(0).saturating_sub(chain_tip_slot),
    );
}
```

A `debug!` is sufficient — this fires at most once per 30s per dynamo rotation
epoch, so it won't flood logs.

---

## Finding 8 — REFUTED: Compile-time assert covers all production networks

**Risk category:** Attack #2 — margin safety

`GENESIS_PARKED_DYNAMO_MARGIN_SLOTS = 2_000 < 25_920` (compile-time assert).

Network verification:
- Preview: k=432, f=0.05 → 3k/f = 25,920. Margin/window ratio = 2000/25920 ≈ 7.7%
- Preprod/Mainnet: k=2160, f=0.05 → 3k/f = 129,600. Ratio ≈ 1.5%
- Local devnet: k=40, f=0.5 → 3k/f = 240 slots. Ratio = 833%. BUT: devnet requires
  `ConsensusMode = genesis` in config, which is not set in the devnet configs.
  With `ConsensusMode` defaulting to `PraosMode`, CSJ is disabled
  (`genesis_enabled = false`), so the watchdog never fires on devnet. The compile-
  time assert against 25,920 (the smallest _production_ network) is correct.

**Verdict:** Refuted as a risk. The assert is correctly scoped to production
networks. The devnet edge case is unreachable via default config.

---

## Summary Table

| Finding | Type | Severity | Verdict |
|---------|------|----------|---------|
| F1: Dead dynamo defers rotation until headers drained | Plausible | Low | Self-correcting; not a permanent wedge |
| F2: Unpatched #735 watchdog fires on parked dynamo | Plausible | — | Refuted: headers are contiguous for parked dynamo |
| F3: `jump_info` staleness / race | Plausible | — | Refuted: Mutex serializes; init guarantees |
| F4: Lock ordering / hot-path contention | Plausible | — | Refuted: sequential, at most once per 30s |
| F5: Praos reachability | Check | — | Confirmed byte-identical (doubly gated) |
| F6: addr not dynamo — meaningless discriminator | Plausible | — | Refuted: rotate_dynamo guards itself |
| F7: Silent skip has no observability log | Observability gap | Low | Not blocking; suggested debug! addition |
| F8: Margin coverage across all networks | Plausible | — | Refuted: devnet unreachable; assert correct |

---

## Conclusion

**SHIP.** The fix correctly protects legitimately-parked dynamos from the 30s
rotation churn while preserving the #742 rotation for genuinely-silent dynamos.
The one real-but-bounded finding (F1) represents a self-correcting deferral of
rotation, not a permanent wedge. The fix is praos-byte-identical and free of
lock-ordering hazards.

The only actionable recommendation before merge is the optional debug-level log
in Finding 7, which improves diagnosability of the fix itself in future
investigations. It is not a correctness requirement for shipping.

Live validation on db-mainnet-val (genesis cold-restart advancing past
snapshot_tip+k to CaughtUp) remains the acceptance gate per the commit message.
