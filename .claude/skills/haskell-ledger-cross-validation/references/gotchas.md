# Cross-Validation Gotchas — Accumulated Pitfalls

Hard-learned from past investigations. Read before starting any new cross-validation work.

## Time-capsule dumps

**Lesson from #481:** A 4.2B-lovelace "residual" was investigated for hours against `reward-dumps-issue-438/` (epoch 1276) generated days earlier. The fix had landed in `19e05c82a` 9 hours before the issue was filed. Re-running the dump with HEAD code: 1276/1276 boundaries byte-exact.

**Rule:** Before investigating ANY residual, delete `epoch-dumps-dugite/` and re-sync from genesis on the current HEAD. Old dump files are time capsules and will mislead bisection.

## Subagent ledger fixes

**Lesson from #438:** A subagent's "follow-on fix" (commit `2a14be2fe`) conflated `undistributed` with `frTotalUnregistered`, double-counted the deduction, and broke `pool_reward` to 44M vs target 352M. Tests passed because the subagent rewrote them to match the wrong implementation. The fix was reverted in `c73dfb0b3`+`69bfcd60e`+`b151cd911`.

**Rule:** Never trust subagent ledger fixes purely on test-pass status. Verify byte-exact via Koios replay or cn-dump diff before merging.

## `+ undistributed` to treasury (the #485-D2 trap)

dugite previously routed pool-formula `undistributed` into `delta_treasury`. Per `Cardano.Ledger.Shelley.Rules.Rupd.PulsingReward.hs::completeStep` and `IncrementalStake.hs::applyRUpdFiltered`:

- Pool-formula `undistributed` (the "leftover" from `rewardPotForPool` after distributing `rs`) refunds to **RESERVES** via `RewardUpdate.deltaR`, NOT to treasury.
- Only `frTotalUnregistered` (rewards owed to accounts whose stake credential was de-registered before the boundary) routes to treasury — and that already lands at apply-time.

Fix in #615b removed `+ undistributed` from `delta_treasury` at all 4 sites (general path + 3 early-return branches).

**Rule:** Before adding any `+ undistributed` or `saturating_sub` to a reward calc, query the haskell-oracle: "Where does X route in `applyRUpd`?" — and prove it routes the way you think.

## Era-transition handler timing

In dugite, `on_era_transition` fires **before** `process_epoch_transition`. This has bitten:
- `prev_d` capture (era transition zeroes it per d=0 in PV≥7)
- `prev_protocol_version_major` (era transition writes the new PV before snapshot read)
- `bprev_blocks_by_pool` (same boundary visibility)

Suspected to be the root cause of #615i (RUPD skipped at preview boundary 3→4) — same Alonzo→Babbage boundary that fires era-transition before transition.

**Rule:** Any field whose value depends on "the state *before* this boundary" must be captured in `apply.rs` **before** `on_era_transition` fires, then passed through. Don't read from `epochs.protocol_params` inside `process_epoch_transition` if you need the pre-transition value.

## cardano-cli `log-epoch-state` flags

- Takes `--node-configuration-file <path>`, **not** `--testnet-magic <N>`.
- Emits per-block (one JSON object per block applied), not per-epoch. The splitter (`split-haskell-jsonl.py`) dedupes by `currentEpoch`.
- Byron-era blocks are unsupported — `log-epoch-state` errors out. Use `scripts/validation/capture-haskell-retry.sh` which silently retries past the Byron prefix.

## dugite-cli `genesis-hash` bug for Shelley+

`dugite-cli genesis-hash` returns the wrong value for Shelley/Allegra/Mary/Alonzo/Babbage/Conway genesis files (#606, fixed). For Byron specifically, use:

```
cardano-cli byron genesis print-genesis-hash --genesis-json <path>
```

NOT `cardano-cli latest genesis hash` (returns the wrong serialization for Byron).

## cn 11.0.1 emission limitations (#612 schema realignment)

The cn 11.0.1 `log-epoch-state` dump emits only the `currentEpochState` subset. The following canonical fields are `HASKELL_UNCOVERABLE` and the diff tool excludes them from the real-divergence count:

| Field | Why |
|---|---|
| `nonce.eta_v`, `eta_c`, `eta_h`, `eta_lj` | Lives on `chainDepState`, not in dump |
| `governance.*` (all) | cn dump still emits pre-Conway `ppups`, never `utxosGovState` |
| `pp_current`, `pp_previous`, `pp_future` | cn emits camelCase Haskell `PParams`; no field-by-field map to dugite serde |
| `era` | Not in cn dump (`protocol_version.major` is reachable via `ppups.curPParams`) |
| `scalars.deposits_drep`, `scalars.deposits_proposal` | Conway-only, cn dump has no DRep / proposal-deposit field |
| `utxo.asset_count` | Needs full UTxO walk; cn dump stages empty `utxo: {}` |
| `utxo.count` | Same — cn stages empty utxo at this point |

Do **not** chase these divergences. They are normalizer gaps, not dugite bugs.

## Multi-node port conflicts

When running dugite + cardano-node side by side, always assign distinct N2N, N2C socket, and metrics ports. Default metrics port is **12798** (matches cardano-node) — collision will make one of them silently rebind or die.

## macOS App Nap on long-running paired syncs

macOS will freeze background processes via App Nap, breaking long paired syncs. Wrap both nodes in `caffeinate -dimsu`:

```bash
caffeinate -dimsu ./target/release/dugite-node run ...
caffeinate -dimsu cardano-node run ...
```

## Stale binary masquerading as "unfixed"

After making a fix, **rebuild before declaring it didn't work.** Check binary mtime against fix commit time. (Previously made #501 look unfixed when commit `7bda93225` had already addressed it.)

```bash
ls -la target/release/dugite-node
git log -1 --format="%H %ai" HEAD
```

If binary mtime < commit time, `cargo build --release` and re-test.

## "Boundary-skipping" RUPD bugs

If a single boundary delta is suspiciously small (e.g., 41 lovelace where 9T expected) and the next boundary is suspiciously large (catching up the missed work), the RUPD likely **didn't fire** at that boundary. Common causes:

- `rupd_ready` gate logic excludes the boundary (e.g., the #615i bug at preview 3→4).
- Era transition disrupted snapshot/`prev_*` capture.
- `last_applied_rupd` field not exposed to dumper, making diff look like 0 when it's actually present.

Don't assume "the math is wrong" before confirming "the rule actually fired".
