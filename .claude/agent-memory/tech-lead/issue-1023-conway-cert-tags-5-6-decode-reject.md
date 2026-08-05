---
name: issue-1023-conway-cert-tags-5-6-decode-reject
description: P1 fix — Conway/Dijkstra must hard-reject cert tags 5 (GenesisKeyDelegation) and 6 (MIR) at decode; era-type-swap not PV-gated; found a third Dijkstra-only divergence (tags 0/1) while there
type: project
---

## #1023 (2026-08-05) — accept-where-Haskell-rejects on LIVE Conway mainnet

dugite's Conway/Dijkstra certificate decoder (`read_conway_certificate` in
`crates/dugite-serialization/src/decode/era_conway.rs`) accepted cert tags 5
(`GenesisKeyDelegation`) and 6 (`MIR`) — certificate types real
cardano-node hard-rejects at CBOR decode in Conway. Reachable via ordinary
transaction construction (no hand-crafted CBOR needed, unlike #1013) — the
#996/#997 class of bug, and worse than most: a MIR/GenesisKeyDelegation cert
decodes fine, then `validate_mir_cert`'s PV>=9 short-circuit silently
no-ops it (no error at all), so it sails all the way to mempool admission
and can be forged into a block every Haskell peer then permanently
re-requests-and-rejects (#996 mechanism).

**Root cause class, independently re-verified (not just trusting an oracle
subagent — fetched the raw upstream files myself via
`gh api repos/IntersectMBO/cardano-ledger/contents/<path>?ref=<sha>` and
decoded the base64 content directly):** at pinned SHA
`4849c13d6f70e5ab46add9af6e0ec5c537b61f69`,
`eras/conway/impl/src/Cardano/Ledger/Conway/TxCert.hs:719-726` has explicit
`fail` arms for tags 5/6 — a clean, unconditional era-TYPE swap
(`type TxCert ConwayEra = ConwayTxCert ConwayEra`, a disjoint 3-constructor
sum with no MIR/GenesisDeleg case at all), **not** PV-gated. Every
pre-Conway era (Shelley/Allegra/Mary/Alonzo/Babbage, confirmed by fetching
all five `TxCert.hs` files) aliases `type TxCert <Era> = ShelleyTxCert <Era>`
and keeps accepting both tags forever.

**This mattered because the task explicitly warned the mechanism could be
PV-gated instead** (citing the aux-data `guardPlutus`/`natVersion` shape
from #1014, which landed on `main` mid-session and IS that mechanism, for a
different field). Two structurally different mechanisms exist side by side
in the same codebase now — do not assume one implies the other. Always
fetch the actual decoder, don't infer from a sibling fix's shape.

**Bonus finding while there — filed separately, not fixed:** Dijkstra's own
`DijkstraTxCert` decoder (`eras/dijkstra/impl/.../TxCert.hs`) additionally
hard-rejects tags 0 and 1 (`StakeRegistration`/`StakeDeregistration`
without a deposit) — a THIRD divergence beyond Conway's 5/6, since
dugite's `read_conway_certificate` is shared between Conway and Dijkstra
dispatch with no `era` parameter. Filed as **#1029**, deliberately NOT
folded into #1023: Dijkstra is unreleased (zero live consensus risk today,
matches the "documented fail-closed gap" precedent used elsewhere for
Dijkstra in this same file), and closing it needs `read_conway_certificate`
to thread `era: Era` through two call sites — a real design decision, not a
two-line patch. **Lesson: when era-scoping a decoder, always check ALL
downstream eras for their OWN divergences, not just the one the issue
names — #1012/#1013 already established fixing named instances leaves
siblings behind, and this session found a third sibling one era further
out.**

## Test-coverage gaps this surfaced

- `encode/certificate.rs`'s `every_conway_certificate_round_trips_through_our_own_decoder`
  test only exercised 15 of the 17 valid Conway tags (missing
  `PoolRegistration` tag 3 and `VoteRegDeleg` tag 12) — found while building
  an independent exhaustive per-tag decode test and cross-checking counts.
  A "round-trips every X" test name is not proof it does; count the tags.
- No pre-Conway "still decodes" test existed for Allegra/Mary/Alonzo/Babbage
  specifically (only Shelley had one, including a real mainnet fixture at
  block 7492516/slot 66137371). Added direct-reader tests in
  `era_alonzo.rs` (covers all three via the shared `read_alonzo_certificate`)
  plus full tx-body-level tests in `era_babbage.rs` (the sharpest edge —
  the era immediately before Conway, reached via
  `read_alonzo_cert_inner` cross-file reuse).
- `fuzz/src/lib.rs`'s `Gen::certificate_for` could still generate a
  GenesisKeyDelegation/MIR cert for `era >= Era::Conway` (`self.choice(19)`
  covered the full 0-18 index range unconditionally) — would have made
  `fuzz_structured_tx_encode` panic with a misleading "#948 shape, always
  an encoder bug" diagnosis the moment the fuzzer hit index 17/18 for
  Conway/Dijkstra. Found by a dispatched read-only search agent explicitly
  tasked with hunting this class across the WHOLE repo (fixtures, fuzz
  seeds, other round-trip assertions, docs) — it found this one real issue
  and cleared everything else as either not-yet-reachable (real on-chain
  fixtures structurally can't carry these tags) or already correctly
  documented. Fixed by narrowing `choice(19)` → `choice(17)` (indices 0-16
  are exactly the tags still valid post-fix).

## Traps hit this session (own mistakes, not the codebase's)

- **Exit-code capture bug**: `just check > log.txt 2>&1; echo "EXIT_CODE=$?"`
  — the trailing `echo` was NOT redirected into the log, only `just check`
  was. The bash tool's own wrapper reported "exit code 0" for the overall
  command, but that 0 was for `echo` (which always succeeds), not for
  `just check`. Had to verify the true result by grepping the log CONTENT
  (`Summary [...] N tests run: N passed`, zero `error[`/`FAILED` lines)
  instead of trusting the reported exit code. Redo pattern that actually
  works: `cmd > log 2>&1; echo "EXIT_CODE=$?" >> log` (note `>>` on the
  echo, appending to the SAME file, not a bare unredirected echo after
  the `;`). This is the exact same class of trap CLAUDE.md already
  documents for `| tail` swallowing exit codes — `;`+bare-echo is another
  way to get the same false-green result.
- **Idling on background jobs without a report is a repeat-offender
  pattern** (per the coordinator: eighth agent this session to do it).
  Ending a turn on "standing by" / "waiting for notification" with no
  substantive interim report is indistinguishable from having given up.
  Always produce the full report with everything already verified BEFORE
  the last background dependency resolves, clearly flagging the one
  remaining open item — don't wait to have 100% of the picture before
  saying anything.
- **A parallel `just check` run manually backgrounded via shell `&` plus a
  second one via the Bash tool's own `run_in_background` contend for the
  SAME `target/` lock** if launched from the same worktree — caused one to
  block on "waiting for file lock on build directory" and wasted a build
  cycle. Only ever launch ONE `just check` per worktree at a time; kill
  redundant self-launched duplicates by exact PID (verified via `ps`), never
  by pattern.
- **After rebasing onto a moved `main`, an external process (outside this
  agent's own tool calls) merged the rebased branch into `main` mid-task**
  — confirmed via `git show <merge-sha> --stat` that the merge tree was
  byte-identical to the already-gate-verified pre-merge commit (empty
  `git diff <merge> <branch-tip>`), so the CI run done BEFORE the merge
  remained valid evidence for the state AFTER it. Don't assume a merge
  automatically invalidates prior verification — check whether the merge
  commit's tree actually differs from what was tested.
