//! Native UTxO RPC (gRPC) server for `dugite-node` — issue #672.
//!
//! This crate is the sole consumer of `tonic` / `prost` in the workspace.
//! `dugite-node` depends on `dugite-rpc` as an opaque library and never sees
//! the gRPC stack in its direct compile graph, keeping the node binary
//! small when the RPC server is disabled (the default).
//!
//! Ships:
//!
//! * Vendored proto from `utxorpc/spec` (pinned tag in `proto/VERSION`) +
//!   tonic-build codegen.
//! * `RpcServer::start` with gRPC reflection + gRPC-Web + TLS + cooperative
//!   shutdown.
//! * `LedgerContext` async trait abstraction over the node — no
//!   `dugite-node` symbols leak into this crate's public API.
//! * `SyncService` / `QueryService` / `SubmitService` / `WatchService`,
//!   both `v1alpha` and `v1beta`, implemented end-to-end — see
//!   `docs/src/running/utxo-rpc.md` for the per-method status table and
//!   documented (not silently missing) limitations.
//!
//! Spec-bump workflow: `just bump-utxorpc-spec <tag>` rewrites
//! `crates/dugite-rpc/proto/` from the upstream and updates the VERSION
//! file. Golden tests catch protobuf shape drift before merge.

#![deny(rust_2018_idioms)]

pub mod config;
pub mod context;
pub mod error;
pub mod map;
pub mod masking;
pub mod mempool_feed;
pub mod metrics;
pub mod proto;
pub mod server;
pub mod services;
pub mod stream;
pub mod tip_feed;

pub use config::{RpcConfig, RpcTlsConfig};
pub use context::{
    EraHistoryView, EraSummary, EvalOutcome, GenesisView, LedgerContext, LedgerStateView,
    ParamsView, RawBlock, RawTx, RedeemerPurpose, RedeemerReport, SubmitOutcome, TipInfo,
    UtxoSnapshot,
};
pub use error::RpcError;
pub use mempool_feed::{MempoolEvent, MempoolFeed, MempoolRemoveReason};
pub use metrics::{noop_metrics, NoopMetrics, RpcMetricsSink, SharedMetrics};
pub use server::{RpcServer, RpcServerHandle};
pub use tip_feed::{TipFeed, TipPublisher, TipRollback};
