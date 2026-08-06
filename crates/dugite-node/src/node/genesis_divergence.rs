//! #1057: the on-disk marker that makes a RESTART recover a genesis-divergent node.
//!
//! A node whose chain diverges from the network's at GENESIS cannot rejoin in
//! place. BlockFetch's #735 gross-request invariant declines every range rooted at
//! genesis (genesis is never a *stored* block), so the ledger never advances,
//! ChainSync's forecast-horizon park times out, and every peer is dropped and
//! re-dialled forever. It strands a block producer.
//!
//! **Restarting alone does not help, and that is measured, not assumed.** On a
//! two-forger devnet, deleting `ledger-snapshot.bin` and restarting left the node
//! at its own fork's tip within twelve seconds — `run()` replays the existing
//! ChainDB and re-applies its own chain before any peer connects — and it still did
//! not adopt the peer's 246-block chain. So a fix has to discard the dead-end
//! chain, not merely the ledger derived from it.
//!
//! This marker is how that happens WITHOUT a live in-place ledger
//! re-initialisation. Everything destructive runs at startup, on the path that
//! already exists and is already validated:
//!
//! * the ledger snapshot is treated as unusable, so `Node::new` builds a fresh
//!   genesis ledger via `init_fresh_ledger`;
//! * `wipe_utxo_store_before_replay` fires, which the LSM block already honours —
//!   and that wipe is load-bearing, not hygiene: a stale store roughly doubles
//!   `sumCoinUTxO` at the Byron→Shelley boundary, drives the reserves recompute to
//!   0, and underflows the first MIR debit into a panic;
//! * the VolatileDB is cleared (WAL truncated), so the dead-end fork is gone and
//!   startup replay has nothing to re-apply, leaving the ledger at Origin.
//!
//! The node then syncs from genesis down the ordinary, well-trodden path.
//!
//! ## Why this is a marker and not an automatic self-restart
//!
//! The node does not exit itself. Writing the marker plus the actionable ERROR
//! makes "restart the node" sufficient, where today it is not — but the decision
//! to restart stays with the operator or the supervisor.
//!
//! That matters because the trigger is a *peer's claim*: it is reached when peers
//! offer chains rooted at genesis. Turning that into an automatic
//! discard-my-chain-and-resync would be a remotely-triggerable forced resync, and
//! dugite-node is adversarial-deployment software.
//!
//! ## Two bounds, both deliberate
//!
//! **The ImmutableDB must be empty.** A rollback past the immutable tip is
//! protocol-impossible under Ouroboros k-finality, so a node that has flushed
//! anything must NOT act on this marker — it is refused and cleared instead. This
//! also bounds the blast radius hard: only a node whose entire chain is still
//! volatile (< k blocks) can be reset at all, and such a node loses seconds of
//! sync, not history.
//!
//! **Attempts are counted.** A node pointed at the WRONG genesis file would
//! diverge, reset, resync, diverge again — an endless resync loop. After
//! [`MAX_RESET_ATTEMPTS`] the marker is refused and the operator is told the
//! genesis configuration is the likely cause, which is the actual defect in that
//! scenario.

use std::io;
use std::path::{Path, PathBuf};

use tracing::{error, info, warn};

/// Refuse to reset more than this many times. A recurring genesis divergence is a
/// configuration error (wrong genesis files), not something to keep resyncing
/// through.
pub(crate) const MAX_RESET_ATTEMPTS: u32 = 3;

const MARKER_FILE: &str = "genesis-divergence-detected";

fn marker_path(db_path: &Path) -> PathBuf {
    db_path.join(MARKER_FILE)
}

