//! `RpcServer` — single entry point that binds the listener, registers
//! every service, and runs the tonic server until cooperative shutdown.
//!
//! Called by the host (`dugite-node`) inside `Node::run()` after the
//! adapter + feeds are constructed. The future returned by
//! [`RpcServer::start`] backgrounds the actual server task and yields an
//! [`RpcServerHandle`] the host stores for graceful shutdown.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Identity, Server as TonicServer, ServerTlsConfig};
use tracing::{error, info, warn};

use crate::config::RpcConfig;
use crate::context::LedgerContext;
use crate::mempool_feed::MempoolFeed;
use crate::metrics::SharedMetrics;
use crate::proto;
use crate::services::{
    query::{QuerySvcAlpha, QuerySvcBeta},
    submit::{SubmitSvcAlpha, SubmitSvcBeta},
    sync::{SyncSvcAlpha, SyncSvcBeta},
    watch::{WatchSvcAlpha, WatchSvcBeta},
    ServiceState,
};
use crate::tip_feed::TipFeed;

/// Handle returned by [`RpcServer::start`]. Dropping it does NOT stop the
/// server — the host triggers shutdown via the `shutdown_rx` it passes
/// in. The handle exists so the host can `await` the join handle from a
/// graceful-shutdown path.
pub struct RpcServerHandle {
    /// The actual bound address (may differ from config if port 0 was
    /// requested for tests).
    pub local_addr: SocketAddr,
    pub join: JoinHandle<Result<(), tonic::transport::Error>>,
}

/// Build + spawn the RPC server.
///
/// Returns once the listener is bound + the background task is running.
/// The caller signals shutdown by sending `true` on `shutdown_rx`'s
/// upstream; the server then drains in-flight RPCs cooperatively.
///
/// `config.bind`/`config.port` control the listener. Use `0` for port to
/// have the OS assign one (the returned [`RpcServerHandle::local_addr`]
/// reflects the actual bound port).
pub struct RpcServer;

impl RpcServer {
    pub async fn start(
        config: Arc<RpcConfig>,
        context: Arc<dyn LedgerContext>,
        tip_feed: TipFeed,
        mempool_feed: MempoolFeed,
        metrics: SharedMetrics,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<RpcServerHandle, std::io::Error> {
        // Eagerly read TLS material so any I/O failure surfaces here (as
        // io::Error) instead of inside the spawned tonic task where it
        // would have to be wrapped as a transport::Error.
        let tls_identity = match config.tls.as_ref() {
            Some(tls) => {
                let cert = std::fs::read(&tls.cert_path).map_err(|e| {
                    std::io::Error::other(format!(
                        "RPC TLS cert read failed ({}): {e}",
                        tls.cert_path.display()
                    ))
                })?;
                let key = std::fs::read(&tls.key_path).map_err(|e| {
                    std::io::Error::other(format!(
                        "RPC TLS key read failed ({}): {e}",
                        tls.key_path.display()
                    ))
                })?;
                Some(Identity::from_pem(cert, key))
            }
            None => None,
        };

        let addr = SocketAddr::new(config.bind, config.port);
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let incoming = TcpListenerStream::new(listener);

        info!(
            local_addr = %local_addr,
            reflection = config.reflection_enabled,
            web = config.web_enabled,
            alpha = config.alpha_enabled,
            tls = config.tls.is_some(),
            "dugite-rpc: gRPC server listening",
        );

        let state = ServiceState {
            context,
            tip_feed,
            mempool_feed,
            metrics,
            config: config.clone(),
        };

        let join = tokio::spawn(async move {
            let result = run_server(state, config, tls_identity, incoming, async move {
                let _ = shutdown_rx.changed().await;
            })
            .await;
            if let Err(ref e) = result {
                error!(error = %e, "dugite-rpc: server task exited with error");
            } else {
                info!("dugite-rpc: server task exited cleanly");
            }
            result
        });

        Ok(RpcServerHandle { local_addr, join })
    }
}

async fn run_server(
    state: ServiceState,
    config: Arc<RpcConfig>,
    tls_identity: Option<Identity>,
    incoming: TcpListenerStream,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), tonic::transport::Error> {
    // Service wrappers — one Arc-shared backing state, two interface
    // implementations per service (v1alpha + v1beta).
    let sync_alpha = SyncSvcAlpha::new(state.clone());
    let sync_beta = SyncSvcBeta::new(state.clone());
    let query_alpha = QuerySvcAlpha::new(state.clone());
    let query_beta = QuerySvcBeta::new(state.clone());
    let submit_alpha = SubmitSvcAlpha::new(state.clone());
    let submit_beta = SubmitSvcBeta::new(state.clone());
    let watch_alpha = WatchSvcAlpha::new(state.clone());
    let watch_beta = WatchSvcBeta::new(state.clone());

    let reflection = if config.reflection_enabled {
        // Reflection builder errors are descriptor-decode failures —
        // they would mean the FILE_DESCRIPTOR_SET baked into the binary
        // is malformed, a programmer error, not a runtime config issue.
        // Panic so the bug surfaces at server-start, not as a wire-level
        // mystery later.
        let svc = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
            .build_v1()
            .expect("FILE_DESCRIPTOR_SET malformed — codegen drift");
        Some(svc)
    } else {
        None
    };

    let mut builder = TonicServer::builder();

    // Apply the HTTP/2 max-concurrent-streams cap (0 = unlimited). Previously
    // `MaxConcurrentStreams` flowed into RpcConfig but was never applied to the
    // tonic server, so the limit was silently ignored.
    if config.max_concurrent_streams > 0 {
        builder = builder.max_concurrent_streams(Some(config.max_concurrent_streams));
    }

    if let Some(identity) = tls_identity {
        builder = builder.tls_config(ServerTlsConfig::new().identity(identity))?;
    }

    if config.web_enabled {
        builder = builder.accept_http1(true);
    }

    let mut router = builder
        .add_service(proto::v1beta::sync::sync_service_server::SyncServiceServer::new(sync_beta))
        .add_service(
            proto::v1beta::query::query_service_server::QueryServiceServer::new(query_beta),
        )
        .add_service(
            proto::v1beta::submit::submit_service_server::SubmitServiceServer::new(submit_beta),
        )
        .add_service(
            proto::v1beta::watch::watch_service_server::WatchServiceServer::new(watch_beta),
        );

    if config.alpha_enabled {
        router = router
            .add_service(
                proto::v1alpha::sync::sync_service_server::SyncServiceServer::new(sync_alpha),
            )
            .add_service(
                proto::v1alpha::query::query_service_server::QueryServiceServer::new(query_alpha),
            )
            .add_service(
                proto::v1alpha::submit::submit_service_server::SubmitServiceServer::new(
                    submit_alpha,
                ),
            )
            .add_service(
                proto::v1alpha::watch::watch_service_server::WatchServiceServer::new(watch_alpha),
            );
    } else {
        warn!("dugite-rpc: v1alpha services disabled — only v1beta clients will reach this server");
    }

    if let Some(reflection) = reflection {
        router = router.add_service(reflection);
    }

    router
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await
}
