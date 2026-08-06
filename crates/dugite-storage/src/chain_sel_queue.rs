//! ChainSelQueue — sequential block processing queue, matching Haskell's
//! `addBlockAsync` / `addBlockRunner` pattern from `ChainSel.hs`.
//!
//! All blocks (whether received from peers or forged locally) enter the node
//! via a single bounded MPSC channel and are processed **one at a time** by
//! the [`add_block_runner`] task.  The sequential discipline means:
//!
//! * No concurrency hazard between chain selection and storage writes.
//! * Invalid-block decisions are visible to every subsequent block immediately.
//! * Fork tracking is deterministic and audit-able.
//!
//! # Current State
//!
//! `add_block_runner` writes every valid, unknown block to the VolatileDB,
//! runs chain selection, and returns [`AddBlockResult::AddedAsTip`] when the
//! block extended the selected chain or [`AddBlockResult::StoredAsFork`] when
//! it was stored as a fork block.  The caller no longer needs a post-hoc tip
//! re-lookup to distinguish the two cases.
//!
//! # Haskell reference
//!
//! `ouroboros-consensus ChainDB/Impl/ChainSel.hs` — `addBlockAsync`,
//! `addBlockRunner`, `chainSelectionForBlock`.
//!
//! # Invariants
//!
//! * Only one outstanding `add_block_runner` task must run per `ChainSelQueue`.
//! * Blocks are stored to VolatileDB **before** any chain-selection logic runs.
//! * Once a block is in the invalid-block cache, it can never become valid.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, trace, warn};

use dugite_consensus::chain_selection::{ChainPreference, ChainSelection};
use dugite_primitives::block::{Point, Tip};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{BlockHeaderHash, Hash32};
use dugite_primitives::time::{BlockNo, SlotNo};

use crate::chain_db::ChainDB;

// ---------------------------------------------------------------------------
// Public message types
// ---------------------------------------------------------------------------

/// Message sent to the chain-selection background task.
///
/// Currently there is only one variant; future work may add `Shutdown`,
/// `Flush`, or priority hint messages.
#[allow(clippy::large_enum_variant)] // AddBlock dominates the queue; boxing would churn every producer for no win
pub enum ChainSelMessage {
    /// Request to add a block to the chain.
    ///
    /// The block is identified by its header hash plus enough metadata to
    /// write it to storage without re-parsing the CBOR.  The `result_tx`
    /// oneshot is fulfilled when the runner finishes processing the block.
    AddBlock {
        /// Blake2b-256 hash of the block header.
        hash: BlockHeaderHash,
        /// Absolute slot number of the block.
        slot: SlotNo,
        /// Sequential block number (height).
        block_no: BlockNo,
        /// Hash of the predecessor block (links the chain).
        prev_hash: BlockHeaderHash,
        /// Raw CBOR bytes of the complete block.
        cbor: Vec<u8>,
        /// Block header for the Praos chain-selection tiebreaker (Bug D, #497).
        /// `None` for legacy callers and Byron blocks; comparator falls back to
        /// strict-greater block_no in that case.
        header: Option<dugite_primitives::block::BlockHeader>,
        /// True when the LOCAL node forged this block. A self-forged block
        /// extends the node's own selection unconditionally — the Limit on
        /// Eagerness constrains trust in PEER chains, never the node's own
        /// production (which an adversary cannot forge). Genesis-mode block
        /// producers would otherwise have their own blocks deferred past k.
        self_forged: bool,
        /// Fulfillment channel for the processing result.
        result_tx: oneshot::Sender<AddBlockResult>,
    },

    /// Re-run chain selection with no new block — Haskell
    /// `ChainSelReprocessLoEBlocks`. Sent when the Limit on Eagerness
    /// advances (or the GSM enters CaughtUp) so blocks whose adoption was
    /// deferred by `trimToLoE` get re-evaluated even when no further block
    /// arrives. Returns the selection outcome: `TriggeredFork` when a
    /// deferred candidate is now adoptable, `StoredAsFork` when nothing
    /// changed.
    ReprocessLoE {
        /// Fulfillment channel for the selection outcome.
        result_tx: oneshot::Sender<AddBlockResult>,
    },
}

/// Result returned to the caller after `AddBlock` is processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddBlockResult {
    /// The block was stored and is the new selected-chain tip.
    ///
    /// The caller's submitted block IS the new tip iff `tip_hash` equals
    /// the hash they submitted. If the block was stored but another block
    /// already extended the tip in a race, this variant is NOT returned —
    /// see `StoredAsFork`.
    ///
    /// Mirrors Haskell's `SuccesfullyAddedBlock (Point blk)` in
    /// `Storage/ChainDB/API.hs` — the new tip point is always carried.
    AddedAsTip {
        tip_hash: BlockHeaderHash,
        tip_slot: SlotNo,
        tip_block_no: BlockNo,
    },
    /// The block was stored in the VolatileDB but is NOT on the selected
    /// chain (a fork block with reachable ancestry but not winning chain
    /// selection).
    StoredAsFork,
    /// The block failed validation. The reason string is human-readable.
    Invalid(String),
    /// The block was already present in either the VolatileDB or ImmutableDB.
    AlreadyKnown,
    /// Chain selection switched to a strictly-longer competing fork. The
    /// VolatileDB has already updated `selected_chain`. The caller must
    /// rollback the ledger to `intersection_hash`/`intersection_slot`.
    ///
    /// Matches Haskell `ChainDiff` (Paths.hs:~55).
    TriggeredFork {
        /// Common ancestor of the old and new chains (the fork point).
        intersection_hash: BlockHeaderHash,
        /// Slot of the intersection block, pre-resolved by VolatileDB so the
        /// caller can build a rollback `Point` without a second lookup.
        intersection_slot: SlotNo,
        /// Hashes of blocks on the old chain to roll back, newest-first.
        rollback: Vec<BlockHeaderHash>,
        /// Hashes of blocks on the new chain to apply, oldest-first.
        apply: Vec<BlockHeaderHash>,
    },
}

// ---------------------------------------------------------------------------
// Invalid-block cache
// ---------------------------------------------------------------------------

/// An entry in the invalid-block cache.
struct InvalidEntry {
    /// Human-readable reason the block was rejected.
    reason: String,
    /// Monotonic instant at which this entry was inserted.
    inserted_at: Instant,
}

/// Bounded cache of recently-rejected block hashes, with TTL expiry.
///
/// Matches Haskell's `invalidBlocks :: STM.TVar (Set (RealPoint blk))` field
/// in `ChainDbEnv`.  The cache is consulted by `add_block_runner` before
/// writing any block to storage; if the block is already known-invalid the
/// runner immediately returns `Invalid` without re-validating.
///
/// The cache is bounded to [`InvalidBlockCache::MAX_ENTRIES`] entries.  When
/// the cache is full, the oldest entry is evicted before inserting the new one
/// (FIFO eviction, not LRU, to match the simplicity of the Haskell implementation).
///
/// TTL expiry is lazy: entries are not proactively removed, but any lookup
/// that finds a stale entry (older than `ttl`) treats it as absent and
/// removes it.
pub struct InvalidBlockCache {
    /// Map from block hash to invalidation reason and insertion instant.
    entries: HashMap<BlockHeaderHash, InvalidEntry>,
    /// Time-to-live for each cache entry.
    ttl: Duration,
    /// Insertion-order queue for FIFO eviction (oldest first).
    order: std::collections::VecDeque<BlockHeaderHash>,
}

impl InvalidBlockCache {
    /// Maximum number of entries retained without eviction.
    ///
    /// Matches a reasonable upper bound for the number of distinct invalid
    /// blocks that could arrive in a TTL window.  Haskell uses an unbounded
    /// `Set` but GC handles it; we use a bounded structure to cap memory.
    pub const MAX_ENTRIES: usize = 1_024;

    /// Default TTL: 10 minutes.  After this interval, a cached entry is
    /// treated as absent and removed on next lookup.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(600);

    /// Create a new cache with the default capacity and TTL.
    pub fn new() -> Self {
        Self::with_ttl(Self::DEFAULT_TTL)
    }

    /// Create a new cache with a custom TTL.  Useful in tests.
    pub fn with_ttl(ttl: Duration) -> Self {
        InvalidBlockCache {
            entries: HashMap::new(),
            ttl,
            order: std::collections::VecDeque::new(),
        }
    }

    /// Insert a block hash into the cache with the given rejection reason.
    ///
    /// If the cache has reached [`MAX_ENTRIES`], the oldest entry is evicted
    /// first.  If `hash` is already present its entry is updated in-place
    /// (TTL reset, reason updated) without affecting the eviction queue order.
    pub fn insert(&mut self, hash: BlockHeaderHash, reason: String) {
        if self.entries.contains_key(&hash) {
            // Refresh the existing entry in-place; no queue change needed.
            if let Some(entry) = self.entries.get_mut(&hash) {
                entry.reason = reason;
                entry.inserted_at = Instant::now();
            }
            return;
        }

        // Evict oldest entry if at capacity.
        if self.entries.len() >= Self::MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }

        self.entries.insert(
            hash,
            InvalidEntry {
                reason,
                inserted_at: Instant::now(),
            },
        );
        self.order.push_back(hash);
    }

    /// Look up a block hash in the cache.
    ///
    /// Returns `Some(reason)` if the block is known-invalid and its cache
    /// entry has not expired.  Expired entries are lazily removed.
    ///
    /// Returns `None` if the block is unknown or its entry has expired.
    pub fn get(&mut self, hash: &BlockHeaderHash) -> Option<&str> {
        if let Some(entry) = self.entries.get(hash) {
            if entry.inserted_at.elapsed() < self.ttl {
                // Entry is still valid — return a reference to the reason.
                // Safety: re-borrow through the map for lifetime correctness.
                return self.entries.get(hash).map(|e| e.reason.as_str());
            }
            // Entry has expired — remove it lazily.
            self.entries.remove(hash);
            // Also remove from the order queue (O(n) but rare in practice).
            self.order.retain(|h| h != hash);
        }
        None
    }

    /// Number of live (non-expired) entries in the cache.
    ///
    /// This is an *approximate* count because expiry is lazy — expired entries
    /// are only removed on [`get`] or when a new insert triggers eviction.
    /// Use this for monitoring/debugging, not for correctness decisions.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the cache contains no live entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Snapshot of all cached invalid-block hashes, for ancestry checks during
    /// chain selection (Haskell `truncateRejectedBlocks`: a candidate whose
    /// chain contains a known-invalid block must never be adopted).
    ///
    /// Includes any lazily-expired entries — that only ever over-rejects a
    /// candidate (conservatively safe), and the entry is purged on the next
    /// `get`. Callers should snapshot once and reuse; this is only consulted
    /// when the cache is non-empty (i.e. after a fork-replay failure).
    pub fn hash_set(&self) -> std::collections::HashSet<BlockHeaderHash> {
        self.entries.keys().copied().collect()
    }
}

