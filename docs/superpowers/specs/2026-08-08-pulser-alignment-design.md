# Pulser alignment — bringing dugite's RUPD and DRep pulsers to 100% structural parity

Status: **DESIGN, not yet approved for implementation**
Date: 2026-08-08
Tracks: #1071 (nesRu hardcoded SNothing); supersedes the "architectural gap"
note in #1071's fix sketch.

Sources are the cardano-ledger revisions cardano-node 11.0.1 actually pins —
CHaP pins per PACKAGE, not per monorepo tag:

- `cardano-ledger-shelley` 1.18.1.0 → `b7c17cf31871062b7883c46e3f367cb5e1b5db6c`
- `cardano-ledger-core` 1.20.0.0 / `-binary` 1.8.1.0 → `94e9618c91a16ec08db477632a158b630722089b`

---

## 1. Sweep result — the scope is bounded, and smaller than it looks

cardano-ledger contains **exactly two** `Pulsable` instances. This was verified
by grepping every fetched source, not inferred:

| Instance | Location | Purpose |
|---|---|---|
| `Pulsable RewardPulser` | `RewardUpdate.hs:300` | reward calculation (RUPD) |
| `Pulsable (DRepPulser era)` | `DRepPulser.hs:299` | DRep stake distribution + RATIFY |

**`InstantStake` is NOT a pulser.** It is an incrementally-maintained stake map
consumed whole at SNAP (`snapShotFromInstantStake`, `SnapShots.hs:418`). It has
no `Pulsable` instance and no in-progress state. dugite's equivalent is already
incremental. It is out of scope, and saying so explicitly is the point of a
sweep — "all mechanisms that require pulser functions" is a set of size two.

---

## 2. What Haskell actually does

### 2.1 RUPD — the only pulser whose inputs freeze MID-EPOCH

`ShelleyRUPD` fires per block. `sr = randomnessStabilisationWindow` (4k/f):

```haskell
-- Rupd.hs
determineRewardTiming currentSlot startAfterSlot endSlot
  | currentSlot > endSlot         = RewardsTooLate
  | currentSlot <= startAfterSlot = RewardsTooEarly
  | otherwise                     = RewardsJustRight

    slot     = epochInfoFirst ei e +* Duration sr        -- startAfter = first + sr
    slotForce = slot +* Duration sr                      -- end        = first + 2sr
```

| phase | condition | action |
|---|---|---|
| TooEarly | `s <= first + sr` | `SNothing` — pulser stays unset |
| JustRight | `first + sr < s <= first + 2sr` | `SNothing` → `startStep`; `Pulsing` → one `pulseStep`; `Complete` → hold |
| TooLate | `s > first + 2sr` | force `completeStep` |

`startStep` freezes, at that mid-epoch slot:

```haskell
-- PulsingReward.hs:99
startStep slotsPerEpoch b@(BlocksMade b') es@(EpochState acnt ls ss nm) maxSupply asc secparam =
  let SnapShot activeStake totalActiveStake stakePoolSnapShots = ssStakeGo ss
      numStakeCreds = fromIntegral (VMap.size $ unActiveStake activeStake)
      pulseSize = max 1 (ceiling (numStakeCreds %. (knownNonZero @4 `mulNonZero` k)))
      Coin reserves = acnt ^. casReservesL
      accounts = ls ^. lsCertStateL . certDStateL . accountsL
      pr = es ^. prevPParamsEpochStateL
      ...
      totalStake = circulation es maxSupply     -- maxSupply <-> casReserves
```

Frozen set: **`casReserves`, `accounts`, `ssStakeGo`, `prevPParams`, `ssFee`,
`BlocksMade`**, plus the constants `slotsPerEpoch`/`asc`/`k`/`maxSupply`.

Types:

