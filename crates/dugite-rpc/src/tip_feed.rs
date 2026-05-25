//! Tip-event broadcaster owned by the RPC server.
//!
//! Mirrors the shape of `node::tip_broadcast::TipBroadcaster` (M0.1) but
//! lives in this crate so service code can subscribe without a
//! `dugite-node` dependency. The host (`NodeRpcAdapter`) bridges the two:
//! a shim task subscribes to the node-side broadcaster and republishes
//! into the dugite-rpc-side broadcaster, mapping payload types in the
//! process.
//!
//! Capacities mirror the node-side defaults so back-pressure semantics
//! match: a service stream that lags by more than `cap_apply` events
//! receives `RecvError::Lagged` and the surrounding service handler is
//! expected to drop the client with `Status::resource_exhausted`.

use tokio::sync::broadcast;

use crate::context::TipInfo;

/// Default capacity for the apply channel — matches node-side
/// `tip_broadcast::TIP_APPLY_CAP` so the back-pressure point is consistent
/// across the publish hop.
pub const DEFAULT_TIP_APPLY_CAP: usize = 1024;

/// Default capacity for the rollback channel — matches node-side
/// `tip_broadcast::TIP_ROLLBACK_CAP`.
pub const DEFAULT_TIP_ROLLBACK_CAP: usize = 256;

/// A chain-rollback event — same shape as
/// `node::tip_broadcast::TipRollback` so the publisher can forward
/// without re-shaping (only the type name differs).
#[derive(Clone, Debug)]
pub struct TipRollback {
    pub slot: u64,
    pub hash: [u8; 32],
}

/// Owns a pair of broadcast senders carrying tip-apply + tip-rollback
/// events. Cheap to clone (the inner [`broadcast::Sender`]s are Arc'd).
///
/// Constructed by the host once at RPC server startup, passed to
/// [`RpcServer::start`](crate::server::RpcServer::start), and shared with
/// the host's shim task via [`TipFeed::publisher`].
#[derive(Clone, Debug)]
pub struct TipFeed {
    apply_tx: broadcast::Sender<TipInfo>,
    rollback_tx: broadcast::Sender<TipRollback>,
}

impl TipFeed {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TIP_APPLY_CAP, DEFAULT_TIP_ROLLBACK_CAP)
    }

    pub fn with_capacity(cap_apply: usize, cap_rollback: usize) -> Self {
        Self {
            apply_tx: broadcast::channel(cap_apply).0,
            rollback_tx: broadcast::channel(cap_rollback).0,
        }
    }

    /// Hand out a publisher — used by the host shim task to forward
    /// events from the node-side broadcaster.
    pub fn publisher(&self) -> TipPublisher {
        TipPublisher {
            apply_tx: self.apply_tx.clone(),
            rollback_tx: self.rollback_tx.clone(),
        }
    }

    pub fn subscribe_apply(&self) -> broadcast::Receiver<TipInfo> {
        self.apply_tx.subscribe()
    }

    pub fn subscribe_rollback(&self) -> broadcast::Receiver<TipRollback> {
        self.rollback_tx.subscribe()
    }
}

impl Default for TipFeed {
    fn default() -> Self {
        Self::new()
    }
}

/// Publisher half of a [`TipFeed`]. Held by the host's shim task; calls
/// are best-effort — a `send` error means no receivers are attached,
/// which is normal when no RPC client is actively streaming tip events.
#[derive(Clone, Debug)]
pub struct TipPublisher {
    apply_tx: broadcast::Sender<TipInfo>,
    rollback_tx: broadcast::Sender<TipRollback>,
}

impl TipPublisher {
    pub fn announce_apply(&self, ev: TipInfo) {
        let _ = self.apply_tx.send(ev);
    }

    pub fn announce_rollback(&self, ev: TipRollback) {
        let _ = self.rollback_tx.send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::Era;

    #[tokio::test]
    async fn publisher_to_subscriber_round_trip() {
        let feed = TipFeed::new();
        let mut apply_rx = feed.subscribe_apply();
        let mut rollback_rx = feed.subscribe_rollback();
        let pubr = feed.publisher();

        pubr.announce_apply(TipInfo {
            slot: 100,
            hash: [1u8; 32],
            block_number: 5,
            era: Era::Conway,
        });
        pubr.announce_rollback(TipRollback {
            slot: 90,
            hash: [2u8; 32],
        });

        let applied = apply_rx.recv().await.unwrap();
        assert_eq!(applied.slot, 100);
        let rolled = rollback_rx.recv().await.unwrap();
        assert_eq!(rolled.slot, 90);
    }

    #[tokio::test]
    async fn no_subscribers_is_not_an_error() {
        let feed = TipFeed::with_capacity(4, 4);
        let pubr = feed.publisher();
        pubr.announce_apply(TipInfo {
            slot: 1,
            hash: [0u8; 32],
            block_number: 1,
            era: Era::Conway,
        });
        pubr.announce_rollback(TipRollback {
            slot: 0,
            hash: [0u8; 32],
        });
    }
}
