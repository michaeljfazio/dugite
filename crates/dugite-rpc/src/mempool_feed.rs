//! Mempool change-event feed.
//!
//! Unlike [`TipFeed`](crate::tip_feed::TipFeed), this is a direct
//! re-export wrapper around the `dugite-mempool` broadcast — there's no
//! payload mapping needed because `dugite-rpc` already depends on
//! `dugite-mempool` (the trait surface uses `TransactionHash` etc.
//! anyway), so the same `MempoolEvent` / `MempoolRemoveReason` types
//! flow end-to-end.
//!
//! The host hands the [`broadcast::Sender`] from `Mempool::tx_events()`
//! straight to [`MempoolFeed::new`] — no shim task required.

use tokio::sync::broadcast;

// Re-export the upstream types so consumers don't need a direct
// `dugite-mempool` dep just for the event enum.
pub use dugite_mempool::{MempoolEvent, MempoolRemoveReason};

/// Wraps the broadcast sender produced by `Mempool::tx_events()` so
/// service code can subscribe without holding a reference to the whole
/// [`dugite_mempool::Mempool`] type.
#[derive(Clone, Debug)]
pub struct MempoolFeed {
    sender: broadcast::Sender<MempoolEvent>,
}

impl MempoolFeed {
    pub fn new(sender: broadcast::Sender<MempoolEvent>) -> Self {
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MempoolEvent> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;

    #[tokio::test]
    async fn forwarded_event_reaches_subscriber() {
        let (tx, _) = broadcast::channel(8);
        let feed = MempoolFeed::new(tx.clone());
        let mut rx = feed.subscribe();

        let hash = Hash32::from_bytes([7u8; 32]);
        tx.send(MempoolEvent::Added {
            tx_hash: hash,
            raw_cbor: Some(vec![1, 2, 3]),
        })
        .unwrap();

        match rx.recv().await.unwrap() {
            MempoolEvent::Added { tx_hash, raw_cbor } => {
                assert_eq!(tx_hash, hash);
                assert_eq!(raw_cbor, Some(vec![1, 2, 3]));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
