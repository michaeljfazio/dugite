---
name: praos-epoch-boundary-nonce-no-extraentropy
description: Verbatim, pinned-SHA proof that Praos (Babbage/Conway/Dijkstra)'s epoch-boundary nonce combine in Praos.hs's tickChainDepState is a 2-term formula (candidateNonce ⭒ lastEpochBlockNonce) with NO extraEntropy term — structurally different from TPraos's 3-term TICKN. Also proves ppExtraEntropy is not even a field of Babbage+ PParams.
type: reference
---

# Praos epoch-boundary nonce combine — decisive source proof (oracle-verified 2026-08-05)

Researched to resolve a real tension between two prior memory notes: does
Babbage's epoch-boundary nonce evolution fold `extraEntropy` the same way
TPraos's TICKN rule does? **No.** Babbage/Conway/Dijkstra go through
`Praos`, not `TPraos`, and `Praos`'s epoch-boundary combine is a
structurally different, hand-written function that never references
`extraEntropy` at all.

## Pins used

- `ouroboros-consensus` tag `release-ouroboros-consensus-3.0.1.0` @
  `c87aa760001e60f0f0d3353f793eb089adb917e7` — verified via
  `gh api repos/IntersectMBO/ouroboros-consensus/commits/c87aa760001e60f0f0d3353f793eb089adb917e7`
  (commit message: "Release consensus 3.0.1.0 + ingest PRs (#1987)"). Same SHA
  used by [[tpraos-overlay-vs-praos-no-overlay]] and
  [[praos-chain-order-v3-verified]] — matches cardano-node 11.0.1's cabal
  bound `^>= 3.0.1`.
- `cardano-ledger` `master` @ `4f7cb2d6874df70561e32147084ed82cee773e8a` —
  same pin as [[tpraos-overlay-vs-praos-no-overlay]].

## 1. The Praos epoch-boundary combine — VERBATIM from `Praos.hs`

File: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos.hs`
at `c87aa760001e60f0f0d3353f793eb089adb917e7`, lines 427–464
(`instance PraosCrypto c => ConsensusProtocol (Praos c)`, `tickChainDepState`):

```haskell
  -- Updating the chain dependent state for Praos.
  --
  -- If we are not in a new epoch, then nothing happens. If we are in a new
  -- epoch, we do three things:
  -- - Store the existing current epoch nonce as the "previous epoch" nonce.
  --   This is needed to validate Peras certificates when they appear in blocks.
  -- - Update the epoch nonce to the combination of the candidate nonce and the
  --   nonce derived from the last block of the previous epoch.
  -- - Update the "last block of previous epoch" nonce to the nonce derived
  --   from the last applied block.
  tickChainDepState
    PraosConfig{praosEpochInfo}
    lv
    slot
    st =
      TickedPraosState
        { tickedPraosStateChainDepState = st'
        , tickedPraosStateLedgerView = lv
        }
     where
      newEpoch =
        isNewEpoch
          (History.toPureEpochInfo praosEpochInfo)
          (praosStateLastSlot st)
          slot
      st' =
        if newEpoch
          then
            st
              { praosStateEpochNonce =
                  praosStateCandidateNonce st
                    ⭒ praosStateLastEpochBlockNonce st
              , praosStatePreviousEpochNonce =
                  praosStateEpochNonce st
              , praosStateLastEpochBlockNonce =
                  praosStateLabNonce st
              }
          else st