impl Default for InvalidBlockCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// add_block_runner
// ---------------------------------------------------------------------------

/// Background task that processes blocks from the [`ChainSelMessage`] queue.
///
/// This is the Rust equivalent of Haskell's `addBlockRunner` loop.  It must
/// run as exactly **one** `tokio::spawn`-ed task per [`ChainSelHandle`].
///
/// # Processing steps for each `AddBlock` message
///
/// 1. Check VolatileDB and ImmutableDB — return `AlreadyKnown` if present.
/// 2. Check the invalid-block cache — return `Invalid` if previously rejected.
/// 3. Write to VolatileDB (captures `extended_tip` bool from `insert_block_internal`).
/// 4. Run chain selection — switch to any strictly-longer fork found in VolatileDB.
/// 5. Return `AddedAsTip` if the block extended the selected chain, else `StoredAsFork`.
///
/// # Arguments
///
/// * `rx` — receiving end of the MPSC queue.
/// * `chain_db` — shared ChainDB protected by a read-write lock.
/// * `invalid_cache` — shared invalid-block cache (also protected by a lock
///   so the handle can inspect it independently if needed).
pub async fn add_block_runner(
    mut rx: mpsc::Receiver<ChainSelMessage>,
    chain_db: Arc<RwLock<ChainDB>>,
    invalid_cache: Arc<RwLock<InvalidBlockCache>>,
    // #1057 half A: published by the node; gates `switch_chain`'s genesis arm.
    ledger_can_reach_origin: Arc<std::sync::atomic::AtomicBool>,
) {
    debug!("add_block_runner: started");

    while let Some(msg) = rx.recv().await {
        match msg {
            ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header,
                self_forged,
                result_tx,
            } => {
                let result = process_add_block(
                    &hash,
                    slot,
                    block_no,
                    prev_hash,
                    cbor,
                    header.as_ref(),
                    self_forged,
                    &chain_db,
                    &invalid_cache,
                    &ledger_can_reach_origin,
                )
                .await;

                trace!(
                    hash = %hash.to_hex(),
                    slot = slot.0,
                    block_no = block_no.0,
                    result = ?result,
                    "add_block_runner: processed block"
                );

                // A send failure means the caller dropped their receiver —
                // this is not an error; log at trace and continue.
                if result_tx.send(result).is_err() {
                    trace!(hash = %hash.to_hex(), "add_block_runner: result receiver dropped");
                }
            }
            ChainSelMessage::ReprocessLoE { result_tx } => {
                let invalid_snapshot: Option<std::collections::HashSet<Hash32>> = {
                    let cache = invalid_cache.write().await;
                    if cache.is_empty() {
                        None
                    } else {
                        Some(cache.hash_set())
                    }
                };
                let result = run_selection_pass(
                    &chain_db,
                    &invalid_snapshot,
                    true,
                    &ledger_can_reach_origin,
                )
                .await
                .unwrap_or(AddBlockResult::StoredAsFork);
                if result_tx.send(result).is_err() {
                    trace!("add_block_runner: ReprocessLoE receiver dropped");
                }
            }
        }
    }

    debug!("add_block_runner: channel closed, exiting");
}

/// One chain-selection evaluation over the current VolatileDB fork tips
/// (Haskell `chainSelectionForBlock` / `ChainSelReprocessLoEBlocks`).
///
/// Applies `trimToLoE` to every candidate (identity when the LoE is
/// disabled — the praos fast path), then the Praos preference order, the
/// known-invalid-ancestry guard, and finally `switch_to_fork`.
///
/// Returns:
/// - `Some(AddBlockResult::TriggeredFork{..})` — a candidate was adopted;
/// - `Some(AddBlockResult::StoredAsFork)` — the preferred candidate was
///   refused (contains a known-invalid block);
/// - `None` — no preferred candidate (or the fork is currently unreachable);
///   the caller falls through to its non-switch path.
async fn run_selection_pass(
    chain_db: &Arc<RwLock<ChainDB>>,
    invalid_snapshot: &Option<std::collections::HashSet<Hash32>>,
    prefer_praos: bool,
    ledger_can_reach_origin: &Arc<std::sync::atomic::AtomicBool>,
) -> Option<AddBlockResult> {
    let mut db = chain_db.write().await;

    let current_tip_info = db.get_tip_info();
    let current_tip_block_no: u64 = current_tip_info
        .as_ref()
        .map(|(_slot, _hash, bn)| bn.0)
        .unwrap_or(0);

    // For the Praos tiebreaker we need: (a) the current tip's BlockHeader,
    // (b) each fork-tip's BlockHeader.  All come from the in-memory cache
    // populated by `add_block_with_header`.
    let current_tip_header = current_tip_info
        .as_ref()
        .and_then(|(_, h, _)| db.get_volatile_header(h).cloned());

    // `trimToLoE` (Ouroboros Genesis): refresh the published LoE and
    // trim every candidate. `Allowed` passes through; `TrimmedTo`
    // candidates participate as their trimmed ancestor (Haskell adopts
    // `candPrefix ++ takeOldest k candSuffix`); `Deferred` candidates
    // are invisible to this pass and re-enter when the LoE advances.
    // With the LoE disabled (praos) this is the identity.
    db.refresh_loe_view();
    let fork_tips: Vec<(Hash32, BlockNo, SlotNo)> = db
        .get_all_fork_tips()
        .into_iter()
        .filter_map(|(h, bn, slot)| match db.loe_verdict(&h) {
            crate::loe_trim::LoeVerdict::Allowed => Some((h, bn, slot)),
            crate::loe_trim::LoeVerdict::TrimmedTo(t) => db
                .get_volatile_block_meta(&t)
                .map(|(t_slot, t_bn)| (t, t_bn, t_slot)),
            crate::loe_trim::LoeVerdict::Deferred => None,
        })
        .collect();

    // Helper: pick the best fork using the Praos comparator.  Returns the
    // (hash, block_no, slot) of the preferred candidate, or None.
    fn select_best_praos(
        fork_tips: Vec<(Hash32, BlockNo, SlotNo)>,
        current_header: &dugite_primitives::block::BlockHeader,
        db: &ChainDB,
    ) -> Option<(Hash32, BlockNo, SlotNo)> {
        // Era for the slot-window decision: use the current tip's era,
        // which always matches the candidate's era within a 5-slot
        // tiebreaker window.
        let era = current_header.protocol_version.era();
        let slot_window: u64 = match era {
            Era::Conway | Era::Dijkstra => 5, // RestrictedVRFTiebreaker 5
            // Byron uses density not Praos; `prefer_chain_with_headers`
            // short-circuits to `compare_density` and never consults
            // `slot_window`. Value is irrelevant — keep `u64::MAX` for
            // consistency with other unrestricted arms.
            Era::Byron => u64::MAX,
            _ => u64::MAX, // pre-Conway Praos: unrestricted
        };

        let mut sel = ChainSelection::new();
        sel.set_tip(Tip {
            point: Point::Specific(current_header.slot, current_header.header_hash),
            block_number: current_header.block_number,
        });

        fork_tips
            .into_iter()
            .filter_map(|(h, bn, slot)| {
                let cand_header = db.get_volatile_header(&h)?.clone();
                let cand_tip = Tip {
                    point: Point::Specific(cand_header.slot, cand_header.header_hash),
                    block_number: cand_header.block_number,
                };
                let pref = sel.prefer_chain_with_headers(
                    &cand_tip,
                    current_header,
                    &cand_header,
                    era,
                    slot_window,
                );
                if matches!(pref, ChainPreference::PreferCandidate) {
                    Some((h, bn, slot))
                } else {
                    None
                }
            })
            // Among preferred candidates, prefer highest block_no.
            .max_by_key(|(_, bn, _)| bn.0)
    }

    // Legacy fallback for callers that did not pass a header (or where
    // some required header is missing).  This preserves the strict-greater
    // semantics used by the older chain_sel_queue tests.
    fn select_best_legacy(
        fork_tips: Vec<(Hash32, BlockNo, SlotNo)>,
        current_tip_block_no: u64,
    ) -> Option<(Hash32, BlockNo, SlotNo)> {
        fork_tips
            .into_iter()
            .filter(|(_h, bn, _slot)| bn.0 > current_tip_block_no)
            .max_by_key(|(_h, bn, _slot)| bn.0)
    }

    let best_fork = match (prefer_praos, current_tip_header.as_ref()) {
        (true, Some(cur_h)) => {
            // Praos path. `select_best_praos` consults the comparator for
            // every fork-tip whose header is cached. Returning `None` here
            // means either:
            //   (a) no candidate's header is cached (legacy / Byron path),
            //       in which case we must fall back to give those callers
            //       some chance of triggering a fork switch; OR
            //   (b) every cached candidate was explicitly rejected by
            //       Praos (PreferCurrent or Equal).
            //
            // Case (b) is safe to fall through because `select_best_legacy`
            // is strictly weaker than Praos for the cases they both decide:
            // any candidate Praos rejected via the EQUAL-block_no
            // tiebreaker has bn == current_tip_block_no, which `> filter`
            // also rejects. A candidate with bn > current_tip would have
            // been chosen by Praos's `compare_length` (PreferCandidate),
            // so we never reach this fallback for that case.
            select_best_praos(fork_tips.clone(), cur_h, &db)
                .or_else(|| select_best_legacy(fork_tips, current_tip_block_no))
        }
        _ => select_best_legacy(fork_tips, current_tip_block_no),
    };

    if let Some((fork_hash, fork_bn, fork_slot)) = best_fork {
        // Haskell `truncateRejectedBlocks`: never adopt a candidate whose
        // chain contains a known-invalid block (e.g. one that failed ledger
        // application during a prior fork replay). Without this, a peer
        // extending such a fork would make us re-adopt it, re-fail the
        // replay, and roll back — an endless loop. Gated on a non-empty
        // invalid cache, so this is a no-op during normal sync.
        if invalid_snapshot
            .as_ref()
            .is_some_and(|inv| db.candidate_contains_invalid(&fork_hash, inv))
        {
            warn!(
                fork_hash = %fork_hash.to_hex(),
                fork_block_no = fork_bn.0,
                "chain_sel: candidate fork contains a known-invalid block — \
                 refusing to adopt (StoreButDontChange)"
            );
            return Some(AddBlockResult::StoredAsFork);
        }

        info!(
            fork_hash = %fork_hash.to_hex(),
            fork_block_no = fork_bn.0,
            fork_slot = fork_slot.0,
            current_tip_block_no,
            "chain_sel: switching to longer fork"
        );

        if let Some(plan) = db.switch_to_fork(
            &fork_hash,
            ledger_can_reach_origin.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            return Some(AddBlockResult::TriggeredFork {
                intersection_hash: plan.intersection,
                intersection_slot: SlotNo(plan.intersection_slot),
                rollback: plan.rollback,
                apply: plan.apply,
            });
        }
        // `switch_to_fork` returned None: the intersection is not
        // reachable within the VolatileDB window.  Per Haskell
        // `isReachable = Nothing` (`Paths.hs`), this is the
        // `StoreButDontChange` case — the block stays in VolatileDB but
        // no chain selection occurs.  We fall through so the caller does
        // NOT attempt a ledger rollback; the block will re-enter chain
        // selection later if its ancestry becomes complete.
        // #1057: report the INPUTS to the decision, not just its outcome.
        //
        // "fork unreachable" is the same message whether the fork's root is genesis
        // or an unknown mid-chain hash, and whether the genesis arm was closed
        // because the ledger cannot reach Origin or because the root simply does not
        // anchor anywhere. Three separate live runs were spent guessing between those
        // cases from a log line that could not distinguish them.
        //
        // `ledger_can_reach_origin` is the one input a reader cannot recover from the
        // ChainDB afterwards, so it is the important one to state.
        warn!(
            fork_hash = %fork_hash.to_hex(),
            fork_block_no = fork_bn.0,
            fork_slot = fork_slot.0,
            current_tip_block_no,
            ledger_can_reach_origin =
                ledger_can_reach_origin.load(std::sync::atomic::Ordering::Relaxed),
            immutable_anchored = db.get_immutable_tip_point().is_some(),
            volatile_selected_len = db.volatile_selected_chain_count(),
            "chain_sel: fork unreachable — StoreButDontChange"
        );
    }

    None
}

