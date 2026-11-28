# The `ledger-rules` runner and strict gate

Phase 2b of #1114 (#1125). 368 phase-one transaction fixtures from
pragma-org/node-diversity-conformance (NDC), PV9/PV10, all preprod; the pins are in `../corpora.toml`.
cardano-node scores 368/368 on them.

```sh
just ndc-gate                 # fetch + verify the corpus, run the gate and its red-proofs, check the issues
just ndc-report ledger-rules  # write the schema-valid report (target/node-diversity/reports/ledger-rules.json)
DUGITE_NDC_UPDATE_SNAPSHOT=1 just ndc-gate   # rewrite raised-snapshot.json after a REVIEWED change
```

CI runs it as the job **`Node-diversity ledger-rules gate`** (`.github/workflows/ci.yml`, job id
`ledger-rules-gate`). Whether it is a *required* status check is a branch-protection setting.

## What a result is

For each fixture the runner (`tests/conformance/src/node_diversity/ledger_rules/`):

1. decodes the transaction with `dugite_serialization::decode_transaction` (a refusal is `DecodingFailure`);
2. builds a real `LedgerState` from `initial_state` and asks it for `mempool_validation_context()`
   (`state.rs`; never a hand-built context);
3. calls `validate_transaction_with_context`, exactly as `LedgerTxValidator::validate_tx` does;
4. converts the ledger's failures with `dugite_tx_admission::rejection_from_validation_errors` at the
   fixture's protocol version (the conversion the node uses, moved out of `dugite-node`) and encodes them
   with `encode_apply_tx_err`;
5. **decodes those bytes** through `tag_table.rs` and names each failure from its tag path.

