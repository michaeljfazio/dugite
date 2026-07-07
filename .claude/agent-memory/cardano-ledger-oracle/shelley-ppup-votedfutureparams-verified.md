---
name: shelley-ppup-votedfutureparams-verified
description: Verified (live GitHub, 2026-07-06) exact semantics of pre-Conway PPUP/NEWPP genesis-key quorum voting and enactment — supersedes the old unverified/missing shelley-ppup summary for the votedFuturePParams half
metadata:
  type: reference
---

Live-fetched and read in full from `IntersectMBO/cardano-ledger` master
(2026-07-06):
`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs` (239 lines) and
`.../Newpp.hs` (110 lines). This replaces the "MISSING FILE, unverified
summary only" note for the PPUP half of
[[shelley-ppup-votedvalue-and-poolparams-adoption]] — the POOLREAP
future-stake-pool-params half of that old note is still unverified/missing.

## Q1 — quorum/enactment groups by byte-identical value, never merges

`votedFuturePParams` (`Ppup.hs`, exported):

```haskell
votedFuturePParams ::
  forall era.
  EraPParams era =>
  ProposedPPUpdates era ->
  PParams era ->      -- ^ params to which the change will be applied
  Word64 ->           -- ^ quorum needed
  Maybe (PParams era)
votedFuturePParams (ProposedPPUpdates pppu) pp quorumN = do
  let votes =
        Map.foldr
          (\vote -> Map.insertWith (+) vote 1)
          (Map.empty :: Map.Map (PParamsUpdate era) Word64)
          pppu
      consensus = Map.filter (>= quorumN) votes
  -- NOTE that `quorumN` is a global constant, and that we require
  -- it to be strictly greater than half the number of genesis nodes.
  -- The keys in the `pup` correspond to the genesis nodes,
  -- and therefore either:
  --   1) `consensus` is empty, or
  --   2) `consensus` has exactly one element.
  [ppu] <- Just $ Map.keys consensus
  let ppNew = applyPPUpdates pp ppu
  guard $
    toInteger (ppNew ^. ppMaxTxSizeL) + toInteger (ppNew ^. ppMaxBHSizeL)
      < toInteger (ppNew ^. ppMaxBBSizeL)
  pure ppNew
```

Key facts:
- `pppu :: Map GenDelegHash (PParamsUpdate era)` — one entry per genesis
  delegate (later resubmission within the voting period overwrites the
  earlier one via ordinary `Map` semantics upstream in `ppupTransitionNonEmpty`,
  `Map.union pup pupS` with `pup` — the newest submission — on the left,
  i.e. left-biased union so the newest wins).
- `Map.insertWith (+) vote 1` tallies by the **entire `PParamsUpdate` value
  as the map key** (needs `Ord (PParamsUpdate era)`, i.e. structural/derived
  equality across every `StrictMaybe` field simultaneously). Two proposals
  differing in even one field are different keys — no partial credit, no
  per-field tallying.
- `[ppu] <- Just $ Map.keys consensus` is a refutable pattern match inside
  the `Maybe` monad: 0 elements (no quorum) or ≥2 elements (tie between
  disagreeing quorum-reaching groups) both fail the match → `Nothing` via
  `MonadFail` → **enact nothing**. There is no merge path in the source at
  all — the code comment explicitly asserts ties should be structurally
  impossible given the quorum-is-strict-majority invariant (see Q3), but
  the pattern match is what actually enforces "nothing enacted" defensively
  regardless.
- `NEWPP`'s `updatePpup` (`Newpp.hs`) calls the exact same
  `votedFuturePParams curProposals pp coreNodeQuorum` a second time at the
  epoch boundary against the freshly-promoted `sgsCurProposals` (which is
  `sgsFutureProposals` promoted, cleared to `emptyPPPUpdates` unless
  `all (hasLegalProtVerUpdate pp) newProposals` fails, in which case it's
  reset to empty).

## Q2 — the size sanity guard

Exact location: **inside `votedFuturePParams` itself** (not a separate rule,
not in `NEWPP`/`ENACT`), applied to `ppNew = applyPPUpdates pp ppu` (the
**already-merged, post-update** params, not the raw delta):

