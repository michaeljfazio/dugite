---
name: issue-998-cip94-poll-commands-removed
description: cardano-cli deleted governance {create,answer,verify}-poll in 2025-05 (PR #1178); CIP-0094 issue premise was stale, dugite deliberately does not implement
metadata:
  type: reference
---

#998 asked for CIP-0094 SPO poll commands (`create-poll`/`answer-poll`/
`verify-poll`) in dugite-cli, framed as "cardano-cli exposes three: ...". That
premise was stale. Verified four independent ways (local binary help-tree
dump, GitHub API, PR diff, live source at the pre-removal tag) that
`cardano-cli` **deleted** these commands in PR #1178 ("Delete `governance`
`poll` commands", merge `db83e11127092b4c216eed5572c4623b8ac51e79`,
2025-05-08) — a full release before `cardano-cli-11.0.0.0`
(`97036a66bcf8c89f687ae57a048eecc0389977ef`), the build this project targets
for parity. Last release with them: `cardano-cli-10.8.0.0`
(`685970733dc4ef5838967cb7cfb6d3fe4c2a2b06`).

Even while the commands existed (2023-04 to 2025-05), they lived under
`compatible babbage governance {create,answer,verify}-poll` (not bare
`governance`) and were **permanently excluded from Conway** by the era
parser:
```haskell
pGovernanceCreatePoll era = do
  w <- forShelleyBasedEraMaybeEon era
  when ("BabbageEraOnwardsConway" `isInfixOf` show w) Nothing
  pure $ ...
```
Same guard on answer-poll/verify-poll. Never reachable on the only era dugite
targets, even at the feature's peak.

`cardano-api`'s `Cardano.Api.Governance.Internal.Poll` module is STILL
exported on current cardano-api — only the CLI front-end is gone. CIP-0094
itself remains `Status: Active`. So this is a case where the CIP spec and the
underlying library are both alive but the CLI surface to target for parity is
dead.

**Decision, applying the CIP-0121/plutus precedent mechanically**: cardano-cli's
actual implementation (zero poll commands) wins over the CIP text. Did NOT
implement the three commands in dugite-cli — doing so would add surface with
no live `cardano-cli` invocation to golden-test against (this project's
stated highest-value guard for exactly this class of change) and no
reachable era to exercise it in. Closed #998 as not planned with full
findings posted; module doc comment added to
`crates/dugite-cli/src/commands/governance.rs` recording the SHAs so a
future CIP audit doesn't re-flag this without context.

Filed **#1006** as a follow-up for the still-valid secondary finding: no
suite in the release gate enumerates `cardano-cli`'s subcommand surface
against `dugite-cli`'s (a real future gap would be just as invisible as this
fictional one was). Concrete design proposed: a recursive `--help`-tree
walker (not a hardcoded era list — same #971/#969 "absent from a hardcoded
list never runs" trap applies), diffed with an allowlist requiring a tracking
issue per entry (mirrors the ledger-rules `SKIP_LIST` pattern), runnable in
CI with no live devnet needed since it only needs both binaries built.

See also [[feedback_haskell_byte_exact_only]] and the CIP-0121/plutus
precedent in CLAUDE.md for the general pattern this follows: when the CIP
spec and the actual deployed reference implementation disagree, the deployed
implementation wins — and here that meant implementing *nothing* rather than
implementing the spec's version of a feature the reference tool dropped.
