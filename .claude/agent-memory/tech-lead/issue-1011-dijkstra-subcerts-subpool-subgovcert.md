---
name: issue-1011-dijkstra-subcerts-subpool-subgovcert
description: How #1011 landed Dijkstra SUBCERTS/SUBDELEG/SUBPOOL/SUBGOVCERT + SUBENTITIES validate-then-apply for sub-transactions, the clone-then-mutate-or-discard pattern used to get correct fold semantics without touching the mainnet-critical top-level validator, and what remains guarded (SUBGOV, mint, key 24)
metadata:
  type: project
---

## What landed (2026-08-05, branch issue-1011-dijkstra-subrules)

Follow-on to #1001 (atomic SUBLEDGERS fold) and #1010 (full SubTransaction wire
shape + own witness set). #1011 closed the "guarded, not implemented" gap:
certificates/withdrawals/direct_deposits/account_balance_intervals in a
Dijkstra sub-tx were previously rejected wholesale with
`SubEntitiesNotYetApplied` rather than being validated and applied.

Landed, in `crates/dugite-ledger/src/eras/dijkstra.rs`:
- **SUBCERTS → SUBCERT → SUBDELEG/SUBPOOL/SUBGOVCERT** — new
  `validate_sub_certificate` (read-only, ~450 lines) followed immediately by
  the EXISTING `common::apply_shelley_cert`/`conway::apply_conway_cert`
  (the latter promoted `private` → `pub(crate)` in `eras/conway.rs`, zero
  behavior change to the parent-tx path).
- **SUBENTITIES** — withdrawals (`SubMissingAccountsInWithdrawals`, new) +
  direct_deposits (reused the EXISTING `validate_direct_deposits_registration`/
  `apply_direct_deposits` verbatim via the sub-tx envelope, zero new logic).
- **Dijkstra UTXO `AccountBalanceOutOfRange`** for a sub-tx's OWN
  `account_balance_intervals` — reused the EXISTING
  `check_account_balance_intervals` verbatim via the envelope.
- **SUBUTXOW widened**: `SubInvalidWitnessesUTXOW` (real Ed25519 verify via
  `dugite_crypto::keys::PaymentVerificationKey::verify`, signing over the
  SUB-TX's own `tx_id` — never the parent's) alongside the pre-existing
  `SubMissingVKeyWitnessesUTXOW` presence check.

**Still guarded** (guard narrowed, not removed): `mint`, `voting_procedures`,
`proposal_procedures` (SUBGOV). SUBGOV is Conway's ENTIRE
`conwayGovTransition` (19 predicate-failure constructors — proposal
deposits, guardrail script hash, prev-gov-action-id chains, bootstrap-phase
gating) and reimplementing it byte-exact for a second, sub-tx-scoped call
site was judged too large for one issue. Mint has no sub-tx balance-check
infrastructure at all (no fee field, no Phase-1 admission path for sub-txs).
Key 24 (`required_top_level_guards`) stays a hard reject — wire VALUE type
still unconfirmed (key type `Credential Guard` is confirmed from #1010's
memory, value type is not).

## The architectural insight: clone-then-mutate-or-discard beats extraction

The issue asked for "a validate-first step callable from a read-only pass."
The obvious move — extract the ~700-line inline cert-validation block
embedded in `validation/mod.rs`'s `validate_transaction_with_pools` (the
already-oracle-verified top-level tx validator) into something reusable —
was rejected as too risky: that function is entangled with intra-tx overlay
tracking, Phase-1 rules, and 18000+ lines of pinning tests in `tests.rs`.