/// Record that this node cannot rejoin because its chain diverges at genesis.
///
/// Idempotent per restart in effect: the attempt counter is incremented so a
/// repeat-offending configuration is caught rather than looped on. Returns the new
/// attempt count.
pub(crate) fn record(db_path: &Path, peer: &str, peer_tip_slot: u64) -> io::Result<u32> {
    let path = marker_path(db_path);
    let attempts = read_attempts(db_path).unwrap_or(0) + 1;
    // Plain text on purpose: an operator finding this file should be able to read
    // it without tooling, and it is also the audit trail for why a database was
    // reset.
    let body = format!(
        "attempts={attempts}\n\
         peer={peer}\n\
         peer_tip_slot={peer_tip_slot}\n\
         reason=this node's chain diverges from the network's at GENESIS; BlockFetch \
         declines every range rooted at genesis, so the ledger cannot advance (#1057)\n\
         action=on the next start, dugite discards the local chain and re-syncs from \
         genesis (only while the ImmutableDB is empty)\n"
    );
    std::fs::write(&path, body)?;
    Ok(attempts)
}

/// The recorded attempt count, or `None` when no marker is present.
pub(crate) fn read_attempts(db_path: &Path) -> Option<u32> {
    let body = std::fs::read_to_string(marker_path(db_path)).ok()?;
    body.lines()
        .find_map(|l| l.strip_prefix("attempts="))
        .and_then(|v| v.trim().parse().ok())
}

/// Remove the marker. Called once the reset has been performed, and also when the
/// marker is refused, so a stale file cannot make every future start suspicious.
pub(crate) fn clear(db_path: &Path) {
    let path = marker_path(db_path);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(path = %path.display(), "failed to remove the #1057 marker: {e}");
        }
    }
}