```haskell
data PulsingRewUpdate
  = Pulsing !RewardSnapShot !Pulser
  | Complete !RewardUpdate

data RewardSnapShot = RewardSnapShot
  { rewFees, rewProtocolVersion, rewNonMyopic, rewDeltaR1, rewR, rewDeltaT1
  , rewLikelihoods :: !(VMap VB VB (KeyHash StakePool) Likelihood)
  , rewLeaders     :: !(Map (Credential Staking) (Set Reward)) }

data RewardPulser m ans where
  RSLP :: !Int -> !FreeVars
       -> !(VMap VB VS (Credential Staking) StakeWithDelegation)
       -> !ans -> RewardPulser m ans

data FreeVars = FreeVars
  { fvAddrsRew :: !(Set (Credential Staking))
  , fvTotalStake :: !Coin
  , fvProtVer :: !ProtVer
  , fvPoolRewardInfo :: !(VMap VB VB (KeyHash StakePool) PoolRewardInfo) }

instance Pulsable RewardPulser where
  done    (RSLP _ _ zs _) = VMap.null zs
  pulseM  (RSLP n free balance (clearRecent -> ans)) = ... VMap.splitAt n balance ...
  completeM (RSLP _ free balance (clearRecent -> ans)) = ... fold the whole balance ...
```

Wire and JSON:

```haskell
instance EncCBOR PulsingRewUpdate where
  encCBOR (Pulsing s p) = encode (Sum Pulsing 0 !> To s !> To p)
  encCBOR (Complete r) = encode (Sum Complete 1 !> To r)

instance ToJSON PulsingRewUpdate where
  toJSON = \case
    Pulsing _ _ -> Null          -- <<< renders the SAME as SNothing
    Complete ru -> toJSON ru
```

That last instance matters and is easy to get wrong: **a `Pulsing` pulser is
JSON-invisible.** The observable JSON divergence against dugite is confined to
the `Complete` window, which is exactly why the measured devnet divergence rate
was ~20% and not ~80%. On the CBOR wire, by contrast, `Pulsing` is a fully
encoded value carrying the whole snapshot and pulser.

### 2.2 DRep — inputs freeze AT THE BOUNDARY

`setFreshDRepPulsingState` (`ConwayGovernance.hs:460`) runs at the epoch
boundary and snapshots 13 fields into a `DRPulsing`: `dpAccounts`,
`dpInstantStake`, `dpStakePoolDistr`, `dpDRepState`, `dpCommitteeState`,
`dpEnactState` (with `ensTreasury` from the boundary treasury), `dpProposals`,
`dpProposalDeposits`, `dpStakePools`, `dpCurrentEpoch`, `dpPulseSize`,
`dpIndex = 0`, `dpDRepDistr = empty`.

It is then stepped on every NON-boundary tick:

```haskell
-- ConwayNewEpoch.hs:166
  if eNo /= succ eL
    then pure $ nes & newEpochStateDRepPulsingStateL %~ pulseDRepPulsingState
                    & newEpochStateGovStateL %~ predictFuturePParams
```

```haskell
data DRepPulsingState era
  = DRPulsing !(DRepPulser era Identity (RatifyState era))
  | DRComplete !(PulsingSnapshot era) !(RatifyState era)
```

Both pulsers use the same pulse-size rule: `max 1 (numEntries / 4k)`.

---

## 3. Gap analysis, classified by consequence

Three properties are bundled in Haskell and separable in principle:

| Property | Consensus-bearing? | RUPD in dugite | DRep in dugite |
|---|---|---|---|
| **Freeze** inputs at a defined instant | **YES** | implicit/accidental | correct (boundary) |
| **Observe** in-progress state | no (wire-visible) | absent | absent |
| **Spread** the work | no (performance) | absent | absent |

### 3.1 RUPD freeze — NOT a live divergence today, but structurally fragile

I set out to prove a consensus bug here and **disproved it.** The concern was
that `startStep` reads `casReserves` mid-epoch while dugite reads it at the
boundary. That difference is only observable if reserves move in between, and
they cannot:

- MIR certificates **queue** rather than move reserves. Haskell accumulates into
  `dsIRewards`; dugite accumulates into `certs.pending_mir_reserves` /
  `pending_mir_delta_reserves` (`eras/common.rs:685+`) and drains at the
  boundary. Verified: no `epochs.reserves` mutation on the cert-apply path.
- `applyRUpd` is itself the boundary.
- MIR is removed entirely at PV >= 9.

So dugite's boundary read is **numerically equal** to Haskell's 4k/f read.

**But the invariant is unstated and has already failed once.** The Byron→Shelley
AVVM return moves reserves at an era transition, and dugite carries a bespoke
compensation for exactly that:

```rust
// eras/shelley.rs:~464
// Haskell computed `nesRu` from PRE-AVVM reserves, so compute the reward
// update from reserves with that return removed
let reward_reserves = Lovelace(
    epochs.reserves.0.saturating_sub(std::mem::take(&mut epochs.pending_avvm_return)));
```

