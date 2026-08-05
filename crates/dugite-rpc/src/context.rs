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
//! Every service (`SyncService` / `QueryService` / `SubmitService` /
//! `WatchService`) is implemented end-to-end, so a production
//! `LedgerContext` impl needs every method above to actually work.
//! `RpcError::Unimplemented` remains for genuinely optional capabilities
//! a given host may not support (e.g. `utxos_by_payment_credential`
//! without a payment-credential index).

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
/// `cbor` is plain `Vec<u8>`, not a zero-copy `bytes::Bytes` handle into
/// the `ChainDB` slice — deliberately, for simplicity, at the cost of
/// one extra copy per call. A `LedgerContext` impl that can hand back a
/// borrowed/`Bytes`-backed slice without copying would need this field
/// widened; not done, and not a behavioural gap (every field is still
/// populated correctly), just an unclaimed allocation optimisation.
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
/// Wraps the in-tree `ProtocolParameters` so `crate::map::pparams` can
/// read every field without `dugite-rpc` re-shaping it. `Arc` so adapter
/// calls are cheap — params change only at epoch boundaries.
#[derive(Clone, Debug)]
pub struct ParamsView {
    pub params: Arc<dugite_primitives::protocol_params::ProtocolParameters>,
    /// Protocol version major (PV) at the tip, used by the mapper to
    /// gate Conway-only fields.
    pub protocol_version_major: u64,
}

/// Opaque era-history view returned by [`LedgerContext::era_history`].
///
/// Issue #1009: `EraSummary` now carries both the `start` AND `end`
/// boundary (previously `end` was always unset, and `start` itself was
/// missing `epoch`/`time_ms` — the mapper hardcoded both to zero). The
/// one field still deliberately absent is `protocol_params` (the
/// `PParams` in force during that era): dugite's ledger only retains the
/// CURRENT era's params, not a per-era history, so there is nothing
/// truthful to populate for past eras. Left `None` — a documented
/// absence, not a silent one; see `crate::map::pparams` for the current
/// live view (`QueryService.ReadParams`, which does not have this gap).
#[derive(Clone, Debug, Default)]
pub struct EraHistoryView {
    pub summaries: Vec<EraSummary>,
}

/// One era boundary (used for both `EraSummary::start` and `::end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EraBoundaryView {
    /// Milliseconds since the Unix epoch (wall-clock), not relative to
    /// system start — the proto field (`EraBoundary.time`) is an
    /// absolute ms timestamp.
    pub time_ms: u64,
    pub slot: u64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct EraSummary {
    pub era: Era,
    pub start: EraBoundaryView,
    /// `None` for the current (open) era — matches
    /// `dugite_consensus::era_history::EraSummaryEntry::end`.
    pub end: Option<EraBoundaryView>,
    pub slot_length_ms: u32,
    pub epoch_length_slots: u32,
}

/// Opaque genesis view returned by [`LedgerContext::genesis`].
///
/// Issue #1009: carries the full Shelley-genesis section of
/// `cardano.Genesis` (14 of its 34 fields) — the Shelley genesis struct
/// is retained for the node's lifetime (`Node::shelley_genesis`), so
/// this is real data, not derived/guessed. Byron (9 fields: `avvm_distr`,
/// `boot_stakeholders`, `heavy_delegation`, `vss_certs`, ...), Alonzo (7:
/// `cost_models`, `execution_prices`, ...), and Conway (10: `committee`,
/// `constitution`, `drep_voting_thresholds`, ...) sections remain
/// unpopulated — DELIBERATELY, not silently: those genesis structs are
/// parsed once during `Node::new()` and dropped rather than retained,
/// which is real additional lifecycle plumbing (mirroring what
/// `shelley_genesis` already does) beyond this issue's scope. `gen_delegs`
/// / `initial_funds` / `staking` (Shelley genesis fields that exist but
/// are `HashMap`-shaped, mostly empty on real networks, and only
/// meaningful for custom devnets) are also left out for the same reason
/// — real, bounded follow-up work, not implemented here.
#[derive(Clone, Debug, Default)]
pub struct GenesisView {
    pub network_magic: u32,
    pub network_id: String,
    pub system_start_unix: i64,
    pub security_param: u32,
    pub epoch_length: u32,
    pub slot_length: u32,
    pub max_lovelace_supply: u64,
    pub max_kes_evolutions: u32,
    pub slots_per_kes_period: u32,
    pub update_quorum: u32,
    /// `(numerator, denominator)` reconstructed from the genesis JSON's
    /// decimal `activeSlotsCoeff` (e.g. `0.05` -> `(1, 20)`) — see
    /// `dugite-node`'s `rpc_adapter.rs` for the conversion. `None` if the
    /// value couldn't be reconstructed as a clean rational.
    pub active_slots_coeff: Option<(i32, u32)>,
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
    /// Per-redeemer execution reports from the CEK machine. Empty when
    /// the tx has no Plutus redeemers or when Phase-1 validation failed
    /// before Phase-2 could run.
    pub redeemers: Vec<RedeemerReport>,
}

