# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Goal
Implement a 100% compatible Cardano node in Rust. Target full compatibility with cardano-node (Haskell).

## Development Methodology: Ralph Loop
Follow the Ralph autonomous development loop:
1. **Assess** — Evaluate current state, identify highest-impact gaps
2. **Implement** — Build the next feature/fix
3. **Test** — Run `cargo test --all`, ensure zero failures
4. **Verify** — Run `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`
5. **Commit** — Commit and push to remote with descriptive message
6. **Repeat** — Continue to the next iteration

## Build & Test Commands

The top-level `justfile` wraps the common dev commands. Pick whichever feels more natural — both shapes are equivalent.

```bash
# Just recipes (preferred when in a fresh shell)
just check          # full CI gate: fmt-check + clippy + build + test + test-doc
just build
just test           # cargo nextest run --workspace
just test-doc
just clippy
just fmt-check      # cargo fmt --all -- --check  (fix with: just fmt)

# Direct cargo (still works for narrow invocations)
cargo build --all-targets
cargo nextest run --workspace
cargo nextest run -p dugite-ledger                    # single crate
cargo nextest run -p dugite-ledger -E 'test(name)'    # single test
cargo test --doc
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --release
```

The storage layer is pure Rust with no system dependencies. cardano-lsm (used for the on-disk UTxO set) supports `--features io-uring` for async I/O on Linux.

## Divergence Fixes — Non-Negotiable Process

Any change made to address a divergence from cardano-node MUST be:

1. **Grounded in canonical Haskell source.** Quote the IntersectMBO code that
   defines the behaviour, with file and revision, so dugite implements it *the
   way Haskell implements it* — not a change that merely produces the same
   number.
2. **Analysed INDEPENDENTLY by a separate agent on the FABLE model.** Not by
   the context that found the divergence and already has a favourite
   explanation. Hand it the EVIDENCE — the bisection, the amounts, the
   boundaries — and the question, withholding the hypothesis so the analysis is
   not anchored.

**Never invent a change to satisfy a number.** A constant, a fudge, or a
plausible rule chosen because it closes a gap is worse than the gap: it puts a
fabricated value on a consensus path and makes the next investigation start
from a false premise.

The justification is measured, not theoretical. The mainnet treasury
investigation raised **ten** hypotheses, nine about dugite's ledger, and every
one was wrong; the tenth — that the ORACLE was wrong — was correct. Several of
the nine would have produced a confident wrong change to a consensus path. And
#1072's fix, written from a correct diagnosis, still introduced **two new
consensus bugs three lines from the one it fixed**.

## Hard Requirements
- **Zero warnings** — All code must compile with `RUSTFLAGS="-D warnings"`
- **Clippy clean** — `cargo clippy --all-targets -- -D warnings` must pass
- **Formatted** — `cargo fmt --all -- --check` must pass
- **Tests pass** — All tests must pass before committing
- **CI green** — GitHub Actions pipeline must be passing
- **Commit regularly** — Push changes to remote after each successful iteration
- **Focused commits** — Stage explicit filenames (no `git add -A` / `git commit -a`). The pre-commit hook warns when staged paths span more than two crates; set `DUGITE_PRECOMMIT_STRICT=1` to make that fatal (recommended for autonomous agent runs).

## Architecture

16-crate Cargo workspace under `crates/` (19 workspace members including
`xtask`, `tests/conformance`, `tests/golden`). Dependency flow:

```
dugite-node (binary: main node, config, pipelined sync, Mithril import, block forging)
├── dugite-network (Ouroboros mini-protocols, N2N/N2C multiplexer, pipelined client)
├── dugite-consensus (Ouroboros Praos, chain selection, epoch transitions, VRF leader check)
├── dugite-ledger (UTxO set via UTxO-HD, tx validation, ledger state, certificates, rewards, governance)
├── dugite-storage (ChainDB = ImmutableDB append-only chunk files + VolatileDB in-memory)
└── dugite-mempool (thread-safe tx mempool with input-conflict checking and TTL sweep)

dugite-cli (binary: cardano-cli compatible, 38+ subcommands)
dugite-monitor (binary: terminal monitoring dashboard, ratatui-based, real-time metrics)
dugite-config (binary: interactive TUI configuration editor with tree navigation, inline editing, diff view)

dugite-serialization (CBOR encode/decode — in-house multi-era decoder + minicbor)
dugite-crypto (Ed25519, VRF, KES, text envelope)
dugite-primitives (core types: hashes, blocks, txs, addresses, values, protocol params, all eras)
dugite-uplc (in-house UPLC CEK machine; 100% conformant as of v1.7.0)
dugite-lsm (LSM-tree on-disk storage for UTxO-HD)
```

### Key Traits & Abstractions
- **`BlockProvider`** (storage) — trait used by N2N server for block serving
- **`TxValidator`** (ledger) — trait used by N2C server for Phase-1/Phase-2 tx validation before mempool admission
- **`ChainDB`** — wraps ImmutableDB (append-only chunk files) + VolatileDB (HashMap), handles rollback and volatile→immutable flush

### Wire Format
- All Cardano wire-format compatibility via the in-house multi-era CBOR decoder under `crates/dugite-serialization/src/decode/`
- `Transaction.hash` is `blake2b_256(raw_body_cbor)` over the bytes captured by `KeepRaw::parse_with` during decode
- Two DIFFERENT protocol-param wire shapes, do not conflate them:
  - `ProtocolParamUpdate` (tx-body key 6 / gov ParameterChange) is a SPARSE
    integer-keyed CBOR map, keys 0-37 (not JSON strings)
  - N2C `GetCurrentPParams` (LSQ tag 3) replies with a POSITIONAL
    `array(31)` per Haskell `ConwayPParams` — see
    `dugite-node/src/node/n2c_query/encoding.rs`

## Key Patterns
- `ChainSyncEvent::RollForward` uses `Box<Block>` to avoid large enum variant size
- Invalid transactions (`is_valid: false`): collateral consumed, collateral_return added, regular inputs/outputs skipped
- Batch block storage: `add_blocks_batch()` for efficient batch writes to ImmutableDB
- ChainDB write happens BEFORE ledger apply to prevent divergence on failure
- Epoch transitions use mark/set/go snapshot model with reward distribution from "go" snapshot
- Governance ratification: DRep/SPO/CC voting thresholds vary by action type (CIP-1694)
- Pipelined ChainSync runs an in-house state machine for maximum throughput; default pipeline depth 300 (configurable via `DUGITE_PIPELINE_DEPTH`)
- Ledger-based peer discovery: extracts SPO relay addresses from `pool_params` when past `useLedgerAfterSlot`
- DatumOption / Nullable wrappers: see `crates/dugite-serialization/src/decode/primitives.rs` for the in-house equivalents (`Nullable`, `MaybeIndef`, `KeyValuePairs`)
- 28-byte hash types (DRep keys, pool voter keys, required signers) must be padded to 32 bytes via `Hash28::to_hash32_padded()` — do not use `Hash<32>::from()` directly on 28-byte hashes

## Current Focus

### 2026-08-10 — mainnet Shelley→Mary is byte-exact; the endgame is a 40h sync

**Epochs 208-273 vs cardano-streamer: 66 paired, 12,408 leaf comparisons,
61 divergent — and ZERO of them are real dugite value divergences.** 50 are the
ORACLE's own defect (#1077) and 11 are definitional. Report:
`reports/mainnet-exactness/report-alonzo.json`.

Getting there took eight defects, and **six were in the measuring apparatus,
not the node.** That ratio is the finding of this wave.

| stage | divergent |
|---|---:|
| start | 961 |
| #1074 (ledger, CONSENSUS) | 376 |
| 4 comparator defects | 311 |
| 2 dump defects | 66 |
| the last-boundary overwrite | **61** — 11 dugite's |

**#1074 (CONSENSUS, mainnet-only) — a statement ORDER was the bug.** Upstream's
`startStep` builds the pulser and its `FreeVars` — including `fvAddrsRew` — in
ONE expression (PulsingReward.hs:89-212), so no pulse can precede the capture.
dugite split that across two statements in `apply.rs` and ran them backwards, so
the trigger block's FIRST pulse folded `ceil(N/8640)` queue-head credentials with
`rupd_addrs_rew` still `None` — and the predicate `is_none_or` reads a missing
set as "everyone is registered". A credential deregistered before epoch 233's
mark was paid a member reward `rewardOnePoolMember` never creates; unregistered
at apply, it went to TREASURY where Haskell leaves it in `deltaR2`, i.e.
RESERVES. 70,698 / 163,916 / 62,209 lovelace, treasury high.

*Invisible everywhere except mainnet*: `hardforkBabbageForgoRewardPrefilter`
forgoes the prefilter at pv>=7, so permissive IS correct on devnet, preview and
preprod. And the differential proptest ran `registered: |_| true, pv_major: 11`
— `ctx.registered` had never once been consulted by any test in the tree.

*Decomposing an atomic upstream expression creates a consensus surface with no
upstream counterpart to check against.* No Haskell line says "capture before
pulse"; the question cannot arise there. Test drives real `apply_block` calls —
a unit test on either statement is green under the bug.

**Three wrong diagnoses preceded the right one**, including two of mine, and the
Allegra-fork alignment of the window is a COINCIDENCE (the window is that
credential's deregistration and its exit from the GO snapshot). The independent
Fable pass is what killed them.

### The measuring apparatus, and why fixing it mattered

Four comparator defects, all one shape: **a digest bypasses every rule above
it.** The large-map/list branches hashed raw values, so relay canonicalisation
never reached a map big enough to be digested; `EXCLUDED_SUFFIXES` was honoured
only by `walk`; `_canon` rendered `0.0` where the other side wrote `0`; and a
SCHEMA GAP (`rc = 2`) was unconditionally overwritten by a divergence
(`rc = 1`), so a gap only ever surfaced when nothing else diverged — backwards,
since a gap means a field was never compared at all.

Two dump defects: `epochFees` reported a total the dump DRIVER accumulated
rather than the ledger's `ssFee` (62 of 64 epochs "divergent", measuring
nothing), and IPv6 relays were hex-dumped in wire form.

**Clearing the noise is what EXPOSED #1078** — 726 pools differed only by relay
spelling, so a real difference in that same field could never have been seen.