Instead: `apply_sub_transactions` clones `certs`/`gov` ONCE
(`working_certs`/`working_gov`) before the per-sub-tx loop, and every
SUBENTITIES/SUBCERTS check both validates AND applies directly against that
clone as it walks — mirroring SUBLEDGERS's `foldM` over the full
`LedgerState` accumulator with zero extra bookkeeping (no separate overlay
needed, unlike the UTXO side which already had one from #1001). On ANY
failure anywhere in the fold, the clone is simply dropped; on total success,
one swap (`*certs = working_certs; *gov = working_gov;`) commits it. This
works because `CertSubState`'s biggest fields are `imbl` persistent maps
(O(1) structural-share clone) and `GovSubState` wraps an `Arc` — full clones
are cheap, not O(n) deep copies. **This pattern generalizes**: any future
"needs the SAME predicate logic the top-level validator has, but for a
different signal/scope" problem in this codebase should reach for
clone-then-mutate-or-discard against the cheap-clone substates BEFORE
reaching for extracting/refactoring the giant top-level validator.

## Oracle work this session

Consulted `cardano-ledger-oracle` fresh (did not just trust the prior
`dijkstra-subtx-wire-and-sub-rule-chain` memory's constructor lists) for the
EXACT predicate conditions of `conwayDelegTransition` / `poolTransition` /
`conwayGovCertTransition` — see its new memory
`deleg-pool-govcert-verbatim-transitions.md`. Two findings that changed the
implementation from my first draft:
- Plain delegation certs (`StakeDelegation`/`VoteDelegation`/
  `StakeVoteDelegation`) ALSO require the delegating stake key to already be
  registered (`StakeKeyNotRegisteredDELEG`), checked AFTER the
  delegatee-registered check — I had initially missed this, only checking
  the delegatee.
- `AuthCommitteeHotCert`/`ResignCommitteeColdCert` are IDENTICAL check paths
  — BOTH get `CommitteeHasPreviouslyResigned` (checked first) AND
  `CommitteeIsUnknown` (checked second, against
  `GovernanceState::committee_auth_eligible_members()` — live committee
  UNION pending `UpdateCommittee` proposals, the SAME method #996 already
  established as canonical). My first draft only gave ColdResign the
  resigned check.
- General STS fact worth remembering project-wide: `?!`/`failOnJust` inside
  ONE Haskell rule body body NEVER short-circuits — every applicable check
  runs and ALL failures accumulate into one `MsgRejectTx` list. dugite's
  `LedgerError::InvalidTransaction(String)` is single-message everywhere in
  this file, so this implementation (like every other Dijkstra predicate
  here) short-circuits on the FIRST failing check. Verdict (accept/reject)
  is unaffected — Haskell's own top-level accept/reject is already
  all-or-nothing regardless of accumulated-list length — but the SPECIFIC
  message surfaced when multiple predicates fail on one cert can differ.
  Not fixed; documented as a deliberate, filed-once-not-per-issue trade-off.

Both oracle memory files independently confirmed SHA
`4849c13d6f70e5ab46add9af6e0ec5c537b61f69` resolves against
`IntersectMBO/cardano-ledger` (dated 2026-08-04, "Merge pull request #5950").

## Testing traps hit this session

- Adding real cert/withdrawal predicate checks made THREE pre-existing
  sub-tx tests fail — not because the new logic was wrong, but because
  those tests' fixture witnesses were `[0xAB_u8; 32]` vkeys with
  `vec![0u8; 64]` all-zero signatures (fine when only witness PRESENCE was
  checked; fails hard once `SubInvalidWitnessesUTXOW` does real Ed25519
  verification). Fixed by generating real `PaymentSigningKey`s and signing
  over each sub-tx's own `tx_id`. This IS the RED-then-GREEN evidence for
  the new crypto check — no separate probe test needed.
- `required_witnesses` (pre-existing, unrelated to this issue) reports a
  witness requirement for the CERT'S OWN credential on ordinary
  `StakeDelegation`/`PoolRegistration`/`RegDRep` certs, not just the
  spend-input credential. Every new test fixture needed a SECOND witness
  for the cert credential, not just the spend witness — easy to miss since
  the error message (`SubMissingVKeyWitnessesUTXOW`) looks identical to a
  plain spend-witness failure until you read the listed hash.

## Reachability

Still zero — no network runs Dijkstra, `sub_transactions` is empty on every
observed preview/preprod/mainnet block. Same caveat #1001/#1010 carried.

## Left undone, explicitly

- SUBGOV (votes/proposals) and mint — guarded, documented, not a silent gap.
- Key 24 wire shape — needs a dedicated oracle pass on the VALUE type of
  `Map (Credential Guard) (StrictMaybe X)`; decoder change would also touch
  `crates/dugite-serialization/src/decode/era_conway.rs`, which a sibling
  agent owned this session (`read_protocol_param_update`) — did not touch
  that file at all.
- Compound-field decoder fixtures (certs/guards/voting/proposal/
  account_balance_intervals inside a sub-tx) are still same-process
  round-trips per the #951 caveat — replacing with independently-derived
  bytes needs a real Haskell-encoded fixture, out of scope for this pass.
- `WrongNetworkPOOL` (SUBPOOL) not implemented — the PARENT tx's own
  `PoolRegistration` path doesn't check it either; adding it ONLY for
  sub-tx would be new, asymmetric, under-tested logic.
