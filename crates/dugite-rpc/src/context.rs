//! `LedgerContext` — the async trait the RPC server uses to talk to the
//! node.
//!
//! The trait is dep-free of `dugite-node` so this crate's compile graph
//! stays small. The host (`dugite-node`) provides a concrete impl
//! (`NodeRpcAdapter`) that delegates to `ChainDBBlockProvider`,
//! `LedgerTxValidator`, `LedgerState`, `Mempool`, and so on.
//!
//! All methods are `async` (via `async_trait`) so impls can acquire
//! `RwLock` reads without `block_in_place`. Returning `Result<_, RpcError>`
//! lets service code map cleanly to gRPC status codes via
//! `error::RpcError::into::<tonic::Status>()`.
//!
//! In M1.A only a handful of methods need a real implementation
//! (`tip`, `block_by_hash`, `block_at_slot`, `intersect`, `genesis`); the
//! rest can return `RpcError::Unimplemented` until the relevant service
//! implementation lands.

use async_trait::async_trait;
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::transaction::{TransactionInput, TransactionOutput};
use dugite_primitives::Era;
use std::sync::Arc;

use crate::error::RpcError;

// ─── Lightweight carrier types ────────────────────────────────────────────

/// Current chain tip — payload-shaped to match
/// `node::tip_broadcast::TipApply` so the RPC adapter can forward without
/// re-shaping.
#[derive(Clone, Debug)]
pub struct TipInfo {
    pub slot: u64,
    pub hash: [u8; 32],
    pub block_number: u64,
    pub era: Era,
}

/// A block in its native CBOR form, paired with the indexable metadata
/// the RPC layer needs without re-decoding.
///
/// `cbor` is borrowed-style ownership: the caller can either hand back the
/// `ChainDB` slice directly (in M1.B we wrap it in `Bytes` for zero-copy)
/// or allocate a `Vec`. For M1.A we use `Vec<u8>` for simplicity.
#[derive(Clone, Debug)]
pub struct RawBlock {
    pub slot: u64,
    pub hash: [u8; 32],
    pub block_number: u64,
    pub era: Era,
    pub cbor: Vec<u8>,
}

/// A transaction in mempool / wire form.
#[derive(Clone, Debug)]
pub struct RawTx {
    pub hash: TransactionHash,
    /// Full transaction CBOR (body + witness + is_valid + aux wrapper).
    /// May be empty if the tx was admitted via a path that did not retain
    /// raw bytes (in practice this should not happen post-M0.2).
    pub cbor: Vec<u8>,
}

/// A single UTxO entry, paired with the slot/height at which it became
/// resident (helpful for `UtxoSnapshot.slot` projection in utxorpc).
#[derive(Clone, Debug)]
pub struct UtxoSnapshot {
    pub ref_: TransactionInput,
    pub output: TransactionOutput,
    /// Slot at which the producing transaction was applied. `None` if
    /// the source path can't recover it (genesis UTxO, mempool virtual
    /// UTxO).
    pub slot: Option<u64>,
}

/// Opaque protocol-params view returned by [`LedgerContext::params_at_tip`].
///
/// Wraps the in-tree `ProtocolParameters` so M1.B mapping can read every
/// field without `dugite-rpc` re-shaping it. `Arc` so adapter calls are
/// cheap — params change only at epoch boundaries.
#[derive(Clone, Debug)]
pub struct ParamsView {
    pub params: Arc<dugite_primitives::protocol_params::ProtocolParameters>,
    /// Protocol version major (PV) at the tip, used by the mapper to
    /// gate Conway-only fields.
    pub protocol_version_major: u64,
}

/// Opaque era-history view returned by [`LedgerContext::era_history`].
///
/// Refined into a richer shape in M1.B as the QueryService mapping needs
/// it; this M1.A placeholder is just enough to compile.
#[derive(Clone, Debug, Default)]
pub struct EraHistoryView {
    /// Era boundaries: `(era, first_slot, slot_length_ms, epoch_length_slots)`
    /// for each era the chain has crossed, in chronological order.
    pub summaries: Vec<EraSummary>,
}

#[derive(Clone, Copy, Debug)]
pub struct EraSummary {
    pub era: Era,
    pub first_slot: u64,
    pub slot_length_ms: u32,
    pub epoch_length_slots: u32,
}

