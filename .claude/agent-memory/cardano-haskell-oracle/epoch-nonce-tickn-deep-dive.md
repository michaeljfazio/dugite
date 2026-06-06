---
name: epoch-nonce-tickn-deep-dive
description: Complete authoritative analysis of TICKN/UPDN epoch nonce update — formula, freeze window per era, Byron→Shelley seeding, prevHashNonce semantics, and dugite divergence verdict
type: reference
---

## Source locations (verified against main/master branches)

- TICKN rule: `cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/Tickn.hs`
- UPDN rule: `cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/Updn.hs`
- StabilityWindow: `cardano-ledger/eras/shelley/impl/src/Cardano/Ledger/Shelley/StabilityWindow.hs`
- Praos nonce state: `ouroboros-consensus/ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos.hs`
- TPraos nonce state: `ouroboros-consensus/ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/TPraos.hs`
- Byron→Shelley translation: `ouroboros-consensus/ouroboros-consensus-cardano/src/ouroboros-consensus-cardano/Ouroboros/Consensus/Cardano/CanHardFork.hs` (translateChainDepStateByronToShelley)
- Praos per-era config: `ouroboros-consensus/ouroboros-consensus-cardano/src/ouroboros-consensus-cardano/Ouroboros/Consensus/Cardano/Node.hs` (partialConsensusConfigBabbage etc.)
- prevHashToNonce: `cardano-ledger/libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/BHeader.hs`
- Nonce ⭒ operator: `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs`

---

## 1. The eta0 Formula (TICKN rule)

**Haskell source** (Tickn.hs tickTransition):
```haskell
if newEpoch
  then TicknState
    { ticknStateEpochNonce = ηc ⭒ ηh ⭒ extraEntropy  -- new η0
    , ticknStatePrevHashNonce = ηph                     -- new ηh
    }
  else st
```

Where:
- `ηc` = `ticknEnvCandidateNonce` = `PrtclState.candidateNonce` (the frozen candidate)
- `ηh` = `ticknState.ticknStatePrevHashNonce` = the PREVIOUS epoch's prevHashNonce (OLD value)
- `ηph` = `ticknEnvHashHeaderNonce` = `csLabNonce` = new ηh set for NEXT epoch computation
- `extraEntropy` = `ticknEnvExtraEntropy` = from PParams (usually NeutralNonce)

**The ⭒ operator** (BaseTypes.hs):
```haskell
Nonce a ⭒ Nonce b = Nonce (blake2b_256(a_bytes || b_bytes))
x ⭒ NeutralNonce = x
NeutralNonce ⭒ x = x
```

**Full formula**: `η0_N = candidateNonce_N ⭒ prevHashNonce_N ⭒ extraEntropy_N`

---

## 2. The candiate nonce freeze window — DEFINITIVE PER-ERA VALUES

### TPraos (Shelley, Allegra, Mary, Alonzo) — uses UPDN rule

**UPDN rule** (Updn.hs):
```haskell
sp <- liftSTS $ asks stabilityWindow  -- from Globals
...
etaC' = if s +* Duration sp < firstSlotNextEpoch
            then etaV'   -- update: slot is OUTSIDE freeze zone
            else eta_c   -- freeze: slot + sp >= firstSlotNextEpoch
```

**`stabilityWindow` = `computeStabilityWindow k f = ceiling(3k/f)`** (3k/f)

### Praos (Babbage) — uses reupdateChainDepState in Praos.hs

```haskell
reupdateChainDepState ... PraosParams{praosRandomnessStabilisationWindow} ...
  candidateNonce = if slot +* Duration praosRandomnessStabilisationWindow < firstSlotNextEpoch
                     then newEvolvingNonce
                     else praosStateCandidateNonce cs
```

**For Babbage specifically** (Node.hs partialConsensusConfigBabbage):
```haskell
partialConsensusConfigBabbage =
  praosParams
    { -- For Praos in Babbage (just as in all TPraos eras) we use the
      -- smaller (3k/f vs 4k/f slots) stability window here for
      -- backwards-compatibility. See erratum 17.3 in the Shelley ledger specs.
      praosRandomnessStabilisationWindow =
          SL.computeStabilityWindow k f  -- 3k/f!
    }
```

