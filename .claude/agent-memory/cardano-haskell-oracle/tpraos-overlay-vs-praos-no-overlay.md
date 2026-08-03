---
name: tpraos-overlay-vs-praos-no-overlay
description: Proof that the BFT overlay-schedule check (OBftSlot/d-parameter) lives ONLY in cardano-protocol-tpraos (Shelley..Alonzo); Praos (Babbage+) header validation has zero overlay/decentralization logic and its LedgerView carries no d field.
metadata:
  type: reference
---

# TPraos OVERLAY rule vs Praos — decisive source proof (oracle-verified 2026-08-03)

Researched for: justifying that a Rust node rejecting a Conway/Babbage block
via any overlay-schedule check is a divergence from Haskell (it should never
happen — Praos has no such check to divergently apply).

## Refs used
- cardano-ledger: `master` @ `4f7cb2d6874df70561e32147084ed82cee773e8a`
- ouroboros-consensus: tag `release-ouroboros-consensus-3.0.1.0` @
  `c87aa760001e60f0f0d3353f793eb089adb917e7` — this is the SHA that matches
  cardano-node 11.0.1's `.cabal` version bound (`^>= 3.0.1`), per
  [[praos-chain-order-v3-verified]]. Do not reuse an older per-package tag
  like `0.30.0.1` — structurally different code.

## 1. OVERLAY is a TPraos-only STS rule

`libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/Overlay.hs`
defines `OBftSlot = NonActiveSlot | ActiveSlot !(KeyHash GenesisRole)`,
`classifyOverlaySlot`, `lookupInOverlaySchedule` (gated by `UnitInterval` d
param + `isOverlaySlot`), and the `OVERLAY` STS whose `overlayTransition`
does: not-in-schedule -> `praosVrfChecks` (stake-weighted VRF leader check);
`NonActiveSlot` -> `failBecause $ NotActiveSlotOVERLAY slot`; `ActiveSlot
gkey` -> PBFT-style checks against the genesis delegate's cold/VRF keys.

`libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/Rules/Prtcl.hs`:
`PrtclPredicateFailure = OverlayFailure (PredicateFailure (OVERLAY c)) |
UpdnFailure ...`; `prtclTransition` does `cs' <- trans @(OVERLAY c) $ TRC
(OverlayEnv dval pd dms eta0, cs, bh)` — PRTCL is the parent STS and always
embeds OVERLAY.

Consumed from consensus side in
`ouroboros-consensus-protocol/.../Ouroboros/Consensus/Protocol/TPraos.hs`:
`import qualified Cardano.Protocol.TPraos.Rules.Overlay as SL` /
`...Rules.Prtcl as SL`; `checkIsLeader` does `case
SL.lookupInOverlaySchedule firstSlot gkeys d asc slot of ...`; and
`updateChainDepState cfg b slot cs = TPraosState (NotOrigin slot) <$>
SL.updateChainDepState (mkShelleyGlobals cfg) (tickedTPraosStateLedgerView
cs) b (tickedTPraosStateChainDepState cs)` — `SL.updateChainDepState` (from
`Cardano.Protocol.TPraos.API`) runs PRTCL, so every TPraos header
(Shelley/Allegra/Mary/Alonzo) goes through OVERLAY.

## 2. Praos (Babbage+) header validation has ZERO overlay logic

`ouroboros-consensus-protocol/.../Protocol/Praos.hs`,
`instance ConsensusProtocol (Praos c)`:

```haskell
updateChainDepState cfg@(PraosConfig PraosParams{praosLeaderF} _) b slot tcs = do
  validateKESSignature cfg lv (praosStateOCertCounters cs) b
  validateVRFSignature (praosStateEpochNonce cs) lv praosLeaderF b
  pure $ reupdateChainDepState cfg b slot tcs
```

`reupdateChainDepState` only updates `praosStateLastSlot`,
`praosStateLabNonce`, `praosStateEvolvingNonce`,
`praosStateCandidateNonce`, `praosStateOCertCounters` — pure nonce/opcert
bookkeeping, no genesis-key lookup, no `d`, no epoch-slot classification.

