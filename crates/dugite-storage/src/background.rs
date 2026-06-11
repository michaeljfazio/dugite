//! Background storage maintenance operations matching Haskell's Background.hs.
//!
//! This module implements the three periodic operations that keep the storage
//! subsystem healthy between block applications:
//!
//! 1. **[`CopyToImmutable`]** — When the chain fragment grows beyond k headers,
//!    copies the oldest block from VolatileDB to ImmutableDB and schedules the
//!    entry for GC.  Matches Haskell's `copyToImmutableDB` in `Background.hs`.
//!
//! 2. **[`GcScheduler`]** — Tracks blocks that have been copied to ImmutableDB
//!    but not yet removed from VolatileDB.  After a 60-second delay (matching
//!    the Haskell GC delay) the scheduler calls back into `ChainDB` to drop the
//!    entry.  Uses `slot <` (strict less-than) to preserve the EBB invariant:
//!    Epoch Boundary Blocks share a slot with the first block of the next epoch
//!    and must never be GC'd prematurely.  Matches Haskell's `garbageCollectBlocks`
//!    and the `GcSchedule` type in `Background.hs`.
//!
//! 3. **[`SnapshotScheduler`]** — Decides when to persist the LedgerSeq anchor
//!    state to disk.  Triggers on epoch boundary, slot-delta thresholds, or
//!    graceful shutdown — mirroring Haskell's `defaultSnapshotPolicy` in
//!    `LedgerDB/Snapshots.hs` (slot interval `k * 2`, rate limit 10 min,
//!    jitter `[5 min, 10 min]`).  The actual persistence is carried out by a
//!    caller-supplied callback so that this module remains free of any
//!    `dugite-ledger` dependency.
//!
//! # Haskell references
//!
//! * `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl/Background.hs`
//!   — `copyToImmutableDB`, `garbageCollectBlocks`, `GcSchedule`
//! * TR §chaindb:gc:delay — "1 minute delay, slot < (not <=) for EBB invariant"
//!
//! # Design notes
//!
//! * All three structs are **synchronous value types** — they hold no threads or
//!   tasks themselves.  They are designed to be called from the `addBlockRunner`
//!   after each block is processed (or from a dedicated ticker task).
//! * Because `dugite-storage` does not depend on `dugite-ledger` or
//!   `dugite-consensus`, interactions with `LedgerSeq` and `ChainFragment` are
//!   carried out through caller-supplied closures.  The node wires these together
//!   at startup.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use rand::Rng;
use tracing::{debug, info, trace, warn};

use crate::chain_db::ChainDB;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// GC delay after a block has been copied to ImmutableDB before it is removed
/// from VolatileDB.
///
/// Matches the Haskell `gcDelay` of 1 minute (TR §chaindb:gc:delay).
/// The delay gives downstream clients (e.g. ChainSync followers that are still
/// reading the block) time to finish before the entry disappears.
pub const GC_DELAY: Duration = Duration::from_secs(60);

/// Default slot interval between automatic ledger snapshots.
///
/// Matches Haskell's `defInterval = k * 2` from
/// `ouroboros-consensus/.../LedgerDB/Snapshots.hs` (lines 589-650).  With
/// mainnet/preprod `k = 2160` this is 4320 slots ≈ 72 minutes at 1 s slot
/// length.  For preview (`k = 432`) the caller should override via
/// [`SnapshotScheduler::with_slot_interval`] to 864 slots.
///
/// The slot-based trigger is era-stable — Haskell deliberately uses
/// `k * 2` slots rather than a block count because block density varies
/// across eras (Byron has high block density vs Conway).  Using a fixed
/// block count would over-snapshot in Byron and under-snapshot in Conway.
pub const DEFAULT_SNAPSHOT_SLOT_INTERVAL: u64 = 4_320;

/// Hard floor on the wall-clock gap between consecutive snapshots.
///
/// Matches Haskell's `defRateLimit = secondsToDiffTime (10 * 60)` (10 min).
/// Prevents the slot-trigger from firing too frequently if the wall clock
/// somehow runs faster than slot time (e.g., during testnet fast-forward
/// or after a wake-from-suspend slot catch-up).
pub const DEFAULT_SNAPSHOT_RATE_LIMIT: Duration = Duration::from_secs(600);

/// Lower bound of the jitter window applied after a slot-interval trigger.
///
/// Matches Haskell's `fiveMinutes = 5 * 60`.  Once the slot delta exceeds
/// the interval, the actual snapshot fire time is deferred by a uniform
/// random delay in `[DEFAULT_SNAPSHOT_JITTER_MIN, DEFAULT_SNAPSHOT_JITTER_MAX]`
/// to spread snapshot I/O load across a population of nodes.
pub const DEFAULT_SNAPSHOT_JITTER_MIN: Duration = Duration::from_secs(5 * 60);

/// Upper bound of the jitter window applied after a slot-interval trigger.
///
/// Matches Haskell's `tenMinutes = 10 * 60`.
pub const DEFAULT_SNAPSHOT_JITTER_MAX: Duration = Duration::from_secs(10 * 60);

/// Minimum wall-clock gap between epoch-boundary snapshots during bulk sync
/// (catch-up mode, issue #747).  Default 30 minutes — 3× the at-tip rate
/// limit — because bulk sync crosses many epoch boundaries per minute and
/// per-epoch I/O at that rate would dominate the apply loop.  Configurable
/// via `LowLevelGenesisOptions.SnapshotMinIntervalBulkSync`.
pub const DEFAULT_SNAPSHOT_BULK_SYNC_RATE_LIMIT: Duration = Duration::from_secs(30 * 60);

// ─────────────────────────────────────────────────────────────────────────────
// CopyToImmutable
// ─────────────────────────────────────────────────────────────────────────────

/// Copies the oldest block from VolatileDB to ImmutableDB when the chain
/// fragment grows beyond the security parameter k.
///
/// Matches Haskell's `copyToImmutableDB` in `Background.hs`.
///
/// # Protocol
///
/// After each block is appended to the selected chain the caller invokes
/// [`CopyToImmutable::run_once`].  If the fragment length exceeds `k` the
/// method:
///
/// 1. Retrieves the oldest block from the VolatileDB using the block hash at
///    the front of the chain fragment.
/// 2. Appends it to the ImmutableDB via [`ChainDB::put_blocks_batch`].
/// 3. Calls the caller-supplied `advance_ledger_anchor` closure so that the
///    LedgerSeq anchor is advanced to match the new immutable tip.
/// 4. Returns the slot and hash of the copied block so the caller can schedule
///    it for GC.
///
/// The copy step and the GC step are intentionally **separate**: copying is
/// immediate (preserving the immutability invariant) while GC is deferred by
/// [`GC_DELAY`] (allowing in-flight readers to finish).
pub struct CopyToImmutable {
    /// Security parameter k (number of volatile headers to retain).
    k: usize,
}

