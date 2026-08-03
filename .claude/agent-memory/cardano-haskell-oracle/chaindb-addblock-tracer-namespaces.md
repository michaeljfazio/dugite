---
name: chaindb-addblock-tracer-namespaces
description: Exact ChainDB.AddBlockEvent.* and Forge.Loop.* new-tracing namespace strings, severities, and JSON fields for cardano-node 11.0.1 — chain-switch/rollback tracing, and why there is no dedicated "orphaned block" trace.
metadata:
  type: reference
---

# ChainDB add-block / Forge tracer namespaces — verified at cardano-node 11.0.1

Source: `cardano-node/src/Cardano/Node/Tracing/Tracers/ChainDB.hs` and
`.../Tracers/Consensus.hs`, tag `11.0.1` (raw.githubusercontent.com fetch).
Underlying trace ADT: `ouroboros-consensus` tag
`release-ouroboros-consensus-3.0.1.0` (see
[[praos-chain-order-v3-verified]] for how that pin was resolved from
cardano-node 11.0.1's own `.cabal` version bound — do not reuse
`0.30.0.1`, it's the wrong version for this node release).

## Namespace assembly

`Tracers.hs` roots the ChainDB tracer at `["ChainDB"]`
(`mkCardanoTracer' trBase trForward mbTrEKG ["ChainDB"]`, ~L118). The
`MetaTrace (ChainDB.TraceEvent blk)` instance then does
`namespaceFor (ChainDB.TraceAddBlockEvent ev) = nsPrependInner
"AddBlockEvent" (namespaceFor ev)`. So the full dotted namespace as it
appears in the new-tracing JSON `"ns"` field is:

```
ChainDB.AddBlockEvent.<Constructor>
```

Confirmed against `bench/trace-schemas/newNamespaces.txt` (generated
namespace inventory) at the same tag — full list under this prefix:

```
ChainDB.AddBlockEvent.AddBlockValidation.InvalidBlock
ChainDB.AddBlockEvent.AddBlockValidation.UpdateLedgerDb
ChainDB.AddBlockEvent.AddBlockValidation.ValidCandidate
ChainDB.AddBlockEvent.AddedBlockToQueue
ChainDB.AddBlockEvent.AddedBlockToVolatileDB
ChainDB.AddBlockEvent.AddedReprocessLoEBlocksToQueue
ChainDB.AddBlockEvent.AddedToCurrentChain
ChainDB.AddBlockEvent.ChainSelectionLoEDebug
ChainDB.AddBlockEvent.ChangingSelection
ChainDB.AddBlockEvent.IgnoreBlockAlreadyInVolatileDB
ChainDB.AddBlockEvent.IgnoreBlockOlderThanK          <- NB: string says "K", constructor is IgnoreBlockOlderThanImmTip (see quirk below)
ChainDB.AddBlockEvent.IgnoreInvalidBlock
ChainDB.AddBlockEvent.PipeliningEvent.OutdatedTentativeHeader
ChainDB.AddBlockEvent.PipeliningEvent.SetTentativeHeader
ChainDB.AddBlockEvent.PipeliningEvent.TrapTentativeHeader
ChainDB.AddBlockEvent.PoppedBlockFromQueue
ChainDB.AddBlockEvent.PoppedReprocessLoEBlocksFromQueue
ChainDB.AddBlockEvent.PoppingFromQueue
ChainDB.AddBlockEvent.StoreButDontChange
ChainDB.AddBlockEvent.SwitchedToAFork
ChainDB.AddBlockEvent.TryAddToCurrentChain
ChainDB.AddBlockEvent.TrySwitchToAFork
```

There is no "legacy" (pre-TraceDispatcher) name distinct from these — the
old iohk-monitoring-style config still exists (`configuration/cardano/
mainnet-config-legacy.json` ships a `"TraceChainDb": true` boolean-only
switch with no per-event granularity) but the *same* `LogFormatting`/
`MetaTrace` module in `Tracers/ChainDB.hs` backs both; `forHuman` renders
identical text regardless of which tracing system is selected. There is no
separate historical string like `ChainDB:AddedToCurrentChain` to worry
about for 11.0.1.

**Known quirk (source-verified, not caused by dugite)**: `namespaceFor
ChainDB.IgnoreBlockOlderThanImmTip {} = Namespace []
["IgnoreBlockOlderThanImmTip"]` but `severityFor (Namespace _
["IgnoreBlockOlderThanK"]) _ = Just Info` — the severity-lookup pattern
uses a DIFFERENT string than the namespace the constructor actually
produces, so that one specific severity clause is dead code in upstream
cardano-node (falls through to the generic default instead). Not something
to fix in dugite; just don't be surprised if you cross-reference it.

## Relevant constructors for chain-switch / rollback observation

| Constructor | Fires when | Default severity |
|---|---|---|
| `ChangingSelection` | Herald, right before either of the next two; carries the new tip `Point` only | Debug |
| `AddedToCurrentChain` | Block extends current chain (`getRollback chainDiff == 0`) | `Notice` (or max severity of embedded ledger events) |
| `SwitchedToAFork` | Block causes a rollback + re-extension (`getRollback chainDiff > 0`) — this IS the "chain switch" event, always implies a fork/rollback happened | `Notice` (or max severity of embedded ledger events) |
| `TryAddToCurrentChain` / `TrySwitchToAFork` | Precede the above; announce the *attempt*, before validation confirms it | Debug / Info |
| `StoreButDontChange` | Block doesn't fit current chain or any known fork tip (stored, chain selection not affected) | Debug |

`switchTo` in `ChainSel.hs` (ouroboros-consensus, 3.0.1.0):
```haskell
let mkTraceEvent
      | getRollback (getChainDiff vChainDiff) == 0 = AddedToCurrentChain
      | otherwise = SwitchedToAFork
```
So **`SwitchedToAFork` is the exact, unambiguous marker for "this node
switched away from a chain it had already adopted"** — `AddedToCurrentChain`
never implies a rollback occurred.

## JSON payload — the "reason" field requires DDetailed verbosity

As of 3.0.1.0, both `AddedToCurrentChain` and `SwitchedToAFork` carry a 5th
field, `ReasonForSwitch' blk` (`Longer` / `HigherOCert` / `VRFTiebreak` —
see [[praos-chain-order-v3-verified]]). But `forMachine` only serializes it
at the `DDetailed` detail level:

```haskell
forMachine DDetailed (ChainDB.SwitchedToAFork events selChangedInfo old new reasonForSwitch) =
  ... ++ [ "reason" .= forMachine DDetailed reasonForSwitch ]
forMachine dtal (ChainDB.SwitchedToAFork events selChangedInfo _old new reasonForSwitch) =
  ... -- normal-detail branch: NO "reason" key, NO "headers", NO tipBlockHash/tipBlockParentHash/tipBlockIssuerVKeyHash
```
At default detail, `SwitchedToAFork`'s JSON has: `kind`,
`newtip`, `newSuffixSelectView`, `oldSuffixSelectView` (if any), `events`.
To assert *why* a switch happened (not just *that* it happened), the node's
tracing config needs the detailed variant for this namespace (per-namespace
`TracingVerbosity`/detail override, not just overall severity filter).

## Forge-side tracing (own-block adoption, not later re-orphaning)

Root namespace: `Forge.Loop` (`Tracers.hs` L295: `["Forge", "Loop"]`).
Source ADT: `ouroboros-consensus-diffusion/.../Node/Tracers.hs`,
`TraceForgeEvent`:

```
Forge.Loop.ForgedBlock        -- Info  — we minted a valid block
Forge.Loop.DidntAdoptBlock    -- Error — we minted it but did NOT adopt it
Forge.Loop.AdoptedBlock       -- Info  — we minted it AND adopted it
```

Doc comment on `TraceDidntAdoptBlock` (verbatim): "We did not adopt the
block we produced, but the block was valid. We must have adopted a block
that another leader of the same slot produced before we got the chance of
adopting our own block. This is very rare, this warrants a warning."

**Critical for Q5 (detecting an orphaned forged block): none of these three
fire again later.** They are emitted synchronously, once, at forge time,
reflecting only whether THIS node's own chain-sel accepted the block at the
moment of minting. If a node forges a block, adopts it (`AdoptedBlock`),
and HOURS/BLOCKS later a competing longer chain arrives causing a
`SwitchedToAFork` that rolls back past that block — there is no
`TraceOrphanedBlock` or equivalent. **Orphaning is never announced as its
own event anywhere in ouroboros-consensus.** It must be inferred:
1. Locally: diff the `old` vs `new` `AnchoredFragment (Header blk)` fields
   carried by the `SwitchedToAFork` trace that supersedes it (the orphaned
   suffix is `AF.anchorNewest (getRollback chainDiff) old`), or via
   `oldSuffixSelectView` in `SelectionChangedInfo`.
2. Externally / for a devnet test: the block's hash simply never appears as
   an ancestor of the canonical tip past the point where the fork was
   resolved, and it never gets copied into the ImmutableDB
   (`ChainDB.CopyToImmutableDBEvent`). This project's own established
   pattern (see `project_preview_forge_v2_0_15_2026_07_13`) is to confirm a
   forged block's survival via an external indexer's confirmation count
   (Koios `num_confirmations`) rather than expect a node-local "orphaned"
   trace — that pattern is correct and matches upstream: there genuinely is
   no better mechanism.

## Practical test design

For a two-forger convergence assertion:
- Watch BOTH nodes' `ChainDB.AddBlockEvent.SwitchedToAFork` /
  `AddedToCurrentChain` `"newtip"` field (present at any detail level).
- Convergence = both nodes' `newtip` reaches the same `(slot, hash)` at the
  same `blockNo`, eventually — not "immediately" and not "at the same
  moment," since the two nodes process the same set of blocks independently
  and may pass through different transient tips first.
- Do not assert on `Forge.Loop.AdoptedBlock` as proof a block survives
  long-term; it only proves local adoption at forge time. Use chain-tip
  convergence or immutable-DB presence for the durable check.