**`praosRandomnessStabilisationWindow` for Babbage = `computeStabilityWindow = ceiling(3k/f)`** (3k/f)

### Praos (Conway, Dijkstra) — default praosParams

```haskell
praosParams = PraosParams {
  praosRandomnessStabilisationWindow =
      SL.computeRandomnessStabilisationWindow k f  -- 4k/f
  ...
}
partialConsensusConfigConway = praosParams  -- inherits 4k/f
```

**`praosRandomnessStabilisationWindow` for Conway/Dijkstra = `computeRandomnessStabilisationWindow = ceiling(4k/f)`** (4k/f)

### Summary table
| Era        | Protocol  | Freeze window |
|------------|-----------|---------------|
| Shelley    | TPraos    | 3k/f          |
| Allegra    | TPraos    | 3k/f          |
| Mary       | TPraos    | 3k/f          |
| Alonzo     | TPraos    | 3k/f          |
| Babbage    | Praos     | 3k/f          |
| Conway+    | Praos     | 4k/f          |

**The 4k/f change happens at Conway, NOT at Babbage.** Dugite correctly uses 3k/f for Shelley/Allegra/Mary/Alonzo/Babbage and 4k/f for Conway+.

---

## 3. The candidateNonce state per block (UPDN / reupdateChainDepState)

### TPraos (UPDN rule, Updn.hs):
```haskell
UpdnState eta_v eta_c
sp = stabilityWindow  -- 3k/f
eta = bnonce bh  -- = mkNonceFromOutputVRF(certifiedOutput(bheaderEta)) = blake2b_256(raw_64_vrf_bytes)
etaV' = eta_v ⭒ eta
etaC' = if s + sp < firstSlotNextEpoch then etaV' else eta_c
```

### Praos (reupdateChainDepState, Praos.hs):
```haskell
eta = vrfNonceValue (Proxy @c) (hvVrfRes b)
-- vrfNonceValue = Nonce . castHash . hashWith id . hashToBytes . hashVRF SVRFNonce
--               = blake2b_256(blake2b_256("N" || raw_vrf_output))  -- DOUBLE hash
newEvolvingNonce = praosStateEvolvingNonce cs ⭒ eta
candidateNonce' = if slot + rsw < firstSlotNextEpoch then newEvolvingNonce else praosStateCandidateNonce
```

### VRF nonce contribution (eta) derivation
- **TPraos**: `bnonce = blake2b_256(raw_64_vrf_output)` (single hash)
  - dugite stores raw 64 bytes in `nonce_vrf_output`, then does `blake2b_256(nonce_vrf_output)` = correct
- **Praos**: `vrfNonceValue = blake2b_256(blake2b_256("N" || raw_vrf_output))` (double hash)
  - dugite stores `blake2b_256("N" || raw_output)` in `nonce_vrf_output`, then does `blake2b_256(nonce_vrf_output)` = correct (double hash achieved)

---

## 4. prevHashNonce (η_h / ticknStatePrevHashNonce / labNonce) semantics

### What is labNonce / csLabNonce?

```haskell
-- After applying block B:
csLabNonce = prevHashToNonce(bheaderPrev(bhbody bh))
-- prevHashToNonce (BlockHash ph) = Nonce (castHash ph)  -- just reinterpret bytes
-- prevHashToNonce GenesisHash    = NeutralNonce
```

`csLabNonce` after applying block B = nonce derived from **B's prevHash field** = the header hash of the block BEFORE B.

### When ticknStatePrevHashNonce is updated (TICKN):
```haskell
ticknStatePrevHashNonce_NEW = csLabNonce  (= ηph)
-- This is the csLabNonce AT TICK TIME = prevHash of LAST APPLIED BLOCK before tick
-- = hash of the SECOND-TO-LAST block of the ending epoch
```

More precisely: at the TICK for the first block of epoch N+1:
- `csLabNonce` = prevHashToNonce(last_block_of_epoch_N.prevHash)
               = the header hash bytes of the second-to-last block of epoch N
- This becomes `ticknStatePrevHashNonce` for epoch N+1

### Byron→Shelley initialization