```

**This is the exact function the Rust code comment was trying to describe.**
Confirmed 2-term: `praosStateCandidateNonce st ⭒ praosStateLastEpochBlockNonce st`.
`praosStatePreviousEpochNonce` is set to the OLD `praosStateEpochNonce`
(matches the Rust comment's `-- the OLD one`), and
`praosStateLastEpochBlockNonce` is refreshed from `praosStateLabNonce st`
(matches `praosStateLastEpochBlockNonce = praosStateLabNonce` in the Rust
comment, modulo the field being read from `st`, the PRE-tick state). **The
Rust comment quoted in the question is accurate** — it correctly has no
`extraEntropy` term.

Grepped the entire file (all 796 lines) for `extraEntropy|ExtraEntropy`
case-insensitive: **zero hits**. `PraosState`'s full field list (lines
271–288) has exactly 8 nonce/bookkeeping fields — `praosStateLastSlot`,
`praosStateOCertCounters`, `praosStateEvolvingNonce`,
`praosStateCandidateNonce`, `praosStateEpochNonce`,
`praosStatePreviousEpochNonce`, `praosStateLabNonce`,
`praosStateLastEpochBlockNonce` — none of them extraEntropy.

## 2. Contrast: `updateChainDepState` / `reupdateChainDepState` (PER-BLOCK, not per-epoch)

Same file, lines 474–533. This is a DIFFERENT function from
`tickChainDepState` — it runs on every block application (VRF/KES
validation + candidate/evolving nonce bookkeeping), not at epoch boundaries.
It does NOT combine the epoch nonce:

```haskell
  reupdateChainDepState
    _cfg@( PraosConfig
             PraosParams{praosRandomnessStabilisationWindow}
             ei
           )
    b
    slot
    tcs =
      cs
        { praosStateLastSlot = NotOrigin slot
        , praosStateLabNonce = prevHashToNonce (Views.hvPrevHash b)
        , praosStateEvolvingNonce = newEvolvingNonce
        , praosStateCandidateNonce =
            if slot +* Duration praosRandomnessStabilisationWindow < firstSlotNextEpoch
              then newEvolvingNonce
              else praosStateCandidateNonce cs
        , praosStateOCertCounters =
            Map.insert hk n $ praosStateOCertCounters cs
        }
     where
      ...
      eta = vrfNonceValue (Proxy @c) $ Views.hvVrfRes b
      newEvolvingNonce = praosStateEvolvingNonce cs ⭒ eta
      ...
```

A prior memory note ([[epoch-nonce-tickn-deep-dive]] section 3, pre-2026-08-05)
quoted only this function and mislabeled it as covering the epoch-boundary
nonce — it never actually showed `tickChainDepState`. That gap is the direct
cause of the ambiguity this note resolves.

## 3. Contrast: TPraos's `tickChainDepState` delegates to the ledger STS TICKN, which DOES fold extraEntropy

File: `ouroboros-consensus-protocol/.../Protocol/TPraos.hs`, same pin.
`TPraos.hs`'s `tickChainDepState` (line 370) calls `SL.tickChainDepState`
(line 381), imported as `qualified Cardano.Protocol.TPraos.Rules.Tickn as SL`
(line 59) — i.e. TPraos's consensus-layer tick is a thin wrapper over the
cardano-ledger TICKN STS rule.

`libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/Tickn.hs` at
`4f7cb2d6874df70561e32147084ed82cee773e8a`, `tickTransition` (verbatim, full
function):

```haskell
tickTransition :: TransitionRule TICKN
tickTransition = do
  TRC (TicknEnv extraEntropy ηc ηph, st@(TicknState _ ηh), newEpoch) <- judgmentContext
  pure $
    if newEpoch
      then
        TicknState
          { ticknStateEpochNonce = ηc ⭒ ηh ⭒ extraEntropy
          , ticknStatePrevHashNonce = ηph
          }
      else st
