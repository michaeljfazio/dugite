---
name: shelley-genesisdeleg-and-mir-witness-quorum
description: GenesisDelegCert FutureGenDeleg maturation window (1x stabilityWindow, NOT 2x) + adoptGenesisDelegs per-block TICK adoption + validateMIRInsufficientGenesisSigs exact quorum-witness semantics per era (live-verified 2026-07-06)
metadata:
  type: reference
---

Live-verified against IntersectMBO/cardano-ledger via `gh api` source fetch, 2026-07-06.

## GenesisDelegCert / FutureGenDeleg maturation (Shelley DELEG rule)

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`, `delegationTransition`,
`GenesisDelegTxCert gkh vkh vrf` branch:

```haskell
GenesisDelegTxCert gkh vkh vrf -> do
  sp <- liftSTS $ asks stabilityWindow
  let s' = slot +* Duration sp
      GenDelegs genDelegs = dsGenDelegs ds
  isJust (Map.lookup gkh genDelegs) ?! GenesisKeyNotInMappingDELEG gkh
  -- ... DuplicateGenesisDelegateDELEG / DuplicateGenesisVRFDELEG checks against
  -- BOTH current (dsGenDelegs minus gkh) and future (dsFutureGenDelegs) cold/VRF keys ...
  pure $ certState & certDStateL . dsFutureGenDelegsL
    .~ Map.insert (FutureGenDeleg s' gkh) (GenDelegPair vkh vrf) (dsFutureGenDelegs ds)
```

**It is a two-phase queue insert, never an immediate `dsGenDelegs` write.**

**Maturation window is exactly ONE `stabilityWindow` (`3k/f`), NOT `2 * stabilityWindow`.**
`stabilityWindow` is a `Globals` field computed once at node startup by
`computeStabilityWindow` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/StabilityWindow.hs`):
```haskell
computeStabilityWindow k asc = ceiling $ (3 * fromIntegral k) /. f
  where f = positiveUnitIntervalNonZeroRational . activeSlotVal $ asc
```
This is the ONLY place GenesisDelegCert maturation reads `stabilityWindow` — the doc comment
on the `Globals` record field (`libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs` L770-775)
says "protocol updates must be submitted at least **twice** this many slots before an epoch
boundary" — that 2x reference is about a *different* mechanism (see below), not this one. Do not
conflate them.

### Where the real "2 * stabilityWindow" DOES apply (and is a distinct mechanism)
`getTheSlotOfNoReturn` (`libs/cardano-ledger-core/src/Cardano/Ledger/Slot.hs`):
```haskell
pointOfNoReturn = firstSlotNextEpoch *- Duration (2 * stabilityWindow)
```
Used for: (1) the PPUP proposal submission deadline in `Ppup.hs` (`slot < tooLate` gate before
accepting a PParams update vote for the current-targeting epoch), and (2) `solidifyNextEpochPParams`
in `Tick.hs` (HFC "slot of no return" for freezing next-epoch PParams). This is genuinely a
6k/f-slot deadline — but it is unrelated to `FutureGenDeleg` maturation, which uses a plain 1x
`stabilityWindow` offset from the certificate's own processing slot.