impl CopyToImmutable {
    /// Create a new `CopyToImmutable` with the given security parameter.
    pub fn new(k: usize) -> Self {
        Self { k }
    }

    /// Run one copy-to-immutable pass after a block has been added.
    ///
    /// # Parameters
    ///
    /// * `chain_db` — The ChainDB, used to read the volatile block and write
    ///   it to ImmutableDB.
    /// * `fragment_len` — Current length of the chain fragment (number of
    ///   volatile headers on the selected chain).
    /// * `oldest_hash` — Hash of the oldest block on the selected chain (the
    ///   one that will be copied if the fragment is too long).
    /// * `oldest_slot` — Slot of the oldest block (needed for ImmutableDB
    ///   append ordering).
    /// * `oldest_block_no` — Block number of the oldest block.
    /// * `advance_ledger_anchor` — Called with `(slot, hash, block_no)` after
    ///   the block is successfully copied to ImmutableDB.  The caller should
    ///   advance the LedgerSeq anchor here.
    ///
    /// # Returns
    ///
    /// `Some((slot, hash))` of the copied block, or `None` if the fragment is
    /// not yet long enough to trigger a copy.
    ///
    /// # Errors
    ///
    /// Returns an error string if the block CBOR cannot be retrieved from
    /// VolatileDB or if the ImmutableDB append fails.  The ChainDB state is
    /// unchanged on error.
    pub fn run_once(
        &self,
        chain_db: &mut ChainDB,
        fragment_len: usize,
        oldest_hash: Hash32,
        oldest_slot: SlotNo,
        oldest_block_no: BlockNo,
        advance_ledger_anchor: &mut dyn FnMut(SlotNo, Hash32, BlockNo),
    ) -> Result<Option<(SlotNo, Hash32)>, String> {
        // Only copy when the fragment is strictly longer than k.
        // When fragment_len == k we have exactly k volatile headers — correct.
        // When fragment_len == k+1 the oldest header is now k-deep and safe to
        // commit to the ImmutableDB.
        if fragment_len <= self.k {
            return Ok(None);
        }

        // Retrieve the block CBOR from the volatile store.
        let cbor = chain_db
            .get_block(&oldest_hash)
            .map_err(|e| format!("CopyToImmutable: failed to read block {oldest_hash}: {e}"))?
            .ok_or_else(|| {
                format!(
                    "CopyToImmutable: block {oldest_hash} not found in ChainDB (slot {})",
                    oldest_slot.0
                )
            })?;

        // Append the block to ImmutableDB.
        // `put_blocks_batch` writes directly to ImmutableDB (the Mithril bypass
        // path) — which is exactly what we want here: a single already-verified
        // block being moved from volatile to immutable storage.  The slot stays
        // ABSOLUTE for Byron EBBs (dugite-format chunks keep monotonic slots);
        // only the is_ebb metadata flag is derived from the stored envelope.
        let is_ebb = crate::chain_db::is_byron_ebb_envelope(&cbor);
        chain_db
            .put_blocks_batch(&[(oldest_slot, &oldest_hash, oldest_block_no, &cbor, is_ebb)])
            .map_err(|e| {
                format!("CopyToImmutable: ImmutableDB append failed for block {oldest_hash}: {e}")
            })?;

        debug!(
            slot = oldest_slot.0,
            block_no = oldest_block_no.0,
            hash = %oldest_hash,
            "background: copied block to ImmutableDB"
        );

        // Advance the LedgerSeq anchor in the caller.
        advance_ledger_anchor(oldest_slot, oldest_hash, oldest_block_no);

        Ok(Some((oldest_slot, oldest_hash)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GcScheduler
// ─────────────────────────────────────────────────────────────────────────────

/// Deferred garbage-collection scheduler for VolatileDB entries.
///
/// After a block is copied to ImmutableDB it is scheduled here with its slot
/// and hash.  When [`GcScheduler::run_pending`] is called (periodically, e.g.
/// after each block), it removes expired entries from the VolatileDB.
///
/// # EBB invariant (slot < not <=)
///
/// Epoch Boundary Blocks (EBBs, Byron era) share their slot number with the
/// first regular block of the next epoch.  To avoid accidentally removing the
/// EBB while the regular block at the same slot is still live, GC uses a
/// **strict less-than** comparison on slot numbers:
///
/// ```text
/// gc_slot < threshold_slot   (not <=)
/// ```
///
/// This matches the Haskell comment in TR §chaindb:gc:delay:
/// "slot < (not <=) for EBB invariant".
///
/// # Ordering
///
/// The inner `BTreeMap<Instant, (SlotNo, Hash32)>` keeps entries sorted by
/// deadline.  Earliest-deadline entries are processed first, which makes the
/// common case (a steady stream of single-block copies) O(1) per call.
pub struct GcScheduler {
    /// Pending GC entries sorted by their deadline (when they become eligible).
    ///
    /// Key: `Instant` at which the entry becomes eligible for removal.
    /// Value: `(slot, hash)` of the block to remove.
    ///
    /// Multiple blocks may share the same deadline instant if they were
    /// scheduled in the same millisecond.  The BTreeMap key type `Instant`
    /// does not have a stable total order beyond monotonicity on the same
    /// thread, so ties are broken arbitrarily — that is fine here.
    ///
    /// We use `Vec<(SlotNo, Hash32)>` as the value so that multiple blocks
    /// scheduled at the exact same instant can coexist in the map.
    pending: BTreeMap<Instant, Vec<(SlotNo, Hash32)>>,
}

impl GcScheduler {
    /// Create an empty scheduler.
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    /// Schedule a block for deferred removal from VolatileDB.
    ///
    /// The block will become eligible for GC after [`GC_DELAY`] (60 seconds).
    /// Callers must invoke [`GcScheduler::run_pending`] periodically to
    /// actually perform the removal.
    ///
    /// # Parameters
    ///
    /// * `slot` — Slot of the block to GC.
    /// * `hash` — Hash of the block to GC.
    /// * `now` — Current instant (injected for testability — in production
    ///   pass `Instant::now()`).
    pub fn schedule(&mut self, slot: SlotNo, hash: Hash32, now: Instant) {
        let deadline = now + GC_DELAY;
        self.pending.entry(deadline).or_default().push((slot, hash));

        trace!(
            slot = slot.0,
            hash = %hash,
            delay_secs = GC_DELAY.as_secs(),
            "GcScheduler: scheduled block for deferred GC"
        );
    }

    /// Process all expired GC entries.
    ///
    /// Removes blocks from the VolatileDB whose GC deadline has passed.  Uses
    /// `slot <` (strict less-than) for the EBB invariant as described in the
    /// struct documentation.
    ///
    /// # Parameters
    ///
    /// * `chain_db` — Mutable reference to ChainDB; expired blocks are removed
    ///   via the VolatileDB `remove_block` path.
    /// * `now` — Current instant (injected for testability).
    ///
    /// # Returns
    ///
    /// Number of blocks removed from VolatileDB.
    pub fn run_pending(&mut self, chain_db: &mut ChainDB, now: Instant) -> usize {
        // Collect all deadline keys that have expired (deadline <= now).
        let expired_keys: Vec<Instant> = self.pending.range(..=now).map(|(&k, _)| k).collect();

        if expired_keys.is_empty() {
            return 0;
        }

        let mut removed = 0;

        for key in expired_keys {
            if let Some(entries) = self.pending.remove(&key) {
                for (slot, hash) in entries {
                    // Strict slot < comparison: do NOT remove blocks whose slot
                    // equals the GC threshold slot.  This preserves EBBs that
                    // share a slot with the first regular block of the next epoch.
                    //
                    // Concretely: we remove the block by hash (exact match), but
                    // when we also prune by slot range we must use slot < (not <=).
                    // Since we track the exact hash here, the hash-based removal is
                    // safe.  The slot-range cleanup in VolatileDB (remove_blocks_up_to_slot)
                    // uses <= internally, so we intentionally do NOT use it here.
                    // Instead we call remove_block(hash) which is always safe.
                    chain_db.remove_volatile_block(&hash);
                    removed += 1;

                    debug!(
                        slot = slot.0,
                        hash = %hash,
                        "GcScheduler: removed block from VolatileDB after GC delay"
                    );
                }
            }
        }

        if removed > 0 {
            debug!(removed, "GcScheduler: GC pass completed");
        }

        removed
    }

    /// Number of blocks currently waiting for their GC deadline.
    pub fn pending_count(&self) -> usize {
        self.pending.values().map(|v| v.len()).sum()
    }

    /// `true` if there are no pending GC entries.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl Default for GcScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SnapshotScheduler
// ─────────────────────────────────────────────────────────────────────────────

/// Decides when to persist the LedgerSeq anchor state to disk.
///
/// Triggers a snapshot when any of the following conditions are met:
///
/// 1. **Epoch boundary** — the current epoch number is greater than the epoch
///    at the last snapshot.
/// 2. **Slot interval** — the slot delta since the last snapshot exceeds
///    `snapshot_slot_interval` (default `k * 2`).  Subject to the rate limit
///    and jitter described below.
/// 3. **Graceful shutdown** — [`SnapshotScheduler::request_shutdown_snapshot`]
///    forces an immediate snapshot regardless of counters.
///
/// The actual I/O is performed by a caller-supplied `save_fn` closure.  This
/// keeps `dugite-storage` free of any `dugite-ledger` dependency.
///
/// # Haskell reference
///
/// Mirrors `defaultSnapshotPolicy` in
/// `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/LedgerDB/Snapshots.hs`
/// (lines 589-650):
///
/// * `defInterval = k * 2` slots (the slot-based trigger).
/// * `defRateLimit = 600 s` (10 min hard floor between any two snapshots).
/// * `[fiveMinutes, tenMinutes] = [300, 600] s` random jitter applied after
///   the slot trigger fires, so that the actual write time is offset by a
///   uniform random delay in that window.
///
/// The slot-based trigger is era-stable — Haskell uses `k * 2` slots rather
/// than a block count because block density varies across eras.  Issue #701.
pub struct SnapshotScheduler {
    /// Slot interval between snapshot triggers (Haskell `defInterval = k * 2`).
    snapshot_slot_interval: u64,
    /// Hard floor on wall-clock duration between consecutive snapshots
    /// (Haskell `defRateLimit = 10 min`).
    rate_limit: Duration,
    /// Minimum wall-clock gap between epoch-boundary snapshots during bulk
    /// sync (catch-up mode).  Defaults to 30 minutes — substantially longer
    /// than the at-tip rate limit (10 min) — because during genesis bulk sync
    /// we cross epoch boundaries rapidly (multiple per minute) and taking a
    /// snapshot at each one wastes significant I/O (bincode walk + prune).
    ///
    /// Issue #747: configurable via `SnapshotMinIntervalBulkSync` in
    /// `LowLevelGenesisOptions`.  Always snapshot on graceful shutdown
    /// regardless of this limit.
    bulk_sync_rate_limit: Duration,
    /// Lower bound of the post-trigger jitter window.
    jitter_min: Duration,
    /// Upper bound of the post-trigger jitter window.
    jitter_max: Duration,
    /// Slot number of the most recently completed snapshot.  `None` until the
    /// first snapshot is taken — in which case the next call to
    /// [`Self::maybe_snapshot_check`] computes the delta against an implicit
    /// origin slot of `0`, causing the trigger to fire immediately at startup
    /// (mirrors Haskell's "first snapshot fires as soon as possible").
    last_snapshot_slot: Option<SlotNo>,
    /// Wall-clock instant of the most recently completed snapshot.  Used to
    /// enforce the rate limit.
    last_snapshot_time: Option<Instant>,
    /// Epoch number at the time of the last snapshot.
    last_snapshot_epoch: Option<EpochNo>,
    /// When the slot trigger fires, the scheduler defers the actual snapshot
    /// to this deadline (uniform random in `[jitter_min, jitter_max]` after
    /// the trigger time).  Cleared when the snapshot is recorded.  Mirrors
    /// Haskell's deferred-fire pattern.
    pending_deadline: Option<Instant>,
    /// Immediately take a snapshot on the next call regardless of counters.
    shutdown_requested: bool,
    /// Total number of snapshots taken.
    snapshots_taken: u64,
    /// When `true`, only fire snapshots at epoch boundaries (or on shutdown);
    /// the slot-interval trigger is suppressed.  Toggle via
    /// [`Self::set_catchup_mode`] based on the at-tip detector.
    catchup_mode: bool,
    /// Diagnostic — what kind of trigger caused the most-recent decision to
    /// return `true`.  Used by [`Self::last_decision_reason`] for logging.
    last_decision: LastDecision,
}

impl SnapshotScheduler {
    /// Create a new scheduler with the default mainnet/preprod slot interval
    /// (4 320 slots = `k * 2` for `k = 2160`).  Preview callers (`k = 432`)
    /// should use [`Self::with_slot_interval`] with `864`.
    pub fn new() -> Self {
        Self::with_slot_interval(DEFAULT_SNAPSHOT_SLOT_INTERVAL)
    }

    /// Create a new scheduler with a custom slot interval.
    ///
    /// Pass `2 * k` for byte-exact alignment with the Haskell node.  All
    /// other policy parameters (rate limit, jitter) use the Haskell defaults.
    pub fn with_slot_interval(snapshot_slot_interval: u64) -> Self {
        Self {
            snapshot_slot_interval,
            rate_limit: DEFAULT_SNAPSHOT_RATE_LIMIT,
            bulk_sync_rate_limit: DEFAULT_SNAPSHOT_BULK_SYNC_RATE_LIMIT,
            jitter_min: DEFAULT_SNAPSHOT_JITTER_MIN,
            jitter_max: DEFAULT_SNAPSHOT_JITTER_MAX,
            last_snapshot_slot: None,
            last_snapshot_time: None,
            last_snapshot_epoch: None,
            pending_deadline: None,
            shutdown_requested: false,
            snapshots_taken: 0,
            catchup_mode: false,
            last_decision: LastDecision::None,
        }
    }

    /// Override the bulk-sync snapshot rate limit.
    ///
    /// Callers can set a longer minimum interval for epoch-boundary snapshots
    /// during catch-up (genesis bulk sync).  The default is 30 minutes
    /// (`DEFAULT_SNAPSHOT_BULK_SYNC_RATE_LIMIT`).  Configurable via
    /// `LowLevelGenesisOptions.SnapshotMinIntervalBulkSync`.
    pub fn set_bulk_sync_rate_limit(&mut self, limit: Duration) {
        self.bulk_sync_rate_limit = limit;
    }

    /// Override the rate-limit / jitter window — primarily for tests that need
    /// deterministic firing without waiting 5+ minutes of real time.
    ///
    /// Also sets `bulk_sync_rate_limit = rate_limit` so that tests using
    /// catch-up mode do not need a separate call.
    #[doc(hidden)]
    pub fn with_test_timing(
        mut self,
        rate_limit: Duration,
        jitter_min: Duration,
        jitter_max: Duration,
    ) -> Self {
        self.rate_limit = rate_limit;
        // Mirror into bulk_sync_rate_limit so test schedulers fire deterministically
        // in both normal and catch-up mode without separate set_bulk_sync_rate_limit calls.
        self.bulk_sync_rate_limit = rate_limit;
        self.jitter_min = jitter_min;
        self.jitter_max = jitter_max;
        self
    }

    /// Enable or disable catch-up mode.
    ///
    /// In catch-up mode the slot-interval snapshot trigger is suppressed;
    /// only epoch-boundary and shutdown triggers fire.  See
    /// [`Self::catchup_mode`] for rationale.  Returns `true` if the mode
    /// changed (caller may want to log the transition).
    pub fn set_catchup_mode(&mut self, catchup: bool) -> bool {
        let changed = self.catchup_mode != catchup;
        self.catchup_mode = catchup;
        changed
    }

    /// Whether the scheduler is currently in catch-up mode.
    pub fn is_catchup_mode(&self) -> bool {
        self.catchup_mode
    }

    /// Slot interval between snapshot triggers.
    pub fn snapshot_slot_interval(&self) -> u64 {
        self.snapshot_slot_interval
    }

    /// One-shot combined check + save.
    ///
    /// Most callers should use the split API
    /// ([`Self::maybe_snapshot_check`] + [`Self::record_snapshot_taken`]) so
    /// the borrow on the scheduler can be released across an async save call.
    /// This convenience form is kept for test use and small synchronous
    /// callers.
    pub fn maybe_snapshot(
        &mut self,
        current_epoch: EpochNo,
        current_slot: SlotNo,
        block_no: BlockNo,
        save_fn: &mut dyn FnMut() -> Result<(), String>,
    ) -> bool {
        if !self.maybe_snapshot_check_at(current_epoch, current_slot, Instant::now()) {
            return false;
        }
        let reason = self.last_decision_reason();
        info!(
            reason,
            block_no = block_no.0,
            epoch = current_epoch.0,
            slot = current_slot.0,
            "SnapshotScheduler: saving ledger anchor snapshot"
        );
        match save_fn() {
            Ok(()) => {
                self.record_snapshot_taken_at(current_epoch, current_slot, Instant::now());
                debug!(
                    snapshots_taken = self.snapshots_taken,
                    "SnapshotScheduler: snapshot saved successfully"
                );
                true
            }
            Err(e) => {
                warn!(
                    error = %e,
                    block_no = block_no.0,
                    "SnapshotScheduler: snapshot save failed"
                );
                false
            }
        }
    }

    /// Check whether a snapshot *should* be taken at the current slot without
    /// actually triggering the save.
    ///
    /// This is the production entry point.  Returns `true` when:
    ///
    /// 1. `shutdown_requested` is set, or
    /// 2. the current epoch is greater than the last-snapshot epoch (and the
    ///    rate limit has elapsed), or
    /// 3. the slot delta exceeds `snapshot_slot_interval`, the jittered
    ///    deadline has elapsed, and the rate limit has elapsed.
    ///
    /// The first slot-interval trigger sets a deferred deadline (uniform
    /// random in `[jitter_min, jitter_max]` from "now").  Subsequent calls
    /// return `false` until that deadline elapses.  After firing, callers
    /// MUST call [`Self::record_snapshot_taken`] to clear the pending state
    /// and arm the next interval.
    ///
    /// `block_no` is logged for diagnostics; the firing decision is purely
    /// slot- and clock-based.
    pub fn maybe_snapshot_check(&mut self, current_epoch: EpochNo, current_slot: SlotNo) -> bool {
        self.maybe_snapshot_check_at(current_epoch, current_slot, Instant::now())
    }

    /// Internal: same as [`Self::maybe_snapshot_check`] but accepts an
    /// injected `Instant` for deterministic unit testing of the rate
    /// limit and jitter window.
    pub fn maybe_snapshot_check_at(
        &mut self,
        current_epoch: EpochNo,
        current_slot: SlotNo,
        now: Instant,
    ) -> bool {
        // 1. Shutdown — always fires immediately, no rate limit / jitter.
        if self.shutdown_requested {
            self.last_decision = LastDecision::Shutdown;
            return true;
        }

        // Rate limit must be satisfied before any slot or epoch trigger can
        // fire (matches Haskell `defRateLimit`).  Skip rate-limit on first
        // snapshot.
        //
        // Issue #747: during catch-up (genesis bulk sync), apply a LONGER
        // rate limit (`bulk_sync_rate_limit`, default 30 min) so epoch-boundary
        // snapshots do not fire at every epoch crossing — bulk sync may cross
        // many epochs per minute, and taking a snapshot at each one wastes
        // significant I/O (bincode walk + prune).
        let effective_rate_limit = if self.catchup_mode {
            self.bulk_sync_rate_limit
        } else {
            self.rate_limit
        };
        let rate_limit_ok = self
            .last_snapshot_time
            .map(|t| now.saturating_duration_since(t) >= effective_rate_limit)
            .unwrap_or(true);

        // 2. Epoch boundary.
        let epoch_boundary = match self.last_snapshot_epoch {
            None => true, // First snapshot always triggers
            Some(last) => current_epoch > last,
        };
        if epoch_boundary && rate_limit_ok {
            self.last_decision = LastDecision::EpochBoundary;
            // Epoch boundary clears any pending slot-trigger deadline — it
            // supersedes the slower slot interval.
            self.pending_deadline = None;
            return true;
        }

        // 3. Slot interval (suppressed in catch-up mode).
        if self.catchup_mode {
            return false;
        }
        let slot_delta = current_slot
            .0
            .saturating_sub(self.last_snapshot_slot.map(|s| s.0).unwrap_or(0));
        if slot_delta < self.snapshot_slot_interval {
            return false;
        }

        // Slot trigger conditions met — either set the deferred deadline or
        // check whether it has elapsed.
        match self.pending_deadline {
            None => {
                // Arm the deferred fire.
                let jitter = self.draw_jitter();
                let deadline = now + jitter;
                self.pending_deadline = Some(deadline);
                debug!(
                    epoch = current_epoch.0,
                    slot = current_slot.0,
                    slot_delta,
                    jitter_secs = jitter.as_secs(),
                    "SnapshotScheduler: slot trigger fired, deferring snapshot",
                );
                false
            }
            Some(deadline) => {
                if now >= deadline && rate_limit_ok {
                    self.last_decision = LastDecision::SlotInterval;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Draw a uniform random jitter delay in `[jitter_min, jitter_max]`.
    fn draw_jitter(&self) -> Duration {
        if self.jitter_max <= self.jitter_min {
            return self.jitter_min;
        }
        let span = self.jitter_max - self.jitter_min;
        let span_micros = span.as_micros().min(u64::MAX as u128) as u64;
        let mut rng = rand::rng();
        let offset_micros = rng.random_range(0..=span_micros);
        self.jitter_min + Duration::from_micros(offset_micros)
    }

    /// String describing the most recent firing decision (for logging).
    fn last_decision_reason(&self) -> &'static str {
        match self.last_decision {
            LastDecision::Shutdown => "graceful shutdown",
            LastDecision::EpochBoundary => "epoch boundary",
            LastDecision::SlotInterval => "slot interval",
            LastDecision::None => "unknown",
        }
    }

    /// Record that a snapshot was successfully taken.
    ///
    /// Resets the slot/time/epoch anchors and clears the pending-deadline
    /// state.  Pair with a prior [`Self::maybe_snapshot_check`] that returned
    /// `true`.
    pub fn record_snapshot_taken(&mut self, current_epoch: EpochNo, current_slot: SlotNo) {
        self.record_snapshot_taken_at(current_epoch, current_slot, Instant::now());
    }

    /// Internal: same as [`Self::record_snapshot_taken`] with an injected
    /// `Instant` for deterministic unit testing.
    pub fn record_snapshot_taken_at(
        &mut self,
        current_epoch: EpochNo,
        current_slot: SlotNo,
        now: Instant,
    ) {
        self.last_snapshot_slot = Some(current_slot);
        self.last_snapshot_time = Some(now);
        self.last_snapshot_epoch = Some(current_epoch);
        self.pending_deadline = None;
        self.shutdown_requested = false;
        self.snapshots_taken += 1;
        debug!(
            snapshots_taken = self.snapshots_taken,
            epoch = current_epoch.0,
            slot = current_slot.0,
            "SnapshotScheduler: snapshot recorded"
        );
    }

    /// Force a snapshot on the next check call regardless of counters.
    /// Call this when initiating a graceful shutdown.
    pub fn request_shutdown_snapshot(&mut self) {
        self.shutdown_requested = true;
    }

    /// Number of snapshots taken so far.
    pub fn snapshots_taken(&self) -> u64 {
        self.snapshots_taken
    }

    /// Slot of the most recently completed snapshot, if any.
    pub fn last_snapshot_slot(&self) -> Option<SlotNo> {
        self.last_snapshot_slot
    }

    /// `true` while the scheduler is holding a deferred-fire deadline armed
    /// (i.e. the slot trigger fired but the jitter delay has not yet
    /// elapsed).
    pub fn has_pending_deadline(&self) -> bool {
        self.pending_deadline.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastDecision {
    None,
    Shutdown,
    EpochBoundary,
    SlotInterval,
}

impl Default for SnapshotScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_db::{ChainDB, DEFAULT_SECURITY_PARAM_K};
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
    use tempfile::TempDir;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// A minimal fake block CBOR (1 byte) for tests that only care about
    /// storage plumbing, not block validity.
    fn fake_cbor() -> Vec<u8> {
        vec![0x80] // CBOR empty array
    }

    fn make_hash(n: u8) -> Hash32 {
        let mut bytes = [0u8; 32];
        bytes[31] = n;
        Hash32::from_bytes(bytes)
    }

    fn open_chain_db(dir: &TempDir) -> ChainDB {
        ChainDB::open(dir.path()).expect("open ChainDB")
    }

    // ── CopyToImmutable ──────────────────────────────────────────────────────

    /// Populate `db` with `count` sequential blocks starting at block_no 1,
    /// slot 1.  Returns the list of (slot, hash, block_no) in order.
    fn populate_volatile(db: &mut ChainDB, count: usize) -> Vec<(SlotNo, Hash32, BlockNo)> {
        let mut prev = make_hash(0); // genesis prev hash
        let mut entries = Vec::with_capacity(count);
        for i in 1..=(count as u64) {
            let hash = make_hash(i as u8);
            let slot = SlotNo(i);
            let block_no = BlockNo(i);
            db.add_block(hash, slot, block_no, prev, fake_cbor())
                .expect("add_block");
            entries.push((slot, hash, block_no));
            prev = hash;
        }
        entries
    }

    /// Test that `CopyToImmutable::run_once` does nothing when fragment_len == k.
    #[test]
    fn copy_to_immutable_no_op_when_at_k() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);
        let blocks = populate_volatile(&mut db, 3);

        let copier = CopyToImmutable::new(3);
        let (oldest_slot, oldest_hash, oldest_block_no) = blocks[0];

        let mut anchor_calls = 0usize;
        let result = copier.run_once(
            &mut db,
            3, // fragment_len == k → no copy
            oldest_hash,
            oldest_slot,
            oldest_block_no,
            &mut |_, _, _| {
                anchor_calls += 1;
            },
        );

        assert!(
            result.unwrap().is_none(),
            "should not copy at fragment_len == k"
        );
        assert_eq!(anchor_calls, 0);
    }

    /// Test that `CopyToImmutable::run_once` copies the oldest block when
    /// fragment_len > k.
    #[test]
    fn copy_to_immutable_copies_oldest_when_fragment_exceeds_k() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);

        // Add k+1 blocks so the oldest one is now k-deep.
        let k = 3usize;
        let blocks = populate_volatile(&mut db, k + 1);

        let copier = CopyToImmutable::new(k);
        let (oldest_slot, oldest_hash, oldest_block_no) = blocks[0];

        let mut anchor_advanced = false;
        let result = copier.run_once(
            &mut db,
            k + 1, // fragment_len > k → copy triggered
            oldest_hash,
            oldest_slot,
            oldest_block_no,
            &mut |s, h, _b| {
                assert_eq!(s, oldest_slot);
                assert_eq!(h, oldest_hash);
                anchor_advanced = true;
            },
        );

        let copied = result
            .expect("run_once should succeed")
            .expect("should copy");
        assert_eq!(copied.0, oldest_slot);
        assert_eq!(copied.1, oldest_hash);
        assert!(anchor_advanced, "ledger anchor callback must be called");

        // Verify the block is now present in the immutable store.
        assert!(
            db.has_block(&oldest_hash),
            "block must still be findable (now in ImmutableDB)"
        );
    }

    /// Test that `run_once` propagates an error when the block is not in
    /// the VolatileDB (e.g., already removed or never added).
    #[test]
    fn copy_to_immutable_error_on_missing_block() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);

        let copier = CopyToImmutable::new(1);
        let missing_hash = make_hash(99);

        let result = copier.run_once(
            &mut db,
            2, // fragment_len > k
            missing_hash,
            SlotNo(42),
            BlockNo(42),
            &mut |_, _, _| {},
        );

        assert!(result.is_err(), "should return Err when block is not found");
    }

    // ── GcScheduler ──────────────────────────────────────────────────────────

    /// Test that scheduled blocks are NOT removed before the GC delay elapses.
    #[test]
    fn gc_scheduler_respects_delay() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);
        let blocks = populate_volatile(&mut db, 2);

        let mut scheduler = GcScheduler::new();
        let t0 = Instant::now();

        // Schedule the first block for GC.
        let (slot, hash, _) = blocks[0];
        scheduler.schedule(slot, hash, t0);

        assert_eq!(scheduler.pending_count(), 1);

        // Run GC immediately — the block should still be in VolatileDB.
        // We simulate "just after scheduling" by passing t0 (before deadline).
        let removed = scheduler.run_pending(&mut db, t0);
        assert_eq!(removed, 0, "nothing should be GC'd before the delay");

        // Verify block is still present.
        assert!(
            db.has_block(&hash),
            "block must still be in ChainDB before GC delay"
        );
    }

    /// Test that blocks ARE removed after the GC delay.
    #[test]
    fn gc_scheduler_removes_after_delay() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);