```

3-term combine, `extraEntropy` sourced from `TicknEnv`'s
`ticknEnvExtraEntropy`, which upstream ultimately threads from
`lvExtraEntropy :: Nonce` on TPraos's `LedgerView` (see
[[tpraos-overlay-vs-praos-no-overlay]] section 3 for that field and its
upstream comment). **`Praos.hs`'s `tickChainDepState` has no equivalent
wrapper, no `TicknEnv`, and no `Environment`-derived extraEntropy input at
all** — it is a self-contained, independently hand-written function that
takes only `PraosConfig` + `LedgerView (Praos c)` (which itself has no
extraEntropy field per [[tpraos-overlay-vs-praos-no-overlay]] section 3) +
`slot` + the previous `PraosState`.

## 4. `ppExtraEntropy` is not even a FIELD of Babbage/Conway/Dijkstra PParams — nothing to leak forward

Checked `EraPParams` instances at `4f7cb2d6874df70561e32147084ed82cee773e8a`:

- `eras/babbage/impl/src/Cardano/Ledger/Babbage/PParams.hs`,
  `instance EraPParams BabbageEra`: `ppDG = to (const minBound)` and
  `hkdDL = notSupportedInThisEraL` / `hkdExtraEntropyL =
  notSupportedInThisEraL` (line 200) — the live `BabbagePParams` record
  (`bpp*` fields) has NEITHER a `d` field NOR an `extraEntropy` field.
- `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` line 859:
  `hkdExtraEntropyL = notSupportedInThisEraL`.
- `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/PParams.hs` line 421:
  `hkdExtraEntropyL = notSupportedInThisEraL`.
- Contrast `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/PParams.hs`: real
  field `appExtraEntropy :: !(HKD f Nonce)` (line 258), real lens
  `hkdExtraEntropyL = lens appExtraEntropy ...` (line 339) — Alonzo is the
  LAST era where this is a genuine, settable, on-chain PParams field.

The only place `ExtraEntropy` appears syntactically in `BabbagePParams.hs` at
all is `DowngradeBabbagePParams` (lines 167–170):

```haskell
data DowngradeBabbagePParams f = DowngradeBabbagePParams
  { dbppD :: !(HKD f UnitInterval)
  , dbppExtraEntropy :: !(HKD f Nonce)
  }
```

This is `type DowngradePParams f BabbageEra = DowngradeBabbagePParams f`, the
argument type for `downgradePParamsHKD` (`downgradeBabbagePParams`, lines
360–383), a generic API-level utility (re-exported from
`cardano-ledger-api/PParams.hs`, used by external tooling/tests, not invoked
automatically by any consensus/ledger STS transition) that reconstructs a
synthetic `AlonzoPParams`-shaped record FROM a real `BabbagePParams` PLUS
caller-supplied `dbppD`/`dbppExtraEntropy` — because `BabbagePParams` itself
has no such fields to read them from. There is no code path where a value
set by a pre-Babbage PPU update is carried through HFC era-translation into
Babbage/Conway/Dijkstra state: the `TranslateEra Alonzo Babbage PParams`
construction produces a fresh `BabbagePParams` value, and that type has no
slot to put an inherited `appExtraEntropy` into even if the translation code
wanted to.

**Verdict on part 4 of the question**: `ppExtraEntropy` is not merely inert
from Babbage onward — it structurally does not exist as ledger STATE past
Alonzo. It cannot leak forward, because HFC era translation constructs a
target-era `PParams` value with no field to receive it. A legacy Alonzo-era
extraEntropy value that was non-neutral at the moment of the Alonzo→Babbage
hard fork has zero effect on anything post-fork: not on nonce evolution
(section 1 above — the new formula never references it), not on `LedgerView`
(Praos's `Views.LedgerView` has no such field), and not on `PParams` itself
(no field, `notSupportedInThisEraL` on every Babbage+ era).

## 5. Direct verdict

For **Babbage** (and Conway, Dijkstra — same `Praos` protocol,
`tickChainDepState` is not overridden per-era) the real epoch-boundary nonce
formula is **2-term**: `praosStateEpochNonce_new = candidateNonce ⭒
lastEpochBlockNonce`, **NOT** the TPraos/Shelley 3-term
`candidateNonce ⭒ prevHashNonce ⭒ extraEntropy`.

Any Rust implementation that reuses the Shelley 3-term TICKN formula
(even with `extraEntropy` pinned to the neutral/identity element, since
`x ⭒ NeutralNonce = x`) is **accidentally correct only as long as
extraEntropy is always NeutralNonce** at every Babbage epoch boundary — but
it is implementing the WRONG function; it happens to be numerically
indistinguishable from the right one only because the extra term is always
the identity of `⭒`. It is not merely a style nit: the codebase's own
description of it as "Shelley's 3-term formula ... reused ... for Babbage"
is describing a different, non-general function than the real one, and if
anything ever supplied a non-neutral 3rd operand (which structurally cannot
happen per section 4, but a bug COULD wire the wrong Nonce field into that
slot) it would silently diverge from Haskell. The Conway-only 2-term
hand-written implementation described in the question is the one that
matches upstream; it should be the ONE implementation, applied uniformly to
Babbage, Conway, and Dijkstra (all three are `Praos`, not `TPraos`), not
Conway-only.
