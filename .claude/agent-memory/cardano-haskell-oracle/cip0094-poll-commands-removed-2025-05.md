---
name: cip0094-poll-commands-removed-2025-05
description: CIP-0094 SPO poll commands (create/answer/verify-poll) were REMOVED from cardano-cli 2025-05-10 (PR #1178); still live in cardano-api's Poll.hs. Exact SHAs, wire format, signing model.
type: reference
---

# CIP-0094 on-chain SPO polls — cardano-cli removed them, cardano-api kept the module

Resolves the discrepancy behind dugite issue **#998**: a locally-installed
`cardano-cli 11.0.0.0` (git rev `97036a66…`) has zero `poll` commands anywhere
in its help tree. This is NOT a bug in the dump — the commands were deleted
upstream over a year before that build.

## Timeline (all SHAs verified via `gh api` against IntersectMBO/cardano-cli)

- **Introduced**: cardano-node PR #5112 "Add new interim governance commands:
  {create, answer, verify}-poll", merged 2023-04-17
  (`cf61eb378049f7e9ae854de998c9bff571b3acfe`), back when cardano-cli still
  lived inside `input-output-hk/cardano-node`. Original invocation:
  `cardano-cli shelley governance {create,answer,verify}-poll` (flat era-tree,
  pre unified-parser refactor). CIP reference: cardano-foundation/CIPs#496.
- **Moved to 'babbage governance' block**: cardano-cli PR #322, merged
  2023-10-05 (`4c615c9e25371c1081384732bbfcb57b39ddbbec`, commit
  `59adca1aa84f3281f1a54479c79b923622e1329b`).
- **Briefly turned OFF for Conway** (2023-10-02, commit
  `56b1fcace895dd88ce4ec69d3adaf380a318468f`), then **"bring back legacy
  *-poll commands"** PR #349, merged 2023-10-10
  (`0fef8a4eee4c2feba99e4c839805bb0aa2b76b00`) — restored gated to
  Babbage-only, never re-enabled for Conway. The parser literally does:
  `when ("BabbageEraOnwardsConway" \`isInfixOf\` show w) Nothing` in each of
  the three `pGovernance{Create,Answer,Verify}Poll` functions
  (`EraBased/Governance/Poll/Option.hs`).
- **REMOVED**: cardano-cli PR #1178 "Delete `governance` `poll` commands",
  merged **2025-05-10** (`4b548aad8bf6ba0ca55559f544b524935266812d`, commit
  `db83e11127092b4c216eed5572c4623b8ac51e79`, parent/last-standing commit
  `0bf3e93d04a1d4a0dd0fcaa6c5172028724e1cea`). PR body: "These are temporary
  `babbage` era only commands" — no CIP-status reason given, purely
  maintenance/dead-weight cleanup (`breaking, maintenance` changelog tags).
  **Confirmed release boundary**: last release WITH the commands is
  `cardano-cli-10.8.0.0` (commit `685970733dc4ef5838967cb7cfb6d3fe4c2a2b06`,
  2025-04-18); first release WITHOUT them is `cardano-cli-10.9.0.0`
  (`e13f84d9fc9cafa293e88f017592d994ca1b12a2`, 2025-05-15). Verified by
  diffing the tag range and confirming
  `cardano-cli/src/Cardano/CLI/EraBased/Governance/Poll/` exists at
  `10.8.0.0` and 404s at `10.9.0.0`.
- **At removal time, the ONLY reachable invocation was**
  `cardano-cli compatible babbage governance {create,answer,verify}-poll`
  — confirmed via the golden `help.cli` dump at the pre-removal commit; there
  was no bare top-level `cardano-cli governance create-poll` by 2025. A
  `Cardano.CLI.Legacy.Governance.{Command,Run}.hs` copy of the same
  constructors existed but fed the "compatible" parser tree, not a separate
  invocation path.
- **cardano-api never removed it.** `cardano-api/src/Cardano/Api/Governance/Internal/Poll.hs`
  is present on the current default branch, re-exported from
  `Cardano.Api.Governance` (`GovernancePoll`, `GovernancePollAnswer`,
  `GovernancePollError`, `hashGovernancePoll`, `verifyPollAnswer`). The file
  carries `{-# OPTIONS_GHC -Wno-deprecations #-}` at the top but no
  `{-# DEPRECATED #-}` pragma was found on the Poll types themselves in the
  456-line file — the pragma is almost certainly suppressing warnings from
  other deprecated cardano-api internals it depends on, not marking Poll
  itself deprecated. CIP-94 is still **Status: Active** in
  cardano-foundation/CIPs (not withdrawn), so the API-level building blocks
  remain a legitimate, spec-current target for a Rust port even though no
  Haskell CLI currently exposes them.

## Wire format / semantics (all verified from source, not memory)

- Metadata label **94**, confirmed `pollMetadataLabel = 94` in Poll.hs.
- Question metadata: `{94: {0: prompt_chunks, 1: [choice_chunks...], "_"(TxMetaText): nonce?}}`.
  Answer metadata: `{94: {2: poll_hash(32B), 3: choice_index}}`.
  Matches CIP-94 CBOR grammar verbatim (`question`/`answer` productions).
- Text fields are chunked at **64 UTF-8 bytes per chunk**
  (`txMetadataTextStringMaxByteLength = 64` in
  `Cardano.Api.Tx.Internal.TxMetadata`, same constant used for CIP-20 style
  `msg` chunking) via `metaTextChunks` → `TxMetaList [TxMetaText chunk, ...]`.
- **Poll hash** = `hashWith @HASH serialiseToCBOR poll` where `HASH = Blake2b_256`
  (`libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs`), and
  `serialiseToCBOR poll = serialiseToCBOR (asTxMetadata poll)`. Critically,
  `asTxMetadata` produces the **full labelled map** `{94: {...}}`, and
  `SerialiseAsCBOR TxMetadata` encodes via
  `CBOR.serialize' CBOR.shelleyProtVer . toShelleyMetadata . unTxMetadata` —
  i.e. the hash preimage is the CBOR of the ENTIRE `{94: {...}}` structure at
  `shelleyProtVer`, not just the inner `{0:...,1:...}` map. The CIP text
  ("entire serialised question metadata payload") is ambiguous on this point;
  the Haskell source is not.
- **No poll-specific signature scheme.** `verifyPollAnswer` does NOT
  cryptographically verify anything — it (1) checks the answer's embedded
  poll-hash matches `hashGovernancePoll poll`, (2) checks the choice index is
  in bounds, (3) reads `txExtraKeyWitnesses` — the tx BODY's declared
  required-signers field — and returns those key hashes as "signatories".
  Actual Ed25519 signature validity is never checked by this function; the
  doc comment says so explicitly: "signatures aren't checked as it is assumed
  to have been done externally (the existence of the transaction in the
  ledger provides this guarantee)." Authentication is 100% delegated to
  normal Cardano tx witnessing (`--required-signer`/cold-key witness),
  confirmed by commit `215ebdf5da42` ("Remove VRF signing option... rely on
  the existing witness mechanism of transactions").
- `verify-poll` is **purely offline** — reads a poll text-envelope file and a
  tx file, no node query, no `LocalStateQuery`.
- Exit/output: success → `"Found valid poll answer with N signatories"` to
  **stderr**, signatories array JSON to stdout/`--out-file` via
  `writeByteStringOutput` (`Nothing` → `BSC.putStr` to stdout, no added
  newline; `Just fp` → `BS.writeFile`). Failure → top-level handler prints
  `"Command failed: <cmd>\nError: <renderGovernancePollError text>"` to
  stderr (`Cardano.CLI.Render.renderAnyCmdError`) and `exitWith (ExitFailure 1)`.

## Flags as they existed (from `Cardano.CLI.EraBased.Common.Option`, lines ~1005-1047, pre-removal)

- `create-poll`: `--question STRING`, `--answer STRING` (repeatable, `some`),
  `--nonce UINT` (optional), `--out-file FILEPATH`.
- `answer-poll`: `--poll-file FILEPATH`, `--answer INT` (optional, prompts
  interactively if omitted), `--out-file FILEPATH` (optional).
- `verify-poll`: `--poll-file FILEPATH`, `--tx-file FILEPATH`, `--out-file
  FILEPATH` (optional).

Golden `--help` text (verbatim, pre-removal commit
`0bf3e93d04a1d4a0dd0fcaa6c5172028724e1cea`) captured in full in the
conversation this memory was written from — see dugite issue #998 for the
complete transcript if regenerating a Rust CLI surface.

Full source of `Poll/{Command,Option,Run}.hs` and `Api/Governance/Internal/Poll.hs`
was pulled from GitHub at these SHAs and read in full; ask for a re-fetch
rather than trusting this summary for byte-level porting work — go back to
source for the actual CBOR/JSON instance code.
