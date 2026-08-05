//! Fully-qualified proto message names for every response type
//! `masking::apply` is called against.
//!
//! Centralised here (rather than as string literals scattered across
//! `services/*.rs`) for one reason: a typo in the name is a silent
//! fail-open in [`crate::masking::apply`] (no pruning happens, no error
//! surfaces), so `masking::tests::every_message_name_used_by_apply_resolves`
//! walks [`ALL_RESPONSE_MESSAGE_NAMES`] against the descriptor pool once,
//! covering every call site instead of trusting each one individually.
//!
//! v1alpha responses are masked via their v1beta name (see the doc on
//! `services::query` / `services::sync` for why: the v1alpha helpers
//! build the response through the shared `*_response_beta` function and
//! only recode to v1alpha afterwards, so the mask is already applied by
//! the time the v1alpha shape exists). v1alpha's `WatchTx` / `WatchMempool`
//! streaming paths build their own response type directly and so use a
//! dedicated v1alpha name — those are listed separately below.

// ─── v1beta.query ──────────────────────────────────────────────────────────

pub const READ_PARAMS_RESPONSE: &str = "utxorpc.v1beta.query.ReadParamsResponse";
pub const READ_UTXOS_RESPONSE: &str = "utxorpc.v1beta.query.ReadUtxosResponse";
pub const SEARCH_UTXOS_RESPONSE: &str = "utxorpc.v1beta.query.SearchUtxosResponse";
pub const READ_DATA_RESPONSE: &str = "utxorpc.v1beta.query.ReadDataResponse";
pub const READ_TX_RESPONSE: &str = "utxorpc.v1beta.query.ReadTxResponse";
pub const READ_GENESIS_RESPONSE: &str = "utxorpc.v1beta.query.ReadGenesisResponse";
pub const READ_ERA_SUMMARY_RESPONSE: &str = "utxorpc.v1beta.query.ReadEraSummaryResponse";
pub const READ_STATE_RESPONSE: &str = "utxorpc.v1beta.query.ReadStateResponse";

// ─── v1beta.sync ───────────────────────────────────────────────────────────

pub const FETCH_BLOCK_RESPONSE: &str = "utxorpc.v1beta.sync.FetchBlockResponse";
pub const DUMP_HISTORY_RESPONSE: &str = "utxorpc.v1beta.sync.DumpHistoryResponse";
pub const FOLLOW_TIP_RESPONSE: &str = "utxorpc.v1beta.sync.FollowTipResponse";

// `FetchBlock` / `DumpHistory` build the v1alpha response directly
// (element-wise recode of each block, not a whole-response recode of an
// already-masked v1beta value like `query.rs` does), so they need their
// own name — unlike `FollowTipResponse`, whose v1alpha stream re-pipes
// from the (already masked) v1beta stream and so never masks natively.

pub const FETCH_BLOCK_RESPONSE_ALPHA: &str = "utxorpc.v1alpha.sync.FetchBlockResponse";
pub const DUMP_HISTORY_RESPONSE_ALPHA: &str = "utxorpc.v1alpha.sync.DumpHistoryResponse";

// ─── v1beta.watch / v1beta.submit ──────────────────────────────────────────

pub const WATCH_TX_RESPONSE_BETA: &str = "utxorpc.v1beta.watch.WatchTxResponse";
pub const WATCH_MEMPOOL_RESPONSE_BETA: &str = "utxorpc.v1beta.submit.WatchMempoolResponse";

// ─── v1alpha — only where the alpha response is built directly, not via
// a v1beta helper + recode ──────────────────────────────────────────────

pub const WATCH_TX_RESPONSE_ALPHA: &str = "utxorpc.v1alpha.watch.WatchTxResponse";
pub const WATCH_MEMPOOL_RESPONSE_ALPHA: &str = "utxorpc.v1alpha.submit.WatchMempoolResponse";

/// Every name above, for the exhaustive descriptor-pool resolvability
/// guard in `masking::tests`.
pub const ALL_RESPONSE_MESSAGE_NAMES: &[&str] = &[
    READ_PARAMS_RESPONSE,
    READ_UTXOS_RESPONSE,
    SEARCH_UTXOS_RESPONSE,
    READ_DATA_RESPONSE,
    READ_TX_RESPONSE,
    READ_GENESIS_RESPONSE,
    READ_ERA_SUMMARY_RESPONSE,
    READ_STATE_RESPONSE,
    FETCH_BLOCK_RESPONSE,
    DUMP_HISTORY_RESPONSE,
    FOLLOW_TIP_RESPONSE,
    FETCH_BLOCK_RESPONSE_ALPHA,
    DUMP_HISTORY_RESPONSE_ALPHA,
    WATCH_TX_RESPONSE_BETA,
    WATCH_MEMPOOL_RESPONSE_BETA,
    WATCH_TX_RESPONSE_ALPHA,
    WATCH_MEMPOOL_RESPONSE_ALPHA,
];
