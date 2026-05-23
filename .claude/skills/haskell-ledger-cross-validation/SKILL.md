---
name: haskell-ledger-cross-validation
description: Cross-validate dugite-node's ledger-state calculations byte-exactly against the Haskell cardano-node 11.0.1+ reference implementation, per-epoch, via paired ledger-state dumps and the canonical IntersectMBO source. Use when investigating any divergence in reserves/treasury/fees/deposits/rewards/snapshots/pool-state/governance vs Haskell; before changing any RUPD/reward/snapshot/era-transition formula in dugite-ledger; when triaging GitHub issues tagged "epoch-diff" or "#615-class"; when a "residual drift" remains after a ledger fix; or when about to commit any ledger fix that changes per-epoch state. NOT for tx-level Phase-1/Phase-2 validation drift (use devnet-validate) and NOT for consensus header divergence.
---

# Haskell Ledger Cross-Validation

Authoritative procedure for proving that a dugite ledger-state calculation matches the Haskell `cardano-node` reference, boundary-by-boundary, with byte-exact equality on every covered field.

## Hard precedence rules

Always honor this order. Skipping a rung wastes hours.

1. **Paired dumps over guesses.** Diff dugite's `epoch_NNNNNN.json` against `cardano-cli debug log-epoch-state` output for the same epoch on the same network. Never reason "I think Haskell does X."
2. **`cardano-haskell-oracle` over `cardano-ledger-oracle`.** The haskell-oracle pulls live GitHub source from IntersectMBO/cardano-ledger; prefer it for any post-2025 PR. Drop to ledger-oracle only when the haskell-oracle returns nothing.
3. **Spec cross-check is non-optional.** Even when the Haskell source is clear, quote the relevant spec section in the PR/issue. Implementation can drift from spec; both must agree.
4. **cardano-node dump over Koios.** Koios is a sanity check, not ground truth. The `cardano-cli debug log-epoch-state` JSONL from a fully-synced cn 11.0.1+ node is authoritative.
5. **Byte-exact or it isn't fixed.** "Close" is wrong. A 1-lovelace diff in reserves cascades into millions of lovelace of reward drift over hundreds of epochs.

## Decision flow

```
divergence reported?
├── regenerate dugite dump with HEAD code  (STALE dumps mislead bisection)
├── still diverges?
│   ├── YES — locate the field in references/dump-schema.md
│   │         ├── is it in HASKELL_UNCOVERABLE?  → not a real bug, log + skip
│   │         └── otherwise → continue
│   └── NO  — STOP. file was a time capsule. close the issue if filed.
├── consult cardano-haskell-oracle for the canonical Haskell calc
├── cross-check the Shelley/Babbage/Conway spec PDF
├── implement fix in dugite-ledger
├── re-sync dugite from genesis (NOT incremental; era-transition timing matters)
├── re-diff against the SAME Haskell dump (don't re-capture unless cn version changed)
└── confirm byte-exact across at least 3 successive boundaries before declaring fixed
```

## The procedure

### Step 1 — start paired nodes

Both nodes from genesis on the same network. Past investigations have shown that **era-transition timing** (Alonzo→Babbage→Conway) interacts with snapshot/RUPD capture — incremental syncs hide bugs.

```bash
# dugite side
cargo build --release -p dugite-node --features dugite-ledger/epoch-state-debug

DUGITE_EPOCH_STATE_DUMP=./epoch-dumps-dugite \
DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS=1 \
./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-dugite-preview \
  --socket-path ./node-dugite.sock

# haskell side (cn 11.0.1+ required for preview/preprod at PV11+)
cardano-node run \
  --config /path/to/preview-config.json \
  --topology /path/to/preview-topology.json \
  --database-path ./db-haskell-preview \
  --socket-path ./node-haskell.sock

# capture Haskell dumps in parallel
scripts/validation/capture-haskell-epoch-dumps.sh \
  --socket ./node-haskell.sock \
  --out-dir ./epoch-dumps-haskell
```

Gotchas (do not re-discover):
- `cardano-cli debug log-epoch-state` takes `--node-configuration-file`, **not** `--testnet-magic`.
- It emits **per-block**; the splitter dedupes to last-per-epoch. Run for a full epoch's worth of blocks past your target.
- Byron-era epochs are unsupported by `log-epoch-state`; `scripts/validation/capture-haskell-retry.sh` papers over the retry storm.
- Preview is at PV11+ — Haskell node must be 11.0.1+ or it will die with "Version number 12 and higher not supported".

### Step 2 — diff

