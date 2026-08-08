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
| `complete.hex` | 353 | `818201850000a00082a000` | `SJust (Complete r)`. |

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
