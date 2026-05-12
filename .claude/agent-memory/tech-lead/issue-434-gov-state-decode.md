---
name: Issue #434 gov-state decode failure
description: Three independent bugs in N2C GetGovState encoding that all surfaced as "0 active proposals" — Conway PParams positional order, duplicate vote-map keys, OMap forest order
type: reference
---

# Issue #434 — N2C `gov-state` returned 0 active proposals

**Symptom (user-facing):** `cardano-cli conway query gov-state --testnet-magic 2` against
dugite reports `proposals: []` despite Koios showing ~13 active proposals at the
same epoch.

**Underlying:** `DecoderFailure` in cardano-cli with two distinct error texts
that cascade — first the PParams element count, then `mkProposals: Could not
add a proposal …`. cardano-cli surfaces a decode failure to the user as an
empty result.

Fix landed in `crates/dugite-node/src/node/n2c_query/encoding.rs` and
`crates/dugite-node/src/node/query.rs`.

## Root causes (three independent bugs)

### 1. Conway PParams positional order (oracle drift)
`encode_protocol_params_cbor` placed `protocolVersion` at array index **30**
(last). Current `cardano-ledger` master defines `eraPParams @ConwayEra` with
`ppGovProtocolVersion` at index **12**, between `tau` and `ppMinPoolCost`.
The earlier `cardano-haskell-oracle` snapshot was outdated and a regression
test (`test_pparams_v21_positional_order_issue_336`) had codified the wrong
order.

**Wire symptom:** `DeserialiseFailure 4207 "Final number of elements: 24 does
not match the total count that was decoded: 31"`. cardano-cli read
`array(2)[major, minor]` as if it were a uint and shifted every subsequent
field; the count-mismatch surfaced later in the response stream when the OMap
decoder tripped over the misaligned bytes.

**Fix:** move `protocolVersion` to slot 12; rename and rewrite the regression
test to `test_pparams_conway_positional_order_issue_434`.

### 2. Duplicate keys in per-proposal vote maps
`votes_by_action` is an append-only `Vec<(Voter, VotingProcedure)>` — every
re-vote on the same proposal is a new element. Haskell's `proposalsAddVote`
uses `Map.insert` (last-wins) and the CBOR decoder calls
`decodeMapEnforceNoDuplicates`. Emitting raw entries with duplicate keys
triggers exactly the same `"Final number of elements: N does not match the
total count that was decoded: M"` error shape as PParams, but at a different
offset — easy to confuse with bug #1.

**Fix:** in `build_vote_maps`, project each vote stream through
`BTreeMap<(hash, cred_type), vote>` keyed by voter, then flatten. Last vote
wins, matching Haskell's `Map.insert`. Source-of-truth dedup at apply time is
a separate cleanup — the wire fix is sufficient for the query path.

### 3. Stale `prev_action_id` after sibling cleanup gap
Conway's `proposalsApplyEnactment` removes all sibling proposals (and their
descendants) when an action of a given purpose enacts. Dugite's epoch
transition has a gap: when a Committee action enacted at some prior epoch,
the NoConfidence/UpdateCommittee siblings proposed at epoch 1261 were left
in `governance.proposals` with their original `prev_action_id` pointing at
the now-superseded root. Haskell's `mkProposals` then fails on the first such
orphan with `mkProposals: Could not add a proposal <id>`.

**Fix (wire-only):** in `query.rs`, replay the same admission check
(prev resolves to current enacted root OR to an already-admitted ancestor in
the iteration order) before emitting the OMap, and silently drop unresolvable
proposals with a `debug!` trace. The underlying sibling-cleanup gap in the
apply path is tracked separately — the wire filter keeps cardano-cli queries
working in the meantime.

## OMap iteration order requirement

`mkProposals` folds `proposalsAddAction` over the decoded OMap in **insertion
order**. Each child's prev must already be admitted. Dugite's
`BTreeMap<GovActionId, ProposalState>` orders by tx-hash bytes — arbitrary.

The fix sorts proposals by `(proposed_epoch, action_id)` before emission.
Within an epoch, proposals can't legally reference each other (a tx cannot
refer to its own outputs as prev), so the secondary tx-hash sort is purely a
canonical tiebreaker. Cross-epoch dependencies are always resolvable because
parents are proposed in earlier epochs and thus emitted first.

## Diagnostic methodology

The error text was the same shape ("Final number of elements: X does not
match the total count that was decoded: Y") for bugs #1 and #2. The
diagnostic that disambiguated them: dump the full encoded response to a
file, walk it with minicbor, locate the byte offset in the error, and count
declared-vs-actual children in the surrounding container. Bug #1 surfaced as
a 31-element PParams array consumed at 24 items because field type
misalignment ate fewer bytes than the declared length. Bug #2 surfaced as a
31-key map with 7 duplicate keys, leaving 24 unique entries.

Future similar `Final number of elements: A does not match … decoded: B`
errors against N2C queries are almost always one of:
* PParams positional order drift vs ledger master (recheck `eraPParams`)
* Map encoder forgetting to dedup an append-only source `Vec`
* Set encoder forgetting to sort and dedup an unordered source
