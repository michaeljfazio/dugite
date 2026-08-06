---
name: issue-1050-1051-18-19-tx-zoo-shakedown
description: First live shakedown of tx-zoo 18-plutus-edges + 19-era-negatives — 3 real encoder bugs filed (#1050, #1051), 4 test-construction bugs fixed script-side
metadata:
  type: project
---

Live shakedown of the two newest tx-zoo categories (#1033 `18-plutus-edges`,
#1034 `19-era-negatives`) against the running 3-node devnet (dugite-relay +
dugite-bp + cardano-bp Haskell arbiter, PV10/Conway), 2026-08-06. Final
stable state: 16/19 pass, 3 fail — all 3 are confirmed real dugite bugs
(filed, not patched — this session did not touch `crates/`).

## Filed (real product bugs, live-reachable at PV10 = mainnet's current PV)

- **#1050** — `ValidationError::CollateralHasTokens` and
  `ValidationError::InsufficientCollateral` fire correctly server-side
  (confirmed via dugite-relay log) but have **zero match arm** in
  `crates/dugite-network/src/protocol/local_tx_submission/encode.rs`, so
  both degrade to the generic `ConwayMempoolFailure "transaction validation
  failed"` sanitized fallback. Same shape as #979/#925/#1025. dugite's own
  N2C *client* decoder (`n2c_client.rs:1804-1808`, `:1796-1802`) already
  knows the correct tags (15, 12) — only the encoder side never got an arm.
  `encode.rs:178` has a comment documenting tag 12 for `InsufficientCollateral`
  sitting directly above the `CollateralMismatch` (tag 20) arm — the tag was
  known, the arm was just never written.
- **#1051** (more severe) — the tag-22 (`BabbageNonDisjointRefInputs` /
  `ReferenceInputOverlapsInput`) reply is **malformed CBOR**: the encoder
  wraps the `NonEmpty TxIn` payload in a CBOR tag-258 "Set" marker
  (`encode.rs`, search `Tag 22`), but Haskell's field type is plain
  `NonEmpty TxIn` (oracle-confirmed against `cardano-ledger`
  `eras/babbage/impl/.../Utxo.hs:96-97` + `EncCBOR (NonEmpty a)` = bare
  `encodeList`, never `setTag`/258 — that's the genuine `Set.Set a` instance
  only). cardano-cli's decoder dies outright:
  `DeserialiseFailure N "expected list len or indef"` — the tx IS correctly
  rejected server-side, but the client can't even parse the reply. Reachable
  for any tx violating `disjointRefInputs` in dugite's own documented
  `8 < PV < 11` window, i.e. PV10 today.

## Fixed script-side (test-construction bugs, not dugite bugs)

- **18a**: `cardano-cli transaction build` (auto mode) silently neutralizes
  a token-bearing collateral input by auto-generating a `collateral_return`
  that returns the ENTIRE token balance — net collateral becomes ADA-only,
  legitimately accepted by both Haskell and dugite. Fixed by switching to
  `build-raw` with **no** `--tx-out-return-collateral`/`--tx-total-collateral`
  declared at all, so the token stays in the net balance (only way to force
  `isAdaOnly` to see a residual). This is what surfaced #1050 for real.
- **18h**: original premise (ordinary `--tx-in-script-file` witness on a
  UTxO that ALSO carries a matching attached `referenceScript`) expected
  ACCEPT. Wrong — oracle-confirmed against `getBabbageScriptsProvided`: the
  reference-script pool spans regular spending inputs too, so the ordinary
  witness for an already-reference-resolvable hash is itself
  `ExtraneousScriptWitnessesUTXOW` — and dugite's own
  `check_extraneous_script_witnesses` (scripts.rs, with a dedicated
  pre-existing test: "Witness script matching a script-locked input must
  not be flagged as extraneous" for the NON-reference case) already
  implements this correctly. A self-referencing `--spending-tx-in-reference`
  variant was tried next but hits #1051's bug (cardano-cli's
  `--spending-tx-in-reference X` always duplicates X into `reference_inputs`
  even when X is already `--tx-in`, so it's structurally the same
  non-disjoint-inputs case as 18f). Settled on the CLEAN, PV-independent
  fix: flip to a negative assertion on `ExtraneousScriptWitnessesUTXOW`
  (the ordinary-witness construction), which decodes cleanly and doesn't
  depend on #1051 or any PV window.