```bash
scripts/validation/diff-epoch-dumps.py \
  --haskell-dir ./epoch-dumps-haskell \
  --dugite-dir  ./epoch-dumps-dugite \
  --from-epoch 1 --to-epoch 50 \
  --report-md  ./diff-report.md
```

The report buckets divergences by severity: `rewards`, `governance`, `stake`, `utxo`, `nonce`, `pp`, `era`. Any non-zero `rewards` bucket is a P0 — it cascades through every subsequent epoch.

### Step 3 — investigate divergences

For each real divergence (i.e. not in `HASKELL_UNCOVERABLE`), in order:

1. **Locate the canonical formula.** Use the `cardano-haskell-oracle` Agent with a specific query: which module, which function, what inputs. Example: "Where is `applyRUpd` defined in cardano-ledger, and which deltas come from `RewardUpdate` vs `frTotalUnregistered`?"
2. **Quote the source.** Paste the relevant Haskell snippet into the issue / PR description with the GitHub permalink.
3. **Identify the dugite mismatch.** Compare line-by-line. Don't paraphrase; mirror the Haskell shape.
4. **Cross-check the spec.** Cite the section number (e.g., Shelley spec §11 for RUPD, CIP-1694 §X.Y for governance).
5. **Fix.** Edit dugite-ledger to match Haskell semantics literally. Avoid clever rewrites.
6. **Re-sync from genesis** with the fix. Old `epoch-dumps-dugite/` must be deleted first.
7. **Re-diff.** Confirm the boundary you targeted is now byte-exact, AND that no other boundary regressed.

### Step 4 — declare fixed

Required before closing any cross-validation issue:

- Byte-exact match on the divergent field across at least 3 consecutive boundaries.
- No new divergences introduced anywhere else.
- Spec citation + Haskell source permalink in the commit body.
- Cross-validation re-run on **fresh from-genesis dumps** generated with the fix in HEAD.

Tests passing is **not sufficient evidence.** Past mistakes (see `references/gotchas.md`) have rewritten tests to match a wrong implementation. The dump diff is the only source of truth.

## When to invoke this skill

- Any change in `crates/dugite-ledger/src/state/` touching reserves, treasury, fees, deposits, rewards, RUPD, snapshots, or pool state.
- Any change in `crates/dugite-ledger/src/eras/{shelley,babbage,conway}.rs` touching era-transition or `on_epoch_boundary` / `on_era_transition`.
- Any new GitHub issue describing "divergence vs cardano-node" or "epoch-diff" or "reserves drift" or "reward drift".
- Before merging a PR that modifies any of the above, even if all tests pass.

## When NOT to invoke

- Tx-level Phase-1/Phase-2 validation drift → use `devnet-validate` instead.
- Consensus header / VRF / KES divergence → consult `cardano-haskell-oracle` directly; epoch-dump diff won't surface header bugs.
- UTxO-set-only investigations → `utxo.count` is in `HASKELL_UNCOVERABLE` (cn dump stages empty `utxo: {}`); use a different validation path.
- Mainnet investigations — this skill targets preview/preprod where from-genesis is feasible. For mainnet, you must work from a synced db snapshot, not from-genesis.

## References

- **`scripts/validation/EPOCH_DIFF.md`** (in the repo) — schema docs, normalizer field map, severity classes. Read this when you need to understand the canonical schema or debug the normalizer.
- **`references/gotchas.md`** — accumulated pitfalls from past investigations (#438 saga, #615a-i, #481, #485-D2). Read before starting any investigation to avoid re-discovering known traps.
- **`references/source-precedence.md`** — exact query patterns for `cardano-haskell-oracle` / `cardano-ledger-oracle`, and how to cite their output in PRs.
- **`references/dump-schema.md`** — canonical schema fields, `HASKELL_UNCOVERABLE` list, mapping from cn 11.0.1 emission shape.

## Anti-patterns (do not do these)

- **Do not** trust subagent ledger fixes without byte-exact dump verification. See #438 saga: a subagent "fix" inverted the semantics and tests were rewritten to match.
- **Do not** investigate residuals against old `reward-dumps-*` directories — they are time capsules from prior code. Always regenerate with HEAD before bisecting.
- **Do not** add `+ undistributed` / `-saturating_sub` patches without checking whether the corresponding flow in Haskell routes to `deltaR` vs `deltaT` vs `deltaF`. See #485-D2.
- **Do not** declare "matches Haskell" based on Koios alone. Koios's reward fields don't expose `frTotalUnregistered` routing or RUPD intermediate state.
- **Do not** assume the era-transition handler runs *after* `process_epoch_transition` — in dugite it fires *before*, which has bitten capture timing for `prev_d`, `prev_protocol_version_major`, `bprev_blocks_by_pool`. Always check ordering in `apply.rs`.
