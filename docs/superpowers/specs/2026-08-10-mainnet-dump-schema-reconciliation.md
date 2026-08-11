# Mainnet exactness — dump schema reconciliation (#1073)

Status: **conwayGov LANDED and cross-validated against real preprod Conway
oracle output.** The remaining honest gap is `instantaneousRewards`, which is
deliberate — see below. Everything under "What must not be done" still stands.

## `nextEnactState` is the pulser's SELF-INCLUSIVE `rsEnactState`

This is the load-bearing fact of the whole conwayGov half, it was not obvious
from the shape, and getting it wrong would have produced a confident
one-epoch-shifted answer in exactly the epochs that matter.

RATIFY threads ONE `EnactState` through the ENACT rule and returns it inside the
`RatifyState` it produces (`Conway/Rules/Ratify.hs`, `ratifyTransition`, at
cardano-ledger `faa7a9dc347697b11d4da5b7818b1731e11aeeef` = conway 1.20.0.0):

```haskell
newEnactState <- trans @(EraRule "ENACT" era) $
                   TRC ((), rsEnactState, EnactSignal gasId govAction)
let st' = st & rsEnactStateL .~ newEnactState
             & rsEnactedL %~ (Seq.:|> gas)
trans @(ConwayRATIFY era) $ TRC (env, st', RatifySignal sigs)
```

So `ensCommittee`, `ensConstitution`, `ensCurPParams` and `ensPrevGovActionIds`
carry the effects of the actions in the pulser's own `rsEnacted` — **one
boundary before live governance state gains them**. `ensPrevPParams` is the
exception: nothing in RATIFY/ENACT writes it, and it changes only at the
boundary (`Rules/Epoch.hs`, `cgsPrevPParamsL .~ curPParams`).

**Verified against the oracle rather than accepted from the analysis.** The
rival reading — that `rsEnactState` equals the `dpEnactState` the pulser was
seeded with — predicts that epoch 180's `curPParams`/`prevPParams` differ in
`protocolVersion` ALONE. Measured, they differ in `{costModels,
protocolVersion}`, which only the self-inclusive reading produces. Three further
predictions, all confirmed on `reports/preprod-conway-oracle/`:

| epoch | cur vs prev differ in | predicted by |
|---|---|---|
| 179 | `costModels` only | self-inclusive |
| 180 | `costModels`, `protocolVersion` | self-inclusive (rival says protocolVersion only) |
| 181 | `protocolVersion` only | self-inclusive |
| 182 | nothing | both |

Dating the preprod events from that: the `PParamUpdate` b52f… enacts at the
179→180 boundary and the `HardFork` ccb2… at 180→181, so preprod runs PV10 from
epoch 181. The top-level `protocolParams` key reads major 9 at epoch 181 because
it is `prevPParams`, not the live parameters.

