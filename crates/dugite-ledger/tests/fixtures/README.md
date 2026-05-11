# Mithril ledger-state fixtures

Test fixtures consumed by the `#[ignore]`d preview-epoch comparison tests in
`crates/dugite-ledger/src/state/tests.rs`:

- `test_preview_epoch_1268_golden_vector_matches_ledger_state`
- `test_preview_epoch_1268_mark_set_go_pool_stake_distinct`
- `test_preview_epoch_1268_pool_stake_haskell_vs_dugite`
- `test_preview_multi_epoch_rotation_matches_ledger_states`

And the integration test in `crates/dugite-node/src/mithril.rs`:

- `test_verify_preview_certificate_chain` — network-only, no fixture needed;
  run with `cargo nextest run -p dugite-node -E 'test(verify_preview_certificate_chain)' -- --ignored`.

## Why these aren't committed

Each fixture is a Haskell `ExtLedgerState` CBOR dump — multi-GB per epoch.
They live outside the repo and are captured per-machine from a synced
cardano-node.

## Capture procedure

1. **Run cardano-node** (Haskell, ≥ 10.6.x) against Preview testnet until it
   syncs past every target epoch (1264–1268 for the multi-epoch test; just
   1268 for the single-epoch tests).

2. **Locate the on-disk ledger snapshots.** cardano-node writes snapshots at
   `<db-dir>/ledger/<slot>` as CBOR-encoded `ExtLedgerState` files. Snapshot
   cadence is governed by `--snapshot-interval`; for repeatable capture set
   it to a value that brackets your target epochs.

3. **Pick the snapshot whose slot lies at the start of each target epoch.**
   Preview epoch length is 86_400 slots; epoch start slot N is
   `(N − shelley_transition_epoch) × 86_400 + shelley_start_slot`.

4. **Copy and rename** the chosen snapshot files into this directory as
   `preview_ledger_e{epoch}.cbor`:

   ```bash
   cp <db-dir>/ledger/<slot>  preview_ledger_e1268.cbor
   # ...for each target epoch
   ```

5. **Verify decoder compatibility.** A quick smoke check:

   ```bash
   cargo nextest run -p dugite-ledger -E \
     'test(test_preview_epoch_1268_golden_vector_matches_ledger_state)' \
     -- --ignored
   ```

   If the decoder errors with an unexpected CBOR tag, the cardano-node version
   may have introduced a new field. Bump `dugite-serialization`'s
   `haskell_snapshot` decoder and re-run.

6. **Remove the `#[ignore]` attribute** on each test you want to gate on the
   fixture being present. Leave the gate in place if you'd rather only run
   the comparison opportunistically.

## What the tests assert

Each test loads the Haskell snapshot via
`dugite_serialization::haskell_snapshot::decode_state_file`, converts it via
`LedgerState::from_haskell_snapshot`, then compares per-pool stake values
against a committed Koios golden JSON (`tests/golden/preview-epoch-*.json`)
with 1.0% lovelace tolerance. Drift inside that band is expected from the
wall-clock gap between Koios capture and the on-disk snapshot.

## Cleanup

This directory is gitignored from the repo (see workspace `.gitignore`); the
README is the only file checked in. Drop captured fixtures here freely — they
won't pollute commits.
