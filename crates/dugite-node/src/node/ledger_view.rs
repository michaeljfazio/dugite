//! Lock-free read-only view of stable ledger state — issue #651 Phase 2
//! and issue #652 Phase 0.
//!
//! # Why
//!
//! Today every reader of ledger state (N2C queries, Prometheus metrics,
//! `dugite-monitor`, header validation, GSM checks, peer-discovery
//! refresh) acquires `node.ledger_state.read().await`. The same `RwLock`
//! is acquired in write mode by the apply path (block apply, rollback,
//! epoch transition) for multi-second windows during bulk sync and at
//! epoch boundaries. Every reader parks for the duration — that's the
//! "one tokio worker pegged while peers idle" symptom from #651, and the
//! "N readers stalled behind one writer" symptom from #652 P0.
//!
//! # How
//!
//! After every successful ledger apply (block, rollback, epoch transition)
//! the apply path constructs a fresh `LedgerView` capturing the fields
//! readers commonly need, and atomically swaps it into an
//! `Arc<ArcSwap<LedgerView>>` field on `Node`. Readers call
//! [`Node::view`] (added in `node/mod.rs`), which loads the pointer in a
//! single relaxed atomic load — no lock, no `.await`. The `Arc` extends
//! the view's lifetime past the next swap so no reader sees a torn
//! state.
//!
//! # Staleness contract
//!
//! Readers may observe a view that is up to **one apply step** older than
//! the live `LedgerState`. This matches Haskell's design: N2C queries
//! land in the consensus thread queue and observe whatever state exists
//! when dequeued — never strictly the head, always close to it.
//!
//! **Strict readers** that need exact state (forge VRF leader check at
//! the precise tip; mempool revalidation against the new tip's UTxO
//! set) must continue to acquire `node.ledger_state.read().await`.
//! Those are surgical, infrequent, and not on the contention path.
//!
//! # Cost of publishing
//!
//! Every field on `LedgerView` is either:
//! - a `Copy` primitive (u64, Hash32),
//! - a small `Clone` value (ProtocolParameters ~few hundred bytes,
//!   StakeSnapshot has Arc-shared inner maps),
//! - an `Arc<...>` (pool_params, governance, epoch_blocks_by_pool,
//!   opcert_counters snapshot),
//! - an `imbl::HashMap` (delegations, reward_accounts) — O(1) structural
//!   clone via persistent HAMT structural sharing.
//!
//! Net cost of constructing one view ≈ ProtocolParameters clone + several
//! `Arc::clone` + two `imbl::HashMap::clone` (O(1)) + one
//! `EpochSnapshots::clone` (mostly Arc-clones). Sub-millisecond at any scale.

use std::collections::HashMap;
use std::sync::Arc;

use imbl::HashMap as ImblHashMap;

use dugite_ledger::state::substates::{ConsensusSubState, EpochSubState};
use dugite_ledger::LedgerState;
use dugite_primitives::block::Tip;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{EpochNo, SlotNo};
use dugite_primitives::transaction::Rational;
use dugite_primitives::value::Lovelace;