/// Opaque genesis view returned by [`LedgerContext::genesis`].
///
/// Currently a placeholder. M1.B fills the fields the QueryService
/// `ReadGenesis` response needs (system start, network magic, genesis
/// pool params, security parameter, etc.) without coupling this crate
/// to `dugite-ledger::CombinedGenesis`.
#[derive(Clone, Debug, Default)]
pub struct GenesisView {
    pub network_magic: u32,
    pub system_start_unix: i64,
    pub security_param: u32,
}

/// Outcome of a transaction submission.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// The tx was admitted to the mempool (or was already there).
    Accepted { hash: TransactionHash },
    /// Phase-1 / Phase-2 / mempool-admission validation rejected it.
    /// The string is the structured `TxValidationError` rendered verbatim
    /// so cardano-node per-rule semantics survive across the wire.
    Rejected { reason: String },
}

/// Outcome of a non-committing transaction evaluation (SubmitService.EvalTx).
#[derive(Clone, Debug)]
pub struct EvalOutcome {
    /// The transaction's declared fee (in lovelace).
    pub fee: u64,
    /// `None` on successful evaluation; `Some(reason)` carries the
    /// structured `TxValidationError` message on failure.
    pub error: Option<String>,
}

// ─── The trait ────────────────────────────────────────────────────────────

/// The full API surface the RPC server uses to talk to the node.
///
/// Implementations live in the host crate (`dugite-node`); this crate
/// only consumes the abstraction.
#[async_trait]
pub trait LedgerContext: Send + Sync + 'static {
    // ── chain / blocks ───────────────────────────────────────────────────

    async fn tip(&self) -> Result<TipInfo, RpcError>;

    async fn block_by_hash(&self, hash: &Hash32) -> Result<Option<RawBlock>, RpcError>;

    async fn block_at_slot(&self, slot: u64) -> Result<Option<RawBlock>, RpcError>;

    /// First block strictly after `slot`. Used by `DumpHistory` to page
    /// through blocks without requiring the client to know exact slots.
    async fn block_after(&self, slot: u64) -> Result<Option<RawBlock>, RpcError>;

    /// Find the latest of the supplied points that exists on the local
    /// chain — the standard ChainSync intersection used by `FollowTip`.
    async fn intersect(&self, points: &[Point]) -> Result<Option<Point>, RpcError>;

    /// Return at most `limit` blocks with slot in `[from_slot, to_slot]`.
    async fn blocks_range(
        &self,
        from_slot: u64,
        to_slot: u64,
        limit: usize,
    ) -> Result<Vec<RawBlock>, RpcError>;

    // ── ledger / UTxO ────────────────────────────────────────────────────

    async fn utxo_by_ref(&self, refs: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError>;

    async fn utxos_by_address(&self, addr: &Address) -> Result<Vec<UtxoSnapshot>, RpcError>;

    /// UTxO lookup by payment credential — NOT indexed in v1, returns
    /// `Unimplemented` until the LSM column-family pattern lands
    /// (tracking issue follow-up).
    async fn utxos_by_payment_credential(
        &self,
        cred: &Hash32,
    ) -> Result<Vec<UtxoSnapshot>, RpcError>;

    /// UTxO lookup by asset. Best-effort O(N) scan capped at a safety
    /// limit; over-cap returns `ResourceExhausted`.
    async fn utxos_by_asset(
        &self,
        policy: &Hash32,
        name: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError>;

    async fn params_at_tip(&self) -> Result<ParamsView, RpcError>;

    async fn era_history(&self) -> Result<EraHistoryView, RpcError>;

    async fn genesis(&self) -> Result<GenesisView, RpcError>;

    // ── submission ───────────────────────────────────────────────────────

    /// Validate + admit a transaction to the mempool. Mirrors the N2C
    /// `LocalTxSubmission` path so on-chain semantics are identical.
    /// `era` is the Cardano era tag the client claims for the wrapping
    /// CBOR (`u16`, 0 = Byron, 1 = Shelley, …); the validator double-
    /// checks against the body shape.
    async fn submit_tx(&self, era: u16, raw_cbor: &[u8]) -> SubmitOutcome;

    /// Non-committing Phase-1 + Phase-2 evaluation. Runs the same
    /// validation pipeline as `submit_tx` but does NOT admit the tx
    /// to the mempool — suitable for `SubmitService.EvalTx` dry-runs.
    async fn eval_tx(&self, era: u16, raw_cbor: &[u8]) -> EvalOutcome;

    // ── mempool snapshot (live feed lives in MempoolFeed) ────────────────

    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError>;

    async fn mempool_contains(&self, hash: &TransactionHash) -> bool;
}
