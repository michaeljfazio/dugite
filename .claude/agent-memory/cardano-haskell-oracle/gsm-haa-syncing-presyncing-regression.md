---
name: gsm-haa-syncing-presyncing-regression
description: Exact Haskell GSM Syncing<->PreSyncing transition (HAA-only, no tip-age term) and OutboundConnectionsState trust predicate (4-way case split); root cause of dugite from-genesis mainnet freeze
type: reference
---

## Canonical source (pinned)

- `ouroboros-consensus` tag `release-ouroboros-consensus-diffusion-0.24.0.0` (SHA `effb0a5c924f`, what cardano-node 11.0.1 pins):
  - `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Node/GSM.hs`
  - `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Node/GsmState.hs`
  - `ouroboros-consensus-diffusion/.../Ouroboros/Consensus/NodeKernel.hs` (`isHaaSatisfied`, `varOutboundConnectionsState`, `getLoEFragment`)
  - `ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Node.hs` (`updateOutboundConnectionsState`)
- `ouroboros-network` `main` HEAD (no clean per-package tag found for cardano-diffusion ^>=1.0; module path + Haddock cross-verified against the pinned NodeKernel.hs import, high confidence):
  - `cardano-diffusion/api/lib/Cardano/Network/PeerSelection/LocalRootPeers.hs` (`OutboundConnectionsState`)
  - `cardano-diffusion/lib/Cardano/Network/PeerSelection/Governor/Types.hs` (`outboundConnectionsState`)
  - `cardano-diffusion/api/lib/Cardano/Network/PeerSelection/Bootstrap.hs` (`requiresBootstrapPeers`)
  - `cardano-diffusion/lib/Cardano/Network/PeerSelection/Governor/Monitor.hs` (`targetPeers`, `localRoots` — trusted-only clamping)
  - `cardano-diffusion/lib/Cardano/Network/Diffusion/Configuration.hs` (`defaultNumberOfBigLedgerPeers = 5`)

## GSM state machine — exact transitions

`GsmState = PreSyncing | Syncing | CaughtUp` (doc-comment: distinguished *solely* by HAA).

| Edge | Condition | Debounce |
|---|---|---|
| `PreSyncing -> Syncing` | `isHaaSatisfied` true | none |
| `Syncing -> PreSyncing` | `isHaaSatisfied` false | **none — immediate**, no tip-age term at all |
| `Syncing -> CaughtUp` | all ChainSync peers idle AND no candidate better than selection (`blockUntilCaughtUp`) | none |
| `CaughtUp -> PreSyncing` | `durationUntilTooOld selection` (tip older than `maxCaughtUpAge`) | `minCaughtUpDuration` floor + 0-300s anti-thundering-herd jitter |

`GSM.hs` `enterSyncing'` races `blockWhileHonestAvailabilityAssumption` (`check . not =<< isHaaSatisfied`) against `blockUntilCaughtUp` via `orElse`. **Tip-staleness (`maxCaughtUpAge`/`durationUntilTooOld`) and `gsmMinCaughtUpDuration` gate ONLY the `CaughtUp <-> PreSyncing` edge, never `Syncing -> PreSyncing`.** Module comment confirms verbatim: "Syncing ⟶ PreSyncing: The Honest Availability Assumption is no longer satisfied" — full stop, no other clause.