        // Add a block to VolatileDB.
        let hash = make_hash(1);
        let slot = SlotNo(1);
        let block_no = BlockNo(1);
        let prev = make_hash(0);
        db.add_block(hash, slot, block_no, prev, fake_cbor())
            .unwrap();

        let mut scheduler = GcScheduler::new();

        // Schedule with a deadline 61 seconds in the past — already expired.
        let past = Instant::now() - Duration::from_secs(61);
        scheduler.schedule(slot, hash, past);

        assert_eq!(scheduler.pending_count(), 1);

        // Run GC at "now" — the entry should be eligible.
        let removed = scheduler.run_pending(&mut db, Instant::now());
        assert_eq!(removed, 1, "one block should have been GC'd");
        assert_eq!(
            scheduler.pending_count(),
            0,
            "scheduler should be empty after GC"
        );
    }

    /// Test that multiple blocks can be scheduled and GC'd independently.
    #[test]
    fn gc_scheduler_handles_multiple_entries() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);

        let past = Instant::now() - Duration::from_secs(61);
        let future = Instant::now() + Duration::from_secs(30);

        // Two blocks with past deadline (should be GC'd).
        let h1 = make_hash(1);
        let h2 = make_hash(2);
        // One block with future deadline (should NOT be GC'd yet).
        let h3 = make_hash(3);

        let prev = make_hash(0);
        db.add_block(h1, SlotNo(1), BlockNo(1), prev, fake_cbor())
            .unwrap();
        db.add_block(h2, SlotNo(2), BlockNo(2), h1, fake_cbor())
            .unwrap();
        db.add_block(h3, SlotNo(3), BlockNo(3), h2, fake_cbor())
            .unwrap();

        let mut scheduler = GcScheduler::new();
        scheduler.schedule(SlotNo(1), h1, past);
        scheduler.schedule(SlotNo(2), h2, past);
        scheduler.schedule(SlotNo(3), h3, future);

        assert_eq!(scheduler.pending_count(), 3);

        let removed = scheduler.run_pending(&mut db, Instant::now());
        assert_eq!(removed, 2, "two expired blocks should be GC'd");
        assert_eq!(scheduler.pending_count(), 1, "one pending entry remains");
    }

    // ── SnapshotScheduler (slot-based + jitter, #701) ───────────────────────

    /// Helper — build a scheduler with deterministic timing for tests:
    /// rate_limit = 0, jitter_min = jitter_max = 0.  This collapses the
    /// jittered fire to "fire on the call that crosses the slot threshold,"
    /// matching the old block-count behaviour while preserving the slot-based
    /// trigger semantics.
    fn test_scheduler(slot_interval: u64) -> SnapshotScheduler {
        SnapshotScheduler::with_slot_interval(slot_interval).with_test_timing(
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        )
    }

    /// First call always fires (epoch-boundary semantics: no last snapshot).
    #[test]
    fn snapshot_scheduler_first_call_fires_via_epoch_boundary() {
        let mut sched = test_scheduler(100);
        let t0 = Instant::now();
        let mut snapshots = 0usize;
        let fired = sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0);
        assert!(fired, "first call always fires");
        if fired {
            sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);
            snapshots += 1;
        }
        assert_eq!(snapshots, 1);
        assert_eq!(sched.snapshots_taken(), 1);
        assert_eq!(sched.last_snapshot_slot(), Some(SlotNo(1)));
    }

    /// Slot-based trigger arms a deadline when
    /// `current_slot - last_snapshot_slot >= interval`, and fires on the
    /// next call after the deadline elapses.
    ///
    /// `test_scheduler` uses zero jitter, so the deadline elapses immediately
    /// — the trigger fires on the very next call.
    #[test]
    fn snapshot_scheduler_slot_trigger_fires_at_interval() {
        let mut sched = test_scheduler(100);
        let t0 = Instant::now();

        // First snapshot at slot 1.
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // Slot 50 — delta 49 < 100, no fire, no pending deadline.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(50), t0));
        assert!(!sched.has_pending_deadline());

        // Slot 101 — delta 100 == interval, ARMS a (zero-duration) deadline.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));
        assert!(sched.has_pending_deadline(), "deadline should be armed");

        // Next call — deadline elapsed (zero duration), fires.
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(101), t0);
        assert!(!sched.has_pending_deadline());

        // Slot 150 — delta 49 < 100, no fire.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(150), t0));
    }

    /// Epoch boundary fires regardless of slot delta.
    #[test]
    fn snapshot_scheduler_epoch_boundary_supersedes_slot_interval() {
        let mut sched = test_scheduler(10_000);
        let t0 = Instant::now();

        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // Stay in epoch 0, slot delta 5 — no fire (interval is 10k).
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(6), t0));

        // Bump to epoch 1 — fires via epoch boundary even though slot delta < interval.
        assert!(sched.maybe_snapshot_check_at(EpochNo(1), SlotNo(6), t0));
    }

    /// Catch-up mode suppresses the slot-interval trigger but preserves the
    /// epoch-boundary trigger and the shutdown trigger.
    #[test]
    fn snapshot_scheduler_catchup_mode_suppresses_slot_interval() {
        let mut sched = test_scheduler(100);
        let t0 = Instant::now();
        assert!(sched.set_catchup_mode(true));
        assert!(sched.is_catchup_mode());
        assert!(!sched.set_catchup_mode(true));

        // First call: epoch-boundary still fires.
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // Even far past the slot interval, no fire in catch-up mode.
        for s in [200u64, 500, 1000, 5000] {
            assert!(
                !sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(s), t0),
                "slot {s}: slot trigger must be suppressed in catch-up mode"
            );
        }

        // Epoch boundary still fires.
        assert!(sched.maybe_snapshot_check_at(EpochNo(1), SlotNo(10_000), t0));
    }

    /// Shutdown trigger fires regardless of any other constraint.
    #[test]
    fn snapshot_scheduler_shutdown_forces_snapshot() {
        let mut sched = test_scheduler(10_000);
        let t0 = Instant::now();
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // No trigger before shutdown.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(2), t0));

        // Request shutdown — next call fires.
        sched.request_shutdown_snapshot();
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(3), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(3), t0);

        // After record, shutdown flag is cleared and no immediate re-fire.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(4), t0));
    }

    /// snapshots_taken counter increments correctly.
    #[test]
    fn snapshot_scheduler_counts_snapshots() {
        let mut sched = test_scheduler(100);
        let t0 = Instant::now();
        assert_eq!(sched.snapshots_taken(), 0);

        // First snapshot via epoch boundary.
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);
        assert_eq!(sched.snapshots_taken(), 1);

        // Slot trigger requires two calls (arm + fire) even with zero jitter.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(101), t0);
        assert_eq!(sched.snapshots_taken(), 2);
    }

    /// Jitter window — when configured, the slot trigger arms a pending
    /// deadline and the check returns false until the deadline elapses.
    /// This pins the Haskell deferred-fire semantics.
    #[test]
    fn snapshot_scheduler_slot_trigger_jitter_deferral() {
        let mut sched = SnapshotScheduler::with_slot_interval(100).with_test_timing(
            Duration::ZERO,
            Duration::from_millis(100),
            Duration::from_millis(100),
        );
        let t0 = Instant::now();

        // First snapshot at slot 1 via epoch boundary (no deferral on epoch
        // boundary).
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // Slot 101 crosses the interval — the first check should ARM a
        // deferred deadline but NOT fire.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));
        assert!(
            sched.has_pending_deadline(),
            "slot trigger should arm a pending deadline"
        );

        // Same slot, same `now` — deadline still in the future.
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t0));

        // 100 ms later — deadline elapsed, fires.
        let t1 = t0 + Duration::from_millis(101);
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t1));
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(101), t1);
        assert!(!sched.has_pending_deadline());
    }

    /// Rate limit — even when the slot interval has fired, the wall-clock
    /// gap to the last snapshot must be at least `rate_limit`.
    #[test]
    fn snapshot_scheduler_respects_rate_limit() {
        let mut sched = SnapshotScheduler::with_slot_interval(100).with_test_timing(
            Duration::from_millis(500), // rate limit
            Duration::ZERO,
            Duration::ZERO,
        );
        let t0 = Instant::now();
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(1), t0);

        // Slot interval crossed, but only 100 ms after last snapshot — rate
        // limit not satisfied.
        let t1 = t0 + Duration::from_millis(100);
        assert!(!sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t1));

        // 600 ms after last snapshot — rate limit satisfied, fires.
        let t2 = t0 + Duration::from_millis(600);
        assert!(sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(101), t2));
    }

    /// Constants — Haskell parity check.  These values are load-bearing for
    /// production behaviour and must not regress silently.  See
    /// `defaultSnapshotPolicy` in
    /// `ouroboros-consensus/src/.../Snapshots.hs`.
    #[test]
    fn snapshot_scheduler_haskell_defaults_pinned() {
        assert_eq!(
            DEFAULT_SNAPSHOT_SLOT_INTERVAL, 4_320,
            "defInterval = k*2 with k=2160"
        );
        assert_eq!(
            DEFAULT_SNAPSHOT_RATE_LIMIT,
            Duration::from_secs(600),
            "defRateLimit = 10 min"
        );
        assert_eq!(
            DEFAULT_SNAPSHOT_JITTER_MIN,
            Duration::from_secs(5 * 60),
            "fiveMinutes"
        );
        assert_eq!(
            DEFAULT_SNAPSHOT_JITTER_MAX,
            Duration::from_secs(10 * 60),
            "tenMinutes"
        );
    }

    /// Sanity: configured slot interval is exposed via the accessor.
    #[test]
    fn snapshot_scheduler_slot_interval_accessor() {
        let sched = SnapshotScheduler::with_slot_interval(864);
        assert_eq!(sched.snapshot_slot_interval(), 864);
        let sched = SnapshotScheduler::new();
        assert_eq!(
            sched.snapshot_slot_interval(),
            DEFAULT_SNAPSHOT_SLOT_INTERVAL
        );
    }

    /// Test that GcScheduler::pending_count returns correct values.
    #[test]
    fn gc_scheduler_pending_count() {
        let mut sched = GcScheduler::new();
        assert_eq!(sched.pending_count(), 0);
        assert!(sched.is_empty());

        let t = Instant::now();
        sched.schedule(SlotNo(1), make_hash(1), t);
        assert_eq!(sched.pending_count(), 1);
        assert!(!sched.is_empty());

        sched.schedule(SlotNo(2), make_hash(2), t);
        assert_eq!(sched.pending_count(), 2);
    }

    /// Verify that security parameter `k` is correctly respected: no copy
    /// should happen when fragment_len is exactly k (not k+1).
    #[test]
    fn copy_to_immutable_boundary_conditions() {
        let dir = TempDir::new().unwrap();
        let mut db = open_chain_db(&dir);

        let _ = populate_volatile(&mut db, 5);
        let hash = make_hash(1);
        let slot = SlotNo(1);
        let block_no = BlockNo(1);

        let copier_k5 = CopyToImmutable::new(5);

        // fragment_len == k → no copy.
        let res = copier_k5.run_once(&mut db, 5, hash, slot, block_no, &mut |_, _, _| {});
        assert!(res.unwrap().is_none());

        // fragment_len == k-1 → definitely no copy.
        let res = copier_k5.run_once(&mut db, 4, hash, slot, block_no, &mut |_, _, _| {});
        assert!(res.unwrap().is_none());

        // fragment_len == k+1 → copy triggered.
        let res = copier_k5.run_once(&mut db, 6, hash, slot, block_no, &mut |_, _, _| {});
        assert!(res.unwrap().is_some());
    }

    /// Confirm the full production security parameter is accessible.
    #[test]
    fn security_param_k_is_correct() {
        // mainnet k = 2160
        assert_eq!(DEFAULT_SECURITY_PARAM_K, 2160);
        let copier = CopyToImmutable::new(DEFAULT_SECURITY_PARAM_K);
        assert_eq!(copier.k, 2160);
    }

    // ─── Fix 4 (#747): bulk-sync snapshot rate-limit tests ───────────────────

    /// During catch-up mode, an epoch boundary that occurs within the
    /// bulk_sync_rate_limit window must be suppressed.
    ///
    /// Without Fix 4 the regular rate_limit (10 min) was used in catchup_mode,
    /// allowing epoch-boundary snapshots as frequently as every 10 min even
    /// during genesis bulk sync.  Fix 4 raises the floor to 30 min.
    #[test]
    fn bulk_sync_epoch_boundary_suppressed_within_rate_limit() {
        let mut sched = SnapshotScheduler::with_slot_interval(864).with_test_timing(
            Duration::from_secs(0),
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        sched.set_bulk_sync_rate_limit(Duration::from_secs(1800)); // 30 min
        sched.set_catchup_mode(true);

        let t0 = Instant::now();
        // First snapshot fires immediately (no last_snapshot_time).
        let fires = sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(100), t0);
        assert!(
            fires,
            "first snapshot must fire immediately even in catchup_mode"
        );
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(100), t0);

        // Cross epoch boundary 11 minutes later — within the 30-min bulk limit.
        let t1 = t0 + Duration::from_secs(11 * 60);
        let fires2 = sched.maybe_snapshot_check_at(EpochNo(1), SlotNo(1000), t1);
        assert!(
            !fires2,
            "epoch boundary at +11min must be suppressed by 30-min bulk_sync_rate_limit"
        );
    }

    /// An epoch boundary that occurs after the bulk_sync_rate_limit has elapsed
    /// MUST fire (otherwise we'd never snapshot during long bulk syncs).
    #[test]
    fn bulk_sync_epoch_boundary_fires_after_bulk_rate_limit() {
        let mut sched = SnapshotScheduler::with_slot_interval(864).with_test_timing(
            Duration::from_secs(0),
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        sched.set_bulk_sync_rate_limit(Duration::from_secs(1800));
        sched.set_catchup_mode(true);

        let t0 = Instant::now();
        // First snapshot.
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(0), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(0), t0);

        // Cross epoch boundary 31 minutes later — beyond the 30-min limit.
        let t1 = t0 + Duration::from_secs(31 * 60);
        let fires = sched.maybe_snapshot_check_at(EpochNo(1), SlotNo(1000), t1);
        assert!(
            fires,
            "epoch boundary at +31min must fire after 30-min bulk_sync_rate_limit expires"
        );
    }

    /// In normal (at-tip) mode the regular rate_limit (10 min) must still apply,
    /// not the bulk_sync_rate_limit.
    #[test]
    fn at_tip_mode_uses_normal_rate_limit_not_bulk() {
        let mut sched = SnapshotScheduler::with_slot_interval(864).with_test_timing(
            Duration::from_secs(0),
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        sched.set_bulk_sync_rate_limit(Duration::from_secs(1800)); // 30 min
                                                                   // Explicitly not in catchup_mode.
        sched.set_catchup_mode(false);

        let t0 = Instant::now();
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(0), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(0), t0);

        // Cross epoch boundary 11 minutes later — within 30-min bulk limit
        // but beyond the 10-min normal rate limit.
        // NOTE: with_test_timing sets rate_limit=0, so we need to test the
        // default rate_limit path explicitly by NOT using with_test_timing.
        let mut sched2 = SnapshotScheduler::with_slot_interval(864);
        sched2.set_bulk_sync_rate_limit(Duration::from_secs(1800));
        sched2.set_catchup_mode(false);

        let t0 = Instant::now();
        // First snapshot fires.
        sched2.maybe_snapshot_check_at(EpochNo(0), SlotNo(0), t0);
        sched2.record_snapshot_taken_at(EpochNo(0), SlotNo(0), t0);

        // Cross epoch boundary 11 minutes later — beyond the 10-min normal limit.
        let t1 = t0 + Duration::from_secs(11 * 60);
        let fires = sched2.maybe_snapshot_check_at(EpochNo(1), SlotNo(1000), t1);
        assert!(
            fires,
            "at-tip epoch boundary at +11min must fire (10-min normal rate limit, not 30-min bulk)"
        );
    }

    /// Shutdown requests must always fire regardless of bulk_sync_rate_limit.
    #[test]
    fn shutdown_fires_regardless_of_bulk_rate_limit() {
        let mut sched = SnapshotScheduler::with_slot_interval(864).with_test_timing(
            Duration::from_secs(0),
            Duration::from_secs(0),
            Duration::from_secs(0),
        );
        sched.set_bulk_sync_rate_limit(Duration::from_secs(1800));
        sched.set_catchup_mode(true);

        let t0 = Instant::now();
        sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(0), t0);
        sched.record_snapshot_taken_at(EpochNo(0), SlotNo(0), t0);

        // Fire shutdown just 1 second later — well within bulk limit.
        sched.request_shutdown_snapshot();
        let t1 = t0 + Duration::from_secs(1);
        let fires = sched.maybe_snapshot_check_at(EpochNo(0), SlotNo(1), t1);
        assert!(
            fires,
            "shutdown must always fire regardless of bulk_sync_rate_limit"
        );
    }
}
