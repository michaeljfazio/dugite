---
name: project_dugite_issue_784_ppup_quorum_fix_2026_07_06
description: Issue #784 — 6 dugite call sites (not 3) implemented pre-Conway PPUP quorum as "count distinct proposers, last-writer-merge every field" instead of Haskell's byte-identical-value grouping; fix landed in working tree same day
metadata:
  type: project
---

On 2026-07-06, while confirming pre-Conway PPUP/NEWPP enactment semantics
against live Haskell source (see
[[shelley-ppup-votedfutureparams-verified]]) for a user cross-checking
dugite's `voted_future_pparams` helper, found the working tree already had
an uncommitted in-progress fix for exactly this bug (issue #784), landing
concurrently in the same session/repo.

**The bug** (present at session start, confirmed via `git diff` against
the pre-fix content): six call sites computed
`distinct_proposers = proposer_set.len()` (the count of distinct
genesis-key hashes that proposed *anything* in the target epoch) and
tested `distinct_proposers >= quorum`, then **field-merged every proposal
in the epoch together** (`merge_field!` macro, last-writer-wins per
field across ALL proposals regardless of whether they agreed). This is
structurally wrong against Haskell's `votedFuturePParams`: it will enact
a Frankenstein `PParamsUpdate` that no single genesis key ever proposed,
any time enough *distinct* keys propose *anything* — including mutually
disagreeing values — instead of requiring one identical value to be
independently proposed by `>= quorum` keys (see
[[shelley-ppup-votedfutureparams-verified]] Q1).

Sites (pre-fix, matched this pattern):
1. `crates/dugite-ledger/src/state/epoch.rs` (`process_epoch_transition`,
   ~line 531) — used `apply_protocol_param_update` (Result-returning,
   validates UnitInterval bounds but had no size guard).
2. `crates/dugite-ledger/src/eras/shelley.rs` (~line 728, covers
   Shelley–Babbage) — used bare-setter `apply_pp_update`, no guard at all.
3. `crates/dugite-ledger/src/eras/conway.rs` (~line 465, Babbage→Conway
   era-crossing edge case only — Conway itself has no PPUP) — same bare
   setter, no guard.
4. `crates/dugite-ledger/src/state/mod.rs::forecast_d_for_epoch` (~1626).
5. `crates/dugite-ledger/src/state/mod.rs::forecast_extra_entropy_for_epoch`
   (~1663).
6. `crates/dugite-ledger/src/state/mod.rs::forecast_max_block_body_size_for_epoch`
   (~1702) — these three exist because header/envelope validation for the
   first block of a new epoch must forecast the boundary-enacted PParam
   value before the state mutation actually runs; none applied the size
   guard either.

**The fix** (observed already-applied in the dirty working tree,
uncommitted at time of writing): a new
`crate::validation::ppup::fold_pp_proposals(&[(Hash32, ProtocolParamUpdate)]) -> BTreeMap<Hash28, ProtocolParamUpdate>`
helper folds the raw per-epoch `Vec` into a per-genesis-key map (last
insert wins per key, mirroring `Map.insert` overwrite-on-resubmission —
doc comment explicitly requires callers to pass proposals in submission
order, oldest first). All six sites now call
`fold_pp_proposals` then feed the result into the pre-existing
`crate::validation::ppup::voted_future_pparams`, which already correctly
implemented the Haskell grouping + single-winner + size-guard semantics
(that helper itself was untouched by this fix — only its callers change).

**Not independently re-verified**: whether `pending_pp_updates`'s
per-epoch `Vec<(Hash32, ProtocolParamUpdate)>` is *always* populated by
appending in true chronological submission order end-to-end (tx
processing → future-proposal promotion at the prior boundary). If some
path ever prepends or reorders, `fold_pp_proposals`'s "last insert wins"
would pick the wrong (not-actually-latest) resubmission from the same
genesis key within one voting period. Grep for the transaction-time
insertion site (not found under the `pending_pp_updates` name directly in
`crates/dugite-ledger/src/validation/` — likely named differently in the
cert/update-processing path) before treating this as fully closed.

Files touched: `crates/dugite-ledger/src/validation/ppup.rs` (+24 lines,
new `fold_pp_proposals`), `crates/dugite-ledger/src/eras/shelley.rs`,
`crates/dugite-ledger/src/eras/conway.rs`, `crates/dugite-ledger/src/state/epoch.rs`,
`crates/dugite-ledger/src/state/mod.rs`.