/// Lock-free read-only snapshot of stable ledger state, published by the
/// apply path after each successful ledger advance.
///
/// Cheaply cloned via `Arc<LedgerView>` — readers receive an `Arc` from
/// [`Node::view`] and hold it for as long as they need to inspect the
/// captured state.
///
/// `#[allow(dead_code)]` covers fields not yet consumed by call-site
/// migration. The fields are foundational for both the in-flight
/// `Node::view` adoption (issue #651 P2) and the eager-validation
/// integration in #652 P1.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LedgerView {
    // ── Chain head ────────────────────────────────────────────────────────
    /// Chain tip at the time of publication.
    pub tip: Tip,
    /// Current epoch.
    pub epoch: EpochNo,
    /// Current era — last value `LedgerState.era` held when the view was built.
    pub era: Era,
    /// Last applied slot (mirrors `tip.point.slot()`).
    pub last_applied_slot: Option<SlotNo>,

    // ── Protocol parameters ───────────────────────────────────────────────
    /// Current epoch's protocol parameters (Haskell `curPParams`).
    pub protocol_params: Arc<ProtocolParameters>,
    /// Previous epoch's protocol parameters (Haskell `prevPParams`).
    /// Captured at the previous epoch boundary; used by RUPD-class
    /// calculations that must observe the pre-transition parameters.
    pub prev_protocol_params: Arc<ProtocolParameters>,
    /// Protocol-major version captured at the previous epoch boundary.
    pub prev_protocol_version_major: u64,
    /// Decentralisation parameter captured at the previous epoch boundary,
    /// stored as exact `Rational` (issue #629).
    pub prev_d: Rational,

    // ── Stake / pools / governance (heavy maps — Arc-shared) ──────────────
    /// Pool registrations (current active map). Mirrors
    /// `LedgerState.certs.pool_params`. Shared via Arc.
    pub pool_params: Arc<HashMap<Hash28, dugite_ledger::state::PoolRegistration>>,
    /// Delegations: credential -> pool (current active map).
    /// `imbl::HashMap` so `from_state` is O(1) structural clone — no iterate+collect.
    pub delegations: ImblHashMap<Hash32, Hash28>,
    /// Reward accounts: credential -> accumulated rewards.
    /// `imbl::HashMap` so `from_state` is O(1) structural clone — no iterate+collect.
    pub reward_accounts: ImblHashMap<Hash32, Lovelace>,
    /// Epoch snapshots (mark / set / go). Used by reward calculations and
    /// by the eager-validation forecast path (issue #652) for the active
    /// stake distribution at a header's slot — the *set* snapshot is the
    /// one observed in-epoch.
    pub snapshots: Arc<dugite_ledger::state::EpochSnapshots>,
    /// Governance state (Conway+). Arc-shared.
    pub governance: Arc<dugite_ledger::state::GovernanceState>,

    // ── Treasury / reserves ───────────────────────────────────────────────
    /// Current treasury balance.
    pub treasury: Lovelace,
    /// Current reserves (non-circulating ADA).
    pub reserves: Lovelace,

    // ── Consensus nonces (header validation surface, #652) ────────────────
    /// Epoch nonce for the current epoch (Haskell `epoch_nonce`).
    pub epoch_nonce: Hash32,
    /// Candidate nonce accumulator across the current epoch's randomness
    /// stability window (Haskell `candidate_nonce`).
    pub candidate_nonce: Hash32,
    /// Last-applied-block VRF output (Haskell `evolving_nonce`).
    pub evolving_nonce: Hash32,
    /// Nonce of the last block of the previous epoch (Haskell
    /// `last_epoch_block_nonce`).
    pub last_epoch_block_nonce: Hash32,

    // ── Op-cert counters (snapshot of body-apply authoritative view) ──────
    /// Per-pool op-cert counter snapshot, captured from the live
    /// `OuroborosPraos.opcert_counters` at view-publish time. Eager
    /// per-peer validation (#652 Phase 1) does NOT mutate this — only
    /// the body-apply path does. Per-peer state evolves locally.
    pub opcert_counters: Arc<HashMap<Hash28, u64>>,

    // ── Stability / consensus-window constants ────────────────────────────
    /// Pre-Conway stability window (3k/f).
    pub stability_window_3kf: u64,
    /// Conway+ randomness stabilisation window.
    pub randomness_stabilisation_window: u64,
    /// `k` (Praos security parameter).
    pub security_param: u64,

    // ── Era history bits ──────────────────────────────────────────────────
    /// Number of Byron epochs before the Shelley hard fork.
    pub shelley_transition_epoch: u64,
    /// Byron epoch length in slots.
    pub byron_epoch_length: u64,
    /// Shelley+ epoch length in slots.
    pub epoch_length: u64,
    /// Slot configuration (POSIX time of slot 0, slot length).
    /// Used by metrics + N2C tip-age conversion.
    pub slot_config: dugite_ledger::plutus::SlotConfig,
}

