# Skill self-audit — 2026-05-28 (run from main @ dc4cab1ab)

Discovered during the kickoff for our first post-update Round 1.

## Confirmed-working

- `setup.sh`, `run.sh`, `stop.sh`, `soak.sh`, `verify.sh` — all present and executable.
- `tx-zoo/run-all.sh` — accepts category-name args (`./tx-zoo/run-all.sh 01-bookkeeping 04-stake ...`), so the bidirectional re-run command in SKILL.md is structurally correct.
- `tx-zoo/09-cli-parity/run.sh` exists and produces `evidence/<ts>/cli-parity.csv`.
- `tx-zoo/cross-validate-cli.sh` exists and produces `tx-zoo/state/cross-validate.csv`.
- `protocols/run.sh` exists.
- `chaos/*.sh` scripts exist.
- `LD_CARDANO_BP_SOCK`, `LD_RELAY_SOCK`, `LD_DUGITE_BP_SOCK` are defined in `testnet/local-devnet/lib/common.sh`.
- `ZOO_SOCKET` env-var is read by `tx-zoo/lib/tx-zoo-common.sh` and overrides the default relay socket.
- Binaries built fresh from HEAD `dc4cab1ab`.

## Issues found in the just-updated skill

### I1 — `LD_*` env-vars aren't auto-exported in user shells
SKILL.md tells the user to run:
```bash
ZOO_SOCKET="$LD_CARDANO_BP_SOCK" ./tx-zoo/run-all.sh 01-bookkeeping 04-stake 06-proposals 08-negative
```
But `LD_CARDANO_BP_SOCK` only exists if `lib/common.sh` has been sourced into the current shell. If not, `ZOO_SOCKET` expands to "" and `tx-zoo-common.sh` falls back to the default `LD_RELAY_SOCK` (path B), making the "bidirectional re-run" a no-op silently.

**Mitigation for this run**: prepend `. ./lib/common.sh` once after `cd testnet/local-devnet` so all subsequent invocations inherit the vars. Long-term: update SKILL.md to either source common.sh in the command or use the literal `state/cbp.sock` relative path.

### I2 — `results.csv` has no socket column → off-diagonal parity criterion is mechanically unverifiable
The new Round 1 PASS criterion "zero off-diagonal cells in `tx-zoo/state/results.csv`" cannot be evaluated as written, because `results.csv` is `ts,name,status,detail` — no record of which `ZOO_SOCKET` produced each row. Re-running with a different socket just appends new rows that look indistinguishable from the first batch.

**Mitigation for this run**: snapshot `results.csv` after each socket-batch (`results.B.csv`, `results.C.csv`), then build a manual parity-matrix.csv by joining on `name` and asserting `status_B == status_C`. We can also save `tx-zoo/state/logs/` per batch so error messages survive.

Long-term: either teach `run-all.sh` to record `ZOO_SOCKET` as a column, or have the bidirectional wrapper write `parity-matrix.csv` directly.

### I3 — `parity-matrix.csv` is referenced but no script writes it
`test-methodology.md` says "tabulate both outcomes into evidence/<ts>/parity-matrix.csv". Nothing creates that file today. Same fix as I2.

### I4 — `analyze-evidence.sh` invocation path in SKILL.md is wrong from inside `testnet/local-devnet/`
SKILL.md Round 1 (line ~88) is:
```bash
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/$(ls -t evidence | head -1)
```
When the current directory is `testnet/local-devnet/` (as Round 1 prescribes via the earlier `cd`), this path resolves to `testnet/local-devnet/.claude/...` which doesn't exist. Correct form is `../../.claude/skills/devnet-validate/scripts/analyze-evidence.sh`.

### I5 — `health-probe.sh` log auto-detection is repo-root-relative
The probe's log auto-locate uses `testnet/local-devnet/logs/...` which only resolves when invoked from the repo root, not from inside `testnet/local-devnet/`. From the devnet dir, pass `--log logs/dugite-bp.log --relay-log logs/dugite-relay.log --cardano-log logs/cardano-bp.log` explicitly.

### I6 — `tx-zoo/10-gov-lifecycle/run.sh` and `tx-zoo/11-mempool/run.sh` don't exist
SKILL.md's "v2 capability matrix" implies category-runner scripts. They don't exist; only the individual `10a..e` and `11a..c` scripts. The way to invoke is `./tx-zoo/run-all.sh 10-gov-lifecycle 11-mempool` (orchestrator-mediated).

## Action

Run Round 1 now using the mitigations above, then file follow-up PRs for I1/I2/I3/I4/I5/I6 once we have empirical data.

## Empirical findings — first probe run @ Round 1 boot

