---
name: haa-outbound-connections-state-verified
description: Exact, commit-pinned verification of outboundConnectionsState / HAA / bootstrap-peers trusted-only closure in ouroboros-network cardano-diffusion, superseding the "high confidence, not tag-pinned" caveat in gsm-haa-syncing-presyncing-regression.md
type: reference
---

## Exact pins (cardano-node 11.0.1, resolved via CHaP meta.toml, not guessed)

cardano-node 11.0.1 `cardano-node.cabal` (line 190-191):
`ouroboros-network ^>= 1.1`, `cardano-diffusion ^>=1.0`.

Resolved via `IntersectMBO/cardano-haskell-packages` `_sources/<pkg>/<ver>/meta.toml`:
- `cardano-diffusion-1.0.0.0` -> `github { repo = "intersectmbo/ouroboros-network", rev = "17525c337a6209011fabd6916fa757efd44df6f8" }`, subdir `cardano-diffusion`, timestamp 2026-03-06/07.
- `ouroboros-network-1.1.0.0` -> rev `a98c88583fa27ac4e567095f8766216442cbb74d`, subdir `ouroboros-network`, timestamp 2026-03-13. (Same repo, close-in-time commit — consistent snapshot.)

Both predate the 11.0.1 release date (2026-05-05) and are the sole `1.0.x`/`1.1.x` entries in CHaP, so these are the exact commits cardano-node 11.0.1 built against. This supersedes the earlier "high confidence, main HEAD, no clean tag found" caveat in [[gsm-haa-syncing-presyncing-regression]] — all quotes below are byte-exact at these commits.

## 1. `outboundConnectionsState` — verbatim, `cardano-diffusion/lib/Cardano/Network/PeerSelection/Governor/Types.hs` (rev a98c885)

```haskell
outboundConnectionsState
    associationMode
    PeerSelectionView {
      viewEstablishedPeers       = (viewEstablishedPeers, _),
        viewActiveBigLedgerPeers = (_, activeNumBigLedgerPeers),
      viewExtraViews = Cardano.ExtraPeerSelectionSetsWithSizes {
        viewEstablishedBootstrapPeers = (viewEstablishedBootstrapPeers, _),
        viewActiveBootstrapPeers      = (viewActiveBootstrapPeers, _)
      }
    }
    PeerSelectionState {
      localRootPeers,
      extraState = Cardano.ExtraState {
        Cardano.consensusMode,
        Cardano.bootstrapPeersFlag,
        Cardano.minNumberOfBigLedgerPeers
      }
    }
    =
    case (associationMode, bootstrapPeersFlag, consensusMode) of
      (LocalRootsOnly, _, _)
        |  viewEstablishedPeers `Set.isSubsetOf` trustableLocalRootSet
        -> TrustedStateWithExternalPeers
        |  otherwise -> UntrustedState

      (Unrestricted, UseBootstrapPeers {}, _)
        |  viewEstablishedPeers `Set.isSubsetOf` (viewEstablishedBootstrapPeers <> trustableLocalRootSet)
        ,  not (Set.null viewActiveBootstrapPeers)
        -> TrustedStateWithExternalPeers
        |  otherwise -> UntrustedState

      (Unrestricted, DontUseBootstrapPeers, PraosMode)
        -> UntrustedState

      (Unrestricted, DontUseBootstrapPeers, GenesisMode)
        |  activeNumBigLedgerPeers >= Cardano.getNumberOfBigLedgerPeers minNumberOfBigLedgerPeers
        -> TrustedStateWithExternalPeers
        |  otherwise -> UntrustedState
  where
    trustableLocalRootSet = LocalRootPeers.trustableKeysSet localRootPeers
```

`OutboundConnectionsState` type + Haddock, `cardano-diffusion/api/lib/Cardano/Network/PeerSelection/LocalRootPeers.hs`:
```haskell
data OutboundConnectionsState =
    TrustedStateWithExternalPeers
    -- * in Praos mode: connected only to trusted local peers and at least
    --   one bootstrap peer or public root;
    -- * in Genesis mode: meeting target of active big ledger peers;
    -- * or LocalRootsOnly mode and only connected to trusted local roots.
  | UntrustedState -- catch all other cases
```