/// Core processing logic for a single `AddBlock` message.
///
/// Extracted from the runner loop to make unit testing straightforward.
#[allow(clippy::too_many_arguments)]
async fn process_add_block(
    hash: &BlockHeaderHash,
    slot: SlotNo,
    block_no: BlockNo,
    prev_hash: BlockHeaderHash,
    cbor: Vec<u8>,
    header: Option<&dugite_primitives::block::BlockHeader>,
    self_forged: bool,
    chain_db: &Arc<RwLock<ChainDB>>,
    invalid_cache: &Arc<RwLock<InvalidBlockCache>>,
    // #1057 half A: gates `switch_chain`'s genesis-anchor arm.
    ledger_can_reach_origin: &Arc<std::sync::atomic::AtomicBool>,
) -> AddBlockResult {
    // --- Step 1: Duplicate check (VolatileDB + ImmutableDB) ----------------
    {
        // Acquire a read lock — no writes needed just for the duplicate check.
        let db = chain_db.read().await;
        if db.has_block(hash) {
            trace!(hash = %hash.to_hex(), "chain_sel: block already known");
            return AddBlockResult::AlreadyKnown;
        }
    }

    // --- Self-forged fast path (LoE-exempt) --------------------------------
    //
    // The node's own forged block extends its selection unconditionally: the
    // Limit on Eagerness restrains trust in PEER chains, not the node's own
    // production. Bypassing the LoE here keeps a genesis-mode block producer
    // from deferring its own blocks past k. Chain selection over competing
    // forks is unnecessary — the forge built this block directly on the
    // current tip.
    if self_forged {
        let mut db = chain_db.write().await;
        let extended = match header {
            Some(h) => db.add_self_forged_block_with_header(
                hash.to_owned(),
                slot,
                block_no,
                prev_hash,
                cbor,
                h.clone(),
            ),
            None => db.add_self_forged_block(hash.to_owned(), slot, block_no, prev_hash, cbor),
        };
        return match extended {
            Ok(true) => match db.get_tip_info() {
                Some((tip_slot, tip_hash, tip_block_no)) => AddBlockResult::AddedAsTip {
                    tip_hash,
                    tip_slot,
                    tip_block_no,
                },
                None => AddBlockResult::StoredAsFork,
            },
            Ok(false) => AddBlockResult::StoredAsFork,
            Err(e) => AddBlockResult::Invalid(format!("self-forged storage write failed: {e}")),
        };
    }

    // --- Step 2: Invalid-block cache check ---------------------------------
    //
    // Reject the arriving block if it is itself known-invalid, and snapshot the
    // cache so Step 4 can refuse to adopt any fork whose ancestry contains a
    // known-invalid block (Haskell `truncateRejectedBlocks`). The snapshot is
    // `None` whenever the cache is empty — the normal case — so the ancestry
    // check is a zero-cost no-op during healthy sync.
    let invalid_snapshot: Option<std::collections::HashSet<Hash32>> = {
        let mut cache = invalid_cache.write().await;
        if let Some(reason) = cache.get(hash) {
            debug!(
                hash = %hash.to_hex(),
                reason,
                "chain_sel: block is in invalid cache"
            );
            return AddBlockResult::Invalid(reason.to_owned());
        }
        if cache.is_empty() {
            None
        } else {
            Some(cache.hash_set())
        }
    };

    // --- Step 3: Write to VolatileDB ---------------------------------------
    let extended_tip;
    {
        let mut db = chain_db.write().await;
        let add_result = match header {
            Some(h) => db.add_block_with_header(
                hash.to_owned(),
                slot,
                block_no,
                prev_hash,
                cbor,
                h.clone(),
            ),
            None => db.add_block(hash.to_owned(), slot, block_no, prev_hash, cbor),
        };
        match add_result {
            Ok(did_extend) => {
                extended_tip = did_extend;
            }
            Err(e) => {
                warn!(
                    hash = %hash.to_hex(),
                    error = %e,
                    "chain_sel: failed to write block to VolatileDB"
                );
                return AddBlockResult::Invalid(format!("storage write failed: {e}"));
            }
        }
    }

    // --- Step 4: Chain selection (Haskell `chainSelectionForBlock`) ---------
    //
    // Factored into `run_selection_pass` so the LoE reprocess path
    // (`ChainSelMessage::ReprocessLoE`) can re-run selection without a new
    // block. See that function for the full Haskell-parity notes.
    if let Some(result) = run_selection_pass(
        chain_db,
        &invalid_snapshot,
        header.is_some(),
        ledger_can_reach_origin,
    )
    .await
    {
        return result;
    }

    // If the block extended our selected_chain, surface the new tip.
    if extended_tip {
        let db = chain_db.read().await;
        if let Some((tip_slot, tip_hash, tip_block_no)) = db.get_tip_info() {
            return AddBlockResult::AddedAsTip {
                tip_hash,
                tip_slot,
                tip_block_no,
            };
        }
    }

    AddBlockResult::StoredAsFork
}

// ---------------------------------------------------------------------------
// ChainSelHandle
// ---------------------------------------------------------------------------

/// Client-side handle for submitting blocks to the chain-selection queue.
///
/// Cheap to clone — it is just an `mpsc::Sender` plus a reference to the
/// shared invalid-block cache.  Each handle can be given to a different
/// subsystem (sync pipeline, block forger, test harness) independently.
///
/// # Example
///
/// ```rust,ignore
/// let (handle, runner_future) = ChainSelHandle::new(chain_db.clone());
/// tokio::spawn(runner_future);
///
/// let result = handle
///     .submit_block(hash, slot, block_no, prev_hash, cbor)
///     .await
///     .unwrap();
/// ```
#[derive(Clone)]
pub struct ChainSelHandle {
    tx: mpsc::Sender<ChainSelMessage>,
    /// Shared invalid-block cache.  Exposed so callers can pre-seed the cache
    /// (e.g. from a persisted blacklist) or inspect it for monitoring.
    pub invalid_cache: Arc<RwLock<InvalidBlockCache>>,
    /// #1057: can the LEDGER be rolled back to Origin?
    ///
    /// NOT "is the ledger at Origin" — that gate was implemented, measured, and
    /// found too narrow: holding ANY chain disqualified the longer one, including a
    /// chain adopted seconds earlier from a peer that was itself broken. On the
    /// devnet a node that had just reset re-adopted a stale 10-block chain from its
    /// sibling and was wedged again 30 seconds later against the canonical
    /// 155-block chain.
    ///
    /// The real predicate is that a rollback to Origin is EXECUTABLE, which holds
    /// when the LedgerSeq's anchor IS Origin and the window is coherent:
    /// `find_rollback_n(Origin)` then returns `Some(deltas.len())` — the
    /// full-rewind-to-anchor case — so no snapshot and no re-initialisation is
    /// needed. A snapshot-restored ledger has a non-Origin anchor, which is exactly
    /// why the earlier attempt hit "Rollback target outside LedgerSeq volatile
    /// window AND no canonical snapshot available".
    ///
    /// `dugite-storage` sits below `dugite-ledger` and cannot read either, so the
    /// node publishes the answer via
    /// [`ChainSelHandle::set_ledger_can_reach_origin`].
    ///
    /// Defaults to `false`, the conservative direction: the genesis arm stays off,
    /// i.e. exactly the pre-#1057 behaviour. A caller that forgets to publish loses
    /// the fix, never correctness.
    ledger_can_reach_origin: Arc<std::sync::atomic::AtomicBool>,
}

impl ChainSelHandle {
    /// Default MPSC channel capacity.
    ///
    /// Chosen to be large enough to absorb a burst of pipelined block-fetch
    /// responses (pipeline depth is typically 300) while keeping backpressure
    /// intact.  When the queue fills the sender will apply natural backpressure
    /// via `await` in [`submit_block`].
    pub const DEFAULT_CHANNEL_CAPACITY: usize = 512;