### A1 — Probe bug: net-stall check is role-blind
On a sole forger, `dugite_blocks_received_total` is 0 by design (the BP produces blocks, doesn't receive them). The probe's step-9 net-stall check on a BP-role node falsely reports stall whenever the slot advances. Confirmed values:
```
dugite-bp(12798)  : blocks_received_total=0  blocks_forged_total=21  blocks_applied_total=0
dugite-relay(12799): blocks_received_total=21 blocks_forged_total=0  blocks_applied_total=21
```
**Fix**: when `IS_BP=1` AND `peers_outbound` is the producer's only stream, treat advance of `blocks_forged_total` as the liveness signal in lieu of `blocks_received_total`. Alternatively, document that the probe should be run against the relay port (`--port 12799`) for the net-stall predicate.

### A2 — Probe bug: ERROR/panic grep is too loose
The grep is `grep -iE 'ERROR|panicked|stale intersection|KES sign failure'`. The `-i` flag makes it match the lowercase substring `error=...` inside a benign WARN-level DNS-config message, producing a false positive. Confirmed match:
```
2026-05-28T04:30:32.xxxx  WARN  ... error=protocol error: failed to parse nameserver address: ...
```
**Fix**: drop `-i` (tracing log levels are uppercase) and anchor with whitespace: `grep -E ' ERROR | panicked | stale intersection | KES sign failure '`. Or match the tracing prefix specifically.

### A3 — Coverage gap: cardano-bp does not expose Prometheus on the devnet
`cardano-bp.config.json` declares only `TraceOptionMetricsPrefix` (a label) — no `hasPrometheus` block. `run.sh` does not bind a metrics port for cardano-bp. So the probe's step-11 Haskell-tip parity via `:12800` can never succeed in the current devnet topology, even though `monitoring.md` claims `:12800 — EKG-backed Haskell metrics`.

**Fix options**:
1. Add `hasPrometheus: ["127.0.0.1", 12800]` to `config/cardano-bp.config.json` and restart cardano-bp.
2. Fall back to socket-based Haskell parity: scrape `cardano-cli query tip --socket-path /tmp/ld-501/cbp.sock` and compare. Slower but always works.

Recommend option (2) as the default (no devnet-config churn) plus option (1) as opt-in via a `--haskell-prometheus` flag on the probe.

### A4 — P0: legacy `Trace*` names never appear in cardano-node 11.0.1
cardano-node 11.0.1 uses the new tracer (cardano-tracer) which emits structured JSON with `ns:"..."` namespace keys, not the legacy `Trace*` prefixes the skill greps for. Observed in cardano-bp.log:

| Legacy name (skill greps for) | Actual 11.0.1 namespace |
|---|---|
| `TraceAdoptedBlock` | `ChainDB.AddBlockEvent.AddedToCurrentChain` |
| `TraceForgedInvalidBlock` | `ChainDB.AddBlockEvent.AddBlockValidation.InvalidBlock` (+ `Forge.Loop.ForgedInvalidBlock` on a forger) |
| `TraceDownloadedHeader` | `ChainSync.Client.DownloadedHeader` |
| `TraceMempoolAccepted` | `Mempool.AddedTx` |
| `TraceMempoolRejectedTx` | `Mempool.RejectedTx` |

Files affected:
- `scripts/health-probe.sh:332` (adoption count) and `:394` (invalid-block count) — both checks are no-ops today.
- `scripts/analyze-evidence.sh:99,105` — the "CRITICAL: TraceForgedInvalidBlock" anomaly never fires.
- `references/monitoring.md`, `references/health.md`, `references/cross-validation.md` — all reference legacy names.

**Severity**: In the present state, p3-equivalent checks ("cardano-bp accepted every dugite-forged block") **always pass** even if Haskell genuinely rejects something. Round 1 PASS for this skill is currently a false-positive predicate.

**Fix applied this run**: probe + analyze-evidence.sh updated to match BOTH legacy and new names (forward-compatible). Reference docs still need updating.

## DUGITE BUG CAUGHT BY ROUND 1 — `TimeTranslationPastHorizon` block-level rejection

**Block**: hash `69a0bd4010c11da7186f7d7135fd20b6cfc4c31984141094737eb49744afd437`, slot 380, block_no 185, contains 1 Plutus tx.

**Forge timestamp**: `2026-05-28T04:37:17.670758Z` by dugite-bp (`TraceForgedBlock`).

**Relay action**: dugite-relay accepted and chain-extended to it at 04:37:17.790Z. Continued to forge on top — chain advanced to block 278 / slot 557 by capture time.

**cardano-bp action**: BlockFetch'd the block at 04:37:17.794Z, ran validation, REJECTED at 04:37:17.796Z with `ChainDB.AddBlockEvent.AddBlockValidation.InvalidBlock`. Stayed at slot 379 / block 184 from that moment, rejecting every subsequent block that builds on slot 380. **Chain divergence is permanent for this run.**

**Reason** (extracted from the JSON error):
```
ExtValidationErrorLedger →
  HardForkLedgerErrorFromEra (Conway era) →
  BlockTransitionError → LedgersFailure → LedgerFailure →
  ConwayUtxowFailure → UtxoFailure → UtxosFailure →
  CollectErrors → BadTranslation →
  BabbageContextError → AlonzoContextError →
  TimeTranslationPastHorizon
```

The Plutus script-context builder (`Cardano.Ledger.Plutus.TxInfo`, `epochInfoSlotToUTCTime`) attempted to translate `SlotNo 979` to UTCTime via `epochInfoSlotToRelativeTime` and threw `PastHorizon` — the era summary's last bounded era ends at `SlotNo 800`.

**Hypothesis**:
- One of the tx-zoo Plutus scripts (most likely `03h-reference-script`, `03i-reference-input`, or `03j-collateral-consumed` — all running at ~04:37:11–18Z) built a Plutus V1/V2 tx with a `txValidityIntervalUpperBound` that maps to a slot past the era's safe-zone horizon.
- dugite's Plutus context builder must NOT be throwing on past-horizon while Haskell's does. **dugite is producing valid blocks that contain Plutus txs with past-horizon validity, but Haskell rejects them at block-apply time.**
- Either (a) dugite's safe-zone horizon is wider than Haskell's, OR (b) dugite's `epochInfoSlotToUTCTime` equivalent doesn't enforce the horizon, OR (c) dugite's Phase-1 validity-interval check doesn't reject upper-bounds past the horizon when it should.

**Severity**: P0 — produces blocks Haskell cannot apply. Would cause a hard fork on any public testnet with Plutus tx flows.

**Next steps**:
1. File a tracking issue with the full error and block hash.
2. Reproduce minimally: feed the offending tx CBOR to dugite-ledger's apply step and observe the Phase-1 / Phase-2 verdict vs the Haskell behaviour.
3. Identify the tx in block 185 by extracting the body via `cardano-cli` query block or via dugite's chainstore.

## Metric coverage gap exposed by this round

The user observed two oddities in dugite-monitor's view of the devnet:

### A5 — Methodology gap: single-forger topology never exercises BP's block-reception path
The devnet runs cardano-bp as a non-forging relay. dugite-bp is the sole producer. Therefore:
- `dugite_blocks_received_total` is structurally **0** on dugite-bp throughout every Round 1.
- This means the BP's "receive a block from upstream, apply it" code path is NEVER tested by this devnet.
- For real BP↔BP cross-validation we'd need either:
  - cardano-bp configured with forging keys (true BP-pair, dual-producer with stake split), or
  - a second dugite-relay-fed forger, or
  - an explicit acknowledgement in the skill that the devnet doesn't validate this dimension.

**Recommendation**: add a `bp-pair` opt-in mode that hands cardano-bp a stake-delegated forging set (cardano-bp can already use Praos forging — we already have the keys & shelley-genesis allocates them via `keys/pool2/`).

### A6 — Metric semantics: peer arithmetic doesn't balance on the relay
Observed at end-of-Round-1 (chain divergence still in effect; but the arithmetic should hold regardless):

| Node | connected | hot | warm | cold | inbound | outbound | duplex |
|---|---:|---:|---:|---:|---:|---:|---:|
| dugite-bp | 2 | 1 | 1 | 0 | 1 | 1 | 2 |
| dugite-relay | 3 | 2 | 1 | 0 | 2 | 2 | 3 |

Invariants that DO hold:
- `connected == hot + warm` ✓ on both
- `cold == 0` ✓ in static-topology devnet

Invariants that do NOT hold:
- `connected == inbound + outbound` ✓ on bp (1+1=2), ✗ on relay (2+2=4 ≠ 3). Duplex peers are double-counted in both directional gauges, but `connected` is the unique-peer count. The skill should explicitly assert `inbound + outbound - duplex_overlap == connected` and document the relation, OR the metrics need a docstring fix in `crates/dugite-node/src/metrics.rs`.

### A7 — `peers_hot` semantics confusing on BP
A BP with the relay as its only peer reports `hot=1, warm=1` with `connected=2`. Reading `connection_lifecycle.rs` + `peer_manager.rs` would reveal whether the BP+relay duplex link surfaces as ONE peer (hot for one direction, warm for the other) or as TWO peer entries (one per direction). Operator confusion is real either way. The probe should assert in the static devnet that **every connected peer becomes hot within `N` slots**, with a documented timeout, and document the multiplicity rule.

### Skill currently checks only ~15 of the ~52 metrics dugite-monitor displays
dugite-monitor consumes 52 distinct `dugite_*` metrics; the probe validates ~15. Missing entirely:
- `dugite_block_number`, `dugite_chain_density`, `dugite_diffusion_mode`, `dugite_protocol_major_version`
- `dugite_committee_*`, `dugite_drep_*`, `dugite_proposal_count`, `dugite_pool_count`, `dugite_pool_id_info`
- `dugite_delegation_count`, `dugite_vote_delegation_count`, `dugite_gov_dormant_epochs`
- `dugite_mem_*`, `dugite_cpu_percent`, `dugite_disk_*`
- `dugite_uptime_seconds`
- `dugite_pparam_*` (5 metrics)
- `dugite_transactions_rejected_total`, `dugite_leader_checks_total` (partial)
- `dugite_peer_handshake_rtt_ms_*`, `dugite_peer_rtt_*` (histogram + bands)

The probe should at minimum **read** every one of these and assert sanity (non-negative, non-monotonic-decreasing for counters, within expected ranges for known-static gauges).

## A8 (P1) — Dugite peer-state counters diverge from Haskell `cardano-node` semantics

**Status: PARTIALLY RESOLVED 2026-05-28**

After re-verification against the canonical Haskell source via the
`cardano-haskell-oracle`, the three divergences in this section
reduce to **ONE real bug** and **TWO false-positives**:

### Divergence 1 (missing `PeerCooling`) — REAL — IMPLEMENTED

Added to dugite in commit `<this commit>`:
- new `PeerState::Cooling` variant (between `Cold` and `Warm`)
- `demote_to_cooling()` for the canonical Hot/Warm → Cooling transition
- `cooling_to_cold()` for the TerminatingState → TerminatedState analogue
- `demote_to_cold()` preserved as fast-path (also accepts `Cooling → Cold`)
- `is_cooling_or_cold()` helper for Haskell's `updateUnlessCoolingOrCold`
- 3 unit tests verifying re-promotion is blocked during Cooling, only
  valid sources for the transition fire, and the cooling → cold completion
  unblocks future promotions

Follow-up wiring to call `demote_to_cooling()` at every disconnect site
(instead of the current `demote_to_cold()`) and to fire `cooling_to_cold()`
on connection-manager `TerminatedState` events is a separate change.
The state machine is now ready for those callers.

### Divergence 2 (peer-counter overlap) — FALSE POSITIVE

Verified via `cardano-haskell-oracle` against
`ouroboros-network/framework/lib/Ouroboros/Network/ConnectionManager/Core.hs`
lines 208–221. Haskell's `connectionStateToCounters` for
`DuplexState`:

```haskell
DuplexState {}                        -> fullDuplexConn
                                       <> duplexConn
                                       <> inboundConn
                                       <> outboundConn
```

A single `DuplexState` connection DOES contribute to ALL of
`fullDuplexConns=1, duplexConns=1, inboundConns=1, outboundConns=1`
simultaneously, regardless of `Provenance`. Dugite's behavior in
`crates/dugite-network/src/connection/state.rs:183-189` is correct.
The audit's claim of "double-counted" is wrong; this is the
Haskell-canonical model.

### Divergence 3 (hot/warm/cold dedup by peeraddr) — FALSE POSITIVE

Verified via `cardano-haskell-oracle` against
`Ouroboros.Network.PeerSelection.Governor.Types` line 634:
`activePeers :: !(Set peeraddr)` where `peeraddr` is the full
`RemoteAddress`/`SockAddr` (IP + port), not just IP. Dugite's
`HashMap<SocketAddr, PeerInfo>` already keys by `SocketAddr` (IP +
port). The audit's claim of double-counting was based on a
misunderstanding of `peeraddr`'s definition. Dugite is Haskell-faithful.

### A8 conclusion

Original audit P1 reduced to one real change (PeerCooling, now implemented).
The two "false-positive" divergences should NOT trigger any code change — the
existing dugite behavior IS the canonical Haskell behavior. The 2026-05-28
verify against the live Haskell source closes the loop.

---



Verified against ouroboros-network `Ouroboros.Network.PeerSelection.Types` + `Ouroboros.Network.ConnectionManager.Types` via the cardano-haskell-oracle.

### Divergence 1: missing `PeerCooling` state

Haskell `data PeerStatus = PeerCold | PeerCooling | PeerWarm | PeerHot` ([ouroboros-network/PeerSelection/Types.hs](https://github.com/IntersectMBO/ouroboros-network/blob/master/ouroboros-network/lib/Ouroboros/Network/PeerSelection/Types.hs)). Dugite has only Cold/Warm/Hot. `PeerCooling` is the intermediate state after a clean disconnect (TCP TIME_WAIT analogue) — the governor must not repromote a peer until it reaches `PeerCold`. Without this state, dugite may repromote too eagerly under churn.

### Divergence 2: peer-counter overlap

In Haskell `ConnectionManagerCounters`, **each connection counts once in inbound XOR outbound by `Provenance`** (whichever side initiated the TCP). A `DuplexConn` (both sides active on a single socket) increments `fullDuplexConns` but contributes to inbound OR outbound based on its `Provenance` field, not both.

Dugite's `inbound_peer_count` / `outbound_peer_count` (in `crates/dugite-node/src/node/networking.rs:779-808`) include `ConnectionState::DuplexConn` in BOTH branches:
```rust
pub fn outbound_peer_count(&self) -> usize {
    self.conn_states.values()
        .filter(|s| matches!(s, OutboundIdle | OutboundUni | OutboundDup | DuplexConn))
        .count()
}
pub fn inbound_peer_count(&self) -> usize {
    self.conn_states.values()
        .filter(|s| matches!(s, InboundIdle | InboundState | DuplexConn))
        .count()
}
```
Empirically observed on our 3-node devnet:
- relay: inbound=2, outbound=2, full_duplex=1, connected=3 → `inbound+outbound=4`, with 1 DuplexConn double-counted.
- BP: inbound=1, outbound=1, full_duplex=0, connected=2 → no DuplexConn, no double-count.

The Haskell-canonical invariant is `connected = inbound + outbound` (no overlap). Dugite needs `connected = inbound + outbound - full_duplex` to make the arithmetic work — but this is a semantic mismatch, not a Haskell-compatible aggregation.

### Divergence 3: hot/warm/cold count peer-entries vs unique addresses

Haskell's `numberOfActivePeers` counts entries in the `activePeers :: Set peeraddr` (unique by address). Dugite's `count_by_state(PeerState::Hot)` iterates `self.inner.peers` which may have multiple entries per address (one per direction). Empirical:
- User expectation (Haskell semantics): BP=1 hot, relay=2 hot, cardano-bp=1 hot.
- Dugite observed: BP=hot=2, relay=hot=3 (after warmup).

The dugite value is double the expected because each Duplex peer is counted twice in the hot-state aggregation (once per direction). For Haskell-compat the dugite gauge should deduplicate by `peeraddr`.

### Recommended fix priorities

| Fix | Severity | Effort |
|---|---|---|
| Track `Provenance` on each connection; count once by initiator side in `inbound_peer_count`/`outbound_peer_count` | P1 | Medium — touches `peer_manager` + tests |
| Deduplicate hot/warm/cold counters by `peeraddr` | P1 | Low — `count_by_state` should fold by address first |
| Add `PeerCooling` 4th state + governor transition | P2 | Medium |
| Emit Haskell-compatible metric names (`peerSelection.Hot`, `connectionManager.fullDuplexConns`, ...) as aliases | P3 | Low |

These should be tracked as a single epic: "Haskell parity for peer-state metrics".

## A9 — `metric-audit.sh` invariant validates dugite's current (divergent) semantics

The script's assertion `peers_inbound + peers_outbound - conn_full_duplex == peers_connected` **passes today** because of A8.divergence-2. Under Haskell-correct semantics it would be `peers_inbound + peers_outbound == peers_connected`, with `conn_full_duplex` orthogonal. Once A8 is fixed in dugite, the audit assertion will need to flip.

To make the audit forward-compatible, replace the strict invariant with a hint: assert `peers_inbound + peers_outbound - conn_full_duplex == peers_connected` (today's behavior) AND emit a warning when `conn_full_duplex > 0` noting that the dugite semantics differ from Haskell.

## User's empirical expectation captured

| Node | Expected hot (Haskell semantics) | Observed dugite | Divergence factor |
|---|---:|---:|---:|
| dugite-bp | 1 | 2 | 2× |
| dugite-relay | 2 | 3 | 1.5× |
| cardano-bp | 1 | (n/a — Prometheus not exposed) | n/a |

The factor depends on how many of a node's peers are full-duplex. Add an audit assertion that **after** dugite's metrics align with Haskell, dugite-bp must report `peers_hot == 1` and dugite-relay must report `peers_hot == 2` in this devnet topology.
