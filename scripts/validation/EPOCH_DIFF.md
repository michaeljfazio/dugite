# Per-Epoch Ledger-State Diff Harness

End-to-end harness for cross-validating dugite-node against
cardano-haskell-node on preview / preprod testnets by emitting a
canonical per-epoch ledger-state JSON dump from each implementation
and diffing them field-by-field.

Tracks tasks #21 (dump), #22 (Haskell capture), #23 (diff).

---

## What gets dumped

One JSON file per epoch boundary, written immediately after the
boundary handler completes.  Both sides emit the **same canonical
schema** so the diff tool can compare them directly.

```
<dir>/epoch_<NNNNNN>.json
```

Schema (top level):

| Field             | Type                                    | Notes                                            |
| ----------------- | --------------------------------------- | ------------------------------------------------ |
| `epoch`           | u64                                     | Epoch label of the just-entered epoch.           |
| `slot`            | u64                                     | Slot the boundary fired at.                     |
| `era`             | string                                  | One of byron/shelley/.../dijkstra.              |
| `protocol_version`| `{major, minor}`                        | Current pparams.                                |
| `scalars`         | reserves/treasury/fees/deposits         | All u64 lovelace.                               |
| `nonce`           | `{eta_v, eta_c, eta_h, eta_lj}` (hex32) | Praos nonce state.                              |
| `utxo`            | `{count, total_lovelace, asset_count}`  | `asset_count` skippable via env var.            |
| `stake_snapshot`  | mark/set/go totals                      | Per snapshot: `{total_active_stake, pool_count}`|
| `pools`           | registered/retiring/retired_this_epoch  |                                                  |
| `rewards`         | `{total_distributed, per_pool_top20}`   | top20 sorted desc by amount then asc by id.     |
| `governance`      | drep + cc + proposals                   | See below.                                       |
| `pp_current`      | full `ProtocolParameters` serde         |                                                  |
| `pp_previous`     | full `ProtocolParameters` serde         |                                                  |
| `pp_future`       | nullable                                | Currently always null (see code note).          |

Governance sub-schema:

```jsonc
{
  "drep_count": 0,
  "drep_total_voting_power": 0,
  "drep_top20": [{ "drep_id_hex": "...", "voting_power": 0, "deposit": 0 }],
  "cc_members": [{ "hot_key_hex": "...", "cold_key_hex": "...", "expiry_epoch": 0 }],
  "cc_threshold_num": 0, "cc_threshold_den": 1,
  "active_proposals": 0,
  "active_proposal_ids": ["<txid>#<ix>"],
  "enacted_this_epoch": [{ "id": "<txid>#<ix>", "action_type": "parameter_change" }],
  "expired_this_epoch": ["<txid>#<ix>"],
  "constitution_anchor_hash": "<hex32>",
  "committee_hash": "<hex32>"  // synthetic — see caveat below
}
```

---

## How to run

### One-shot

```bash
scripts/validation/epoch-diff-driver.sh \
  --network preview \
  --haskell-dir ./epoch-dumps-haskell \
  --dugite-dir ./epoch-dumps-dugite \
  --from-epoch 1 --to-epoch 5
```

The driver only **documents** the run order; it does not start nodes
itself.  You start each node with the appropriate dump enabled, wait
for them to cross the target epochs, then let the driver invoke the
diff tool.

### Dugite-side

```bash
cargo build --release -p dugite-node --features dugite-ledger/epoch-state-debug

DUGITE_EPOCH_STATE_DUMP=./epoch-dumps-dugite \
DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS=1 \   # optional, skips asset enumeration
./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node-dugite.sock
```

### Haskell-side

```bash
scripts/validation/capture-haskell-epoch-dumps.sh \
  --socket ./node-haskell.sock \
  --out-dir ./epoch-dumps-haskell
```

`capture-haskell-epoch-dumps.sh` runs `cardano-cli debug
log-epoch-state` in the background and pipes the JSONL stream
through `scripts/validation/split-haskell-jsonl.py`, which splits
each record into its own `epoch_NNNNNN.json` file.

### Normalize + diff

```bash
scripts/validation/diff-epoch-dumps.py \
  --haskell-dir ./epoch-dumps-haskell \
  --dugite-dir  ./epoch-dumps-dugite \
  --from-epoch 1 --to-epoch 5 \
  --report-md  ./epoch-diff-report.md \
  --report-json ./epoch-diff-report.json
```

Use `--tolerance-config tolerance.yaml` to relax specific fields
(e.g. allow `utxo.asset_count` to differ by ±5).

---

## Field-mapping caveats

The Haskell `cardano-cli debug log-epoch-state` output exposes a
nested record; the normalizer flattens it to the canonical schema.
Maintained in `scripts/validation/normalize-epoch-dump.py`.

Known mismatches that the **normalizer must paper over**:

- **`nonce.eta_lj`** — Haskell field name is `lastEpochBlockNonce` /
  `nesPd._labNonce` depending on era; dugite calls it `lab_nonce`.
- **`scalars.deposits_drep`** — Haskell totals the DRep deposit from
  `vsDReps.drepDeposit`; dugite sums `governance.dreps[*].deposit`.
- **`scalars.deposits_proposal`** — Haskell uses `proposalsDeposits`;
  dugite sums `proposals[*].procedure.deposit`.
- **`governance.committee_hash`** — *Synthetic*.  Haskell's actual
  `committeeHash` is the Blake2b of the canonical CBOR of the
  `Committee` value, which dugite does not currently materialise at
  dump time.  Both sides therefore emit a SHA3-256 over the same
  canonical input list (sorted cold-key + hot-key + expiry triples +
  threshold).  Structural equality holds — i.e. if both sides agree
  on membership and threshold, the hashes match.  Use the
  tolerance config to whitelist this field if you only want bit-exact
  comparisons elsewhere.
- **`utxo.asset_count`** — counts distinct `(policy_id, asset_name)`
  occurrences across UTxOs.  Haskell exposes the same total via
  `utxoStateUtxo` enumeration; the normalizer must walk the same
  way.  Skippable via `DUGITE_EPOCH_STATE_DUMP_SKIP_ASSETS=1`.

Fields with **no Haskell equivalent** (set to a sentinel by the
normalizer, ignored by the diff tool):
- `pp_future` — dugite emits this best-effort; Haskell only surfaces
  the *queued* `ProtocolParamUpdate` rather than a full materialised
  `ProtocolParameters`.

Fields with **no dugite equivalent**:
- (none currently — extend the dugite dumper or the normalizer's
  "ignored" list when adding new canonical fields.)

---

## Severity classes (used by the diff tool)

- `rewards`   — differences in reward distribution or total credits.
- `governance` — DReps, CC, proposals, constitution.
- `stake`     — snapshot totals, pool counts.
- `utxo`      — UTxO count, total lovelace, asset count.
- `nonce`     — Praos nonce state.
- `pp`        — protocol parameters.
- `era`       — era / protocol version.

Each diff record carries `severity` so the markdown report can
bucket them.  The report's bucket counts feed CI thresholds — once
the harness is wired to GitHub Actions, a non-zero `rewards`
bucket should fail the build.

---

## Smoke testing the harness

The fixture-based unit test under
`scripts/validation/tests/test_diff_smoke.py` runs the diff tool over
two synthetic 1-epoch dumps with exactly one known divergence and
asserts the divergence is flagged.  Run with:

```bash
python3 -m pytest scripts/validation/tests/
```

The dugite-side Rust tests are gated on the `epoch-state-debug`
feature:

```bash
cargo test -p dugite-ledger --features epoch-state-debug epoch_state_debug
```