    /// Create a new `ChainSelHandle` and return the associated runner future.
    ///
    /// Callers MUST spawn the returned future (via `tokio::spawn`) before
    /// calling `submit_block`.
    ///
    /// ```rust,ignore
    /// let (handle, runner) = ChainSelHandle::new(chain_db);
    /// tokio::spawn(runner);
    /// ```
    pub fn new(chain_db: Arc<RwLock<ChainDB>>) -> (Self, impl std::future::Future<Output = ()>) {
        Self::with_capacity(chain_db, Self::DEFAULT_CHANNEL_CAPACITY)
    }

    /// Create with a custom channel capacity.  Primarily useful in tests.
    pub fn with_capacity(
        chain_db: Arc<RwLock<ChainDB>>,
        capacity: usize,
    ) -> (Self, impl std::future::Future<Output = ()>) {
        let invalid_cache = Arc::new(RwLock::new(InvalidBlockCache::new()));
        let (tx, rx) = mpsc::channel(capacity);
        let ledger_can_reach_origin = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let runner = add_block_runner(
            rx,
            chain_db,
            Arc::clone(&invalid_cache),
            Arc::clone(&ledger_can_reach_origin),
        );

        let handle = ChainSelHandle {
            tx,
            invalid_cache,
            ledger_can_reach_origin,
        };

        (handle, runner)
    }

    /// Publish whether the ledger can be rolled back to Origin (#1057).
    ///
    /// The node calls this whenever the ledger tip changes. It gates
    /// `VolatileDB::switch_chain`'s genesis-anchor arm, which must not emit a
    /// genesis-rooted `SwitchPlan` the ledger cannot execute — doing so relocates
    /// the #1057 wedge from BlockFetch to the ledger rollback, which was measured
    /// live and reverted.
    pub fn set_ledger_can_reach_origin(&self, at_origin: bool) {
        self.ledger_can_reach_origin
            .store(at_origin, std::sync::atomic::Ordering::Relaxed);
    }

