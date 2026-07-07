---
name: issues-796-803-batch-fix
description: Signed RUPD delta_reserves (i128) + MIR apply NoMirTransfer fix, both oracle-verified, SNAPSHOT v26->27
metadata:
  type: project
---

## #796 — signed `delta_reserves` (u64 -> i128)

`PendingRewardUpdate.delta_reserves` was `u64` computed via `saturating_sub`,
silently flooring to 0 whenever a degraded/low-block epoch made
`epoch_fees > treasury_cut + total_distributed` — Haskell's `deltaR` is a
signed `DeltaCoin`/`Integer` that CREDITS reserves in that case. Changed to
`i128` end-to-end: `state/mod.rs` field, 4 compute sites in
`state/rewards.rs` (3 early-return branches + the main path, all now
`treasury_cut as i128 [+ total_distributed as i128] - epoch_fees as i128`,
no saturation), and exactly **7 apply sites** (confirmed by direct grep +
read, matches the reviewer's count): `rewards.rs` (`apply_pending_reward_update`
+ test-only `calculate_and_distribute_rewards`), `state/epoch.rs`,
`eras/shelley.rs` x2, `eras/conway.rs` x2. All 7 now go through one shared
helper `rewards::apply_reserves_delta(u64, i128) -> u64` (sign-branch:
`d>=0` debits via `checked_sub`, `d<0` credits via `checked_add((-d) as u64)`),
re-exported `pub(crate)` from `state/mod.rs` since `eras::*` is not a
descendant of the private `state::rewards` submodule.

Three pre-existing tests were **pinning the bug**: `test_reward_zero_reserves_no_expansion`
asserted "reserves should not change when already at 0" — after the fix
reserves correctly increase by 800_000 (the undistributed fee remainder).
Updated to assert the credit.

Correct conservation identity (re-derived from scratch — do NOT trust the
pre-existing `-delta_reserves + delta_treasury + total_distributed +
undistributed - epoch_fees == 0` comment at `rewards.rs` ~570-574 without
re-checking; it has an extra `undistributed` term that doesn't algebraically
cancel when substituted through — likely a pre-existing doc bug, out of
scope to fix here): **`-delta_reserves + delta_treasury + total_distributed
== epoch_fees`** (the only new lovelace entering the reserves+treasury+
distributed system each boundary is `epoch_fees`; expansion is reserves'
own money moving around net of what returns via `delta_reserves`).

SNAPSHOT_VERSION bumped 26 -> 27 (`PendingRewardUpdate` is embedded in
every `LedgerState` snapshot via `pending_reward_update`/`last_applied_rupd`).

## #803 — MIR apply panic -> Haskell `NoMirTransfer`

Oracle-verified (`cardano-ledger-oracle`, live source research, saved at
`cardano-ledger-oracle/mir-pot-transfer-semantics.md`) semantics for
`Cardano.Ledger.Shelley.Rules.Mir.mirTransition`:
- **No cross-term** in Haskell's own signed accumulator:
  `availableReserves = reserves \`addDeltaCoin\` deltaReserves` (own-pot
  delta only). BUT dugite represents the pot-to-pot transfer as **two
  independent non-negative magnitudes** (`pending_mir_delta_reserves` =
  reserves->treasury, `pending_mir_delta_treasury` = treasury->reserves)
  rather than Haskell's one signed `deltaReserves = -deltaTreasury` value.
  Translating dugite's fields into Haskell's own-delta formula THEREFORE
  legitimately produces `available_reserves = reserves - dr + dt` and
  `available_treasury = treasury + dr - dt` — this LOOKS like a cross-term
  but isn't one relative to Haskell; it falls out of dugite's two-magnitude
  encoding of the same net value. Don't be confused by an apparent conflict
  between "oracle says no cross-term" and "the dugite code has one".
- Solvency is a **single combined AND**, not independent per-pot:
  `totR <= availableReserves && totT <= availableTreasury`. A treasury
  shortfall blocks an otherwise-solvent reserves payout in the same epoch.
- `PredicateFailure (MIR era) = Void` — this is a **total, non-throwing**
  STS. Insolvency emits `NoMirTransfer` (informational event only): both
  pots left byte-identical, pending maps (`dsIRewards`) wiped regardless of
  branch (success or failure both clear it).
- Registered-credential filter (`Map.intersection accountsMap`) happens
  BEFORE summing totR/totT — unregistered entries never count and are
  silently dropped when the map is wiped (matches dugite's pre-existing
  behavior).
- Haskell's own boundary check does NOT re-verify per-credential
  negativity (that's a separate, earlier DELEG-time check
  `MIRProducesNegativeUpdate`/`InsufficientForInstantaneousRewards`).
  Added an extra defensive per-credential non-negative check in dugite's
  `apply_pending_mir` anyway (task-directed, defense-in-depth) since
  `validation/mir.rs` documents Phase-1 admission gaps for exactly this
  predicate — vacuously true on any valid history, only fires on
  adversarial/malformed input that slipped past Phase-1.

Rewrote `state/certificates.rs::apply_pending_mir`: drains all 4 pending
accumulators up front (`mem::take`, matching Haskell's unconditional wipe),
filters to registered, computes totR/totT + available_reserves/treasury,
checks `solvent && all_non_negative`, and on failure just `warn!`s and
returns (no panic, pots untouched). On success, applies byte-identically to
the old code but as one combined i128 computation (`available_reserves -
tot_r` cast once) instead of 4 sequential u64 subtractions — provably
equivalent given solvency was pre-checked, and avoids spurious
intermediate-step underflow that the OLD per-step code was vulnerable to
even in some cases where the *net* result would have been fine.

Four pre-existing tests were **pinning the panic** as correct behavior
(`test_mir_pot_transfer_capped_at_available` in `certificates.rs`,
`test_mir_compound_credential_and_pot_transfer`,
`test_mir_pot_transfer_exceeds_source_treasury`,
`test_mir_pot_transfer_zero_source` in `state/tests.rs`) — all four
updated to assert NoMirTransfer (pots unchanged, pending accumulators
cleared, no panic) instead. Added a new test
(`test_mir_insolvent_per_credential_no_mir_transfer_803`) that specifically
exercises the case where the *aggregate* solvency check alone would pass
(a negative per-credential delta only makes totR easier to satisfy) but the
defensive per-credential guard still catches it.

## Gotchas for future edits in this area

- `state::rewards` is declared as plain `mod rewards;` (private to `state`
  and its descendants) in `state/mod.rs`, NOT `pub(crate) mod` like
  `governance`. Anything needed from `eras::*` must be re-exported via
  `pub(crate) use rewards::foo;` in `state/mod.rs` — `crate::state::rewards::foo`
  from outside `state` will not resolve even if `foo` itself is `pub(crate)`.
- `certs.reward_accounts` is `imbl::ImblHashMap<Hash32, Lovelace>`, not
  `std::HashMap` — `.get`/`.get_mut`/`.contains_key` all work the same.
- `reward_debug.rs` (feature `reward-debug-dump`) and `epoch_state_debug.rs`
  (feature `epoch-state-debug`) are cfg-gated out of default builds but ARE
  compiled by CI's `--all-features` job (`.github/workflows/ci.yml:184`) —
  always build+test with `--all-features` too when touching
  `PendingRewardUpdate` or any type these dumpers embed.
