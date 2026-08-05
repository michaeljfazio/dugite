---
name: issue-1018-1027-lsq-mempool-audit-2026-08-05
description: Conway N2C LSQ + mempool-reject audit — 2 fixed (tag33 GetFuturePParams, ensWithdrawals), 4 filed (NextEpochChange/ensCommittee, MIR/GenesisDeleg accept-where-reject, residual ScriptFailed degradations, ledger-state undecodable)
metadata:
  type: reference
---

# Conway LSQ + mempool-reject audit (2026-08-05, issues #1018-#1027)

Full audit of `crates/dugite-node/src/node/n2c_query/` (all ~40 LSQ query
tags) against Haskell, plus the `serve.rs` MsgRejectTx typed-failure mapping.
Method: read every handler/encoder, oracle-verify (`cardano-ledger-oracle`,
pinned `cardano-ledger` @ `4849c13d6f70e5ab46add9af6e0ec5c537b61f69`,
`ouroboros-consensus` @ `346de035ff8e19d0a0e5f9db7ea37511656ccc7f`), cross-check
against `testnet/local-devnet/evidence-archive/auto/*/cli-parity.csv` history,
then a live round-trip against a freshly-built `dugite-node` + real
`cardano-cli 11.0.0.0` on a throwaway preview-genesis devnet.

## Fixed this session (#1018, #1019)

- **#1018 — `GetFuturePParams` (tag 33) hardcoded to always `Nothing`.**
  Separate bug from #977 (which only fixed the EMBEDDED copy inside
  `GetGovState` tag 24 field [5]). Oracle-verified tag 33's wire shape is a
  **bare `Maybe (PParams era)`** (`queryFuturePParams`'s 3-way→Maybe
  collapse), NOT the 3-way sum tag 24 uses — copying that encoder would have
  been a NEW wrong-shape bug. `09-cli-parity`'s `future-pparams` row has
  reported `EQUAL` with an identical hash on every recorded run because the
  devnet has simply never had a pending update queued at sample time — the
  #977/#992 vacuous-agreement trap, recurring on the twin query.
- **#1019 — `EnactState.ensWithdrawals` hardcoded empty map.** Resets only
  once per epoch boundary (`mkEnactState`), accumulates via
  `Map.unionWith (<>)` across every `TreasuryWithdrawals` action accepted in
  that epoch's ratification pass, visible for the rest of the epoch. Same
  "one term left reading live" shape as #966 (`ensTreasury`) / #994
  (`ensCurPParams`). Fixed by deriving it from `enacted` (== frozen
  `rsEnacted`, already threaded into `encode_ratify_state`), converting each
  29-byte reward account to `Credential` (header bit `0x10`: `0xe*`=key,
  `0xf*`=script) and summing per-credential.
  **Trap for next time**: `cardano-cli conway query ratify-state
  --output-json`'s `ToJSON EnactState` does NOT render `ensTreasury`/
  `ensWithdrawals` at all — confirmed live, `nextEnactState` keys are only
  `committee/constitution/curPParams/prevPParams/prevGovActionIds`. A wrong
  VALUE in either field is invisible to a human reading cardano-cli's pretty
  output; only a wrong SHAPE breaks decode. Value-correctness needs a raw-CBOR
  client or a unit test, not eyeballing `cardano-cli` output.

## Filed, not fixed

