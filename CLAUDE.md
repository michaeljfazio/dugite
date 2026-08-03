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
**v2.5.0 (2026-08-03)** — release-gate coverage wave. **RE-SYNC RELEASE:
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

## Fuzzing

59 declared libFuzzer targets in `fuzz/`, 58 in the nightly matrix
(`plutus_script_decode` is deliberately excluded — upstream Aiken panics),
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
