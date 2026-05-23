# Canonical Dump Schema (Pointer)

The full schema, severity classes, and normalizer field map live in
**`scripts/validation/EPOCH_DIFF.md`** in the repo. That file is the source of truth — this reference exists to surface the headline fields and route the reader.

## Top-level shape

```jsonc
{
  "epoch":           0,                    // u64; epoch just entered
  "slot":            0,                    // u64; boundary slot
  "era":             "shelley",            // byron/shelley/.../conway/dijkstra
  "protocol_version":{ "major": 0, "minor": 0 },
  "scalars":         { "reserves", "treasury", "fees", "deposits_stake",
                       "deposits_drep", "deposits_proposal" },  // all u64 lovelace
  "nonce":           { "eta_v", "eta_c", "eta_h", "eta_lj" },   // hex32
  "utxo":            { "count", "total_lovelace", "asset_count" },
  "stake_snapshot":  { "mark": {…}, "set": {…}, "go": {…} },    // per: total_active_stake, pool_count
  "pools":           { "registered", "retiring", "retired_this_epoch" },
  "rewards":         { "total_distributed", "per_pool_top20" },
  "governance":      { … see EPOCH_DIFF.md … },
  "pp_current":      { … full ProtocolParameters … },
  "pp_previous":     { … },
  "pp_future":       null
}
```

## What's coverable vs not (cn 11.0.1)

**Coverable** — diff these:
- `scalars.reserves`, `scalars.treasury`, `scalars.fees`, `scalars.deposits_stake`
- `protocol_version.major` (via `ppups.curPParams.protocolVersion.major`)
- `stake_snapshot.{mark,set,go}.total_active_stake` and `.pool_count`
- `pools.registered`, `pools.retiring`
- `rewards.total_distributed`

**Not coverable** (`HASKELL_UNCOVERABLE` — excluded from divergence count):
- `nonce.*` (lives on `chainDepState`, not in dump)
- `governance.*` (cn 11.0.1 still emits pre-Conway `ppups`)
- `pp_current` / `pp_previous` / `pp_future` (camelCase Haskell `PParams`, no serde map)
- `era` (not in cn dump)
- `scalars.deposits_drep`, `scalars.deposits_proposal` (Conway-only fields cn doesn't expose)
- `utxo.count`, `utxo.asset_count`, `utxo.total_lovelace` (cn stages empty `utxo: {}` at this point)

## Severity buckets

| Class | Examples | Triage urgency |
|---|---|---|
| `rewards` | total_distributed, per_pool_top20 | **P0** — cascades to every subsequent epoch |
| `governance` | drep/cc/proposals | P1 — Conway-only, isolated |
| `stake` | snapshot totals, pool counts | P1 |
| `utxo` | count, total_lovelace, asset_count | P1 — mostly uncoverable today |
| `nonce` | eta_v etc | P1 — uncoverable today |
| `pp` | protocol parameters | P2 — uncoverable today |
| `era` | era / protocol version | P2 — uncoverable today |

Any non-zero `rewards` bucket on coverable fields is a P0 — investigate immediately.
