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
│   ├── raw-socket-send.py     (write arbitrary bytes to a unix/TCP socket)
│   ├── tx-cbor-tool.py        (byte-level tx surgery: show/dup-input/sign)
│   ├── ed25519_pure.py        (RFC 8032 signer — stdlib only, self-testing)
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

# 7. Fail the run if any script skipped for an ENVIRONMENTAL reason
./tx-zoo/run-all.sh --strict-skips
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

## Skips are classified (#918)

A permanent SKIP is indistinguishable from a PASS in a summary line, which
is how three scripts (`08r`, `08f`, `11c`) claimed coverage they had never
once exercised. `run-all.sh` therefore splits them:

- **ENV-SKIP** — the check could not run *at all* on this host: a missing
  tool, key, binary, or harness capability. Structural: it will skip
  identically on every future run. Listed separately in the summary, and
  `./run-all.sh --strict-skips` exits non-zero when any are present.
- **SKIP** — the chain legitimately lacked the precondition this round
  (e.g. `04g no-rewards` before the first epoch boundary). Non-fatal by
  design; a later round covers it.

Record the first kind with `zoo_record_env_skip "$NAME" "<reason>"` (it
prefixes the detail with `env:`); the status column stays `SKIP`, so every
existing consumer of `results.csv` is unaffected. Reasons predating the
convention are classified by the pattern tables in `lib/tx-zoo-common.sh`.

Required tooling (`cardano-cli`, `jq`, `python3`, `curl`) is checked once,
loudly, by `./run-all.sh --setup` — a missing required tool is a hard error
there rather than a silent per-script SKIP at run time.

## Vendored python helpers

tx-zoo depends on `python3` already, so capabilities that would otherwise
need an extra binary are vendored (stdlib only, no pip):

- `lib/raw-socket-send.py` — writes arbitrary/malformed bytes to a unix or
  TCP socket and reports the connection outcome. Replaces `socat`, which is
  not installed by default on macOS or minimal CI images.
- `lib/tx-cbor-tool.py` — `show` / `body-hash` / `dup-input` / `sign` on a
  text-envelope transaction, operating on byte spans so nothing but the
  edited field changes. Needed because cardano-cli cannot represent some
  adversarial transactions at all (see `08f`).
- `lib/ed25519_pure.py` — RFC 8032 signer used by `tx-cbor-tool sign`.
  Never trusted blind: callers sign a body cardano-cli *can* sign and
  byte-compare the two signatures first.

## Coverage matrix

| # | Category | Scripts | Status |
|---|----------|---------|--------|
| 01 | bookkeeping       | 8 | implemented |
| 02 | native scripts    | 7 | implemented |
| 03 | plutus            | 13 | implemented |
| 04 | stake             | 7 | implemented |
| 05 | governance certs  | 8 | implemented |
| 06 | governance props  | 7 | implemented |
| 07 | voting            | 7 | implemented |
| 08 | negative          | 4 | implemented |
| ** | **total**         | **59** | |

## Plutus binaries

`lib/build-plutus.sh` materialises them from
`tests/conformance/upstream/plutus-examples.json` — IntersectMBO's own
plutus-tx-compiled scripts, vendored from cardano-ledger — and verifies every
envelope against the ScriptHash upstream recorded beside the bytes before the
zoo uses it. No compiler to install (#970).

They are **not** "always true". They assert on the script purpose and on datum
presence, which is the point: an always-true validator never reads the
ScriptContext, so it cannot catch a context-construction defect (#772, #969).

- spending tests want `alwaysSucceedsWithDatum` — every spend in the zoo
  carries a datum, including V3's inline one
- mint / certify / reward / vote / propose want `alwaysSucceedsNoDatum`
- a spend that must FAIL wants `alwaysFailsWithDatum`; `alwaysFailsNoDatum` is
  TRUE for spending-with-datum

`17-context-inspecting` drives the ones that read `TxInfo` contents.

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
