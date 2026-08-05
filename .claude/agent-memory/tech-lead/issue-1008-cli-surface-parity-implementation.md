---
name: issue-1008-cli-surface-parity-implementation
description: First implementation pass on the #1008 CLI surface-parity backlog — 5 new commands, 3 renames, 1 checker rule, plus a Plutus script-hash CBOR-wrapper detail and a walker-integrity verification
metadata:
  type: project
---

Worked #1008 (the 82-command backlog from #1006's cli-surface-parity.sh
first run) in worktree `issue-1008-cli-surface`. Full triage posted as two
comments on #1008 (2026-08-05) — that's the durable artifact; this memory
covers what's worth remembering beyond the code.

## Result

69/151 -> 77/149 matched. 10 allowlist entries resolved: 5 new commands
(`hash anchor-data`/`script`/`genesis-file`, `version`,
`governance drep metadata-hash`) + 5 via renames/checker rule (`stake-pool
deregistration-certificate`, `stake-address stake-delegation-certificate`
made primary names with old dugite names as `visible_alias`; `query
tx-mempool info`/`next-tx`/`tx-exists` collapsed via a new
`collapse_structural_equivalents` rule in cli-surface-parity.sh, `has-tx`
renamed to `tx-exists`). 6 commits, rebased clean onto main (which had moved
with #1011/#1012 — no conflicts, disjoint crates).

## Plutus script-hash: the CBOR wrapper must be RETAINED, not stripped

`hash script`'s Plutus-envelope path: hash input is
`blake2b_224(tag || cborHex_bytes_AS_IS)` where `cborHex_bytes` still
carries a CBOR byte-string header — NOT the bare flat-encoded UPLC program.
This is the well-known "Plutus scripts are double-CBOR-wrapped" fact
(see [[uplc-kont-depth-scope-check-decode-source-842-836-817-823]] — #836
proved ref scripts are double-wrapped) applied to the SafeToHash instance:
`cardano-api`'s `PlutusScriptSerialised.serialiseToCBOR` is a deliberate
IDENTITY over that 1-wrap form, so a text envelope's `cborHex` field
already IS the exact hash input. Verified three independent ways: (1)
cardano-ledger-oracle citing `Plutus/Language.hs` + the identity
serialization, (2) empirically against a real mainnet PlutusV2 script hash
via Koios, (3) against all three V1/V2/V3 hashes in the vendored
`tests/conformance/upstream/plutus-examples.json` fixture. All three agree
and match real interactive `cardano-cli hash script` output.

`dugite-primitives::hash::blake2b_224_tagged`'s doc comment describes the
OPPOSITE convention ("data is the inner content of the CBOR bstr, NOT
CBOR-encoded") — inconsistent with the correct code path
(`dugite-ledger`'s `compute_script_ref_hash`, and this pass's `hash
script`). The helper appears unused by any correct call site, so not an
active bug, but a footgun for a future author who trusts the doc comment.
Flagged in the #1008 issue comment; NOT fixed (dugite-primitives, out of a
CLI-surface-parity pass's scope).

## Walker integrity: verified, not just reasoned about

A reviewer flagged that `query tx-mempool info|next-tx|tx-exists` renders
as a positional alternative `(a | b | c)` in cardano-cli's `Usage:` line,
raising the question of whether `cli-surface-parity.sh`'s walker generally
mis-treats positional value alternatives as subcommands (which would mean
part of the 82-entry backlog is a parsing artifact, not a real gap).

Checked and it does NOT: the walker's `extract_commands()` never reads the
`Usage:` line at all, only the `Available commands:` section — and that
section only gets per-item descriptions for genuine `Opt.command` entries
in optparse-applicative (tx-mempool's three verbs ARE such entries,
confirmed: each has its own leaf-specific `Usage:` line when invoked
directly, exit 0). Wrote a standalone verifier reusing the walker's own
`extract_commands()`/`walk()` functions verbatim, re-walked cardano-cli
from the root (all 386 raw leaf paths, every era prefix), and confirmed
EVERY ONE independently dispatches (exit 0 + leaf-specific `Usage:` line
ending in its own name). 386/386 passed. No walker fix made — nothing to
fix, and speculative hardening against a non-manifesting failure mode
would just be unverifiable complexity on top of a parser that already has
two real documented bug fixes in its history (#1006).

## Process notes

- Renaming a clap subcommand VARIANT (not just adding `visible_alias`) was
  necessary to make the cardano-cli name discoverable by the walker: it
  only reads the FIRST token of each `Commands:` help line, never
  `[aliases: ...]`. An alias-only fix (keep old name primary, add new name
  as alias) stays invisible to the walker even though the command works
  when invoked directly — this is a real trap for future similar renames.
- Found (but did not fix, out of scope): a systemic cert-file JSON envelope
  divergence — dugite emits `"type": "CertificateShelley"` (2-space indent,
  no trailing newline, `cborHex`/`description`/`type` key order) where real
  cardano-cli emits `"type": "Certificate"` (4-space indent, trailing
  newline, `type`/`description`/`cborHex` order). CBOR cert BODY is
  byte-identical (verified) — client-tooling-facing only, not
  consensus/wire. At least 6 call sites (`genesis.rs`, `stake_address.rs`
  x3, `stake_pool.rs`), pre-existing, unrelated to this session's renames.
  Worth its own issue.
- No live-devnet fixture exists for fast golden-testing of node-backed
  query commands (`query drep-stake-distribution`/`era-history`/etc.) —
  `dugite-integration-tests`'s tier1/tier2 tests require an
  ALREADY-RUNNING synced node via `DUGITE_INTEGRATION_SOCKET`, and
  `devnet-validate` is a many-minute multi-round harness. This is why those
  query commands were scoped but deferred rather than implemented with
  unverified JSON shapes.
