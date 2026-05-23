---
name: issue-609-snapshot-version-quarantine
description: SNAPSHOT_VERSION bump silently wiped chain — guard + quarantine prevents recurrence
metadata:
  type: project
---

# Issue #609 — SNAPSHOT_VERSION bump silently wipes ledger snapshot

**Fix landed in** `crates/dugite-ledger/src/state/snapshot.rs` (Option B from the issue: fail-fast guard + quarantine, ChainDB untouched, caller falls back to chain replay).

**Why:** Prior to the fix the v15→v16 bump (PlutusV4 cost-model slot) produced
the user-visible cascade:

  1. `WARN: Snapshot version mismatch snapshot_version=15 current_version=16`
  2. Loader ignored the warning and called bincode anyway → `tag for enum is
     not valid, found 65` (the shifted `CostModels` field boundary surfaces as
     a corrupted enum tag elsewhere in the stream).
  3. Caller (`node/mod.rs:1036`) swallowed the error as
     `Failed to load ledger snapshot, starting fresh`, ran `init_fresh_ledger`,
     and the node spent the next many hours replaying from genesis.

**How to apply:**

* **Bumping `SNAPSHOT_VERSION` is now ALWAYS a breaking change for users with
  existing snapshots.** The release process must call this out explicitly so
  operators know the first restart on a new binary will trigger a chain replay
  (minutes-to-hours of startup time depending on tip distance).
* If a future bump genuinely requires forward migration (e.g. mainnet
  operators with day-long replay costs), add a `migrate_vN_to_vM()` shim in
  `snapshot.rs` and call it **before** the version-mismatch guard at
  `load_snapshot()`. The shim needs a duplicate copy of the *prior*
  `LedgerStateSnapshot` shape (and any transitively-changed types like
  `CostModels`) so bincode can decode the old wire format positionally.
* Quarantine path: `<name>.bin` → `<name>.bin.v{N}-unreadable`. The
  `.bin`-suffix filter in `dugite_node::startup::enumerate_snapshots` no
  longer matches, so the next restart skips it (otherwise we would loop on
  the same unreadable file forever).
* Two tests pin the behaviour:
  - `test_version_mismatch_returns_error_and_does_not_attempt_decode` —
    error must mention both versions and must NOT contain the
    bincode-internal "tag for enum is not valid" string.
  - `test_version_mismatch_quarantines_original_file` — original `.bin` is
    gone, `*.vNN-unreadable` exists with original DUGT magic + version byte
    preserved.

**Constraints discovered:**

* Bincode is positional and not self-describing — adding a single
  `Option<T>` anywhere except the very end of the outermost serialised
  struct shifts every subsequent byte boundary. v15 → v16 changed one
  `Option` in `CostModels` (nested in `ProtocolParameters`), which is
  enough to break the entire stream.
* `init_fresh_ledger` in `node/mod.rs` does NOT delete ChainDB — the
  "deletes entire chain db" in the user report was the *functional* effect
  (re-sync from genesis). Snapshot-only fix preserves both ImmutableDB and
  VolatileDB on disk; only the ledger snapshot file is renamed.

Related: [[forge-connectivity-gate-bug-c]] (other startup-path silent failures).