**`dump-snapshot` overwrote each run's LAST boundary snapshot** with in-progress
state, producing three false signals at once, one of which I filed as a ledger
defect (#1079, retracted). Settled by running past the boundary: the same epoch
then matches the oracle byte-for-byte.

### Open, with mechanisms established

- **#1071** — `nesRu` wire arms. Per-block pulsing IS done; what remains is
  relocating `rewLikelihoods`/`rewLeaders` to the mark (ledger + SNAPSHOT), the
  three encoder arms, and a LIVE transition-timing validation on a devnet seeded
  with ~120 credentials. Not started: `tests/fixtures/nesru/README.md` is
  explicit that emitting `Complete` where cardano-node emits `Pulsing` replaces
  an honest gap with a confident wrong answer (#979).
- **#1077** (oracle-side) — cardano-streamer's `sumRs` folds every `Set Reward`
  element and never applies `filterRewards`, so its `totalDistributed`
  over-counts at pv<=2. Window is exactly epochs 212-236 and the two diffs
  cancel to zero in all 25. Fix: `sumRewards rewProtocolVersion`.
- **#1081** — a dugite-written ImmutableDB is UNREADABLE by cardano-node.
  cardano-node rolls chunks on a fixed 21600-slot range; dugite rolls one per
  write-open (`next_chunk = current_epoch.max(last + 1)`, and on a
  Mithril-bootstrapped DB the clamp always wins). Measured: imported chunks are
  a uniform ~2 MB, dugite's are 235 MB / 146 MB / 59 MB. Isolated by pointing
  the same binary at a clone of a genuine cardano-node DB, which reads fine.
  Costs dugite too — recovery granularity is coupled to chunk size, and this DB
  holds a 52 MB `.chunk.orphaned`. **Gate exists**:
  `scripts/validation/check-immutable-chunk-invariant.py` checks
  `slot // chunkSize == chunkIndex` straight off the secondary index — no node,
  seconds per run. cardano-node's DB PASSES (2357 chunks); dugite's preview
  fails 14 of 27578 and preprod 19 of 5930, **tail only**, and the drift is
  BIDIRECTIONAL (preview 27564 holds a block BELOW its range), so a fix that
  merely bounds chunk size would leave the low-side violations. Its own control
  run caught a bug in the checker: `blockOrEBB` is a UNION and holds the EPOCH
  NUMBER for a Byron EBB, so `slot == idx` is the EBB case — without the control
  it would have condemned every correct database. TWO of my hypotheses on this
  issue were falsified by measurement; both are recorded there so they are not
  re-derived.
- **#1067/#1068/#1070/#1072/#1074/#1078** are fixed in-tree on
  `worktree-nonmyopic-1067`, verified, and carry `Closes` trailers that fire on
  MERGE — they are still open on GitHub until then, which is the honest state
  rather than a checkmark ([[feedback_closed_issue_is_not_evidence_work_landed]]).
  **#1080 is CLOSED** (closed by hand after its four items were all done).

### Mainnet tip coverage — how to finish it

`scripts/validation/mainnet-exactness-run.sh` is now the whole pipeline: wait →
stop → clone → dugite replay → cstreamer replay → diff, each step skippable
(`SKIP_WAIT/SKIP_STOP/SKIP_CLONE/SKIP_DUGITE`). Set `TARGET_EPOCH` to the tip.

**The chain is cloned, not copied.** dugite's ChainDB reads cardano-node's
chunk format natively, and the volume is APFS, so `cp -c` gives dugite an
independent writable view for ~0 disk (measured: 13 GB cloned, free space
unchanged). It must be a CLONE, not hardlinks — dugite's open path may
reconcile the index or quarantine a chunk (#926-#929), and those repairs would
land on the oracle's database.

**Validated end to end already**: dugite replayed the cloned cn chain,
6,547,320 blocks, Byron→Alonzo, in 27 minutes with zero read errors.

**The 10.7.1 cardano-streamer port is NOT needed** — mainnet is PV11 and the
already-validated 10.6.2 branch pins `cardano-ledger-conway 1.20.0.0`, whose
released changelog bumps `ProtVerHigh ConwayEra` to 11. Parked at `d0ebc95`.
The oracle binary is pinned at `oracle-bin/cstreamer-10.6.2` with its sha256,
because both branches build to ghc-9.6.5 and share one dist-newstyle directory.

**Still owed before the tip run can be trusted**: `conwayGov` schema
reconciliation. dugite emits `drepDistr`/`proposals`/`enactedRoots` at top level;
cardano-streamer nests a different set under `conwayGov`. Until they agree, ~140
Conway epochs would compare VACUOUSLY and the final answer would be a false
PASS. It needs real Conway oracle output to align against — the preprod shortcut
for that is blocked by #1081.

**Sync status**: cardano-node at epoch 313/648 (Alonzo), ~1.2 %/hour and
decaying, so **40-45+ hours remain**. Measure the rate over hours before
quoting an ETA; a single window is how "4.0 days" and "~11 hours" both got said
in this campaign and both were wrong.

### Superseded: the v2.8.0 release wave (2026-08-08)
**v2.8.0 (2026-08-08) — the NonMyopic record becomes real, and the RUPD pulser
grows up.**
**RE-SYNC RELEASE: SNAPSHOT_VERSION 37 -> 38**, so existing DBs replay chunks on
first restart. Closes **#1067**, **#1072** (CONSENSUS), and the pulser-alignment
programme's Phases 0/1/2/4 plus Phase 3's ledger half. Open: #1068, **#1070**,
**#1071** (needs the `Pulsing` wire arm — see below), #1008, the Dijkstra set,
and one filed `PulsingSnapshot` divergence.

### 2026-08-09 — #1070 and #1068 closed; the mainnet oracle is cardano-streamer

**#1070** — `dugite-cli query ledger-state` sent Shelley tag 4
(`GetProposedPParamsUpdates`), so it returned `82 04 81 a0` — four bytes of the
wrong record — from every node. The mislabel lived in the METHOD NAME, so the
call site read correctly; renaming to `query_proposed_pparams_updates` means
nothing is called `ledger_state` and the wrong method cannot be reached by
name. `encode_shelley_query` is split out so the bytes a named query puts on
the wire can be pinned with no socket: nothing about the REPLY is malformed, so
only an assertion on what goes OUT can see this.

**#1068** — UTxO queries read the live ledger while every other query read the
pinned acquisition snapshot. `UtxoSet` is a plain `HashMap`, so there is no
cheap snapshot at mainnet scale; the pinned view is RECONSTRUCTED by undoing
the `LedgerSeq` deltas newer than the acquired point. Two orderings decide
correctness and each is silently wrong in one direction — within a delta
restores must precede removals, across deltas iteration must be newest-first.

**The #1068 tests did not bound what they claimed, twice.** The first drove
`undo_into` with a hand-built slice, so reversing the PRODUCER left it green.
Adding a producer test then exposed a SECOND ordering site: `Origin` returns
early with its own `.rev()`, so the specific-point branch's reversal was still
untested and disarming it also stayed green. Both now driven, with three
deltas — with two, a reversal bug and a correct implementation differ only by
which single element is dropped.

### Mainnet byte-exactness — cardano-streamer, NOT Koios (#1073)

`/Users/michaelfazio/Source/cardano-streamer`, branch `10.6.2-dump-snapshot`.
**Both sides dump at the SAME instant** — verified, not assumed: cstreamer's
`dumpEpochSnapshots` fires at `siFinal` when `isFirstSlotOfNewEpoch`, using
`swbNewExtLedgerState` (post-block state of the first block of the new epoch);
dugite's `run_dump_snapshot` writes post-`apply_block` when
`current_epoch > last_epoch`. Both label with the NEW epoch.

**cstreamer emits NOTHING for Byron** — `buildSnapshotJson` returns `Nothing`,
because `ChainAccountState` is introduced BY the Shelley translation. Byron
epochs are ORACLE-SILENT, never divergent. Back-projecting a Shelley shape onto
Byron is what disqualified Koios.

**`rupdNext` was ALWAYS NULL and nobody noticed** — the dump read
`pending_reward_update`, which has no writer, so the single most important
field of a reward cross-validation dataset compared vacuously at every epoch.
It also emitted 3 of upstream's 6 fields and published the NET signed
`delta_reserves` under the name `deltaR1`, the GROSS expansion.
`forced_reward_update` now forces a complete fold, as cstreamer forces its own
pulser. Valid at a boundary dump point because every input `startStep` freezes
already holds its final value once the epoch's first block lands.

Comparator: `scripts/validation/diff-cstreamer-dumps.py`, era-aware, bisects to
the FIRST divergent epoch per field, counts leaf COMPARISONS rather than rows,
and exits 3 rather than printing PASS when it compared nothing. Self-tested in
four directions; the negative run found two defects in the script itself.

dugite's side: `db-mainnet-avvm` replays genesis→271 (5,851,768 blocks) in
~7 min. Oracle side needs a real cardano-node mainnet ImmutableDB
(`db-cn-mainnet`) synced past 273 — cstreamer reads the immutable chain, which
lags the tip by k.

### The pulser-alignment programme

Design + full status table: `docs/superpowers/specs/2026-08-08-pulser-alignment-design.md`.

**#1072 (CONSENSUS)** — NEWEPOCH applies a reward update only when a pulser
exists. `nesRu` is `SNothing` whenever no block landed strictly after the
epoch's `4k/f` mark, and the `SNothing` arm is `pure es`: no deltaR, no deltaT,
no rewards, **no `ssFee` drain**. dugite applied one unconditionally. The fee
drain sitting OUTSIDE the gate was a second defect found by review, and its test
was structurally blind (fixture `epoch_fees` = 0 against a `saturating_sub`).

**Phase 0 — the number that gates the rest.** ~2.55 s for the mainnet-scale
reward fold inside ONE boundary block, linear in credentials
(`state::rupd_work_measurement`, `--release`, `#[ignore]`d). That overruns a 1 s
slot by 2.5x, so incremental pulsing is justified. **The first measurement said
0.29 s and would have retired the work**: a fixed 1000 ADA per credential made
per-pool `sigma` ~5e-10, floored `maxPool'` to zero, and dropped every MEMBER
reward, so the fold returned one entry per pool and the timing described a loop
over 50 pools while claiming to describe 1000 credentials. An assertion that
the fold rewarded at least half its input is the only reason the number is
right.

**Phase 3's ledger half.** The reward fold is now CREDENTIAL-major via
`PoolRewardInfo` (upstream's own decomposition), and production runs THROUGH
`RewardFold` with a single maximal pulse — no batch path beside a pulse path to
drift. The gate is a differential property, `fold_incremental(any pulse_size)
== fold_batch`, proptested with owner and zero-stake credentials seeded so the
skip paths land AT chunk boundaries. The work queue is sorted: upstream's
balance is a `Set` consumed in `Ord` order, and an unsorted queue would make the
folded/pending split differ across a restart, so two nodes computing identical
rewards would still disagree about `nesRu`.

**Phase 1b — `fvTotalStake`.** `pending_avvm_return` did not only correct the
expansion; it corrected `total_stake`, which is **sigma's denominator**, so it
reached `maxPool'`, every member share and every likelihood. Deleting it while
freezing only the monetary terms would have left the pot pre-AVVM and the
DISTRIBUTION post-AVVM — worse than the patch. `MonetaryStep` now freezes
`total_stake` as Haskell's `FreeVars` does.

**Phase 4 CLOSED as YAGNI, measured not asserted.** `ConwayGovState[6]` sampled
from both nodes across a full epoch is `array(2)` with no sum tag, every time:
upstream's encoder force-completes, so `DRPulsing` never reaches the wire.
Implementing it would add ledger, snapshot and rollback surface for zero
observable difference.

**Still open in Phase 3**: ONLY the `nesRu` wire arms (#1071). The arms could
not ship first — decoding the captured `Pulsing` record shows `balance` (work
remaining) and `RewardAns` (answer so far) are live fold state, so a node
computing at the boundary has nothing to encode from. The plan that put the
arms in Phase 2 was wrong.

**CORRECTION (2026-08-09): per-block pulse scheduling IS DONE.** Both this
file and the design spec's §Phase-3 body said it was still open while the
spec's own status table said DONE. The tree agrees with the table:
`apply.rs:585` freezes the monetary step at the 4k/f mark and `apply.rs:614`
calls `pulse_rupd_member_fold` on EVERY block. `RewardFold::is_done()` and
`remaining()` both exist, the latter documented as "directly encodable as the
wire arm's tag-258 set". So the three-way state the arms need
(`SNothing` / `Pulsing` / `Complete`) is fully available and #1071 is now a
pure encoder job plus a live transition-timing validation — not architecture.
Found by diffing the claim against the tree, which is the standing rule for a
checkmark ([[feedback_closed_issue_is_not_evidence_work_landed]]).

### Three wrong readings of a real artefact, all caught the same way

Worth more than any individual correction: each was plausible, each survived
review, and each fell to **decoding or sampling the whole thing rather than its
head**.

1. The `Pulsing` arm was scheduled before incremental pulsing. Reading only the
   record's head made the `Pulser` look like a summary.
2. The tag-258 set inside the `Pulser` was called "the fold's work queue". It is
   `fvAddrsRew` — **140** entries where the real queue holds **19**. Both are
   credential sets inside the pulser; the COUNTS separate them (140 tracks live
   registrations, 19 is what the GO snapshot carries in epoch 2). The test
   asserting it PASSED and would have kept passing — only the meaning was wrong.
3. Phase 4's "no wire form" was asserted from the types. True, as it turned out,
   but it took a capture to know.

### #938's threshold, still not fully swept — found by byte-diffing a live node

`costModels` arrays were framed DEFINITE where cardano-node 11.0.1 frames them
indefinite (`98 a6` vs `9f … ff`). `variableListLenEncoding` switches above
`lengthThreshold = 23`, and a real cost model is 166+ parameters, so every
`costModels` reply differed. #938 swept this exact threshold through the block
and transaction encoders and missed the LSQ pparams path.

It survived every release gate because **both framings decode to the same
list** — cardano-cli renders identical JSON, cli-parity passes on
`protocol-parameters`, and no suite comparing VALUES can see it. Only a raw byte
diff against a running Haskell node shows it. Found while investigating Phase 4,
which is the argument for running an investigation whose expected answer is
"no change needed".

### Rollback: the RUPD freeze pair is now atomic

`rupd_monetary` had no `LedgerDelta`, so a rollback regressed the flag while the
frozen terms stayed newer. It was SAFE — by a two-step argument about which
fields are read when, which is exactly what #985 was. Both are now written by
the same branch. The delta field is `Option<Option<MonetaryStep>>`: collapsing
"not recorded" into "genuinely absent" is #1028's defect verbatim.

### #1067 — both halves of `NonMyopic` were invented, not just the named one

dugite emitted `array(2) [map(0), 0]` and tracked per-pool `Likelihood` nowhere.
The issue title names `likelihoodsNM`, but an EMPTY map is CORRECT until the go
snapshot first carries pools — cardano-node itself reports `{}` at epoch 2. The
unconditionally-wrong field was the other one: **`rewardPotNM`**, non-zero from
the first RUPD onward and live-visible the whole time.

`newLikelihoods` is now built inside `compute_reward_update` (mirroring Haskell
building it inside `startStep`) and folded through `updateNonMyopic` on the way
out as `RewardUpdate.nonMyopic`, which `applyRUpd` installs as the new
`esNonMyopic`.

**Five things decide byte-exactness, and only three were written down before.**

1. `l x = n * log x + m * log (1 - t*x)`, `m = slotsPerEpoch - blocks`. NOT
   `n * log (x*t)` — which is what dugite's own notes said.
2. Precision is mixed on purpose: `l x` in f64, `realToFrac` narrows ONCE to
   f32, then decay-multiply / zip-add / min-subtract all stay f32.
3. `<>` normalises, so `mempty <> newPerf` min-subtracts even a pool's FIRST
   epoch. The raw likelihood is never what gets stored.
4. `sigma` is the UNCAPPED relative stake over `totalStake` (circulating
   supply) — not `min sigma z0`, and not over `totalActiveStake`, which is
   `sigmaA` and belongs to `mkApparentPerformance` alone.
5. **`realToFrac` on a `Rational` rounds the exact ratio ONCE.** mainnet
   `totalStake` (~3.7e16) exceeds 2^53, so `num as f64 / den as f64` rounds
   twice and lands a ulp out. This one was not on anybody's list.

**The trap that cannot be tested end-to-end.** The wrong `n*log(x*t)` differs
from the correct formula by `n*log t` — a CONSTANT in `x`. `normalizeLikelihood`
subtracts the minimum, so a constant offset cancels at every step, including
across decayed history. Measured end-to-end the two formulas agree to within one
f32 ulp (3.8e-06 after one epoch, 1.5e-05 after two). **A tolerance-based test on
the stored value passes under the bug.** The assertion has to sit on the RAW
likelihood, where they differ by ~30. This is the mirror image of
[[feedback_red_proven_unit_test_bounds_the_function_not_the_system]]: sometimes
the unit test is the only thing that CAN go red.

`allPoolInfo` covers EVERY pool in the go snapshot's pool set. The
reward-distribution loop `continue`s past zero-block and pledge-unmet pools, so
the likelihood set is built by its own iteration — piggy-backing it onto that
loop would silently drop exactly the pools the `Left` branch exists to serve.
And `updateNonMyopic` maps over `newLikelihoods`, so a pool absent this epoch is
DROPPED, not carried forward decayed.

**Scope was wider than the issue twice over.** There were **two** hardcoded
encoder sites — `encode_debug_epoch_state` (tag 8) as well as tag 12 — so fixing
the named one would have left the other lying. And the Mithril ancillary import
`skip_cbor_value`'d the record entirely, so every mainnet/preview/preprod
bootstrap started with no history and converged over ~20 epochs while looking
authoritative. Both fixed.

**The wire shape is pinned to a capture, and the capture is reproducible.**
`cargo run -p dugite-network --example capture_non_myopic -- <sock> <magic>`,
run at epoch 3 or later — capturing earlier returns `a0` and would "confirm" the
hardcoded empty map by accident. Two of the three framing decisions are what
reading `deriving EncCBOR` gets wrong: the map is DEFINITE (2 <= 23) while each
100-element `Likelihood` is INDEFINITE (100 > 23, #938), and every `LogWeight`
is `0xfa` float32 at every magnitude. `likelihoods` is held sorted by pool id
because upstream's `VMap` is key-ordered and dugite's source is a `HashMap` —
without the sort the same node emits different bytes for the same state on
consecutive runs.

**cardano-cli's JSON renders `exp(LogWeight)`** — it shows `1`, `1.4e305` and
`+inf` for an f32 field. Use it to decide WHEN to capture, never to check WHAT
was captured.

Live proof, epoch 3 devnet: the whole `esNonMyopic` record — all 100 log-weights
for both pools plus `rewardPotNM` — is byte-identical to cardano-node's, and
`09w-ledger-state` compares 48 key paths identical with the `likelihoodsNM`
exclusion REMOVED.

### #1072 — dugite applied a reward update at EVERY boundary (CONSENSUS, CLOSED)

Haskell's NEWEPOCH applies one only when a pulser exists:

```haskell
-- NewEpoch.hs:161, identically ConwayNewEpoch.hs:172
es' <- case ru of
  SNothing -> pure es        -- no deltaR, no deltaT, no rewards, no fee drain
  SJust p@(Pulsing _ _) -> ... completeRupd p ... updateRewards
  SJust (Complete ru')  -> updateRewards es eNo ru'
```

`nesRu` is `SNothing` whenever no block landed strictly after
`epoch_first + 4k/f`. dugite applied one regardless, so pots moved on one side
only and stayed wrong. The window is 80 slots on the devnet — SHORTER than the
chaos suite's SIGKILL and Round 3's 90 s outage, so it is reachable on the
networks this repo tests, and permanent once it happens.

Found by an adversarial review of the pulser spec, which had (correctly) ruled
out the *reserves* route and then concluded "not a live divergence". The
reserves analysis was right; a different route through the same gap was not.

**The fix's first attempt introduced TWO NEW consensus bugs on the identical
trigger**, both caught by a second adversarial review and both worth
remembering:

* **F1 — the `ssFee` drain was left outside the gate.** `deltaF` is a FIELD of
  `RewardUpdate` (`PulsingReward.hs:276`) applied only by `updateRewards`, so
  Haskell never touches `utxosFees` on the SNothing arm. dugite drained it
  anyway: lovelace left the fee pot without entering any other pot, and the
  next reward update was short by the same amount. The existing test could not
  have caught it — the fixture's `epoch_fees` was 0 and `saturating_sub` masks
  the subtraction. **A RED proof only bounds what you assert**; mine asserted
  reserves and stopped three lines above the bug.
* **F2 — no `LedgerDelta` representation**, so rollback restored the ANCHOR's
  value. #985 again, in a change whose own spec called rollback "the largest
  omission". The audit guard that should have caught it destructured
  `LedgerState` with `epochs: _` — one level above where the field landed. It
  is now EXHAUSTIVE over `EpochSubState`; note `{ field: _, .. }` does NOT
  work, because the `..` still matches anything new.

Also from that review: the import path started the flag `false` on a false
premise (an imported ledger past the mark corresponds to `nesRu = SJust`); the
test-only `state/epoch.rs` helper applied the RUPD ungated (#977/#1015's third
copy, and gating it turned 4 tests red — proof they had been asserting
always-apply); Conway had ZERO coverage of the gate; and `last_applied_rupd`
kept the previous boundary's update on the SNothing path.

**Proven live, not only by unit test.** `live-1072-differential.sh` stops the
sole forger before the mark and restarts after the epoch end, so no block lands
in the window and the boundary fires on the first block back. Both nodes must
apply nothing — and do. It passes for the right reason: a reward update there
would have moved ~1.8e13 lovelace of expansion plus a tau cut, and treasury
stayed 0.

**Still open (F5)**: a tick that crosses a boundary AND lands past the new
epoch's mark makes Haskell freeze over PRE-rotation `bprev`/`ssStakeGo`/`ssFee`.
A bool cannot express that; only `Pulsing(RewardSnapShot, Pulser)` can.
Documented at the field, deferred to the pulser program's Phase 2.

### #1071 — `nesRu` is hardcoded SNothing, and every prior gate passed by luck

Found BY this release's gate. `NewEpochState[4]` is `enc.array(0)`, while
cardano-node populates the pending `RewardUpdate` from `4k/f` into each epoch
until the boundary — **80 of 400 slots on the devnet, 20% of the time**. A
one-shot `09w-ledger-state` therefore diverges ~1 run in 5; v2.7.1 recorded
`ledger-state` as `23 equal / 0 divergent` because it sampled outside the
window.

**This is #977's `futurePParams` shape**: the interesting state is an epoch
PHASE, so a point sample almost always observes the boring value and reports
agreement. The root cause is architectural, not a missing encoder arm — dugite
has **no RUPD pulser**; it computes the reward update inline at the boundary, and
`pending_reward_update` is `take()`n at two sites while nothing ever writes it.
So `array(0)` faithfully renders dugite's state; the state is what differs.

Not attempted here: a pulser means deciding byte-exactly WHEN reward inputs
freeze, which is the surface #966/#988/#949/#991 kept finding consensus bugs in,
plus another SNAPSHOT bump. `09w` carries a narrow `possibleRewardUpdate`
children exclusion citing it — parent still required, everything else strict,
`ledger-state` still OUT of `KNOWN_DIVERGENCES`.

### #1070 — `dugite-cli query ledger-state` queries the wrong tag

`N2CClient::query_ledger_state()` sends Shelley tag 4, which is
`GetProposedPParamsUpdates`. Against a real cardano-node it returns four bytes,
`82 04 81 a0`. The query carrying `NewEpochState` is tag 12, added as
`query_debug_new_epoch_state` for the capture tool. Node-side dispatch was
always correct, and `09-cli-parity` runs `cardano-cli` against both sockets so
it structurally cannot see this. Not fixed in-release: it changes a user-visible
CLI surface and wants its own round.

### QA — v2.8.0 (SHIPPED)

devnet-validate standard preset, **4/4 rounds PASS**
(`reports/devnet-validate/v2.8.0.json`) vs cardano-node 11.0.1, strict,
`gate_integrity.admissible = true`, zero missing evidence.

- 623 canonical blocks, 0 invalid forges, 0 critical anomalies, **0 ERROR lines
  on any node in any round**
- tx-zoo 154 scripts: 151 pass / **0 fail** / 3 state-skip / 0 ENV-SKIP
- bidirectional parity **99/99, 0 OFFDIAG, 0 CLASSDIFF**
- cli-parity 23 EQUAL / **0 DIVERGENT** / 26-26; adversarial N2N 26/26;
  UTxO RPC 27/27; chaos 5 rows / 0 FAIL / 0 ENV_SKIP
- tip-parity **100% in all four rounds** (24/24, 24/24, 176/176, 12/12)
- treasury+reserves **byte-exact** vs cardano-node after the RUPD boundary
- futurePParams 489 compared / 0 diffs; ratify-state 489 compared / 0 diffs /
  1 `PLAN_APPLIED` — both reached their non-vacuous condition

**The two tx-zoo failures were VERIFIED, not pattern-matched.** Both are
`01a-simple-pay` from Round 2's 20-second trickle racing itself on one address —
the same count and script as previous releases, which is exactly what makes it
easy to wave a real one through. The two have DIFFERENT reasons (`Input
conflict: input already claimed by mempool tx` and `InputNotFound`), one cause
seen at two moments: a competitor still in the mempool versus one already in a
block. The decisive evidence is p3 — **29 txs, 24 accepted / 5 rejected, all
three nodes agreeing**. A dugite defect appears as disagreement, never as a
shared verdict.

**Preprod soak, 60 min.** The 37 -> 38 re-sync quarantined the existing
snapshot as `.v37-unreadable` and rebuilt the ledger by chunk replay at
65-81k blk/s, reaching tip at `delta=0` vs Koios. `esNonMyopic` at real scale:
**546 pools, every Likelihood exactly 100 long**, `rewardPotNM`
28118802294396. The 8-pool gap against the *post-rotation* `go` snapshot is the
rotation itself — `compute_reward_update` runs BEFORE SNAP (`conway.rs:630` vs
`:729`), so the stored likelihoods come from the pre-rotation `go`, which held
exactly 546. Zero pools appear in `likelihoodsNM` that are absent from `go`.

**Two harness defects, both mine, both the family the gate exists to catch.**
Round scripts relaxed `errexit` BEFORE sourcing `lib/common.sh`, which
re-enables it (#1044's trap), so a round aborted at its first non-zero exit
instead of running its remaining suites. And the Round 2 samplers wrote to
`evidence/current/` rather than the round's pinned directory — making their
CSVs indistinguishable from suites that never ran. **The generator caught that
one and refused to report a PASS (exit 3)**, which is the check working; the
artifacts were relocated into the round that produced them rather than waved
through with `--no-strict`.

**Coverage gap named rather than papered over**: the devnet gate never
round-trips a NON-EMPTY `non_myopic` through a snapshot save/load, because the
rounds with real likelihoods (1, 2) never restart and the restart round (3) is
pre-RUPD. Layout is covered by `snapshot_format_hash_stability` +
`fixture_populates_every_snapshot_field`; the live path is covered by the
preprod restart check.

### Superseded: v2.7.1 (2026-08-07) — the #1057 P0, plus four `query ledger-state` wire defects.

Drop-in from v2.7.0, **SNAPSHOT unchanged at 37**, so no re-sync.

**#1057 is CLOSED** — the P0 that stranded a block producer. It needed **THREE**
fixes, not one, and each was only visible once the one above it was fixed; a
fourth guard came out of an adversarial review. The second and third were found
by asking what cardano-node does rather than what would make the symptom go
away, and the long-standing written prediction that this required
`init_fresh_ledger` was WRONG (see below).

**`query ledger-state` was undecodable in its entirety** and cardano-cli hides
that — it exits 0 and prints a raw-CBOR dump instead of JSON, so the query had
been broken for every operator while looking merely verbose. Four defects:
`SnapShot` arity 3 -> 2; its contents (the delegation map MERGED into each stake
entry, and the pool map's values are a 10-field snapshot aggregate, not
`PoolParams(9)`); `blocksBefore`/`blocksCurrent` swapped; and `nesPd` answered
from the LIVE pool set instead of the frozen `set` snapshot. Consolidating the
last one onto the shared encoder then exposed a fifth: `pdTotalActiveStake` was
written unclamped, and it is a `NonZero Coin` upstream — so tag 36, the only arm
a V21+ client can reach, could emit a value that fails to decode.

The shape could NOT be read off cardano-ledger: 11.0.1 pins CHaP at
index-state 2026-05-02 and master's nearest record has an extra field in a
different order. It was established by proxying the running node's own N2C
socket and is now pinned to those bytes —
`snap_shot_bytes_match_a_cardano_node_11_0_1_capture`. See
[[reference_running_node_is_the_wire_oracle]] for the method and its two traps.

**Open at the time (superseded — see Current Focus above):**
- **#1067** (`reachable:live`) — `NonMyopic.likelihoodsNM` is hardcoded EMPTY and
  dugite tracks per-pool `Likelihood` nowhere (8 mentions in the tree, all
  comments or the two hardcoded encoder lines). NOT consensus: it feeds the
  non-myopic reward ESTIMATE and pool ranking, not block validity. Deliberately
  NOT rushed into v2.7.1 — it is a float-encoded per-epoch accumulator needing
  byte-exactness and a SNAPSHOT_VERSION bump, which turns a drop-in patch into a
  re-sync, and a half-right typed arm is worse than an honest gap (#979).
  `09w-ledger-state` excludes only that subtree's CHILDREN, citing the issue;
  the parent is still required and everything else is compared strictly.
- **#1068** — UTxO queries read the LIVE ledger while every other query reads the
  pinned LSQ acquisition snapshot, so one `MsgAcquire..MsgRelease` session can answer
  from two ledger points. Upstream cannot express this: `answerQuery` runs against one
  acquired `ExtLedgerState`. Window is one refresh interval (~1 s) at tip, non-consensus,
  query-only. Not fixed here because `QueryHandler::acquire` is a SYNCHRONOUS trait
  method in dugite-network, so the fix is a cross-crate change, and pinning a UTxO view
  is not a cheap `Arc` clone under UTxO-HD. Found by reading the query path while
  attributing a tx-zoo failure — it was NOT the cause of that failure.
- **#1008** — 82 cardano-cli commands, a backlog by design, referenced by
  `cli-surface-known-gaps.txt` so the gate stays honest.
- Dijkstra: #1011/#1014/#1029/#607, pre-activation by definition.

**Harness defects fixed this wave, all of the same family** — a check that
reports success while measuring nothing, or an assertion that never runs:
`wait-tip-parity.sh` existed and was called by nothing (the `ALL_CATEGORIES`
shape, #969) and now lives inside `soak.sh`; `XV_RC` was captured and never
checked, so a cross-validate failure could not fail the gate; three of six
11-mempool scripts selected a shared UTxO with no `zoo_wait_mempool_quiet`,
which is #918's mechanism left unfixed; and cross-validate now submits a
rejected tx to cardano-node before blaming dugite. Two of those were mine, added
in this same wave.

### QA — v2.7.1 (SHIPPED)

devnet-validate **standard** preset, **4/4 rounds PASS**, strict,
`gate_integrity.admissible = true`, `missing = []`
(`reports/devnet-validate/v2.7.1.json`) vs cardano-node 11.0.1.

- 1055 canonical blocks, **0 invalid forges**, 0 critical anomalies
- bidirectional parity **96/99 classified identically, 0 OFFDIAG**, 0 class-diff
  (1 tracked known-diff, 2 stateful-excluded)
- tx-zoo Round 1 **151/154 pass, 0 fail**, 0 ENV-SKIP; cli-parity **23 EQUAL /
  0 divergent / 0 ENV-SKIP**; cross-validate 7/7; adversarial N2N, UTxO RPC, chaos clean
- Round 2 pots **byte-exact** vs Haskell: treasury `7173850901297`,
  reserves `5992798100783733`
- **both epoch-boundary samplers reached NON-VACUITY** — 264 non-empty
  `nextRatifyState` samples, 146 `PotentialPParamsUpdate` samples, 0 diffs each. That is
  the condition that matters; #992's hardcoded empty pulser satisfied the vacuous
  version of it in every previous gate
- restart round rejoined 27 -> 57, 0 ERROR on any node

The 3 tx-zoo failures are all `01a-simple-pay` from the Round 2 trickle racing itself.
**Verify the REASON, do not carry it forward** — confirmed for this run from Round 2's
logs: 5x `Input conflict: input already claimed by mempool tx` and zero
fee/value/input errors.

**Preprod steady-state soak, 60 min at tip on the same binary — PASS on all seven
predicates.** 12 usable samples, worst tip delta vs Koios **8 slots**, min peers 8,
**0 ERROR**, RSS 3873 -> 4107 MB (6%, flat). Two are positive evidence rather than
absent noise: **0 `LedgerSeq was incoherent`** means #985's startup re-anchor fired (the
devnet is structurally blind to that defect, so preprod is the only place to see it), and
**0 genesis-range declines** confirms #1057's wedge does not arise on a synced node.
Catch-up was exact — tip at slot 130419082, delta 0 vs Koios.

**Five harness defects surfaced during this release, and THREE were introduced in this
same wave.** All of one family: a check reporting success while measuring nothing.
- both epoch-boundary samplers wrote to `$LD_EVIDENCE/current/`, which nothing reads, so
  they ran, passed and printed real numbers while a STRICT report called them "absent in
  EVERY round" — and the apparent escape is `--no-strict`, which voids the whole gate
- `10e-assert-enactment`'s timeout was a constant encoding `epochLength=400`, so it gave
  up ~3 min before the enactment it awaited once Round 2's overlay changed that parameter
- the gate driver discarded the governance zoo's exit code, so the suite carrying
  Round 2's headline capability could not fail the round (the `XV_RC` hole again)
- `04i` reported an unanswered query as a missing pool, having discarded exit code AND
  stderr, so a correct node looked like a ledger bug
- the chain-density predicate was a flat +/-20% band, i.e. a ~1-in-8 coin flip at a
  54-slot window; now the p99.9 binomial interval, which is STRICTER where the sample
  supports it (Round 2: [0.461,0.539] vs [0.400,0.600])

**Two of those were caught only by the strict report generator**, and only because
`--no-strict` was refused. A gate whose evidence manifest is not enforced cannot tell a
suite that passed from one that never ran.

### #1057 — a node holding its own chain now adopts one diverging at genesis (CLOSED)

**THREE independent defects, one symptom** — and each was only visible once the one
above it was fixed, which is why this took four attempts. A node whose local chain must be replaced
from Origin never adopted the canonical chain: BlockFetch declined the peer's block 0
forever, the ledger never advanced, ChainSync's forecast-horizon park timed out, and
every peer was disconnected in an endless reconnect loop. It stranded a block producer
(dugite-bp forged blocks 0..8 on Origin, then froze at its own tip while the Haskell
chain reached 240+, dying at the *same* header slot on every peer and every reconnect).

Both halves are now aligned with cardano-node, and the alignment is what made the fix
tractable — every earlier attempt guessed at the mechanism instead.

**Half 1 — the live path. Upstream's condition is on the ANCHOR, not the tip.**
`Paths.hs::isReachable`:

```haskell
(_, StoppedAtGenesis)
  | AF.anchorIsGenesis (AF.anchor chain) -> Just (ChainDiff rollback' …AnchorGenesis…)
  | otherwise                            -> Nothing
```

with `rollback' = rollback + length chain` — a full rollback of the whole current
chain. dugite's gate is `ledger_can_reach_origin` (the LedgerSeq anchor IS Origin and
the window is coherent), which is that condition derived rather than invented. An
earlier version tested "the ledger is at Origin" and was measured too narrow: a node
that had just recovered re-adopted a stale 10-block chain from its sibling and was
wedged again 30 seconds later against the canonical 155-block chain.

BlockFetch and chain selection read the SAME `Arc<AtomicBool>`. They briefly disagreed
— storage on `ledger_can_reach_origin`, BlockFetch still on `ledger tip == Origin` — so
the node accepted the fork switch and was never given the blocks to switch to.
`find_rollback_n`'s `target == anchor` case is what makes the rollback executable.

**Half 2 — the restart path. dugite snapshotted the wrong state.** cardano-node
snapshots the LedgerDB *anchor* (`LedgerDB/V2.hs`):

```haskell
let pruneStrat = LedgerDbPruneBeforeSlot (slot + 1)
(slot,) <$> (duplicateStateRef $ anchorHandle $ snd $ prune pruneStrat lseq)
```

then `pruneToImmTipOnly`s on init and re-pushes the volatile chain as individually
rollback-able states. So with nothing flushed, upstream's anchor IS genesis and
`anchorIsGenesis` still holds after a restart.

dugite snapshotted the LIVE TIP and then `reset_anchor`ed onto the replayed tip, so a
restart came up unable to roll back *anywhere* and the wedge outlived the process. Two
changes, both scoped to "nothing flushed to the ImmutableDB" — the regime in which the
whole chain is inside k of genesis, so genesis is the only correct anchor:

- `snapshot_is_a_valid_anchor` rejects a volatile-tip snapshot in that regime, so the
  ledger starts at genesis. Bounded: under k blocks to replay.
- replay pushes every block through the LedgerSeq delta path instead of bulk-advancing,
  and the post-replay `reanchor_ledger_seq` is SKIPPED when it did (`reset_anchor`
  clears the window, which would discard exactly what the delta replay just built).

**Half 3 — the ledger declined the rollback because Origin has TWO spellings.**
Found only by running the round with halves 1 and 2 in place: storage switched the
chain, chain selection asked for a rollback to Origin, and the ledger refused —

```
INFO  Chain selection: fork switch at live tip — rolling back ledger to intersection
        intersection=0000…0000 intersection_slot=0 rollback_count=6 apply_count=7
ERROR Rollback target outside LedgerSeq volatile window AND no canonical snapshot
        available. Aborting rollback; ledger state preserved.  rollback_slot=0 ledger_slot=48
WARN  Fork rollback failed; skipping fork replay
```

`find_rollback_n` compared points with `==`. The LedgerSeq anchor is `Point::Origin`;
chain selection builds `Point::Specific(intersection_slot, intersection_hash)`, so a
genesis intersection arrives as `Specific(0, ZERO)`. Same chain position, different
enum variant, unequal — so it fell through to the snapshot slow path, found nothing at
or before genesis, and aborted, leaving the node on its dead fork with storage already
switched. That is the EXACT fingerprint recorded for the reverted unconditional fix,
which is why that attempt looked like a storage/ledger design problem: it was this
one-line comparison all along.

Fixed with `Point::denotes_origin()` (in dugite-primitives, documented with both
spellings and why `Specific(0, ZERO)` is unambiguous — a real block at slot 0 carries a
real hash), used in `find_rollback_n`.

**This is why the earlier four RED-proven unit tests were worthless.** They all drove
`Point::Origin` — the form the LEDGER uses and the form production never sends on this
path. The new test drives BOTH spellings, plus two negatives (a real hash at slot 0, a
ZERO hash above slot 0) so the predicate cannot start swallowing unrelated targets.

**And the fix needs no ledger re-initialisation at all.** The prediction carried in the
issue, in CLAUDE.md and in the round's own header for three attempts — that this needed
a full `init_fresh_ledger` on rollback-to-Origin — was WRONG. The live log shows the
real mechanism:

```
LedgerSeq rollback: restored ledger via in-memory volatile window
  rollback_slot=0 rolled_back_blocks=6 new_tip_slot=0
```

No snapshot reload, no re-initialisation. The volatile window merely had to be
REACHABLE: anchor at Origin (halves 1 and 2) and the comparison able to see it (half 3).

**The marker mechanism was DELETED, not kept.** An earlier mitigation persisted
`<db>/genesis-divergence-detected` and the next start discarded the node's own chain.
Upstream does no such thing: `isReachable` returns `Nothing`, chain selection keeps
what it has, and no marker, operator restart or re-sync is involved. It was a
dugite-only invention presented as a fix. What remains is a throttled WARN per declined
genesis range plus one latched ERROR naming the remedy — reaching it now means the fork
really is deeper than k, which upstream cannot switch across either.

**Traps worth keeping.**

- **A gate can read a value nobody sets.** `set_ledger_can_reach_origin` is only called
  from `reanchor_ledger_seq` — the exact call half 2 skips. Without an explicit publish
  the flag keeps its initial `false`, BlockFetch declines every genesis range, and the
  restart path is dead while looking implemented.
- **Why the UNCONDITIONAL two-line fix was wrong.** Teaching both layers to accept
  `prev_hash == Hash32::ZERO` with no ledger precondition was attempted and REVERTED:
  storage emitted a genesis-rooted `SwitchPlan`, chain selection asked the ledger to
  roll back to Origin, and `sync.rs` aborted with *"Rollback target outside LedgerSeq
  volatile window AND no canonical snapshot available"*, leaving dugite-bp at block 4
  while the network was at 84. The wedge MOVED to the ledger rollback. Unit tests on
  `switch_chain` in isolation all passed, because nothing drove the storage plan through
  the ledger — a RED-proven unit test bounds the FUNCTION, not the SYSTEM.
- **Two published findings had to be RETRACTED**: "a dugite BP cannot mint the first
  block of a chain" (the harness omitted the forging keys — a keyless BP is
  indistinguishable from a gate-blocked one) and a PASS on tip-hash equality (dugite's
  fork had WON; cardano adopted it). `relay=?` from an unreadable tip was also read as
  "did not converge" for three runs — **unmeasured is not failed**.
- `Hash32::ZERO` IS dugite's canonical Origin parent (encoder → CBOR `null` == Haskell
  `PrevHash = GenesisHash`), and upstream needs no guard at all because `AnchorGenesis`
  is a first-class constructor of `Anchor blk`.
- **An existing test PINNED the storage half**:
  `test_switch_chain_reachable_via_immutable_anchor` used `h(0)` as its "immutable tip",
  and `h(0)` is `[0u8; 32]` — the genesis sentinel. Re-pointed at a non-ZERO anchor.

**The reproduction is `testnet/local-devnet/genesis-fork-round.sh`, and four of its own
defects are the reason it took so long to trust:**

1. **Depth asymmetry is REQUIRED.** With both islands running up symmetrically dugite's
   fork wins chain selection and the path under test is never entered (measured: 4-vs-5
   and 9-vs-9 both converged with 0 switches). `LD_POOL2_STAKE_PCT=85` makes cardano's
   chain longer by construction (~0.45 vs ~0.10 leader probability per slot) instead of
   fighting the race.
2. **A slot gap between two LIVE islands cannot grow.** A slot number is wall-clock, so
   each live tip tracks the current slot and their difference stays near zero. Four runs
   chased that impossible target. The gap accrues only against a genuinely frozen node.
3. **It measured the wrong node.** The wedged node is dugite-**relay** — it peers with
   cardano-bp and must replace its chain. An earlier gate compared cardano-bp's slot
   against dugite-**bp**'s and reported INCONCLUSIVE "slot gap 17" on a run whose relay
   was frozen 302 slots back. Step 4 once read the wrong node's log for the same reason.
4. **A topology reload does NOT close an established connection.** SIGHUP-ing
   `localRoots = []` stops future dials only; the hot session survived and dugite-bp fed
   the relay from block 6 to 32 while the round believed it was frozen. The freeze is
   `SIGSTOP` on the producer — reversible, and no restart, so the relay's anchor stays
   at Origin.

Also: the horizon breach is the SECONDARY condition, downstream of the defect. The
primary one is just that chain selection prefers a chain sharing only genesis
(`GF_MIN_BLOCK_LEAD`). A round demanding the breach up front skips the step it exists to
exercise. And cardano's chain must out-length **dugite-bp's**, not just the relay's: the
relay's chain is a PREFIX of dugite-bp's, so adopting that one is a plain roll-forward.

**The allowlist SHRANK when the fix landed**, which is the direction that matters. While
the node could not adopt, `genesis-fork-round.allowed-errors` excused the wedge's own
diagnostics as "the round's evidence"; left in place, the round would pass whether the
node adopted or wedged — a check that cannot fail.

### #1015 — Babbage folded extraEntropy; Praos has 2 terms, not 3

`babbage.rs` delegated wholesale to `ShelleyRules::process_epoch_transition`,
which folds the TPraos TICKN 3-term nonce. Babbage is **Praos**:
`tickChainDepState` folds two terms and `extraEntropy` does not exist for Praos
at all (`Praos.hs` has zero occurrences; `hkdExtraEntropyL =
notSupportedInThisEraL` for Babbage/Conway/Dijkstra). Dormant — extra_entropy is
`ZERO` by the time Babbage runs on every known network — but nonce evolution has
no self-correcting mechanism, so a chain carrying a non-neutral value across
Alonzo→Babbage would split via the VRF leader schedule forever.

THREE implementations existed and the duplication WAS the mechanism: Conway had
the right formula, Babbage reused the wrong era's code. Now one
`compute_epoch_boundary_nonce` + an `EpochNonceMode` derived from `ctx.era` via
`Era::uses_tpraos()` (#985's predicate), so no caller can select the wrong one.

**The third copy was in `state/epoch.rs`** — the `#[doc(hidden)]` test-only
`LedgerState::process_epoch_transition`, i.e. *exactly the path #977's fix landed
in while production did nothing*, and the path every unit test drives. Fixing
only the era-rules path would have left the tests exercising the old formula.

### #1046 — the invented default PlutusV2 cost model

dugite injected a hardcoded `defaultV2CostModel` whenever genesis supplied no
V2, so `curPParams.costModels` reported `[V1,V2,V3]` where cardano-node reports
`[V1,V3]` on mainnet/preview/preprod. Latent accept-where-Haskell-rejects: a V2
script executes on dugite and fails upstream with
`CollectErrors [NoCostModel PlutusV2]`.

The issue supposed the devnet/preview disagreement came from different *paths
into Conway*. It does not — it is the **genesis file**:

| genesis | `costModels` | `extraConfig.costModels` |
|---|---|---|
| devnet (`create-testnet-data`) | V1 | V1, **V2** |
| preview/preprod/mainnet | V1 | *absent* |

`alonzoInjectCostModels` (`Alonzo/Transition.hs`) applies
`agExtraConfig.aecCostModels` via `curPParamsEpochStateL` — **cur-only**, and
`updateCostModels` is a **per-language** update. cardano-ledger has no default
V2 anywhere; `defaultV2CostModel` lives in **cardano-api** as a value written
INTO a generated genesis file. dugite's constant was copied from that same
source, so it matched on the devnet by coincidence while being wrong everywhere
real. Reading only the top-level `costModels` key is what produced #994's wrong
conclusion — and #994's claim that real alonzo-genesis files "DO define
PlutusV2" is false; none ever has.

### #1058 — script-integrity mismatch used tag 13 at every PV (#1030 item 3)

`checkScriptIntegrityHash` picks by PV: `< 11` ⇒ `PPViewHashesDontMatch` (UTXOW
**13**, `ToGroup` = Mismatch FLATTENED); `>= 11` ⇒ `ScriptIntegrityHashMismatch`
(UTXOW **18**, Mismatch NESTED + a `StrictMaybe ByteString` preimage field).
dugite emitted 13 unconditionally — wrong at PV11, **which preview runs**, so
the reachable case was the wrong one. #978's inversion. Found only because
#1030 item 3 said the split had "not been independently re-verified against
dugite's encoder" — it was worth re-verifying.

### #1047 / #1026 / #1028 — three accept-where-Haskell-rejects gaps

- **#1047**: no wire-era check on N2C submission. A legacy-era tx was rejected
  only as an ACCIDENTAL CBOR array-length error, and that accident was
  load-bearing: correct any legacy standalone decoder without adding an era
  check and MIR / GenesisKeyDelegation become ACCEPTABLE on a Conway chain (MIR
  Phase-1 is a no-op at PV>=9; GenesisKeyDelegation has era-unconditional
  apply-time support, so the ledger would not catch it). Now
  `HardForkApplyTxErrWrongEra` before decoding, era from
  `EraHistory::current_era()` — **not** ledger pparams (#985).
- **#1026**: PV9 restricts proposal SUBMISSION to
  ParameterChange/HardForkInitiation/InfoAction. dugite had the symmetric VOTE
  restriction but not the proposal side. `isBootstrapAction` is now SHARED by
  both, as upstream gates both on the one predicate.
- **#1028**: `SNothing` guardrail was treated as "anything goes". Haskell
  compares the WHOLE `StrictMaybe`, so a guardrail-less constitution requires the
  proposal to supply `SNothing` too. Root cause was a type that could not express
  the distinction — `Option<Hash28>` conflated "not plumbed" with "genuinely
  absent"; now `Option<Option<Hash28>>`.

### Testing discipline this wave

Every fix landed with a test **proven RED by disarming the fix**, not merely
written — and the one case where that was NOT enough is the lesson of this wave:
#1057's unit tests all passed while the fix was wrong, because they exercised
`switch_chain` in isolation and nothing drove its output through the ledger. A
RED-proven unit test bounds the function, not the system.

Where it did work, it worked well. The two cases that matter most: #1026's behavioural
tests were RED against the *wiring* (not the predicate), which is #977's exact
failure mode; and #1028's not-plumbed-vs-SNothing test drives the same input
through both and asserts they behave DIFFERENTLY, so the doubly-optional type
cannot silently collapse again.

### Superseded: v2.6.0 (2026-08-04) — the DRep pulser becomes one mechanism.
**RE-SYNC RELEASE: SNAPSHOT_VERSION 32 -> 37**, so existing DBs replay chunks
on first restart. Closes #977, #980, #969, #970 (the earlier backlog) plus
#988, #989, #990, #991, #992, #993, #995, #996, #997. Open: #994 (devnet-only,
non-consensus `previousPParams` genesis seeding — deliberately deferred).

### #996 — the mempool re-checked a CHECKLIST, not the rules

A tx valid at admission can be invalidated by a later block. Haskell drops it:
`revalidateTxsFor` re-checks EVERY remaining mempool tx on each tip change via
`reapplyTxs`, which at the ledger layer is

```haskell
reapplyTx globals env state (Validated tx) =
  fst <$> internalApplyTxWithValidation
            (ValidateSuchThat (notElem lblStatic)) globals env state tx
```

— every state-dependent predicate re-run, only the static ones skipped.

dugite re-checked a hand-written list instead: consumed inputs, TTL, missing
UTxO, dangling gov-action votes. Every other predicate was invisible after
admission, and each entry on that list had been added REACTIVELY after a
Haskell peer rejected one of our blocks (the gov-action entry says so in its own
comment). The list was always one defect behind.

The case that outran it: a `CommitteeHotAuth` admitted while its cold credential
was still seated, a later block carrying that member's `CommitteeColdResign`,
and the stale certificate forged at slot 1363. cardano-node rejected the block
with `ConwayCommitteeHasPreviouslyResigned` and — because a Haskell peer
re-requests the same block on every reconnect — **never recovered**. The
reported symptoms (connection churn every ~10 s, the per-IP rate-limiter
lockout, cardano-bp frozen) were all downstream of that one ledger divergence.
The relay's N2C path had rejected the identical tx correctly 7 s earlier; only
the after-admission path was blind.

**One context builder now.** There were THREE hand-rolled copies (N2C admission,
the rollback re-admission path, and one inlined "to avoid a cross-module
dependency on that private fn"). The admission copy was a strict SUBSET of what
block-apply builds — missing `registered_vrf_keys`, `current_treasury`,
`current_epoch`, `stake_key_deposits`, `vote_delegations`,
`genesis_delegate_keys`, `update_quorum`, `constitution_script_hash` — and each
omission is its own way to admit a tx block-apply rejects.
`LedgerState::mempool_validation_context` is the only one now. It also settled a
live divergence: admission keyed `CommitteeHotAuth` membership off
`committee_expiration` alone while block-apply uses
`committee_auth_eligible_members`; Haskell's GOVCERT accepts a pre-authorization
from an incoming member of a live `UpdateCommittee` proposal
(`isPotentialFutureMember`), so the narrow set was a false reject.

Wired into all three revalidation sites. The epoch-boundary one had been calling
bare `validate_transaction` with NO context at all — and the boundary is exactly
where the governance registries change most, so a tx invalidated BY the boundary
was guaranteed to survive it.

Second half: the per-IP inbound rate limiter now EXEMPTS local roots. Upstream
has no per-IP window at all — `Ouroboros.Network.Server.RateLimiting` is a
global soft/hard limit plus a graduated accept delay — and a declared peer is
trusted, never throttled by source address. It also collapsed co-located and
NAT'd peers into one bucket: on the devnet all three nodes are 127.0.0.1. The
window still applies to undeclared IPs. It only ever amplified; with the
revalidation fix the reconnect storm does not happen.

### #997 — Ed25519 accepted small-order keys (CONSENSUS)

`verify` used `ed25519_dalek`'s permissive `Verifier::verify`. cardano-base
implements Ed25519 DSIGN over libsodium's `crypto_sign_verify_detached`, which
rejects small-order and non-canonical public keys and small-order `R`.

With `A` = identity (`0x01 00 … 00`), `R` = identity and `s` = 0, the
cofactorless equation `[s]B = R + [k]A` degenerates to `identity = identity` and
verification succeeds **for any message** — so those bytes in a Byron bootstrap
witness authenticated an arbitrary tx. Accept-where-Haskell-rejects, i.e. the
#996 wedge one layer down. Fixed with `verify_strict`; real keys and real `R`
have full order so only degenerate input is newly rejected.

**Found on the first nightly fuzz run after the workflow was repaired** — every
target had been failing to BUILD because #983 made `dugite-rpc` a fuzz
dependency and `fuzz.yml` never installed `protoc` (ci.yml and release.yml
install it in every job that builds; this workflow was simply missed).

The finder had its own defect, and it is the more instructive half: the target
asserted the canonical bootstrap `public_key` is 64 bytes. Shelley CDDL says
**32** — the 64-byte Byron *extended* key is `public_key || chain_code` and the
halves travel in separate fields, recombined only for address-root derivation.
So the target demanded that a CANONICAL witness be rejected, an assertion that
held only while every such witness happened to fail the signature check for some
other reason. The identity-point input is exactly the case where it did not,
which is why a soundness bug surfaced disguised as a harness assertion. **A
wrong invariant can hide a real bug behind its own false premise.**

### #988 — the epoch boundary APPLIES the frozen pulser; it does not re-decide

Conway's EPOCH rule never runs RATIFY:

```haskell
pulsingState = epochState0 ^. epochStateDRepPulsingStateL
ratifyState@RatifyState {rsEnactState, rsEnacted, rsExpired} =
  extractDRepPulsingState pulsingState
```

RATIFY runs inside `finishDRepPulser`, over inputs frozen one boundary earlier.
dugite froze a plan for `GetRatifyState` and then independently RECOMPUTED the
decision at the next boundary — the same answer only as long as four separate
patches (#903/#922/#950/#966) each kept their term frozen by hand.

Step 3 collapsed those five independently-`Option`al fields into ONE
`Option<DRepPulsingState>` (`{snapshot, ratify_state}` = Haskell's
`DRComplete PulsingSnapshot RatifyState`). A torn pulser and a reader mixing
frozen with live terms are now both inexpressible. **That consolidation is what
found #991** — the capture and the consumer being separate fields IS the
mechanism.

### The three defects it surfaced, all consensus- or wire-affecting

- **#990 (ledger, CONSENSUS)** — `ratifyTransition` tests expiry in its `else`
  branch, *after* the ratification attempt fails, so an action gets a final
  ratification pass on the same boundary that expires it. dugite `continue`d on
  `expires < epoch` BEFORE the threshold check, discarding **every vote cast in
  a proposal's final epoch**. False reject.
- **#991 (ledger, CONSENSUS)** — proposal deposits added TWICE to DRep voting
  power (once at capture per `computeDRepDistr`, again at consumption) while
  `compute_total_drep_stake_from` counted them once. Numerator inflated against
  an unchanged denominator ⇒ `dRepAcceptedRatio` too high ⇒ **accept-early**.
  Introduced by #949 adding the missing term to the capture without removing it
  from the consumer: a missing term became a doubled one. Present since v2.4.4.
- **#993 (n2c)** — `rsEnacted` elements were encoded as an
  `array(2) [GovActionState, GovActionId]` pair that does not exist upstream,
  with the id duplicated (`gasId` is already field [0]). cardano-cli rejected
  the whole reply with `Expected 7, but found 2`. `GetRatifyState` was
  **undecodable exactly when it has something to say** — `rsEnacted` is
  non-empty only during the epoch before something enacts. The #968 shape.

Plus **#992** — `GetGovState` embedded a hardcoded EMPTY `DRepPulsingState`
beside a second hand-written copy of `EnactState`. Narrower than it looks:
cardano-cli renders `nextRatifyState` from tag 32, NOT from the tag-24 embedded
pulser, so this has no cardano-cli-visible form and is guarded at the encoder.

### #989 — a snapshot whose UTxO store is gone is rejected, not "reset"

`LedgerState::reset_to_origin` reset only `tip` and `epoch`, so a forced
re-replay ran from slot 0 carrying epoch-1379 pots — #985's chimera, ending in
a snapshot OF the chimera. Deleted rather than repaired: genesis state needs
genesis inputs, so no in-place reset can be correct. The check now runs BEFORE
the snapshot is loaded.

### Testing — preview replay is the gate, not the devnet

The devnet produces ONE boundary with `enacted=0`. A preview replay from
genesis gives **733 Conway boundaries with real governance in ~4 minutes**, and
Koios provides independent pot truth. Every ledger change here was gated on it:
733/733 boundaries applied with `planned_at == boundary_epoch - 1`, the same 14
enactment boundaries before and after, 0 apply failures, 0 ERROR, and final
pots byte-exact vs Koios for epoch 1379 (treasury `6975097769635306`, reserves
`7743240562481380`).

**#993 was found only by RUNNING the new devnet oracle** — see
`ratify-state-parity.sh`. Writing a gate-enforced check and never executing it
is the defect class the check exists to catch; one run also exposed three
missing gov-lifecycle prerequisites in the Round 2 recipe and a `null#null`
tautology in the check's own jq.

### QA — v2.6.0 (SHIPPED)

devnet-validate standard preset, **4/4 rounds PASS**
(`reports/devnet-validate/v2.6.0.json`) vs cardano-node 11.0.1, strict,
`gate_integrity.admissible = true`, zero missing evidence — run against the
tagged commit, not an earlier one.

- 651 canonical blocks, 0 invalid forges, 0 critical anomalies
- tx-zoo 120/123 baseline, **0 fail**; bidirectional parity **79 scripts,
  0 OFFDIAG, 0 CLASSDIFF**; cli-parity 25/25; adversarial N2N 26/26;
  UTxO RPC 27/27; chaos 5/5 with 0 ENV_SKIP
- tip-parity **100% in all four rounds** (24/24, 24/24, 176/176, 36/36)
- treasury+reserves **byte-exact** vs cardano-node after the RUPD boundary
- futurePParams parity 548 compared / 0 diffs / **6 samples inside the
  `PotentialPParamsUpdate` window**; ratify-state parity 544 compared /
  0 diffs / **181 non-empty `nextRatifyState` samples**. Both samplers reached
  their NON-VACUOUS condition — two implementations agreeing that nothing is
  about to happen is not evidence, and that is precisely what #992's hardcoded
  empty pulser produced for every previous gate.

The 3 tx-zoo failures are all `01a-simple-pay` from the Round 2 trickle racing
itself (one address, every 20 s ⇒ `Input conflict: input already claimed by
mempool tx`). Verify the REASON before dismissing these — the identical count
appears in v2.4.3 for the identical cause, which makes it easy to wave through
a real one.

**Preprod soak, 3 h on v2.6.0** — the 32→37 re-sync replayed 2.6M blocks at
~45k blk/s and came up clean. Byte-exact against Koios throughout: tip
(delta 0 at every sample), treasury `1952221204641186`, reserves
`12986390967589170`, protocol params at PV11, and the **entire registered pool
set (558/558, zero set difference)**. 0 ERROR, 0 apply failures, 15 peers,
RSS flat (a 1.8 GB reclaim mid-run, no growth). 2 ledger rollbacks, both clean
1-block fork switches with **0** `LedgerSeq was incoherent` — positive evidence
the #985 startup re-anchor fired. No epoch boundary falls inside a 3 h preprod
window (5-day epochs), so the soak validates steady-state sync/apply/serve;
boundary handling is the devnet gate's job.

**Trap seen twice this run**: starting `soak.sh` immediately after a
deliberate disruption (the chaos suite's SIGKILL, or Round 3's 90 s outage)
samples tip-parity DURING reconvergence and fails p4 on noise. Gate the soak on
`wait-catchup.sh` — an added assertion, not a relaxed predicate.

### Superseded within v2.6.0 — the earlier backlog (#977, #980, #969, #970)

### #977 — `futurePParams` was a hardcoded constant, and the fix landed in a dead path
The LSQ encoder wrote `NoPParamsUpdate` unconditionally; dugite had no
`futurePParams` at all to encode. Modelled now as Haskell's three-way sum with
its full lifecycle (solidify FIRST in `validatingTickTransition`, then NEWEPOCH
takes the boundary branch OR `predictFuturePParams`, never both; the window is
`3k/f`, NOT the `4k/f` RUPD one). Needed #988's frozen DRep pulser first —
`predictFuturePParams` reads `rsEnacted`/`ensCurPParams`, which dugite could not
answer mid-epoch before it existed.

**The part worth remembering**: the unconditional boundary reset was written
into `LedgerState::process_epoch_transition`, which is `#[doc(hidden)]` and
whose own rustdoc says *"Production code MUST go through `Self::apply_block`"*.
Every unit test in the crate uses that helper, so all eight new tests passed
while `EraRulesImpl::process_epoch_transition` — the path a real block takes —
did nothing. #985's N-copies trap recurring, with the current copy being the one
nobody edited. Both boundary paths now call one shared
`epoch_boundary_governance_step`, so the drift is inexpressible.

Caught by diffing `gov-state` against cardano-node across a live boundary: 4
DIFF / 219 samples before, 699 MATCH / 700 after. The gate could not have found
it — `2*stabilityWindow (480) > epochLength (400)` on devnet, so
`PotentialPParamsUpdate` survives ~3 slots out of 400 and a one-shot `09k`
lands there ~1% of the time. `futurepparams-boundary-parity.sh` now samples
continuously through Round 2, tip-pinned, and reports INCONCLUSIVE rather than
PASS when it never sees the window.

### #980 — responder mini-protocols were orphaned, not restarted or fatal
Not a ChainSync bug and not really load-dependent. The five N2N **responder**
tasks were spawned bare: the return value was logged, the task exited, nothing
observed the handle, and the mux silently discards frames for a route whose
receiver is gone. cardano-node sends ChainSync `MsgDone` on every Hot->Warm
demotion and opens a fresh session on the SAME bearer when it re-promotes —
dugite returned `Ok(())` and the route was dead for the connection's life.
Load only made the peer governor churn more; restarting the DOWNSTREAM fixed it
because a new connection got a new task.

Upstream policy (oracle-verified, ouroboros-network `c45735a5`): an exception
in a mini-protocol **terminates the whole mux** (*"We always respond by
terminating the whole mux"*); a clean exit is **restarted** by InboundGovernor
(`TrResponderRestarted`). The state dugite was in is not representable upstream
at all. Both halves implemented per protocol; re-arm reuses `MuxHandle::resubscribe`,
the initiator-side Hot->Warm mechanism whose responder half was simply missing.

**Trap**: `cancel()` alone is NOT teardown. Nothing aborts the mux when the
connection token fires — `shutdown()` does the two separately and `is_alive()`
reads only the mux handle — so cancel-only leaves the bearer open and the
connection "alive" to the reaper: a worse silence. The test caught it only
because it asserts what the PEER observes (`!is_alive()` + channel closed), not
that a token flipped. The harness restart gates are deleted; `wait-catchup.sh`
now FAILS instead of repairing the devnet mid-round.

### #969 / #970 — context-inspecting validators, and Aiken removed
Every Plutus validator on the devnet was always-true or always-false, and such
a validator never reads the ScriptContext — so the gate proved dugite BUILDS a
context but never that its CONTENTS are right, which is exactly #772's class.
Aiken could not fix it either: a third-party compiler sharing dugite's
misunderstanding agrees with the bug ("aiken parity is circular", #772).

Resolved with **route 1**: `Test.Cardano.Ledger.Plutus.Examples` is generated by
`plutus-preprocessor` from plutus-tx source using `plutus-tx-plugin`, and
cardano-ledger **checks the compiled bytes in**. 14 scripts x V1/V2/V3/V4 = 55
arms vendored at `tests/conformance/upstream/plutus-examples.json`. No GHC, no
cabal, no network at devnet setup. dugite reproduces all 55 of upstream's own
ScriptHashes through the production `script_ref_hash`, and decodes all 55.

These are **not** drop-in "always true" — they assert on purpose and datum
presence, so the mapping is per-call-site (spend => `WithDatum`, including V3;
mint/cert/vote/propose => `NoDatum`; `alwaysFailsNoDatum` is TRUE for
spending-with-datum, so a spend that must fail wants `alwaysFailsWithDatum`).
New `17-context-inspecting` drives the real ones, including `17d`/`17e` — the
same script with one redeemer byte changed and opposite verdicts, so "the
script accepted it" cannot be confused with "the script never ran" — and `17f`,
which brackets dugite's phase-2 CPU accounting against cardano-api's evaluator
from both sides (accepted at exactly 4,456,575 steps, rejected one below). That
is the gate's first assertion about evaluation COST rather than verdict; #772
was a cost bug that changes no verdict.

Four fixture defects surfaced by the swap, all invisible while a trivial
validator hid them: 03c on the wrong variant (`PT5`); 03j/03l budgeted for a
trivial script, so 03l silently stopped testing what it claims and 03j started
passing for the wrong reason (budget exhaustion, not the script's verdict);
03j's collateral pinned to the old fee; and keygen caching `stake-script.plutus`
on existence rather than content, so a binary swap left `stake.addr` on the old
hash.

**And the category did not run.** `run-all.sh` iterates a hardcoded
`ALL_CATEGORIES`; 17 was absent, so a full run reported `total=115 pass=112
fail=0` with the whole new category skipped and the summary looking identical.
The #971 shape again. `run-all.sh` now hard-fails on disk/array drift.

### Superseded: v2.5.1 (2026-08-03) — P0 hotfix. Drop-in from v2.5.0, SNAPSHOT
unchanged at 32. Closes #985 (found in the field by a tester) and #971-#976 +
#984 (the fuzz-coverage backlog, PR #982).

### #985 — a canonical Conway block rejected as a non-active overlay slot
A v2.5.0 preview BP rejected canonical block 4535827 as
`NotActiveOverlaySlot`, cached it in `invalid_cache`, and then refused every
descendant (`StoreButDontChange`) for the rest of the process lifetime — no
self-healing, while its forge loop kept running against a corrupted ledger.
Three defects, all three now closed:

- **The LedgerSeq was never re-anchored after a bulk ledger advance.** It is
  anchored once in `Node::new`, BEFORE replay; startup replay, the rollback
  snapshot slow path and the gap-bridge all move `ledger_state` without
  pushing deltas. `startup::recover_ledger_seq` and `LedgerSeq::reset_anchor`
  both existed with **zero production callers** — dead code that read as an
  implemented feature. The SNAPSHOT 31->32 quarantine is what made it a P0:
  `Node::new` falls back to `init_fresh_ledger`, so the stale anchor is
  *genesis* (preview: PV 6, d = 1/1, 7 genDelegs).
- **`rollback_via_seq` then installed a chimera.** Reconstruction from a stale
  anchor is CURRENT in every field a delta touches (tip/slot — so the
  forecast-horizon check passed and nothing upstream noticed) and STALE in
  every field none does. `advance_anchor` does not save you: it folds tip
  deltas INTO the genesis anchor, so pparams stay at genesis forever.
- **The overlay gate keyed off ledger pparams, not the block's era.** With
  PV 6 / d = 1 / delegates present, all three terms held on a PV 11 chain.
  d=1 ⇒ every slot is an overlay slot; slot 119084816 is offset 25616 in
  epoch 1378 and `25616 % 20 = 16` ⇒ `NonActiveSlot`. The arithmetic
  reproduces the log to the slot.

Fix: `Era::uses_tpraos()` leads a single shared
`should_build_overlay_context` (the condition existed in TWO hand-written
copies, only one current — the recurring N-copies trap);
`Node::reanchor_ledger_seq` at all three bulk sites; `LedgerDelta::parent_point`
+ `LedgerSeq::incoherent` so `find_rollback_n` declines rather than
reconstructing; and **self-heal** at the live apply path, so a future missed
re-anchor costs ONE BLOCK, not the process lifetime.

**Haskell (oracle-verified, cardano-ledger `4f7cb2d6…`, ouroboros-consensus
`release-ouroboros-consensus-3.0.1.0`)**: `HFEras` binds `TPraos` to
Shelley-Alonzo and `Praos` to Babbage+ at the TYPE level; Praos's
`updateChainDepState` is only KES+VRF, its `PraosValidationErr` has 11
constructors and none is overlay-related, and its `LedgerView` carries neither
`d` nor `GenDelegs`. A Conway header can never be overlay-rejected upstream.
The same check **corrected a wrong assumption**: `AnchoredSeq` does NOT
enforce chaining (no hash-chain concept; `(:>)` appends unconditionally —
`isValidSuccessorOf` is on `AnchoredFragment`, the block layer). Upstream
`LedgerSeq` is coherent BY CONSTRUCTION (`reapplyBlock` derives from
`currentHandle db`), which is exactly why dugite needs a guard where upstream
needs none. Do not "align" this away.

**Testing caveat — the devnet is structurally blind to this** (it runs PV >= 7,
so the chimera's symptom cannot appear). A green devnet-validate is NOT
evidence. Live signal: a node applying blocks WITHOUT logging `LedgerSeq was
incoherent at block apply` is positive evidence the startup re-anchor fired.
Both fix layers were confirmed RED before green by disarming them — with the
guard off, `rollback_via_seq` returns `Some(1)` and leaves `pv_after = 6` on a
PV 11 ledger.

### Superseded: v2.5.0 (2026-08-03) — release-gate coverage wave. **RE-SYNC RELEASE:
SNAPSHOT_VERSION 31 -> 32.** Closes #953-#961 (the #962 audit backlog) plus
#965, #966, #968, #978.

The theme: nearly every defect this wave — in the harness AND in the node — was
a check reporting success while measuring nothing. The node bugs were found
*because* those checks were repaired.

### Node fixes
- **#966 (ledger, CONSENSUS, SNAPSHOT 31->32)** — RATIFY gated
  `TreasuryWithdrawals` on the LIVE post-RUPD treasury. Haskell reads
  `ensTreasury` sealed into the DRep pulser one boundary EARLIER
  (`setFreshDRepPulsingState`), so RATIFY is structurally blind to the
  `applyRUpd` landing at the boundary it runs on. dugite would enact a
  withdrawal an epoch EARLY — a split in the accept-early direction.
  `RatificationSnapshot` already froze every other `dpEnactState` term;
  `treasury` was the one left reading live. Same shape as #949.
- **#968 (network)** — `MsgHasTx` carries `[era, bstr32]` (a `OneEraGenTxId`),
  not a bare bstr, so `cardano-cli query tx-mempool tx-exists` HUNG FOREVER.
  Both existing tests PINNED the bug: the test helper encoded the same wrong
  shape. Second half: `tokio::select!` DROPS the losing branches and dropping a
  `JoinHandle` DETACHES — so a protocol error left the mux running and the
  socket open. The #924 trap, recurring in the N2C path.
- **#965 (ledger)** — certificates never required a SCRIPT witness. Only the
  deposit-less `reg_cert` (idx 0) is permissionless; index 7 is not. dugite
  accepted what cardano-node rejects.
- **#978 (n2c)** — `WithdrawalsNotInRewardsCERTS` had no encoder arm. dugite
  had implemented BOTH PV>=11 withdrawal encodings but not the PV<=10 one —
  and PV10 is what every real network runs, so the only REACHABLE variant
  degraded to a generic mempool error while the two implemented arms were dead
  code. Sweep of 36 more: #979.

### Gate coverage (#962 backlog)
Two-forger topology with an independent Haskell arbiter (#957); governance
enactment with real DRep voting power and a funded `ensTreasury` (#956);
the reward-withdrawal path executing POSITIVELY for the first time ever
(#958); UTxO RPC wired and asserted (#960); chaos repaired (#959); Plutus
script purposes beyond spend/mint (#955); the parity oracle widened 41 -> 79
scripts comparing reject REASONS (#954); and a release gate that hard-fails on
evidence never produced (#953).

### Open, found but not fixed
#963/#964 (LSQ), #967 (snapshot guard fixture), #969/#970 (ScriptContext
coverage + replace Aiken with plutus-tx — Aiken parity is circular), #977
(`futurePParams` hardcoded), #979 (36 generic rejections), **#980 (dugite's
ChainSync server stops feeding a Haskell peer under load and never recovers —
its documented restart workaround provably does not work)**.

Both found by pushing `decode(encode(x)) == x` against the REAL decoder into
new areas, and both oracle-verified against Haskell BEFORE touching either side
— which mattered, because the verdict went a different way each time.

- **#951 (serialization)** — PPU key 26 `drep_voting_thresholds` encoder wrote
  the 10 elements in the WRONG ORDER: dropped `constitution` from index 3,
  shifted six up, appended it at index 9 where Haskell puts
  `treasuryWithdrawal`. The DECODER was always right (matches
  `EncCBOR DRepVotingThresholds` exactly) — so a dugite-built ParameterChange
  installed the WRONG governance thresholds, the very values that decide
  whether actions pass. Key 25 (5-element pool thresholds) verified CORRECT.
- **#952 (serialization)** — `encode_plutus_int` gated the bignum path on
  `to_i128()` then called `encode_int(i128)`, whose `value as u64` SILENTLY
  TRUNCATES. Haskell's threshold is Word64: plain int only for
  `[-(2^64) .. 2^64-1]`. Integers in `(2^64, i128::MAX]` wrapped mod 2^64 ⇒
  wrong `script_data_hash` ⇒ wrong phase-2. `encode_int` now carries a
  `debug_assert` + doc making its narrow contract explicit.

**Clean negatives from the same wave** (recorded so they are not re-audited):
all 18 Conway certificate variants, all 11 `GovAction` variants, all 5 `Voter`
discriminators, `VotingProcedure`, and every `dugite-uplc` `to_i128()` site
(those are CHECKED conversions that fail loudly, not truncating casts).

**Caveat pinned in the tests**: a same-process round-trip is necessary but NOT
sufficient — a shared wrong order on BOTH halves still passes. #951 was caught
only because encoder and decoder disagreed. The durable guard is a
Haskell-derived fixture.

### QA — v2.5.0
devnet-validate standard preset, **4/4 rounds PASS**
(`reports/devnet-validate/v2.5.0.json`) vs cardano-node 11.0.1.
`gate_integrity.admissible = true`, zero missing evidence.

- tx-zoo **105 scripts**, source="round" in every round (no shared counts)
- bidirectional parity **79 scripts, 0 OFFDIAG, 0 CLASSDIFF**
- cli-parity **22/22 COMPARED** (not merely 22 rows emitted — see below)
- adversarial N2N **26/26**, UTxO RPC **27/27**, chaos **5/5**
- tip-parity **100% in all four rounds** (24/24, 12/12, 175/175, 12/12)
- byte-exact treasury+reserves vs Haskell after the RUPD boundary
- restart 52 -> 84, all predicates green

**The parity oracle now runs in its OWN round.** It re-executes the zoo twice
(once per socket), and combining that with Round 1's full zoo put ~263 script
executions on one devnet — up from ~167 before #954 widened it 41 -> 79. That
volume reliably tips cardano-bp into the **#980** stall, after which every
tip-sensitive suite becomes UNMEASURABLE rather than failing. Splitting it out
is not a coverage reduction; the same 79 scripts still run through both
sockets.

**Two gate defects found by this run, both of the "measures nothing" family:**
- the cli-parity denominator counted rows EMITTED, not comparisons MADE, and
  reported `22/22 queries OK` for a run that compared FOUR;
- `chaos.expected_cases` was pinned from the EXTENDED set (6) while the
  standard set produces 5 — a denominator no standard run could ever satisfy.

### QA — v2.4.5
devnet-validate standard **3/3 rounds PASS** (`reports/devnet-validate/v2.4.5.json`)
vs cardano-node 11.0.1: 524 canonical blocks, 0 orphans, 0 invalid forges, 0
critical anomalies. Bidirectional parity **41/41, 0 off-diagonal**. Adversarial
N2N **26/26, 0 panic, 0 silent-skip**. cli-parity **18 EQUAL / 0 divergent**.
Byte-exact pot parity after the first RUPD. Restart tip 34 -> 71, 0
stale-intersection.

**One tx-zoo failure investigated and cleared**: R1 reported
`03k-datum-hash-reveal` as `not-included` (tx accepted, never landed in a block
within the harness window). NOT a regression from #952 — that datum is
`{"int": 42}`, and #952 only alters integers above 2^64, so `to_u64()` succeeds
and the bytes are provably identical to the old path. Re-ran the whole
`03-plutus` category on a fresh devnet with the v2.4.5 binaries: **13/13 pass,
including 03k**. Timing flake.

### Superseded: v2.4.4 (2026-08-01) — second audit wave: encoders + DRep voting power. Drop-in, SNAPSHOT
unchanged at 31. Closes #946, #947, #948, #949, #950.

- **#948 (serialization)** — `encode_drep` emitted a **32-byte** DRep KeyHash
  where CDDL `drep = [0, addr_keyhash]` wants **bstr(28)**, while the
  ScriptHash arm in the SAME function emitted 28. `read_drep` builds the value
  as `read_hash28_cert()?.to_hash32_padded()` and `read_hash28_cert` rejects
  any width but 28 — so dugite's output was **self-undecodable**. Identical to
  #932's `encode_voter` StakePool arm but with wider reach: every DRep
  delegation cert (`vote_delegation`, `stake_vote_delegation`,
  `stake_reg_deleg_vote`). **Two existing tests PINNED the bug** (asserting
  bstr(32) / len 36).
- **#947 (serialization)** — tx-body key 14 `required_signers` is
  `Set (KeyHash Guard)` and needs tag 258 at PV>=9; it was the only Set-typed
  body field the #939/#940 sweep missed. Key 14 is INSIDE the body, so the
  omission changed the **tx id** vs cardano-cli.
- **#946 (nix)** — flake built a `dugite-tui` package for a crate that does not
  exist, and the devShell lacked `protobuf`, so `nix develop` gave a shell the
  workspace cannot compile in. No CI covers the flake.
- **#949 (ledger, CONSENSUS-relevant)** — the frozen DRep distribution snapshot
  (dugite's `psDRepDistr`, consumed by `ratify_proposals()` as on-chain voting
  power) summed only `InstantStake + AccountBalance`. Haskell's
  `computeDRepDistr` sums `InstantStake + ProposalDeposits + AccountBalance`.
  Under-counting can flip a governance action's ratification vs cardano-node.
  The SAME term had been found missing once before and fixed ONLY in the live
  query path — so the bug moved out of sight, not away.
- **#950 (node)** — `GetDRepStakeDistr` (LSQ tag 26) answered from LIVE state;
  Haskell reads `psDRepDistr . fst $ finishedPulserState`, frozen once per
  epoch boundary. A credential REGISTERED mid-epoch is not in `dpAccounts` at
  all. Same class as #922. Found by the cli-parity round — visible ONLY because
  #945 fixed the all-zero reporting.
- **Hardening** — all 18 Conway certificate variants now round-trip through the
  REAL decoder in test. That property is what catches the #948 shape; asserting
  the encoder's own output shape is what let it survive.

### QA — v2.4.4
devnet-validate standard **3/3 rounds PASS** (`reports/devnet-validate/v2.4.4.json`)
vs cardano-node 11.0.1: 560 canonical blocks, **0 orphans**, **0 tx-zoo
failures**, 0 invalid forges, 0 critical anomalies. Bidirectional parity
**41/41, 0 off-diagonal**. Adversarial N2N **26/26, 0 panic, 0 silent-skip**.
cli-parity **18 EQUAL / 0 divergent** — the `drep-stake-distribution`
divergence that exposed #949/#950 is resolved on the wire. Byte-exact pot
parity after the first RUPD. Restart tip 37 -> 71, 0 stale-intersection.

### Superseded: v2.4.3 (2026-08-01) — CBOR encoder alignment sweep: the non-map half of
`cardano-ledger-binary`, never previously audited. Drop-in, SNAPSHOT unchanged
at 31. Closes #935, #936, #937, #938, #939, #940.

Three of the six were found BY oracle-verifying the first three — every claim
in this release is backed by verbatim IntersectMBO source (pinned
`58ba7795273f9301a9a198930e50a6ca1ee85238`).

- **#938 (serialization)** — #930/#932 aligned the Map encoders with
  `encodeMap`; the identical `lengthThreshold = 23` governs
  `variableListLenEncoding`, and dugite emitted a DEFINITE array header at
  every array/list/set site. Now `encode_array_open`/`encode_array_close`
  (siblings of `encode_map_open/close`) on: tx-body outputs / required_signers
  / proposals / sub_transactions, `encode_tagged_set` + `encode_plain_array`
  (inputs, certs, collateral, ref inputs), witness-set collections, aux-data
  script arrays, and the block-body segments. Fixed-arity
  `encode_array_header(n)` records deliberately untouched (`encodeListLen n`).
  Block body now shares ONE encoder per segment (`encode_tx_bodies_segment`,
  `encode_witness_sets_segment`, `encode_invalid_indices_segment` alongside
  `encode_aux_data_segment`) across `encode_block` /
  `compute_block_body_hash` / forge `compute_body_size` — the triplication was
  the defect mechanism for BOTH #932 and #938.
  **Not a chain split**: Haskell's `DecCBOR (Annotator (AlonzoBlockBody))` uses
  `withSlice` and hashes the bytes AS RECEIVED, so dugite's definite framing
  was self-consistent and accepted. Real impact = non-canonical output, a
  different tx id than cardano-cli for the same synthetic tx, and a 1-byte
  over-count at >=256 elements (definite `0x99 xxxx` = 3B vs `0x9f`+`0xff` = 2B)
  — the #930 shape, so false REJECT possible, never false accept.
- **#940 (serialization)** — Conway `ctbrCerts`/`ctbrProposalProcedures` are
  **OSet, not Set**. dugite ran them through the sorting `encode_tagged_set`,
  which **reordered certificates** (order is semantically load-bearing:
  registration must precede the delegation using it), and omitted tag 258 on
  proposals entirely. `OSet`'s `setTag` is UNCONDITIONAL — no
  `ifEncodingVersionAtLeast` guard, unlike `Set`'s PV>=9 gate. New
  `encode_ordered_set` = tag + variable array, order preserved.
- **#939 (serialization)** — Conway witness keys 0/1/2/3/6/7 omitted tag 258
  (`encodeWithSetTag`, PV>=9). Confirmed empirically: the real `conway.hex`
  fixture has 4 witness sets with `key0 -> tag258 -> array`. Era-gated.
  **Ordering is correct as-is** — Haskell decodes these into `Set`/`Map`
  (order unobservable, no order check at any PV) and its `MemoBytes`
  `encodePreEncoded` replays original bytes on relay, so sorting would BE the
  divergence. Sort keys recorded in-code for any future fresh-construction
  path; note `BootstrapWitness` orders by the Byron addr-root hash, NOT
  `WitVKey`'s blake2b224(vkey).
- **#937 (serialization)** — three drifted copies of `read_metadatum` all gated
  nested maps/lists/text on the definite form only. Haskell accepts BOTH forms
  of every compound token. One shared decoder in `decode/helpers.rs` (the
  duplication WAS the drift mechanism) + `Reader::read_str_owned`. Encoder
  stays always-definite (`encodeMetadatum`); `TypeTag` stays rejected.
- **#936 (serialization)** — Dijkstra `sub_transactions` is an `OMap`, which
  encodes as a BARE ARRAY of values (`encodeStrictSeq`, keys reconstructed via
  `toOKey`), not the `{tx_id => body}` map dugite emitted. Decoder now derives
  each id from its own body bytes and rejects duplicates
  (`EnforceNoDuplicates`), making key-smuggling structurally inexpressible.
- **#935 (cli)** — 4 lenient CBOR unwrap heuristics replaced by one strict
  `envelope::unwrap_key_bytes` (the `& 0xe0` test ate the first byte of any raw
  key starting 0x40..=0x5f, 1-in-8). Plus `--mainnet`/`--testnet-magic`/
  `CARDANO_NODE_NETWORK_ID`, inline verification-key STRINGs,
  `--key-output-bech32`/`-text-envelope`/`-format`, `key-hash-VRF --out-file`.
  Era-prefix leniency KEPT (dugite is a strict superset of cardano-cli 11).

### v2.4.3 second wave — found by cross-checking the DOCS against the code
- **#941 (node)** — metrics port drifted 3 ways: `--help` said 12798, binary
  used 12796, and `config.rs`'s test module mirrored a THIRD rule (12798 +
  no `TurnOnLogMetrics` branch). One `config::resolve_metrics_port` now;
  default **12796** (deliberately off cardano-node's 12798 — co-located).
- **#942 (node)** — `--log-retention-days` deleted nothing: `cleanup_old_logs`
  was `#[cfg(test)]` (absent from release builds) and the
  `start_log_cleanup_task` its rustdoc referenced never existed. Now wired,
  with tests driving the SPAWNED TASK not the helper.
- **#943 (node)** — `BlockFetchLogicTask` spawned in production, no peer ever
  registered ⇒ `evaluate_and_fetch` early-returned forever. Deleted. It was
  the decoy that made the docs claim a multi-peer fetch pool. Live path is
  `ConnectionLifecycleManager::make_blockfetch_task` (single fetcher,
  Haskell `bfcMaxConcurrencyBulkSync=1`).
- **#944 (harness)** — devnet-validate Round 3 queried
  `state/dugite-bp.sock`, which does not exist (sockets are `/tmp/ld-$UID/`;
  macOS sun_path 104B). Restart criterion never evaluated — reported FAIL
  identically whether the node recovered or died. Now uses
  `$LD_DUGITE_BP_SOCK` + INCONCLUSIVE guards.
- **#945 (harness)** — `cli-parity.csv` header declared 6 columns while rows
  had 7 (`status` missing), and the report generator indexed `$5`/`$6` off the
  bad header, with a skip rule matching `$2~/\//` (any query NAME with a
  slash). **Every release report ever published recorded `cli_parity` as
  all-zero** — v2.4.2.json included; the "18 EQUAL" claims were transcribed
  from console logs by hand. Fixed header + generator; v2.4.3 is the first
  report with real parity numbers.

### QA — v2.4.3
devnet-validate standard **3/3 rounds PASS** at b6b9f2b024
(`reports/devnet-validate/v2.4.3.json`) vs cardano-node 11.0.1:
543 canonical blocks, **0 orphans**, 0 invalid forges, 0 critical anomalies,
0 ERROR lines in any node log. tx-zoo 84/85 baseline (1 state-skip:
no-rewards). Bidirectional parity **41/41 identical, 0 off-diagonal**.
Adversarial N2N **26/26 handled (22 correctly REJECTED, 4 PASS), 0 panic,
0 silent-skip**. cli-parity **18 EQUAL / 0 divergent / 4 skip**.
Byte-exact pot parity after the first RUPD (boundary 1->2, epoch 2):
treasury=3347997655395 reserves=5996646007361582 on BOTH dugite and Haskell.
Restart: tip 27 -> 65 within 60s, 0 stale-intersection.
The 2 epoch-boundary tx-zoo failures are the trickle racing itself
(`Input conflict: input already claimed by mempool tx`) — the mempool
input-conflict check working as designed, not a defect.

### Superseded: v2.4.2 (2026-07-31)
Full Haskell-alignment sweep. Drop-in,
SNAPSHOT unchanged at 31. Closes #932, #933, #934.

- **#932 (serialization)** — `encodeMap` semantics (definite <=23 entries,
  indefinite `0xbf…0xff` above; shared `encode_map_open/close` in cbor.rs)
  applied to ALL remaining Map encoder sites: withdrawals, redeemers map
  form (PV>=9), voting-procedures (both levels), treasury-withdrawals,
  committee, MIR creds, metadata maps, block aux-data segment, Dijkstra
  direct_deposits + account_balance_intervals. Pinned always-definite (do
  NOT "align"): `PlutusData::Map`, nested `Metadatum::Map`, integer-keyed
  struct maps. Bare-metadata MapIndef decode fixed (was silently EMPTY).
  Audit find fixed: `encode_voter` StakePool emitted 32B where CDDL
  `voter = [4, pool_keyhash]` wants bstr(28) — synthetic SPO votes were
  self-undecodable. Forge `compute_body_size` now shares
  `encode_aux_data_segment` (was +1 byte declared at >255 aux txs).
- **#933 (node)** — `haa_satisfied` = Haskell `outboundConnectionsState`'s
  independent case split: bootstrap-configured → closure + >=1 ACTIVE
  BOOTSTRAP peer (specifically, not any trusted peer); Praos+no-bootstrap
  → false, silent; Genesis+no-bootstrap → hot-BLP count ONLY (untrusted
  established peers irrelevant — the branch is now reachable).
  associationMode documented always-Unrestricted. Clamp/#920/#931 intact.
- **#934 (cli)** — cardano-cli compat: `key-gen-KES`/`key-gen-VRF`/
  `key-hash-VRF` canonical casings (lowercase aliased),
  `--operational-certificate-issue-counter[-file]` aliases, `--network`
  hard-errors on typos (was silent Testnet fallback), typed
  `verification-key-hash` (rejects signing/KES/VRF keys by name), exact
  `0x58 0x20` CBOR unwrap in `pool_id_from_cold_vkey`.
- **Deferred with issues**: #935 cli surface parity backlog, #936 Dijkstra
  sub_transactions OMap shape (unreleased era), #937 nested-metadatum
  MapIndef decode liberality (needs Haskell-source verification).

QA: devnet-validate standard **3/3 rounds PASS** at 261b7852e3
(`reports/devnet-validate/v2.4.2.json`) — 558 canonical blocks, 0 invalid
forges, tx-zoo 84/84 full run, bidirectional parity 41/41, byte-exact
treasury/reserves after first RUPD, restart rejoin <60s. Workspace suite
7653. **Open issues: #935/#936/#937 (documented deferrals only).**

### v2.4.1 (2026-07-31)
Encodemap parity + diagnostics + coverage.
Drop-in, SNAPSHOT unchanged at 31. Closes #930, #931.

- **#930 (serialization/ledger)** — `encode_multi_asset`/`encode_mint` now
  match Haskell cardano-ledger-binary `encodeMap`: indefinite-length CBOR
  map headers (`0xbf…0xff`) for maps with >23 entries, definite otherwise,
  at both map levels. Fixes Rule 5a (`OutputValueTooLarge`) over-counting
  by 1 byte per >=256-entry map — preprod tx `96ae78f7…` (324-entry asset
  map) measured 5001 vs Haskell's 5000 at maxValSize=5000 (strict `>`),
  a false Phase-1 reject (N2C submit + forging; chain-follow was safe via
  trust-consensus). Over-count only — never false accepts. On-chain tx
  pinned as fixture; boundary tests at 23/24/255/256. Residual: other
  synthetic-only encoders (withdrawals, voting-procedures, metadata…)
  still definite-only — see #930 comment. `PlutusData::Map` is CORRECTLY
  definite-only (different encoder — never "align" it).
- **#931 (node)** — HAA clause (a)/(b) diagnostics now WARN only when the
  sync-time trusted-only clamp is actually active (clamp `is_some()`
  mirrored into `NodePeerManager`); debug otherwise, "bypassed" claim
  removed. In Praos mode (preprod default) the clamp never exists and
  untrusted established ledger peers are normal (Haskell
  `outboundConnectionsState` → `UntrustedState`, silent). Zero behavior
  change, pinned by test. Deferred: Haskell's independent 4-branch case
  split (+ the structurally-unreachable hot-BLP clause during clamped
  Genesis sync).
- **Coverage** — +58 dugite-cli tests (key/address/node/query + end-to-end
  command_files.rs), +29 dugite-rpc tests (submit/watch services had ZERO
  coverage; config/error units). Workspace suite 7503 → 7608.

QA: devnet-validate standard **3/3 rounds PASS** at 4a8a03148a
(`reports/devnet-validate/v2.4.1.json`) — 552 canonical blocks, 0 invalid
forges, tx-zoo 84/84 full run, bidirectional parity 41/41, byte-exact
treasury/reserves after first RUPD, restart rejoin <60s with 0
stale-intersection. **Zero open issues.**

### v2.4.0 (2026-07-30)
Storage durability & sync recovery:
#926-#929, the full defect chain behind the 2026-07-28 preprod BP incident
(38k-slot indexed hole + permanent all-peer sync wedge). Drop-in, SNAPSHOT
unchanged at 31; two new DB files (`lock`, `immutable/clean`).

- **#926 (storage)** — the active chunk's secondary index was memory-only
  until the shutdown-only flush(), so a hard stop lost every entry since
  boot (~10 h in the incident); open silently skipped the index-less chunk
  and `open_for_writing` reused its number (File::create over live data).
  Now: entries written per-append (Haskell-style); open-time
  reconciliation in BOTH open paths (the old validate ran only in
  read-only `open()` — the node never validated). Tail chunk: full CRC +
  truncate-to-verified-prefix, last entry's true end recovered by CRC scan
  (0x82-envelope candidates); index-less non-empty tail quarantined as
  `.chunk.orphaned`; damage below the tail = hard `InconsistentChunk`
  error. Cross-chunk boundary linkage (first block's prev_hash vs previous
  chunk's tip — Haskell ChunkFileDoesntFit): per-chunk checks alone PASS
  the incident DB (the orphan island is internally CRC-valid and tip.meta
  agrees with it); tail-boundary break quarantines, deeper break refuses.
- **#928 (storage)** — tip.meta trusted only when (slot,hash) == last
  indexed entry, else clamped (block_no recovered by decoding the tip
  block) and rewritten; `immutable/clean` marker (written by shutdown
  flush, removed at open-for-writing) gates mmap hash_index reuse —
  unclean stop → rebuild; flush path uses `has_verified_block` (read+CRC)
  so a phantom index entry can't suppress re-flush.
- **#927 (sync)** — with ledger < immutable, `build_known_points` offered
  the stale ledger tip FIRST and the #699 guard disconnected every peer's
  protocol-mandated initial rollback to that exact offered point (HAA
  dead, zero progress forever). Now newest-first by slot in that state,
  plus the guard exempts the initial rollback to the EXACT agreed
  intersection (slot+hash) at-or-above the ledger tip — oracle-verified
  Haskell alignment (`intersectFound` re-anchors the candidate fragment
  without any rollback-validity check; only wire rollbacks in StNext hit
  the k-bound). Exemption unreachable when ledger >= immutable; #699
  divergent-peer protection intact. Startup warns on ledger < immutable.
- **#929 (storage)** — exclusive advisory flock on `<db>/lock` in
  `ChainDB::open` (cardano-node withLockDB equivalent); second process
  fails fast naming the holder pid. Tests opening one dir twice must drop
  the first handle.

QA: full gate 7503/0; devnet-validate standard **3/3 rounds PASS**
(`reports/devnet-validate/v2.4.0.json`) — 541 canonical blocks, 0 invalid
forges, bidirectional parity 41/41 (0 off-diagonal), cli-parity clean,
adversarial 7/7, byte-exact treasury/reserves after first RUPD.
**Incident replay**: a copy of the preserved damaged db-preprod opens
under v2.4.0 to tip=(129437577, block 4983447, df28215f…) with the orphan
island quarantined — the #926 manual recovery, automated (block height
verified vs header decode + Koios; the issue text's 4983444 was an
off-by-three). **Zero open issues.**

### v2.3.1 (2026-07-30)
Patch: #925 N2C rejection diagnostics.
Drop-in from v2.3.0, SNAPSHOT unchanged at 31. Root cause was two
compounding defects: (1) `N2CClient`'s file-wide `protocol_err` hardcoded
`LocalStateQuery`/`CborDecode` for EVERY client error, including
LocalTxSubmission `MsgRejectTx` — now a dedicated `NetworkError::TxRejected`
("LocalTxSubmission: transaction rejected: …") with real protocol labels on
the LocalTxMonitor/LocalTxSubmission decode paths; (2) a Conway duplicate
input fails `decode_transaction` at the strict-set layer BEFORE Phase-1, so
the `DuplicateInput` encoder arm is unreachable for wire txs — the resulting
`DecodeFailed` had no encoder arm and fell into the generic C8 fallback. Now
`ConwayMempoolFailure(7, "transaction decode failed: <reason>")` (C8-safe:
the rejected bytes are the client's own). Haskell fails these at the codec
layer and drops the connection; dugite deliberately answers a structured
MsgRejectTx. QA: devnet-validate standard 2/2 rounds PASS
(`reports/devnet-validate/v2.3.1.json`) — 349 canonical blocks, 0 orphans,
tx-zoo 168/0, all 5 predicates green both rounds; 08f-double-spend
validates the fix on the wire. Also fixed: the dugite-monitor probe-timeout
test's wall-clock backstop (third flake of the same shape — it measured
nextest scheduling latency, not the probe; `is_none()` + the compile-time
budget guard already prove the contract). **Zero open issues.**

### v2.3.0 (2026-07-29)
Backlog sweep closing #914-#924. Two
byte-exact ledger/LSQ divergences, one remotely-triggerable connection leak,
and five harness defects that made suites report success while measuring
nothing. **Re-sync release: SNAPSHOT_VERSION 30 -> 31.**

- **#919 (ledger, SNAPSHOT 30 -> 31)** — dugite had exactly ONE min-UTxO
  helper, the Babbage `(160+size) x coinsPerUTxOByte` formula, applied in
  every era, because `ada_per_utxo_byte` is seeded from the Alonzo genesis at
  startup regardless of the chain's era. Mainnet Shelley txs with 1 ADA
  outputs were rejected at `minimum=1051640` (= 4310 x 244). Haskell defines
  `getMinCoinTxOut` per era and can never apply a Babbage calc to a Shelley
  TxOut. Now PV-dispatched: PV<=3 flat `minUTxOValue`; PV4 Mary
  `scaledMinDeposit` (ada-only short-circuits BEFORE `size`); PV5-6 Alonzo
  `(27 + size + dataHashSize) x coinsPerUTxOWord`; PV>=7 unchanged. The
  shared `Value::mary_value_size()` returns **2** for ada-only — deliberately
  "wrong for Mary, right for Alonzo" upstream, since Mary never reaches it.
  Also fixed: PPU key 15 was decoded then dropped, and key 17 is
  coinsPerUTxOWord pre-Babbage but coinsPerUTxOByte after (disambiguated by
  the PV in force before that update's own PV bump).
- **#922 (LSQ)** — `GetProposals` served the LIVE proposal set. Haskell's
  `queryProposals` never reads `cgsProposals`; it reads the DRep pulser's
  frozen `dpProposals`/`psProposals`, refreshed once per epoch boundary by
  `setFreshDRepPulsingState`, so mid-epoch submissions are invisible until
  the next boundary. Now answers from dugite's #903 ratification snapshot
  (the same `dpProposals` equivalent) — one mechanism, two bugs.
  `GetGovState`'s embedded `cgsProposals` correctly stays live.
- **#920 (network)** — the v2.2.4 trusted-only clamp gated PROMOTION only, so
  peers established during a CaughtUp period that later regressed stayed
  established and the HAA closure could still fail. Now self-healing: the
  governor demotes untrusted established outbound peers straight to Cold
  every tick the clamp holds (no cooldown, no fetch-slot exclusion — a
  planned policy teardown, not a failure), plus a one-shot sweep on the
  regression edge and a register-time gate closing the mid-handshake race.
- **#914 (ledger)** — the GOV apply path silently dropped proposals with an
  invalid `prev_action_id` under a comment claiming Haskell does the same.
  Canonical `Conway.Rules.Gov` does the opposite (`failBecause`). Now hard
  errors: reaching it on ApplyOnly means governance state already diverged
  (the #898 shape), so crash rather than corrupt pots silently.
- **#915 (network)** — `InvalidPrevGovActionId` rejections now encode as
  canonical `ConwayGovFailure` (Ledger tag 3) / GOV tag 8 carrying the full
  `ProposalProcedure`, instead of a generic reason.
- **Harness defects (#916/#917/#918/#921/#923)** — the recurring shape is a
  check that reports success while measuring nothing. The release report
  counted the substring "error" (so `error=` fields on INFO lines showed
  thousands of errors on a clean run); the forge-stall predicate was a ~3%
  per-sample coin flip on a single-forger devnet; three tx-zoo scripts
  skipped structurally on every run; `adv_send_expect_close` returned PASS
  when socat was missing, so every adversarial N2N case in protocols/01-07
  "passed" without sending a byte. Level counting is now shared by generator
  and analyzer with an agreement test; forge-stall accumulates a
  Praos-derived p99.9 gap budget; tx-zoo vendors a stdlib raw-socket writer
  and a CBOR splicer and classifies env-vs-state skips (`--strict-skips`);
  nextest has a `slow-timeout` terminate-after backstop.

- **#924 (network, found BY the validation round)** — a failed handshake
  left the TCP connection open for the process lifetime. The mux task owns the
  `TcpBearer`, and the handshake-failure early return dropped its `JoinHandle`
  — which **detaches** a tokio task rather than aborting it. Unauthenticated
  and remotely triggerable (one malformed handshake per socket), and it
  defeated the inbound connection cap since leaked sockets are never
  registered and so never counted. cardano-node closes all five malformed
  cases; dugite closed none. Fixed with a `MuxAbortGuard` on both the inbound
  and outbound paths. **It was only reachable because #923 stopped the
  adversarial suite from passing without sending bytes** — the two compound.

**#919 bumps SNAPSHOT_VERSION 30 -> 31** — existing DBs replay chunks on
first restart. Pre-v2.1.0 Mithril DBs still need a full `mithril-import`.

### QA status

devnet-validate standard preset, **3/3 rounds PASS** vs cardano-node 11.0.1
(`reports/devnet-validate/v2.3.0.json`): 522 canonical blocks, 0 orphans, 0
invalid-block events, byte-exact treasury/reserves parity after both RUPD
boundaries, tx-zoo 84 PASS / 0 FAIL / **0 env-skip**, adversarial N2N **7/7,
0 SILENT_SKIP**, cli-parity 18 EQUAL / 0 divergent (including `proposals`,
which validates #922 on the wire), bidirectional parity 34/34 identical
across both sockets, 100% tip parity every round.

Coverage caveat: bidirectional parity ran 34 scripts, not 41 —
`06-proposals` is excluded because re-submitting an already-enacted proposal
chain is rejected by BOTH implementations (parity holds; the zoo just reports
it as a failure).

**Open issues: none.** #905/#906/#912 CLOSED earlier; **#925 CLOSED in
v2.3.1** (see Current Focus — it was a `dugite-cli` mislabel plus a missing
`DecodeFailed` encoder arm, not an LSQ bug).

**Adversarial results recorded on socat-less hosts (stock macOS) before
v2.3.0 are unverified** — see #923.

Soak testing via Sandstone Pool [SAND] on preview and preprod (pool IDs:
preview `6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856`, preprod
`pool1uju7fuqzv...nh0ch`). Preview is at PV11 — requires peers running
cardano-node 11.0.1+.

### Reading the cli-parity suite

`tx-zoo/09-cli-parity` runs `cardano-cli` against **both** sockets and diffs the
answers — it never invokes `dugite-cli`. What it measures is dugite-**node**'s
LSQ responses. A failure on both sides is a harness bug, never a dugite gap
(this misreading produced four phantom "dugite-cli gaps" in #900). ERROR rows
fail the round, every divergence writes `evidence/<ts>/cli-parity-diffs/`, and
the tip is pinned across both sockets so a block applied mid-comparison cannot
manufacture a false divergence.

## Running the Node

Config files live under per-network subdirectories (`config/{mainnet,preview,preprod}/{config,topology,*-genesis}.json`). The justfile wraps the common launchers; underlying scripts live in `scripts/run/`.

```bash
# Justfile (preferred)
just mithril-import preview
just run-relay preview          # or: just run-bp preview

# Equivalent direct invocation
./target/release/dugite-node mithril-import --network-magic 2 --database-path ./db-preview
./target/release/dugite-node run \
  --config config/preview/config.json \
  --topology config/preview/topology.json \
  --database-path ./db-preview \
  --socket-path ./node.sock \
  --host-addr 0.0.0.0 --port 3001
```

Network magic: Mainnet=764824073, Preview=2, Preprod=1

## Scripts & configs at a glance

- `config/{mainnet,preview,preprod}/` — per-network configs and genesis files (self-contained, relative paths).
- `config/bp-pair/` — Sandstone preview BP-pair soak rig (dugite-bp + dugite-relay + haskell-relay).
- `config/monitoring/` — Grafana dashboard, Prometheus scrape + alert rules.
- `scripts/run/`, `scripts/soak/`, `scripts/monitoring/`, `scripts/validation/`, `scripts/mithril/`, `scripts/dev/` — see `just --list` for the entry points.
- `scripts/soak/preprod-steady-state-soak.sh [MINUTES]` — pre-release soak on the
  REAL preprod network. REUSES an existing synced DB and never wipes, which is what
  distinguishes it from `goal-soak.sh` (that one wipes and re-imports to answer "can
  a fresh node reach tip in 30 min"). Gated on catch-up before anything is sampled —
  sampling during reconvergence measures reconvergence, not steady state. Every
  predicate records the compared VALUE: tip delta vs Koios, minimum peers, ERROR
  lines, `LedgerSeq was incoherent` (absence = positive evidence #985's re-anchor
  fired), #1057 genesis declines, RSS drift. Fewer than 3 usable samples FAILS
  rather than passing vacuously.
- `testnet/local-devnet/wait-tip-parity.sh` — waits until relay, dugite-bp AND cardano-bp
  report the SAME tip block, for N consecutive samples. Run it before any `soak.sh`: p4
  scores exact parity across all three observers, so a soak started right after a
  disruption measures RECONVERGENCE and fails on noise (measured twice: 79% then 83%).
  `wait-catchup.sh` is NOT a substitute — it allows a 5-block gap by default and compares
  only cardano-bp against dugite-bp, never the relay. Both its queries are bounded,
  because a SIGSTOPped node's socket still listens and an unbounded query hangs the gate
  on the very condition it exists to catch.
- `testnet/local-devnet/genesis-fork-round.sh` — the #1057 reproduction (Round 12), now
  a regression gate: it REQUIRES live in-place adoption of a genesis-divergent chain,
  across a restart. Needs depth asymmetry (`LD_POOL2_STAKE_PCT=85`), freezes the
  producer with SIGSTOP rather than a topology reload, and measures the RELAY. Four
  separate constructions failed silently first — see the #1057 section before changing
  any precondition.

## Fuzzing

60 declared libFuzzer targets in `fuzz/`, all 60 in the nightly matrix
(no exclusions — `plutus_script_decode` was the only one, and #970 deleted it
rather than excluding it: it fuzzed the third-party Aiken `uplc` crate, which
dugite does not ship, so it could never have found a dugite defect),
run by `.github/workflows/fuzz.yml`:
Mon-Sat 1200s per target, Sunday 3600s, `workflow_dispatch` overrides.
The short weekday budget is only sound because the corpus **persists**
between runs (Actions cache + `cargo fuzz cmin` before save) — it is the
same search resumed, not a shorter one.

```bash
just fuzz-check                      # compile-guard every target (part of `just check`)
scripts/dev/regen-fuzz-seeds.sh      # rebuild fuzz/seeds/ from repo fixtures

cd fuzz
cp -n seeds/decode_block/* corpus/fuzz_decode_block/   # CI does this before each run
cargo +nightly fuzz run fuzz_decode_block -- -max_total_time=300 -max_len=32768
```

Three traps, all of which shipped silently for months (#971/#972):

- **A target not in `matrix.target` never runs.** Eleven sat
  declared-but-dead for 2.5 months. `xtask/tests/fuzz_matrix_coverage.rs`
  now fails the build on that drift; document deliberate exclusions in its
  `DOCUMENTED_EXCLUSIONS`.
- **`fuzz/` declares its own `[workspace]`**, so a plain
  `cargo build --all-targets` at the repo root does not compile, format or
  lint it. Use `just fuzz-check`.
- **Seeds live in `fuzz/seeds/<target>/`, not `fuzz/corpus/`** — cargo-fuzz
  owns `corpus/fuzz_<target>/` (note the prefix; it comes from the BIN
  name). A seed must also fit `-max_len` (libFuzzer *truncates* rather than
  skipping: cov 84 vs 1331 on the same 29 KB block) and match the target's
  byte layout, since several read a control prefix before the payload.

Encoder targets come in two directions and are complementary:
`fuzz_encode_roundtrip` is decode-first and reaches real-world encodings no
generator would invent; `fuzz_structured_tx_encode` /
`fuzz_structured_pparam_update` generate the structure and reach the deep
optional fields (PPU keys, gov certificates, witness sets) that byte
mutation cannot synthesise. Standing caveat: a same-process round-trip
cannot catch a wrong shape shared by BOTH halves — Haskell-derived fixtures
remain the oracle.

`dugite-node` is deliberately **not** a fuzz dependency: it pulls in
mithril-client, whose native deps and `inventory`/`typetag` static
initializers do not survive sancov instrumentation. Its parsers are
compiled in via `#[path]` instead — see `fuzz/src/node/n2c_query/mod.rs`.

## Upstream Conformance Testing

Dugite maintains byte-exact alignment with upstream Cardano implementations
via a republished corpus. Every upstream artefact flows through a single
pipeline (`scripts/regenerate-conformance-corpus/`) and is published as a
dugite GitHub release pinned in `tests/conformance/upstream/manifest.toml`.

### Daily workflow

```bash
# Download all upstream fixture areas (reads manifest.toml for the release tag)
just download-upstream-fixtures

# Run the full UPLC + upstream golden test suite
just test-conformance

# Run a single area
cargo xtask download-upstream-fixtures --area ledger-rules
DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance \
  --features upstream-conformance --test upstream_tests
```

### Seven fixture areas

| Area | Source | Content |
|------|--------|---------|
| `ouroboros-consensus` | IntersectMBO/ouroboros-consensus | Block/header golden files per era |
| `cardano-ledger` | IntersectMBO/cardano-ledger | Genesis JSON, CDDL schema, golden txs |
| `cardano-node` | IntersectMBO/cardano-node | Genesis spec files |
| `plutus` | IntersectMBO/plutus | 999 UPLC evaluation test cases |
| `ledger-rules` | ImpSpec dump of cardano-ledger | CBOR ImpSpec vectors (NEWEPOCH + LEDGER) |
| `cardano-base` | IntersectMBO/cardano-base | VRF v03 crypto test vectors |
| `mithril` | input-output-hk/mithril | Certificate fixture JSON |

### Refreshing the corpus

1. Edit `tests/conformance/upstream/sources.toml` to bump a pin.
2. Trigger `.github/workflows/regenerate-conformance-corpus.yml` (manual dispatch or weekly auto).
3. Update `[release].tag` in `tests/conformance/upstream/manifest.toml`.
4. Run `just download-upstream-fixtures && just test-conformance`.
5. Commit `sources.toml` + `manifest.toml` + any code changes.

The `ledger-rules` area builds cardano-ledger from source (GHC 9.6.5 +
cabal 3.10.x, ~35 min cold, ~5 min cached) and runs the official ImpSpec
conformance suite with `CONFORMANCE_CBOR_DUMP_PATH` set to capture every
test vector. Phase 4 acceptance: `SKIP_LIST` in
`tests/conformance/src/upstream/ledger_rules_replay/mod.rs` is empty or
every entry has a tracking issue.

### CI

The `upstream-conformance` job in `.github/workflows/ci.yml` runs both the
UPLC and upstream golden suites with `DUGITE_REQUIRE_UPSTREAM=1`. Fixture
cache is keyed on `manifest.toml` content hash; bumping the tag invalidates
the cache automatically.
