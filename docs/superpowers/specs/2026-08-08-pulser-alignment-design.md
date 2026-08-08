# Pulser alignment — bringing dugite's RUPD and DRep pulsers to 100% structural parity

Status: **revision 4 — Phases 0, 1a, 1b, 2, 3 and 4 CLOSED; Phase 5's rollback
half LANDED. Only the `nesRu` wire arms (#1071) remain.**
Date: 2026-08-08

| Phase | State |
|---|---|
| 0 — measure the boundary fold | **DONE.** ~2.55 s at mainnet scale, linear ⇒ **GO** on 3 (§3.4) |
| 1a — explicit `startStep` freeze | **DONE**, pot-parity validated |
| 1b — retire `pending_avvm_return` | **DONE** via frozen `fvTotalStake` — the spec's own instruction was under-specified, see below |
| 2 — RUPD state machine + #1072 | **DONE.** Wire arms MOVED to 3 — they need a live fold |
| 3 — incremental pulsing | **DONE** — credential-major fold, incremental `RewardFold`, per-block scheduling. Only the `nesRu` wire arms remain (#1071) |
| 4 — DRep internal pulsing | **CLOSED as YAGNI** — measured unobservable, not asserted |
| 5 — persistence + rollback | **DONE.** Freeze pair atomic; the fold is deliberately transient (no snapshot change). `is_byron_to_shelley_fork` documented as subsumed, retained — untestable here |

Three claims in revision 2 were wrong and are corrected in place rather than
quietly edited: the Phase 2/3 ordering, `FreeVars[0]` read as the fold's work
queue, and Phase 4's "no wire form" asserted rather than measured. Each is
marked where it appears. The pattern is worth more than the corrections: all
three were plausible readings of a real artefact, and all three fell to
decoding or sampling the whole thing instead of its head.
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

### 3.4 Work distribution — MEASURED 2026-08-08

One 8 s tip-age stall was observed at a devnet boundary with **2 pools**, and
mainnet has ~1.3M credentials. Phase 0 has now produced the real number
(`state::rupd_work_measurement`, release build, M-series):

| creds | pools | wall | ns/cred |
|---:|---:|---:|---:|
| 1,000 | 50 | 4.8 ms | 4828 |
| 10,000 | 500 | 26.9 ms | 2688 |
| 50,000 | 1,500 | 90.1 ms | 1803 |
| 200,000 | 3,100 | 392.2 ms | 1961 |

**Extrapolated to mainnet (1.3M creds / 3.1k pools): ~2.55 s inside ONE
boundary block.** Scaling is linear (1.13x per-credential over a 10x size
increase), so the extrapolation is sound.

A 2.55 s fold overruns a 1 s slot by 2.5x. It does not stall the chain — mainnet
blocks arrive ~20 s apart — but it freezes the node for the boundary block, and
a producer whose leader slot lands in that window forges late or not at all.
**Phase 3 is justified on work distribution**, independently of `nesRu` parity.

> **The first version of this measurement said 0.29 s and would have retired
> Phase 3.** It used a fixed 1000 ADA per credential, which against a 37.2B
> circulation made per-pool `sigma` ~5e-10, floored `maxPool'` to zero, and
> dropped every MEMBER reward — the fold returned exactly one entry per pool
> and the timing described a loop over 50 pools while claiming to describe
> 1000 credentials. `time_fold` now asserts the fold rewarded at least half the
> input credentials, and `TOTAL_DELEGATED` is held constant across scales at
> ~65% of circulation. The assertion is what changed the conclusion, and it is
> the same failure family as #916/#917/#945: a check reporting a clean number
> while measuring nothing.

---

## 4. Design

### Phase 0 — DONE (measured 2026-08-08): **GO on Phase 3**

Deliverable was a number and a go/no-go. Both are in §3.4: **~2.55 s** for the
mainnet-scale fold inside one boundary block, linear in credentials, against a
1 s slot. That is a go.

Measured by scaling a synthetic snapshot rather than by instrumenting a mainnet
replay — the replay route is blocked on disk (§5b), and the synthetic route
turned out to be the better instrument anyway, since it isolates the fold from
block application and can be re-run in half a second on any machine.

`--release` is required; a debug number overstates the stall ~10x. The two
measurement tests are `#[ignore]`d, because a wall-clock assertion in CI
measures runner load — the flake shape already hit in `dugite-monitor`'s probe
timeout. The one automatic assertion is scale-relative (not super-linear),
which catches a real algorithmic regression without pinning a wall clock.

### Phase 1a — DONE (validated 2026-08-08)

Split out from Phase 1 because only 1b is blocked; treating them as one unit
was an error that stalled work unnecessarily.

Landed as four RED-proven increments: the frozen-input types
(`RewardSnapShot`/`FreeVars`/`RewardEntry`, with `ord_key` reproducing
`Set.deleteFindMin`); `start_step_monetary` as a SHARED function so the pulser
and the current path cannot drift; the capture at the 4k/f mark into
`EpochSubState.rupd_monetary`, persisted; and the boundary consuming it.

Validated: **POT PARITY OK, byte-exact vs cardano-node**
(`treasury=3347998634108 reserves=5996646011276436`), all predicates,
tip-parity 176/176, NO ANOMALIES. That equivalence is the entire safety
argument for the relocation, and it is now measured rather than argued.

Carries a `debug_assert_eq!` that the frozen `deltaR1` equals the
boundary-recomputed value, so if an input ever does begin moving mid-epoch,
debug builds fail loudly instead of silently producing a different update.

### Phase 1b — BLOCKED (see section 5b)

Deleting `pending_avvm_return`. Redundant in principle now that the freeze is
structural; unproven in fact until a mainnet ep235->236 replay runs.

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

### Phase 2 — status and the exact next step

**Landed**: `PulsingRewUpdate { Pulsing(RewardSnapShot) | Complete(RewardSnapShot) }`
with `Option::None` for `SNothing` — a three-state space where #1072 shipped a
bool. Closes F5's representation gap. Tests pin that BOTH constructors apply at
the boundary (only `None` does not), that `complete` is idempotent and
preserves the freeze, and that `Pulsing` is JSON-invisible.

**Not landed**: the swap of `rupd_pulser_started`/`rupd_monetary` for
`Option<PulsingRewUpdate>`, and the `nesRu` wire arms.

**The next increment is blocked on a capture, not on code.** The wire shape is

```text
SNothing              -> array(0)                          (dugite emits this today)
SJust (Pulsing s p)   -> array(1)[ array(3)[0, s, p] ]
SJust (Complete r)    -> array(1)[ array(2)[1, r] ]
```

and it must be PINNED to cardano-node bytes rather than derived from the
Haskell types — the `SnapShot` record (#1057) and `NonMyopic` (#1067) both
proved that reading a shape off `deriving EncCBOR` produces a plausible wrong
answer. `RewardSnapShot` alone has eight fields including a `NonMyopic` and a
`Map (Credential Staking) (Set Reward)`; guessing its framing is not an option.

**Capture procedure**, and the trap it must avoid:

1. Bring up the devnet; wait for an epoch to pass slot `first + 4k/f` (320).
2. Wait a further ~15-20 slots for the pulser to COMPLETE (`pulseSize = 1` with
   few credentials), i.e. sample around slots 340-395 of the epoch.
3. Capture `NewEpochState[4]` from cardano-node's socket.

Capturing OUTSIDE that window yields `array(0)` for `SNothing` — or, between
320 and ~335, a `Pulsing` whose JSON is `null` and which is easy to mistake for
`SNothing`. Either would "confirm" the current hardcoding by accident. This is
the identical trap #1067's capture had before epoch 3, and the reason
`is_json_visible()` exists on the type: **the JSON cannot be used to decide
whether the capture landed in the right window.** Use the CBOR.

**CORRECTION, measured 2026-08-08.** The prediction above — `Pulsing` in slots
~321-335 — is WRONG. A capture at slot 324, four slots past the mark, is
already `Complete`:

```text
slot 120  80                        SNothing
slot 324  818201850000a00082a000    Complete   <- expected Pulsing here
slot 353  818201850000a00082a000    Complete
```

`pulseSize = max 1 (ceil(numStakeCreds / 4k))` is 1 on the devnet and there are
a handful of credentials, so the fold completes in a SINGLE pulse — on the first
block past the mark. `Pulsing` exists for at most one block and cannot be pinned
from this network.

So the `Complete` arm is captured (`tests/fixtures/nesru/`) and the `Pulsing`
arm is NOT. It needs a network with enough stake credentials for the fold to
span blocks, or a fixture built from the Haskell encoder and labelled SYNTHETIC.
Shipping a guessed `Pulsing` encoding as though it were observed is the
#1057/#1067 mistake.

**RESOLVED, 2026-08-08.** `pulseSize` stays 1 until `numStakeCreds > 4k = 160`,
so ~100 credentials give ~100 pulses over ~200 slots against an 80-slot window
— `Pulsing` then holds from the mark to the boundary. 120 registered, delegated
and funded credentials were seeded (40 per tx; 120 in one tx is 32488 bytes
against `maxTxSize` 16384) and both arms are now captured from cardano-node:

```text
slot 1122 (offset 322)  SJust Pulsing    8183 0088 …   7.4 KB
slot 1165 (offset 365)  SJust Complete   8182 0185 …   2.6 KB
```

### Phase 2/3 ORDERING — corrected by the capture

The plan had the `nesRu` wire arms in Phase 2 and incremental pulsing in Phase
3. **That order is not achievable**, and decoding the `Pulsing` fixture is what
shows it:

```text
array(3)[ 0, RewardSnapShot(8) = 1178 B, Pulser = 6204 B ]

Pulser = array(4)[ pulseSize=1, FreeVars 4688 B, balance 1369 B, RewardAns 145 B ]
  FreeVars  = array(4)[ fvAddrsRew (tag-258 set, 140), fvTotalStake,
                        fvProtVer, fvPoolRewardInfo (map 1) ]
  balance   = map(19) Credential -> CompactCoin      <- work remaining
  RewardAns = array(2)[ map(1), map(1) ]             <- answer so far
```

The `Pulser` is **84% of the record**, and two of its four fields — `balance`
and `RewardAns` — are live fold state that exists only while a fold is in
progress. A node that computes the whole update at the boundary has neither, so
there is nothing to encode from. The `Pulsing` arm is therefore a CONSEQUENCE
of incremental pulsing, not a precondition for it.

> **Correction.** The first reading of this fixture called `FreeVars[0]` "the
> work queue". It is `fvAddrsRew`, the pv<=6 registration prefilter, and it
> holds **140** entries where the real queue holds **19** — both are credential
> sets inside the pulser, which is what made the misreading easy, and the
> counts are what separate them. 140 is the 120 seeded credentials plus genesis
> accounts in the LIVE registration set; 19 is what the GO snapshot carries in
> epoch 2, since delegations registered in epoch 0 reach `go` only after two
> boundaries. A set whose size tracks live registrations cannot be the queue of
> a fold over a frozen snapshot. The conclusion survives the correction and is
> stronger for it: two fields require a live fold, not one.

The `Complete` arm has the same dependency from the other side: upstream's
`Complete` carries a `RewardUpdate` (`deltaT, deltaR, rs, deltaF, nonMyopic`),
whereas dugite's `Complete` carries the `RewardSnapShot` it froze — a Phase-1
approximation that is adequate for boundary application (dugite recomputes
there) but has no `rs` map to emit. And the timing differs regardless: upstream
reaches `Complete` mid-epoch, ~35 slots before the boundary, where dugite would
still report `Pulsing`.

**So Phase 3 subsumes the wire arms.** Emitting them earlier could only produce
a fabricated `Pulser`, which is worse than the honest `SNothing` dugite emits
today: a wrong `nesRu` is a wrong `NewEpochState`, and #1057/#1067 are both
cases of a plausible invented shape reaching the wire. The arms land when the
state they describe exists — with `tests/fixtures/nesru/` as the oracle, which
is the state this work leaves them in.

### Phase 3 — RUPD incremental pulsing — **GO** (Phase 0 cleared it, §3.4)

`pulse_size = max(1, ceil(num_stake_creds / (4k)))`; `done` when the balance map
is exhausted; `pulse_m` consumes `pulse_size` per block; `complete_m` folds the
rest. Reproduce `clearRecent` on the accumulator.

**The structural change is the loop's major axis.** dugite folds POOL-major —
`for (pool_id, stake) in &go.pool_stake` with an inner `for cred_hash in
delegators` (`rewards.rs:533`, `:742`). Upstream's work queue is
CREDENTIAL-major: the captured pulser's remaining set is a tag-258 set of
`8200581c…` items, i.e. `array(2)[0, bytes(28)]` = `Credential 'Staking`.
Chunking a pool-major loop at pool granularity would NOT match — one pool can
hold hundreds of thousands of delegators, so a "pulse" could be arbitrarily
large and the `Pulsing` wire arm would still not reproduce upstream's queue.
The fold has to be re-expressed credential-major, with the per-pool terms
(`maxPool'`, `sigma`, leader reward) precomputed once into a per-pool table
that the credential fold then reads — which is what upstream's `FreeVars` +
`PoolRewardInfo` pair already is.

**The safety gate is a differential property, not a replay.** Incremental and
batch must produce byte-identical output from identical frozen inputs, which is
directly assertable and cheap:

```text
fold_incremental(frozen, pulse_size = 1)   ==  fold_batch(frozen)
fold_incremental(frozen, pulse_size = 7)   ==  fold_batch(frozen)
fold_incremental(frozen, pulse_size = n)   ==  fold_batch(frozen)
```

Property-tested over random snapshots and pulse sizes, this pins the ONLY
correctness claim Phase 3 makes — the restructuring changes when work happens,
never what it computes. It also fails loudly on the specific hazard of a
credential-major rewrite: a per-pool term (`maxPool'` denominators, the pledge
check, `clearRecent`) accidentally recomputed per credential from mutating
state instead of read from the frozen table.

Run the preview replay (733 Conway boundaries, `db-preview` is on disk) as the
end-to-end gate on top, per the standing rule that ledger changes are gated on
it — but the differential property is what makes the change reviewable, since
a replay that passes tells you the answers matched without telling you the
frozen table was actually frozen.

**Ledger half LANDED and validated.** The credential-major restructure, the
incremental `RewardFold`, and the differential gate are in. Production runs
THROUGH the fold with a single maximal pulse, so there is no batch path beside
the pulse path to drift (the N-copies trap of #985/#932/#938, with a
consensus-critical fold inside it).

Live validation, devnet Round 1 against cardano-node 11.0.1:

```text
verify.sh          p1/p2/p4/p5 PASS   tip-parity 24/24 = 100%
node logs          0 ERROR on all three nodes
adversarial N2N    26/26          UTxO RPC   0 ERROR
pots (tip-pinned, block 895, after a RUPD boundary)
  treasury  10559788876077   IDENTICAL on both nodes
  reserves  5989391426218560 IDENTICAL on both nodes
```

Two zoo scripts failed in that round (04i, 11e) and both PASSED on re-run
against the same devnet and binary — flakes, not regressions. Verified rather
than assumed: they had passed 151/0/154 in v2.7.1 and in this release's earlier
gate, so a shrug was not available.

**Still open in Phase 3**: per-block pulse scheduling (the fold runs at the
boundary in one pulse, not spread across the epoch) and the `nesRu` wire arms
that depend on it. It is a restructuring of a consensus-critical fold
and belongs in its own release with its own gate, not appended to one that
already carries a validated consensus fix (#1072). Everything it needs is in
place: the freeze (Phase 1a), the measurement (Phase 0), and the wire fixtures
(`tests/fixtures/nesru/`) that its `Pulsing` arm must reproduce.

### Phase 4 — DRep internal pulsing — **CLOSED, not implemented**

The "no wire arm" claim (§2.2) was an assertion, and after `fvAddrsRew` I stopped
trusting assertions in this document. It is now MEASURED, with
`capture_gov_pulser`, sampling `ConwayGovState[6]` from both nodes across a full
devnet epoch including the 4k/f mark:

```text
slot off=241  cardano: array(2), no sum tag   dugite: array(2), no sum tag
slot off=267  …290 …320 …346 …371 — identical at every sample
```

`DRepPulsingState` reaches the wire as a bare `array(2)`, never as a tagged sum.
Upstream's encoder forces the pulser before encoding — `encCBOR` on a
`DRPulsing` calls `finishDRepPulser` and writes the resulting `DRComplete` —
so **`DRPulsing` is unobservable to any peer**, at any point in the epoch. It is
an in-memory representation and nothing else.

Combined with §3.3 (no semantic gap — inputs freeze at the same instant in both
implementations) and Phase 0's finding that the measured cost is in the RUPD
fold rather than the DRep one, Phase 4 has no remaining justification of any
kind. **Implementing it would add ledger state, snapshot surface and rollback
surface for zero observable difference**, and every one of those surfaces has
produced a defect in this repo (#985, #989, #1057). Closed as YAGNI, with the
measurement recorded so the question does not get reopened on a guess.

> **The investigation was still worth running.** It did not change Phase 4, but
> diffing the raw `GetGovState` bytes it captured found a live wire divergence
> nothing else would have: `costModels` arrays framed definite where
> cardano-node frames them indefinite (`98 a6` vs `9f … ff`), the #938 class in
> the LSQ pparams path. Invisible to every value-comparing parity suite, since
> both framings decode identically. See the commit for the fix.

**One divergence remains open here.** The embedded `PulsingSnapshot` differs:

```text
                psProposals  psDRepDistr  psDRepState  psPoolDistr   bytes
  cardano-node            0            0            0            0       5
  dugite                  8            0            2            4    2081
```

Stable across samples on both nodes, and the `RatifyState` half — the part
cardano-cli renders — is the same size on both.

**ORACLE-CHECKED, and it eliminates one of the two hypotheses.**
`finishDRepPulser` populates every field; it does NOT empty the snapshot on
completion:

```haskell
finishDRepPulser (DRPulsing (DRepPulser {..})) =
  ( PulsingSnapshot
      dpProposals                       -- psProposals
      finalDRepDistr                    -- psDRepDistr
      dpDRepState                       -- psDRepState
      (Map.map individualTotalPoolStake $ unPoolDistr finalStakePoolDistr)
  , ratifyState' )
```

So "cardano-node legitimately reports an emptied snapshot" is REFUTED, and
dugite's populated shape is the structurally correct one. What remains is a
question about CONTENTS, not shape: cardano-node's four fields were all zero at
every sample while dugite's were 8/0/2/4 on the same chain.

Two candidate explanations survive, and they are distinguishable by ONE
measurement rather than by reading more source:

1. cardano-node's frozen snapshot genuinely was empty at the boundary that
   created it — the devnet's proposals were submitted mid-epoch, after the
   freeze — and dugite is reporting LIVE state where upstream reports FROZEN.
   That is #922's shape reappearing in the embedded pulser.
2. The devnet's pools genuinely carried no stake in the pulser's accumulated
   distribution at that epoch, making all four legitimately zero on upstream.

`psPoolDistr = 0` on a chain with two live pools is what makes (1) the stronger
reading — but dugite's 4 entries against 2 registered pools is itself odd and
needs explaining either way.

**The deciding measurement**: sample the embedded pulser IMMEDIATELY after an
epoch boundary and again late in the same epoch, on both nodes, with proposals
submitted in between. If dugite's `psProposals` grows within an epoch while
cardano-node's does not, it is (1) and the fix is to source the embedded
snapshot from the frozen `dpProposals` — the same correction #922 applied to
`GetProposals`. Still not fixed here: the measurement is cheap and the fix
without it is a guess, which is how #1057 got worse.

**Caveat on the source above**: it is read from cardano-ledger `master`, and
cardano-node 11.0.1 pins CHaP per PACKAGE at an older revision. The refutation
is robust to that (the constructor takes four populated arguments in both), but
do not treat any FIELD ORDER read from master as pinned — that is the trap
recorded in [[reference_running_node_is_the_wire_oracle]].

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

## 5b. BLOCKER — Phase 1 cannot be validated on this machine

Measured 2026-08-08, not estimated. `mithril-import` for mainnet stages the
snapshot in `$TMPDIR/dugite-mithril` BEFORE writing the database:

```
free before import          263 GiB
free after ~10 min          39 GiB      (224 GB staged in TMPDIR)
final db-mainnet            not yet written
```

So mainnet needs roughly **224 GB temp + ~150 GB final = ~375 GB**, against
263 GiB free with the 202 GB main `target/` already deleted. The import was
aborted at 39 GiB free to protect a running devnet gate — filling the disk
would have failed the gate and risked ImmutableDB corruption.

**Consequence: Phase 1 must not be marked done here.** Its decisive test is the
mainnet **epoch 235 -> 236** (Shelley->Allegra) replay proving
`pending_avvm_return` is redundant once the freeze is explicit; preprod and
preview carry no AVVM balances and cannot substitute. Options, in order:

1. Run Phase 1's validation on a machine with >= 400 GB free.
2. Teach `mithril-import` to stream chunks rather than staging the whole
   snapshot — removes the 224 GB temp requirement and is independently
   worth doing.
3. Reconstruct a minimal mainnet chunk range around epoch 235-236 rather than
   importing the whole chain.

Until one of those exists, Phase 1 stops at the explicit-freeze design and the
`pending_avvm_return` deletion stays UNSHIPPED. Deleting a compensation worth
-561K ADA reserves / +184K ADA treasury on the strength of a gate that cannot
observe it would be exactly the un-reddable-test failure recorded in section 5.

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