impl LedgerView {
    /// Build a `LedgerView` from a borrowed `LedgerState`. Called from the
    /// apply path after each successful advance, just before publishing
    /// via `ArcSwap::store`.
    ///
    /// **Cost**: one `ProtocolParameters` clone, several `Arc::clone`, one
    /// `EpochSnapshots` clone (which itself is mostly Arc-clones). Designed
    /// to be cheap enough to publish on every block apply.
    pub fn from_state(ls: &LedgerState) -> Self {
        let consensus: &ConsensusSubState = &ls.consensus;
        let epochs: &EpochSubState = &ls.epochs;

        LedgerView {
            tip: ls.tip.clone(),
            epoch: ls.epoch,
            era: ls.era,
            last_applied_slot: ls.tip.point.slot(),
            protocol_params: Arc::new(epochs.protocol_params.clone()),
            prev_protocol_params: Arc::new(epochs.prev_protocol_params.clone()),
            prev_protocol_version_major: epochs.prev_protocol_version_major,
            prev_d: epochs.prev_d.clone(),
            pool_params: Arc::clone(&ls.certs.pool_params),
            // O(1) imbl structural clone — no iterate+collect; see field docs.
            delegations: ls.certs.delegations.clone(),
            reward_accounts: ls.certs.reward_accounts.clone(),
            snapshots: Arc::new(epochs.snapshots.clone()),
            governance: Arc::clone(&ls.gov.governance),
            treasury: epochs.treasury,
            reserves: epochs.reserves,
            epoch_nonce: consensus.epoch_nonce,
            candidate_nonce: consensus.candidate_nonce,
            evolving_nonce: consensus.evolving_nonce,
            last_epoch_block_nonce: consensus.last_epoch_block_nonce,
            opcert_counters: Arc::new(consensus.opcert_counters.clone()),
            stability_window_3kf: ls.stability_window_3kf,
            randomness_stabilisation_window: ls.randomness_stabilisation_window,
            security_param: ls.security_param,
            shelley_transition_epoch: ls.shelley_transition_epoch,
            byron_epoch_length: ls.byron_epoch_length,
            epoch_length: ls.epoch_length,
            slot_config: ls.slot_config,
        }
    }

    /// Convenience: current chain tip slot, or 0 at origin.
    #[allow(dead_code)] // wired by call-site migration follow-ups
    pub fn tip_slot(&self) -> u64 {
        self.last_applied_slot.map(|s| s.0).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::protocol_params::ProtocolParameters;

    /// `LedgerView::from_state` captures the live fields verbatim.
    #[test]
    fn test_from_state_captures_epoch_and_tip() {
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        state.epoch = EpochNo(11);
        state.epochs.treasury = Lovelace(42);
        state.era = Era::Conway;

        let view = LedgerView::from_state(&state);

        assert_eq!(view.epoch, EpochNo(11));
        assert_eq!(view.treasury, Lovelace(42));
        assert_eq!(view.era, Era::Conway);
        assert_eq!(view.tip_slot(), 0); // origin
    }

    /// `LedgerView` is cheaply clonable via `Arc`. Field equality should
    /// roundtrip through publish/load.
    #[test]
    fn test_view_clone_roundtrip() {
        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let view = LedgerView::from_state(&state);
        let arc = Arc::new(view.clone());
        assert_eq!(view.epoch, arc.epoch);
        assert_eq!(view.era, arc.era);
        // Arc::clone is what publishes use; verify no panic / no copy:
        let loaded: Arc<LedgerView> = Arc::clone(&arc);
        assert_eq!(loaded.epoch, view.epoch);
    }

    /// Publish via ArcSwap, then load from another thread — verify the
    /// loaded view is the published one and the load is non-blocking.
    #[test]
    fn test_arc_swap_publish_and_load_lock_free() {
        use arc_swap::ArcSwap;
        use std::sync::atomic::{AtomicBool, Ordering};

        let state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let initial = LedgerView::from_state(&state);
        let swap: Arc<ArcSwap<LedgerView>> = Arc::new(ArcSwap::from_pointee(initial));

        // Publish an updated view from a writer thread.
        let writer_swap = Arc::clone(&swap);
        let writer = std::thread::spawn(move || {
            let mut s = LedgerState::new(ProtocolParameters::mainnet_defaults());
            s.epoch = EpochNo(99);
            s.epochs.treasury = Lovelace(123_000);
            writer_swap.store(Arc::new(LedgerView::from_state(&s)));
        });

        // Reader thread polls the load until it sees the updated view.
        let reader_swap = Arc::clone(&swap);
        let saw_update = Arc::new(AtomicBool::new(false));
        let saw_update_clone = Arc::clone(&saw_update);
        let reader = std::thread::spawn(move || {
            for _ in 0..1_000 {
                let view = reader_swap.load();
                if view.epoch == EpochNo(99) && view.treasury == Lovelace(123_000) {
                    saw_update_clone.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });

        writer.join().expect("writer thread should not panic");
        reader.join().expect("reader thread should not panic");
        assert!(
            saw_update.load(Ordering::Acquire),
            "reader must observe the published view"
        );
    }
}