/// Per-redeemer execution outcome from the CEK machine. Mirrors the
/// fields needed to populate utxorpc `TxEval.redeemers` / `traces` /
/// `errors`.
#[derive(Clone, Debug)]
pub struct RedeemerReport {
    /// 0-based index of the redeemer within its purpose group.
    pub index: u32,
    /// Redeemer purpose tag (Spend / Mint / Cert / Reward / Vote / Propose).
    pub purpose: RedeemerPurpose,
    /// ExUnits consumed: `(cpu_steps, memory)`.
    pub ex_units: (u64, u64),
    /// `trace` builtin output captured during evaluation.
    pub logs: Vec<String>,
    /// `None` on success; carries the typed `PhaseTwoError` message on
    /// per-redeemer failure.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedeemerPurpose {
    Unspecified,
    Spend,
    Mint,
    Cert,
    Reward,
    Vote,
    Propose,
}

/// Minimum-viable ledger-state envelope returned by
/// [`LedgerContext::ledger_state`]. Holds the epoch + the tip the
/// snapshot was taken at.
#[derive(Clone, Debug)]
pub struct LedgerStateView {
    pub tip: TipInfo,
    pub epoch: u64,
    pub slot_in_epoch: u64,
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

    /// Predicate-based UTxO scan — invoked when the caller's
    /// `UtxoPredicate` cannot be served by the address / payment-cred
    /// / asset indexes (e.g. `delegation_part`, `not`, `all_of` /
    /// `any_of` composites). Returns at most `cap` matches; impl is
    /// expected to walk the in-memory UTxO map and apply `keep` to
    /// each candidate.
    ///
    /// Returning `Err(Unimplemented)` is acceptable for backends that
    /// cannot afford the full scan (e.g. the LSM store); the service
    /// layer then surfaces UNIMPLEMENTED with a descriptive reason.
    async fn utxos_filter(
        &self,
        keep: &(dyn for<'a> Fn(&'a UtxoSnapshot) -> bool + Send + Sync),
        cap: usize,
    ) -> Result<Vec<UtxoSnapshot>, RpcError>;

    /// Fetch a datum by its 32-byte hash. Bounded scan: implementations
    /// may walk the current UTxO set's inline datums plus a configurable
    /// window of recent volatile blocks' witness data. Returns `None`
    /// if not found inside the scan window.
    async fn datum_by_hash(&self, hash: &Hash32) -> Result<Option<Vec<u8>>, RpcError>;

    /// Fetch a transaction by its 32-byte hash. Bounded scan: checks
    /// the mempool first, then walks the last N volatile blocks.
    /// Returns `None` if not found inside the scan window.
    async fn tx_by_hash(&self, hash: &TransactionHash) -> Result<Option<RawTx>, RpcError>;

    /// Return a compact ledger-state snapshot envelope. The current
    /// view is intentionally minimal (epoch + tip slot); richer queries
    /// (stake-pool distribution, DRep delegation, etc.) are layered on
    /// top once the underlying state projections stabilise.
    async fn ledger_state(&self) -> Result<LedgerStateView, RpcError>;

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
