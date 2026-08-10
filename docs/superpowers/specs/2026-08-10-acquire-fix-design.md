# Fix design: make the acquired point the actual volatile tip

Defect: `docs/superpowers/specs/2026-08-10-acquire-pins-a-stale-volatile-tip.md`.
This is the design AFTER an adversarial FABLE review, which refuted a
load-bearing claim in the first draft. Recorded because two of the findings are
not obvious and would be re-derived expensively.

## What the review killed

**Draft claim, FALSE:** "every field that can differ within an epoch is
current". It contradicted the draft's own table — `stake_pools`,
`drep_entries`, `governance_proposals` stay on the slow cadence — and the table
also omitted `stake_addresses` (reward balances, change on every withdrawal),
`committee`, `stake_deleg_deposits` and `era`.

Honest framing instead: today's code is session-**consistent at a wrong
point** (the whole snapshot is built under one `ls` read lock, `query.rs:282`).
The fix trades that for **right point, bounded collection lag** — a narrower
tear than the defect it removes, but a tear. Claim that, not consistency.

Observable pairs, in one session:
- `GetChainPoint` (tip block carries a reward withdrawal) + tag 10
  `GetFilteredDelegationsAndRewardAccounts` (stale) — the withdrawn funds show
  in the change UTxO *and* still in the reward account.
- `GetChainPoint` (tip block carries a pool registration) + tag 16
  `GetStakePools` (stale) — confirmed-but-absent.

**Upstream requires the collections at the acquired point too.**
`openStateRefAtTarget` (`Storage/LedgerDB/V2.hs:561-591`,
`release-ouroboros-consensus-3.0.1.0`) pins the whole `l EmptyMK` plus a
duplicated table handle in one atomic read of the TVar chain selection commits
into. `answerQuery` reads only the forker, and `GetCurrentPParams`,
`GetStakePools`, `GetGovState`, `GetProposals`, `GetPoolDistr` are all
`QFNoTables` — answered purely from the pinned state. There is no cadence
upstream. So this fix is a bounded approximation, not upstream's shape.

## Reading the ledger at acquire: possible, but priced

`QueryHandler::acquire` is a SYNCHRONOUS trait method taking `&self`
(`dugite-network/src/protocol/local_state_query/server.rs:46`), so it cannot
`.await` the ledger `RwLock` — the cross-crate constraint that shaped #1068.

An earlier draft of this document concluded from that alone that the review's
preferred "capture the header at acquire from the live ledger" was **not
implementable**. That was too strong, and the counter-example is inside
`acquire` itself: the `SpecificPoint` arm already does

```rust
tokio::task::block_in_place(|| {
    let db = chain_db.blocking_read();
    db.has_block(&block_hash)
})
```

So the pattern exists and would work for the ledger too. It is not free:
`QueryHandler` holds no ledger reference today (only `chain_db`), so one must
be added; `block_in_place` moves the task off its worker and requires the
multi-threaded runtime; and taking the ledger read lock on EVERY acquire puts
query latency behind block application, which indexers acquire often enough to
notice.

Both shapes are therefore available:

| | cost at acquire | cost per block | staleness |
|---|---|---|---|
| publish a cheap header per block | none | one small lock-free write | none |
| read the live ledger at acquire | `block_in_place` + ledger read lock | none | none |

Prefer the published header: it keeps `acquire` allocation-free and lock-free,
which is what the trait's own contract asks for ("must be cheap to produce").
The ledger-read variant stays a legitimate fallback if publishing turns out to
need more plumbing than expected — recorded so the option is not re-derived.

## Two things that make the epoch-frozen argument weaker than assumed

- `governance_proposals_frozen` is **not** frozen when no pulsing snapshot
  exists: `query.rs:492-503` falls back to a clone of the LIVE list. That is
  precisely a Conway-from-genesis devnet before its first boundary — where the
  gate runs.
- `protocol_params` has a non-boundary writer: `ledger_seq.rs:1617`, the
  rollback path restoring params from an epoch-transition delta.

Therefore the forced heavy rebuild must key on **`published_epoch !=
ledger_epoch`** (an inequality, evaluated on every publish path), not on "this
block crossed a boundary" — the inequality also covers a rollback that
un-rotates an epoch backwards.

## Shape

`NodeStateSnapshot` cannot be cheaply cloned: `stake_addresses` (~1.5M entries
at mainnet), three full mark/set/go `SnapshotStakeData`, `stake_snapshots`,
`gen_delegs`, plus the collections. So "correct the header at acquire" needs
either a header/heavy split in `Acquired`, or the heavy fields moved behind
`Arc` so the outer clone is cheap. The latter is smaller: `Arc<Vec<T>>`
auto-derefs, so read sites are unchanged and only the construction sites in
`query.rs` need wrapping, and `type Acquired = Arc<NodeStateSnapshot>` stays —
leaving all four handler files untouched.

## Publish sites — the completeness requirement

The review's stated most-likely failure is the fix landing on the live-tip
apply path only, while the devnet gate (at tip, forkless) reports green: #977's
shape. The header publish must cover **all** of:

| site | path |
|---|---|
| `mod.rs:7915` | `apply_fetched_block` — the live-tip path |
| `mod.rs:7146` | fork-replay final (deliberately once per replay) |
| `mod.rs:10789` | forge-adopt |
| `sync.rs:2363` | `process_forward_blocks` — its OWN 30 s cadence, a second subsystem |

A fork switch also leaves the snapshot naming an abandoned block until the
rate window elapses; an acquire then pins a point `undo_diffs` cannot find.

## `unpinnable()` is reachable today, twice

Its comment — "acquire only pins the current tip" — is the false premise that
hid this defect, and both its claims are false:

- **catch-up**: 30 s of staleness at bulk rates puts the pinned point far
  outside the k-delta LedgerSeq window, so `undo_diffs` returns `None` and
  EVERY UTxO query fails; re-acquiring pins another stale point, so the
  advertised "re-acquire and retry" never succeeds until catch-up ends.
- **fork switch mid-session**: the pinned block's deltas are discarded.
  Upstream answers here (the forker's StateRef survives chain selection);
  dugite refuses.

Refusing stays correct for the fork case — answering live would be the silent
tear #1068 removed. The comment must be rewritten. Note the unclaimed upside:
a current point makes catch-up UTxO queries go from always-failing to working,
because the undo plan becomes empty.

## Tests the fix must carry

No existing test pins the staleness — that is why it shipped. `utxo.rs`'s mock
provider **ignores the `at` parameter entirely**, so every unit test there is
structurally blind to this family and should be fixed while in the area.

1. **RED**: publish at B1; apply B2 spending `u` inside the rate window so the
   REAL limiter skips (not a stub that "doesn't refresh"); acquire; query
   `GetUTxOByTxIn(u)` through the real provider. Assert absent and
   `GetChainPoint == B2`. Today this fails for the right reason — #1068's undo
   machinery faithfully RESTORES `u` from B2's delta.
2. **Anti-regression**: session pinned at B2, apply B3 spending `v`; the same
   session must still see `v` present. Guards against degenerating into live
   reads.
3. **Boundary force**: boundary block landing <1 s after a refresh; assert the
   published `pool_distr` is the new `set` snapshot and the snapshot epoch is
   the new epoch.
4. **System**: 5 Hz `query utxo` across a devnet inter-block gap — the spent
   input must vanish within the gap, not at the next `Chain extended`. A
   RED-proven unit test bounds the function, not the system (#1057).
