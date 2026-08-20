---
name: issue-1088-snapshot-map-ordering-fix
description: #1088 fix — every map/set reachable from LedgerStateSnapshot now writes in key order via BTreeMap/BTreeSet or a *Wire mirror struct, plus the replacement version-pinned hash guard.
metadata:
  type: project
---

Implemented #1088: `snapshot_format_hash_stability` was only stable because
every map field in its fixture held ≤1 entry. Fixed at the serialization
boundary (option "a" from the architect's prior planning pass, NOT converting
live `imbl::HashMap`/`HashMap` fields to ordered containers) — SNAPSHOT_VERSION
was already bumped 38→39 earlier in the same session for other reasons
(#1067/#1073/#1085), so this rides under the existing bump.

**Scope, measured not assumed: 42 distinct field declarations** reachable
from `LedgerStateSnapshot` needed conversion (54 instances counting
`StakeSnapshot` ×3 mark/set/go and `PGraph` ×4 governance purposes). Verified
by grepping `HashMap<`/`imbl::HashMap<`/`HashSet<`/`imbl::HashSet<` across
every type reachable from the struct's field list — `dugite-primitives` (the
key types: `Hash<N>`, `GovActionId`, `Voter`, `Credential`, `Pointer`,
`EpochNo`, `ProtocolParameters`) has NO nondeterministic collections at all;
`CostModels.unknown_cost_models` and `GovAction::{TreasuryWithdrawals,
UpdateCommittee}`'s maps were ALREADY `BTreeMap`. The entire nondeterminism
was confined to `dugite-ledger/src/state/`.

**17 direct `LedgerStateSnapshot` fields** converted `HashMap`/`HashSet` →
`BTreeMap`/`BTreeSet` in place (matching the pre-existing `vrf_key_hashes`
precedent): delegations, pool_params, future_pool_params,
pending_retirements, reward_accounts, pointer_map, genesis_delegates,
future_gen_delegs, epoch_blocks_by_pool, ptr_stake,
script_stake_credentials, pending_mir_reserves, pending_mir_treasury,
opcert_counters, stake_key_deposits, pool_deposits, rupd_addrs_rew.

**12 new `*Wire` mirror structs** for types shared between live `LedgerState`
and the snapshot (cloned wholesale, so the live type can't just change):
`StakeDistributionStateWire`, `NonMyopicWire`, `PendingRewardUpdateWire`,
`StakeSnapshotWire` (used ×3 via mark/set/go), `EpochSnapshotsWire`,
`DRepRegistrationWire` (fixes the nested `delegs: ImblHashSet` one level
below `dreps`' own keys), `PGraphWire` (×4 via `GovRelation`'s four
governance purposes), `EnactedGovTermsWire`, `PulsingSnapshotWire`,
`PulsedRatifyStateWire`, `DRepPulsingStateWire`, `GovernanceStateWire`. Each
has `From<&Live> for Wire` / `From<Wire> for Live`, field-for-field
identical except hash-ordered collections become `BTreeMap`/`BTreeSet`.
Fields already `imbl::OrdMap`/`OrdSet` (`proposals`, `votes_by_action`,
`drep_expiry`, the `PRoot`/`PEdges` proposal trees) were left untouched —
already deterministic, wrapping them again would be noise.

All 12 wire types got `PartialEq` added to their derive (needed by
`dugite-node/src/verify_snapshot.rs`'s `cmp_pretty`, a pre-existing
byte-exact-import verification harness that reads `LedgerStateSnapshot`
fields directly — its `diff_map`/`diff_set` helpers became dead code once
every field it called them on moved to BTreeMap/BTreeSet, and were deleted
rather than `#[allow(dead_code)]`'d, matching house style).

**The `at_least_two!` test discipline.** `fixture_populates_every_snapshot_field`
now requires every affected map/set to carry 2+ entries (0 or 1 has nothing
to reorder). `test_fixtures::populated_ledger_state()` was extended
field-by-field to match — including `governance.proposal_graph`, which the
OLD fixture never populated AT ALL (zero entries, invisible to any check).
Two fields deliberately EXCLUDED from the 2+-entries check:
`pending_pp_updates`/`future_pp_updates` are `BTreeMap<EpochNo,
Vec<(Hash32,...)>>` — already ordered at both levels (outer key, inner
`Vec`'s push order, verified by reading the one production call site in
`apply.rs` — genuinely push-based, never a HashMap iteration) — so requiring
2 OUTER keys there would test nothing about #1088.

**The determinism test's key finding.** `snapshot_bytes_are_independent_of_insertion_order`
just calls `populated_ledger_state()` TWICE and diffs the bytes — no second
hand-maintained "reordered" fixture needed, because `std::collections::hash_map::RandomState::new()`
increments a thread-local counter on every call
(`library/std/src/collections/hash/map.rs`), so two `LedgerState::new()`
calls in one process get genuinely different HashMap seeds. RED-proven by
swapping the whole fixed `snapshot_format.rs` back to the git-HEAD
(pre-fix) version and confirming the test failed reliably (5/5 runs) with
the SAME extended fixture/test files in place — disarming just ONE 2-entry
field is NOT reliable (only 2 possible orderings, ~50% chance of masking by
luck per run — confirmed empirically, worth remembering for future RED
proofs on small fixtures).

**Guard replacing `xtask/tests/snapshot_one_bump_invariant.rs`** (deleted
earlier same session — its `git tag`-based mechanism was vacuous under CI's
shallow checkouts). Landed as a NEW test in the same file,
`snapshot_hash_is_pinned_to_the_current_snapshot_version`, plus a new public
`LedgerState::snapshot_version()` accessor (wraps the `pub(crate)`
`SNAPSHOT_VERSION` so the external integration-test crate can read it). It
pins `EXPECTED_SNAPSHOT_VERSION: u8 = 39` beside `snapshot_format_hash_stability`'s
`EXPECTED_HASH`, asserting they describe the same code — no tags, no git,
works at any clone depth. `snapshot_format_hash_stability`'s own failure
message was rewritten to explicitly say "bump SNAPSHOT_VERSION AND update
BOTH constants" vs "fixture-only, update EXPECTED_HASH alone".

New pinned hash: `e3f85424803c58e66691144fbff3d6289dc37702fb07ef263c88358cc89ffbef`
(verified stable across 5+ separate process invocations before pinning —
the whole point of the fix).

Full verification: `cargo build/clippy(-D warnings)/fmt --check` all clean
workspace-wide; `cargo nextest run --workspace` 8200/8201 pass, the one
failure is `xtask::qa_report_covers_shipped_code` — pre-existing/deliberate
(CLAUDE.md's own "Current Focus" already documents the QA report as stale
whenever `crates/` changes since the last release-gate run; unrelated to
this fix, a release-lead concern).

**Not touched, deliberately**: `UtxoSet` (out of scope per the issue —
`attach_store` clears the in-memory map in production, so it's empty at
serialization time). Left `verify_snapshot.rs`'s `utxo_set` comparison logic
and its module doc comment's "semantic diff because HashMap order is
nondeterministic" framing — updated the doc comment to note the framing is
now narrower (only `utxo_set` remains genuinely order-sensitive) but kept
the harness AS semantic diff rather than switching it to byte-exact, since
semantic diff also reports WHICH keys differ.