Case split is 4-way and **each branch independent** (not layered) — confirms prior memory verbatim, now at exact pin.

## 2. Genesis + DontUseBootstrapPeers — ACTIVE big ledger peers only, quorum not closure

`activeNumBigLedgerPeers` comes from `viewActiveBigLedgerPeers` (the size component, i.e. HOT/active, not established) in `PeerSelectionView`. Confirmed via `peerSelectionStateToView` in base `ouroboros-network/lib/Ouroboros/Network/PeerSelection/Governor/Types.hs` (rev a98c885): `activeBigLedgerPeersSet = establishedBigLedgerPeersSet `Set.intersection` activePeers`.

Threshold type + doc, `cardano-diffusion/lib/Cardano/Network/PeerSelection/Governor/PeerSelectionState.hs` (rev 17525c3):
```haskell
-- | Minimum number of hot big ledger peers in Genesis mode
--   for trusted state to be signalled to Consensus. This number
--   should be smaller than the `targetNumberOfActiveBigLedgerPeers`
--   but greater than 1. In Genesis, we may demote a big ledger peer
--   for underperformance, but not promote a replacement immediately
--   to guard against adversaries which may want to slow down our
--   progress.
newtype NumberOfBigLedgerPeers = NumberOfBigLedgerPeers { getNumberOfBigLedgerPeers :: Int }
```
Default, `cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs`:
```haskell
defaultNumberOfBigLedgerPeers :: NumberOfBigLedgerPeers
defaultNumberOfBigLedgerPeers = NumberOfBigLedgerPeers 5
```
Same file, `defaultSyncTargets` (= the `genesisPeerSelectionTargets` used by `Monitor.targetPeers` when `(TooOld, GenesisMode)`):
```haskell
defaultSyncTargets = PeerSelectionTargets
  { targetNumberOfRootPeers = 0, targetNumberOfKnownPeers = 150
  , targetNumberOfEstablishedPeers = 10, targetNumberOfActivePeers = 5
  , targetNumberOfKnownBigLedgerPeers = 100
  , targetNumberOfEstablishedBigLedgerPeers = 40
  , targetNumberOfActiveBigLedgerPeers = 30 }
```
So while syncing in Genesis mode, established-BLP target is 40 but only 5 need be **active/hot** for HAA. Established-but-not-yet-active BLPs do NOT count — a node with 40 established, 0 active big ledger peers is `UntrustedState`/HAA-not-satisfied.

## 3. UseBootstrapPeers + TooOld — trustable-only restriction mechanics, `Governor/Monitor.hs` (rev 17525c3)