LoE: `getLoEFragment` returns `Empty AF.AnchorGenesis` while `PreSyncing`, live shared-candidate-prefix while `Syncing`, `LoEDisabled` while `CaughtUp` — confirms PreSyncing genuinely freezes chain selection (architecturally a freeze during multi-million-block bulk sync even though `trimToLoE`'s k-cushion technically allows some slack).

## OutboundConnectionsState trust predicate — 4-way case split (verbatim, `outboundConnectionsState` in Governor/Types.hs)

```haskell
case (associationMode, bootstrapPeersFlag, consensusMode) of
  (LocalRootsOnly, _, _)
    | viewEstablishedPeers `Set.isSubsetOf` trustableLocalRootSet -> TrustedStateWithExternalPeers
    | otherwise -> UntrustedState
  (Unrestricted, UseBootstrapPeers {}, _)
    | viewEstablishedPeers `Set.isSubsetOf` (viewEstablishedBootstrapPeers <> trustableLocalRootSet)
    , not (Set.null viewActiveBootstrapPeers)
    -> TrustedStateWithExternalPeers
    | otherwise -> UntrustedState
  (Unrestricted, DontUseBootstrapPeers, PraosMode) -> UntrustedState  -- always, unconditionally
  (Unrestricted, DontUseBootstrapPeers, GenesisMode)
    | activeNumBigLedgerPeers >= minNumberOfBigLedgerPeers -> TrustedStateWithExternalPeers
    | otherwise -> UntrustedState
```

Key point: **each branch is independent, not layered**. Genesis-mode-without-bootstrap-peers (the real mainnet/preview/preprod default deployment) drops the "all established peers trusted" closure entirely and replaces it with a pure count of ACTIVE big-ledger-peers (default threshold 5, `defaultNumberOfBigLedgerPeers`). Ordinary ledger/public-root peers established concurrently (Sync-mode targets: `establishedPeers=10, activePeers=5` alongside `activeBigLedger=30`) are irrelevant to this branch's predicate.

The "ALL established peers must be trusted" closure (branch 2, `UseBootstrapPeers`) is real Haskell code but is legitimate ONLY because the governor *also* restricts itself: `requiresBootstrapPeers ubp TooOld = isBootstrapPeersEnabled ubp` in `Bootstrap.hs` gates `Monitor.hs`'s local-roots/target clamping to trustable-only whenever ledger judgement is `TooOld` and bootstrap peers are configured — i.e. the governor structurally never establishes an untrusted peer during that phase, so the closure holds by construction, not by luck.

## Root-caused dugite bug (from-genesis mainnet freeze)

dugite's `haa_satisfied()` in `crates/dugite-node/src/node/networking.rs` (~line 1022) already has the right two-branch shape (BLP-count branch first, bootstrap/local-root closure branch as fallback) — this is NOT a naive implementation. But:

1. The BLP branch (`active_big_ledger_peer_count() >= min_active_blp`) structurally cannot activate during from-genesis Byron-era sync: BLP classification (`identify_big_ledger_peers`, `crates/dugite-node/src/gsm.rs:1052`) requires a stake distribution, which doesn't exist before Shelley. This mirrors real cardano-node's own bootstrap-before-ledger-state design (that's what `UseBootstrapPeers` is *for*).
2. So during from-genesis sync it falls to the bootstrap/local-root closure branch (networking.rs:1058-1065, `established.iter().filter(is_outbound).all(|a| trusted.contains(a))`). But dugite's peer-selection governor does NOT mirror Haskell's `requiresBootstrapPeers`-gated clamping — it establishes ordinary public/ledger peers unrestricted throughout bulk sync — so this closure condition is essentially never satisfiable once any ordinary peer goes warm/hot.
3. `crates/dugite-node/src/gsm.rs` `evaluate()`'s `Syncing -> PreSyncing` edge (~line 471-527) ANDs `haa_lost` with a `tip_stale` check (`tip_age_secs >= syncing_startup_threshold_secs`), with a special-case bypass when the tip is "recent" (added for issue #757, the Mithril-fast-restart case). This bypass does not help from-genesis sync because during from-genesis historical block processing the selection tip's age is inherently ancient (real historical mint timestamps), so `tip_stale` is true almost the entire time — meaning the bypass added for #757 never engages, and this AND-clause has **no Haskell analogue at all**: canonical `Syncing -> PreSyncing` is unconditional on HAA loss alone.

Recommended two-part fix (not yet implemented as of this memory's writing):
(a) Remove tip-staleness from the `Syncing -> PreSyncing` edge entirely — match `isHaaSatisfied` alone, no AND-clause, no bypass-hack.
(b) Fix `haa_satisfied`'s closure branch at the root: either (preferred, byte-exact) make dugite's governor restrict established outbound peers to the trusted set while ledger judgement is TooOld and bootstrap peers are configured (mirror `requiresBootstrapPeers`/Monitor.hs clamping), so the closure holds by construction as in Haskell; or (pragmatic fallback, weaker) drop the "ALL established peers trusted" clause down to a small quorum (">=1 hot trusted peer", or 3-5 to mirror the BLP threshold's intent) as tech debt pending the governor fix. Fix (b) must land before/with (a) — removing the tip-staleness bypass while `haa_satisfied` is still structurally unsatisfiable would make the freeze immediate instead of merely most-of-the-time.

See also [[loe-chain-selection.md]], [[gdd-governor-deep-dive.md]], [[p2p-governor-architecture.md]].
