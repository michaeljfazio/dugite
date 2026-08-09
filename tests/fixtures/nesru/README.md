# `nesRu` (`NewEpochState[4]`) wire captures — cardano-node 11.0.1

Captured from a live devnet with

```
cargo run -p dugite-network --example capture_nesru -- <sock> 42
```

sampled by **slot**, because `instance ToJSON PulsingRewUpdate` renders
`Pulsing` as `Null` exactly like `SNothing` (`RewardUpdate.hs:359-365`) — the
JSON view cannot tell you which state you captured.

| file | slot | bytes | meaning |
|---|---|---|---|
| `snothing.hex` | 120 | `80` | `SNothing` — `array(0)`. What dugite emits today, unconditionally (#1071). |
| `at-slot-324.hex` | 324 | `818201850000a00082a000` | **`Complete`, not `Pulsing`** — see below. |
| `complete.hex` | 353 | `818201850000a00082a000` | `SJust (Complete r)`, all-zero (epoch 0). |
| `pulsing.hex` | 1122 | 7.4 KB, `818300 88 …` | `SJust (Pulsing s p)` — see below. |
| `complete-nonzero.hex` | 1165 | 2.6 KB, `818201 85 1b…` | `Complete` with REAL values. |

The all-zero `complete.hex` pins the shape but exercises no field widths. Prefer
`complete-nonzero.hex` for encoder work: its `deltaT` is a 64-bit
`1b 00000345b632b621` and its `rs` carries real credentials, so a fixture that
only ever saw zeros cannot hide a width or map-framing bug.

**The transition is bracketed**: Pulsing at offsets 322/331/345, Complete at
365. With 120 credentials the fold takes ~120 pulses, so it finishes between
offsets 345 and 365 — roughly 25-45 blocks after the mark, matching
~2 slots/block at f=0.5.

```text
81                array(1)  = SJust
  82              array(2)  = the sum
    01            tag 1     = Complete   (tag 0 would be Pulsing)
    85            array(5)  = RewardUpdate
      00            deltaT
      00            deltaR
      a0            rs        = map(0)
      00            deltaF
      82 a0 00      nonMyopic = array(2)[map(0), 0]
```

The all-zero `RewardUpdate` is expected at epoch 0 — no stake in `go`, no
blocks in `bprev`. It pins the SHAPE, not the values.

## `pulsing.hex` — CAPTURED after seeding 120 credentials

The threshold reasoning below was acted on rather than filed, and it worked.
With 120 registered + delegated + funded stake credentials, `Pulsing` holds
across the whole window instead of a single block:

```text
slot 1122 (offset 322)  SJust Pulsing (sum tag 0)
slot 1131 (offset 331)  SJust Pulsing (sum tag 0)
slot 1145 (offset 345)  SJust Pulsing (sum tag 0)
```

```text
81            array(1)  = SJust
  83          array(3)  = the sum          <- THREE elements, unlike Complete's two
    00        tag 0     = Pulsing
    88        array(8)  = RewardSnapShot   <- rewFees, rewProtocolVersion,
                                              rewNonMyopic, rewDeltaR1, rewR,
                                              rewDeltaT1, rewLikelihoods, rewLeaders
    ...       Pulser
```

7.4 KB, versus 11 bytes for `Complete` at epoch 0 — the snapshot carries real
likelihoods and leader rewards.

Seeding recipe (`scratchpad/seed-batched.sh`): 120 credentials cannot go in one
tx (`MaxTxSizeUTxO`, 32488 vs 16384), so they are batched 40 per tx with the
funder's UTxO re-queried each round, since the previous batch consumes the
change output. Registration alone is not enough — `resolveInstantStake` counts
a credential only if it is registered AND delegated AND holds non-zero stake.

## Why the first attempt saw no `Pulsing` — a corrected assumption

The design spec predicted `Pulsing` in slots ~321-335 and `Complete` from
~336. **That was wrong.** The capture at slot 324 — four slots past the `4k/f`
mark — is already `Complete`.

`pulseSize = max 1 (ceil(numStakeCreds / 4k))` is 1 here, and there are a
handful of credentials, so the fold finishes in a SINGLE pulse: on the first
block after the mark. `Pulsing` therefore exists for at most one block on this
devnet and cannot be reliably captured from it.

Consequences for Phase 2:

* the `Pulsing` arm needs a network with enough ACTIVE stake credentials that
  the fold spans several blocks. The threshold is lower than it looks —
  **~100 credentials is enough** (measured, not guessed):

  | active creds | pulseSize | pulses | ~slots to complete | Pulsing observable? |
  |---|---|---|---|---|
  | 5    | 1  | 5   | 10  | no — completes in the window |
  | 100  | 1  | 100 | 200 | **yes** |
  | 1000 | 7  | 143 | 286 | yes |

  `pulseSize = max 1 (ceil(creds / 4k))` stays **1** until credentials exceed
  `4k = 160`, so the pulse COUNT grows linearly and overruns the 80-slot window
  long before the pulse SIZE starts absorbing it. Registering and delegating
  ~100 stake credentials, then waiting two boundaries for them to reach the
  `go` snapshot, makes `Pulsing` observable for most of slots 321-400.

  Failing that, a synthetic fixture built from the Haskell encoder definition
  and **labelled as synthetic** — never presented as a
  capture. Shipping a guessed encoding as though it were observed is the
  #1057 / #1067 mistake, where reading a shape off `deriving EncCBOR` produced
  a plausible wrong answer both times.
* the JSON-visible `Complete` window is ~slots 321-400, about 20% of the
  epoch, which is consistent with #1071's independently measured divergence
  rate.

## Re-capturing

Use the same example against a node in the window. Do not use cardano-cli's
JSON to decide whether the capture landed correctly — `Pulsing` and `SNothing`
are indistinguishable there. The example classifies from the CBOR and prints
the verdict to stderr.

---

## The `Pulsing` record, DECODED (2026-08-09)

Decoded with a real CBOR decoder rather than read off the head — the head is
what produced three wrong readings of this same artefact before. Every field
below is from `pulsing.hex`.

```text
array(1)                                  SJust
  array(3)                                Pulsing
    0                                     sum tag
    array(8)                              RewardSnapShot
      [0] 0                               rewFees
      [1] array(2) [10, 0]                rewProtocolVersion
      [2] array(2) [map(0), 13679988715120]  rewNonMyopic  <- the OLD NonMyopic
      [3] 17989722017445                  rewDeltaR1
      [4] 14391777613956                  rewR
      [5] 3597944403489                   rewDeltaT1
      [6] map(2)  hash28 -> array(100) f32   rewLikelihoods
      [7] map(1)  RewardAcnt -> set Reward   rewLeaders
    array(4)                              Pulser
      [0] 1                               pulse size
      [1] array(4)                        FreeVars
          [0] set of Credential           fvAddrsRew          (140 here)
          [1] 54003425994184880           fvTotalStake
          [2] array(2) [10, 0]            fvProtocolVersion
          [3] map(1) hash28 -> array(5)   fvPoolRewardInfo
      [2] map(19)  Credential -> [stake, poolId]   balance    <- the WORK QUEUE
      [3] array(2) [map, map]             RewardAns           <- answer so far
```

**`FreeVars` is FOUR fields, not the large record it looks like from the byte
count** — the 7.4 KB is almost entirely `rewLikelihoods` (2 pools x 100 f32)
and `fvAddrsRew` (140 credentials).

**Note the two same-shaped credential collections again**: `fvAddrsRew` holds
**140** and `balance` holds **19**. Only the COUNTS separate them, which is the
trap recorded in CLAUDE.md — an assertion pointed at the wrong one passes
forever.

### Every field maps to state dugite already has

| upstream | dugite |
|---|---|
| `rewFees` | `epochs.snapshots.ss_fee` |
| `rewProtocolVersion` | `epochs.prev_protocol_version_{major,minor}` |
| `rewNonMyopic` | `epochs.non_myopic` — already encoded for #1067 |
| `rewDeltaR1` / `rewR` / `rewDeltaT1` | `epochs.rupd_monetary` (`MonetaryStep`) |
| `rewLikelihoods` | `build_new_likelihoods` — computed, **not yet stored at the mark** |
| `rewLeaders` | leader rewards from the fold — **not yet stored at the mark** |
| `fvAddrsRew` | `epochs.rupd_addrs_rew` |
| `fvTotalStake` | `rupd_monetary.total_stake` (this fixture: 54003425994184880) |
| `fvPoolRewardInfo` | `rupd_fold.table` (`PoolRewardInfo`, 5 fields) |
| `balance` | `RewardFold::remaining()` |
| `RewardAns` | `RewardFold`'s accumulator |

So #1071 is an ENCODER job plus relocating two computations to `startStep`,
not an architectural one. It still needs:

1. `rewLikelihoods` and `rewLeaders` computed at the 4k/f mark and persisted,
   which means a SNAPSHOT_VERSION bump.
2. The `SNothing -> Pulsing -> Complete` transition instants validated against a
   live cardano-node across a full epoch. dugite's pulse cadence must match
   Haskell's for the arms to be right at the moment they switch, and emitting
   `Complete` where cardano-node emits `Pulsing` would be a confidently wrong
   answer replacing an honestly wrong one (#979). The fixtures already bracket
   the transition: Pulsing at offsets 322/331/345, Complete at 365.
