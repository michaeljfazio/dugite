---
name: issue-784-ppup-voted-value-quorum
description: Pre-Conway PPUP enactment must use Haskell's identical-value votedValue quorum, not distinct-proposer-count + field-merge
metadata:
  type: reference
---

Issue #784 (branch `fix/ledger-review-2026-07-04`): pre-Conway PPUP
(protocol-parameter update) enactment had THREE buggy copies —
`eras/shelley.rs`, `eras/conway.rs` (era-crossing), `state/epoch.rs`
(test-driver) — all counting *distinct proposers* across any proposals
toward quorum, then field-merging every proposal's fields together
(last-writer-wins per field). Haskell's `votedFuturePParams`
(`Shelley.Rules.Ppup`) requires a quorum of genesis delegates to vote the
BYTE-IDENTICAL `PParamsUpdate` value; ties or no value at quorum enact
NOTHING — it never merges. A correct implementation already existed
unused: `validation::ppup::voted_future_pparams`.

Oracle-verified Haskell source (`Ppup.hs`):
```haskell
votedFuturePParams (ProposedPPUpdates pppu) pp quorumN = do
  let votes = Map.foldr (\vote -> Map.insertWith (+) vote 1) Map.empty pppu
      consensus = Map.filter (>= quorumN) votes
  [ppu] <- Just $ Map.keys consensus   -- 0 or >=2 keys -> Nothing (MonadFail)
  let ppNew = applyPPUpdates pp ppu
  guard $ toInteger (ppNew^.ppMaxTxSizeL) + toInteger (ppNew^.ppMaxBHSizeL)
            < toInteger (ppNew^.ppMaxBBSizeL)   -- STRICT <, checked on ppNew (post-apply)
  pure ppNew
```
`vote` key = the entire `PParamsUpdate` value (structural equality across
all fields simultaneously) — not a per-field tally. Quorum itself is a
fixed genesis constant (`sgUpdateQuorum`, mainnet=5), enforced at
genesis-load to be a strict majority of genesis delegates (so 0-or-1
winners is guaranteed by construction, never checked at runtime).

Fix: added `validation::ppup::fold_pp_proposals(&[(Hash32, ProtocolParamUpdate)]) -> BTreeMap<Hash28, ProtocolParamUpdate>`
(last-writer-per-key fold, matching Haskell `Map.insert` semantics — relies
on `pending_pp_updates`/`future_pp_updates` Vecs always being appended in
submission order via `.push()`/`.extend()`, verified no sort/reorder
anywhere in the crate). All 3 enactment sites + 3 header/envelope forecast
helpers (`forecast_d_for_epoch`, `forecast_extra_entropy_for_epoch`,
`forecast_max_block_body_size_for_epoch` in `state/mod.rs`) now fold then
call `voted_future_pparams`. Forecast helpers matter as much as enactment
sites — if only enactment is fixed, header validation forecasts diverge
from the boundary's actual enactment, creating a NEW consensus split.

Reachability: LATENT. Every historical mainnet/preview/preprod quorum
boundary had byte-identical genesis-key proposals, so old and new code
agree on all real chain data — no epoch dump can byte-diff this fix.
Correctness rests entirely on matching Haskell semantics + unit tests, not
live-chain validation. See also [[issues-799-800-802-812-batch-fix]] for
a related atomic-PParams-enactment fix in the same review sweep.
