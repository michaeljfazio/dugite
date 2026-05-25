//! Node-local broadcaster carrying payload-shaped tip events for external
//! consumers (the forthcoming UTxO RPC server — see issue #672, milestone M0).
//!
//! Today's `block_announcement_tx` / `rollback_announcement_tx` channels are
//! shaped for the N2N/N2C ChainSync servers and carry the
//! `dugite_network::BlockAnnouncement` / `RollbackAnnouncement` types defined
//! in the network crate. `TipBroadcaster` is a sibling channel pair with
//! richer payloads (`era`, distinct `Apply`/`Rollback` types) suitable for an
//! external RPC consumer that should not depend on `dugite-network`.
//!
//! In M0 the existing announcement channels remain authoritative for N2N/N2C;
//! `TipBroadcaster` is fanned-in additively from the same send sites so no
//! existing subscriber behaviour changes.
//!
//! Slow-consumer policy: receivers handle `RecvError::Lagged` themselves —
//! `TipBroadcaster` makes no attempt to disconnect them.

// The payload fields + subscribe accessors are intentionally unused until
// milestone M1 wires the `dugite-rpc` crate up as a subscriber. The
// announce_* fan-in sites in `node/mod.rs` + `node/sync.rs` exercise the
// senders; the rest of the surface is the public API for M1.
#![allow(dead_code)]

use dugite_primitives::Era;
use tokio::sync::broadcast;

/// Capacity of the tip-apply broadcast channel.
///
/// 1024 covers any realistic fork burst comfortably (Conway blocks arrive at
/// most once per 20s slot; capacity at this size will only ever be exhausted
/// by a subscriber that has stalled for several minutes).
const TIP_APPLY_CAP: usize = 1024;

/// Capacity of the tip-rollback broadcast channel.
const TIP_ROLLBACK_CAP: usize = 256;

/// A block was applied at the chain tip.
#[derive(Clone, Debug)]
pub struct TipApply {
    pub slot: u64,
    pub hash: [u8; 32],
    pub block_number: u64,
    pub era: Era,
}

/// The chain rolled back to (slot, hash). `slot == 0 && hash == [0; 32]` is
/// the origin sentinel, matching the convention used by
/// `dugite_network::RollbackAnnouncement`.
#[derive(Clone, Debug)]
pub struct TipRollback {
    pub slot: u64,
    pub hash: [u8; 32],
}

/// Pair of broadcast senders carrying payload-bearing tip events.
///
/// Cheap to clone (broadcast::Sender is internally Arc'd).
#[derive(Clone, Debug)]
pub struct TipBroadcaster {
    apply_tx: broadcast::Sender<TipApply>,
    rollback_tx: broadcast::Sender<TipRollback>,
}

impl TipBroadcaster {
    pub fn new() -> Self {
        let (apply_tx, _) = broadcast::channel(TIP_APPLY_CAP);
        let (rollback_tx, _) = broadcast::channel(TIP_ROLLBACK_CAP);
        Self {
            apply_tx,
            rollback_tx,
        }
    }

    /// Best-effort fan-out — a `send` error means no receivers are attached,
    /// which is the normal state when the RPC server is disabled.
    pub fn announce_apply(&self, ev: TipApply) {
        let _ = self.apply_tx.send(ev);
    }

    /// Best-effort fan-out — see [`announce_apply`].
    pub fn announce_rollback(&self, ev: TipRollback) {
        let _ = self.rollback_tx.send(ev);
    }

    pub fn subscribe_apply(&self) -> broadcast::Receiver<TipApply> {
        self.apply_tx.subscribe()
    }

    pub fn subscribe_rollback(&self) -> broadcast::Receiver<TipRollback> {
        self.rollback_tx.subscribe()
    }
}

impl Default for TipBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apply_and_rollback_round_trip() {
        let broadcaster = TipBroadcaster::new();
        let mut apply_rx = broadcaster.subscribe_apply();
        let mut rollback_rx = broadcaster.subscribe_rollback();

        broadcaster.announce_apply(TipApply {
            slot: 42,
            hash: [1u8; 32],
            block_number: 7,
            era: Era::Conway,
        });
        broadcaster.announce_rollback(TipRollback {
            slot: 40,
            hash: [2u8; 32],
        });

        let applied = apply_rx.recv().await.unwrap();
        assert_eq!(applied.slot, 42);
        assert_eq!(applied.block_number, 7);
        assert_eq!(applied.era, Era::Conway);

        let rolled_back = rollback_rx.recv().await.unwrap();
        assert_eq!(rolled_back.slot, 40);
        assert_eq!(rolled_back.hash, [2u8; 32]);
    }

    #[tokio::test]
    async fn no_subscribers_is_not_an_error() {
        let broadcaster = TipBroadcaster::new();
        // Should not panic / propagate any error.
        broadcaster.announce_apply(TipApply {
            slot: 1,
            hash: [0u8; 32],
            block_number: 1,
            era: Era::Conway,
        });
        broadcaster.announce_rollback(TipRollback {
            slot: 0,
            hash: [0u8; 32],
        });
    }
}
