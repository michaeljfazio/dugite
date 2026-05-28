# Test methodology — coverage axes, parity oracles, and adversarial stimuli

This file is the **coverage charter** for devnet-validate. It defines what "thoroughly tested" means in this skill, organised around six orthogonal axes. Every meaningful behaviour of dugite-node should be exercised along **every** axis where it is meaningful:

1. **Tx-type axis** — every Conway-era transaction class (and every era's classes when era coverage is in play).
2. **Validity axis** — the same tx class submitted with both valid and invalid variants (positive and negative cases).
3. **Submit-path axis** — submitted to every reachable N2C socket: dugite-bp, dugite-relay, cardano-relay, plus dugite-cli vs cardano-cli on each.
4. **Propagation-direction axis** — the data flow observed end-to-end from the submit-point to every other observer in both directions through the relay.
5. **Actor axis** — good-actor input (well-formed protocol traffic) and bad-actor input (malformed CBOR, oversized, replay, censorship, flood).
6. **Workload axis** — quiescent / single-tx / sustained-trickle / saturation / burst, including under epoch and era transitions.

For tx-class definitions and per-script semantics, see `tx-coverage.md`. For the full chaos catalogue, see SKILL.md "Chaos tests". For end-of-run report aggregation, see `cross-validation.md`. For per-probe runtime evaluation, see `health.md`.

## The bidirectional acceptance-parity oracle

The strongest cross-validation predicate in this skill is **symmetric acceptance parity**:

> For **every** transaction `T`, dugite's accept/reject decision must match Haskell's, regardless of which node ingested it first.

This generalises the existing one-direction tx-zoo (submit to dugite, check Haskell adopts the block) into a four-cell parity matrix:

```
                 Haskell accepts            Haskell rejects
              ┌───────────────────────┬───────────────────────┐
dugite accepts │  PASS (positive)      │  FAIL — silent-skip  │
              │  expected for 01–07   │  dugite too lax       │
              ├───────────────────────┼───────────────────────┤
dugite rejects │  FAIL — false-reject  │  PASS (negative)     │
              │  dugite too strict    │  expected for 08–11   │
              └───────────────────────┴───────────────────────┘
```

Both off-diagonal cells are bugs of the same severity. The skill MUST exercise enough variants to populate the matrix for every relevant tx class.

### How to exercise it in practice

For each representative tx (a balanced subset of all 59 zoo scripts plus the 19 negatives), run the same tx through **both** submit paths and capture both outcomes:

```bash
# Path A — submit to dugite (default)
ZOO_SOCKET=$LD_RELAY_SOCK ./tx-zoo/<script>.sh

# Path B — submit to Haskell
ZOO_SOCKET=$LD_CARDANO_BP_SOCK ./tx-zoo/<script>.sh
```

Tabulate both outcomes into `evidence/<ts>/parity-matrix.csv` (`txid,class,submit_node,dugite_accept,haskell_accept,result`) and FAIL the round on any off-diagonal row.

For negative cases (08-, 11-), the parity predicate is more nuanced: not only must both reject, the **rejection reason** should match (or be filed as a documented divergence in `references/troubleshooting.md`). Reject-reason mismatches are P2 — accept-set mismatches are P0.

## Coverage matrix — six axes

The cells below describe what the skill currently exercises and where the gaps are.

### Axis 1 — Tx-type coverage (within Conway)

| Class | Scripts | Bidirectional? | Era coverage? |
|---|---|---|---|
| 01 bookkeeping | 8 | ✓ via ZOO_SOCKET | Conway only — see "Era coverage" below |
| 02 native scripts | 7 | ✓ | Conway |
| 03 plutus V1/V2/V3 | 11 | ✓ | Conway (V1/V2/V3 wire formats all exercised) |
| 04 stake | 7 | ✓ | Conway |
| 05 governance certs | 8 | ✓ | Conway |
| 06 gov proposals | 7 | ✓ | Conway |
| 07 voting | 7 | ✓ | Conway |
| 08 negative (phase-1) | 19 | ✓ | Conway |
| 10 gov lifecycle | 5 | E2E | Conway (1+ epoch boundary required) |
| 11 mempool | 3 | dugite-only — gap | Conway |

**Gap**: 11-mempool tests only dugite. Mirror against cardano-bp (TTL eviction + input-conflict) and assert symmetric behaviour.

### Axis 2 — Validity coverage

For every positive class above, ensure a corresponding negative exists in `08-negative/`. Current 08e–08s already cover: NoInputs, DuplicateInput, InputNotFound, ValueNotConserved, TxTooLarge, NotYetValid, BadSignature, MissingRequiredSigner, OutputValueTooLarge, WrongNetworkOutput, InvalidMint, NativeScriptFailed, RefInputNotFound, MalformedCBOR, StakePoolCostTooLow.

**Gaps to add** (no current script):
- Plutus V1/V2/V3 — script-eval false (UnknownCostModel, ExBudgetExceeded, MalformedScript, BadConstructorTag, IndefiniteLengthCBOR in script body)
- Vote with stale epoch / proposal expired / DRep deregistered
- Proposal deposit below minimum / above pparam cap
- Treasury withdrawal exceeding treasury balance
- Hard-fork initiation with unsupported PV
- Duplicate stake-key registration / DRep registration / pool registration
- Withdrawal of zero-balance reward account
- Reference input that exists but is not a script UTxO when used as ref-script
- Plutus V2/V3 with a `posix` time outside chain's `validity-interval`

Each is a `08-negative/<id>` candidate. The cross-validation requirement is symmetric: dugite and Haskell must both reject AND the predicate failure tag should match (`ConwayUtxowFailure`, `ConwayLedgerPredFailure`, `ConwayGovFailure`, …).

### Axis 3 — Submit-path coverage

The devnet exposes four N2C ingestion paths:

| Path | Socket | Default in tx-zoo | Use for |
|---|---|---|---|
| A | `state/dugite-bp.sock` | opt-in (`ZOO_SOCKET=$LD_DUGITE_BP_SOCK`) | Forger-direct submission — proves N2C handler on the BP |
| B | `state/dugite-relay.sock` | default | Production-shape path — proves relay→BP mempool forwarding |
| C | `state/cardano-bp.sock` | opt-in (`ZOO_SOCKET=$LD_CARDANO_BP_SOCK`) | Haskell-validator submission — parity oracle source |
| D | dugite-cli vs cardano-cli, on any of A/B/C | `cross-validate-cli.sh` for a smoke subset | Proves dugite-cli wire-format equivalence |

Each test in `tx-zoo/` should run at least once via A or B (proves dugite ingests it) AND at least once via C (proves Haskell ingests it). The parity-matrix CSV pinpoints which subset is fully covered.

### Axis 4 — Propagation-direction coverage

The devnet is **not** a flat broadcast — it has a fixed hop order:

```
dugite-bp ── N2N ──▶ dugite-relay ── N2N ──▶ cardano-bp
    ▲                                              │
    └──────────── reverse mempool path ────────────┘
```

A submitted tx can be observed at any of three nodes. The matrix of `(submitter, mempool-observer, ledger-observer)` is what we want to populate exhaustively:

| Submit to | Mempool seen at | Block-inclusion seen at | Tests |
|---|---|---|---|
| dugite-bp | dugite-bp, then propagates forward to dugite-relay → cardano-bp | dugite-bp forges; relay + cardano adopt | A→{A,B,C} ledger |
| dugite-relay | dugite-relay, then to dugite-bp (forward) and cardano-bp (forward) | dugite-bp forges; all 3 adopt | B→{A,B,C} ledger |
| cardano-bp | cardano-bp, then back to dugite-relay → dugite-bp (reverse direction) | dugite-bp forges (only it has keys); all 3 adopt | C→{A,B,C} ledger |

Path C is the most demanding: it exercises **dugite-relay's TxSubmission server** (receiving Haskell-originated txs) and **dugite-bp's TxSubmission client** (pulling txs back from the relay). Many subtle bugs live only on this path (TxSubmission state-machine bugs, indef-length CBOR in tx hashes, etc.). The `08r-malformed-cbor-n2c.sh` negative is path-A only; mirror it on path C.

### Axis 5 — Actor coverage (good actor + bad actor)

#### Good-actor inputs

Everything in `tx-zoo/01..07` plus `09-cli-parity` plus `10-gov-lifecycle`. Already exhaustive within Conway.

#### Bad-actor inputs

| Class | Where | Coverage today | Gap |
|---|---|---|---|
| Malformed tx CBOR | `tx-zoo/08r-malformed-cbor-n2c.sh` | path A only | mirror to paths B + C |
| Adversarial N2N framing | `protocols/01–07-*.sh` | full | — |
| Slow-loris on the listener | `protocols/07-slow-loris.sh` | extended preset only | should run in every standard round once #535 ships |
| Inbound SYN flood | `chaos/inbound-syn-flood.sh` | smoke | — |
| Replay attack | NOT TESTED — gap | — | Add: submit a confirmed-on-chain tx a second time; both nodes must reject (`BadInputsUTxO`/`InputNotFound`) without re-broadcasting |
| Censorship attack | NOT TESTED — gap | — | Add: relay node refuses to forward a specific txid; assert dugite-bp's mempool still receives it via direct A-path submission |
| Long-range fork attack | covered conceptually by chain-selection tests; not stimulus-driven | — | (Out of scope for devnet — needs synthetic peer) |
| Equivocation (double-forge) | NOT TESTED — gap | — | Inject two blocks at the same slot from same VRF key; both peers must keep the first-seen and tag the second as adversarial in logs |
| KES key compromise | NOT TESTED — gap | — | Hand-craft a block with a valid opcert but a KES signature from a sibling key; both nodes must reject |
| Mempool-size DoS | partial (capacity check) | — | Add: submit `mempool_tx_max` valid txs from N2C; observe oldest evicted; submit one more tx via Haskell and observe identical eviction order |
| Network-level abuse | `protocols/` covers framing; mux-level fairness not tested | — | Open 100 mux conns from one IP; verify per-IP rate-limit holds (memory: `project_inbound_per_ip_rate_limit`) |

Each gap is a candidate script. Filing them as `08-negative` / `protocols/` / `chaos/` issues is the unit of work.

### Axis 6 — Workload coverage

| Workload | Today | Stress goal | Where it lives |
|---|---|---|---|
| Idle (no tx) | Round 1 baseline | — | `./soak.sh 120` |
| Single-tx waves | tx-zoo sequential | — | `./tx-zoo/run-all.sh` |
| Sustained trickle (one tx every 20s) | Round 2 | catches boundary RUPD interactions | `tx-zoo/01a-simple-pay.sh` loop |
| Saturation (mempool full) | NOT in standard rounds — gap | should land for v1.8.0 | candidate `11-mempool/11d-saturation.sh` |
| Concurrent burst (multiple paths simultaneously) | NOT TESTED — gap | catches mempool race conditions, double-spend rejection | candidate `11-mempool/11e-multi-source-burst.sh` |
| Throughput sweep (block-size lattice) | extended preset (`sync/bulk-sync-throughput.sh`) | — | — |
| Tx-size sweep (1KB → maxBlockBodySize) | NOT TESTED — gap | catches Phase-1 size predicates + N2C frame fragmentation | candidate `11-mempool/11f-tx-size-sweep.sh` |

## Era and HF transition coverage

The devnet pins at Conway PV10 by default. Era coverage is therefore not exercised "live" on the local devnet — but the skill still validates era-aware code paths in three other ways:

1. **Tx-class diversity within Conway**: the zoo deliberately exercises every wire feature that originated in Byron/Shelley/Allegra/Mary/Alonzo/Babbage/Conway (multi-sig from Shelley, native tokens from Mary, Plutus from Alonzo, reference inputs from Babbage, governance from Conway). Any era-decode regression surfaces here.
2. **PV-bump within Conway**: governance lifecycle (`10-gov-lifecycle/`) ratifies a `HardForkInitiation` to a new minor PV inside Conway. Both nodes must enact at the same boundary; cross-check `dugite-pparam-protocol-version` against `cardano-cli query protocol-parameters`.
3. **Full era walk via fixture replay** — out of scope for devnet; lives in `tests/conformance/upstream/`. The devnet only proves "Conway PV10 today works"; the era-walk corpus proves "every historical era's blocks decode and apply".

If a future devnet variant supports a `--start-era babbage` override, the Round 2 PASS criteria should include an in-run Babbage→Conway HF.

## Governance lifecycle — full E2E coverage

`tx-zoo/10-gov-lifecycle/` already exercises propose → DRep vote → SPO vote → CC vote → assert-enactment for a `ParameterChange` action. For full governance coverage, the skill should also exercise (current state in parens):

| Action class | E2E test? |
|---|---|
| `ParameterChange` | ✓ (`10a..10e`) |
| `HardForkInitiation` | partial — proposal exists in 06; lifecycle test gap |
| `TreasuryWithdrawals` | proposal in 06; lifecycle test gap |
| `NewCommittee` | proposal in 06; lifecycle test gap |
| `NewConstitution` | proposal in 06; lifecycle test gap |
| `InfoAction` | proposal in 06; lifecycle test gap |
| `NoConfidence` | proposal in 06; lifecycle test gap |

Each enactment must be byte-validated on both sides: `cardano-cli query gov-state` outputs from dugite vs cardano-bp should be JSON-equal modulo ordering after the boundary.

Negative governance tests (gap):
- Vote from a deregistered DRep
- Vote past the proposal's expiration epoch
- Proposal with deposit below pparam
- Multiple proposals competing for the same `NewCommittee` slot

## Stress-testing recipes

### Mempool-saturation soak (5 min)

```bash
# Pre: ./run.sh up, wallets funded
( while true; do
    for sock in "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK"; do
        ZOO_SOCKET="$sock" ./tx-zoo/01-bookkeeping/01a-simple-pay.sh \
            >/dev/null 2>&1 &
    done
    wait
done ) &
SAT=$!
sleep 300
kill $SAT
# Verify both mempools drained AND no tx was dropped before forge:
n_submitted=$(grep -c TraceMempoolAccepted logs/cardano-bp.log)
n_inblocks=$(grep -c TraceAdoptedBlock     logs/cardano-bp.log)
[ "$n_submitted" -le $((n_inblocks * 200)) ] || echo "DROP: $n_submitted vs $n_inblocks"
```

### Concurrent-burst race

```bash
# 100 distinct txs submitted to all three sockets simultaneously.
# Predicates: zero double-spend acceptances; mempool count peaks then drains.
for i in $(seq 1 100); do
    for sock in "$LD_DUGITE_BP_SOCK" "$LD_RELAY_SOCK" "$LD_CARDANO_BP_SOCK"; do
        ZOO_SOCKET="$sock" ./tx-zoo/01-bookkeeping/01a-simple-pay.sh \
            >/dev/null 2>&1 &
    done
done
wait
# Hit health-probe.sh once a minute throughout to catch transient stalls.
```

### Tx-size lattice

For tx sizes `[1KB, 4KB, 16KB, 64KB, maxBlockBodySize-1, maxBlockBodySize, maxBlockBodySize+1]`:
- The last must be rejected by both nodes with `TxTooLarge`.
- All others must be accepted with byte-identical body hashes on both sides.

### Adversarial-stimulus run (Round 4 — new)

A new round dedicated to bad-actor inputs:

```bash
./protocols/run.sh                          # N2N adversarial framing (7 scripts)
./chaos/inbound-syn-flood.sh                # listener resilience
./chaos/clock-skew.sh                       # future-slot rejection
./chaos/disk-full.sh                        # write-failure containment
./tx-zoo/run-all.sh 08-negative             # all 19 negative tx classes (orchestrator runs the directory)
# Replay + censorship + double-spend (when scripts exist):
# ./tx-zoo/08-negative/08t-replay.sh
# ./tx-zoo/08-negative/08u-double-spend-burst.sh
./verify.sh evidence/$(ls -t evidence | head -1)
.claude/skills/devnet-validate/scripts/health-probe.sh --verbose
```

**PASS criteria**: every adversarial input is rejected; no panic; no `TraceForgedInvalidBlock`; no `dugite_block_apply_failures_total` increment; `health-probe.sh` returns HEALTHY at the end.

## End-to-end observation matrix

For every submitted tx, the skill should be able to answer: at which observers, in which order, was the tx observed?

| Observer | Mempool acceptance | Block inclusion | Ledger effect |
|---|---|---|---|
| dugite-bp | dugite-bp.log `mempool added` | dugite-bp.log `Forged block` | UTxO query at `state/dugite-bp.sock` |
| dugite-relay | dugite-relay.log `mempool added` | dugite-relay.log `Adopted block` | UTxO query at `state/dugite-relay.sock` |
| cardano-bp | cardano-bp.log `TraceMempoolAccepted` | cardano-bp.log `TraceAdoptedBlock` | UTxO query at `state/cardano-bp.sock` |

`evidence/<ts>/blocks.csv` already records per-observer block events. To make tx-level propagation auditable, the skill should also collect per-observer **mempool** events into `evidence/<ts>/tx-flow.csv` (`ts, observer, txid, event(mempool_added|in_block|rejected), source_socket`). This makes the parity matrix above mechanically verifiable.

## Per-round mapping

| Coverage area | Round 1 (baseline) | Round 2 (boundary) | Round 3 (restart) | Round 4 (adversarial — new) |
|---|---|---|---|---|
| Tx classes 01–07 | ✓ | ✓ (trickle) | — | — |
| Negative txs 08 | ✓ | — | — | ✓ (all 19 + new ones) |
| Bidirectional submit (paths A/B/C) | ✓ representative subset | — | — | ✓ all paths |
| Gov lifecycle (10-) | ✓ | — | — | + negative gov tests |
| Mempool stress (11-) | — | — | — | ✓ + saturation + burst |
| Era / PV transitions | — | partial via gov lifecycle | — | — |
| Chaos / failure injection | — | — | ✓ (restart) | ✓ full suite |
| N2N adversarial framing | ✓ | — | — | ✓ + slow-loris |
| Health probe sampling | every ≤60s | every ≤30s near boundary | post-restart + steady | every ≤60s |
| Parity oracle | dugite-side only | dugite-side only | dugite-side only | ✓ bidirectional + reason-match |

## dugite-cli surface coverage

The cli mirrors `cardano-cli`'s subcommand tree for drop-in compatibility, with both flat and era-prefixed paths (`conway`, `babbage`, `alonzo`, `mary`, `allegra`, `shelley`, `latest` all route to the same handlers today). Top-level groups in `crates/dugite-cli/src/main.rs`:

| Group | Module | Subcommands of note | Coverage in devnet |
|---|---|---|---|
| `address` | `address.rs` | `build`, `key-gen`, `key-hash`, `info` | implicit (tx-zoo derives addresses) — no explicit parity tests; gap |
| `byron key` | `byron.rs` | Byron-era key conversion | not exercised on devnet (Conway-only); needs separate fixture replay |
| `key` | `key.rs` | `generate-payment-key`, `generate-stake-key`, `verification-key-hash` | exercised via tx-zoo keygen.sh; gap on output-format parity |
| `transaction` | `transaction.rs` | `build`, `build-raw`, `sign`, `submit`, `txid`, `calculate-min-fee`, `calculate-min-required-utxo`, `view`, `witness`, `assemble`, `policyid`, `hash-script-data` | `submit` covered by `cross-validate-cli.sh`; `build`/`sign`/`txid` exercised indirectly; `calculate-min-fee`, `calculate-min-required-utxo`, `view`, `witness`, `assemble`, `policyid`, `hash-script-data` NOT explicitly parity-tested; gap |
| `query` | `query.rs` | 22+ subcommands — `tip`, `utxo`, `protocol-parameters`, `stake-distribution`, `stake-address-info`, `gov-state`, `drep-state`, `committee-state`, `tx-mempool {info,next-tx}`, `stake-pools`, `non-myopic-member-rewards`, `stake-snapshot`, `pool-params`, `treasury`, `constitution`, `ratify-state`, `leadership-schedule`, `slot-number`, `kes-period-info`, `proposals`, `ledger-state`, `protocol-state` | `09-cli-parity/` covers tip, protocol-parameters, utxo, stake-distribution, stake-pools, pool-state, stake-snapshot, protocol-state, kes-period-info, slot-number, gov-state, drep-state, drep-stake-distribution, committee-state, treasury, constitution, proposals, future-pparams, stake-pool-default-vote, leadership-schedule, tx-mempool-info, tx-mempool-next (22 checks total); gaps: `stake-address-info`, `non-myopic-member-rewards`, `pool-params`, `ratify-state`, `ledger-state` |
| `stake-address` | `stake_address.rs` | `key-gen`, `build`, `registration-certificate`, `deregistration-certificate`, `delegation-certificate`, `stake-and-vote-delegation-certificate`, `vote-delegation-certificate` | exercised via tx-zoo 04-stake + 05-governance; output-format parity gap |
| `stake-pool` | `stake_pool.rs` | `registration-certificate`, `deregistration-certificate`, `id`, `metadata-hash` | exercised in tx-zoo 04; explicit parity gap |
| `governance` | `governance.rs` | `action {...}`, `committee {...}`, `drep {...}`, `vote {...}` | exercised by tx-zoo 05/06/07; full subtree parity not measured; gap |
| `node` | `node.rs` | `key-gen-KES`, `key-gen-VRF`, `issue-op-cert`, `new-counter`, `key-gen` | exercised at setup time; gap on byte-identical KES/VRF key derivation |
| `genesis` | `genesis.rs` | `hash`, `create-cardano`, others | `dugite-cli genesis-hash` is currently wrong for shelley+ (#606 — memory: `project_epoch_diff_harness_2026_05_22`); explicit negative test should pin this |
| `text-view` | `text_view.rs` | `decode-cbor` | not parity-tested; gap |

### CLI parity-mode meta-test

For every dugite-cli subcommand that has a `cardano-cli` counterpart, the predicate is:

```
dugite-cli <args>  ==  cardano-cli <args>           (byte-equal stdout)
                  or   stable canonical-JSON match  (when output is JSON with ordering noise)
                  or   identical exit code + stderr substring  (for error cases)
```

`09-cli-parity/run.sh` already implements this for 22 query subcommands using a SHA-256 of stdout. Extending to the full surface is largely mechanical — drop one script per subcommand into `09-cli-parity/`.

**Coverage debt for cli**:
- [ ] `transaction calculate-min-fee` parity (formula divergence is a recurring class of bug; see #438)
- [ ] `transaction calculate-min-required-utxo` parity (era-aware; Conway PV9/10/11 differ)
- [ ] `transaction view` JSON-equal parity
- [ ] `transaction witness` + `assemble` round-trip parity (multi-sig flows)
- [ ] `transaction policyid` parity (native-script hashing)
- [ ] `transaction hash-script-data` parity (datum hashing; #624-class encoder bugs surface here)
- [ ] `address build` for every credential variant (payment-only, payment+stake, script, pointer) parity
- [ ] `address info` JSON parity
- [ ] `key verification-key-hash` parity (Hash28 vs Hash<32> mistakes — memory: `CLAUDE.md` rule about 28→32 padding)
- [ ] `stake-pool id` parity for cold-key-derived pool IDs
- [ ] `node issue-op-cert` parity (KES counter + signature wire-format)
- [ ] `genesis hash` parity per era (issue #606 explicit negative test)
- [ ] `query stake-address-info`, `query non-myopic-member-rewards`, `query pool-params`, `query ratify-state`, `query ledger-state`

Tests should land into `09-cli-parity/` numbered after the existing 22.

### Error-path parity

The cli also has an adversarial-input surface that's currently untested. For every subcommand:
- Pass a malformed input file → exit code + stderr substring should match `cardano-cli`.
- Pass an out-of-range argument → same.
- Pass a missing required argument → clap-generated usage error should be identical or documented as a known divergence.

These belong in a new `09-cli-parity/errors/` subdirectory.

## UTxO RPC (UTxORPC gRPC) coverage

dugite-node ships a tonic-based gRPC server implementing the UTxORPC spec at both `v1alpha` and `v1beta`, in `crates/dugite-rpc/`. The server exposes four services, each duplicated for the two API versions:

| Service | Method | What it returns | v1alpha | v1beta |
|---|---|---|---|---|
| `cardano.query.v*.QueryService` | `read_params` | protocol params snapshot | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_utxos` | UTxOs by `TxoRef` | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `search_utxos` | UTxOs by predicate (address, asset, payment cred) | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_data` | datum by hash | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_tx` | tx by hash | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_genesis` | genesis fields | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_era_summary` | era boundaries | ✓ | ✓ |
| `cardano.query.v*.QueryService` | `read_state` | full ledger-state view | (v1beta only) | ✓ |
| `cardano.submit.v*.SubmitService` | `submit_tx` | unary tx submit | ✓ | ✓ |
| `cardano.submit.v*.SubmitService` | `wait_for_tx` | inclusion notification stream | ✓ | ✓ |
| `cardano.submit.v*.SubmitService` | `read_mempool` | mempool snapshot | ✓ | ✓ |
| `cardano.submit.v*.SubmitService` | `watch_mempool` | mempool-event stream | ✓ | ✓ |
| `cardano.sync.v*.SyncService` | `follow_tip` | tip-following stream | ✓ | ✓ |
| `cardano.sync.v*.SyncService` | `read_tip` | one-shot tip | ✓ | ✓ |
| `cardano.watch.v*.WatchService` | `watch_tx` | per-tx event stream | ✓ | ✓ |

The devnet `run.sh` currently does **not** pass `--rpc-port`, so the RPC server is not started by default — this is a coverage gap. Steps to enable:

```bash
# In testnet/local-devnet/run.sh, when launching dugite-bp and dugite-relay,
# append:
#   --rpc-port 9090   (BP)
#   --rpc-port 9091   (relay)
# Both bind to 127.0.0.1.
```

### UTxO RPC test recipe

The skill should add an `rpc/` subdirectory mirroring `tx-zoo/`, with one script per `(service, method, api-version)` tuple. Each script asserts byte-equality of the gRPC response against a reference value derived from either:
- `cardano-cli` for query methods (e.g. `read_params` ≡ `cardano-cli query protocol-parameters` after JSON canonicalisation)
- `evidence/<ts>/blocks.csv` and tx-zoo state for stream + submit methods
- Direct golden fixtures for genesis + era-summary methods

| Suggested script | Predicate |
|---|---|
| `rpc/01-query-read-params.sh` | gRPC `read_params` (v1alpha + v1beta) ≡ `cardano-cli query protocol-parameters` after canonical JSON sort |
| `rpc/02-query-read-utxos-by-ref.sh` | gRPC `read_utxos` for a known funded `TxoRef` returns the same UTxO row as `cardano-cli query utxo --tx-in` |
| `rpc/03-query-search-utxos-by-addr.sh` | gRPC `search_utxos` by address returns the same set as `cardano-cli query utxo --address` |
| `rpc/04-query-search-utxos-by-asset.sh` | post-mint, gRPC search by `policy_id+asset_name` returns the minted output |
| `rpc/05-query-search-utxos-by-pcred.sh` | gRPC search by payment credential returns all addrs sharing that cred |
| `rpc/06-query-read-data.sh` | gRPC `read_data` for the hash of an inline-datum tx output returns the datum CBOR byte-exact |
| `rpc/07-query-read-tx.sh` | gRPC `read_tx` for a confirmed txid returns CBOR byte-equal to `cardano-cli query tx-info` |
| `rpc/08-query-read-genesis.sh` | gRPC `read_genesis` returns the same fields as the local genesis JSON |
| `rpc/09-query-read-era-summary.sh` | gRPC `read_era_summary` matches `cardano-cli query era-history` (when available) |
| `rpc/10-query-read-state.sh` (v1beta) | gRPC `read_state` returns a ledger-state view with treasury/reserves matching `query treasury` + `query ledger-state` |
| `rpc/20-submit-tx-positive.sh` | gRPC `submit_tx` (v1alpha+v1beta) accepts a valid tx; `wait_for_tx` resolves with the in-block status; `cardano-cli query tx` confirms |
| `rpc/21-submit-tx-malformed.sh` | gRPC `submit_tx` of malformed CBOR returns a structured error with the right error code; mempool unaffected |
| `rpc/22-submit-tx-double-spend.sh` | second submission of the same tx returns "already-in-mempool" or "already-on-chain" status, never a duplicate accept |
| `rpc/23-submit-read-mempool.sh` | `read_mempool` returns every tx submitted via N2C in `tx-zoo`; counts match `dugite_mempool_tx_count` |
| `rpc/24-submit-watch-mempool.sh` | `watch_mempool` emits one `Added` event per tx-zoo submit; one `Removed` event per inclusion; ordering matches per-tx wall-clock |
| `rpc/30-sync-read-tip.sh` | gRPC `read_tip` ≡ `cardano-cli query tip` (slot, hash, block_no, era) |
| `rpc/31-sync-follow-tip.sh` | streaming `follow_tip` emits a `BlockAdvance` per new block forged in the soak window; hashes match `blocks.csv` |
| `rpc/40-watch-tx.sh` | `watch_tx` emits per-tx events for a given filter; consistent with N2C `LocalTxMonitor` |
| `rpc/50-versioning.sh` | every method is reachable at BOTH `v1alpha` and `v1beta`; responses are semantically equal modulo proto field renames |
| `rpc/51-reflection.sh` | tonic reflection endpoint lists every expected service; `grpcurl localhost:9090 list` matches the inventory above |

### Adversarial RPC coverage

| Stimulus | Predicate |
|---|---|
| Oversized message (>4MB default) | server returns `INVALID_ARGUMENT` / `RESOURCE_EXHAUSTED`; doesn't crash |
| Invalid TxoRef (zero-length hash) | structured error, no panic, mempool unaffected |
| Concurrent 100× `submit_tx` of the same valid tx | exactly one mempool entry; others get "already-known" status |
| Stream client disconnects mid-`follow_tip` | server cleans up; `dugite_rpc_active_streams` (if present) returns to baseline within 5s |
| `read_state` while ledger is mid-apply | response is consistent with one specific tip (not a torn read) |
| Per-IP flood (100 concurrent calls/sec from one client) | server enforces rate limit / connection cap; other clients unaffected |

### Cross-validation oracles for RPC

| Method | Oracle |
|---|---|
| `read_params` | `cardano-cli query protocol-parameters` (canonical JSON) |
| `read_utxos` / `search_utxos` | `cardano-cli query utxo --whole-utxo` filtered |
| `read_tx` | `cardano-cli query tx-info` (when available) or evidence `blocks.csv` body lookup |
| `read_state` | `cardano-cli query ledger-state` JSON (modulo field-name remap) |
| `read_tip` / `follow_tip` | `cardano-cli query tip` + `blocks.csv` |
| `submit_tx` | tx must appear in `blocks.csv` within `1/f` × tx_zoo timeout; cardano-bp must adopt the block |
| `read_mempool` | `cardano-cli query tx-mempool info` and `dugite_mempool_tx_count` |
| `watch_mempool` / `watch_tx` | mempool delta from two consecutive `read_mempool` calls |

This corner of the system is also a natural place for a "Path D" extension to the parity matrix: every tx-zoo tx should be submittable via gRPC as well as N2C, with identical accept/reject decisions.

## Coverage debt — items still missing

Tracked openly in this file so it's visible to every invocation of the skill:

- [ ] Bidirectional submission for 11-mempool tests
- [ ] Negative-tx symmetry for cardano-bp ingestion (mirror 08r to path C)
- [ ] Plutus script-eval-failure negative cases
- [ ] Replay attack negative test
- [ ] Equivocation / double-forge negative test
- [ ] Mempool-size-DoS positive measurement (eviction order parity)
- [ ] Tx-size lattice with `maxBlockBodySize` boundary check
- [ ] Gov-lifecycle E2E for action classes other than `ParameterChange`
- [ ] Per-observer mempool-event capture into `tx-flow.csv`
- [ ] Round 4 (adversarial & stress) as a standard preset
- [ ] Enable `--rpc-port` on the devnet's `run.sh` for both dugite-bp and dugite-relay
- [ ] `09-cli-parity/` scripts for the un-covered cli subcommands (see CLI surface section)
- [ ] `09-cli-parity/errors/` for cli error-path parity
- [ ] `rpc/` subdirectory implementing the 20+ UTxO RPC parity scripts
- [ ] Path-D extension to the parity matrix: every tx-zoo tx also submitted via gRPC `submit_tx`
- [ ] Adversarial-RPC stimuli (oversized message, stream-disconnect, concurrent duplicates, per-IP flood)

When closing any of these, update both the skill and the corresponding evidence schema in `cross-validation.md`.