    /// Re-run chain selection after the Limit on Eagerness advanced
    /// (Haskell `ChainSelReprocessLoEBlocks` / `triggerChainSelectionAsync`).
    ///
    /// Returns the selection outcome — `Some(TriggeredFork{..})` when a
    /// previously-deferred candidate is now adoptable (the caller must run
    /// the fork-switch plan), `Some(StoredAsFork)` when nothing changed,
    /// `None` when the runner has exited.
    pub async fn reprocess_loe(&self) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();
        self.tx
            .send(ChainSelMessage::ReprocessLoE { result_tx })
            .await
            .ok()?;
        result_rx.await.ok()
    }

    /// Submit a block for chain-selection processing.
    ///
    /// Awaits backpressure if the queue is full.  Returns `None` if the
    /// background runner has exited (i.e. the channel is closed).
    ///
    /// # Arguments
    ///
    /// * `hash` — Blake2b-256 block header hash.
    /// * `slot` — Absolute slot number.
    /// * `block_no` — Block height.
    /// * `prev_hash` — Hash of the parent block.
    /// * `cbor` — Raw CBOR bytes of the complete block.
    pub async fn submit_block(
        &self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
    ) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();

        self.tx
            .send(ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header: None, // legacy path, no Praos tiebreak
                self_forged: false,
                result_tx,
            })
            .await
            .ok()?;

        result_rx.await.ok()
    }

    /// Variant of [`submit_block`] that also forwards the block's
    /// `BlockHeader` so chain selection can run the Praos tiebreaker
    /// (Bug D / issue #497).
    ///
    /// Production callers (live BlockFetch path, forge path) should call this
    /// method.  The legacy [`submit_block`] is retained as a thin wrapper for
    /// tests and any code that cannot easily obtain a header.
    pub async fn submit_block_with_header(
        &self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
        header: dugite_primitives::block::BlockHeader,
    ) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();

        self.tx
            .send(ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header: Some(header),
                self_forged: false,
                result_tx,
            })
            .await
            .ok()?;

        result_rx.await.ok()
    }

    /// Submit a block the LOCAL node forged. LoE-exempt: it extends the
    /// node's own selection unconditionally (see `AddBlock::self_forged`).
    pub async fn submit_self_forged_block_with_header(
        &self,
        hash: BlockHeaderHash,
        slot: SlotNo,
        block_no: BlockNo,
        prev_hash: BlockHeaderHash,
        cbor: Vec<u8>,
        header: dugite_primitives::block::BlockHeader,
    ) -> Option<AddBlockResult> {
        let (result_tx, result_rx) = oneshot::channel();
        self.tx
            .send(ChainSelMessage::AddBlock {
                hash,
                slot,
                block_no,
                prev_hash,
                cbor,
                header: Some(header),
                self_forged: true,
                result_tx,
            })
            .await
            .ok()?;
        result_rx.await.ok()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};
    use std::path::Path;

    // -----------------------------------------------------------------------
    // Helper: open a ChainDB in a temp dir
    // -----------------------------------------------------------------------

    fn make_chain_db(dir: &Path) -> Arc<RwLock<ChainDB>> {
        let db = ChainDB::open(dir).expect("failed to open test ChainDB");
        Arc::new(RwLock::new(db))
    }

    /// Minimal synthetic CBOR for tests: just the hash bytes, enough to be
    /// non-empty and distinguishable per block.
    fn fake_cbor(hash: &Hash32) -> Vec<u8> {
        hash.as_bytes().to_vec()
    }

    // -----------------------------------------------------------------------
    // 1. AlreadyKnown: duplicate block
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_add_block_already_known() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let hash = Hash32::from_bytes([0x01; 32]);
        let slot = SlotNo(1000);
        let block_no = BlockNo(1);
        let prev = Hash32::ZERO;
        let cbor = fake_cbor(&hash);

        // First submission: new block extends chain → AddedAsTip
        let r1 = handle
            .submit_block(hash, slot, block_no, prev, cbor.clone())
            .await
            .expect("runner exited unexpectedly");
        assert!(
            matches!(r1, AddBlockResult::AddedAsTip { .. }),
            "first submission of a chain-extending block must return AddedAsTip, got {r1:?}"
        );

        // Second submission with the same hash → AlreadyKnown
        let r2 = handle
            .submit_block(hash, slot, block_no, prev, cbor.clone())
            .await
            .expect("runner exited unexpectedly");
        assert_eq!(r2, AddBlockResult::AlreadyKnown);
    }

    // -----------------------------------------------------------------------
    // 2. AddedAsTip: new block extends selected chain
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_add_block_added_as_tip() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let hash = Hash32::from_bytes([0xAB; 32]);
        let slot = SlotNo(42);
        let block_no = BlockNo(1);
        let prev = Hash32::ZERO;
        let cbor = fake_cbor(&hash);

        let result = handle
            .submit_block(hash, slot, block_no, prev, cbor)
            .await
            .expect("runner exited unexpectedly");

        match result {
            AddBlockResult::AddedAsTip {
                tip_hash,
                tip_slot,
                tip_block_no,
            } => {
                assert_eq!(
                    tip_hash, hash,
                    "tip_hash must equal the submitted block hash"
                );
                assert_eq!(tip_slot, slot);
                assert_eq!(tip_block_no, block_no);
            }
            other => panic!("expected AddedAsTip, got {other:?}"),
        }

        // Verify the block actually landed in the VolatileDB.
        let db = chain_db.read().await;
        assert!(db.has_block(&hash), "block should be present in VolatileDB");
    }

    // -----------------------------------------------------------------------
    // 2b. Forge-path invariant: extending block becomes selected_chain tip
    // -----------------------------------------------------------------------
    //
    // Positive case for #439 follow-up: when the submitted block's `prev_hash`
    // matches the current selected_chain tip, the block MUST become the new tip
    // and `AddedAsTip` must be returned (no separate re-lookup needed).

    #[tokio::test]
    async fn test_forge_path_extending_block_becomes_tip() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Genesis block.
        let genesis = Hash32::from_bytes([0x01; 32]);
        handle
            .submit_block(
                genesis,
                SlotNo(1),
                BlockNo(0),
                Hash32::ZERO,
                fake_cbor(&genesis),
            )
            .await
            .unwrap();

        // Forged block extending genesis.
        let forged = Hash32::from_bytes([0x02; 32]);
        let result = handle
            .submit_block(forged, SlotNo(10), BlockNo(1), genesis, fake_cbor(&forged))
            .await
            .unwrap();

        match result {
            AddBlockResult::AddedAsTip {
                tip_hash,
                tip_slot,
                tip_block_no,
            } => {
                assert_eq!(
                    tip_hash, forged,
                    "AddedAsTip.tip_hash must equal the forged block hash"
                );
                assert_eq!(tip_slot, SlotNo(10));
                assert_eq!(tip_block_no, BlockNo(1));
            }
            other => panic!(
                "extending block on an unopposed chain should return AddedAsTip, got {other:?}"
            ),
        }

        // Critical invariant: VolatileDB's selected-chain tip MUST be the
        // forged block, because `insert_block_internal` advances the tip
        // whenever `prev_hash == selected_chain.last()`.
        let db = chain_db.read().await;
        let (_slot, tip_hash, tip_bn) = db
            .get_tip_info()
            .expect("tip info should exist after forge");
        assert_eq!(
            tip_hash, forged,
            "forge-path invariant: forged block MUST be selected-chain tip"
        );
        assert_eq!(tip_bn, BlockNo(1));
    }

    // -----------------------------------------------------------------------
    // 2c. Forge-path invariant: race-lost block is NOT selected_chain tip
    // -----------------------------------------------------------------------
    //
    // Models the sequence behind issue #439:
    //   1. BP starts forging against tip X at height H.
    //   2. Upstream delivers Y at H (also extending X), becoming tip.
    //   3. BP's forged block Z at height H+1 WITH prev_hash=X arrives at the
    //      queue AFTER Y — so Z's prev_hash no longer matches the
    //      selected_chain tip (which is now Y).
    //   4. `insert_block_internal` stores Z as a FORK block (not on
    //      selected_chain).  `forged_is_tip` must be false and the forge
    //      path must abort without ledger-applying or announcing Z.

    #[tokio::test]
    async fn test_forge_path_race_lost_block_is_not_tip() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let x = Hash32::from_bytes([0xA0; 32]);
        handle
            .submit_block(x, SlotNo(1), BlockNo(0), Hash32::ZERO, fake_cbor(&x))
            .await
            .unwrap();

        // Upstream block Y lands first — Y extends X and becomes tip.
        let y = Hash32::from_bytes([0xB0; 32]);
        handle
            .submit_block(y, SlotNo(2), BlockNo(1), x, fake_cbor(&y))
            .await
            .unwrap();

        // BP's forged block Z arrives LATE — still claims prev_hash=X but
        // selected_chain tip is now Y. Z is stored as a fork block.
        let z = Hash32::from_bytes([0xC0; 32]);
        let result = handle
            .submit_block(z, SlotNo(3), BlockNo(1), x, fake_cbor(&z))
            .await
            .unwrap();

        assert_eq!(
            result,
            AddBlockResult::StoredAsFork,
            "race-lost block must return StoredAsFork (not AddedAsTip)"
        );

        // Forge-path invariant: the forged block must NOT be selected-chain tip.
        let db = chain_db.read().await;
        let (_slot, tip_hash, _tip_bn) = db.get_tip_info().expect("tip exists");
        assert_eq!(
            tip_hash, y,
            "selected-chain tip must remain at Y (the race winner) — Z lost"
        );
        assert_ne!(
            tip_hash, z,
            "forge-path invariant: race-lost Z MUST NOT be the tip; \
             forge-path `forged_is_tip` check must detect this and abort"
        );
        assert!(db.has_block(&z), "Z must still be stored as a fork block");
    }

    // -----------------------------------------------------------------------
    // 3. Chain selection: TriggeredFork returned for longer competing fork
    // -----------------------------------------------------------------------

    /// Verify that submitting two competing forks causes chain selection to
    /// return `TriggeredFork` for the block that makes the fork strictly longer.
    ///
    /// Chain layout:
    ///
    ///   common → a2 → a3          (selected chain, block_nos 2, 3)
    ///          ↘ b2 → b3 → b4    (fork, block_nos 2, 3, 4 — strictly longer)
    ///
    /// When b4 arrives, chain selection should switch to the b-fork and return
    /// `TriggeredFork { rollback: [a3, a2], apply: [b2, b3, b4] }`.
    #[tokio::test]
    async fn test_chain_selection_switches_to_longer_fork() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner = tokio::spawn(runner);

        // All hashes use a fixed high byte to stay far from ZERO.
        let common = Hash32::from_bytes([0xC0; 32]);
        let a2 = Hash32::from_bytes([0xA2; 32]);
        let a3 = Hash32::from_bytes([0xA3; 32]);
        let b2 = Hash32::from_bytes([0xB2; 32]);
        let b3 = Hash32::from_bytes([0xB3; 32]);
        let b4 = Hash32::from_bytes([0xB4; 32]);

        // Build main (a) chain.
        let r = handle
            .submit_block(
                common,
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&common),
            )
            .await
            .unwrap();
        assert!(
            matches!(r, AddBlockResult::AddedAsTip { .. }),
            "common: {r:?}"
        );

        let r = handle
            .submit_block(a2, SlotNo(200), BlockNo(2), common, fake_cbor(&a2))
            .await
            .unwrap();
        assert!(matches!(r, AddBlockResult::AddedAsTip { .. }), "a2: {r:?}");

        let r = handle
            .submit_block(a3, SlotNo(300), BlockNo(3), a2, fake_cbor(&a3))
            .await
            .unwrap();
        assert!(matches!(r, AddBlockResult::AddedAsTip { .. }), "a3: {r:?}");

        // Build competing (b) fork starting from common.
        // b2 and b3 have the same block_nos as a2/a3 — no switch yet.
        let r = handle
            .submit_block(b2, SlotNo(200), BlockNo(2), common, fake_cbor(&b2))
            .await
            .unwrap();
        // b2 is a fork tip with block_no=2, but selected chain tip is a3 at
        // block_no=3, so b2 does NOT trigger a switch.
        assert_eq!(r, AddBlockResult::StoredAsFork, "b2: {r:?}");

        let r = handle
            .submit_block(b3, SlotNo(300), BlockNo(3), b2, fake_cbor(&b3))
            .await
            .unwrap();
        // b3 block_no=3 == current tip a3 block_no=3.
        // Strictly-greater check: 3 > 3 is false → no switch.
        assert_eq!(r, AddBlockResult::StoredAsFork, "b3: {r:?}");

        // b4 extends the fork to block_no=4, strictly longer than a3 (3).
        let r = handle
            .submit_block(b4, SlotNo(400), BlockNo(4), b3, fake_cbor(&b4))
            .await
            .unwrap();

        match r {
            AddBlockResult::TriggeredFork {
                intersection_hash: _,
                intersection_slot: _,
                rollback,
                apply,
            } => {
                // Rollback should un-apply the a-chain blocks above common.
                assert!(
                    rollback.contains(&a3) && rollback.contains(&a2),
                    "rollback should include a3 and a2, got: {rollback:?}"
                );
                // Apply should bring in the b-chain blocks.
                assert!(
                    apply.contains(&b2) && apply.contains(&b3) && apply.contains(&b4),
                    "apply should include b2, b3, b4, got: {apply:?}"
                );
                // common should NOT appear in either list.
                assert!(
                    !rollback.contains(&common) && !apply.contains(&common),
                    "intersection block should not appear in rollback/apply"
                );
            }
            other => panic!("expected TriggeredFork but got: {other:?}"),
        }

        // After the switch, the VolatileDB tip should be b4.
        let db = chain_db.read().await;
        let tip = db.get_tip_info().expect("should have a tip");
        assert_eq!(tip.2 .0, 4, "tip block_no should be 4 (b4)");
    }

    /// A longer fork whose chain contains a KNOWN-INVALID block must NOT be
    /// adopted (Haskell `truncateRejectedBlocks`). Regression for the
    /// fork-replay-failure loop: after a bad fork block is marked invalid, a
    /// peer extending that fork must not make us re-adopt (and re-fail) it.
    #[tokio::test]
    async fn test_chain_selection_refuses_fork_with_invalid_block() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner = tokio::spawn(runner);

        let common = Hash32::from_bytes([0xC0; 32]);
        let a2 = Hash32::from_bytes([0xA2; 32]);
        let a3 = Hash32::from_bytes([0xA3; 32]);
        let b2 = Hash32::from_bytes([0xB2; 32]);
        let b3 = Hash32::from_bytes([0xB3; 32]);
        let b4 = Hash32::from_bytes([0xB4; 32]);

        // Main chain common → a2 → a3 (tip block_no 3).
        for (h, slot, bn, prev) in [
            (common, 100u64, 1u64, Hash32::ZERO),
            (a2, 200, 2, common),
            (a3, 300, 3, a2),
        ] {
            handle
                .submit_block(h, SlotNo(slot), BlockNo(bn), prev, fake_cbor(&h))
                .await
                .unwrap();
        }
        // Competing fork common → b2 → b3 (stored, not yet longer).
        for (h, slot, bn, prev) in [(b2, 200u64, 2u64, common), (b3, 300, 3, b2)] {
            let r = handle
                .submit_block(h, SlotNo(slot), BlockNo(bn), prev, fake_cbor(&h))
                .await
                .unwrap();
            assert_eq!(r, AddBlockResult::StoredAsFork, "{h:?}");
        }

        // Mark b3 invalid (as if its replay had failed).
        handle
            .invalid_cache
            .write()
            .await
            .insert(b3, "test: forced invalid".to_string());

        // b4 extends the fork to block_no 4 — strictly longer than a3 — so it
        // WOULD normally trigger a switch. But the fork's chain contains the
        // invalid b3, so chain selection must refuse it.
        let r = handle
            .submit_block(b4, SlotNo(400), BlockNo(4), b3, fake_cbor(&b4))
            .await
            .unwrap();
        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "fork containing invalid b3 must not be adopted, got: {r:?}"
        );

        // The selected tip must remain a3 (block_no 3) — no switch occurred.
        let db = chain_db.read().await;
        let tip = db.get_tip_info().expect("should have a tip");
        assert_eq!(tip.1, a3, "tip must remain a3 (poisoned fork rejected)");
        assert_eq!(tip.2 .0, 3, "tip block_no must remain 3");
    }

    /// Verify that equal-length chains do NOT trigger a fork switch.
    ///
    /// Haskell invariant: chain selection only switches to a STRICTLY-preferred
    /// candidate (block_no > current tip). Equal block_no is not sufficient.
    #[tokio::test]
    async fn test_chain_selection_no_switch_equal_length() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner = tokio::spawn(runner);

        let common = Hash32::from_bytes([0xC0; 32]);
        let a2 = Hash32::from_bytes([0xA2; 32]);
        let b2 = Hash32::from_bytes([0xB2; 32]);

        handle
            .submit_block(
                common,
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&common),
            )
            .await
            .unwrap();
        handle
            .submit_block(a2, SlotNo(200), BlockNo(2), common, fake_cbor(&a2))
            .await
            .unwrap();

        // b2 has the same block_no as a2 — no switch should occur.
        let r = handle
            .submit_block(b2, SlotNo(200), BlockNo(2), common, fake_cbor(&b2))
            .await
            .unwrap();
        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "equal-length fork must not trigger a switch (b2 is a fork block)"
        );

        // Selected chain tip is still a2.
        let db = chain_db.read().await;
        let tip = db.get_tip_info().expect("should have a tip");
        assert_eq!(tip.2 .0, 2, "selected-chain tip block_no should still be 2");
    }

    // -----------------------------------------------------------------------
    // 4. InvalidBlockCache: insert / lookup / TTL
    // -----------------------------------------------------------------------

    #[test]
    fn test_invalid_block_cache_insert_and_lookup() {
        let mut cache = InvalidBlockCache::new();
        let hash = Hash32::from_bytes([0x11; 32]);

        // Initially absent.
        assert!(cache.get(&hash).is_none());

        cache.insert(hash, "bad VRF proof".to_string());

        // Now present.
        let reason = cache.get(&hash).expect("should be in cache");
        assert_eq!(reason, "bad VRF proof");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_invalid_block_cache_ttl_expiry() {
        // Use a TTL short enough to expire within a test.
        let mut cache = InvalidBlockCache::with_ttl(Duration::from_millis(1));
        let hash = Hash32::from_bytes([0x22; 32]);

        cache.insert(hash, "expired entry".to_string());

        // Entry is present immediately after insertion.
        assert!(cache.get(&hash).is_some());

        // Wait for TTL to expire.
        std::thread::sleep(Duration::from_millis(10));

        // Lookup should find the entry expired and return None.
        assert!(cache.get(&hash).is_none(), "expired entry should be absent");
        // Lazy removal should have shrunk the map.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_invalid_block_cache_refresh_existing() {
        let mut cache = InvalidBlockCache::new();
        let hash = Hash32::from_bytes([0x33; 32]);

        cache.insert(hash, "reason A".to_string());
        assert_eq!(cache.len(), 1);

        // Re-inserting the same hash updates reason, does not grow the cache.
        cache.insert(hash, "reason B".to_string());
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&hash).unwrap(), "reason B");
    }

    #[test]
    fn test_invalid_block_cache_eviction_at_capacity() {
        let mut cache = InvalidBlockCache::new();

        // Fill to exactly MAX_ENTRIES.
        for i in 0..InvalidBlockCache::MAX_ENTRIES {
            let mut bytes = [0u8; 32];
            let idx_bytes = (i as u64).to_be_bytes();
            bytes[..8].copy_from_slice(&idx_bytes);
            cache.insert(Hash32::from_bytes(bytes), format!("reason {i}"));
        }
        assert_eq!(cache.len(), InvalidBlockCache::MAX_ENTRIES);

        // The first entry inserted (i=0).
        let mut first_bytes = [0u8; 32];
        first_bytes[..8].copy_from_slice(&0u64.to_be_bytes());
        let first_hash = Hash32::from_bytes(first_bytes);

        // Inserting one more entry should evict the oldest (i=0).
        let mut new_bytes = [0xFF; 32];
        new_bytes[0] = 0xFE; // make it unique
        cache.insert(Hash32::from_bytes(new_bytes), "new entry".to_string());

        // Cache size must stay bounded.
        assert_eq!(cache.len(), InvalidBlockCache::MAX_ENTRIES);

        // The oldest entry should be gone.
        assert!(
            cache.get(&first_hash).is_none(),
            "oldest entry should have been evicted"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Invalid block cache wired into the runner
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_add_block_invalid_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let hash = Hash32::from_bytes([0x99; 32]);

        // Pre-seed the invalid cache.
        {
            let mut cache = handle.invalid_cache.write().await;
            cache.insert(hash, "pre-seeded invalid".to_string());
        }

        let result = handle
            .submit_block(hash, SlotNo(5), BlockNo(1), Hash32::ZERO, fake_cbor(&hash))
            .await
            .expect("runner exited unexpectedly");

        assert_eq!(
            result,
            AddBlockResult::Invalid("pre-seeded invalid".to_string())
        );

        // Verify the block was NOT written to storage.
        let db = chain_db.read().await;
        assert!(
            !db.has_block(&hash),
            "invalid block must not reach VolatileDB"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Concurrent block submission
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_concurrent_block_submission() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        // Use a generous channel capacity for this burst test.
        let (handle, runner) = ChainSelHandle::with_capacity(Arc::clone(&chain_db), 256);
        let _runner_task = tokio::spawn(runner);

        const N: usize = 64;
        let mut tasks = Vec::with_capacity(N);

        for i in 0..N {
            let h = handle.clone();
            // Use i+1 so that no block hash collides with Hash32::ZERO
            // (which is used as prev_hash for all blocks).  If hash == ZERO
            // == prev_hash, walk_chain_back() would loop forever.
            let mut hash_bytes = [0u8; 32];
            hash_bytes[..8].copy_from_slice(&((i as u64) + 1).to_be_bytes());
            let hash = Hash32::from_bytes(hash_bytes);
            let cbor = fake_cbor(&hash);

            tasks.push(tokio::spawn(async move {
                h.submit_block(
                    hash,
                    SlotNo(i as u64),
                    BlockNo(i as u64),
                    Hash32::ZERO,
                    cbor,
                )
                .await
                .expect("runner exited")
            }));
        }

        let mut stored = 0usize;
        let mut switched = 0usize;
        let mut already_known = 0usize;

        for task in tasks {
            match task.await.unwrap() {
                AddBlockResult::AddedAsTip { .. } | AddBlockResult::StoredAsFork => stored += 1,
                AddBlockResult::AlreadyKnown => already_known += 1,
                // Each block has a unique block_no so chain selection may
                // switch to a longer fork as blocks arrive out of order.
                AddBlockResult::TriggeredFork { .. } => switched += 1,
                other => panic!("unexpected result: {other:?}"),
            }
        }

        // All N hashes are distinct; every block must be either stored or
        // trigger a fork switch (both outcomes mean the block is in storage).
        assert_eq!(
            stored + switched,
            N,
            "all unique blocks should be stored (stored={stored}, switched={switched})"
        );
        assert_eq!(already_known, 0, "no duplicates submitted");

        // Verify VolatileDB contains exactly N blocks.
        let db = chain_db.read().await;
        assert_eq!(
            db.volatile_block_count(),
            N,
            "VolatileDB should contain {N} blocks"
        );
    }

    // -----------------------------------------------------------------------
    // 6. AddedAsTip / StoredAsFork disambiguation (TDD for #439 follow-up)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_extending_block_returns_added_as_tip() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let genesis = Hash32::from_bytes([0x01; 32]);
        handle
            .submit_block(
                genesis,
                SlotNo(1),
                BlockNo(0),
                Hash32::ZERO,
                fake_cbor(&genesis),
            )
            .await
            .unwrap();

        let extending = Hash32::from_bytes([0x02; 32]);
        let result = handle
            .submit_block(
                extending,
                SlotNo(10),
                BlockNo(1),
                genesis,
                fake_cbor(&extending),
            )
            .await
            .unwrap();

        match result {
            AddBlockResult::AddedAsTip {
                tip_hash,
                tip_slot,
                tip_block_no,
            } => {
                assert_eq!(tip_hash, extending);
                assert_eq!(tip_slot, SlotNo(10));
                assert_eq!(tip_block_no, BlockNo(1));
            }
            other => panic!(
                "Extending block must return AddedAsTip, got {other:?}. \
                 This disambiguates the normal forge path from StoredAsFork (race lost)."
            ),
        }
    }

    #[tokio::test]
    async fn test_race_lost_block_returns_stored_as_fork() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let x = Hash32::from_bytes([0xA0; 32]);
        handle
            .submit_block(x, SlotNo(1), BlockNo(0), Hash32::ZERO, fake_cbor(&x))
            .await
            .unwrap();

        let y = Hash32::from_bytes([0xB0; 32]);
        handle
            .submit_block(y, SlotNo(2), BlockNo(1), x, fake_cbor(&y))
            .await
            .unwrap();

        // Z arrives late — still claims prev_hash = x, but selected_chain tip is Y now.
        let z = Hash32::from_bytes([0xC0; 32]);
        let result = handle
            .submit_block(z, SlotNo(3), BlockNo(1), x, fake_cbor(&z))
            .await
            .unwrap();

        assert_eq!(
            result,
            AddBlockResult::StoredAsFork,
            "Race-lost block is a fork block, must return StoredAsFork (not AddedAsTip)"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for #439 correctness fixes (items 2.3 + 2.4)
    // -----------------------------------------------------------------------

    /// Submitting a strictly-longer fork tip must produce a `TriggeredFork`
    /// result whose `intersection_hash` maps to a block present in VolatileDB
    /// AND whose `intersection_slot` equals the slot stored for that block.
    ///
    /// This invariant prevents the cd3d03a92 regression where the intersection
    /// slot lookup via `get_block_location` failed and the code fell back to
    /// `Point::Origin`, causing the ledger to roll back all the way to genesis.
    ///
    /// Chain layout:
    ///
    ///   common (SlotNo 100, BlockNo 1)
    ///     ├── a2 (SlotNo 200, BlockNo 2) → a3 (SlotNo 300, BlockNo 3)
    ///     └── b2 (SlotNo 200, BlockNo 2) → b3 (SlotNo 300, BlockNo 3)
    ///                                         → b4 (SlotNo 400, BlockNo 4)  ← triggers fork
    ///
    /// When b4 arrives:
    ///   - result is `TriggeredFork`
    ///   - `intersection_hash` == common
    ///   - `intersection_slot` == SlotNo(100)
    ///   - `has_block(&intersection_hash)` is true (block is in ChainDB)
    #[tokio::test]
    async fn test_triggered_fork_intersection_slot_is_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner = tokio::spawn(runner);

        let common = Hash32::from_bytes([0xC0; 32]);
        let a2 = Hash32::from_bytes([0xA2; 32]);
        let a3 = Hash32::from_bytes([0xA3; 32]);
        let b2 = Hash32::from_bytes([0xB2; 32]);
        let b3 = Hash32::from_bytes([0xB3; 32]);
        let b4 = Hash32::from_bytes([0xB4; 32]);

        // Build the selected (a) chain.
        let r = handle
            .submit_block(
                common,
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&common),
            )
            .await
            .unwrap();
        assert!(
            matches!(r, AddBlockResult::AddedAsTip { .. }),
            "common must extend tip: {r:?}"
        );

        handle
            .submit_block(a2, SlotNo(200), BlockNo(2), common, fake_cbor(&a2))
            .await
            .unwrap();
        handle
            .submit_block(a3, SlotNo(300), BlockNo(3), a2, fake_cbor(&a3))
            .await
            .unwrap();

        // Build the competing (b) fork — b2 and b3 at same heights, no switch yet.
        let r = handle
            .submit_block(b2, SlotNo(200), BlockNo(2), common, fake_cbor(&b2))
            .await
            .unwrap();
        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "b2 must be stored as fork: {r:?}"
        );

        let r = handle
            .submit_block(b3, SlotNo(300), BlockNo(3), b2, fake_cbor(&b3))
            .await
            .unwrap();
        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "b3 must be stored as fork: {r:?}"
        );

        // b4 is strictly longer (BlockNo 4 > 3) — must trigger a fork switch.
        let r = handle
            .submit_block(b4, SlotNo(400), BlockNo(4), b3, fake_cbor(&b4))
            .await
            .unwrap();

        match r {
            AddBlockResult::TriggeredFork {
                intersection_hash,
                intersection_slot,
                ..
            } => {
                // Invariant 1: intersection must be the common ancestor.
                assert_eq!(
                    intersection_hash, common,
                    "intersection_hash must equal the common ancestor block (item 2.3)"
                );

                // Invariant 2: intersection slot must be resolved from the block
                // stored in VolatileDB, NOT fabricated or defaulted.
                assert_eq!(
                    intersection_slot,
                    SlotNo(100),
                    "intersection_slot must equal the slot stored for `common` \
                     (item 2.4: no Point::Origin fallback)"
                );

                // Invariant 3: the intersection block must be present in ChainDB
                // so any subsequent ledger rollback lookup will succeed.
                let db = chain_db.read().await;
                assert!(
                    db.has_block(&intersection_hash),
                    "intersection block must be present in ChainDB — \
                     a missing block would cause ledger rollback to fail (cd3d03a92 regression)"
                );
            }
            other => panic!("b4 (strictly longer fork) must return TriggeredFork; got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Bug D (issue #497) — Praos tiebreaker regression tests
    // -----------------------------------------------------------------------
    //
    // The legacy tests in this module pass `None` for the header via
    // `submit_block`, so they exercise the strict-greater fallback unchanged.
    // The tests below pass synthesized `BlockHeader`s via the new
    // `submit_block_with_header` method, so they exercise the Praos
    // `comparePraos` tiebreaker imported from dugite-consensus.

    use dugite_primitives::block::{BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
    use dugite_primitives::time::{BlockNo as PrimBlockNo, SlotNo as PrimSlotNo};

    /// Construct a minimal BlockHeader for tiebreaker tests.
    ///
    /// `issuer_vkey` determines the pool ID (blake2b-224 of these bytes).
    /// `vrf_output` is the VRF output bytes used by the Praos cross-pool
    /// tiebreaker — lower lex wins.
    /// `opcert_seq` is the operational certificate sequence number — used by
    /// the same-pool-same-slot tiebreaker.
    /// `protocol_major` selects the era: 9..=11 → Conway (5-slot window),
    /// 12+ → Dijkstra, 7..=8 → Babbage (unrestricted), etc.
    #[allow(clippy::too_many_arguments)]
    fn praos_header(
        hash_bytes: [u8; 32],
        prev_hash_bytes: [u8; 32],
        slot: u64,
        block_no: u64,
        issuer_vkey: Vec<u8>,
        opcert_seq: u64,
        vrf_output: Vec<u8>,
        protocol_major: u64,
    ) -> BlockHeader {
        BlockHeader {
            header_hash: Hash32::from_bytes(hash_bytes),
            prev_hash: Hash32::from_bytes(prev_hash_bytes),
            issuer_vkey,
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vrf_output,
                proof: vec![],
            },
            block_number: PrimBlockNo(block_no),
            slot: PrimSlotNo(slot),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: opcert_seq,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion {
                major: protocol_major,
                minor: 0,
            },
            kes_signature: vec![],
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
        }
    }

    /// Bug D regression: equal block_no + lower VRF on candidate + within the
    /// 5-slot Conway window → MUST trigger a fork switch.
    ///
    /// Mirrors the local-devnet scenario where two BPs slot-battle at f=0.2
    /// and never converged under the old strict-greater filter.
    #[tokio::test]
    async fn praos_tiebreaker_switches_on_equal_block_no_lower_vrf_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Common parent at slot 100, block_no 1 (Conway era, protocol_major 9).
        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes,
            [0u8; 32],
            100,
            1,
            vec![0xAA; 32],
            0,
            vec![0xFF; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .expect("runner alive");

        // Current tip: pool A forges at slot 110, block_no 2, vrf=0xFF.
        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes,
            common_bytes,
            110,
            2,
            vec![0xAA; 32], // pool A vkey
            1,
            vec![0xFFu8; 32], // high VRF
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .expect("runner alive");
        assert!(matches!(r, AddBlockResult::AddedAsTip { .. }), "a: {r:?}");

        // Candidate: pool B forges at slot 112 (within 5-slot window),
        // block_no 2 (same as A), vrf=0x00 (strictly lower than A's 0xFF).
        // Praos tiebreaker: lower VRF wins → MUST switch to B.
        let b_bytes = [0xB2u8; 32];
        let b_header = praos_header(
            b_bytes,
            common_bytes,
            112,
            2,
            vec![0xBB; 32], // pool B vkey (different from A)
            1,
            vec![0x00u8; 32], // low VRF
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b_bytes),
                SlotNo(112),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b_bytes)),
                b_header,
            )
            .await
            .expect("runner alive");

        match r {
            AddBlockResult::TriggeredFork { .. } => {} // OK
            other => panic!(
                "expected TriggeredFork (Praos tiebreaker should switch on lower VRF \
                 within 5-slot window), got: {other:?}"
            ),
        }
    }

    /// Bug D regression: equal block_no + lower VRF on candidate but OUTSIDE
    /// the 5-slot Conway window → MUST NOT switch (RestrictedVRFTiebreaker).
    #[tokio::test]
    async fn praos_tiebreaker_does_not_switch_when_slot_gap_exceeds_window() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Same setup as above, but candidate slot is 120 vs current tip slot
        // 110 (gap of 10, exceeds the Conway window of 5).
        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes,
            [0u8; 32],
            100,
            1,
            vec![0xAA; 32],
            0,
            vec![0xFF; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .unwrap();

        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes,
            common_bytes,
            110,
            2,
            vec![0xAA; 32],
            1,
            vec![0xFFu8; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .unwrap();

        // Candidate B at slot 120 (>5 from A's slot 110), block_no 2, lower VRF.
        let b_bytes = [0xB2u8; 32];
        let b_header = praos_header(
            b_bytes,
            common_bytes,
            120,
            2,
            vec![0xBB; 32],
            1,
            vec![0x00u8; 32],
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b_bytes),
                SlotNo(120),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b_bytes)),
                b_header,
            )
            .await
            .unwrap();

        assert_eq!(
            r,
            AddBlockResult::StoredAsFork,
            "expected StoredAsFork: slot gap {} > Conway window {}, RestrictedVRFTiebreaker \
             must keep current selection",
            10,
            5
        );
    }

    /// Sanity regression: strictly-greater block_no still triggers a switch
    /// when full headers are present. (The existing strict-greater test uses
    /// `submit_block`; this twin confirms the Praos path agrees.)
    #[tokio::test]
    async fn praos_tiebreaker_switches_on_strictly_greater_block_no() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());

        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let common_bytes = [0xC0u8; 32];
        let common_header = praos_header(
            common_bytes,
            [0u8; 32],
            100,
            1,
            vec![0xAA; 32],
            0,
            vec![0xFF; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(common_bytes),
                SlotNo(100),
                BlockNo(1),
                Hash32::ZERO,
                fake_cbor(&Hash32::from_bytes(common_bytes)),
                common_header,
            )
            .await
            .unwrap();

        let a_bytes = [0xA2u8; 32];
        let a_header = praos_header(
            a_bytes,
            common_bytes,
            110,
            2,
            vec![0xAA; 32],
            1,
            vec![0xFFu8; 32],
            9,
        );
        handle
            .submit_block_with_header(
                Hash32::from_bytes(a_bytes),
                SlotNo(110),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(a_bytes)),
                a_header,
            )
            .await
            .unwrap();

        // Sibling fork at block_no 2 with a VRF that is NOT lower than a's
        // (equal, so the praos tiebreaker says "no switch" — b2 stays as a
        // fork at the same height as the current tip).  Then extending to
        // block_no 3 (strictly greater) MUST trigger a switch under both
        // the legacy strict-greater rule and the Praos comparator path.
        let b2_bytes = [0xB2u8; 32];
        let b2_header = praos_header(
            b2_bytes,
            common_bytes,
            115,
            2,
            vec![0xBB; 32],
            1,
            vec![0xFFu8; 32], // equal to a's vrf — Praos tiebreaker = ShouldNotSwitch
            9,
        );
        let b2_result = handle
            .submit_block_with_header(
                Hash32::from_bytes(b2_bytes),
                SlotNo(115),
                BlockNo(2),
                Hash32::from_bytes(common_bytes),
                fake_cbor(&Hash32::from_bytes(b2_bytes)),
                b2_header,
            )
            .await
            .unwrap();
        // Pin the intermediate state: b2 has equal block_no AND equal VRF to
        // `a`, so Praos's comparePraos returns ShouldNotSwitch EQ and b2 must
        // remain a fork. Asserting this prevents a future regression that
        // would adopt equal-VRF candidates from passing this test on the
        // b3 step alone.
        assert_eq!(
            b2_result,
            AddBlockResult::StoredAsFork,
            "b2 (equal block_no, equal VRF) must stay a fork — Praos tiebreaker = ShouldNotSwitch EQ; got {b2_result:?}",
        );

        let b3_bytes = [0xB3u8; 32];
        let b3_header = praos_header(
            b3_bytes,
            b2_bytes,
            117,
            3,
            vec![0xBB; 32],
            2,
            vec![0x88; 32],
            9,
        );
        let r = handle
            .submit_block_with_header(
                Hash32::from_bytes(b3_bytes),
                SlotNo(117),
                BlockNo(3),
                Hash32::from_bytes(b2_bytes),
                fake_cbor(&Hash32::from_bytes(b3_bytes)),
                b3_header,
            )
            .await
            .unwrap();

        match r {
            AddBlockResult::TriggeredFork { .. } => {} // OK
            other => panic!(
                "strictly greater block_no MUST trigger switch under both legacy and Praos rules, got: {other:?}"
            ),
        }
    }

    // -----------------------------------------------------------------------
    // LoE (trimToLoE) behaviour — Ouroboros Genesis
    // -----------------------------------------------------------------------

    fn h(b: u8) -> Hash32 {
        Hash32::from_bytes([b; 32])
    }

    /// Install an LoE on the ChainDB and return the publisher.
    async fn install_loe(
        chain_db: &Arc<RwLock<ChainDB>>,
        initial: dugite_consensus::loe::LoeState,
    ) -> Arc<arc_swap::ArcSwap<dugite_consensus::loe::LoeState>> {
        let handle = Arc::new(arc_swap::ArcSwap::from_pointee(initial));
        chain_db.write().await.set_loe_handle(handle.clone());
        handle
    }

    fn loe_fragment(
        anchor: Option<(u64, u8)>,
        entries: &[(u64, u8)],
        k: u64,
    ) -> dugite_consensus::loe::LoeState {
        use dugite_consensus::loe::{LoePoint, LoeState};
        LoeState::Fragment {
            anchor: anchor.map(|(slot, b)| LoePoint {
                slot,
                hash: [b; 32],
            }),
            entries: entries
                .iter()
                .map(|(slot, b)| LoePoint {
                    slot: *slot,
                    hash: [*b; 32],
                })
                .collect(),
            k,
        }
    }

    #[tokio::test]
    async fn loe_disabled_is_identity_with_praos() {
        // Publishing LoeState::Disabled must behave exactly like no handle.
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let _loe = install_loe(&chain_db, dugite_consensus::loe::LoeState::Disabled).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        for i in 1..=5u8 {
            let prev = if i == 1 { Hash32::ZERO } else { h(i - 1) };
            let r = handle
                .submit_block(
                    h(i),
                    SlotNo(i as u64 * 10),
                    BlockNo(i as u64),
                    prev,
                    vec![i],
                )
                .await
                .unwrap();
            assert!(
                matches!(r, AddBlockResult::AddedAsTip { .. }),
                "Disabled LoE must not defer extensions (block {i}): {r:?}"
            );
        }
    }

    #[tokio::test]
    async fn loe_defers_extension_beyond_k_then_adopts_on_advance() {
        // LoE = empty fragment at Origin, k = 2: blocks 1,2 adopt; block 3
        // is stored-not-adopted (deferred). When the LoE advances to cover
        // block 1, ReprocessLoE adopts block 3 (depth from new tip = 2 ≤ k).
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let loe = install_loe(&chain_db, loe_fragment(None, &[], 2)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        let r1 = handle
            .submit_block(h(1), SlotNo(10), BlockNo(1), Hash32::ZERO, vec![1])
            .await
            .unwrap();
        assert!(matches!(r1, AddBlockResult::AddedAsTip { .. }), "{r1:?}");
        let r2 = handle
            .submit_block(h(2), SlotNo(20), BlockNo(2), h(1), vec![2])
            .await
            .unwrap();
        assert!(matches!(r2, AddBlockResult::AddedAsTip { .. }), "{r2:?}");

        // Depth 3 past the LoE tip (Origin) → deferred.
        let r3 = handle
            .submit_block(h(3), SlotNo(30), BlockNo(3), h(2), vec![3])
            .await
            .unwrap();
        assert_eq!(
            r3,
            AddBlockResult::StoredAsFork,
            "extension beyond k past the LoE tip must be deferred"
        );
        // Selection unchanged.
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(2))
        );

        // LoE advances: tip = block 1 → block 3 is now depth 2 ≤ k.
        loe.store(Arc::new(loe_fragment(None, &[(10, 1)], 2)));
        let r = handle.reprocess_loe().await.unwrap();
        match r {
            AddBlockResult::TriggeredFork { apply, .. } => {
                assert_eq!(apply, vec![h(3)], "deferred block adopted on LoE advance");
            }
            other => panic!("expected TriggeredFork, got {other:?}"),
        }
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(3))
        );
    }

    #[tokio::test]
    async fn loe_rejects_fork_diverging_below_its_tip() {
        // Selection a1,a2 with LoE fragment [a1,a2] (the peers' common
        // prefix). A higher-block_no fork c2 child of a1 diverges from the
        // fragment BELOW its tip → candPrefix case → never adopted while the
        // LoE says peers agree on a2.
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let loe = install_loe(&chain_db, loe_fragment(None, &[(10, 1), (20, 2)], 10)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        handle
            .submit_block(h(1), SlotNo(10), BlockNo(1), Hash32::ZERO, vec![1])
            .await
            .unwrap();
        handle
            .submit_block(h(2), SlotNo(20), BlockNo(2), h(1), vec![2])
            .await
            .unwrap();

        // Competing fork: c3 (block_no 3!) child of a1 — longer than the
        // selection but diverging from the LoE fragment below its tip.
        let r = handle
            .submit_block(h(0xC3), SlotNo(25), BlockNo(3), h(1), vec![3])
            .await
            .unwrap();
        assert_eq!(r, AddBlockResult::StoredAsFork, "divergent fork deferred");
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(2)),
            "selection must hold while the LoE covers the honest chain"
        );

        // The peers switch: the LoE fragment now ends at a1 (the fork point).
        // The deferred fork (1 block past the new LoE tip) becomes adoptable.
        loe.store(Arc::new(loe_fragment(None, &[(10, 1)], 10)));
        let r = handle.reprocess_loe().await.unwrap();
        match r {
            AddBlockResult::TriggeredFork {
                apply, rollback, ..
            } => {
                assert_eq!(apply, vec![h(0xC3)]);
                assert_eq!(rollback, vec![h(2)], "a2 rolled back");
            }
            other => panic!("expected TriggeredFork, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn loe_trims_overlong_candidate_to_k_past_tip() {
        // k = 2, LoE tip = block 1. A 5-block chain is stored; selection may
        // only advance to block 3 (= LoE tip + k). After the LoE advances to
        // block 3, the rest follows.
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let loe = install_loe(&chain_db, loe_fragment(None, &[(10, 1)], 2)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // b1..b2 extend within k of the tip; b3 hits depth 2 (allowed);
        // b4, b5 deferred.
        for i in 1..=5u8 {
            let prev = if i == 1 { Hash32::ZERO } else { h(i - 1) };
            let _ = handle
                .submit_block(
                    h(i),
                    SlotNo(i as u64 * 10),
                    BlockNo(i as u64),
                    prev,
                    vec![i],
                )
                .await
                .unwrap();
        }
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(3)),
            "selection holds at LoE tip + k"
        );

        // LoE advances to block 3 → the stored suffix (b4, b5) is adoptable:
        // the candidate b5 is depth 2 ≤ k past the new tip.
        loe.store(Arc::new(loe_fragment(
            None,
            &[(10, 1), (20, 2), (30, 3)],
            2,
        )));
        let r = handle.reprocess_loe().await.unwrap();
        match r {
            AddBlockResult::TriggeredFork { apply, .. } => {
                assert_eq!(apply, vec![h(4), h(5)]);
            }
            other => panic!("expected TriggeredFork, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn loe_trims_candidate_arbitrarily_deep_past_tip() {
        // The bulk-sync stall shape (mainnet Byron, 2026-07-28): BlockFetch
        // accumulates a volatile candidate chain far deeper past the LoE tip
        // than k + fragment length, while selection is pinned at the tip.
        // Haskell's trimToLoE has no depth limit — the candidate must be
        // TRIMMED to LoE tip + k, never deferred outright, no matter how deep
        // it is. (A walk cap of k + members + 2 turned deep candidates into
        // Deferred, freezing selection until the LoE jumped forward.)
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let loe = install_loe(&chain_db, loe_fragment(None, &[(10, 1)], 2)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // 12-block chain: b2, b3 adopt (≤ k past LoE tip b1); b4..b12 stored
        // deferred, forming a candidate 9 blocks past the LoE tip — beyond
        // the old cap of k + members + 2 = 5.
        for i in 1..=12u8 {
            let prev = if i == 1 { Hash32::ZERO } else { h(i - 1) };
            let _ = handle
                .submit_block(
                    h(i),
                    SlotNo(i as u64 * 10),
                    BlockNo(i as u64),
                    prev,
                    vec![i],
                )
                .await
                .unwrap();
        }
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(3)),
            "selection holds at LoE tip + k"
        );

        // LoE advances to block 3. The candidate tip b12 is now 9 blocks
        // past the new LoE tip — still far beyond the old cap. Selection
        // must adopt exactly k more blocks (b4, b5), not defer.
        loe.store(Arc::new(loe_fragment(
            None,
            &[(10, 1), (20, 2), (30, 3)],
            2,
        )));
        let r = handle.reprocess_loe().await.unwrap();
        match r {
            AddBlockResult::TriggeredFork { apply, .. } => {
                assert_eq!(
                    apply,
                    vec![h(4), h(5)],
                    "deep candidate must be trimmed to LoE tip + k"
                );
            }
            other => panic!("expected TriggeredFork adopting the k-prefix, got {other:?}"),
        }
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, h_, _)| h_),
            Some(h(5))
        );
    }

    #[tokio::test]
    async fn self_forged_blocks_bypass_the_loe() {
        // A genesis-mode block producer: LoE empty@Origin, k=2 — a SYNCED
        // peer block beyond k defers, but the node's OWN forged blocks
        // extend unconditionally (the LoE constrains peer chains, not the
        // node's own production).
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let _loe = install_loe(&chain_db, loe_fragment(None, &[], 2)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);

        // Forge 6 blocks (well past k=2) — every one must be adopted as tip.
        for i in 1..=6u8 {
            let prev = if i == 1 { Hash32::ZERO } else { h(i - 1) };
            let r = handle
                .submit_self_forged_block_with_header(
                    h(i),
                    SlotNo(i as u64 * 10),
                    BlockNo(i as u64),
                    prev,
                    vec![i],
                    praos_header(
                        *h(i).as_bytes(),
                        if i == 1 {
                            [0u8; 32]
                        } else {
                            *h(i - 1).as_bytes()
                        },
                        i as u64 * 10,
                        i as u64,
                        vec![0xAA],
                        i as u64,
                        vec![i],
                        10,
                    ),
                )
                .await
                .unwrap();
            assert!(
                matches!(r, AddBlockResult::AddedAsTip { .. }),
                "self-forged block {i} must extend the chain past the LoE: {r:?}"
            );
        }
        assert_eq!(
            chain_db.read().await.get_tip_info().map(|(_, _, bn)| bn.0),
            Some(6),
            "forger advanced to block 6 despite k=2 LoE"
        );
    }

    #[tokio::test]
    async fn loe_reprocess_noop_when_nothing_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let chain_db = make_chain_db(dir.path());
        let _loe = install_loe(&chain_db, loe_fragment(None, &[], 10)).await;
        let (handle, runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        let _runner_task = tokio::spawn(runner);
        handle
            .submit_block(h(1), SlotNo(10), BlockNo(1), Hash32::ZERO, vec![1])
            .await
            .unwrap();
        let r = handle.reprocess_loe().await.unwrap();
        assert_eq!(r, AddBlockResult::StoredAsFork, "no-op reprocess");
    }
}