```haskell
  -- TODO: Remove this incorrect check from the code and the spec. It is incorrect because
  -- block header size is not part of the block body size, therefore this relation makes
  -- no sense. ... See relevant spec ticket:
  -- https://github.com/IntersectMBO/cardano-ledger/issues/4251
  guard $
    toInteger (ppNew ^. ppMaxTxSizeL) + toInteger (ppNew ^. ppMaxBHSizeL)
      < toInteger (ppNew ^. ppMaxBBSizeL)
```

- Fields: `ppMaxTxSizeL` (maxTxSize) + `ppMaxBHSizeL` (maxBlockHeaderSize)
  **strictly less than** `ppMaxBBSizeL` (maxBlockBodySize). Strict `<`, not `<=`.
- Arithmetic is `Integer` (`toInteger`) — no overflow possible in Haskell;
  a Rust port summing `u32`/`u64` fields should use non-wrapping (checked
  or widened) arithmetic, though in practice these are all small network
  parameters.
- Haskell's own comment flags this guard as acknowledged-wrong-but-kept
  for compatibility (block header size was never part of block body size,
  so the relation is spec debt from an earlier design) — **do not fix the
  "incorrect" semantics in a port**; byte-exact compatibility means
  reproducing the bug, not correcting it.
- This is the *only* guard `votedFuturePParams` applies. It does **not**
  re-check protocol-version legality (`hasLegalProtVerUpdate`) at
  enactment time — that check already happened per-individual-update at
  submission time in `ppupTransitionNonEmpty` (`PVCannotFollowPPUP`), and
  separately `NEWPP.updatePpup` resets `sgsCurProposals` to empty if any
  future-promoted proposal fails `hasLegalProtVerUpdate pp` against the
  just-enacted `pp`.

## Q3 — quorum is a fixed genesis-config constant, not computed

`quorum :: Word64` lives in `Globals` (`cardano-ledger-core` `BaseTypes.hs`)
and is wired directly from the Shelley genesis file, not computed at
runtime:

```haskell
-- Shelley/Genesis.hs, mkShelleyGlobals:
    , quorum = sgUpdateQuorum genesis
```

`sgUpdateQuorum :: Word64` is a literal field of `ShelleyGenesis` (the
`updateQuorum` key in `shelley-genesis.json`; mainnet = 5). The PPUP/NEWPP
rules never compute a majority — they just read this constant via
`asks quorum` (`Control.Monad.Trans.Reader`) and pass it straight into
`votedFuturePParams`.

The genesis file itself is sanity-checked **once, at load time**, by
`validateGenesis`'s `checkQuorumSize`:

```haskell
      checkQuorumSize =
        let numGenesisNodes = fromIntegral $ length sgGenDelegs
            maxTooSmal = numGenesisNodes `div` 2
         in if numGenesisNodes == 0 || sgUpdateQuorum > maxTooSmal
              then Nothing
              else Just $ QuorumTooSmall sgUpdateQuorum maxTooSmal numGenesisNodes
```

i.e. `updateQuorum` must be a **strict majority** of `sgGenDelegs`
(`quorum > numGenesisNodes \`div\` 2`, integer division). This is what
makes the "consensus has 0 or exactly 1 element" comment in
`votedFuturePParams` true by construction — two disjoint groups of
genesis keys can never simultaneously reach a strict-majority quorum on
different values. It is a load-time genesis-file validation, not a
runtime recomputation; `votedFuturePParams` itself has zero majority
logic and would still correctly return `Nothing` on a hypothetical tie if
a malformed genesis ever violated the invariant (the refutable pattern
match protects it either way).

## Rust translation notes (dugite)

`crates/dugite-ledger/src/validation/ppup.rs::voted_future_pparams`
correctly mirrors this: groups by full structural equality of
`ProtocolParamUpdate` (`Vec<(ProtocolParamUpdate, u64)>` tally since the
type isn't `Hash`), filters to `count >= quorum`, requires exactly one
survivor (`at_quorum.len() != 1` → `None`), then applies the identical
`max_tx_size + max_block_header_size >= max_block_body_size` → `None`
guard (strict-`<` semantics preserved via `>=` rejection). See
[[project_dugite_issue_784_ppup_quorum_fix_2026_07_06]] for the six call
sites that previously bypassed this helper with a "count distinct
proposers, then last-writer-merge every field" bug, and the in-progress
fix routing them all through it via a new `fold_pp_proposals` helper.