- **#1020 — `NextEpochChange` hardcoded `NoChangeExpected`; `EnactState.ensCommittee` likely live not frozen.**
  Oracle gave the full `NextEpochChange` sum (`ToBeEnacted`/`ToBeRemoved`/
  `NoChangeExpected`/`ToBeExpired`/`TermAdjusted EpochNo`) and confirmed
  `GetCommitteeMembersState` (tag 27) is a genuine HYBRID: current members +
  hot-cred status = LIVE (dugite has this right); `NextEpochChange` alone
  needs a comparison against the FROZEN next-boundary committee
  (`finishDRepPulser`'s `ensCommittee`, i.e. dugite's #988 pulser). Deriving
  that "post-fold" committee needs replicating `UpdateCommittee`/
  `NoConfidence`'s effect on top of `gov.ratify_enacted` — a real feature,
  not a wiring fix.
- **#1023 (P1) — Conway cert decoder accepts MIR (tag 6) / GenesisKeyDelegation
  (tag 5), which real `cardano-ledger` REMOVED from `ConwayTxCert` entirely
  and hard-fails at CBOR decode** (`fail "MIR certificates are no longer
  supported"` / `"Genesis delegation certificates are no longer supported"`,
  `TxCert.hs:719-726`). Same for tx-body key 6 (old PPUP `update` field) —
  absent from Conway's `ConwayTxBodyRaw`, decode-fails via `invalidField 6`.
  dugite's `era_conway.rs::read_conway_certificate` still decodes both, and
  `dugite-ledger/validation/conway.rs`'s era-gate classifies them as valid in
  every post-Shelley era — the opposite of "removed entirely". Accept-where-
  Haskell-rejects, the #996/#997 class. Fix spans `dugite-serialization` +
  `dugite-ledger`, outside this audit's `dugite-node` remit — filed only.
- **#1025 — ~10 residual reachable `ValidationError` variants still degrade
  to generic `ScriptFailed`** in `serve.rs` (post-#979): `GovernancePreConway`,
  `MissingRedeemer`, `MissingDatumWitness`, `ExtraDatumWitness`,
  `ZeroWithdrawal`, `MalformedProposal`, `ScriptLockedCollateral`,
  `OutputBootAddrAttrsTooBig`, `InvalidRewardAccount`,
  `ZeroTreasuryWithdrawals`, `ProposalProcedureNetworkIdMismatch` (fallback
  arm only). Explicitly EXCLUDES all MIR/PPUP-related arms (8 of them) —
  those are decode-unreachable once #1023 lands, so don't need wire tags.
- **#1027 (P1, LIVE-VERIFIED BROKEN) — `cardano-cli query ledger-state`
  completely undecodable.** `GetDebugNewEpochState` (tag 12)/`GetDebugEpochState`
  (tag 8) both self-admit "simplified empty placeholder" for the nested
  `LedgerState`/`UTxOState`/`GovState`/`CertState` — literally
  `enc.array(0)` where `ConwayGovState` needs `array(7)`. Live reproduction:
  `cardano-cli conway query ledger-state` returns cardano-cli's raw-CBOR
  diagnostic fallback dump (exit 0, but NOT the documented JSON), proving the
  strict Haskell decoder rejects it. **Zero test coverage anywhere** —
  `09-cli-parity` never queries `ledger-state`; the only reference in
  `testnet/local-devnet/` (`two-forger-round.sh`) queries it against the
  HASKELL ARBITER ONLY, never dugite's own socket. This command has never
  been exercised against dugite by any existing check.

## Methodology notes worth repeating

- **`09-cli-parity`'s CSV history is a fast, free "has this ever been
  non-vacuously tested" oracle** — grep
  `testnet/local-devnet/evidence-archive/auto/*/cli-parity.csv` for a query
  name; identical hashes across many runs = vacuous, a changing hash = real
  content being compared. Distinguishes `future-pparams` (vacuous, hid a real
  bug) from `committee-state` (non-vacuous — but only for the fields that
  actually vary; a hardcoded sub-field like `NextEpochChange` can still hide
  inside an otherwise-changing hash).
  Also cheap: `grep -rn "<query-name>" testnet/local-devnet/tx-zoo/*/` to
  confirm a query is exercised AT ALL — `ledger-state` had zero hits.
- **A live round-trip via a throwaway single-node devnet is cheap and
  decisive** when you just need "does this decode": `dugite-node run` against
  a fresh `--database-path`/`--socket-path` in scratch, no sync needed (LSQ
  works at genesis). macOS `sun_path` is 104 bytes — use `/tmp/ld-*.sock`
  directly, NOT a path under the long scratchpad directory (`SUN_LEN` error).
  Bound with `timeout N` and an `until [ -S sock ]; do sleep 1; done` wait
  loop backgrounded via Bash `run_in_background` (not Monitor — Monitor is
  for recurring/unbounded events, not a single "ready" signal).
- **`cardano-cli`'s pretty-JSON output is not a complete oracle** — its
  `ToJSON` instances can silently omit fields that exist on the wire
  (`ensTreasury`/`ensWithdrawals` never render). A live round-trip proves
  SHAPE correctness (decode succeeds) but not VALUE correctness for fields
  the CLI doesn't print; that still needs a unit test against a realistic
  non-empty state.
