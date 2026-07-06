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
| `pp_future`       | nullable                                | `null` when nothing is queued for the next boundary; Haskell-side derivation is partial (#807, see below). |

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

### cn 11.0.1 emission-shape limitation (issue #612)

`cardano-cli debug log-epoch-state` in cardano-node 11.0.1 emits only
the **`currentEpochState`** subset of `NewEpochState`, plus a small
sidecar (`rewardUpdate`, `currentEpochBlocks`, `currentStakeDistribution`,
`priorBlocks`, `currentEpoch`).  The top level looks like:

```
{
  "currentEpoch": N,
  "currentEpochBlocks": { ... },
  "currentEpochState": {
    "esChainAccountState": { "reserves", "treasury" },
    "esLState": {
      "delegationState": { "dstate": {...}, "pstate": {...} },
      "utxoState": { "deposited", "fees", "ppups", "stake", "utxo" }
    },
    "esNonMyopic": {...},
    "esSnapshots": { "pstakeMark", "pstakeSet", "pstakeGo", "feeSS" }
  },
  "currentStakeDistribution": {...},
  "priorBlocks": {...},
  "rewardUpdate": { "deltaT", "deltaR", "deltaF", "rs", "nonMyopic" }
}
```

There is **no** `chainDepState` (Praos nonce), **no** `utxosGovState`
(Conway governance), **no** `era` label, and `utxoState.ppups`
contains Haskell-shape `PParams` that don't line up field-for-field
with dugite's `ProtocolParameters` serde.

The normalizer projects this subset into canonical form and emits
`null` for any canonical leaf cn cannot supply.  These nulls are
matched against `HASKELL_UNCOVERABLE` in the diff tool and reported
under a separate "Haskell-uncoverable fields" section, **excluded
from the real-divergence count**.

#### Canonical fields the cn 11.0.1 dump cannot supply

| Canonical path                       | Reason                                                                       |
| ------------------------------------ | ---------------------------------------------------------------------------- |
| `nonce.eta_v`                        | Lives on `chainDepState.csProtocol.prtclState.evolvingNonce`, not in dump.   |
| `nonce.eta_c`                        | `chainDepState.csProtocol.prtclState.candidateNonce`, not in dump.           |
| `nonce.eta_h`                        | `chainDepState.csTickn.ticknStateEpochNonce`, not in dump.                   |
| `nonce.eta_lj`                       | `chainDepState.csTickn.ticknStateLastEpochBlockNonce`, not in dump.          |
| `governance.*` (all subfields)       | cn dump still emits pre-Conway `ppups`, never `utxosGovState`.               |
| `pp_current`, `pp_previous`           | cn emits camelCase Haskell `PParams`; no field-by-field mapping to dugite's `ProtocolParameters` serde. |
| `era`                                | Not in cn dump (only `protocol_version.major` is reachable via `ppups.curPParams`). |
| `scalars.deposits_drep`              | Conway-only; cn dump has no DRep deposit field.                              |
| `scalars.deposits_proposal`          | Conway-only; cn dump has no proposal-deposit field.                          |
| `utxo.asset_count`                   | Needs a full UTxO walk; cn dump only stages an empty `utxo: {}` at this point. |

#### Fields the normalizer **can** map (and how)

| Canonical path                        | cn dump source                                                            |
| ------------------------------------- | ------------------------------------------------------------------------- |
| `epoch`                               | `currentEpoch`                                                            |
| `scalars.reserves` / `scalars.treasury` | `currentEpochState.esChainAccountState.{reserves,treasury}`              |
| `scalars.fees`                        | `currentEpochState.esLState.utxoState.fees`                              |
| `scalars.deposits_stake`              | `currentEpochState.esLState.utxoState.deposited`                          |
| `protocol_version.{major,minor}`      | `currentEpochState.esLState.utxoState.ppups.curPParams.protocolVersion.*` |
| `utxo.count`                          | `len(currentEpochState.esLState.utxoState.utxo)`                          |
| `stake_snapshot.{mark,set,go}.total_active_stake` | Sum `swdStake` across `esSnapshots.pstake*.activeStake` entries |
| `stake_snapshot.{mark,set,go}.pool_count`         | `len(esSnapshots.pstake*.stakePoolsSnapShot)`                    |
| `pools.registered`                    | `len(delegationState.pstate.stakePools)`                                  |
| `pools.retiring`                      | `len(delegationState.pstate.retiring)`                                    |
| `rewards.total_distributed`           | Sum `rewardAmount` across `rewardUpdate.rs[*][*].rewardAmount`            |
| `pp_future` (partial, #807)           | Merges `ppups.proposals` + `ppups.futureProposals` — each an ARRAY of `[genesisKeyHashHex, PParamsUpdate]` pairs (`ProposedPPUpdates`'s `ToJSON` does `Map.toList` first, NOT an object keyed by hash), field names hand-written in `ShelleyGovState`'s `ToKeyValuePairs` instance. Translates the `PParamsUpdate` fields dugite's legacy `ProtocolParamUpdate` understands (`_PP_UPDATE_FIELD_MAP` — data-driven `ppName` JSON keys, e.g. `stakePoolTargetNum` not `nOpt`) into canonical snake_case names, and returns `None` when nothing is queued. On an actual Conway (cn 11.0.1) dump `ppups.proposals` is a structurally different CIP-1694 `GovActionState` list and there is no `futureProposals` key at all; the array-of-pairs shape check doubles as the era discriminator so this safely yields `None` rather than misparsing governance-action data. This is a **partial** dict — only the overridden fields — not a full `ProtocolParameters` clone, since `pp_current`/`pp_previous` still lack a full renamer to merge onto. Good enough to catch premature/delayed PPUP enactment timing; every `pp_future.*` diff stays `severity=info` regardless (see `diff-epoch-dumps.py`). |

### Other normalizer notes

- **`governance.committee_hash`** — *Synthetic*.  Even when both
  sides emit it, the dugite-side value is a SHA3-256 over a
  canonical input list, not a Blake2b over CBOR `Committee` (which
  dugite does not materialise at dump time).  The diff tool already
  demotes this field to `info` severity by default.

### Fields with **no dugite equivalent**
- (none currently — extend the dugite dumper or the normalizer's
  `HASKELL_UNCOVERABLE` list when adding new canonical fields.)

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