When several failures are raised the report carries the expected one if it is among them, else the
first one that has a scenario name (the scenario's rule). The generic `ConwayMempoolFailure` fallback
has no name: it is a failure and can never be a match. A rejection whose failures have no scenario name
is reported with a placeholder that is not the fixture's expectation (the schema can only carry the
scenario's 81 names); the gate compares the true value, `unmapped:<constructor>`.

## What the oracle is, and what a match proves

A match proves that the failure dugite's encoder would send NAMES the scenario's expected failure. It does
not prove that cardano-cli would be sent the same bytes by cardano-node, because the scenario's own oracle is
not "the bytes cardano-node sends". It is the Haskell `applyTx` (the MEMPOOL rule, which wraps LEDGER) plus
the reference checker's normalisation (`Amaru validation-rules/.../Run.hs`, `normalizeApplyTxError`):

* the PV<11 mempool-only failure `ConwayMempoolFailure "Unelected committee members are not allowed to cast
  votes: .."` is DROPPED (`Run.hs:387-401`). Fixture 00154 (PV10, vote by an unelected member) therefore
  expects `Pass`; cardano-node's mempool would refuse it (`Conway/Rules/Mempool.hs:122-134`) and dugite has no
  such check, so dugite agrees with the scenario and differs from the node it models. Tracked as #1179.
* `ConwayMempoolFailure "All inputs are spent. .."` is mapped to `InputSetEmptyUTxO` (empty input set) or
  `BadInputsUTxO`. A real node sends the mempool text there; dugite sends the typed `BadInputsUTxO`. Fixtures
  00050 and 00074 expect those names and match by name since #1138 was fixed (they were refused at decode before),
  so the question of the mempool text stays open.
* when the fixture names an expected predicate and ANY raised failure renders to a string containing it, the
  checker reports the expected name alone. Comparison is therefore unordered and payload-free, and so is
  ours: 00290 matches with `[ValueNotConservedUTxO, StakeKeyNotRegisteredDELEG]` raised, where the oracle
  would have reported only the expected name.

Three fixtures match while ALSO emitting the generic fallback, because the expected failure is among the raised
ones: 00128 (`NetworkMismatch`) and 00329 (`RedeemerIndexOutOfRange`) are tracked as #1180; 00285's fallback is
`ExUnitsExceeded` (#1143). A match there does not mean the raised set is right: `raised-snapshot.json` records it.

## The tag table (`tag_table.rs`) and the name table (`ndc_names.rs`)

`tag_table.rs` is transcribed from cardano-ledger, never from dugite's encoder. Every constructor quotes
its source line at revision `226b002d5b5e83e24355f8a28ab214f3259eabda` (tag
`cardano-ledger-conway-1.22.1.0`, the revision cardano-node 11.0.1's `cabal.project` pins). The tags do
not depend on the protocol version (a constructor keeps its tag; the version decides which one the ledger
raises, e.g. `IncorrectDepositDELEG` below PV11, `DepositIncorrectDELEG` from it, and that choice is made
by the conversion above, at the fixture's version).

It is checked three ways:

* `the_tag_table_decodes_the_haskell_golden`: a **Haskell-produced** `ApplyTxErr`.
  `goldens/ApplyTxErr_Conway` is
  `ouroboros-consensus-cardano/golden/cardano/QueryVersion3/CardanoNodeToClientVersion19/ApplyTxErr_Conway`
  of IntersectMBO/ouroboros-consensus (last changed in `90c220fe61130c8b9793bbe9c75ce0a9a59397bb`, bytes
  copied unchanged, SHA-256 `d57c0f43771b4cd3d7033d727d91a146877f8960969b35524cb470bbfb23503b`). It is the one
  Conway `ApplyTxErr` golden upstream publishes and exercises one path:
  `ConwayCertsFailure/CertFailure/DelegFailure/DelegateeStakePoolNotRegisteredDELEG` `[2, 1, 1, 6]`. The other
  96 constructors rest on the quoted source lines; no upstream golden covers them.
* every row's `source` line was re-read at the pinned revision (the constructor name and tag are on that
  line; POOL and `Credential` are hand-written `encodeListLen` code, so their line carries the tag literal).
* `a_wrong_tag_table_entry_turns_a_fixture_red`: a table that swaps two constructors turns a fixture red.

`ndc_names.rs` maps a Haskell constructor to the scenario's name where they differ. Rows are read from
Yano, Dingo, Scarab and Gerolamo, which map the same Haskell failures onto the same names, and from the
fixtures. Four rows that appeared in those sources are deliberately NOT carried, because each would turn a
wire-level divergence into a match (see the comment at the top of the file).

## The context is the ledger's

`state.rs` populates the ledger's own state (UTxO, reward accounts, pools, DReps, committee, proposals,
roots, pots, constitution guardrail). Interpretations the fixtures force, each stated where it is made:

* `valid_until` is a member's term in the enacted committee; `status` is its authorisation state. A member with a
  `status` and no term is authorised but unelected (00171, 00217); one with neither is a bare member (00211).
* `guardrail_script` absent or null still enacts a constitution (with no guardrail).
* a fixture's output with an indefinite outer map is made definite **in state only**; the transaction is never
  touched (see below).
* the committee is built as the reference checker builds it (`InitialState.hs`, `electedCommitteeMembers`,
  `committeeAuthorizations`, `orphanAuthorizations`): `valid_until` elects a member, `status` is its authorisation
  entry, and every member WITHOUT `valid_until` is added through one synthetic `UpdateCommittee` proposal's
  `members_to_add`. `ConstitutionalCommittee` proposals are `NoConfidence`, other stand-ins follow `toGovAction`,
  every initial proposal is proposed in the current epoch, and the system start is POSIX 0 (`Run.hs`).
* `k = 2160`, magic 1; the fixture's era history (one open era from slot 0, asserted) supplies the epoch size and
  the per-transaction safe-zone horizon.

Both tests below count SCORED results only (a fixture refused at decode has none; the shadow run is a diagnostic).
`context_none_set_is_the_documented_one` (exactly the documented context fields are `None`:
the Shelley-Babbage MIR fields, and `drep_deposits`, which the node's own context builder leaves unset, see
the `drep-incorrect-refund` class) and `dropping_each_initial_state_section_is_observable` (the negative
control: dropping a section turns a scored fixture red; `governance_activity` is read by no admission-path rule, so it
is the one listed and asserted section; `proposals_roots` left the list when #1138 was fixed, because the five fixtures
that carry it now reach a rule).

## The contract

`expected-results.toml` lists every fixture dugite does not match: `(id, observed result)`, a class and an
**open** issue (`scripts/node-diversity/check-expected-issues.sh`). It is not a skip list. The gate fails on an
unknown id, on an entry whose fixture now matches, on an observed result that changed, and on a fixture that
fails without an entry. `raised-snapshot.json` is the full set of failures raised per fixture, so a new
over-rejection shows as a diff. Red-proofs: `gate_fails_on_*`.

Never edit an entry to make a number match. Fix the ledger and delete the entry.

## The shadow run

Before #1138 was fixed, 108 fixtures were refused at decode because dugite did not accept an indefinite-length
output map, so no rule ran on them. For a fixture still refused at decode the runner re-runs the rules on the
transaction with only the outer map header of each output made definite, keeping the original body bytes, hash
and size. It is a diagnostic: it appears in the snapshot, never in the contract. Since the fix only the two
fixtures that expect a decoding failure (00362, 00363) are in that situation; the rules that the decode refusal
had been hiding are now ordinary entries (#1160).

## What this does not prove

* Six fixtures are `ValidationTagMismatch`, so the set is not purely phase-one.
* PV9/PV10 only (three fixtures override to PV11); no evidence for PV11 behaviour in general or for Dijkstra.
* Names are read from the bytes, but the bytes' payloads are not compared (the scenario scores names only), except
  the `ValidationTagMismatch` narrowing.
* One `ApplyTxErr` golden path (see above).

`DecodingFailure` is the one scenario name not read from bytes: an undecodable transaction has no `ApplyTxErr`.
Haskell fails it at the codec layer (a `DeserialiseFailure`, connection dropped); dugite answers a
`ConwayMempoolFailure` with the text "transaction decode failed: ..." (`encode.rs`). The two fixtures that
expect it and match (00362, 00363) are pinned to fail for their own reason (the reference-script shape)
(`decoding_failure_fixtures_fail_for_their_own_reason`).