/// What `Node::new` should do about a marker, decided from the two bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetDecision {
    /// No marker — ordinary start.
    None,
    /// Discard the local chain and rebuild from genesis.
    Reset { attempt: u32 },
    /// Marker present but must NOT be acted on. The marker is cleared and the
    /// reason logged; the node starts normally (and will re-detect the wedge if it
    /// is still real, which is the honest outcome).
    Refused(RefusalReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusalReason {
    /// Blocks have been flushed to the ImmutableDB, so rolling back to Origin is
    /// protocol-impossible (Ouroboros k-finality).
    ImmutableNotEmpty,
    /// Too many resets — the genesis configuration is the likely defect.
    TooManyAttempts { attempts: u32 },
}

/// Decide, from the marker and the ImmutableDB state, whether to reset.
///
/// Pure so the two bounds are unit-testable without a filesystem or a node:
/// getting either wrong is the difference between a bounded recovery and either an
/// unbounded resync loop or a node that discards flushed history.
pub(crate) fn decide(attempts: Option<u32>, immutable_is_empty: bool) -> ResetDecision {
    match attempts {
        None => ResetDecision::None,
        Some(attempts) if !immutable_is_empty => {
            let _ = attempts;
            ResetDecision::Refused(RefusalReason::ImmutableNotEmpty)
        }
        Some(attempts) if attempts > MAX_RESET_ATTEMPTS => {
            ResetDecision::Refused(RefusalReason::TooManyAttempts { attempts })
        }
        Some(attempt) => ResetDecision::Reset { attempt },
    }
}

/// Log the decision. Separated from [`decide`] so the decision stays pure and the
/// wording lives in one place.
pub(crate) fn log_decision(decision: ResetDecision, db_path: &Path) {
    match decision {
        ResetDecision::None => {}
        ResetDecision::Reset { attempt } => {
            warn!(
                attempt,
                max_attempts = MAX_RESET_ATTEMPTS,
                db = %db_path.display(),
                "#1057 recovery: this node previously could not rejoin because its chain \
                 diverges from the network's at GENESIS. Discarding the local chain \
                 (VolatileDB + ledger snapshot + UTxO store) and re-syncing from genesis. \
                 The ImmutableDB is empty, so no finalised history is being discarded."
            );
        }
        ResetDecision::Refused(RefusalReason::ImmutableNotEmpty) => {
            error!(
                db = %db_path.display(),
                "#1057 marker present but REFUSED: blocks have been flushed to the \
                 ImmutableDB, so a rollback to Origin is protocol-impossible under \
                 Ouroboros k-finality and dugite will not discard finalised history. If \
                 this node genuinely needs to re-sync, that is an operator decision: stop \
                 it, remove the database directory, and re-sync or restore a Mithril \
                 snapshot."
            );
        }
        ResetDecision::Refused(RefusalReason::TooManyAttempts { attempts }) => {
            error!(
                attempts,
                max_attempts = MAX_RESET_ATTEMPTS,
                db = %db_path.display(),
                "#1057 marker present but REFUSED: already reset {attempts} times. A \
                 genesis divergence that recurs after a full re-sync is almost certainly \
                 a CONFIGURATION error — check that the genesis files (and their hashes) \
                 match the network this node is meant to join. Repeatedly re-syncing would \
                 hide that, not fix it."
            );
        }
    }
}

/// Announce that the reset completed, with what was discarded.
pub(crate) fn log_reset_performed(volatile_blocks_cleared: usize) {
    info!(
        volatile_blocks_cleared,
        "#1057 recovery: local chain discarded; the ledger starts at Origin and the \
         node will sync the network's chain from genesis"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_marker_means_ordinary_start() {
        assert_eq!(decide(None, true), ResetDecision::None);
        assert_eq!(decide(None, false), ResetDecision::None);
    }

    /// The ImmutableDB bound is checked BEFORE the attempt bound on purpose: a node
    /// with flushed history must never be reset, however few attempts it has made.
    #[test]
    fn flushed_history_is_never_discarded() {
        assert_eq!(
            decide(Some(1), false),
            ResetDecision::Refused(RefusalReason::ImmutableNotEmpty)
        );
        assert_eq!(
            decide(Some(MAX_RESET_ATTEMPTS + 5), false),
            ResetDecision::Refused(RefusalReason::ImmutableNotEmpty),
            "immutable-not-empty must win over the attempt count — discarding \
             finalised history is the worse outcome"
        );
    }

    /// A node pointed at the WRONG genesis would diverge → reset → resync →
    /// diverge, forever. The counter bounds that and blames the right thing.
    #[test]
    fn repeated_resets_are_refused_as_a_config_error() {
        for attempt in 1..=MAX_RESET_ATTEMPTS {
            assert_eq!(
                decide(Some(attempt), true),
                ResetDecision::Reset { attempt },
                "attempt {attempt} is within budget"
            );
        }
        assert_eq!(
            decide(Some(MAX_RESET_ATTEMPTS + 1), true),
            ResetDecision::Refused(RefusalReason::TooManyAttempts {
                attempts: MAX_RESET_ATTEMPTS + 1
            })
        );
    }

    #[test]
    fn marker_round_trips_and_counts_attempts() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path();
        assert_eq!(read_attempts(db), None, "no marker to begin with");

        assert_eq!(record(db, "127.0.0.1:3001", 342).unwrap(), 1);
        assert_eq!(read_attempts(db), Some(1));

        // A second detection on a later run increments rather than resetting, which
        // is what makes the config-error bound work at all.
        assert_eq!(record(db, "127.0.0.1:3001", 900).unwrap(), 2);
        assert_eq!(read_attempts(db), Some(2));

        clear(db);
        assert_eq!(
            read_attempts(db),
            None,
            "cleared after the reset is performed"
        );
        // Clearing twice must not panic or complain.
        clear(db);
    }

    /// The file is plain text an operator can read without tooling, and it records
    /// WHY the database was reset — it doubles as the audit trail.
    #[test]
    fn marker_body_is_human_readable_and_names_the_issue() {
        let tmp = tempfile::tempdir().unwrap();
        record(tmp.path(), "10.0.0.5:3001", 1234).unwrap();
        let body = std::fs::read_to_string(tmp.path().join(MARKER_FILE)).unwrap();
        assert!(body.contains("attempts=1"));
        assert!(body.contains("peer=10.0.0.5:3001"));
        assert!(body.contains("peer_tip_slot=1234"));
        assert!(body.contains("#1057"), "must name the issue: {body}");
        assert!(
            body.contains("GENESIS"),
            "must name the cause, not just the effect: {body}"
        );
    }
}
