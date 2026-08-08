# Pulser alignment — bringing dugite's RUPD and DRep pulsers to 100% structural parity

Status: **DESIGN — revision 2, post adversarial review. Not approved for implementation.**
Date: 2026-08-08
Tracks: #1071 (nesRu hardcoded SNothing), **#1072 (CONSENSUS — reward update
applied unconditionally)**.

Revision 2 incorporates an adversarial review. Five substantive corrections are
marked **[R2]**; the two load-bearing claims (reserves immobility, DRep
freeze-equivalence) survived the attack unchanged. The review found a live
consensus divergence revision 1 missed (#1072) and a decisive test revision 1
wrote that **could not go red** — both recorded below rather than quietly fixed.

Sources are the cardano-ledger revisions cardano-node 11.0.1 pins — CHaP pins
per PACKAGE, not per monorepo tag:

- `cardano-ledger-shelley` 1.18.1.0 → `b7c17cf31871062b7883c46e3f367cb5e1b5db6c`
- `cardano-ledger-core` 1.20.0.0 / `-binary` 1.8.1.0 → `94e9618c91a16ec08db477632a158b630722089b`

---

## 1. Sweep result

**[R2 — method corrected.]** A repo-wide search finds **four** `instance
Pulsable`, not two:

| Instance | Location | Status |
|---|---|---|
| `Pulsable PulseListM` | `libs/cardano-data/src/Data/Pulse.hs` | library scaffolding, **no production use** |
| `Pulsable PulseMapM` | `libs/cardano-data/src/Data/Pulse.hs` | library scaffolding, **no production use** |
| `Pulsable RewardPulser` | `RewardUpdate.hs:300` | RUPD — persisted in ledger state |
| `Pulsable (DRepPulser era)` | `DRepPulser.hs:299` | DRep — persisted in ledger state |

So the set of pulsers **persisted in ledger state** is two, and that is the set
this spec addresses. Revision 1 claimed "exactly two, verified by grepping every
fetched source" — that was verification theatre: a hand-picked 13-file download
cannot establish a "contains exactly N" claim about a repository. The conclusion
survived; the method did not. Recorded because the method is what gets reused.

Dijkstra reuses Conway's DRepPulser unchanged, and dugite's Dijkstra delegates
NEWEPOCH/EPOCH to Conway wholesale (`eras/dijkstra.rs:388-403`).

Adjacent mechanisms a `Pulsable` grep cannot see, checked and out of scope:
`predictFuturePParams` (per-tick deferred, dugite has it — #977);
`ssStakeMark`/`ssStakeMarkPoolDistr` lazy memoisation (ADR-7; dugite eager, same
values); `InstantStake` (incremental but not pulsed — consumed whole at SNAP,
`SnapShots.hs:418`).

---

## 2. What Haskell actually does

### 2.1 RUPD — the only pulser whose inputs freeze MID-EPOCH

`ShelleyRUPD` fires from TICK (`Tick.hs:274-277`). `sr = randomnessStabilisationWindow` (4k/f):

| phase | condition | action |
|---|---|---|
| TooEarly | `s <= first + sr` | `SNothing` — pulser stays unset |
| JustRight | `first + sr < s <= first + 2sr` | `SNothing` → `startStep`; `Pulsing` → one `pulseStep`; `Complete` → hold |
| TooLate | `s > first + 2sr` | `SNothing` → **`startStep` then immediately `completeStep`**; `Pulsing` → `completeStep`; `Complete` → hold |

**[R2]** The TooLate `SNothing` case (`Rupd.hs:152-157`) was omitted in revision 1.
It matters: it is a second freeze instant, later than `first+sr`.

`startStep` (`PulsingReward.hs:99`) freezes, at that mid-epoch slot:
**`casReserves`, `accounts`, `ssStakeGo`, `prevPParams`, `ssFee`, `BlocksMade`**,
plus constants `slotsPerEpoch`/`asc`/`k`/`maxSupply`. `pulseSize = max 1 (ceil(numStakeCreds / 4k))`.

Types (`RewardUpdate.hs`): `PulsingRewUpdate = Pulsing RewardSnapShot Pulser | Complete RewardUpdate`;
`RewardSnapShot { rewFees, rewProtocolVersion, rewNonMyopic, rewDeltaR1, rewR, rewDeltaT1, rewLikelihoods, rewLeaders }`;
`RSLP Int FreeVars (VMap Credential StakeWithDelegation) RewardAns`;
`FreeVars { fvAddrsRew, fvTotalStake, fvProtVer, fvPoolRewardInfo }`.

Wire and JSON:

```haskell
encCBOR (Pulsing s p) = encode (Sum Pulsing 0 !> To s !> To p)
encCBOR (Complete r) = encode (Sum Complete 1 !> To r)

instance ToJSON PulsingRewUpdate where
  toJSON = \case
    Pulsing _ _ -> Null          -- renders the SAME as SNothing
    Complete ru -> toJSON ru
```

A `Pulsing` pulser is JSON-invisible. That is why #1071 measures ~20% and not
~80%: on the devnet (`epochLength=400`, `k=40`, `f=0.5`, `sr=320`) the pulser
starts just after slot 320, completes within ~10-20 slots (`pulseSize=1`, few
credentials), and is JSON-visible only from ~slot 335 to 400. **`slotForce = first+640 > 400`,
so the TooLate arm is unreachable on the devnet** — the reachable completion path
there is NEWEPOCH's `Pulsing → completeRupd` branch.

### 2.2 DRep — inputs freeze AT THE BOUNDARY

`setFreshDRepPulsingState` (`ConwayGovernance.hs:460`) runs at the boundary and
snapshots **14** fields **[R2 — was 13; `dpGlobals` omitted]**. Stepped per
non-boundary tick (`ConwayNewEpoch.hs:166-170`). Pulse size: `max 1 (numAccounts / 4k)`.

**[R2] There is no `DRPulsing` wire form.** The encoder force-completes:

```haskell
encCBOR (DRComplete x y) = encode (Rec DRComplete !> To x !> To y)
encCBOR x@(DRPulsing (DRepPulser {})) = encode (Rec DRComplete !> To snap !> To ratstate)
  where (snap, ratstate) = finishDRepPulser x

decCBOR = decode (RecD DRComplete <! From <! From)   -- accepts ONLY DRComplete
```

The in-progress DRep state is externally unobservable — CBOR and JSON both
always show the completed form. Revision 1's "missing: the DRPulsing constructor
(wire + query)" was **wrong**, and emitting a `DRPulsing` wire arm would be a
#948-class self-undecodable defect.

---

## 3. Gap analysis

| Property | Consensus? | RUPD | DRep |
|---|---|---|---|
| **Freeze** inputs at a defined instant | **yes** | implicit/accidental | correct (boundary) |
| **Fire conditionally** on a pulser existing | **yes** | **WRONG — #1072** | n/a |
| **Observe** in-progress state | wire-visible | absent (#1071) | n/a — no wire form |
| **Spread** the work | performance | absent | absent |

### 3.1 Frozen inputs — the reserves route is CLEAN

Revision 1 hypothesised a divergence because `startStep` reads `casReserves`
mid-epoch while dugite reads it at the boundary. **That hypothesis is refuted,
and the refutation survived adversarial attack.** The complete upstream write
set of `casReserves` is: `applyRUpd` (boundary), the MIR rule (boundary, after
`updateRewards` — `NewEpoch.hs:161-169`), `returnRedeemAddrsToReserves`
(Shelley→Allegra translation, at the boundary), and genesis/init. Conway pot
movements (`applyEnactedWithdrawals`, donations, unclaimed deposits, POOLREAP)
touch `casTreasury`/accounts only. MIR certificates **queue** into `dsIRewards`
(dugite: `certificates.rs:728`, drained at `certificates.rs:86-179` from
`shelley.rs:585` — *after* the RUPD, same order as Haskell). MIR is gone at PV>=9.

So reserves cannot move in `(first+sr, epoch_end]` in either implementation,
Shelley→Conway. **Dijkstra is inherited-by-delegation, not independently
verified — say so, do not imply otherwise.**

The other frozen inputs are all provably stable mid-epoch **[R2 — revision 1
never analysed these]**: `ssStakeGo` and `ssFee` are written only by SNAP
(`Snap.hs:96-103`), `prevPParams` only at EPOCH (`Epoch.hs:183`), `BlocksMade`
(`nesBprev`) only by NEWEPOCH (`NewEpoch.hs:193-194`), `rewNonMyopic` only by
applyRUpd. `accounts` is the sole mid-epoch-mutable input, and it is handled
(`apply.rs:558-580`, gated PV<=6 because the prefilter short-circuits above —
`Rewards.hs:315`).

**The fragility is real even though the values agree.** dugite depends on an
unstated invariant that Haskell gets structurally, and that invariant has
already failed once: `pending_avvm_return` is a bespoke compensation for the
Shelley→Allegra AVVM return. Per `substates.rs:200-216` the un-compensated
divergence was **mainnet ep236: −561K ADA reserves / +184K ADA treasury**.

### 3.2 [R2] #1072 — a LIVE consensus divergence revision 1 missed

Revision 1 concluded "not a live divergence today". **That was wrong.** The
reserves route is clean; a different route through the same gap is not.

```haskell
-- NewEpoch.hs:161-167, identically ConwayNewEpoch.hs:172-178
es' <- case ru of
  SNothing -> pure es          -- NO reward update applied, at all
  ...
```

`nesRu` is `SNothing` at a boundary whenever **no block landed in
`(first + 4k/f, epoch_end]`**. dugite applies a full reward update at every
boundary regardless (`shelley.rs:443`, `conway.rs:630`; only gate is
`is_byron_to_shelley_fork`). Pots then diverge permanently.

Unreachable on a healthy mainnet (~8640 expected blocks in the window);
genuinely reachable on the devnet, where the window is **80 slots** and both the
chaos suite's SIGKILL and Round 3's deliberate outage exceed it. Filed as #1072
with its own regression test, which fails today.

### 3.3 DRep — no semantic gap

Inputs freeze at the same instant in both implementations, so
compute-immediately and compute-incrementally are extensionally equal.
`dpStakePoolDistr`'s documented laziness is a thunk over an immutable value; in
a pure language it cannot observe a different value than an eager snapshot of
the same inputs. **[R2]** Combined with §2.2 (no wire form), Phase 4 is reduced
to an internal-representation change with no external justification — see Phase 4.

### 3.4 Work distribution — unmeasured

One 8 s tip-age stall observed at a devnet boundary with **2 pools**. Mainnet has
~1.3M credentials. **That number does not exist yet and this design does not
pretend otherwise** — Phase 0 produces it.

---

## 4. Design

### Phase 0 — measure before building (no shipping code)

Instrument the boundary; measure reward-computation wall time at preprod scale
and, if feasible, mainnet replay. Deliverable: a number, and a go/no-go on
Phase 3. Phases 1-2 proceed regardless — they are correctness, not performance.

### Phase 1 — make the freeze EXPLICIT (consensus-relevant)

Capture a real `RewardSnapShot` + `FreeVars` at the existing 4k/f trigger,
carrying everything `startStep` freezes. Design constraints **[R2]**:

- **Keep the `prev_protocol_version_major <= 6` gate on `addrs_rew`.** Revision 1
  proposed ungating it for symmetry. At PV>=7 the set is dead upstream
  (`Rewards.hs:315`); ungating would serialise ~1.3M credentials into every
  mainnet snapshot for zero semantic payoff. Where Haskell keeps the set inside
  the pulser, dugite may too — but it must not become unconditional persisted
  state.
- **`rewLeaders`/`rs` are `Set Reward` upstream, not a list.** `filterRewards` at
  PV<3 selects via `Set.deleteFindMin` (`Rewards.hs:176-181`) — Ord-minimum
  semantics — and aggregation dedups by Set union. A Rust `Vec` must reproduce
  ordering and dedup exactly or the mainnet Shelley-era replay diverges.
- **The delivery filter stays live-at-boundary.** `applyRUpdFiltered` routes by
  *boundary-time* registration (unregistered → treasury). Freeze the inputs, not
  the delivery decision. dugite already does this (`shelley.rs:531-546`); the
  "read the frozen snapshot" framing must not erode it.
- **The frozen snapshot must survive `on_era_transition`**, which runs before
  `process_epoch_transition` (`apply.rs:366-398`).

Then **delete `pending_avvm_return`** — including its `LedgerDelta` restore
(`ledger_seq.rs:1689-1693`) and its v38 snapshot field (`snapshot_format.rs:236`)
**[R2 — neither was named in revision 1]**.

### Phase 2 — the RUPD state machine, #1072, and the `nesRu` wire arms

```rust
pub enum PulsingRewUpdate { Pulsing(RewardSnapShot, RewardPulser), Complete(RewardUpdate) }
```

Implement all three timing phases **including the TooLate `SNothing` →
start-then-complete case**, and the NEWEPOCH consumption arms — `SNothing → no
update` (**this is the #1072 fix**), `Pulsing → completeRupd`, `Complete → apply`.

**[R2]** On the devnet the TooLate arm is unreachable (`first+2sr = 640 > 400`)
while the `Pulsing → completeRupd` arm is the *normal* path. So the devnet gate
cannot exercise force-complete; it needs unit/config-level coverage, and §6 must
not claim otherwise.

Encode `NewEpochState[4]` as `StrictMaybe PulsingRewUpdate`: `SNothing` →
`array(0)`; `SJust (Pulsing s p)` → `array(1)[array(3)[0,s,p]]`; `SJust (Complete r)`
→ `array(1)[array(2)[1,r]]`. Pin to a node capture taken **inside the `Complete`
window** — outside it the reply is `array(0)` and would "confirm" the current
hardcoding by accident, the identical trap #1067's capture had before epoch 3.

Closes #1071 and #1072; removes the `09w-ledger-state` exclusion.

### Phase 3 — RUPD incremental pulsing (gated on Phase 0)

`pulse_size = max(1, ceil(num_stake_creds / (4k)))`; `done` when the balance map
is exhausted; `pulse_m` consumes `pulse_size` per block; `complete_m` folds the
rest. Reproduce `clearRecent` on the accumulator.

### Phase 4 — DRep internal pulsing **(justification reduced [R2])**

Add `DRPulsing` as an **internal representation only**. There is no wire arm to
emit and no query to serve (§2.2), and no semantic gap to close (§3.3). Its sole
remaining value is work-spreading, so **Phase 4 is gated on Phase 0 exactly as
Phase 3 is**, and is the first thing to cut if the measurement does not justify
it. Acceptance criterion is a provable no-op on every ratification outcome.

### Phase 5 — persistence, rollback, and import

**[R2] Rollback and LedgerSeq integration — the largest omission in revision 1.**
Phase 1 creates consensus-bearing state captured *mid-epoch*. A fork switch
across the capture slot must undo or re-capture it. Precedent exists:
`rupd_addrs_rew_snapshot` in `LedgerDelta` (`ledger_seq.rs:287-293, 1263-1264`).
This must cover the bulk-replay and re-anchor paths too — #985's entire lesson
was that `LedgerSeq` state moved without deltas. A rollback that reconstructs
state with a post-fork `RewardSnapShot` yields a wrong reward update at the next
boundary, silently.

**[R2] Retire what Phase 2 subsumes**: `is_byron_to_shelley_fork`
(`shelley.rs:441-443`) and the legacy `epochs.pending_reward_update` apply path
(`shelley.rs:385-416`, `conway.rs:563+`) — including the migration case where the
legacy field is `Some` *and* the new machine holds a pulser (double-apply risk).

Decode both pulsers from the Mithril ancillary snapshot rather than skipping.

**ONE replay for operators.** v2.8.0 is committed but not tagged and not pushed
(verified: no `v2.8*` tag; 9 commits ahead of `origin/main`), so no artefact
carries SNAPSHOT 38 with the #1067-only layout. This work therefore extends
SNAPSHOT 38 in place rather than bumping to 39.

**[R2] That premise is a standing obligation, not a checkable fact.** CLAUDE.md
requires pushing after each iteration; holding 9 commits unpushed across a
multi-phase program contradicts the repo's own process, and any routine push
voids the plan. Mitigations, in order of preference:
1. An `xtask` guard that fails the build if a `v2.8*` tag exists while the
   pulser fields are absent from `LedgerStateSnapshot` — makes it checkable.
2. Push the branch but do NOT tag; tagging is what creates operator artefacts.
3. Accept 38→39 and ship v2.8.0 now. Costs operators one extra replay but
   removes the tripwire and unblocks a finished, gate-validated #1067.

The failure mode if violated is loud, not silent: a layout mismatch fails the
sequential decode → snapshot rejected → full replay. Cost, not corruption.

---

## 5. Testing

Every phase lands with tests **proven RED by disarming the fix**.

**[R2] Revision 1's decisive test was wrong and could not have gone red.** It
proposed deleting `pending_avvm_return` and requiring the mainnet
**Byron→Shelley** boundary to stay byte-exact. But the AVVM return is at
**Shelley→Allegra** (`shelley.rs:55-67`, `Cardano.Ledger.Allegra.Translation`),
and dugite **skips the RUPD entirely at Byron→Shelley**
(`is_byron_to_shelley_fork`, `shelley.rs:441-443`) — so the test passes trivially
with the patch disarmed, while "validating" the deletion of a compensation worth
−561K ADA reserves / +184K ADA treasury at mainnet ep236. Recording this because
writing an un-reddable test is the precise failure this repo's discipline exists
to prevent.

Corrected decisive tests:

1. **Phase 1**: delete `pending_avvm_return`, replay **mainnet epoch 235→236**
   (Shelley→Allegra) and require byte-exact pots. Preprod/preview have no AVVM
   balances and cannot substitute.
2. **#1072**: synthesise an epoch with no block after `4k/f`; assert the boundary
   applies NO reward update — pots unchanged, no rewards credited, `utxosFees`
   not drained. Fails today.
3. **Mid-epoch reserves mutation must not move the RUPD.** Synthesise one even
   though unreachable today; this is the regression guard for §3.1's fragility.
4. **Phase 3 equivalence**: pulsed and single-pass must produce byte-identical
   `RewardUpdate` for the same frozen inputs. Property test over pulse sizes
   including 1 and `>= num_creds`.
5. **Force-complete arm**: unit/config-level, since the devnet cannot reach it.
6. **`rewLeaders` ordering/dedup** against `Set Reward` semantics at PV<3.
7. **Rollback across the capture slot** re-captures or invalidates correctly.
8. **Phase 4 no-op**: preview replay, same 14 enactment boundaries, same
   `planned_at`, before and after.
9. **Wire fixtures** for `Pulsing` and `Complete`, with the capture-window trap
   documented in the fixture.

Unit coverage is required on every new type and transition — not only the
end-to-end replays. Note the #1067 lesson in reverse: a RED-proven unit test
bounds the FUNCTION, not the SYSTEM; the replays are what bound the system.

## 6. Validation gates

- `just check` green at every phase
- devnet-validate standard, 4/4, `gate_integrity.admissible = true`, with the
  `possibleRewardUpdate` exclusion REMOVED after Phase 2
- **mainnet replay through epoch 236** — the only gate that can see the AVVM case
- preview replay from genesis: 733 Conway boundaries, pots byte-exact vs Koios
- preprod soak >= 60 min including the re-sync
- Phase 0's number re-measured after Phase 3

## 7. Risks

1. **This is the reward path.** #966, #988, #949, #991 were all consensus bugs
   here, three about *which snapshot a term is read from* — exactly what Phase 1
   changes. Preview replay before anything merges; mainnet replay before Phase 1
   is called done.
2. **Rollback of mid-epoch consensus state** (Phase 5) is the subtlest part and
   has #985 precedent for going wrong silently.
3. **Phase 3/4 may not be justified.** Decide on Phase 0's measurement.
4. **SNAPSHOT-38 standing obligation** — see Phase 5.

## 8. Explicitly out of scope

- `InstantStake`; `PulseListM`/`PulseMapM` (no production use)
- `RupdEvent` / `DeltaRewardEvent` emission (no consumer in dugite)
- #1068 (LSQ acquisition snapshot)
