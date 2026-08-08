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

## `Pulsing` is NOT observable on this devnet — a corrected assumption

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
