//! Shared streaming helpers used by service implementations.
//!
//! Most utxorpc service methods that return `tonic::Streaming` follow the
//! same pattern: fan a [`broadcast::Receiver`] into a bounded
//! [`mpsc::Sender`] so slow clients can be detected and disconnected with
//! `Status::resource_exhausted` instead of back-pressuring the publisher.
//! This module factors the boilerplate out.

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;
use tracing::warn;

use crate::config::DEFAULT_STREAM_BUFFER;

/// Spawn a per-stream fan-out task that pulls items from `source` and
/// pushes them into a bounded `mpsc::channel(buffer)`. Returns the
/// receiving stream + the join handle for the spawned task.
///
/// Behaviour on receiver state:
///
/// * `Ok(item)` — forwarded with `try_send`. If the client buffer is full,
///   the stream is terminated with `Status::resource_exhausted` and a
///   warning is logged. The publisher is NOT blocked.
/// * `Err(broadcast::error::RecvError::Lagged)` — `service`/`method`
///   labels are logged at WARN and the stream is terminated with
///   `Status::resource_exhausted`.
/// * `Err(broadcast::error::RecvError::Closed)` — publisher dropped;
///   end the stream cleanly.
///
/// The forwarding closure `map` allows projecting the broadcast payload
/// into the wire protobuf type without an extra hop. Returning `None`
/// from `map` drops the item silently (use for pattern filtering).
pub fn spawn_broadcast_fan_out<S, T, F>(
    mut source: broadcast::Receiver<S>,
    buffer: usize,
    service: &'static str,
    method: &'static str,
    mut map: F,
) -> (ReceiverStream<Result<T, Status>>, JoinHandle<()>)
where
    S: Clone + Send + 'static,
    T: Send + 'static,
    F: FnMut(S) -> Option<T> + Send + 'static,
{
    let buf = if buffer == 0 {
        DEFAULT_STREAM_BUFFER
    } else {
        buffer
    };
    let (tx, rx) = mpsc::channel(buf);
    let handle = tokio::spawn(async move {
        loop {
            match source.recv().await {
                Ok(item) => {
                    if let Some(mapped) = map(item) {
                        if tx.try_send(Ok(mapped)).is_err() {
                            warn!(
                                service,
                                method,
                                "RPC stream: client too slow, dropping with RESOURCE_EXHAUSTED"
                            );
                            let _ = tx
                                .send(Err(Status::resource_exhausted("client stream buffer full")))
                                .await;
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        service,
                        method,
                        lagged = n,
                        "RPC stream: client lagged on broadcast, disconnecting with RESOURCE_EXHAUSTED"
                    );
                    let _ = tx
                        .send(Err(Status::resource_exhausted(format!(
                            "subscriber lagged by {n} events; reconnect and resync"
                        ))))
                        .await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Publisher dropped — end the stream cleanly.
                    break;
                }
            }
        }
    });

    (ReceiverStream::new(rx), handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forwards_items_until_publisher_closes() {
        let (tx, rx) = broadcast::channel::<u32>(8);
        let (stream, handle) = spawn_broadcast_fan_out(rx, 4, "test", "method", |n| Some(n * 2));

        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);

        let collected: Vec<_> = tokio_stream::StreamExt::collect::<Vec<_>>(stream).await;
        handle.await.unwrap();

        let values: Vec<u32> = collected.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(values, vec![2, 4]);
    }

    #[tokio::test]
    async fn lagged_subscriber_disconnects_with_resource_exhausted() {
        // Tiny capacity + many sends → guaranteed Lagged.
        let (tx, rx) = broadcast::channel::<u32>(2);
        // Send several items before the consumer drains anything.
        for i in 0..10 {
            let _ = tx.send(i);
        }
        let (stream, _) = spawn_broadcast_fan_out(rx, 4, "test", "method", Some);

        let collected: Vec<_> = tokio_stream::StreamExt::collect::<Vec<_>>(stream).await;
        // Last item should be the resource-exhausted error.
        let last = collected.last().expect("at least one item");
        assert!(last.is_err(), "expected resource-exhausted: {collected:?}");
        assert_eq!(
            last.as_ref().err().unwrap().code(),
            tonic::Code::ResourceExhausted
        );
    }

    #[tokio::test]
    async fn map_returning_none_filters_silently() {
        let (tx, rx) = broadcast::channel::<u32>(8);
        let (stream, _) =
            spawn_broadcast_fan_out(rx, 4, "test", "method", |n| (n % 2 == 0).then_some(n));

        for i in 0..5 {
            tx.send(i).unwrap();
        }
        drop(tx);

        let collected: Vec<_> = tokio_stream::StreamExt::collect::<Vec<_>>(stream).await;
        let values: Vec<u32> = collected.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(values, vec![0, 2, 4]);
    }
}