`PraosValidationErr` (the ENTIRE error type Praos header validation can
throw) has exactly 11 constructors, all VRF/KES/OCert:
`VRFKeyUnknown | VRFKeyWrongVRFKey | VRFKeyBadProof | VRFLeaderValueTooBig |
KESBeforeStartOCERT | KESAfterEndOCERT | CounterTooSmallOCERT |
CounterOverIncrementedOCERT | InvalidSignatureOCERT |
InvalidKesSignatureOCERT | NoCounterForKeyHashOCERT`. Nothing
overlay/genesis-delegate/decentralization shaped. Grepped the whole Praos
side (`Praos.hs`, `Praos/Views.hs`, `Praos/Header.hs`, `Praos/VRF.hs`,
`Praos/Common.hs`) for `overlay|obft|decentral` (case-insensitive): the only
hit is an unrelated doc-comment string "geographic decentralisation" in
`Common.hs` (the VRF-tiebreaker "Frankfurt problem" haddock, see
[[praos-chain-order-v3-verified]]). Zero code hits. Contrast: the same grep
on `TPraos.hs` hits 5 times (import + 2x `lookupInOverlaySchedule` call +
2x "overlay schedule" comments/error).

**Conclusion**: a Babbage/Conway block header cannot be rejected by any
overlay-schedule-shaped error in the real implementation — the code path
that could produce one (`OVERLAY`/`PRTCL`) is never invoked for `Praos c`
blocks. A Rust node that rejects a Conway header via an overlay-schedule
check is unconditionally divergent.

## 3. Praos LedgerView has no `d` field — contrast with TPraos's

`ouroboros-consensus-protocol/.../Protocol/Praos/Views.hs`:
```haskell
data LedgerView = LedgerView
  { lvPoolDistr :: SL.PoolDistr
  , lvMaxHeaderSize :: !Word16
  , lvMaxBodySize :: !Word32
  , lvProtocolVersion :: !ProtVer
  }
```
No decentralization param, no `GenDelegs`.

Contrast, `libs/cardano-protocol-tpraos/src/Cardano/Protocol/TPraos/API.hs`
(TPraos's `LedgerView`, used Shelley..Alonzo):
```haskell
data LedgerView = LedgerView
  { lvD :: !UnitInterval
  , lvExtraEntropy :: Nonce  -- comment: "this field is not present in Babbage..."
  , lvPoolDistr :: !PoolDistr
  , lvGenDelegs :: !GenDelegs
  , lvChainChecks :: !ChainChecksPParams
  }
```
The Haskell source's OWN comment on `lvExtraEntropy` says verbatim: "this
field is not present in Babbage, but we require this view in order to
construct the Babbage ledger view" — direct textual acknowledgment from
upstream that Babbage's ledger view is structurally missing these TPraos-only
fields (extra entropy AND, by the `Views.LedgerView` definition above, `d`
and `GenDelegs` too).

## 4. Era -> protocol wiring

`ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/HFEras.hs`:
```haskell
type StandardShelleyBlock = ShelleyBlock (TPraos StandardCrypto) ShelleyEra
type StandardAllegraBlock = ShelleyBlock (TPraos StandardCrypto) AllegraEra
type StandardMaryBlock    = ShelleyBlock (TPraos StandardCrypto) MaryEra
type StandardAlonzoBlock  = ShelleyBlock (TPraos StandardCrypto) AlonzoEra
type StandardBabbageBlock = ShelleyBlock (Praos StandardCrypto) BabbageEra
type StandardConwayBlock  = ShelleyBlock (Praos StandardCrypto) ConwayEra
type StandardDijkstraBlock = ShelleyBlock (Praos StandardCrypto) DijkstraEra
```
One `ShelleyCompatible (TPraos c) BabbageEra` instance also exists but is
explicitly commented as a forecast-plumbing leftover ("the ledger view
forecast function for Praos/Babbage still goes through the forecast for
TPraos") — it does NOT mean Babbage blocks are validated under TPraos; the
actual `StandardBabbageBlock`/`StandardConwayBlock` type aliases (what
`ouroboros-consensus-cardano` actually forges/validates against) are fixed
to `Praos`.
