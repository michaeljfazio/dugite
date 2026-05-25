# Mithril Ancillary Import — Trust Model

The Mithril aggregator publishes two artefacts per snapshot:

1. **Main archive** — the certified ImmutableDB chunk files (`immutable/*.{chunk,primary,secondary}`).
2. **Ancillary archive** — the serialised Haskell `ExtLedgerState` at the immutable tip, plus the partial next-chunk trio (`ledger/<slot>/state`, `ledger/<slot>/tables`).

Dugite consumes both: the main archive populates `ImmutableDB`, and the ancillary archive lets the node skip the multi-hour chain-from-genesis replay normally needed to rebuild ledger state.

This document records the trust model, the operator-exposure decision, and the verification harness used to confirm byte-exactness.

## Why ancillary matters

Without ancillary, after `mithril-import` returns the node still has to replay every certified block through its own validator to rebuild:

- The UTxO set
- Pool, DRep, committee and proposal state
- Treasury, reserves, fees, deposits
- All five Praos nonces (`evolving`, `candidate`, `epoch`, `lab`, `last_epoch_block`)
- Operational-certificate counters per pool
- Stake distribution + mark/set/go snapshots
- Pending reward updates and MIR deltas

On mainnet that replay takes **multi-hour** today (typically 10+ hours on commodity hardware). Importing the ancillary directly drops cold-start to **~15 minutes** of decode + LSM bulk-load.

## Trust model

The ancillary archive is signed and certified by Mithril's threshold multi-signature scheme using the **same** ≥ 2/3 stake-weighted aggregator key that signs the immutable chunks. Concretely:

- Dugite downloads the `ancillary.tar.zst` from the aggregator's V2 `/artifact/cardano-database/{hash}/cardano-database-snapshots` endpoint.
- The manifest is a JSON map of `filename → SHA-256(content)` plus an Ed25519 signature over the sorted key+value pairs, signed by a network-specific ancillary verification key. Dugite verifies the signature before unpacking.
- Per-file SHA-256 hashes are checked against the manifest as files are extracted (`mithril.rs::verify_ancillary_manifest`).

The Ed25519 verification keys are pinned in source:

| Network | Pinned key location |
|---------|---------------------|
| Mainnet | `crates/dugite-node/src/mithril.rs:2135-2176` |
| Preview | `crates/dugite-node/src/mithril.rs:2135-2176` |
| Preprod | `crates/dugite-node/src/mithril.rs:2135-2176` |

The aggregator's full STM certificate chain is also verified (back to the genesis certificate) when `--skip-certificate-verification` is not set. This is identical to the trust posture used for the main snapshot.

**What we trust when we accept the ancillary**

We trust that **a Haskell cardano-node implementation honestly computed the ledger state at the certified immutable tip and published it.** We do not re-validate the state against our own from-genesis replay before using it.

This is a stronger trust assumption than the main snapshot alone: the main snapshot is a certified statement that "this chain is canonical and ≥ 2/3 of stake agrees"; the ancillary is a stronger statement that "the state derived by Haskell from this chain is also correct."

**What we do not trust**

We do not trust the ancillary's CBOR encoder to be implementation-independent: dugite's runtime types differ from Haskell's HFC-telescope / HKD-parameterised types, so the adapter (`LedgerState::from_haskell_snapshot`, `crates/dugite-ledger/src/state/mod.rs:697`) explicitly converts every field. Any new era will need adapter coverage before the ancillary path will work for that era.

## Operator-exposure decision

The flag `--include-ancillary` is **default-on**. Passing `--no-include-ancillary` skips the ancillary download and falls back to chain-from-genesis replay.

Rationale:

