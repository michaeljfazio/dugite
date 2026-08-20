---
name: issue-1084-1093-byron-genesis-authority-fix
description: resolve_genesis_authority OR-clause removed (memberR≡lookupR proof), endorsement key fixed to block_sig delegate (not consensus-data issuer_pubkey), register_vote membership fixed to proposal_registration_slot (#1093), register_endorsement prune made unconditional
metadata:
  type: project
---

Fixed on 2026-08-20, on top of the THEN-unpushed #1084 commits (2f920c8c14,
06b3a9c489, f4db692b70) — an independent Fable review of that Byron
delegation/update-state code found real gaps, all confirmed against the
pinned `cardano-ledger-byron-1.2.0.0` tarball
(`~/.cache/cabal/packages/cardano-haskell-packages/cardano-ledger-byron/1.2.0.0/`)
and the `bimap-0.5.0` package it depends on
(`~/.cache/cabal/packages/hackage.haskell.org/bimap/0.5.0/`). Left
UNCOMMITTED per explicit instruction — this file records the finding, not a
claim that it landed ([[feedback_closed_issue_is_not_evidence_work_landed]]).

## The proof that mattered: memberR ≡ lookupR

`Data/Bimap.hs:177-178,368-373`: `memberR y (MkBimap _ right) = M.member y
right` and `lookupR y (MkBimap _ right) = ... M.lookup y right` — literally
the same `right :: Map KeyHash KeyHash` field, same key. So
`Registration.hs`'s `Delegation.memberR proposerId delegationMap` (proposals)
and `Voting.hs`/`Endorsement.hs`'s `Delegation.lookupR voter/vk
delegationMap` (votes/endorsements) are the SAME predicate, not two
different rules needing different dugite-side logic. dugite's
`delegation_map_rev: BTreeMap<Hash28, Hash28>` (keyed delegate -> genesis
key, with genesis-key identity self-pairs seeded at `seed_byron_genesis`)
IS that `right` map already — a single `.get()`/`.contains_key()` serves
all three call sites. No separate "direct bootStakeholders membership"
check exists anywhere upstream.

## What was wrong

- `resolve_genesis_authority` had `if allowed_delegators.contains(key_hash)
  { return Some(*key_hash) }` before the `delegation_map_rev` lookup — an
  OR-clause with no upstream counterpart. Made genesis-key standing
  PERMANENT even after a real delegation-away event destroys the identity
  self-pair in `delegation_map_rev` (`activate_delegations`'s
  `old_delegate` removal). Real on mainnet: all 7 genesis keys delegate
  away at slot 0 (`heavyDelegation` in `byron-genesis.json`).
- The per-block endorsement hashed `aux.issuer_pubkey` (consensus-data
  field 1 — the raw GENESIS key doing the delegating) where upstream hashes
  `headerIssuer`/`blockIssuer` = `Delegation.delegateVK cert` from
  `block_sig`'s embedded certificate (`Header.hs:274-276`,
  `Block/Validation.hs`'s `updateEndorsement`). dugite's decoder was
  `r.skip()`-ing `block_sig` entirely. The two bugs CANCELLED on every real
  mainnet block observed so far (hashing the wrong key + widened lookup
  happened to equal hashing the right key + correct lookup) — an accidental
  correctness the mainnet exactness campaign's own process exists to
  distrust ([[feedback_byte_compare_against_the_other_implementation]]-adjacent:
  same-mechanism cancellation, not independent agreement).
- `register_vote`'s registered-proposal check read
  `registered_protocol_update_proposals` (PROTOCOL-only) instead of
  `proposal_registration_slot` (every successful registration, protocol OR
  software OR both — `Interface.hs::registerProposal`'s unconditional
  insert at :274-276, and `registerVote`'s `rups = M.keysSet
  proposalRegistrationSlot` at :390,:396). This is the confirmed likely
  mechanism behind #1093 (real mainnet slot 73486 vote rejected as
  `ProposalNotRegistered`).
- `register_endorsement` pruned (`prune_stale_proposals`) only on its OWN
  success path — an early `return` on an unresolvable issuer key skipped
  pruning entirely. Upstream's `Interface.hs::registerEndorsement` wrapper
  prunes UNCONDITIONALLY after `Endorsement.register`, regardless of that
  function's branch (including "no proposal registered for this pv" and
  "issuer key unresolvable" — `Endorsement.hs:210-218`'s own comment says
  the latter is not an error). Also reordered so the
  confirm/threshold/candidate logic runs against PRE-prune state, matching
  upstream's structure (prune is the wrapper's LAST step).

## Fix

`resolve_genesis_authority` now a pure `delegation_map_rev.get(key_hash)`,
`allowed_delegators` parameter dropped from it and from
`register_proposal`/`register_vote`/`register_endorsement` (was only ever
threaded through for the removed OR-clause). Added
`ByronBlockAux::delegate_pubkey` (kept `issuer_pubkey` — still useful for
future signature-verification work, #1092) populated by a new
`read_byron_block_sig` decoder (`dugite-serialization/era_byron.rs`) that
parses `[2, [dlg_cert(4), sig]]` — tag 2 is the ONLY variant a conforming
encoder ever emits (`Header.hs:676-701`, `DecoderErrorUnknownTag` on
anything else), so an unknown tag is now a hard decode error, not a lenient
skip. `apply_update_payload` hashes `aux.delegate_pubkey`, not
`aux.issuer_pubkey`. `register_vote` checks `proposal_registration_slot`.
`register_endorsement` restructured to match Haskell's exact control flow
(see function doc for the full derivation).

## Validation

Mainnet Byron replay, genesis→epoch 95 (2,057,068 blocks, 20s, from a
cloned `db-cn-mainnet` chain — CoW `cp -Rc`, never touch the source).
**Zero** `byron update vote/proposal rejected` or `byron delegation
certificate rejected` log lines anywhere in the replay — including slot
73486's epoch (3) — where the pre-fix code was known to log one. Diffed
against `reports/mainnet-exactness/cstreamer-byron-full/` with
`diff-cstreamer-dumps.py`: 95 paired epochs, 1045 leaf comparisons, **0
divergent** on `byronDelegation.count`, `byronProtocolParams.{maxBlockSize,
maxTxSize,scriptVersion}`, `byronUpdateEpoch`, `epoch`, `lastSlot`,
`snapshotEraName`, `utxo.{balance,count}` — including both real adoption
boundaries landing at the EXACT right epoch (maxTxSize 4096→65536 at epoch
16 not 15/17; →8192/32768 at epoch 84 not 83/85). The one reported
"divergence" (`byronProtocolParams.txFeePolicy`) is a pre-existing
dict-vs-Haskell-`Show`-string JSON shape mismatch in the dump/comparator,
confirmed via `git diff` to be outside every file this fix touches — same
numeric value (155381, 21973/500) on both sides at every epoch, not a
regression and not addressed here (out of scope, would need comparator
work). Full workspace: `cargo build --workspace --all-targets` clean,
`clippy --all-targets -D warnings` clean, `fmt --check` clean, `nextest run
--workspace` 8236/8237 (the one failure is the pre-existing, documented
`xtask::qa_report_covers_shipped_code` staleness gate — expected whenever
`crates/` changes since the last QA report, unrelated to this fix).

## Traps worth remembering

- **Two independent-looking bugs cancelling is not evidence either is
  right.** The wrong endorsement key + the widened lookup produced the
  right answer on every mainnet block seen — only a source-grounded
  re-derivation caught it, not the green replay that had already run.
- **`memberR`/`lookupR` "different rule, same predicate" needs the actual
  library source**, not just the ledger package that calls it — the
  answer lived in `bimap-0.5.0`, one dependency layer down from
  `cardano-ledger-byron`.
- **A membership set can be RIGHT for one purpose and WRONG for a
  same-shaped check elsewhere in the same struct** —
  `registered_protocol_update_proposals` is correct for "does a PROTOCOL
  proposal exist for this pv" (endorsement/adoption) and wrong for "was
  ANY proposal registered" (vote admission) — two different Haskell sets
  that happen to overlap whenever a proposal is protocol-only.