That is a per-case patch where Haskell gets the property structurally. It is the
N-copies/reactive-patch shape this repo keeps rediscovering (#985, #996, #1015).
The next mid-epoch reserves mutation — a new era's construct, a governance
mechanism, anything — silently diverges until someone notices.

`accounts` is frozen correctly (`state/apply.rs:~560`, gated on
`prev_protocol_version_major <= 6` because the prefilter is bypassed above that;
that gate is sound and documented from a live mainnet 365→366 divergence).

### 3.2 RUPD observability — a real, measured divergence (#1071)

dugite emits `enc.array(0)` unconditionally for `NewEpochState[4]`. cardano-node
emits a populated `Complete` for part of every epoch. Measured on the devnet at
slot 1166: 30 divergent key paths. Every prior release gate sampled outside the
window and passed.

### 3.3 Work distribution — unmeasured at scale, plausibly material

dugite computes all rewards in the boundary block. Observed: an 8 s tip-age
stall at a devnet boundary with **2 pools**. Mainnet has ~1.3M stake
credentials; Haskell deliberately spreads that over ~4k blocks. dugite's
preprod replay does thousands of boundaries at 65-81k blk/s, but replay is not
live block application and does not bound the live spike.

**This number does not exist yet and the design should not pretend otherwise.**
Phase 0 measures it.

### 3.4 DRep — no semantic gap

Inputs freeze at the same instant in both implementations, so
compute-immediately and compute-incrementally yield identical results. dugite
models only `DRComplete`. Missing: the `DRPulsing` constructor (wire + query)
and work spreading.

---

## 4. Design

### Phase 0 — measure before building (no shipping code)

Instrument the boundary and measure reward-computation wall time at preprod
scale (~1.2M credentials) and, if feasible, a mainnet replay. Deliverable: a
number, and a decision on whether Phase 3/5 (spreading) is justified or whether
freeze+observe suffices. Phases 1-2 proceed regardless — they are correctness
and observability, not performance.

### Phase 1 — make the freeze EXPLICIT (consensus-relevant)

Introduce `RewardSnapShot` as a real persisted type, captured at the existing
4k/f trigger site, carrying everything `startStep` freezes:

```rust
pub struct RewardSnapShot {
    pub fees: Lovelace,             // ssFee
    pub protocol_version: u64,      // rewProtocolVersion
    pub non_myopic: NonMyopic,      // rewNonMyopic  (already exists — v2.8.0)
    pub delta_r1: Lovelace,
    pub r: Lovelace,                // _R
    pub delta_t1: Lovelace,
    pub likelihoods: HashMap<Hash28, Likelihood>,   // already exists
    pub leaders: HashMap<Hash32, Vec<Reward>>,
}
```

Captured alongside it, as `FreeVars`: `addrs_rew` (already captured),
`total_stake`, `prot_ver`, `pool_reward_info`.

Ungate the `addrs_rew` capture from `prev_protocol_version_major <= 6` — it
becomes part of a record captured unconditionally.

**Delete `pending_avvm_return`.** With reserves frozen at 4k/f, the AVVM
compensation becomes structurally unnecessary. Its removal is the test that the
freeze is real: the mainnet Byron→Shelley boundary must stay byte-exact with
the patch gone.

`compute_reward_update` changes from "read live state" to "read the frozen
snapshot", with the boundary supplying only what Haskell's `completeRupd`
supplies.

### Phase 2 — the RUPD state machine and the `nesRu` wire arms

```rust
pub enum PulsingRewUpdate {
    Pulsing(RewardSnapShot, RewardPulser),
    Complete(RewardUpdate),
}
```

Implement the three-way `determineRewardTiming` in the per-block tick path, with
`start_step` / `pulse_step` / `complete_step`, and the force-complete branch.
Encode `NewEpochState[4]` as `StrictMaybe PulsingRewUpdate`:

- `SNothing` → `array(0)` (unchanged)
- `SJust (Pulsing s p)` → `array(1)[ array(3)[0, s, p] ]`
- `SJust (Complete r)` → `array(1)[ array(2)[1, r] ]`

Pin to a cardano-node capture, the way `snap_shot_bytes_match_*` and
`non_myopic_bytes_match_*` are pinned — **captured during the `Complete`
window**, i.e. between `first+4k/f` and the boundary. Capturing outside it
yields `array(0)` and would "confirm" the current hardcoding by accident. This
is the identical trap #1067's capture had at epoch < 3, and it must be called
out in the fixture's doc comment.

Closes #1071 and removes the `09w-ledger-state` `possibleRewardUpdate`
exclusion.

### Phase 3 — RUPD incremental pulsing (gated on Phase 0)

`pulse_size = max(1, ceil(num_stake_creds / (4 * k)))`; `done` when the balance
map is exhausted; `pulse_m` consumes `pulse_size` entries per block;
`complete_m` folds the remainder. `clearRecent` semantics on the accumulator
must be reproduced — the `RewardAns` "recent" map is cleared each pulse and is
what drives `RupdEvent`.

### Phase 4 — DRep `DRPulsing` + stepping

Add the `DRPulsing` variant carrying the 13 snapshot fields; step it on every
non-boundary tick; keep `DRComplete` as the consumed form. Because inputs
already freeze at the same instant, **this phase must be a provable no-op on
every ratification outcome** — that is its acceptance criterion, not a nice-to-
have.

### Phase 5 — persistence and import

Both pulsers become persisted ledger state → **SNAPSHOT_VERSION 38 → 39**, a
second consecutive re-sync release. Decode both from the Mithril ancillary
snapshot rather than skipping (the #1067 lesson: a skipped field means every
bootstrapped node starts blind).

---

## 5. Testing

Every phase lands with tests **proven RED by disarming the fix**. Specific to
this work:

- **Phase 1's decisive test**: delete `pending_avvm_return` and require the
  mainnet Byron→Shelley boundary to stay byte-exact. If the freeze is real the
  patch is redundant; if it is not, this goes red. This is the one test that
  distinguishes "explicit freeze" from "renamed the same accident".
- **A mid-epoch reserves mutation must not move the RUPD.** Synthesise one
  (even if unreachable today) and assert the reward update is unchanged. This is
  the regression guard for the fragility in §3.1 — it fails today.
- **Phase 3 equivalence**: for the same frozen inputs, pulsed and single-pass
  computation must produce byte-identical `RewardUpdate`. Property test over
  pulse sizes, including `pulse_size = 1` and `>= num_creds`.
- **Phase 4 no-op**: replay preview from genesis (733 Conway boundaries) and
  require the same 14 enactment boundaries, same `planned_at`, before and after.
- **Wire fixtures** for `Pulsing` and `Complete` from a real node capture, with
  the capture-window trap documented.

Beware the #1067 lesson in reverse: a RED-proven unit test bounds the FUNCTION,
not the SYSTEM. Phase 3's equivalence property is a unit-level claim; the
preview replay is what bounds the system.

## 6. Validation gates

- `just check` green at every phase
- devnet-validate standard, 4/4, `gate_integrity.admissible = true`, with the
  `possibleRewardUpdate` exclusion REMOVED after Phase 2
- preview replay from genesis: 733 boundaries, pots byte-exact vs Koios
- preprod soak ≥ 60 min including the 38 → 39 re-sync
- Phase 0's number re-measured after Phase 3 to show the spike is actually gone

## 7. Risks

1. **This is the reward path.** #966, #988, #949 and #991 were all consensus
   bugs found in exactly this surface, three of them about *which snapshot a
   term is read from*. Phase 1 changes precisely that. It is the highest-risk
   change in the plan and deserves the preview replay before anything else
   merges.
2. **Two consecutive re-sync releases** (37→38 in v2.8.0, 38→39 here) is
   operationally unfriendly. Consider holding v2.8.0 unreleased and shipping
   both bumps as one 37→39 — decision needed before v2.8.0 is tagged.
3. **Phase 3 may not be justified.** If Phase 0 shows the boundary spike is
   small at mainnet scale, incremental pulsing buys only structural symmetry at
   real complexity cost. Deciding that on measurement rather than aesthetics is
   the point of Phase 0.
4. **Scope creep into `RewardAns`/`RupdEvent`.** Haskell emits reward events per
   pulse for db-sync. dugite has no consumer. Explicitly OUT of scope; do not
   implement events without a consumer.

## 8. Explicitly out of scope

- `InstantStake` (not a pulser; already incremental)
- `RupdEvent` / `DeltaRewardEvent` emission (no consumer in dugite)
- #1068 (LSQ acquisition snapshot) — unrelated, tracked separately