### Adoption: `adoptGenesisDelegs`, called EVERY BLOCK via TICK, not just at epoch boundary

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Tick.hs`:
```haskell
adoptGenesisDelegs es slot = es'
  where
    ds = ... certDStateL
    fGenDelegs = dsFutureGenDelegs ds
    GenDelegs genDelegs = dsGenDelegs ds
    (curr, fGenDelegs') = Map.partitionWithKey (\(FutureGenDeleg s _) _ -> s <= slot) fGenDelegs
    latestPerGKey (FutureGenDeleg s genKeyHash) delegate latest = -- keeps latest `s` per gkey
      ...
    genDelegs' = Map.map snd $ Map.foldrWithKey latestPerGKey Map.empty curr
    ds' = ds { dsFutureGenDelegs = fGenDelegs', dsGenDelegs = GenDelegs $ Map.union genDelegs' genDelegs }
```
Called from `validatingTickTransition` (itself called by `bheadTransition`, the sole
`transitionRules` entry of `TICK`) as: run NEWEPOCH first, THEN `adoptGenesisDelegs (nesEs nes') slot`
— i.e. once per **block**, using that block's own slot as the comparison point (`s <= slot`), not
gated on crossing an epoch boundary. Comment in `validatingTickTransitionFORECAST`: "note that
the genesis delegates are updated not only on the epoch boundary." `genDelegs'` (newly-matured)
is left-biased via `Map.union genDelegs' genDelegs`, so a freshly-matured delegate for a gkey
overrides the old current entry for that gkey; `latestPerGKey` resolves multiple matured
entries for the same gkey by picking the one with the largest `fgdSlot`.

**No CHANGELOG entry or documented deviation was found sanctioning an "immediate insert"
simplification.** `FutureGenDeleg`/`dsFutureGenDelegs` renames appear in cardano-ledger-core's
CHANGELOG only as API-rename notices, never as "this queue is optional." The two-phase delay is
part of the original formal Shelley ledger spec's genesis-delegation mechanism (anti-adaptive-
corruption: a freshly-delegated hot/VRF key must not gain effect until it is settled against
rollback), so an immediate-insert shortcut is a genuine consensus-relevant behavioral divergence,
not a benign implementation simplification — see [[project_dugite_genesisdeleg_mir_gaps_2026_07_06]].

## MIR genesis-delegate quorum witness check

Function: `validateMIRInsufficientGenesisSigs`, module `Cardano.Ledger.Shelley.Rules.Utxow`
(`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs`). Predicate failure:
`MIRInsufficientGenesisSigsUTXOW (Set (KeyHash Witness))`.

```haskell
validateMIRInsufficientGenesisSigs (GenDelegs genMapping) coreNodeQuorum witsKeyHashes tx =
  let genDelegates = Set.fromList $ asWitness . genDelegKeyHash <$> Map.elems genMapping
      genSig = Set.intersection genDelegates witsKeyHashes
      mirCerts = ... filter isInstantaneousRewards (txBody ^. certsTxBodyL) ...
   in failureUnless (not (null mirCerts) ==> Set.size genSig >= fromIntegral coreNodeQuorum)
        $ MIRInsufficientGenesisSigsUTXOW genSig
```

- **Compares the map's VALUES, not its keys.** `genMapping :: Map (KeyHash GenesisRole) GenDelegPair`;
  `Map.elems genMapping` gives the `GenDelegPair`s, and `genDelegKeyHash` extracts the **delegate
  (hot) key hash** from each pair — NOT the genesis (cold) key hash, which is the map's key type
  and is never referenced here. `asWitness` just recasts the `KeyRole` phantom type to `Witness`.
- Intersected against `witsKeyHashes` = `keyHashWitnessesTxWits (tx ^. witsTxL)`, the actual VKey
  witness hashes present on the transaction.
- **Keyed on `dsGenDelegs` only** (`certState ^. certDStateL . dsGenDelegsL` / `dsGenDelegs`), i.e.
  matured genesis delegations. `dsFutureGenDelegs` is never consulted by this check.
- `coreNodeQuorum <- liftSTS $ asks quorum` — pulled from `Globals.quorum`, which
  `mkShelleyGlobals` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Genesis.hs` L768-776) sets
  as `quorum = sgUpdateQuorum genesis` — a **static genesis constant** (mainnet = 5), never
  recomputed from the live `dsGenDelegs` map size. `validateGenesis`'s `checkQuorumSize` enforces
  at genesis-load time that `sgUpdateQuorum > length sgGenDelegs \`div\` 2` (strict majority of
  the *initial* genesis delegate count) — this is a load-time sanity check, not a per-tx
  recomputation.
- The check fires (`not (null mirCerts) ==> ...`) only when the tx actually contains an MIR cert;
  when it does, quorum is unconditionally required regardless of pot or amount.

### Era wiring — where the call site lives, and Conway removal

| Era | Call site | Notes |
|---|---|---|
| Shelley | `transitionRulesUTXOW` (`Shelley/Rules/Utxow.hs`) inline, unconditional | `genDelegs <- certState^.certDStateL; coreNodeQuorum <- asks quorum; runTest $ validateMIRInsufficientGenesisSigs ...` |
| Allegra/Mary | inherits Shelley `UTXOW` (no override found beyond era-specific TxBody wiring) | same code path |
| Alonzo | `alonzoStyleWitness` (`Alonzo/Rules/Utxow.hs`), inline, unconditional | calls `Shelley.validateMIRInsufficientGenesisSigs` explicitly by name |
| Babbage | **separate** `babbageUtxowMirTransition` sub-rule, chained via `transitionRules = [babbageUtxowMirTransition @era >> babbageUtxowTransition @era]` (`Babbage/Rules/Utxow.hs` L286-296, L413) | pulled out into its own `Rule ... 'Transition ()` |
| Conway | `transitionRules = [Babbage.babbageUtxowTransition @era]` (`Conway/Rules/Utxow.hs` L195) — **`babbageUtxowMirTransition` is NOT in the list** | check is absent, not merely vacuous |

Structural confirmation that Conway can't even express this: `isInstantaneousRewards :: (ShelleyEraTxCert era, AtMostEra "Babbage" era) => TxCert era -> Bool` (`Shelley/TxCert.hs`) is type-constrained to `AtMostEra "Babbage" era`, so it cannot be called for `ConwayEra` at all. `Conway/Rules/Utxow.hs`'s `shelleyToConwayUtxowPredFailure` maps
`Shelley.MIRInsufficientGenesisSigsUTXOW _xs -> error "Impossible: MIR has been removed in Conway"`
— dead code kept only for the injection typeclass's totality, never actually reachable.

**Conclusion for era applicability**: enforced Shelley through Babbage inclusive; **removed
(not merely dormant) starting Conway**, consistent with `ConwayTxCert` having no MIR constructor.

## Related
- [[project_dugite_genesisdeleg_mir_gaps_2026_07_06]] — two concrete dugite gaps found while
  answering this question: immediate-insert GenesisKeyDelegation (wrong "2x stabilityWindow" in
  its code comment) and a completely absent MIR quorum-witness check.
