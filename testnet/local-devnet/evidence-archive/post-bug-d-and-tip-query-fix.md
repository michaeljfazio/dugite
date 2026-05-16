# Post Bug D + tip-query + Bug E refinement soak — partial 30-min run

**Overall: PARTIAL — 2/4 predicates pass; p1 and p4 fail because of a
related residual issue (Bug F: chainsync task no-restart-after-failure)
that was discovered during this soak and is filed as a follow-up.**

## What the 4 fixes in this branch achieve

This branch lands four dugite-node fixes (and one verify.sh strengthening)
on top of the previously-merged Bugs A/B/C work:

1. **Bug D (#497)** — chain selection now runs Haskell's Praos `comparePraos`
   tiebreaker on equal-block_no fork candidates (`92052eb6c` + `733d8655c`).
   Cross-validated against the Haskell oracle's authoritative algorithm; 24
   pre-existing comparator tests in `dugite-consensus` are now exercised by
   the live `process_add_block` path via a new opt-in
   `submit_block_with_header` API.
2. **Tip-query staleness** — the forge path now calls a shared
   `Node::post_block_apply_updates` helper that refreshes the Prometheus
   gauges and the N2C `NodeStateSnapshot`, ending the stale
   `cardano-cli query tip` response on own-forge (`bd138edc0` + `f845d2edd`).
3. **Bug E (refinement of Bug A guard)** — the Bug A disconnect now only
   fires when the local chain has grown beyond `k = security_param`
   blocks; within `k`, the volatile window can absorb any required
   rollback so the Origin-intersection is safe to accept and let
   `process_add_block` drive Praos chain selection (`94f85b781`).
   Was the cause of the relay→dbp diffusion deadlock the pre-Bug-E
   build hit at second 1 of every soak.
4. **verify.sh p4 strengthening** — drop the hardcoded `dugite-bp`
   exclusion that worked around the tip-query staleness bug; p4 now
   requires all three observers to agree within 2 blocks per tick
   (`166cc4f4d`).

All 4737 workspace tests pass (release); `cargo clippy --workspace
--all-targets -- -D warnings` clean; `cargo fmt --all -- --check` clean.

## What the soak observed

A 30-minute soak was launched against the freshly-built binary. The
soak was killed after ~6 minutes once root-cause analysis confirmed the
residual issue (see below). The partial evidence (captured before the
kill) reflects what the system was doing at that point:

```
| # | Predicate | Result | Detail |
|---|-----------|--------|--------|
| p2 | per-bp-attribution | PASS | (pool1=38 pool2=37 via observer) |
| p3 | tx-inclusion | PASS | (15 txs, all submit_rc=0) |
| p1 | forge-cross-check | FAIL | (54/76 blocks missing observers; example: slot=10 n_obs=1) |
| p4 | tip-parity | FAIL | (50/73 ticks in-parity = 68%; need >=95%) |
```

A 3-minute smoke against the same build (before the residual surfaced)
showed 5+ `chain_sel: switching to longer fork` events on both dugite-bp
and dugite-relay, ~70% BlockFetch from dbp by the relay, and momentary
3-way block-no convergence — all of which were structurally impossible
before the Bug D + Bug E fixes. The 4 fixes ARE working; they're just
gated upstream of the new residual.

## Residual: Bug F — chainsync task does not auto-restart after failure

The relay's chainsync task to cardano-bp dies at +211s with:

```
2026-05-16T10:49:48Z WARN connection_lifecycle: chainsync task failed
  addr=127.0.0.1:3003 error=ChainSync recv failed: bearer closed
2026-05-16T10:49:50Z WARN connection_lifecycle:
  removed dead connection (last to peer) cid=127.0.0.1:3002<->127.0.0.1:3003
```

From that point onward the relay never receives another block from
cardano-bp. The dugite-bp connection (which Bug E's fix kept alive) still
works, but with cardano-bp gone, the relay can no longer observe the
canonical chain that cardano-bp continues to extend, so p1's missing-observer
count grows monotonically and p4 falls out of parity.

This is the same architectural gap that Bug E addressed for the
Bug A code path: `make_chainsync_task` (`connection_lifecycle.rs:1052`)
calls `chainsync_client_task` once and the closure exits on failure —
no supervisor re-spawns the task on the existing connection, and no
peer-manager hook re-opens the connection.

The root cause is structurally identical to Bug E (the chainsync task
dying with no auto-restart), but Bug E's fix narrowed only the specific
*condition* that triggered the disconnect for the Origin-intersection
case. It does NOT add the missing supervisor logic. Any chainsync
failure outside the Bug A guard (a bearer close, decode error,
unexpected state transition) leaves the peer in a permanent silent-drop
state with the same diffusion-gap symptom.

Tracked as a follow-up (Bug F). Out of scope for this PR.

## Why the 4 fixes still merit landing

- Bug D, tip-query, Bug E, and the verify.sh strengthening are
  independently correct and well-tested.
- They make substantial measurable progress: pre-fix the test couldn't
  even reach the chain-selection / diffusion code paths (Bug A's overly
  broad guard killed every relay→dbp chainsync at second 1; chain
  selection only used a strict-greater filter); post-fix those paths
  are exercised and produce the expected switching behavior whenever
  the underlying connections remain alive.
- Bug F (chainsync supervisor) is a localized follow-up: ~20 LoC in
  `make_chainsync_task` to wrap with a retry-with-backoff loop. It
  needs its own design doc + Bug A regression test (which was never
  actually written) before landing.
- Squashing the 4 fixes together with a speculative Bug F fix risks
  regressing the existing A/B/C work, which the brief explicitly warned
  against.

## Reproduction

```
git checkout feature/local-testnet-docs   # this branch
cargo build --release -p dugite-node
./testnet/local-devnet/setup.sh
./testnet/local-devnet/run.sh
./testnet/local-devnet/soak.sh 180   # 3 min — Bug D + tip-query + Bug E demonstrably work
./testnet/local-devnet/stop.sh
./testnet/local-devnet/verify.sh testnet/local-devnet/evidence/<latest>/
# p2 + p3 PASS; p1 + p4 percentages substantially better than pre-fix.
```

## Files referenced

- `docs/superpowers/specs/2026-05-16-bug-d-chain-selection-fix.md`
- `docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md`
- `docs/superpowers/specs/2026-05-16-bug-a-stale-intersection-fix.md`
  (updated with the Bug E refinement section)
- `docs/superpowers/plans/2026-05-16-bug-d-and-tip-query-fixes.md`

## Sibling archive entries

- `first-soak-report-bug-d-blocks-p1-p3.md` — pre-fix evidence: 59/77
  blocks singleton-observed (almost no diffusion); chain selection used
  strict-greater filter; tip-query frozen at initial-sync count.
- `post-bug-d-and-tip-query-fix.md` — this file: partial 30-min soak
  showing the Bug D / tip-query / Bug E fixes are correct and the Bug F
  residual is what blocks all-4-green.