- **18a/18b/18c/18d/18e/18i**: shared `EXUNITS="(1000000,1000000)"` hardcode
  was under-provisioned — `always-true-v2.plutus` needs ~1,893,779 steps in
  practice (confirmed via a real `cardano-cli transaction build` auto
  estimate: `memory: 5894, steps: 1893779`) despite "trivial" script logic
  (CEK decode overhead for datum/redeemer dominates). Also discovered:
  cardano-cli's `--tx-in-execution-units (INT, INT)` tuple is **(steps,
  memory)**, not (memory, steps) — confirmed by matching `cpu_remaining` in
  a `ScriptFailed` budget-exhaustion server log against the FIRST tuple
  element. This only became visible once an ACCEPT-path (Phase-2-reaching)
  scenario was tested (18c's exact-match arm) — Phase-1 collateral checks
  correctly short-circuit before Phase-2 execution, so REJECT-path tests
  (18d, 18e, 18i as originally written) were never exposed to the bug. Fixed
  to `(2000000,1000000)` uniformly.
- `_edge-helper.sh`'s `expect_utxo_rejection` reason-extraction regex
  (`Babbage[A-Za-z]+` etc.) spuriously matched inert era-name tokens like
  `BabbageEra` inside a `DeserialiseFailure`'s HardFork-combinator
  boilerplate, mislabeling a wire-corruption bug as "wrong reason". Added a
  `DeserialiseFailure`-specific branch that reports it explicitly as a
  malformed-CBOR wire-encoder bug — this is what surfaced #1051 cleanly.

## Additional finding (not yet filed as its own issue): MsgRejectTx echoes the wrong era_id

`local_tx_submission/server.rs:111,181,200` — `encode_apply_tx_err(&e, era_id)`
uses the CLIENT-declared wire `era_id` verbatim as the outer HardFork NS index
in every `MsgRejectTx`, not the ledger's actual current era. Since the inner
payload is always encoded in dugite's Conway-shaped predicate vocabulary,
any era-mismatched submission (era_id != real chain era — the exact gap
#1047 documents as unimplemented) gets nested under the WRONG NS index.
cardano-cli's per-era reply decoder then picks the wrong shape and crashes
with a raw `DeserialiseFailure` Haskell exception instead of showing a clean
rejection — confirmed live on every 19a-19d run (era_id=1 Shelley
submissions against a Conway ledger; direct `cardano-cli conway transaction
submit` replay outside the harness reproduces it standalone). Distinct from
#1047 (missing the WrongEra CHECK itself — why these get rejected at all);
this is the malformed REPLY on the reject path that already fires today.
Worth fixing alongside #1047: adding the missing check without also fixing
this reply-tagging leaves the crash reachable via any other future
era-mismatch path.

## Methodology notes

- **`transaction build` auto-balancing can silently defeat a negative
  test's premise** — always verify the actual built tx shape via
  `cardano-cli debug transaction view` (NOT `transaction view`, which
  doesn't exist in this cardano-cli build) before trusting a
  positive-vs-rejected verdict on a hand-crafted edge case.
- **Server-side log (`dugite_ledger::validation: ... errors=[...]`) is the
  ground truth for root-causing a generic wire rejection** — the client-
  visible `ConwayMempoolFailure "transaction validation failed"` string is
  identical for at least 3 structurally distinct real causes (missing
  encoder arm x2, and separately genuine Phase-2 `ScriptFailed` budget
  exhaustion). Never classify a generic-fallback failure without checking
  the log for the actual typed `ValidationError`.
- **This worktree was NOT exclusively owned during this session** despite
  the task framing — a concurrent process was actively editing the SAME
  tx-zoo scripts throughout (visible via repeated "file modified, either by
  the user or by a linter" system reminders, a shared `results.csv` with
  interleaved/duplicate timestamped rows across scripts, and mid-flight
  "RED-PROOF SABOTAGE" WANT-flip states that self-reverted). Content and
  reasoning converged with this session's independent findings in every
  observed case (same root causes, same fix directions) — but it made
  `run-all.sh`'s aggregate summary and `results.csv` unreliable as a
  point-in-time oracle; scratch-directory reproductions
  (`/private/tmp/.../scratchpad/repro*.sh`, isolated from the shared tx-zoo
  state) were the reliable verification path. If this recurs, don't trust a
  single `run-all.sh` invocation's summary — rerun and cross-check against
  direct log/reproduction evidence before filing anything.