- Most end-users prefer fast bootstrap and accept the certified-ancillary trust posture (which is the same posture used for the main snapshot's stake-weighted certification).
- The pre-ancillary import path (chunk-only) historically caused issue #335 — stale genesis-default protocol parameters at the imported tip — because the node had no way to learn current PParams without replay. The ancillary path resolves this by providing live PParams directly.
- Operators who need byte-exact verification of dugite's own ledger derivation against the Haskell reference should run two imports — one with `--include-ancillary` and one with `--no-include-ancillary` (waiting for the replay to complete) — then diff the two `ledger-snapshot.bin` files via `dugite-node verify-ledger-snapshot`.

## Verification harness (issue #670 acceptance)

`dugite-node verify-ledger-snapshot --left <path> --right <path>` performs a semantic byte-exact comparison of two ledger snapshots. The harness reports any field-level mismatch with full diagnostic detail and exits non-zero on failure.

Each `<path>` may be either a `ledger-snapshot.bin` file or a database directory containing one.

### Acceptance procedure for a new era / boundary

1. **Build two databases:**

   ```bash
   dugite-node mithril-import \
     --network-magic 2 \
     --database-path ./db-preview-ancillary \
     --include-ancillary

   dugite-node mithril-import \
     --network-magic 2 \
     --database-path ./db-preview-replay \
     --no-include-ancillary
   ```

2. **Replay the no-ancillary database** by running `dugite-node run` against it until the ledger state catches up to the same anchor as the ancillary database. The replay is the slow path the ancillary is designed to avoid; allow multi-hour wall time.

3. **Compare:**

   ```bash
   dugite-node verify-ledger-snapshot \
     --left ./db-preview-ancillary \
     --right ./db-preview-replay
   ```

4. **Expected output on PASS:**

   ```text
   PASS — snapshots are semantically equal
   ```

5. **On FAIL,** the harness prints one line per differing field. Use those entries to locate the divergence — for example a `governance.proposals` mismatch points to the Conway governance decoder, a `snapshots.set` mismatch points to mark/set/go snapshot semantics, etc.

### Acceptance status

The harness must PASS for at least one preview boundary AND one preprod boundary before #670 is considered fully verified.

Prior to commits in 2026 Q2 (issues #438, #481, #624, #626, #678, #685) the from-genesis replay did not match the Haskell ancillary at all boundaries — the harness reported drift in pot fields (`treasury`, `reserves`, `epoch_fees`) cascading from a missing Babbage→Conway PPUP path that left the on-chain protocol version stuck at 8 in dugite while the canonical chain ran at 9. With those resolved, a preview-mainnet-style chunk replay through to the mithril anchor now reproduces the Haskell ledger byte-exact, including:

- Pots (treasury, reserves, fees, deposits, donation)
- All five Praos nonces
- DRep + committee + proposal + vote state
- Mark/set/go snapshots + ssFee
- bprev block production counters
- Stake distribution + per-credential deposits

Any remaining open epoch-diff issue is a blocker for re-claiming the gate; consult the project tracker before signing off a new release that touches era-translation, governance enactment, or PPUP semantics.

## Code references

| Concern | File:line |
|---------|-----------|
| `--include-ancillary` CLI flag | `crates/dugite-node/src/main.rs:307` |
| `--no-include-ancillary` alias | `crates/dugite-node/src/main.rs:325` |
| Ancillary download + verify | `crates/dugite-node/src/mithril.rs:2338` (`download_ancillary`) |
| Manifest signature verification | `crates/dugite-node/src/mithril.rs:2203` (`verify_ancillary_manifest`) |
| Per-network ancillary verification keys | `crates/dugite-node/src/mithril.rs:2135` |
| Haskell snapshot decoder | `crates/dugite-serialization/src/haskell_snapshot/` |
| Adapter (Haskell → dugite types) | `crates/dugite-ledger/src/state/mod.rs:697` (`from_haskell_snapshot`) |
| Node startup integration | `crates/dugite-node/src/node/mod.rs:781` |
| `verify-ledger-snapshot` subcommand | `crates/dugite-node/src/main.rs` (search `VerifyLedgerSnapshot`) |
| Comparison harness module | `crates/dugite-node/src/verify_snapshot.rs` |

## Related issues

- **#670** — This issue. Adds the explicit CLI flag, documents the trust model and operator-exposure decision, and ships the byte-exact verification harness.
- **#335** — Stale genesis-default protocol parameters when ancillary was skipped (root cause for making ancillary default-on).
- **#626** — Residual +297K-ADA drift at preview boundary 3→4 (resolved; PPUP timing aligned with Haskell HFC tick).
- **#624** — Pre-Conway PPUP decoder fix that closed earlier boundary drifts.
- **#678** — Conway treasury-value check incorrectly gated; resolved by mode-gating on `ValidateAll` to match Haskell `ApplySTSOpts.asoValidation`.
- **#685** — Missing PPUP application at Babbage→Conway era boundary + `prev_pp` captured AFTER `ratify_proposals_impl`. Both fixed; replay now byte-exact through the originally-failing preview slot 76172461 and past.
- **#516** — Single-use channel constraint workaround (unrelated to ancillary but referenced from the same lifecycle code).
