//! Native UTxO RPC (gRPC) server for `dugite-node` — issue #672.
//!
//! This crate is the sole consumer of `tonic` / `prost` in the workspace.
//! `dugite-node` depends on `dugite-rpc` as an opaque library and never sees
//! the gRPC stack in its direct compile graph, keeping the node binary
//! small when the RPC server is disabled (the default).
//!
//! See `.claude/plans/create-a-detailed-plan-adaptive-pretzel.md` for the
//! multi-milestone implementation plan. M1.A (this scaffold) ships:
//!
//! * Vendored proto from `utxorpc/spec v0.19.2` + tonic-build codegen.
//! * `RpcServer::start` with gRPC reflection + gRPC-Web + TLS + cooperative
//!   shutdown.
//! * `LedgerContext` async trait abstraction over the node — no
//!   `dugite-node` symbols leak into this crate's public API.
//! * Service stubs for `SyncService` / `QueryService` / `SubmitService` /
//!   `WatchService` in both `v1alpha` and `v1beta` — every method returns
//!   `UNIMPLEMENTED` until M1.B and later milestones land.
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
    EraHistoryView, EraSummary, EvalOutcome, GenesisView, LedgerContext, ParamsView, RawBlock,
    RawTx, SubmitOutcome, TipInfo, UtxoSnapshot,
};
pub use error::RpcError;
pub use mempool_feed::{MempoolEvent, MempoolFeed, MempoolRemoveReason};
pub use metrics::{noop_metrics, NoopMetrics, RpcMetricsSink, SharedMetrics};
pub use server::{RpcServer, RpcServerHandle};
pub use tip_feed::{TipFeed, TipPublisher, TipRollback};
