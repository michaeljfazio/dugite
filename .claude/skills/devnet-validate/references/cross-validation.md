# Cross-validation — evidence schemas and predicate semantics

## Evidence directory

Each `soak.sh <duration>` run creates `testnet/local-devnet/evidence/<UTC-timestamp>/`:

```
evidence/20260521T120000Z/
├── metadata.json          # git rev, versions, ports, genesis hashes
├── tip-samples.csv        # ts,node,slot,block_no,hash,era
├── blocks.csv             # ts,observer,event,slot,hash,issuer_vkey,body_size,n_txs
├── tx-submissions.csv     # ts,target_socket,wave,txid,submit_rc
├── tip-age-samples.csv    # ts,node,tip_age_seconds
└── logs/                  # post-run snapshots of the 3 node logs
```

## Schema details

### `blocks.csv`
The richest evidence. Each row is one observation of a block at one observer:

| Column | Source | Meaning |
|---|---|---|
| `ts` | wall clock | When this observer first saw the block |
| `observer` | one of `dugite-bp` / `dugite-relay` / `cardano-bp` | The node that emitted the event |
| `event` | `forge` / `recv` | `forge` = this node minted it; `recv` = it arrived from a peer |
| `slot` | block header | Slot number |
| `hash` | block header | Block hash |
| `issuer_vkey` | block header | Forger's verification key hash |
| `body_size` | block trailer | Body length in bytes |
| `n_txs` | block | Tx count |

`?` in any column means the sampler couldn't decode that field — usually transient at boot.

### `tip-age-samples.csv`
Sampled every 5s from each dugite process's Prometheus endpoint. `tip_age_seconds` is the wall-clock delta between now and the slot time of the current chain tip. A healthy node oscillates between 0 and `1/f` seconds (≈2s). A stuck node climbs monotonically. This is the issue #508 detector.

### `tx-submissions.csv`
Filled when `soak.sh` also drives a tx workload (Round 2). `submit_rc` is the cardano-cli exit code (0 = accepted).

## `verify.sh` predicates

`./verify.sh evidence/<ts>` runs four predicates and prints `PASS:` / `FAIL:` for each.

### p1 — forge cross-check

**Claim**: every canonical block is observed by all 3 nodes.

A block is **canonical** at end-of-soak iff both BPs (dugite-bp + cardano-bp) have an event for it. Orphans only have the forger's `forge` event and are filtered out before counting observers (Bug J / 2026-05-16 follow-up — see verify.sh comment block).

The most-recent 10 blocks are trimmed off the tail to allow for in-flight propagation at soak end.

`p1 PASS` proves diffusion is reliable across the hub. `p1 FAIL` means a block was forged + accepted by one peer but not adopted by another — usually a chain-selection or ChainSync bug.

### p2 — tip-age freshness

**Claim**: at end-of-soak, both dugite processes report `tip_age_seconds < 30`.

`p2 PASS` proves the chain isn't stale at the moment we stopped sampling. `p2 FAIL` means the node thinks the tip is from the past — Issue #508 / stall class.

### p3 — invalid forge detector

**Claim**: zero `TraceForgedInvalidBlock` events in `logs/cardano-bp.log`.

`p3 PASS` proves Haskell ledger accepted every dugite-forged block. `p3 FAIL` is the most-critical failure: dugite produced a block the reference rejected. Quote the reason verbatim in the report.

### p4 — tx admission parity

**Claim**: every tx in `tx-submissions.csv` with `submit_rc = 0` is found in the on-chain UTxO of every observer; every tx with `submit_rc != 0` is NOT found.

`p4 PASS` proves Phase-1 validation parity. `p4 FAIL` means dugite accepted a tx Haskell would have rejected (or vice-versa).

## Augmentation: `analyze-evidence.sh`

The skill bundles `scripts/analyze-evidence.sh` for the human-readable summary:

- Total canonical blocks observed
- Orphan count and rate (= orphans / total_forges)
- Average + p99 tip-age across the soak
- Boot timing (relay socket appearance, first forge, first cardano-bp adoption)
- Histogram of log ERROR / WARN lines per node
- Diff of `dugite_chain_density` vs `activeSlotsCoeff`

Run it after every `verify.sh`:
```bash
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/<ts>
```

It exits non-zero if it surfaces any anomaly above the configured thresholds — useful as a CI gate.

## What "byte-exact" actually proves

A PASS of p1+p3 across all three rounds means:
- The CBOR bytes dugite-bp wrote on the wire **deserialize to the same Haskell `Block` record** as cardano-node would have produced
- The Haskell ledger transition function applied to those bytes produces the same UTxO state delta
- The Praos validation function applied to those bytes accepts the block (correct VRF leader, correct opcert, correct KES signature, correct body hash)

This is the highest cross-validation signal available without a full epoch-state byte-diff. For ledger-state byte-equality see the public-testnet reward-dumps tooling (out of scope here).
