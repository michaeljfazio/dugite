---
name: issue-1071-complete-arm-followup-4-defects
description: nesRu Complete-arm follow-up — 4 defects fixed after c5dbb5b5bd (P1 key shape, pulse timing, zero-amount leaders, sort order), all grounded independently since the coordinating task's design doc never existed in the worktree
type: reference
---

Follow-up to [[issue-1071-nesru-wire-arms]]. A coordinating-agent task claimed
a Fable design doc at `docs/superpowers/specs/2026-08-21-nesru-pulsing-wire-arm-design.md`
existed "uncommitted, in the current working tree" — it did NOT exist in this
worktree (checked: not in git history, not on disk, not in any sibling
worktree's committed history). A subagent's working tree is not shared with
the agent that spawned it; only committed history is. All four defects below
were independently re-derived and grounded from the REAL committed fixtures
(`tests/fixtures/nesru/*.hex`) and two oracle dispatches, not from the
claimed doc. Worth remembering: when a task cites an "uncommitted" file as
grounding, verify it exists in YOUR tree before trusting the task's framing —
do not assume prior-agent claims about file state are still true.

## The 4 fixes (all in `c5dbb5b5bd`'s Complete-arm code, none touching Pulsing)

**P1 — `rs` map keys were a bare `bstr(32)`, not `Credential = array(2)[disc,
hash28]`.** Decoded `complete-nonzero.hex` with `cbor2` in Python (ground
truth, not by eye): every key is `[0|1, bstr(28)]`. The discriminator was
**already present** in the padded `Hash32` at byte 28
(`Credential::to_typed_hash32()`: 0=KeyHashObj, 1=ScriptHashObj) — nothing
had to be threaded through the pipeline, only unpacked at the encode site
(`encoding.rs`'s `encode_possible_reward_update`) and the query.rs population
site. `query.rs:1182`'s `cred.as_ref().to_vec()` was NOT truncating (confirmed
`Hash<32>::as_ref()` returns all 32 bytes) — a subagent flagged this as
unverified and it turned out fine.

**#4 — outer `rs` map sort order was hash-major, ignoring the discriminator's
RANK.** Two independent confirmations that `ScriptHashObj` sorts BEFORE
`KeyHashObj` (opposite the numeric wire discriminator 0/1): (a) oracle
dispatch quoting `Cardano.Ledger.Credential`'s `data Credential = ScriptHashObj
| KeyHashObj deriving Ord` at pinned rev `faa7a9dc347697b11d4da5b7818b1731e11aeeef`
— derived Ord compares constructor declaration order first; (b) dugite's OWN
pre-existing `Credential::cmp_ledger()` (crates/dugite-primitives/src/credentials.rs)
already implements this exact rule for an unrelated call site (Plutus
ScriptContext credential ordering) — found independently, corroborates the
oracle without having been consulted first. New `sort_reward_wire_credentials`
in query.rs. No real fixture has a mixed-discriminator `rs` map (the seeding
devnet used only key-hash credentials) so this direction is synthetic-but-
grounded, not fixture-derived — labelled as such in the test.

**#3 — zero-amount leader rewards dropped by an `operator_reward > 0` gate.**
Real captures (`pulsing.hex`'s `rewLeaders`, `complete-nonzero.hex`'s `rs`)
both carry a genuine `[1, pool, 0]` entry — Haskell's `collectLRs` never
gates on amount, only the pv<=6 registration prefilter. Concrete zero-producing
scenario: cost=0, margin=0, no self-delegation makes the whole
`operator_reward` bracket 0 regardless of pool_reward magnitude.

**The trap this one has**: `PoolRewardInfo.leader` feeds TWO consumers —
the wire-only `rs`/`raw_rewards` path AND the CREDITED `reward_map`/
`total_distributed` aggregation (same shared `reward_entries` collection).
At pv<=2 (mainnet epochs 208-236, i.e. LIVE and already-validated), the
aggregation uses `Set.deleteFindMin` (lowest `(is_member,pool_id)` wins,
leader always beats member) — so blindly un-gating `operator_reward>0` would
have let a zero-amount leader entry WIN that selection over a real nonzero
member entry for the same credential, silently zeroing a live-network reward.
Fixed by cloning `reward_entries` into a separate `raw_reward_entries` right
after the member fold (before the leader loop), pushing the zero-amount entry
into the RAW clone unconditionally but into the aggregation-feeding original
only when `amount > 0` — preserving 100% of prior pv<=2 behavior while fixing
the wire. `raw_rewards` (source of `rs`) now built from the clone, not from
`reward_entries`. This is exactly the "read the doc's caveat before touching
aggregation logic" instruction from the coordinating task, done by tracing the
code myself since the doc didn't exist.

**#2 — pulse/Complete timing off by one tick in TWO independent ways.**
Oracle-verified against `PulsingReward.hs`'s `pulseStep`:
```haskell
pulseStep p@(Pulsing _ pulser) | done pulser = completeStep p   -- checked BEFORE pulsing
pulseStep (Pulsing rewsnap pulser) = do p2 <- pulseM pulser; pure (Pulsing rewsnap p2, event)
```
(a) the tick that CREATES the pulser (`SNothing->Pulsing`) performs ZERO
pulses — `startStep` is a pure constructor, Haskell's advancing clause only
matches when `ru` is ALREADY `SJust (Pulsing _)` on entry, true starting the
NEXT tick. dugite's `apply.rs` froze the pulser then immediately called
`pulse_rupd_member_fold` in the SAME block, which — because `just_built_table`
fell through to the pulse code — performed pulse #1 on the creation tick.
(b) completion has a ONE-TICK LAG: `done` is checked BEFORE pulsing, so the
pulse that drains the queue still returns wrapped in `Pulsing`; only the
FOLLOWING tick's check (no new pulse) promotes to `Complete`. dugite checked
`fold.is_done()` AFTER pulsing in the same call and promoted immediately.

Fix, both in `rewards.rs`'s `pulse_rupd_member_fold`: the `just_built_table`
branch now `return`s before reaching the pulse code (was falling through);
the `is_complete()` check moved from AFTER `fold.pulse(...)` to a NEW check at
the TOP of the function (before the pv<=6 gate and before `just_built_table`),
which does the wire promotion and returns WITHOUT pulsing — the pulse call at
the bottom of the function has NO post-pulse `is_done()` check at all anymore.
`InFlightFold::is_complete()` itself (internal fold bookkeeping, consumed by
`compute_reward_update`'s `prepulsed` param) is UNCHANGED and stays immediate
— only the WIRE snapshot's promotion timing has the lag; conflating the two
would have been wrong.

## Collateral test fix (found by full-suite run, not anticipated)

`state::tests::first_pulse_applies_the_pv6_prefilter_to_the_queue_head`
(#1074's regression test) assumed 2 blocks = 2 pulses (old creation-tick-
also-pulses model). Now needs 3 blocks: block1 creates (0 pulses), block2
takes pulse #1 (queue head), block3 takes pulse #2. Fixed by adding a third
`apply_block` call and updating the explanatory comments — the #1074
mechanism itself (fvAddrsRew must be captured before the first REAL pulse)
is untouched and still correctly tested; only the block-count needed to
observe "both credentials folded" changed.

## Method notes worth keeping

- `python3 -c "import cbor2"` was available and is the fastest way to
  ground-truth decode a `.hex` fixture — faster and more reliable than
  hand-walking bytes, and `cbor2`'s dict iteration preserves wire order so
  sort-order claims can be checked directly from `repr(dict)`.
- Writing a manual CBOR item-length walker (not cbor2) was necessary to get
  exact BYTE OFFSETS (`rs` occupies `complete-nonzero.hex[22:1552]`) for a
  byte-slice-identity test — cbor2 gives you values, not offsets.
- The disk filled to 119Mi free mid-task (926Gi volume, other worktrees/
  mainnet-replay activity) and an `Edit` call failed with ENOSPC on a tiny
  text file. It self-recovered (124Mi → 73Gi over ~10 min) without
  intervention — a transient contention spike, not a leak in this worktree
  (`target/` here was empty at the time). Retry before assuming a real
  disk problem; do not attempt cleanup outside your own worktree.
- `cargo test -p dugite-node --lib` does NOT run anything under `node::` —
  that module tree lives behind `mod node;` in `main.rs` (the `[[bin]]`
  target), and `lib.rs`'s `pub mod node` re-export is gated on
  `feature = "test-utils"` and only exposes `peer_connection`. Tests inside
  `node::query`/`node::n2c_query::*` require
  `cargo test -p dugite-node --bin dugite-node`. Cost ~10 minutes of
  confusion (`-- --list` showing 203 tests with zero `node::` entries).
