---
name: issue-1071-nesru-wire-arms
description: nesRu (NewEpochState[4]) SNothing/Complete wire arms implemented; Pulsing deliberately deferred — RewardUpdate.rs is tag-258 Set at PV>=9, threshold-23 gated at BOTH levels
type: reference
---

#1071 (SNAPSHOT_VERSION 39, no further bump). Landed: `SNothing`/`Complete`
fully byte-correct; `Pulsing` deliberately reports `SNothing` (documented gap,
not fabricated bytes).

## Design that survived: TWO parallel fields, not one collapsed enum

The design doc's own "intended next step" said collapse `rupd_pulser_started`/
`rupd_monetary`/`rupd_snapshot` into one `Option<PulsingRewUpdate>`. Reverted
after discovery: `state/epoch.rs` alone has 20+ hand-rolled unit tests setting
`rupd_pulser_started = true` directly (bypassing `apply.rs`'s freeze) while
relying on `rupd_monetary == None`'s boundary-recompute fallback inside
`compute_reward_update`. Collapsing would force each of those ~40 sites to
construct an internally-consistent `RewardSnapShot` or trip
`compute_reward_update`'s own `debug_assert_eq!(m.delta_r1, expansion, ...)`.
Kept `rupd_pulser_started: bool` + `rupd_monetary: Option<MonetaryStep>`
UNTOUCHED (zero risk to the validated #1072 consensus fix and its test
suite); added `rupd_snapshot: Option<PulsingRewUpdate>` as a THIRD,
deliberately-separate WIRE-ONLY field, written at the same two call sites
(apply.rs freeze + boundary clear) so it "moves with" the pair without being
the same storage. See `EpochSubState::rupd_pulser_started`'s doc for the full
argument — this is a documented exception to "one mechanism, not N copies",
not an oversight.

## `RewardSnapShot`'s `likelihoods`/`leaders` — captured on first PULSE, not at freeze

`rewLikelihoods`/`rewLeaders` need the per-pool `PoolRewardInfo` table, which
doesn't exist yet at the freeze instant (apply.rs's `4k/f` mark). Filled in by
`pulse_rupd_member_fold`'s `just_built_table` branch — the SAME table build
`InFlightFold.table` uses — not a third copy of `build_pool_reward_table`.
`Pulsing -> Complete` promoted in the same function when `fold.is_done()`.
Both are wire-only; `compute_reward_update` still derives its own answer
independently at the real boundary.

## `Complete`'s `rs` field needed a NEW `#[serde(skip)]` field on `PendingRewardUpdate`

Haskell's wire `Complete` carries `RewardUpdate{deltaT,deltaR,rs,deltaF,nonMyopic}`
where `rs :: Map Credential (Set Reward)` is the RAW pre-aggregation per-source
map — NOT `PendingRewardUpdate.rewards: HashMap<Hash32,Lovelace>` (the
post-`filterRewards` aggregated total consensus actually credits). Added
`PendingRewardUpdate.raw_rewards: HashMap<Hash32, Vec<RewardEntry>>`,
`#[serde(skip)]` (never persisted, zero snapshot-format risk), populated at
the ONE point inside `compute_reward_update` where `reward_entries` exists
before the aggregating loop consumes it. The node's query layer calls
`compute_reward_update` FRESH (pure function, clone of `rupd_fold.fold`,
never touches live state) to render `Complete` — reuses the one true
implementation rather than re-deriving deltaT/deltaR.

## Oracle-verified wire shape for `RewardUpdate.rs` — do not re-derive by analogy

`RewardUpdate`'s `EncCBOR` is HAND-WRITTEN (`encodeListLen 5 <> ... <> encCBOR rw <> ...`),
not the `Rec !> To` combinator DSL `RewardSnapShot`/`FreeVars` use in the same
file — so `rs` gets NO bespoke treatment, straight through the library's
generic `Map`/`Set` instances:
- outer `Map (Credential Staking) (Set Reward)`: `encodeMap`, threshold-23
  definite/indefinite, **NO tag**, at any PV.