Two independent, MODE-AGNOSTIC clamps (fire in both Praos and Genesis, since `requiresBootstrapPeers ubp TooOld = isBootstrapPeersEnabled ubp` doesn't check consensusMode):

**`localRoots`** (reads topology file) and **`targetPeers`** (reacts to churn-set targets) both do:
```haskell
localRootPeers' = LocalRootPeers.clampToLimit targetNumberOfKnownPeers
                 . (if usingBootstrapPeers then LocalRootPeers.clampToTrustable else id)
                 $ localRootPeers
```
where `usingBootstrapPeers = requiresBootstrapPeers bootstrapPeersFlag ledgerStateJudgement`. This drops non-`IsTrustable` entries OUT of the local-root-peers config; it does not touch already-established connections to them.

**Explicit doc comment on `localRoots`** (verbatim) — this is the direct answer to "how are existing established non-trustable peers handled":
> "if the current ledger state is TooOld we can only trust our trustable local root peers... if the node decided to reconfigure the local root peers... we will keep a connection to it **until the outbound governor notices it and disconnects from it**."

I.e. **not synchronous** — reconfiguration of the target/local-root config happens in one governor tick, actual teardown of the now-untrusted established connection happens via the ordinary target-vs-actual reconciliation in a LATER tick (`EstablishedPeers.aboveTarget`/`ActivePeers.aboveTarget`, not shown in Monitor.hs). A transient window where `outboundConnectionsState` returns `UntrustedState` (or, worse, is evaluated while a stale non-trustable peer is still established) is expected, not prevented.

**PraosMode-only extra teardown** — `monitorLedgerStateJudgement`'s `TooOld` branch (only fires `| PraosMode <- consensusMode, isBootstrapPeersEnabled bootstrapPeersFlag`) zeroes ALL targets to 0 and sets `localRootPeers = LocalRootPeers.empty`, forcing full teardown-then-rebuild:
```haskell
TooOld -> st { targets = PeerSelectionTargets{..all 0..}
             , localRootPeers = LocalRootPeers.empty
             , extraState = cpst { ledgerStateJudgement = lsj, hasOnlyBootstrapPeers = False
                                  , bootstrapPeersTimeout = Just (addTime governor_BOOTSTRAP_PEERS_TIMEOUT now) } }
```
**GenesisMode's branch of the SAME function does NOT do this** — it only updates `ledgerStateJudgement`:
```haskell
| GenesisMode <- consensusMode = Guarded Nothing $ do
    lsj <- readLedgerStateJudgement
    check (lsj /= ledgerStateJudgement)
    return $ \_now -> Decision { decisionState = st { extraState = cpst { ledgerStateJudgement = lsj } }, .. }
```
So in Genesis mode, TooOld does NOT zero ordinary peer targets — it relies on `targetPeers`' separate switch to `genesisPeerSelectionTargets` (still 10 established/5 active ordinary peers, non-zero) plus the BLP-active-count HAA branch, which is entirely independent of local-root trustability. **There is no "all established peers must be trustable" closure enforced in GenesisMode+DontUseBootstrapPeers at all** (branch 4 of `outboundConnectionsState` ignores `viewEstablishedPeers`/local-root-trust completely).

**`hasOnlyBootstrapPeers` / closure verification** — `Governor/Monitor.hs` `waitForSystemToQuiesce`:
```haskell
waitForSystemToQuiesce st
  | requiresBootstrapPeers bootstrapPeersFlag ledgerStateJudgement
  , not hasOnlyBootstrapPeers
  , all (\case LocalRootConfig{extraLocalRootFlags=IsTrustable} -> True; _ -> False) (LocalRootPeers.toMap localRootPeers)
  , KnownPeers.toSet knownPeers' `Set.isSubsetOf`
      (PublicRootPeers.getBootstrapPeers publicRootPeers' <> LocalRootPeers.keysSet (clampToTrustable localRootPeers'))
  , Set.null inProgressPromoteCold, Set.null inProgressPromoteWarm
  , inProgressPeerShareReqs == 0, not inProgressBigLedgerPeersReq, not inProgressPublicRootsReq
  = ... decisionState = st { hasOnlyBootstrapPeers = True, bootstrapPeersTimeout = Nothing }
  | otherwise = GuardedSkip bootstrapPeersTimeout
```
This is **mode-agnostic too** (no Praos/Genesis pattern guard) and continuously re-evaluated every governor tick — it's not a one-shot sweep, it's the standing convergence predicate that flips `hasOnlyBootstrapPeers` once ALL of: known peers ⊆ bootstrap∪trustable-local, local roots all trustable, zero in-flight promotions. `hasOnlyBootstrapPeers=True` is itself a gate (`isNodeAbleToMakeProgress`) that unblocks `localRoots`/`targetPeers` from acting again (they're blocked — `GuardedSkip` — while `requiresBootstrapPeers && not hasOnlyBootstrapPeers`).

**15-minute crash guard is PraosMode-only**: `bootstrapPeersTimeout` is set ONLY inside `monitorLedgerStateJudgement`'s `PraosMode`+TooOld branch (`governor_BOOTSTRAP_PEERS_TIMEOUT = 15*60`) and consumed by `abortGovernor` in `Governor/Types.hs` (`cardanoPeerSelectionGovernorArgs`) which throws `BootstrapPeersCriticalTimeoutError`, crashing the node. **GenesisMode never sets `bootstrapPeersTimeout`** — so a Genesis-mode node that never converges to `hasOnlyBootstrapPeers` simply never crashes from this guard (it's stuck relying on the independent BLP-active-count branch instead). Real divergence risk if a Rust reimplementation copies the 15-min abort into Genesis mode, or omits it from Praos mode.

## 4. Inbound duplex connections — NOT auto-counted in `viewEstablishedPeers`

`viewEstablishedPeers` derives from `PeerSelectionState.establishedPeers`, which is state the OUTBOUND governor itself owns and mutates only via its own promote/demote actions (`EstablishedPeers.belowTarget`/`aboveTarget`, `Governor/Monitor.hs::connections`). Inbound-originated connections are governed separately by `InboundGovernor` and are NOT written into this state merely by being accepted.

The bridge is `PeerSelectionActions.readInboundPeers :: m (Map peeraddr PeerSharing)` ("Read inbound peers which negotiated duplex connection", base `Governor/Types.hs`), consumed ONLY here in `Governor.hs`'s main loop:
```haskell
inboundPeers <- readInboundPeers actions
...
<> KnownPeers.belowTarget enableProgressMakingActions actions blockedAt inboundPeers policy st
```
i.e. an inbound duplex peer is merged into `KnownPeers` (a candidate pool) when the governor is below its known-peers target — it still has to go through the ordinary, non-trust-checked `EstablishedPeers.belowTarget`/promotion path to become `established`, exactly like any other known peer, and the connection is *reused* (same duplex bearer) rather than a new one dialed. There is no special-case that either (a) auto-promotes inbound duplex peers into `establishedPeers`, or (b) tags them trustable. Consequence: during Genesis-mode TooOld (targets not zeroed, no trust closure enforced — see #3), an inbound-dialed duplex peer CAN legitimately end up counted in `viewEstablishedPeers`/the trust closure exactly like any outbound-dialed one; during Praos+UseBootstrapPeers TooOld it's blocked by the same zeroed-targets/`hasOnlyBootstrapPeers` gate that blocks all other non-trustable promotion.

## 5. Violation handling — eventual convergence, not synchronous invariant enforcement

No code path snapshot-checks-and-rejects at connection-establishment time. `outboundConnectionsState` is a pure read of current governor state, recomputed every `peerSelectionGovernorLoop` iteration (`Governor.hs`: `peerSelectionView = peerSelectionStateToView st''` then `updateWithState ... (outboundConnectionsState associationMode psv st)` — this pushes the value into `Cardano.updateOutboundConnectionsState`, which NodeKernel wires to `varOutboundConnectionsState`, which `isHaaSatisfied` reads). So a transient `UntrustedState` reading (feeding `isHaaSatisfied = False` into consensus, tripping `Syncing -> PreSyncing` immediately per GSM.hs — see [[gsm-haa-syncing-presyncing-regression]]) is expected and normal during any TooOld-edge reconfiguration window, bounded only by how many governor ticks it takes for `EstablishedPeers.aboveTarget`/`ActivePeers.aboveTarget` to actually tear down the newly-non-trustable connections. There is no separate "detect violation, log warning, force GSM reset" mechanism beyond this ordinary reactive loop — the GSM state machine itself is the passive consumer, not an enforcer.

## 6. `isHaaSatisfied` wiring — exact quote, `NodeKernel.hs` (ouroboros-consensus, tag `release-ouroboros-consensus-diffusion-0.24.0.0`)

```haskell
, GSM.isHaaSatisfied = do
    readTVar varOutboundConnectionsState <&> \case
      TrustedStateWithExternalPeers -> True
      UntrustedState -> False
```
Direct, unconditional boolean map — confirms the diffusion-layer `OutboundConnectionsState` IS the consensus-layer HAA signal, no additional debounce/hysteresis on the consensus side (all debounce, if any, lives in how fast the governor's own state converges, per #5).

## 7. PreSyncing does NOT stop ChainSync/BlockFetch — only 3 concrete effects

Verified in `ouroboros-consensus/.../MiniProtocol/ChainSync/Client.hs` (same tag): `cschOnGsmStateChanged = updateLopBucketConfig lopBucket`, and:
```haskell
lopBucketConfig gsmState = case (gsmState, csBucketConfig) of
  (Syncing, ChainSyncLoPBucketEnabled cfg) -> LeakyBucket.Config { capacity = csbcCapacity cfg, rate = csbcRate cfg, onEmpty = throwIO EmptyBucket, .. }
  (_, ChainSyncLoPBucketDisabled)          -> LeakyBucket.dummyConfig
  (PreSyncing, ChainSyncLoPBucketEnabled _) -> LeakyBucket.dummyConfig   -- LoP DISABLED in PreSyncing
  (CaughtUp,   ChainSyncLoPBucketEnabled _) -> LeakyBucket.dummyConfig
```
The ChainSync client threads themselves are NOT paused, killed, or reconnected on any GSM transition — `cschOnGsmStateChanged` is the *only* GSM-state hook into the ChainSync client, and it just reconfigures the LoP leaky bucket (disabled outside Syncing). No `GsmState` reference exists anywhere in NodeKernel's BlockFetch wiring (grepped — none). So while `PreSyncing`:
1. Headers keep arriving and get appended to each peer's `csCandidate` fragment as normal (ChainSync protocol keeps running unmodified).
2. BlockFetch keeps downloading blocks as normal (untouched by GSM state).
3. Consensus-side effects are: (a) `getLoEFragment` returns `LoEEnabled (Empty AnchorGenesis)` — per [[loe-chain-selection]]'s corrected analysis this still permits up to `k` blocks of selection past the immutable tip via `trimToLoE`'s case-1 branch, it is NOT a hard freeze; (b) GDD's `gddWatcher` stops evaluating density-disconnect (`wNotify (GDDPreSyncing) = pure ()`) — no adversarial-peer disconnection logic runs; (c) LoP patience-timeout enforcement is disabled (peers can idle indefinitely without being killed for it).

## Pitfalls for Dugite (`crates/dugite-node/src/gsm.rs`, `crates/dugite-node/src/node/networking.rs`)

- The from-genesis freeze root-caused in [[gsm-haa-syncing-presyncing-regression]] is now doubly confirmed: dugite's governor must implement the Praos-vs-Genesis **mode split**, not just the bootstrap-flag split. A single "restrict to trustable while TooOld+UseBootstrapPeers" clamp is correct for Praos AND for the `UseBootstrapPeers` branch of Genesis, but Genesis+`DontUseBootstrapPeers` (the real mainnet/preview/preprod default) must use the ACTIVE-BLP-count branch and must NOT also demand an established-peer trust closure — the two are genuinely alternative, independent code paths in Haskell, selected by a 4-way case split, not layered AND-conditions.
- Do not zero ordinary peer targets on TooOld in Genesis mode — only Praos does that. Genesis mode switches to `defaultSyncTargets` (10 established/5 active ordinary, 40 established/30 active BLP, still non-zero).
- Do not port the 15-minute `BootstrapPeersCriticalTimeoutError` abort into Genesis-mode code paths — canonical Haskell only arms it in Praos.
- Any dugite equivalent of `hasOnlyBootstrapPeers`/closure-verification should be a continuously-reevaluated predicate over live state (known peers ⊆ trustable∪bootstrap, zero in-flight promotions), not a one-shot check at a state-transition edge — and it is legitimate/expected for the trust closure to be transiently false for multiple ticks after a TooOld edge.
- Inbound-accepted duplex connections should NOT be blindly added to whatever set dugite uses for its own established-peers/trust view; they should only count once dugite's own outbound-governor-equivalent logic separately promotes that address from its known-peers pool, mirroring `readInboundPeers` -> `KnownPeers.belowTarget` -> ordinary promotion.
- PreSyncing in dugite must not pause ChainSync/BlockFetch tasks — only LoE-fragment/GDD/LoP behavior changes.

See also [[gsm-haa-syncing-presyncing-regression]], [[gdd-governor-deep-dive]], [[loe-chain-selection]], [[p2p-governor-architecture]].