**Consequence for dugite.** `PulsedRatifyState` already stored the
self-inclusive `cur_pparams` (#977 needs it for `predictFuturePParams`) but
none of the other three. They are now stored beside it as
`PulsedRatifyState.enact_state` (`EnactedGovTerms`), captured from the state the
pulser's own dry run produced — `compute_pulsed_ratify_state` clones, runs
`ratify_proposals_impl`, and that clone has had every enactment applied by the
same `enact_gov_action_impl` the real boundary uses. Nothing is re-derived, so
there is no second copy of enactment to drift.

Reading them live instead would have been correct in every epoch that enacts
nothing and wrong in every epoch that enacts something — #977's and #1071's
shape. The regression test asserts the two are DIFFERENT at that instant
(`frozen_plan_carries_its_own_enactment_roots_before_live_state_does`), which is
what makes it fail when the source is swapped for live state; it was proven red
by doing exactly that.

`scripts/validation/diff-cstreamer-dumps.py` exits **2 (SCHEMA GAP)** on the
validated 208–316 range. That is the correct verdict and it is not the fault of
`conwayGov`. This document records what actually drives it, measured from
`reports/mainnet-exactness/report-208-316-alonzo.json`.

## The gap inventory, measured

Eight top-level paths are absent on one side in every one of the 109 paired
epochs:

| path | present on | absent on | comparable today? |
|---|---|---|---|
| `snapshots.mark.epoch` | dugite | cstreamer | n/a — dugite-only |
| `snapshots.set.epoch` | dugite | cstreamer | n/a — dugite-only |
| `snapshots.go.epoch` | dugite | cstreamer | n/a — dugite-only |
| `proposals` | dugite | cstreamer | no upstream counterpart |
| `enactedRoots` | dugite | cstreamer | overlaps `conwayGov.nextEnactState` |
| `drepDistr` | dugite (top level) | cstreamer (nests it) | after conwayGov lands |
| `instantaneousRewards` | cstreamer | dugite | **no — see below** |
| `rupdApplied` | cstreamer | dugite | **redundant — see below** |

`conwayGov` itself is **not** in this list. `era_applicable` returns False for
it below Conway (`diff-cstreamer-dumps.py:88`), so the era model already
handles it correctly and the Alonzo range never counted it as a gap. The
conwayGov problem is real but it is a *future* problem, reachable only once the
oracle replays a Conway epoch.

## What the oracle actually emits

Authoritative, from `cardano-streamer/src/Cardano/Streamer/Run.hs`
(branch `10.6.2-dump-snapshot`, commit `8c5b285`):

`extractConwayGovData` (Run.hs:150–166) — five keys, and **neither `proposals`
nor `enactedRoots` is among them**:

```haskell
Aeson.object
  [ "drepDistr"       Aeson..= drepDistr        -- Map.map fromCompact (psDRepDistr snap)
  , "committee"       Aeson..= committee        -- newEpochStateGovStateL . committeeGovStateL
  , "constitution"    Aeson..= constitution     -- newEpochStateGovStateL . constitutionGovStateL
  , "committeeState"  Aeson..= committeeState   -- certVStateL . vsCommitteeStateL
  , "nextEnactState"  Aeson..= nextEnactState   -- ratifyState ^. rsEnactStateL
  ]
```

`snapshotInfo` (Run.hs:296–303) — exactly `name`, `stake`, `delegations`,
`poolParams`, plus `blocks` for mark and go only. There is no `epoch` field.
Confirmed against a real dump (`cstreamer/240-18316800.json`).

## Findings that change what should be built

### `snapshots.*.epoch` is dugite-only, and one of the three is a hardcoded 0

`main.rs:1267` emits `snapshot.epoch.0` for mark/set, but the `go` fallback
branch at `main.rs:1568` emits a literal `"epoch": 0`. Nothing compares it, so
the wrong value has never surfaced. Upstream has no such field.

Removing it from the dump closes three of the eight gaps and deletes a latent
wrong value. This is the one item here that is unambiguously safe.

### `instantaneousRewards` cannot be made non-vacuous at a dump point

Measured across all 66 reduced oracle dumps (epochs 208–273, which include the
28 Shelley epochs where mainnet actually carried MIR certificates):

```
epochs with non-empty iRReserves/iRTreasury : 0 / 66
epochs with non-zero deltaReserves/deltaTreasury : 0 / 66
```

The field holds *pending* MIR transfers. The epoch boundary applies and clears
them, and both sides dump at the first block of the new epoch — immediately
after that clear. It is an epoch-**phase** field, the same shape as #977's
`futurePParams` and #1071's `nesRu`, and the phase in which it carries a value
does not contain a dump point.

So implementing it would convert a schema GAP into a comparison that reads
`{}` vs `{}` and `0` vs `0` in every epoch — a **vacuous** result the
comparator counts separately precisely because two implementations agreeing
that there is nothing to say is not evidence. It would improve the exit code
and add nothing. That is the #979 principle: an unverified arm is worse than an
honest gap.

Shape, if it is ever built for a non-boundary dump point, is known from real
output: `{deltaReserves, deltaTreasury, iRReserves, iRTreasury}`. The
credential key encoding of the two maps is **not** known — every observed
instance is `{}`, so it cannot be read off the data and must not be guessed.

### `rupdApplied` is redundant with `rupdNext`, not new coverage

`buildSnapshotJson` returns `(json, rupdData)` and the driver threads the
previous epoch's value in as `mPrevRupd` (Run.hs:169, 370):

```haskell
, "rupdApplied" Aeson..= mPrevRupd
```

So `rupdApplied[E] == rupdNext[E-1]` **by construction on the oracle side**.
Comparing it compares dugite's `rupdNext[E-1]` against cstreamer's
`rupdNext[E-1]` — the identical comparison already made one epoch earlier,
where it currently matches in 107 of 109 epochs.

Implementing it is cheap and closes a gap honestly (dugite would thread its own
`rupdNext` forward the same way). It must not be described as new coverage of
the applied reward update. Computing it instead from what dugite *actually
applied* would be more informative but would no longer match the oracle's
semantics, and would manufacture divergences that are definitional.

## What must not be done

The comparator exits 2 on a schema gap so that ~140 Conway epochs cannot
compare vacuously and report a false PASS. Do not weaken that to obtain a
green:

- Do not add `instantaneousRewards` merely to remove it from the gap list.
- Do not add an exclusion for a field whose absence is the honest state.
- Do not infer `conwayGov`'s member shapes from cardano-ledger's types. The
  Aeson instances for `Committee`, `Constitution`, `CommitteeState` and
  `EnactState` are what the oracle prints, and three separate readings of a
  real artefact have already been wrong in this campaign by reasoning from
  types instead of decoding output.

## Validated on preprod, not just asserted

Items 1 and 2 were run, not trusted. dugite replayed the cloned preprod chain
(172,931 blocks, 17 epochs) and the output was diffed against cardano-streamer
over the same chain:

- `snapshots.{mark,set,go}` now carry exactly `blocks, delegations, name,
  poolParams, stake` — the oracle's five. The three `epoch` gaps are gone.
- `rupdApplied` moved out of SCHEMA GAPS into real comparison, and
  `rupdApplied[10] == rupdNext[9]` byte-for-byte on dugite's own side.
- Schema gaps fell from 8 to 4: `drepDistr`, `enactedRoots`, `proposals`,
  `instantaneousRewards`. Exit code still 2, which is correct.

**The remaining divergences are the known definitional ones, reproduced on an
independent network.** preprod shows dugite `null` vs oracle value for `eta`
(from epoch 4), `rupdNext` (4–5) and `snapshots.set` (4 only) — the identical
shape mainnet shows at 208, 208–209 and 208 respectively. Both are the first
Shelley epoch of their chain. `rupdApplied` diverges at preprod 5–6, i.e.
`rupdNext`'s 4–5 shifted by exactly one, which is the threading proving itself.

## Consequence to expect on the next mainnet run

Adding `rupdApplied` will roughly DOUBLE the #1077-attributable divergence
count, from 50 to about 100, and that is not a regression.

The 50 recorded today are `rupdNext.deltaR2` (25 epochs, 212–236) and
`rupdNext.totalDistributed` (25 epochs, 212–236) — cardano-streamer's `sumRs`
folding every `Set Reward` element without `filterRewards` at pv<=2. Because
`rupdApplied[E] == rupdNext[E-1]`, the same oracle defect will surface again as
`rupdApplied.deltaR2` and `rupdApplied.totalDistributed` over epochs 213–237.

Anyone reading a jump from 61 to ~111 divergent as a regression will be wrong.
It is one oracle defect counted twice, by construction.

## The conwayGov shape, from REAL Conway oracle output

No longer blocked. A fresh preprod Mithril import (cardano-node's own chunks,
so cardano-streamer reads it with no dugite writer involved) was replayed to
preprod Conway, which begins at **epoch 163**. Captured from
`reports/preprod-conway-oracle/`:

```
conwayGov = { committee, committeeState, constitution, drepDistr, nextEnactState }
```

| key | shape |
|---|---|
| `committee` | `{"members": {"<kind>Hash-<56 hex>": <expiryEpoch>}, "threshold": {"numerator":N,"denominator":D}}` |
| `committeeState` | `{"csCommitteeCreds": {"<kind>Hash-<56 hex>": {"tag": "CommitteeHotCredential", "contents": {"scriptHash"\|"keyHash": "<56 hex>"}}}}` — a TAGGED sum, not a bare hash; empty until preprod 171 |
| `constitution` | `{"anchor": {"dataHash": "<64 hex>", "url": "ipfs://…"}, "script": "<56 hex>"}` |
| `drepDistr` | `{"drep-keyHash-<56 hex>" \| "drep-scriptHash-<56 hex>" \| "drep-alwaysAbstain" \| "drep-alwaysNoConfidence": <lovelace>}` |
| `nextEnactState` | `{committee, constitution, curPParams, prevGovActionIds, prevPParams}` |

`nextEnactState.prevGovActionIds` is
`{"Committee": null, "Constitution": null, "HardFork": null, "PParamUpdate": null}`
— **this is dugite's top-level `enactedRoots`**, same four lanes, same
information, nested and named differently. It is not an orphan field after all.

The non-null rendering is captured too, at preprod epoch 179 — the first
enactment on that chain:

```json
"prevGovActionIds": {
  "PParamUpdate": {"govActionIx": 0,
                   "txId": "b52f02288e3ce8c7e57455522f4edd09c12797749e2db32098ecbe980b645d45"},
  "Committee": null, "Constitution": null, "HardFork": null
}
```

An **object with `govActionIx` and `txId`**, not a string. dugite emits
`format!("{}#{}", txid, index)`, so the format is wrong as well as the
location. Worth stating that this needed 16 Conway epochs to observe: every
lane is null from 163 through 178, so a capture that stopped at the era
boundary would have "confirmed" a null-only shape and the string format would
have survived.

`proposals` remains genuinely dugite-only — upstream's `extractConwayGovData`
(Run.hs:150-166) emits neither it nor `enactedRoots`.

### Two defects in dugite's `drepDistr`, both certain from this output

`main.rs` builds the key as:

```rust
format!("drep-keyHash-{}", &hash.to_hex()[..30])
```

1. **The hash is TRUNCATED to 30 hex characters** (15 of 28 bytes). The oracle
   emits the full 56. Every entry would mismatch on the key alone.
2. **There is no `drep-scriptHash-` arm.** Script DReps are labelled
   `drep-keyHash-`. Real preprod data has both — epoch 166 carries 9 keyHash
   entries and one `drep-scriptHash-763ef7…`, and that script DRep holds
   5,007,783,823,575 lovelace, more than all nine keyHash DReps combined.

**CORRECTION (2026-08-11): the credential kind is NOT erased, and the claim
that it was is withdrawn.** An earlier revision of this document — and the
adversarial review that accepted it — said `build_drep_power_cache` returns
`ImblHashMap<Hash32, u64>` and therefore loses the kind, making a
consensus-adjacent ledger type change a prerequisite. That is wrong.

The key is a **typed** Hash32. `DRep::credential_hash32`
(`dugite-primitives/src/transaction.rs:312`) writes the 28-byte credential into
`bytes[..28]` and the discriminator into `bytes[28]` — `0x00` for a key hash
(by zero-padding) and `0x01` for a script hash:

```rust
DRep::ScriptHash(h) => { bytes[..28].copy_from_slice(h.as_bytes()); bytes[28] = 0x01; }
```

So the kind is recoverable at the dump layer and must be READ, not assumed.
`drepDistr` is therefore a **dump-only** change with no ledger surface, and no
consensus review is required for it. Reading the type rather than the map's
signature is what settled this — the same lesson as decoding a whole record
instead of its head.

**FIXED and cross-validated.** Both key defects are corrected, and the result
is checked against real cardano-streamer output rather than asserted: dugite
replayed 2,702,108 preprod blocks into Conway and its `drepDistr` is
**byte-identical to the oracle in all 6 paired Conway epochs**, including the
script DRep that was previously mislabelled as a key hash.

One further difference surfaced and is also fixed: upstream omits a pseudo-DRep
nobody has delegated to (`psDRepDistr` is a Map with no entry), while dugite
emitted `drep-alwaysNoConfidence: 0` unconditionally — one spurious key in
every such epoch, which is the constant-noise shape that hid a real defect in
#1078. Now emitted only when non-zero, with the limitation stated at the code:
the cache returns the two pseudo-DReps as plain counters, so dugite cannot
distinguish "absent" from "present with zero".

## What conwayGov emits, and what preprod could not exercise

dugite now emits the oracle's five keys and drops the two dugite-only ones.
`proposals` and `enactedRoots` are **removed outright**, not excluded in the
comparator: an exclusion is for a field that is compared and whose difference is
known-benign, and neither has anything to compare against. The live proposal set
is still reachable over N2C (`GetProposals`).

The two provenances are distinct and must stay so — `committee` and
`constitution` are read LIVE from governance state, while
`nextEnactState`'s copies of the same two come from the frozen pulser.

Preprod exercises a lot of this but not all of it, and the untested arms are
worth naming rather than discovering on mainnet:

| arm | preprod 163-184 | note |
|---|---|---|
| `scriptHash-` credential keys | YES — the whole committee | key kind read from byte 28 of the typed hash, not a side set |
| `keyHash-` credential keys | NO | mainnet's committee will exercise it |
| `committeeState` non-empty | YES — grows 0 → 4 | all `CommitteeHotCredential` |
| `CommitteeMemberResigned` | NO | shape taken from the ToJSON instance, unobserved |
| `constitution.script` present | YES | absent-arm (key omitted, not null) unobserved |
| `committee = null` (SNothing) | NO | needs an enacted `NoConfidence` |
| `prevGovActionIds` non-null | YES — 2 of 4 lanes | `Committee`/`Constitution` lanes unobserved |
| `curPParams` ≠ `prevPParams` | YES — at 179, 180, 181 | covers costModels and protocolVersion |

## Remaining work, in order

1. **Remove `snapshots.*.epoch`** from dugite's dump (closes 3 of 8; deletes a
   hardcoded 0). Safe today.