```haskell
translateChainDepStateByronToShelley TPraosConfig{tpraosParams} pbftState =
  TPraosState (PBftState.lastSignedSlot pbftState) $
    SL.ChainDepState
      { SL.csProtocol = SL.PrtclState Map.empty nonce nonce
      , SL.csTickn =
          SL.TicknState
            { SL.ticknStateEpochNonce = nonce          -- initNonce
            , SL.ticknStatePrevHashNonce = SL.NeutralNonce  -- ← NeutralNonce!
            }
      , SL.csLabNonce = SL.NeutralNonce  -- ← NeutralNonce!
      }
 where nonce = tpraosInitialNonce tpraosParams  -- = genesisHashToPraosNonce(shelleyGenesisHash)
```

Initial state: `ticknStatePrevHashNonce = NeutralNonce`, `csLabNonce = NeutralNonce`.

---

## 5. Epoch-by-epoch nonce evolution for preprod (shelley_transition_epoch=4)

| Event                          | ticknStatePrevHashNonce | Haskell behavior                   |
|-------------------------------|-------------------------|-------------------------------------|
| Byron→Shelley init             | NeutralNonce            | Initial = NeutralNonce              |
| TICK for first block of ep4    | NeutralNonce (input ηh) | η0_4 = genesis_hash; ηh→NeutralNonce|
| After ep4 blocks processed     | NeutralNonce (current)  | csLabNonce ≠ neutral now            |
| TICK for first block of ep5    | NeutralNonce (OLD ηh)   | η0_5 = candidate_4 ⭒ NeutralNonce  |
| prevHashNonce SET at ep4→ep5   | prevHash(B_last_ep4)    | csLabNonce → hash of last ep4 block |
| TICK for first block of ep6    | prevHash(B_last_ep4)    | η0_6 = candidate_5 ⭒ prevHashNonce |
| ...                            | ...                     | ...                                 |

**KEY FACT**: prevHashNonce = NeutralNonce (0) at BOTH ep4 AND ep5 boundaries is CORRECT per Haskell. The non-zero value first appears at ep6 (= 331575dc in the logs). This matches dugite.

---

## 6. Verdict on dugite divergence

### What is correct:
1. **combine_nonce (⭒ operator)**: Correct. NeutralNonce=ZERO as identity. ✓
2. **freeze condition direction**: `slot + sw < firstSlotNext` → update, otherwise freeze. ✓
3. **freeze window values**: 3k/f for Shelley–Babbage, 4k/f for Conway+. ✓
4. **prevHashNonce = 0 at ep4 and ep5 on preprod**: CORRECT per Haskell. NOT a bug.
5. **lab_nonce = ZERO for Byron blocks**: Correct (mirrors NeutralNonce csLabNonce). ✓
6. **nonce_vrf_output extraction**: TPraos raw 64 bytes (then hashed), Praos pre-hashed with "N". ✓
7. **epoch_nonce_for_slot forecast**: Mirrors process_epoch_transition formula. ✓

### Possible remaining divergence for ep7→ep8 on preprod:

The most likely cause of VRF failure is NOT in the formula but in the **candidate nonce** being wrong. If the freeze window fired at the wrong time (e.g., `first_slot_of_next_epoch` was computed incorrectly at one point in the chain, causing candidate to freeze too early or too late), then candidate_7 ≠ Haskell's candidate_7.

The `first_slot_of_shelley_epoch` function was the site of a KNOWN BUG (see commit message "stability-window freeze never fired on mainnet") where it was computing slot offsets wrong for chains with `shelley_transition_epoch > 0`. If any version before that fix was used for the preprod sync, candidate nonces would have diverged from epoch 5 onward.

To diagnose: add `frozen = (candidate_nonce != evolving_nonce)` to the per-block nonce logs at the LAST FEW SLOTS of epoch 7 (slots near `firstSlotEpoch8 - 129600 = 1814400 - 129600 = 1684800`). If dugite and Haskell's candidate_7 differ, that's the bug. Compare with `cardano-cli debug log-epoch-state` dumps.

Also verify: the `extra_entropy` field should be NeutralNonce for all preprod epochs < 259. If the PP update decode incorrectly set extra_entropy to non-zero at some epoch, that would corrupt all subsequent epoch nonces.
