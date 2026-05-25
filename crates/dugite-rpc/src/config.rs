//! Static configuration for the RPC server.
//!
//! Constructed by `dugite-node` from `NodeConfig.Rpc` (JSON) + CLI flags and
//! passed to [`RpcServer::start`](crate::server::RpcServer::start). This crate
//! never reads files directly — the host owns config parsing.

use std::net::IpAddr;
use std::path::PathBuf;

/// Default gRPC listen port — matches the de-facto utxorpc convention
/// used by Dolos, Demeter, and others. Override per-deployment via JSON
/// `Rpc.Port` or `--rpc-port`.
pub const DEFAULT_RPC_PORT: u16 = 50051;

/// Default per-stream buffer size (events). A slow client whose receiver
/// falls behind by more than this drops with `RESOURCE_EXHAUSTED`.
pub const DEFAULT_STREAM_BUFFER: usize = 256;

/// Default cap on the number of concurrent streams a single connection
/// can drive. Mirrors tonic's HTTP/2 default safety floor.
pub const DEFAULT_MAX_CONCURRENT_STREAMS: u32 = 64;

/// Top-level RPC server configuration.
///
/// All fields have explicit defaults via [`Default`] so callers only set
/// what they care about. `bind` + `port` are mandatory in practice but
/// [`Default`] supplies `127.0.0.1:50051` for tests / dev.
#[derive(Debug, Clone)]
pub struct RpcConfig {
    /// IP address to bind. `127.0.0.1` for loopback-only (the safe default
    /// for an unauthenticated TCP gRPC endpoint).
    pub bind: IpAddr,
    /// TCP port to listen on.
    pub port: u16,
    /// Per-stream buffer size (events). Slow consumers exceeding this are
    /// dropped with `RESOURCE_EXHAUSTED`. See [`DEFAULT_STREAM_BUFFER`].
    pub stream_buffer: usize,
    /// Maximum concurrent HTTP/2 streams per connection. See
    /// [`DEFAULT_MAX_CONCURRENT_STREAMS`].
    pub max_concurrent_streams: u32,
    /// Expose the gRPC reflection service (`grpc.reflection.v1.ServerReflection`)
    /// so `grpcurl -plaintext :port list` works without a schema bundle.
    pub reflection_enabled: bool,
    /// Accept gRPC-Web traffic (HTTP/1.1). Off by default — opt in for
    /// browser dApps; the HTTP/1.1 accept loop costs a small amount of
    /// per-connection bookkeeping when enabled.
    pub web_enabled: bool,
    /// Expose the `v1alpha` services in addition to `v1beta`. Enabled by
    /// default during the upstream-stabilisation cycle; operators can
    /// pre-disable to test that their clients have migrated.
    pub alpha_enabled: bool,
    /// Optional TLS termination — if `None`, plaintext gRPC. For mTLS or
    /// production deployments, prefer Envoy / a sidecar.
    pub tls: Option<RpcTlsConfig>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: DEFAULT_RPC_PORT,
            stream_buffer: DEFAULT_STREAM_BUFFER,
            max_concurrent_streams: DEFAULT_MAX_CONCURRENT_STREAMS,
            reflection_enabled: true,
            web_enabled: false,
            alpha_enabled: true,
            tls: None,
        }
    }
}

/// Optional TLS configuration. Plain PEM-encoded cert/key on disk; no
/// hot-reload in v1 — config changes require a node restart.
#[derive(Debug, Clone)]
pub struct RpcTlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}