- inner `Set Reward` (per credential): `encodeSet`, tag **258** at PV>=9 (same
  convention as every other Set in this codebase), ALSO threshold-23 gated.
- `Reward = array(3)[rewardType(0=Member,1=Leader), pool KeyHash(28B), Coin]`.
- `deltaT`/`deltaR`/`deltaF` are `encCBOR (invert dr)`/`(invert df)` —
  `deltaR`/`deltaF` are NEGATED relative to the field name in the record
  before encoding (Haskell quirk noted in the source's own TODO comment).
  dugite's `delta_reserves`/`delta_fee` sign convention (positive=debit) as
  already used by `PendingRewardUpdate`/`apply_pending_reward_update`
  matches the WIRE value directly (no extra negation needed) — verified by
  matching against the `complete-nonzero.hex` capture's structural bytes.

First implementation used unconditional `enc.map()`/`enc.array()` (definite
only) for both levels — the exact #938 class, and REAL: `rs` routinely holds
>23 credentials on any live network. Fixed with `open_variable_map`/
`open_variable_array`/`close_if_indefinite` helpers; RED-proven test
`complete_reward_update_rs_map_goes_indefinite_above_23_credentials`.

## `Pulsing` deliberately NOT implemented — the real blocker, once `#1071`'s design doc's stated blocker was wrong

Design doc claimed Pulsing was blocked because "incremental pulsing doesn't
exist" — FALSE by the time this landed (Phase 3 shipped it, production-wired,
per-block). The REAL blocker, found only by dispatching the cardano-ledger
oracle for the Pulser's exact CBOR shape: `FreeVars.fvPoolRewardInfo :: Map
(KeyHash StakePool) PoolRewardInfo`, and `PoolRewardInfo.poolPs ::
StakePoolSnapShot` — a DIFFERENT, 10-field DERIVED record from `PoolParams`
(NOT the registration record dugite already encodes elsewhere as
`PoolSnapshotEntry` for `SnapShots`... actually IS structurally the same shape
as that type per `snapshots-encoding.md`, but dugite's `PoolRewardInfo` struct
carries none of those 10 fields — it only has the 5 REWARD-relevant scalars).
`balance` needs `Credential -> StakeWithDelegation` (stake amount PLUS the
delegated pool — NOT a bare `CompactCoin`, corrected from an earlier
assumption both in this fix and in `nesru_wire_shape.rs`'s own comment).
`RewardAns` needs `Reward`-typed entries (rewardType/pool/amount), not raw
lovelace. None of this data is computed or stored anywhere, live or
persisted — the fold only ever produces the AGGREGATE amount per credential.
Fabricating it (plausible field values not backed by real computation) would
be the exact #1057/#1067 mistake. `encode_possible_reward_update` therefore
emits `array(0)` (SNothing) whenever `rupd_snapshot` is `Some(Pulsing(_))`.

This is a SMALLER gap than #1071 started with: `Pulsing` is JSON-invisible
(renders as `null` same as `SNothing` per Haskell's own `ToJSON` instance) and
the fold typically completes within ~20-45 blocks of the `4k/f` mark then
stays `Complete` for the rest of the epoch — so the dominant, JSON-visible
~80% case (`Complete`) is now fully correct; only the narrow `Pulsing`
sub-window (a few dozen blocks, once per epoch) still reads `SNothing`.

## Live-timing validation — NOT completed, scoped correctly deferred

The acceptance bar in the issue (observe all THREE states against a real
cardano-node 11.0.1 devnet peer, sampled by CBOR not JSON, needing ~100+
seeded credentials per `tests/fixtures/nesru/README.md`'s threshold math) was
never attempted this pass — explicit judgment call given the scope already
consumed on the mechanism + the Pulser-shape research. `Pulsing` isn't wired
to the encoder at all, so the three-state acceptance bar as originally framed
no longer applies as written; the live check that WOULD still be valuable is
narrower: confirm `SNothing -> Complete` fires at the right slot and that
`Complete`'s deltaT/deltaR/rs match a live capture, on a real devnet. Not run.