2. **Thread `rupdApplied`** through dugite's dump driver, matching the oracle's
   previous-epoch semantics. Safe today; label it redundant.
3. **conwayGov** — no longer blocked; the shape is captured above. Build a
   `conwayGov` object, null before Conway, carrying the oracle's five keys:
   - `drepDistr` moves inside it, with the full 56-hex hash and a real
     `drep-scriptHash-` arm. That needs `build_drep_power_cache` to stop
     erasing the credential kind, which is a ledger-side change, not a dump
     one.
   - `enactedRoots` becomes `nextEnactState.prevGovActionIds`, four lanes,
     `null` when absent. Its non-null rendering is still unobserved — capture
     it from a preprod epoch that has actually enacted something before
     choosing a format.
   - `committee`, `committeeState`, `constitution`, `curPParams`,
     `prevPParams` are new emissions.
   - `proposals` has no upstream counterpart. Decide it deliberately: drop it,
     or keep it and record an explicit exclusion with a reason. Do not leave it
     to be discovered as a gap by the tip run.

   Deliberately NOT attempted in the session that captured this. It is five
   nested fields plus a ledger-type change, landing on the dump that certifies
   consensus equivalence, and #1072's fix — written from a correct diagnosis —
   still introduced two consensus bugs three lines from the one it fixed.
   Implementing it against a shape learned the same hour, then not re-running
   the gate it invalidates, would be the worse trade.

## Why `db-preprod` had to be rebuilt first

The pre-existing `db-preprod` was a **hybrid** and unreadable to cstreamer past
its Mithril boundary — #1081's defect D2, visible on disk:

| chunk | writer | `.primary` bytes | implied arity |
|---|---|---|---|
| `03000` | Mithril (cardano-node) | 86,409 | (21600+2)·4+1 → `chunk_size+2`, chunk = 10·k ✓ |
| `05928` | dugite, pre-#1081 | 1,728,005 | (432000+1)·4+1 → `epoch_length+1` ✗ |
