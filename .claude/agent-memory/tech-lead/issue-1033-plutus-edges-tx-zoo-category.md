---
name: issue-1033-plutus-edges-tx-zoo-category
description: tx-zoo 18-plutus-edges (12 scripts) implementation — collateral/ref-input/datum edge cases, one real dugite gap found (collateral_return skips minUTxO), one vendored non-plutus-examples.json artifact
metadata:
  type: project
---

Implemented `testnet/local-devnet/tx-zoo/18-plutus-edges/` (#1033, part of the
#1031 cardano-node-tests adoption program) as a scoped, no-devnet-run task:
12 scripts + `_edge-helper.sh` (mirrors `_cert-neg-helper.sh`'s
`expect_cert_rejection` three-outcome pattern as `expect_utxo_rejection`) +
one new vendored fixture `tests/conformance/upstream/plutus-v2-v3-builtins.json`.

## Real finding, not fixed (in scope was tests only)

`crates/dugite-ledger/src/validation/phase1.rs` Rule 5 (~line 1353,
`for output in &body.outputs`) checks minUTxO on regular outputs only. It
never folds `body.collateral_return` into that check. Haskell's
`allSizedOutputsBabbageTxBodyF` (Babbage `TxBody.hs`) appends
`collateralReturn` onto the output list before running
`validateOutputTooSmallUTxO`, firing `BabbageOutputTooSmallUTxO` (tag 21) —
oracle-verified against live `cardano-ledger` master, 2026-08-06. dugite has
NO equivalent check anywhere (grepped collateral.rs too — nothing). Test
`18d-return-collateral-below-minutxo.sh` asserts the Haskell-correct
rejection and will likely FAIL (dugite over-accepts) the first time it's run
on a live devnet — that's the intended signal, not a test bug. Worth a
follow-up issue once devnet-validate actually runs this category.

## Confirmed-correct (oracle-verified), used as PASS assertions

- `CollateralContainsNonADA`, `InsufficientCollateral`,
  `IncorrectTotalCollateralField`, `BabbageNonDisjointRefInputs` — all fully
  wired dugite constructors, high confidence.
- `ScriptsNotPaidUTxO` (collateral input at a script address) — dugite
  already implements this exactly (`ValidationError::ScriptLockedCollateral`
  → wire tag 13 → `ScriptsNotPaidUTxOUTXO`). The issue's "pin the submit-path
  constructor live" instruction undersold how solid this one already is.
- `NotAllowedSupplementalDatums` — dugite's `ExtraDatumWitness` maps to wire
  tag 12 correctly in `serve.rs`'s TYPED N2C mapping (there's a SEPARATE,
  unrelated `ExtraDatumWitness => ScriptFailed` degrade at serve.rs:920 in a
  different code path — don't confuse the two when grepping).
- V1-script-with-reference-input Conway inversion: dugite already implements
  the era gate correctly in
  `crates/dugite-uplc/src/tx_info_populate.rs::check_v1_output_restrictions`
  (`conway_or_later` bool gates off the Babbage blanket-reject rule). No gap.
- BabbageNonDisjointRefInputs is PV-windowed (8 < PV < 11) in dugite,
  mirroring Haskell PR #5011 exactly — devnet's Conway PV10 sits inside that
  window, so 18f is a valid reject case TODAY but would flip to accept (moving
  to a phase-2 `ConwayContextError::ReferenceInputsNotDisjointFromInputs`) if
  the devnet ever moves to PV11. Documented in the script header, not fixed.

## Vendoring methodology for the ONE extra artifact (18l)

Needed a PlutusV2 script exercising `byteStringToInteger` (a V3-era builtin
retrofit into the V2 cost model) — not present in `plutus-examples.json`
(cardano-ledger's own Plutus.Examples set doesn't include it). Traced via
`gh api search/code` (works without extra auth for cardano-node-tests, unlike
GitHub's public code-search API which 401s) →
`tests_conway/test_update_plutusv2_builtins.py` →
`tests_plutus_v2/mint_raw.py::check_missing_builtin` →
`plutus_common.BYTE_STRING_ROUNDTRIP_V2_REC` →
`cardano_node_tests/tests/data/plutus/v2/byteStringToIntegerRoundtripPolicyV2.plutus`.
Fetched via `curl` (raw bytes), NOT `WebFetch` (WebFetch runs an LLM
transcription pass over content — verified byte-identical against curl this
time, but do not trust it alone for hex/cborHex payloads; always cross-check
with a direct fetch for anything hash-sensitive).

Key trap avoided: this artifact's `cborHex` came from cardano-node-tests' OWN
cardano-cli text envelope, which is ALREADY double-wrapped the way cardano-cli
expects (outer CBOR `bstr(34)` containing inner `5820`-prefixed `bstr(32)`).
This is the OPPOSITE of `plutus-examples.json`'s `script_hex` (a bare flat
hex needing `lib/build-plutus.sh`'s `_wrap_cbor_bytes` to wrap it once). Do
NOT run cardano-node-tests-sourced envelope hex through `_wrap_cbor_bytes` —
that produces a triple-wrapped script cardano-cli still silently loads, under
yet another wrong hash (the #836/#969 vendoring trap, recurring in a new
place). Verified hash locally: `cardano-cli conway transaction policyid
--script-file` gives `7185ac3c12f0dc1cb6c7cbe1fcdf77bfd0c7943c41e7f33e5469aad1`
against commit `ad1430e3d3747ab48b2adac085e1845a8dab508c`.

## Scoping decisions

Task explicitly forbade touching `run-all.sh`/`denominators.json`/`SKILL.md`
even though the issue itself calls for those edits — the category is
DELIBERATELY not wired into `ALL_CATEGORIES` yet (a `18-plutus-edges` dir on
disk but absent from the array will hard-fail run-all.sh's own drift guard at
the NEXT full run until someone adds it — that's a known, deliberate
follow-up, not an oversight). Chose "add a materialise step in
`_edge-helper.sh`" over extending `build-plutus.sh` for the 18l vendored
script (the task explicitly offered both options) to keep the diff scoped to
the new category directory only.

## Result-row convention worth remembering

Every zoo script must emit exactly ONE `zoo_record` row under its own
`$NAME` — several existing scripts (13g, 17f, 15e) call `zoo_record`
MANY times but only ONE ever fires (early-exit guards), not because they
report multiple sub-results. A multi-arm script (like 18b's collateral
bracket, under+exact) must check its "internal" arm manually and only
`zoo_record` the FINAL outcome — using the shared `expect_*_rejection`
helper for an internal-only sub-check would emit an extra row under a
different name and silently break the N-scripts-to-N-rows bookkeeping
`denominators.json` depends on.
