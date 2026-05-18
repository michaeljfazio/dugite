# tx-zoo — transaction surface exerciser for the local devnet

A self-contained collection of transaction scripts that exercise every
class of Conway-era transaction Dugite needs to accept and produce
identical results to the Haskell node. Each script builds, signs, and
submits one (or more) transaction(s) against the local devnet (3 nodes:
dugite-bp, dugite-relay, cardano-bp), then asserts inclusion across all
three observers.

> Designed for the local devnet only (magic 42, controlled keys, fast
> epochs). Not safe to run against public testnets.

## Layout

```
tx-zoo/
├── README.md                 (this file)
├── run-all.sh                 (orchestrator with category filtering)
├── lib/
│   ├── tx-zoo-common.sh       (shared helpers — sourced by every script)
│   ├── keygen.sh              (provisions sub-payment, stake, DRep, CC, pool keys)
│   ├── build-plutus.sh        (compiles or vendors Plutus binaries)
│   ├── plutus/                (Plutus V1/V2/V3 always-true binaries)
│   └── native/                (sample native script JSONs)
│
├── 01-bookkeeping/            (Phase-1, non-script)
├── 02-native-scripts/         (CIP-1 native scripts: mint, burn, multisig)
├── 03-plutus/                 (V1/V2/V3 spend + mint, inline datum, ref scripts)
├── 04-stake/                  (stake reg/dereg/deleg, pool reg/retire, withdrawals)
├── 05-governance-certs/       (DRep, vote-deleg, CC hot-key auth)
├── 06-proposals/              (CIP-1694 governance actions)
├── 07-voting/                 (DRep/SPO/CC votes × yes/no/abstain)
└── 08-negative/               (must-reject txs — fee/ttl/min-utxo/collateral)
```

## Usage

```bash
# 1. Bring up the devnet (separate terminal)
cd testnet/local-devnet && ./run.sh

# 2. One-time prep (keys + plutus binaries)
./tx-zoo/run-all.sh --setup

# 3. Run a single category
./tx-zoo/run-all.sh 01-bookkeeping

# 4. Run a single script
./tx-zoo/03-plutus/03a-spend-v1.sh

# 5. Run the full zoo (sequential — total ~5 min)
./tx-zoo/run-all.sh

# 6. Inspect results
column -t -s, ./tx-zoo/state/results.csv
```

Override the target socket per-run with `ZOO_SOCKET=<sock>` (defaults to
the relay). Useful for sending different waves at each node.

## What each script asserts

A script PASSES when:
1. The tx builds without error.
2. `cardano-cli transaction submit` returns success (no mempool rejection).
3. The transaction's outputs (or effects) appear in the UTxO of at least
   one observer within 60s. Critical scripts wait for all 3.

Negative tests (08-) invert (2): they PASS when the submit *fails* with
a specific expected error.

## Coverage matrix

| # | Category | Scripts | Status |
|---|----------|---------|--------|
| 01 | bookkeeping       | 8 | implemented |
| 02 | native scripts    | 7 | implemented |
| 03 | plutus            | 11 | implemented (V3 may need aiken — see below) |
| 04 | stake             | 7 | implemented |
| 05 | governance certs  | 8 | implemented |
| 06 | governance props  | 7 | implemented |
| 07 | voting            | 7 | implemented |
| 08 | negative          | 4 | implemented |
| ** | **total**         | **59** | |

## Plutus binaries

`lib/build-plutus.sh` builds always-true validators for V1/V2/V3.

- If `aiken` is installed (`brew install aiken-lang/tap/aiken`), it
  compiles real binaries. Preferred.
- Otherwise it vendors known-good bytes for V1 and V2 from the IOG
  cardano-node integration test fixtures, plus a best-effort V3
  candidate. The V3 wire shape changed several times during Conway
  development; if a V3 spend/mint submit fails, install aiken and
  rerun `lib/build-plutus.sh`.

## State and reruns

All artifacts live under `tx-zoo/state/` (gitignored):
- `state/keys/`     — auxiliary keys (idempotent across reruns)
- `state/built/`    — built tx bodies and signed files
- `state/logs/`     — per-script stderr capture
- `state/results.csv` — append-only run log

`run-all.sh --reset` wipes state. Otherwise reruns reuse keys.

## Adding a new tx type

1. Drop a `NN-name.sh` into the appropriate category dir.
2. Source `lib/tx-zoo-common.sh` at the top.
3. End with `zoo_record "$(zoo_name)" PASS "$txid"` or `FAIL`.
4. `chmod +x` the script.

`run-all.sh` discovers scripts via shell glob — no central registry to
update.

## Related issues

- Issue #508 (caught false 19d stall) — coverage of negative tip-age was
  the trigger for asking what other transaction surfaces we should
  exercise. The tx-zoo answers that for every other ledger op.
