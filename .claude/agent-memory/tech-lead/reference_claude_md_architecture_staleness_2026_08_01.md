---
name: claude-md-architecture-staleness-2026-08-01
description: Specific points where CLAUDE.md's Architecture section / tech-lead persona domain summary had drifted from crates/ as of 2026-08-01 — verify against code, not the doc, for these
metadata:
  type: reference
---

During a full audit of `docs/src/architecture/*.md` against `crates/` (2026-08-01), several
claims baked into CLAUDE.md's Architecture section (and this agent's own persona domain summary)
were found stale vs. current code. CLAUDE.md was NOT edited (out of scope for that task), so
future sessions should know to verify these directly against code rather than trust the doc:

- **Crate count**: CLAUDE.md says "15-crate workspace" and omits `dugite-rpc` entirely.
  `crates/` actually has 16 crate directories (add `dugite-rpc` — a native UTxO RPC/gRPC server,
  issue #672, depended on by `dugite-node`).
- **Reward `Rat` arithmetic**: CLAUDE.md's Key Invariants list says "cross-reduce before mul/add
  to prevent i128 overflow." The current implementation (`dugite-ledger/src/state/rewards.rs`)
  uses `num_bigint::BigInt`, not i128 — the doc comment there explicitly says the previous i128 +
  BigInt-fallback implementation silently saturated to `i128::MAX` on mainnet-scale values and was
  replaced. The "cross-reduce i128" description is the OLD, superseded design.
- **BlockFetch "4 concurrent fetchers"**: CLAUDE.md's Networking domain summary says "multi-fetcher
  architecture (4 concurrent block fetchers)". The real production path is single-fetcher
  (`bfcMaxConcurrencyBulkSync = 1` parity) — see [[reference_block_fetch_logic_dead_code_trap]].
- **P2P governor tick interval**: actual `governor_ticker` in `node/mod.rs` is
  `Duration::from_secs(2)`, not 30s (multiple docs pages had this wrong before the 2026-08-01 fix).
- **Peer target defaults**: actual code (`config.rs` `NodeConfig::default()`) is
  known=150, established=30, active=20 (Haskell cardano-node defaults) — not 85/30(or 40)/15 as
  several doc pages previously claimed.
- **PeerCategory / circuit breaker / subnet diversity**: none of these exist anywhere in
  `crates/dugite-network/src/peer/` (zero grep hits, including tests) as of 2026-08-01. Big-ledger-peer
  and local-root membership are tracked as separate `HashSet`/group-list parameters passed into the
  governor, not a per-peer category enum. `PeerState` is actually four-valued: `Cold < Cooling <
  Warm < Hot` (the `Cooling` state — a TCP TIME_WAIT analogue — is easy to miss).

See [[project_docs_architecture_audit_2026_08_01]] for the full page-by-page fix list.
