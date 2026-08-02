# tx-zoo — what each category proves

The tx-zoo lives at `testnet/local-devnet/tx-zoo/`. It exercises every Conway-era transaction class dugite must accept and produce results identical to the Haskell node.

> For the broader test-coverage charter (six orthogonal axes, parity oracle, dugite-cli surface map, UTxO RPC matrix, era / governance / stress / adversarial recipes), see `test-methodology.md`. This file is the **per-category catalogue**; the methodology doc is the **coverage strategy**.

## Coverage matrix

| # | Category | Scripts | What it proves |
|---|---|---|---|
| 01 | bookkeeping | 8 | Phase-1 happy path: address derivation, fee calc, witness binding, change handling, UTxO accounting, multi-input txs, multi-output txs, metadata |
| 02 | native scripts | 7 | CIP-1 multi-sig: simple all/any/of, time-bounded, key-hash, mint+burn under native policy |
| 03 | plutus | 13 | V1 spend, V2 spend, V3 spend; V1/V2/V3 mint; inline datum; reference scripts; reference inputs; collateral selection; cost models; is_valid=false collateral path. **Spend and mint purposes ONLY** — certifying / rewarding / voting / proposing are unexercised (#955) |
| 04 | stake | 7 | Stake registration/deregistration, delegation, pool registration/retirement, reward withdrawal, MIR (if applicable) |
| 05 | governance certs | 8 | DRep registration/update/deregister, vote delegation, CC hot-key authorize, CC hot-key resign, committee membership |
| 06 | proposals | 7 | CIP-1694 gov actions: ParameterChange, HardForkInitiation, TreasuryWithdrawals, NewCommittee, NewConstitution, InfoAction, NoConfidence |
| 07 | voting | 7 | DRep / SPO / CC votes × {Yes, No, Abstain} — six wire paths plus a vote-rationale anchor |
| 08 | negative | 19 | Must-reject txs: fee-below-min, ttl-expired, min-utxo-violation, missing-collateral, NoInputs, DuplicateInput, InputNotFound, ValueNotConserved, TxTooLarge, NotYetValid, BadSignature, MissingRequiredSigner, OutputValueTooLarge, WrongNetworkOutput, InvalidMint, NativeScriptFailed, RefInputNotFound, MalformedCBOR, StakePoolCostTooLow. PASS = `cardano-cli ... submit` returns non-zero with the *expected* reason |
| 10 | gov lifecycle | 5 | propose → DRep vote → SPO vote → CC vote → assert enactment. **ParameterChange only** (#956) |
| 11 | mempool | 3 | TTL eviction, input-conflict rejection, drain latency p99 |
| 12 | post-enactment | 1 | InvalidPrevGovActionId — only meaningful once an action has actually enacted, so it MUST run after 10 |
| 13 | script purposes | 9 | Plutus **Certifying / Rewarding / Voting / Proposing** purposes + native-script and Plutus stake credentials. Asserts the redeemer purpose tag is on the wire (`tx-cbor-tool.py redeemers --require`), and requires Haskell confirmation via `wait_all_strict` — no "cbp lagging" soft-pass (#955) |
| 14 | gov negatives | 6 | Conway `ConwayGovPredFailure` rejection **classes**: GovActionsDoNotExist, VotersDoNotExist, DisallowedVoters, ProposalDepositIncorrect (below *and* above — the check is exact equality, not a floor), TreasuryWithdrawalReturnAccountsDoNotExist (#956) |

Total: **100 scripts** (excluding the 22 in `09-cli-parity`, which measure the
node's LSQ responses rather than tx admission). Sequential runtime ~8–12 minutes.

The count is pinned in `../schemas/denominators.json` and asserted by
`../scripts/test-denominators.sh`; adding a script without bumping the pin fails
that check rather than silently widening the gate.

## How each script asserts

A positive (01–07) script PASSES when:
1. `cardano-cli transaction build`/`build-raw` succeeds
2. `cardano-cli transaction submit` returns 0 (mempool accepted)
3. The tx's outputs (or effects) appear in the UTxO of ≥1 observer within 60s. Critical scripts wait for all 3 observers.

A negative (08) script PASSES when step (2) FAILS with a known error pattern matched in the script.

Skips are classified (`lib/tx-zoo-common.sh`, `zoo_skip_class`):
- **ENV-SKIP** — the check could not run at all (missing tool/key/capability).
  Structural: it skips identically every run, so the surface is *never*
  exercised. `run-all.sh --strict-skips` turns these into failures.
- **SKIP** — the chain legitimately lacked the precondition this round.

Results are appended to `tx-zoo/state/results.csv`. Inspect with:
```bash
column -t -s, testnet/local-devnet/tx-zoo/state/results.csv
```

## Known-flaky cases

| Script | Failure mode | Workaround |
|---|---|---|
| `03-plutus/03e-spend-v3.sh` (or similar V3) | Vendored V3 binary uses an older wire shape | Install `aiken` (`brew install aiken-lang/tap/aiken`), then rerun `tx-zoo/lib/build-plutus.sh` to regenerate the V3 always-true binary |
| Anything that submits to `cardano-bp.sock` | cardano-node socket appears 1–2s after the process starts | tx-zoo's helpers already wait for the socket; only an issue if `run.sh` was interrupted mid-boot |

Treat anything else failing as a **real** failure. Do not auto-skip.

## Running subsets

```bash
# All categories, sequential
./tx-zoo/run-all.sh

# One category
./tx-zoo/run-all.sh 06-proposals

# One script
./tx-zoo/03-plutus/03a-spend-v1.sh

# Wipe state and rerun fresh
./tx-zoo/run-all.sh --reset
```

`run-all.sh` discovers scripts via shell glob. New scripts dropped into a numbered directory are picked up automatically (see `tx-zoo/README.md` "Adding a new tx type").

## Cross-validation per tx

When a tx-zoo script submits a transaction, the validation path is:

1. **dugite-cli builds it** (proves cardano-cli ⇄ dugite-cli compat at the build layer — though scripts use cardano-cli for now)
2. **Submit to one socket** (relay by default; override `ZOO_SOCKET=<sock>`)
3. **dugite-node validates Phase-1** (proves dugite ledger admits the tx)
4. **dugite-bp forges it into a block**
5. **cardano-bp re-applies the block** (proves Haskell ledger accepts what dugite produced — byte-exact)
6. **Script queries each observer's UTxO** (proves diffusion + apply on all three nodes)

A failure at any step pinpoints the layer. Always read the failing script's `state/logs/<script>.stderr` first.

## Negative tests (08-) interpretation

The four negative scripts test that dugite REJECTS what Haskell rejects. They are equally critical: silent acceptance of bad txs is the worst-class bug (memory: `feedback_dugite_node_hostile_environment`).

If a negative script reports FAIL, dugite has either:
- Wrongly accepted a malformed tx (silent-skip class — security audit-style failure), or
- Rejected it with a different error than Haskell (compat regression).

Either way, do not declare the round PASS. Capture the script's `state/logs/<n>-*.stderr` and the corresponding `dugite-bp.log` lines.

## Bidirectional submission (parity oracle)

Every tx-zoo script honours `ZOO_SOCKET` to choose the N2C ingestion socket:

```bash
ZOO_SOCKET=$LD_DUGITE_BP_SOCK ./tx-zoo/<script>.sh   # path A: forger direct
ZOO_SOCKET=$LD_RELAY_SOCK    ./tx-zoo/<script>.sh    # path B: relay-mediated (default)
ZOO_SOCKET=$LD_CARDANO_BP_SOCK  ./tx-zoo/<script>.sh    # path C: Haskell-mediated (reverse mempool path)
```

The standard playbook runs path B for the full 85-script suite, then re-runs
**79 of them** via path C (all categories except `10-gov-lifecycle` and
`12-post-enactment`, whose subject is chain-global governance state). Invoke
`bidirectional-parity.sh` with no arguments to get the pinned standard set;
naming categories at the call site is what kept it at 41 scripts (#954). Off-diagonal cells in the resulting accept/reject matrix (see `test-methodology.md` for the matrix) are P0 bugs — they prove a tx that dugite admits is rejected by Haskell (or vice versa). All 19 `08-negative` scripts must produce identical rejection reasons on both paths.

For dugite-cli ↔ cardano-cli submit-layer parity (path D), use `cross-validate-cli.sh` — it submits one representative tx per category via dugite-cli to dugite-bp.sock and observes inclusion via cardano-cli on the relay socket. The two cli implementations must produce byte-identical signed-tx CBOR; a mismatch shows up as the wrong txid on inclusion.

## Suggested extensions (P2)

Not in scope for the standard 3-round playbook, but useful for deeper investigations:
- Tx hitting max body size (`maxBlockBodySize = 90112`)
- Tx with all-V3 reference scripts (cost-model worst case)
- Tx with 100+ inputs (UTxO-HD diff worst case)
- Concurrent submission to all three sockets simultaneously
- CBOR fuzz inputs to N2C (existing `cargo test -p dugite-network` has some; not yet wired to devnet)

If you find a class of bug that the existing scripts didn't catch, add a script
to the appropriate category — the test should fail before the fix lands and pass
after — and bump the pinned count in `../schemas/denominators.json` in the same
commit.
