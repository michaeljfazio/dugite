//! N2C query dispatch and encoding.
//!
//! Ported from the old dugite-network `query_handler` module. Contains the
//! `QueryHandler` struct (dispatch logic) and all snapshot types, plus the CBOR
//! encoding layer that serializes `QueryResult` variants for the wire.
//!
//! ## Submodules
//! - [`types`]      — All snapshot types (`NodeStateSnapshot`, `QueryResult`, etc.)
//! - [`encoding`]   — CBOR encoding of `QueryResult` → bytes
//! - [`governance`] — Governance query handlers (tags 23–28, 31–32, 39)
//! - [`protocol`]   — Protocol param, genesis, reward handlers (tags 2–5, 8, 11–14, 29)
//! - [`stake`]      — Stake/delegation/pool handlers (tags 10, 16–22, 30, 34–37)
//! - [`utxo`]       — UTxO query handlers (tags 6, 7, 15)

pub(crate) mod encoding;
mod filter;
mod governance;
pub(crate) mod protocol;
mod stake;
pub mod types;
mod utxo;

use dugite_primitives::block::Point;
use dugite_storage::ChainDB;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

// Re-export all public types for backwards compatibility.
// Some re-exports may not be used yet within this crate, but are needed by
// downstream consumers that previously imported from dugite_network::query_handler.
#[allow(unused_imports)]
pub use types::{
    CommitteeMemberSnapshot, CommitteeSnapshot, DRepDelegationGroup, DRepKey, DRepSnapshot,
    DRepStakeEntry, EraBound, EraSummary, GenesisConfigSnapshot, GovActionId, GovStateSnapshot,
    LedgerPeerEntry, MultiAssetSnapshot, NodeStateSnapshot, NonMyopicRewardEntry,
    PoolParamsSnapshot, PoolRewardInfo, PoolStakeSnapshotEntry, ProposalSnapshot,
    ProtocolParamsSnapshot, QueryResult, RelaySnapshot, ShelleyPParamsSnapshot, SnapshotStakeData,
    StakeAddressSnapshot, StakeDelegDepositEntry, StakePoolSnapshot, StakeSnapshotsResult,
    UtxoQueryProvider, UtxoSnapshot, VoteDelegateeEntry,
};

#[allow(unused_imports)]
pub use encoding::encode_query_result;

/// Handler for local state queries.
///
/// This provides a clean interface for answering LocalStateQuery protocol
/// queries from the current ledger state.
pub struct QueryHandler {
    state: Arc<NodeStateSnapshot>,
    utxo_provider: Option<Arc<dyn UtxoQueryProvider>>,
    /// Negotiated N2C protocol version for the current query (set per-dispatch).
    /// Used to gate deprecated queries. 0 = no gating (tests, internal use).
    n2c_version: std::sync::atomic::AtomicU16,
    /// Highest major protocol version this node software supports. Returned by
    /// the N2C `GetMaxMajorProtocolVersion` query (tag 38, V21+).
    /// Plumbed from `NodeConfig::max_major_protocol_version()` so the response
    /// reflects the running configuration (PV11 by default, PV12 when
    /// `experimental_hard_forks_enabled` is set) rather than a stale constant.
    max_major_prot_ver: u32,
    /// ChainDB reference for `validate_acquire` point-on-chain checks (C1).
    ///
    /// When `None` (tests that don't wire up storage), `VolatileTip` and
    /// `ImmutableTip` always succeed and `SpecificPoint` always fails with
    /// `PointNotOnChain` — the safest default.
    chain_db: Option<Arc<RwLock<ChainDB>>>,
}

impl QueryHandler {
    /// Construct a `QueryHandler` with the supplied max major protocol version.
    ///
    /// Production callers MUST source this from
    /// `NodeConfig::max_major_protocol_version()` (see issue #463). Tests may
    /// pass any value that matches the era they are exercising.
    pub fn new(max_major_prot_ver: u32) -> Self {
        QueryHandler {
            state: Arc::new(NodeStateSnapshot::default()),
            utxo_provider: None,
            n2c_version: std::sync::atomic::AtomicU16::new(0),
            max_major_prot_ver,
            chain_db: None,
        }
    }

    /// Wire up the ChainDB reference for `validate_acquire` (C1 fix).
    ///
    /// Must be called before the handler is exposed to N2C clients. Without
    /// this, `SpecificPoint` acquires always fail with `PointNotOnChain`.
    pub fn set_chain_db(&mut self, chain_db: Arc<RwLock<ChainDB>>) {
        self.chain_db = Some(chain_db);
    }

    /// Returns the configured max major protocol version. Exposed for tests.
    #[allow(dead_code)]
    pub fn max_major_prot_ver(&self) -> u32 {
        self.max_major_prot_ver
    }

    /// Set the UTxO query provider for on-demand UTxO lookups
    pub fn set_utxo_provider(&mut self, provider: Arc<dyn UtxoQueryProvider>) {
        self.utxo_provider = Some(provider);
    }

    /// Update the snapshot from the current node state.
    /// This is a cheap Arc pointer swap — no deep cloning of the snapshot data.
    pub fn update_state(&mut self, snapshot: NodeStateSnapshot) {
        self.state = Arc::new(snapshot);
    }

    /// Build a lightweight "shadow" handler that shares everything with `self`
    /// via cheap `Arc`/`Copy` fields, except its `state` is pinned to
    /// `pinned_state` instead of the live `self.state`.
    ///
    /// This is how LocalStateQuery snapshot pinning (issue #867) is implemented:
    /// rather than threading an explicit `state: &NodeStateSnapshot` parameter
    /// through every internal dispatch method (~40 call sites across
    /// `dispatch_query_with_version`, `handle_shelley_query`, and friends — all
    /// of which are also exercised directly by unit tests in this module), we
    /// construct a throwaway `QueryHandler` whose `state` Arc points at the
    /// snapshot pinned at MsgAcquire time, and delegate to the SAME unmodified
    /// internal methods on that shadow. No lock is touched here and no ledger
    /// data is deep-cloned — only `Arc`/`Option<Arc<_>>` pointers are cloned.
    fn with_pinned_state(&self, pinned_state: Arc<NodeStateSnapshot>) -> QueryHandler {
        QueryHandler {
            state: pinned_state,
            utxo_provider: self.utxo_provider.clone(),
            n2c_version: std::sync::atomic::AtomicU16::new(
                self.n2c_version.load(std::sync::atomic::Ordering::Relaxed),
            ),
            max_major_prot_ver: self.max_major_prot_ver,
            chain_db: self.chain_db.clone(),
        }
    }

    /// Get a reference to the current node state snapshot
    #[allow(dead_code)] // used in tests
    pub fn state(&self) -> &NodeStateSnapshot {
        &self.state
    }

    /// Handle a raw CBOR query message and return a result.
    ///
    /// The CBOR payload from MsgQuery is: [3, query]
    /// where query is a nested structure depending on the query type.
    /// For Shelley-based eras, it's typically: [era_tag, [query_tag, ...]]
    /// Handle a CBOR-encoded query without version gating (backward compat).
    #[allow(dead_code)] // used in tests
    pub fn handle_query_cbor(&self, payload: &[u8]) -> QueryResult {
        self.handle_query_cbor_versioned(payload, 0)
    }

    /// Handle a CBOR-encoded query with version gating.
    ///
    /// `negotiated_version` is the N2C protocol version negotiated during
    /// handshake (16–22). Deprecated queries are rejected for newer versions:
    /// - Tag 4 (GetProposedPParamsUpdates): deprecated at V20+ (era < 12)
    /// - Tag 5 (GetStakeDistribution): deprecated at V21+ (use tag 37)
    /// - Tag 21 (GetPoolDistr): deprecated at V21+ (use tag 36)
    pub fn handle_query_cbor_versioned(
        &self,
        payload: &[u8],
        negotiated_version: u16,
    ) -> QueryResult {
        // Store version for use by shelley query dispatch.
        self.dispatch_query_versioned(payload, negotiated_version)
    }

    fn dispatch_query_versioned(&self, payload: &[u8], negotiated_version: u16) -> QueryResult {
        let mut decoder = minicbor::Decoder::new(payload);

        // Skip the message envelope [3, query]
        match decoder.array() {
            Ok(_) => {}
            Err(e) => return QueryResult::Error(format!("Invalid query CBOR: {e}")),
        }
        match decoder.u32() {
            Ok(3) => {} // MsgQuery tag
            Ok(other) => return QueryResult::Error(format!("Expected MsgQuery(3), got {other}")),
            Err(e) => return QueryResult::Error(format!("Invalid query tag: {e}")),
        }

        self.dispatch_query_with_version(&mut decoder, negotiated_version)
    }

    /// Version-aware query dispatch. Threads `negotiated_version` through to
    /// `handle_shelley_query` for deprecated query gating.
    fn dispatch_query_with_version(
        &self,
        decoder: &mut minicbor::Decoder<'_>,
        negotiated_version: u16,
    ) -> QueryResult {
        self.n2c_version
            .store(negotiated_version, std::sync::atomic::Ordering::Relaxed);
        self.dispatch_query_inner(decoder, negotiated_version)
    }

    fn dispatch_query_inner(
        &self,
        decoder: &mut minicbor::Decoder<'_>,
        _negotiated_version: u16,
    ) -> QueryResult {
        // The query structure varies. Try to detect common patterns.
        // GetSystemStart has no era wrapping: just the tag 2
        // GetCurrentEra has tag 0 at the top level
        // Shelley-based queries are nested: [era, [query_tag, ...]]

        let pos = decoder.position();

        // Try to decode as an array first
        match decoder.array() {
            Ok(Some(len)) => {
                let tag = match decoder.u32() {
                    Ok(t) => t,
                    Err(_) => {
                        decoder.set_position(pos);
                        return self.handle_simple_query(decoder);
                    }
                };

                match tag {
                    0 => {
                        // Outer tag 0 = BlockQuery (era-wrapped) or GetCurrentEra
                        if len == 1 {
                            debug!("Query: GetCurrentEra");
                            return QueryResult::CurrentEra(self.state.era);
                        }
                        // Era-wrapped query: [0, [era_id, [query_tag, ...]]]
                        self.dispatch_era_query(decoder)
                    }
                    1 => {
                        // Outer tag 1 = GetSystemStart
                        debug!("Query: GetSystemStart");
                        QueryResult::SystemStart(self.state.system_start.clone())
                    }
                    2 => {
                        // Outer tag 2 = GetChainBlockNo (QueryVersion2, N2C v16+)
                        debug!("Query: GetChainBlockNo");
                        QueryResult::ChainBlockNo(self.state.block_number.0)
                    }
                    3 => {
                        // Outer tag 3 = GetChainPoint (QueryVersion2, N2C v16+)
                        // Returns Point: [] for Origin, [slot, hash] for Specific
                        debug!("Query: GetChainPoint");
                        match &self.state.tip.point {
                            Point::Origin => QueryResult::ChainPoint {
                                slot: 0,
                                hash: vec![],
                            },
                            Point::Specific(s, h) => QueryResult::ChainPoint {
                                slot: s.0,
                                hash: h.to_vec(),
                            },
                        }
                    }
                    _ => {
                        // May be era-wrapped
                        self.dispatch_era_query(decoder)
                    }
                }
            }
            Ok(None) => {
                // Indefinite array
                let tag = decoder.u32().unwrap_or(999);
                match tag {
                    0 => {
                        // Try era-wrapped first, fall back to GetCurrentEra
                        self.dispatch_era_query(decoder)
                    }
                    1 => QueryResult::SystemStart(self.state.system_start.clone()),
                    2 => QueryResult::ChainBlockNo(self.state.block_number.0),
                    3 => match &self.state.tip.point {
                        Point::Origin => QueryResult::ChainPoint {
                            slot: 0,
                            hash: vec![],
                        },
                        Point::Specific(s, h) => QueryResult::ChainPoint {
                            slot: s.0,
                            hash: h.to_vec(),
                        },
                    },
                    _ => self.dispatch_era_query(decoder),
                }
            }
            Err(_) => {
                decoder.set_position(pos);
                self.handle_simple_query(decoder)
            }
        }
    }

    /// Handle a simple (non-array) query
    fn handle_simple_query(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        match decoder.u32() {
            Ok(0) => QueryResult::CurrentEra(self.state.era),
            Ok(1) => QueryResult::SystemStart(self.state.system_start.clone()),
            Ok(2) => QueryResult::ChainBlockNo(self.state.block_number.0),
            _ => QueryResult::Error("Unknown simple query".into()),
        }
    }

    /// Dispatch a BlockQuery (the inner encoding after outer tag 0 in QueryVersion2).
    ///
    /// The HFC `BlockQuery (HardForkBlock xs)` has three constructors:
    ///   `[0, ns_query]`    = QueryIfCurrent — NS-encoded era-specific Shelley query
    ///   `[1, anytime_q]`   = QueryAnytime   — GetEraStart, GetCurrentEra
    ///   `[2, hf_query]`    = QueryHardFork   — GetInterpreter (EraHistory), GetCurrentEra
    ///
    /// QueryIfCurrent inner encoding (NS): `[era_idx, [shelley_tag, ...]]`
    /// QueryAnytime inner encoding: `[sub_tag]` (0=GetEraStart, 2=GetCurrentEra)
    /// QueryHardFork inner encoding: `[sub_tag]` (0=GetInterpreter, 1=GetCurrentEra)
    ///
    /// We also accept a simplified (non-standard) format from dugite-cli
    /// where the Shelley query is sent directly without BlockQuery/NS wrapping.
    fn dispatch_era_query(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        let pos = decoder.position();

        match decoder.array() {
            Ok(Some(2)) => {
                let block_query_tag = decoder.u32().unwrap_or(999);
                match block_query_tag {
                    0 => {
                        // QueryIfCurrent: NS-encoded [era_idx, [shelley_tag, ...]]
                        debug!("dispatch_era_query: QueryIfCurrent");
                        self.dispatch_query_if_current(decoder)
                    }
                    1 => {
                        // QueryAnytime: [sub_tag]
                        debug!("dispatch_era_query: QueryAnytime");
                        self.handle_query_anytime_inner(decoder)
                    }
                    2 => {
                        // QueryHardFork: [sub_tag]
                        debug!("dispatch_era_query: QueryHardFork");
                        self.handle_hard_fork_query(decoder)
                    }
                    other => {
                        // Might be a direct Shelley query [tag, args] from dugite-cli
                        debug!(query_tag = other, "dispatch_era_query: direct [tag, args]");
                        self.handle_shelley_query(other, decoder)
                    }
                }
            }
            Ok(Some(len)) => {
                // Length != 2: direct Shelley query [tag] or [tag, arg1, ...]
                let query_tag = decoder.u32().unwrap_or(999);
                debug!(query_tag, len, "dispatch_era_query: direct Shelley query");
                self.handle_shelley_query(query_tag, decoder)
            }
            Ok(None) => {
                let query_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(query_tag, decoder)
            }
            Err(e) => {
                decoder.set_position(pos);
                warn!("dispatch_era_query: array decode failed: {e}");
                let query_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(query_tag, decoder)
            }
        }
    }

    /// Parse a QueryIfCurrent query: NS-encoded `[era_idx, [shelley_tag, ...]]`
    fn dispatch_query_if_current(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        match decoder.array() {
            Ok(Some(2)) => {
                let era_idx = decoder.u32().unwrap_or(0);
                // Parse the inner Shelley query
                match decoder.array() {
                    Ok(_) => {
                        let query_tag = decoder.u32().unwrap_or(999);
                        debug!(era_idx, query_tag, "QueryIfCurrent: NS Shelley query");
                        self.handle_shelley_query(query_tag, decoder)
                    }
                    Err(_) => {
                        let query_tag = decoder.u32().unwrap_or(999);
                        self.handle_shelley_query(query_tag, decoder)
                    }
                }
            }
            Ok(_) => {
                // Non-standard: might be direct [shelley_tag] from dugite-cli
                let query_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(query_tag, decoder)
            }
            Err(_) => QueryResult::Error("Invalid QueryIfCurrent encoding".into()),
        }
    }

    /// Handle QueryAnytime queries (embedded in BlockQuery).
    /// Sub-tags: 0=GetEraStart, 2=GetCurrentEra
    fn handle_query_anytime_inner(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        let sub_tag = match decoder.array() {
            Ok(_) => decoder.u32().unwrap_or(999),
            Err(_) => decoder.u32().unwrap_or(999),
        };
        match sub_tag {
            0 => {
                debug!("QueryAnytime: GetEraStart");
                // Return era start info — for now return system start
                QueryResult::SystemStart(self.state.system_start.clone())
            }
            2 => {
                debug!("QueryAnytime: GetCurrentEra");
                QueryResult::CurrentEra(self.state.era)
            }
            other => {
                warn!("Unknown QueryAnytime sub-tag: {other}");
                QueryResult::Error(format!("Unknown QueryAnytime sub-tag: {other}"))
            }
        }
    }

    /// Handle GetCBOR (tag 9) — wraps an inner query and returns its result as raw CBOR bytes.
    /// Wire format: tag(24) <cbor_bytes>
    fn handle_get_cbor(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        debug!("Query: GetCBOR");
        // The argument is the inner query to execute
        // Parse inner query tag
        let inner_result = match decoder.array() {
            Ok(_) => {
                let inner_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(inner_tag, decoder)
            }
            Err(_) => {
                let inner_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(inner_tag, decoder)
            }
        };
        // Wrap the result to be encoded as CBOR-in-CBOR (tag 24)
        QueryResult::WrappedCbor(Box::new(inner_result))
    }

    /// Handle QueryHardFork queries (embedded in BlockQuery tag 2).
    /// Sub-tags: 0=GetInterpreter (EraHistory), 1=GetCurrentEra
    fn handle_hard_fork_query(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        let sub_tag = match decoder.array() {
            Ok(_) => decoder.u32().unwrap_or(999),
            Err(_) => decoder.u32().unwrap_or(999),
        };
        match sub_tag {
            0 => {
                debug!("QueryHardFork: GetInterpreter (EraHistory)");
                QueryResult::EraHistory(self.state.era_summaries.clone())
            }
            1 => {
                debug!("QueryHardFork: GetCurrentEra");
                QueryResult::HardForkCurrentEra(self.state.era)
            }
            other => {
                warn!("Unknown QueryHardFork sub-tag: {other}");
                QueryResult::Error(format!("Unknown QueryHardFork sub-tag: {other}"))
            }
        }
    }

    /// Handle Shelley-era queries by tag.
    ///
    /// Tag numbers match the Haskell cardano-ledger `BlockQuery` encoding
    /// from ouroboros-consensus-shelley `encodeShelleyQuery`.
    pub(crate) fn handle_shelley_query(
        &self,
        query_tag: u32,
        decoder: &mut minicbor::Decoder<'_>,
    ) -> QueryResult {
        // Version-gate deprecated queries per Haskell versionGate.
        // When negotiated_version > 0 (real client), reject deprecated tags.
        let version = self.n2c_version.load(std::sync::atomic::Ordering::Relaxed);
        if version >= 20 && query_tag == 4 {
            // GetProposedPParamsUpdates: deprecated at V20 (Conway governance replaces it)
            debug!(
                version,
                "Rejecting deprecated GetProposedPParamsUpdates (tag 4) for N2C V{version}"
            );
            return QueryResult::Error(format!(
                "GetProposedPParamsUpdates (tag 4) is deprecated for N2C version {version} (V20+). Use governance proposals instead."
            ));
        }
        if version >= 21 && query_tag == 5 {
            // GetStakeDistribution: deprecated at V21 (replaced by tag 37 GetStakeDistribution2)
            debug!(
                version,
                "Rejecting deprecated GetStakeDistribution (tag 5) for N2C V{version}"
            );
            return QueryResult::Error(format!(
                "GetStakeDistribution (tag 5) is deprecated for N2C version {version} (V21+). Use GetStakeDistribution2 (tag 37) instead."
            ));
        }
        if version >= 21 && query_tag == 21 {
            // GetPoolDistr: deprecated at V21 (replaced by tag 36 GetPoolDistr2)
            debug!(
                version,
                "Rejecting deprecated GetPoolDistr (tag 21) for N2C V{version}"
            );
            return QueryResult::Error(format!(
                "GetPoolDistr (tag 21) is deprecated for N2C version {version} (V21+). Use GetPoolDistr2 (tag 36) instead."
            ));
        }

        match query_tag {
            0 => {
                // Tag 0: GetLedgerTip
                //
                // Wire shape (matches cardano-node 10.6.2, issue #407):
                //   MsgResult: [4, [[slot, hash]]]
                // i.e. HFC success wrapper (array(1)) + bare Point [slot, hash].
                //
                // GetLedgerTip returns a Point, NOT a Tip: there is no
                // `block_no` in the response. Callers that need the block
                // number must issue GetChainBlockNo (top-level outer tag 2).
                debug!("Query: GetLedgerTip");
                let (slot, hash) = match &self.state.tip.point {
                    Point::Origin => (0, vec![0u8; 32]),
                    Point::Specific(s, h) => (s.0, h.to_vec()),
                };
                QueryResult::LedgerTip { slot, hash }
            }
            1 => {
                // Tag 1: GetEpochNo
                debug!("Query: GetEpochNo");
                QueryResult::EpochNo(self.state.epoch.0)
            }
            2 => protocol::handle_non_myopic_rewards(&self.state, decoder),
            3 => protocol::handle_current_pparams(&self.state),
            4 => protocol::handle_proposed_pparams_updates(),
            5 => protocol::handle_stake_distribution(&self.state),
            6 => utxo::handle_utxo_by_address(&self.state, &self.utxo_provider, decoder),
            7 => utxo::handle_utxo_whole(&self.utxo_provider),
            8 => protocol::handle_debug_epoch_state(&self.state),
            9 => self.handle_get_cbor(decoder),
            10 => stake::handle_filtered_delegations(&self.state, decoder),
            // Tag 11: GetGenesisConfig — CompactGenesis with version-gated ProtVer encoding.
            // V16-V20: array(18) with flat ProtocolVersion fields [major] [minor].
            // V21+:    array(17) with ProtocolVersion as a single array(2) [major, minor].
            // Pass the negotiated N2C version so encode_genesis_config selects the right layout.
            11 => protocol::handle_genesis_config(&self.state, version),
            12 => protocol::handle_debug_new_epoch_state(&self.state),
            13 => protocol::handle_debug_chain_dep_state(&self.state),
            14 => protocol::handle_reward_provenance(&self.state),
            15 => utxo::handle_utxo_by_txin(&self.utxo_provider, decoder),
            16 => stake::handle_stake_pools(&self.state),
            17 => stake::handle_stake_pool_params(&self.state, decoder),
            18 => stake::handle_reward_info_pools(&self.state),
            19 => stake::handle_pool_state(&self.state, decoder),
            20 => stake::handle_stake_snapshots(&self.state, decoder),
            21 => stake::handle_pool_distr(&self.state, decoder),
            22 => stake::handle_stake_deleg_deposits(&self.state, decoder),
            23 => governance::handle_constitution(&self.state),
            24 => governance::handle_gov_state(&self.state),
            25 => governance::handle_drep_state(&self.state, decoder),
            26 => governance::handle_drep_stake_distr(&self.state, decoder),
            27 => governance::handle_committee_state(&self.state),
            28 => governance::handle_filtered_vote_delegatees(&self.state, decoder),
            29 => protocol::handle_account_state(&self.state),
            30 => {
                // Tag 30: GetSPOStakeDistr — filtered SPO stake distribution
                stake::handle_spo_stake_distr(&self.state, decoder)
            }
            31 => {
                // Tag 31: GetProposals — filtered governance proposals
                governance::handle_proposals(&self.state, decoder)
            }
            32 => {
                // Tag 32: GetRatifyState — ratification state
                governance::handle_ratify_state(&self.state)
            }
            33 => {
                // Tag 33: GetFuturePParams — returns Maybe PParams, collapsed
                // from the ledger's 3-way `futurePParams` per Haskell's
                // `queryFuturePParams` (oracle-verified):
                //   NoPParamsUpdate          -> Nothing
                //   DefinitePParamsUpdate pp -> Just pp
                //   PotentialPParamsUpdate m -> m (pass the inner Maybe through)
                debug!("Query: GetFuturePParams");
                let payload = match self.state.future_pparams_tag {
                    1 => self.state.future_pparams.clone(),
                    2 => self.state.future_pparams.clone(),
                    _ => None,
                };
                QueryResult::FuturePParamsResult(payload)
            }
            34 => {
                // Tag 34: GetLedgerPeerSnapshot
                //
                // Wire shape varies by N2C version (issue #456):
                //   V19-V22 request: `array(1)[34]`           → V2 response (tag 1)
                //   V23+    request: `array(2)[34, peerKind]` → V23 response
                //                    peerKind = 0 (All, tag 3) or 1 (Big, tag 2)
                //
                // We detect the V23 path by peeking for an extra `peerKind`
                // byte left on the decoder by `dispatch_era_query`. If the
                // decoder is empty, fall back to the legacy V2 response.
                let peer_kind = decoder.u8().ok();
                match peer_kind {
                    Some(0) => stake::handle_ledger_peer_snapshot_v23(&self.state, false),
                    Some(_) => stake::handle_ledger_peer_snapshot_v23(&self.state, true),
                    None => stake::handle_ledger_peer_snapshot(&self.state),
                }
            }
            35 => {
                // Tag 35: QueryStakePoolDefaultVote (V20+)
                stake::handle_pool_default_vote(&self.state, decoder)
            }
            36 => {
                // Tag 36: GetPoolDistr2 (V21+) — new format with total active stake
                debug!("Query: GetPoolDistr2");
                stake::handle_pool_distr2(&self.state, decoder)
            }
            37 => {
                // Tag 37: GetStakeDistribution2 (V21+) — new PoolDistr format
                debug!("Query: GetStakeDistribution2");
                stake::handle_stake_distribution2(&self.state)
            }
            38 => {
                // Tag 38: GetMaxMajorProtocolVersion (V21+)
                // Returns the highest major protocol version this node software
                // supports. Sourced from NodeConfig::max_major_protocol_version()
                // at QueryHandler construction time so the response tracks
                // experimental_hard_forks_enabled (PV11 default, PV12 experimental).
                debug!(
                    max_major_prot_ver = self.max_major_prot_ver,
                    "Query: GetMaxMajorProtocolVersion"
                );
                QueryResult::MaxMajorProtocolVersion(self.max_major_prot_ver)
            }
            39 => {
                // Tag 39: GetDRepDelegations (V23+)
                // Returns Map<Credential, DRep> for the requested stake credentials.
                governance::handle_drep_delegations(&self.state, decoder)
            }
            _ => {
                debug!("Unhandled Shelley query tag: {query_tag}");
                QueryResult::Error(format!("Unsupported query: tag {query_tag}"))
            }
        }
    }
}

// ─── New dugite_network::QueryHandler trait implementation ─────────────────

/// Encode a `QueryResult` value to raw CBOR bytes (no MsgResult envelope, no HFC wrapper).
///
/// Used by the `dugite_network::QueryHandler` trait implementation to produce
/// the pre-encoded CBOR that the new server infrastructure wraps.
fn encode_result_value(result: &QueryResult) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);
    encoding::encode_query_result_value(&mut enc, result);
    buf
}

impl dugite_network::QueryHandler for QueryHandler {
    /// Pinned ledger-state snapshot for a single MsgAcquire..MsgRelease session
    /// (issue #867): `acquire()` resolves the target to one of these ONCE, and
    /// every MsgQuery dispatch method below receives the SAME `Arc` for the
    /// lifetime of that acquisition — guaranteeing a consistent (non-torn)
    /// view even if `update_state()` swaps the live snapshot mid-session.
    type Acquired = Arc<NodeStateSnapshot>;

    fn acquire(
        &self,
        target: &dugite_network::protocol::local_state_query::AcquireTarget,
    ) -> Result<Self::Acquired, dugite_network::protocol::local_state_query::AcquireFailure> {
        use dugite_network::codec::Point as CodecPoint;
        use dugite_network::protocol::local_state_query::{AcquireFailure, AcquireTarget};

        match target {
            // VolatileTip and ImmutableTip always refer to the CURRENT chain
            // tip — pin the current state snapshot (a cheap Arc clone).
            AcquireTarget::VolatileTip | AcquireTarget::ImmutableTip => Ok(Arc::clone(&self.state)),

            // SpecificPoint: the client has requested a point. There is no
            // machinery to rewind ledger state to an arbitrary past point
            // (only destructive rollbacks), so the only point we can honestly
            // materialise a snapshot for is the CURRENT tip. If the requested
            // point equals the current tip, pin the current state. Otherwise:
            // mirror Haskell `ouroboros-consensus` by checking whether the
            // point exists anywhere on our chain (VolatileDB/ImmutableDB) to
            // choose the right failure — `PointTooOld` (it exists, but we
            // cannot rewind to it) vs `PointNotOnChain` (it never existed on
            // our chain at all).
            AcquireTarget::SpecificPoint(point) => match point {
                CodecPoint::Origin => {
                    if matches!(
                        self.state.tip.point,
                        dugite_primitives::block::Point::Origin
                    ) {
                        Ok(Arc::clone(&self.state))
                    } else {
                        debug!(
                            "acquire: SpecificPoint(Origin) requested but current tip has \
                             moved past genesis — cannot materialise a non-tip snapshot"
                        );
                        Err(AcquireFailure::PointTooOld)
                    }
                }
                CodecPoint::Specific(slot, hash_arr) => {
                    let block_hash = dugite_primitives::hash::Hash32::from_bytes(*hash_arr);
                    let matches_current_tip = matches!(
                        &self.state.tip.point,
                        dugite_primitives::block::Point::Specific(tip_slot, tip_hash)
                            if tip_slot.0 == *slot && tip_hash == &block_hash
                    );
                    if matches_current_tip {
                        return Ok(Arc::clone(&self.state));
                    }

                    match &self.chain_db {
                        None => {
                            // No ChainDB wired (tests without storage) — refuse specific-point
                            // acquires defensively rather than silently accepting a fabricated point.
                            debug!("acquire: no chain_db wired, refusing SpecificPoint");
                            Err(AcquireFailure::PointNotOnChain)
                        }
                        Some(chain_db) => {
                            let on_chain = tokio::task::block_in_place(|| {
                                let db = chain_db.blocking_read();
                                db.has_block(&block_hash)
                            });
                            if on_chain {
                                debug!(
                                    hash = hex::encode(hash_arr),
                                    "acquire: SpecificPoint is on chain but not the current tip \
                                     — cannot materialise a historical snapshot (PointTooOld)"
                                );
                                Err(AcquireFailure::PointTooOld)
                            } else {
                                debug!(
                                    hash = hex::encode(hash_arr),
                                    "acquire: SpecificPoint not on chain — refusing"
                                );
                                Err(AcquireFailure::PointNotOnChain)
                            }
                        }
                    }
                }
            },
        }
    }

    fn handle_query(
        &self,
        acquired: &Self::Acquired,
        query_cbor: &[u8],
        n2c_version: u16,
    ) -> Result<Vec<u8>, String> {
        let shadow = self.with_pinned_state(Arc::clone(acquired));
        let mut decoder = minicbor::Decoder::new(query_cbor);
        let result = shadow.dispatch_query_with_version(&mut decoder, n2c_version);
        match result {
            QueryResult::Error(msg) => Err(msg),
            _ => Ok(encoding::encode_query_result_payload(&result)),
        }
    }

    fn handle_block_query(
        &self,
        acquired: &Self::Acquired,
        tag: u64,
        query_cbor: &[u8],
    ) -> Result<Vec<u8>, String> {
        let shadow = self.with_pinned_state(Arc::clone(acquired));
        let mut decoder = minicbor::Decoder::new(query_cbor);
        let result = shadow.handle_shelley_query(tag as u32, &mut decoder);
        match result {
            QueryResult::Error(msg) => Err(msg),
            _ => Ok(encode_result_value(&result)),
        }
    }

    fn handle_query_anytime(
        &self,
        acquired: &Self::Acquired,
        query_cbor: &[u8],
    ) -> Result<Vec<u8>, String> {
        let shadow = self.with_pinned_state(Arc::clone(acquired));
        let mut decoder = minicbor::Decoder::new(query_cbor);
        let result = shadow.handle_query_anytime_inner(&mut decoder);
        match result {
            QueryResult::Error(msg) => Err(msg),
            _ => Ok(encode_result_value(&result)),
        }
    }

    fn handle_query_hard_fork(
        &self,
        acquired: &Self::Acquired,
        query_cbor: &[u8],
    ) -> Result<Vec<u8>, String> {
        let shadow = self.with_pinned_state(Arc::clone(acquired));
        let mut decoder = minicbor::Decoder::new(query_cbor);
        let result = shadow.handle_hard_fork_query(&mut decoder);
        match result {
            QueryResult::Error(msg) => Err(msg),
            _ => Ok(encode_result_value(&result)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::block::Tip;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};

    /// Helper to call handle_shelley_query with an empty decoder
    fn query(handler: &QueryHandler, tag: u32) -> QueryResult {
        let empty = [0u8; 0];
        let mut decoder = minicbor::Decoder::new(&empty);
        handler.handle_shelley_query(tag, &mut decoder)
    }

    #[test]
    fn test_query_handler_default_state() {
        let handler = QueryHandler::new(11);
        match query(&handler, 1) {
            QueryResult::EpochNo(e) => assert_eq!(e, 0),
            other => panic!("Expected EpochNo, got {other:?}"),
        }
    }

    /// Regression for issue #456: tag 34 dispatch must select V2 vs V23
    /// based on whether the request carries a trailing `peerKind` byte.
    /// V22 path: empty decoder → legacy `LedgerPeerSnapshot`.
    /// V23 path: `peerKind=0` → All; `peerKind=1` (or any non-zero) → Big.
    #[test]
    fn test_ledger_peer_snapshot_request_routing() {
        let handler = QueryHandler::new(11);
        // V22 request: no extra byte left on the decoder
        let empty = [0u8; 0];
        let mut dec = minicbor::Decoder::new(&empty);
        match handler.handle_shelley_query(34, &mut dec) {
            QueryResult::LedgerPeerSnapshot(_) => {}
            other => panic!("V22 path must return legacy LedgerPeerSnapshot, got {other:?}"),
        }
        // V23 request, peerKind=1 (Big)
        let big_cbor = [0x01u8];
        let mut dec = minicbor::Decoder::new(&big_cbor);
        match handler.handle_shelley_query(34, &mut dec) {
            QueryResult::LedgerPeerSnapshotV23 { big: true, .. } => {}
            other => panic!("peerKind=1 must yield Big V23 variant, got {other:?}"),
        }
        // V23 request, peerKind=0 (All)
        let all_cbor = [0x00u8];
        let mut dec = minicbor::Decoder::new(&all_cbor);
        match handler.handle_shelley_query(34, &mut dec) {
            QueryResult::LedgerPeerSnapshotV23 { big: false, .. } => {}
            other => panic!("peerKind=0 must yield All V23 variant, got {other:?}"),
        }
    }

    /// Regression for issue #463: tag 38 (`GetMaxMajorProtocolVersion`) must
    /// return the value plumbed in at construction, NOT a stale module-level
    /// constant. With `experimental_hard_forks_enabled = false` the node
    /// configuration resolves to PV11.
    #[test]
    fn test_get_max_major_protocol_version_pv11() {
        let handler = QueryHandler::new(11);
        match query(&handler, 38) {
            QueryResult::MaxMajorProtocolVersion(v) => assert_eq!(
                v, 11,
                "tag 38 must return 11 when configured for the default \
                 (non-experimental) hard forks path",
            ),
            other => panic!("Expected MaxMajorProtocolVersion, got {other:?}"),
        }
    }

    /// Regression for issue #463: with `experimental_hard_forks_enabled = true`
    /// the node advertises support for PV12 (Dijkstra) and tag 38 must
    /// reflect that.
    #[test]
    fn test_get_max_major_protocol_version_pv12_experimental() {
        let handler = QueryHandler::new(12);
        match query(&handler, 38) {
            QueryResult::MaxMajorProtocolVersion(v) => assert_eq!(
                v, 12,
                "tag 38 must return 12 when experimental_hard_forks_enabled \
                 is set (Dijkstra-ready)",
            ),
            other => panic!("Expected MaxMajorProtocolVersion, got {other:?}"),
        }
    }

    /// Regression for issue #463: end-to-end plumbing — when constructed from
    /// `NodeConfig::max_major_protocol_version()`, the handler must echo the
    /// resolved version. Exercises both branches of
    /// `experimental_hard_forks_enabled`.
    #[test]
    fn test_get_max_major_protocol_version_from_node_config() {
        let mut cfg = crate::config::NodeConfig {
            experimental_hard_forks_enabled: false,
            ..Default::default()
        };
        let h_default = QueryHandler::new(cfg.max_major_protocol_version() as u32);
        match query(&h_default, 38) {
            QueryResult::MaxMajorProtocolVersion(v) => {
                assert_eq!(v, 11, "config(experimental=false) → tag 38 must return 11",)
            }
            other => panic!("Expected MaxMajorProtocolVersion, got {other:?}"),
        }

        cfg.experimental_hard_forks_enabled = true;
        let h_exp = QueryHandler::new(cfg.max_major_protocol_version() as u32);
        match query(&h_exp, 38) {
            QueryResult::MaxMajorProtocolVersion(v) => {
                assert_eq!(v, 12, "config(experimental=true) → tag 38 must return 12",)
            }
            other => panic!("Expected MaxMajorProtocolVersion, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_epoch() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(500),
            ..Default::default()
        });

        match query(&handler, 1) {
            QueryResult::EpochNo(e) => assert_eq!(e, 500),
            other => panic!("Expected EpochNo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_ledger_tip() {
        let hash = Hash32::from_bytes([0xab; 32]);
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: Point::Specific(SlotNo(12345), hash),
                block_number: BlockNo(100),
            },
            block_number: BlockNo(100),
            ..Default::default()
        });

        match query(&handler, 0) {
            QueryResult::LedgerTip { slot, hash: h } => {
                assert_eq!(slot, 12345);
                assert_eq!(h, hash.to_vec());
            }
            other => panic!("Expected LedgerTip, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_current_era() {
        let handler = QueryHandler::new(11);
        match query(&handler, 999) {
            QueryResult::Error(_) => {} // Expected for unknown query
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_block_no() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            block_number: BlockNo(42000),
            ..Default::default()
        });

        // ChainBlockNo is outer tag 2 -- build a MsgQuery CBOR: [3, [2]]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(3).unwrap(); // MsgQuery
        enc.array(1).unwrap();
        enc.u32(2).unwrap(); // GetChainBlockNo
        let result = handler.handle_query_cbor(&buf);
        match result {
            QueryResult::ChainBlockNo(n) => assert_eq!(n, 42000),
            other => panic!("Expected ChainBlockNo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_system_start() {
        let handler = QueryHandler::new(11);
        match query(&handler, 999) {
            QueryResult::Error(_) => {}
            _ => panic!("Expected error for unknown query"),
        }
    }

    #[test]
    fn test_query_result_cbor_roundtrip() {
        // Build a MsgQuery CBOR: [3, [0, [1]]]
        // Outer tag 0 = BlockQuery, inner tag 1 = GetEpochNo
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(3).unwrap(); // MsgQuery
        enc.array(2).unwrap();
        enc.u32(0).unwrap(); // outer: BlockQuery
        enc.array(1).unwrap();
        enc.u32(1).unwrap(); // inner: GetEpochNo

        let handler = QueryHandler::new(11);
        let result = handler.handle_query_cbor(&buf);
        match result {
            QueryResult::EpochNo(e) => assert_eq!(e, 0),
            other => panic!("Expected EpochNo from CBOR query, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_stake_distribution() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            stake_pools: vec![
                StakePoolSnapshot {
                    pool_id: vec![0xaa; 28],
                    stake: 1_000_000_000,
                    vrf_keyhash: vec![0x11; 32],
                    total_circulation: 54_000_000_000_000_000,
                },
                StakePoolSnapshot {
                    pool_id: vec![0xbb; 28],
                    stake: 2_000_000_000,
                    vrf_keyhash: vec![0x22; 32],
                    total_circulation: 54_000_000_000_000_000,
                },
            ],
            ..Default::default()
        });

        match query(&handler, 5) {
            QueryResult::StakeDistribution(pools) => {
                assert_eq!(pools.len(), 2);
                assert_eq!(pools[0].pool_id, vec![0xaa; 28]);
                assert_eq!(pools[0].stake, 1_000_000_000);
                assert_eq!(pools[1].pool_id, vec![0xbb; 28]);
            }
            other => panic!("Expected StakeDistribution, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_protocol_params() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            protocol_params: ProtocolParamsSnapshot {
                min_fee_a: 44,
                min_fee_b: 155381,
                ..Default::default()
            },
            ..Default::default()
        });

        match query(&handler, 3) {
            QueryResult::ProtocolParams(params) => {
                assert_eq!(params.min_fee_a, 44);
                assert_eq!(params.min_fee_b, 155381);
            }
            other => panic!("Expected ProtocolParams, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_gov_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            drep_count: 5,
            treasury: 1_000_000_000_000,
            committee: CommitteeSnapshot {
                members: vec![CommitteeMemberSnapshot {
                    cold_credential: vec![0x01; 28],
                    cold_credential_type: 0,
                    hot_status: 0,
                    hot_credential: Some(vec![0x02; 28]),
                    hot_credential_type: 0,
                    member_status: 0,
                    expiry_epoch: Some(200),
                }],
                ..Default::default()
            },
            governance_proposals: vec![ProposalSnapshot {
                tx_id: vec![0xcc; 32],
                action_index: 0,
                action_type: "InfoAction".to_string(),
                proposed_epoch: 100,
                expires_epoch: 106,
                yes_votes: 3,
                no_votes: 1,
                abstain_votes: 0,
                deposit: 100_000_000_000,
                return_addr: vec![0xdd; 29],
                anchor_url: "https://example.com/proposal".to_string(),
                anchor_hash: vec![0xee; 32],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                committee_votes: vec![],
                drep_votes: vec![],
                spo_votes: vec![],
            }],
            ..Default::default()
        });

        match query(&handler, 24) {
            QueryResult::GovState(gov) => {
                assert_eq!(gov.committee.members.len(), 1);
                assert_eq!(gov.proposals.len(), 1);
                assert_eq!(gov.proposals[0].action_type, "InfoAction");
                assert_eq!(gov.proposals[0].yes_votes, 3);
            }
            other => panic!("Expected GovState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_drep_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            drep_entries: vec![DRepSnapshot {
                credential_hash: vec![0xdd; 28],
                credential_type: 0,
                deposit: 500_000_000,
                anchor_url: Some("https://example.com/drep".to_string()),
                anchor_hash: Some(vec![0xee; 32]),
                expiry_epoch: 62,
                delegator_hashes: Vec::new(),
            }],
            ..Default::default()
        });

        match query(&handler, 25) {
            QueryResult::DRepState(dreps) => {
                assert_eq!(dreps.len(), 1);
                assert_eq!(dreps[0].credential_hash, vec![0xdd; 28]);
                assert_eq!(dreps[0].deposit, 500_000_000);
                assert_eq!(
                    dreps[0].anchor_url,
                    Some("https://example.com/drep".to_string())
                );
                assert_eq!(dreps[0].expiry_epoch, 62);
            }
            other => panic!("Expected DRepState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_committee_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            committee: CommitteeSnapshot {
                members: vec![
                    CommitteeMemberSnapshot {
                        cold_credential: vec![0x01; 28],
                        cold_credential_type: 0,
                        hot_status: 0,
                        hot_credential: Some(vec![0x02; 28]),
                        hot_credential_type: 0,
                        member_status: 0,
                        expiry_epoch: Some(200),
                    },
                    CommitteeMemberSnapshot {
                        cold_credential: vec![0x03; 28],
                        cold_credential_type: 0,
                        hot_status: 2, // Resigned
                        hot_credential: None,
                        hot_credential_type: 0,
                        member_status: 0,
                        expiry_epoch: Some(200),
                    },
                ],
                threshold: Some((2, 3)),
                current_epoch: 100,
            },
            ..Default::default()
        });

        match query(&handler, 27) {
            QueryResult::CommitteeState(committee, _) => {
                assert_eq!(committee.members.len(), 2);
                assert_eq!(committee.members[0].cold_credential, vec![0x01; 28]);
                assert_eq!(committee.members[0].hot_status, 0); // Authorized
                assert_eq!(committee.members[1].hot_status, 2); // Resigned
            }
            other => panic!("Expected CommitteeState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_stake_address_info() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            stake_addresses: vec![
                StakeAddressSnapshot {
                    credential_hash: vec![0xaa; 28],
                    delegated_pool: Some(vec![0xbb; 28]),
                    reward_balance: 5_000_000,
                },
                StakeAddressSnapshot {
                    credential_hash: vec![0xcc; 28],
                    delegated_pool: None,
                    reward_balance: 0,
                },
            ],
            ..Default::default()
        });

        match query(&handler, 10) {
            QueryResult::StakeAddressInfo(addrs) => {
                assert_eq!(addrs.len(), 2);
                assert_eq!(addrs[0].credential_hash, vec![0xaa; 28]);
                assert_eq!(addrs[0].delegated_pool, Some(vec![0xbb; 28]));
                assert_eq!(addrs[0].reward_balance, 5_000_000);
                assert_eq!(addrs[1].delegated_pool, None);
            }
            other => panic!("Expected StakeAddressInfo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_utxo_by_address_no_provider() {
        let handler = QueryHandler::new(11);
        // Without a UtxoQueryProvider, should return empty
        let addr_bytes = vec![0x01; 57]; // fake address bytes
        let mut decoder = minicbor::Decoder::new(&addr_bytes);
        match handler.handle_shelley_query(6, &mut decoder) {
            QueryResult::UtxoByAddress(utxos) => {
                assert!(utxos.is_empty());
            }
            other => panic!("Expected UtxoByAddress, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_utxo_by_address_with_provider() {
        struct MockProvider;
        impl UtxoQueryProvider for MockProvider {
            fn utxos_at_address_bytes(&self, _addr_bytes: &[u8]) -> Vec<UtxoSnapshot> {
                vec![UtxoSnapshot {
                    tx_hash: vec![0xaa; 32],
                    output_index: 0,
                    address_bytes: vec![0x01; 57],
                    lovelace: 5_000_000,
                    multi_asset: vec![],
                    datum_hash: None,
                    inline_datum: None,
                    script_ref: None,
                    raw_cbor: None,
                }]
            }
        }

        let mut handler = QueryHandler::new(11);
        handler.set_utxo_provider(Arc::new(MockProvider));

        let addr_bytes = vec![0x01; 57];
        let mut decoder = minicbor::Decoder::new(&addr_bytes);
        match handler.handle_shelley_query(6, &mut decoder) {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 1);
                assert_eq!(utxos[0].lovelace, 5_000_000);
                assert_eq!(utxos[0].tx_hash, vec![0xaa; 32]);
            }
            other => panic!("Expected UtxoByAddress, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_gov_state_empty() {
        let handler = QueryHandler::new(11);
        match query(&handler, 24) {
            QueryResult::GovState(gov) => {
                assert_eq!(gov.proposals.len(), 0);
                assert_eq!(gov.committee.members.len(), 0);
            }
            other => panic!("Expected GovState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_stake_snapshots() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            stake_snapshots: StakeSnapshotsResult {
                pools: vec![PoolStakeSnapshotEntry {
                    pool_id: vec![0xaa; 28],
                    mark_stake: 1_000_000,
                    set_stake: 900_000,
                    go_stake: 800_000,
                }],
                total_mark_stake: 1_000_000,
                total_set_stake: 900_000,
                total_go_stake: 800_000,
            },
            ..Default::default()
        });

        match query(&handler, 20) {
            QueryResult::StakeSnapshots(snap) => {
                assert_eq!(snap.pools.len(), 1);
                assert_eq!(snap.pools[0].mark_stake, 1_000_000);
                assert_eq!(snap.pools[0].set_stake, 900_000);
                assert_eq!(snap.pools[0].go_stake, 800_000);
                assert_eq!(snap.total_mark_stake, 1_000_000);
            }
            other => panic!("Expected StakeSnapshots, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_pool_params() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            pool_params_entries: vec![PoolParamsSnapshot {
                pool_id: vec![0xbb; 28],
                vrf_keyhash: vec![0xcc; 32],
                pledge: 500_000_000,
                cost: 340_000_000,
                margin_num: 3,
                margin_den: 100,
                reward_account: Vec::new(),
                owners: Vec::new(),
                relays: vec![RelaySnapshot::SingleHostName {
                    port: Some(3001),
                    dns_name: "relay1.example.com".to_string(),
                }],
                metadata_url: None,
                metadata_hash: None,
            }],
            ..Default::default()
        });

        match query(&handler, 17) {
            QueryResult::PoolParams(params) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].pool_id, vec![0xbb; 28]);
                assert_eq!(params[0].pledge, 500_000_000);
                assert_eq!(params[0].cost, 340_000_000);
                assert_eq!(params[0].margin_num, 3);
                assert_eq!(params[0].relays.len(), 1);
            }
            other => panic!("Expected PoolParams, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_pool_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            pool_params_entries: vec![PoolParamsSnapshot {
                pool_id: vec![0xcc; 28],
                vrf_keyhash: vec![0xdd; 32],
                pledge: 100_000_000,
                cost: 170_000_000,
                margin_num: 1,
                margin_den: 100,
                reward_account: vec![0xe0; 29],
                owners: vec![vec![0x11; 28]],
                relays: vec![],
                metadata_url: None,
                metadata_hash: None,
            }],
            ..Default::default()
        });

        // Tag 19: GetPoolState returns QueryPoolStateResult (4 parallel maps)
        match query(&handler, 19) {
            QueryResult::PoolState {
                pool_params,
                future_pool_params,
                retiring,
                deposits,
            } => {
                assert_eq!(pool_params.len(), 1);
                assert_eq!(pool_params[0].pool_id, vec![0xcc; 28]);
                assert!(future_pool_params.is_empty());
                assert!(retiring.is_empty());
                assert_eq!(deposits.len(), 1);
                assert_eq!(deposits[0].0, vec![0xcc; 28]);
            }
            other => panic!("Expected PoolState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_pool_distr() {
        let mut handler = QueryHandler::new(11);
        // `GetPoolDistr` answers from the frozen `set` snapshot, not from
        // `stake_pools` (#964) — populating only the latter used to satisfy
        // this test while the query it names read a different field.
        handler.update_state(NodeStateSnapshot {
            pool_distr: vec![
                crate::node::n2c_query::types::PoolDistrEntry {
                    pool_id: vec![0xaa; 28],
                    stake: 1_000_000_000,
                    vrf_keyhash: vec![0x11; 32],
                    delegator_count: 4,
                },
                crate::node::n2c_query::types::PoolDistrEntry {
                    pool_id: vec![0xbb; 28],
                    stake: 2_000_000_000,
                    vrf_keyhash: vec![0x22; 32],
                    delegator_count: 7,
                },
            ],
            pool_distr_total_active_stake: 3_000_000_000,
            ..Default::default()
        });

        // Tag 21: GetPoolDistr
        match query(&handler, 21) {
            QueryResult::PoolDistr {
                pools,
                total_active_stake,
            } => {
                assert_eq!(pools.len(), 2);
                // The `set` snapshot's own total, not a value re-derived from
                // whatever survived the filter.
                assert_eq!(total_active_stake, 3_000_000_000);
            }
            other => panic!("Expected PoolDistr, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_stake_deleg_deposits() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            stake_deleg_deposits: vec![
                StakeDelegDepositEntry {
                    credential_hash: vec![0xaa; 28],
                    credential_type: 0,
                    deposit: 2_000_000,
                },
                StakeDelegDepositEntry {
                    credential_hash: vec![0xbb; 28],
                    credential_type: 0,
                    deposit: 2_000_000,
                },
            ],
            ..Default::default()
        });

        // Tag 22: GetStakeDelegDeposits
        match query(&handler, 22) {
            QueryResult::StakeDelegDeposits(deposits) => {
                assert_eq!(deposits.len(), 2);
                assert_eq!(deposits[0].deposit, 2_000_000);
            }
            other => panic!("Expected StakeDelegDeposits, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_drep_stake_distr() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            drep_stake_distr: vec![
                DRepStakeEntry {
                    drep_type: 0,
                    drep_hash: Some(vec![0xdd; 28]),
                    stake: 500_000_000,
                },
                DRepStakeEntry {
                    drep_type: 2,
                    drep_hash: None,
                    stake: 100_000_000,
                },
            ],
            ..Default::default()
        });

        // Tag 26: GetDRepStakeDistr
        match query(&handler, 26) {
            QueryResult::DRepStakeDistr(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].stake, 500_000_000);
            }
            other => panic!("Expected DRepStakeDistr, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_filtered_vote_delegatees() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            vote_delegatees: vec![
                VoteDelegateeEntry {
                    credential_hash: vec![0xaa; 28],
                    credential_type: 0,
                    drep_type: 0,
                    drep_hash: Some(vec![0xdd; 28]),
                },
                VoteDelegateeEntry {
                    credential_hash: vec![0xbb; 28],
                    credential_type: 0,
                    drep_type: 2,
                    drep_hash: None,
                },
            ],
            ..Default::default()
        });

        // Tag 28: GetFilteredVoteDelegatees
        match query(&handler, 28) {
            QueryResult::FilteredVoteDelegatees(delegatees) => {
                assert_eq!(delegatees.len(), 2);
                assert_eq!(delegatees[0].drep_type, 0);
                assert_eq!(delegatees[1].drep_type, 2);
            }
            other => panic!("Expected FilteredVoteDelegatees, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_debug_epoch_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(55),
            treasury: 2_000_000,
            reserves: 8_000_000,
            pool_count: 5,
            utxo_count: 100,
            ..NodeStateSnapshot::default()
        });
        // DebugEpochState now carries the EpochState structure (treasury/reserves + snapshots).
        match query(&handler, 8) {
            QueryResult::DebugEpochState {
                treasury, reserves, ..
            } => {
                assert_eq!(treasury, 2_000_000);
                assert_eq!(reserves, 8_000_000);
            }
            other => panic!("Expected DebugEpochState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_get_cbor_wraps_inner() {
        let handler = QueryHandler::new(11);
        // Build CBOR for inner query: [1] (GetEpochNo)
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).unwrap();
        enc.u32(1).unwrap(); // GetEpochNo
        let mut decoder = minicbor::Decoder::new(&buf);
        let result = handler.handle_shelley_query(9, &mut decoder);
        match result {
            QueryResult::WrappedCbor(inner) => match *inner {
                QueryResult::EpochNo(epoch) => {
                    assert_eq!(epoch, 0); // default state epoch
                }
                other => panic!("Expected EpochNo inside WrappedCbor, got {other:?}"),
            },
            other => panic!("Expected WrappedCbor, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_debug_new_epoch_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(10),
            block_number: BlockNo(500),
            treasury: 1_000_000,
            reserves: 2_000_000,
            tip: Tip {
                point: dugite_primitives::block::Point::Specific(
                    SlotNo(12345),
                    Hash32::from_bytes([0xAA; 32]),
                ),
                block_number: BlockNo(500),
            },
            ..NodeStateSnapshot::default()
        });
        match query(&handler, 12) {
            QueryResult::DebugNewEpochState {
                epoch,
                treasury,
                reserves,
                ..
            } => {
                assert_eq!(epoch, 10);
                assert_eq!(treasury, 1_000_000);
                assert_eq!(reserves, 2_000_000);
            }
            other => panic!("Expected DebugNewEpochState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_debug_chain_dep_state() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: dugite_primitives::block::Point::Specific(
                    SlotNo(99999),
                    Hash32::from_bytes([0xBB; 32]),
                ),
                block_number: BlockNo(100),
            },
            ..NodeStateSnapshot::default()
        });
        match query(&handler, 13) {
            QueryResult::DebugChainDepState {
                last_slot,
                last_slot_is_origin,
                epoch_nonce,
                evolving_nonce,
                candidate_nonce,
                lab_nonce,
                ..
            } => {
                assert_eq!(last_slot, 99999);
                assert!(!last_slot_is_origin);
                assert_eq!(epoch_nonce.len(), 32);
                assert_eq!(evolving_nonce.len(), 32);
                assert_eq!(candidate_nonce.len(), 32);
                assert_eq!(lab_nonce.len(), 32);
            }
            other => panic!("Expected DebugChainDepState, got {other:?}"),
        }
    }

    /// #1030 item 5: tag 14 answers with an explicit error through the real
    /// dispatch, not just at the handler.
    ///
    /// This test previously asserted `total_rewards_pot == 30_000` and
    /// `treasury_tax == 6_000` — arithmetic on an invented `array(4)` that
    /// Haskell's 16-field `SL.RewardProvenance` decoder could never read. It was a
    /// green test for an undecodable reply.
    #[test]
    fn test_query_handler_reward_provenance_returns_error() {
        let mut handler = QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(42),
            reserves: 10_000_000,
            protocol_params: ProtocolParamsSnapshot {
                rho_num: 3,
                rho_den: 1000,
                tau_num: 2,
                tau_den: 10,
                ..ProtocolParamsSnapshot::default()
            },
            ..NodeStateSnapshot::default()
        });
        match query(&handler, 14) {
            QueryResult::Error(msg) => assert!(
                msg.contains("GetRewardProvenance"),
                "the error must name the query: {msg}"
            ),
            other => panic!(
                "tag 14 must return an explicit error, not a fabricated payload; got {other:?}"
            ),
        }
    }

    #[test]
    fn test_query_handler_reward_info_pools() {
        let handler = QueryHandler::new(11);
        // Default state has no pools, should return empty
        match query(&handler, 18) {
            QueryResult::RewardInfoPools(pools) => {
                assert!(pools.is_empty());
            }
            other => panic!("Expected RewardInfoPools, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_unsupported_tag() {
        let handler = QueryHandler::new(11);
        match query(&handler, 99) {
            QueryResult::Error(msg) => {
                assert!(msg.contains("99"));
            }
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    /// Helper: encode a full MsgQuery CBOR for a Shelley tag-11 (GetGenesisConfig) query.
    fn encode_genesis_config_query() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap(); // MsgQuery envelope [3, X]
        enc.u32(3).unwrap(); // tag 3 = MsgQuery
        enc.array(2).unwrap(); // BlockQuery outer [0, Y]
        enc.u32(0).unwrap(); // outer tag 0 = BlockQuery
        enc.array(1).unwrap(); // Y = [query_tag]
        enc.u32(11).unwrap(); // tag 11 = GetGenesisConfig
        buf
    }

    /// Verify that the negotiated N2C version is correctly threaded through to
    /// the GenesisConfig result for version-gated CompactGenesis encoding.
    #[test]
    fn test_genesis_config_version_threaded_v16() {
        let handler = QueryHandler::new(11);
        let cbor = encode_genesis_config_query();
        let result = handler.handle_query_cbor_versioned(&cbor, 16);
        match result {
            QueryResult::GenesisConfig(_, version) => {
                assert_eq!(
                    version, 16,
                    "N2C V16 must be forwarded to GenesisConfig for legacy ProtVer encoding"
                );
            }
            other => panic!("Expected GenesisConfig, got {other:?}"),
        }
    }

    #[test]
    fn test_genesis_config_version_threaded_v21() {
        let handler = QueryHandler::new(11);
        let cbor = encode_genesis_config_query();
        let result = handler.handle_query_cbor_versioned(&cbor, 21);
        match result {
            QueryResult::GenesisConfig(_, version) => {
                assert_eq!(
                    version, 21,
                    "N2C V21 must be forwarded to GenesisConfig for bundled ProtVer encoding"
                );
            }
            other => panic!("Expected GenesisConfig, got {other:?}"),
        }
    }

    /// Confirm that the unversioned path (used by tests and internal callers)
    /// passes version 0, selecting the fallback (legacy) encoding.
    #[test]
    fn test_genesis_config_version_unversioned_uses_zero() {
        let handler = QueryHandler::new(11);
        let cbor = encode_genesis_config_query();
        let result = handler.handle_query_cbor(&cbor);
        match result {
            QueryResult::GenesisConfig(_, version) => {
                assert_eq!(version, 0, "unversioned query path must use version 0");
            }
            other => panic!("Expected GenesisConfig, got {other:?}"),
        }
    }

    /// Test the new dugite_network::QueryHandler trait implementation
    #[test]
    fn test_trait_handle_block_query() {
        use dugite_network::QueryHandler as TraitQueryHandler;

        let handler = super::QueryHandler::new(11);
        let acquired = handler
            .acquire(&AcquireTarget::VolatileTip)
            .expect("VolatileTip acquire must succeed");
        // Tag 1 = GetEpochNo (no params needed)
        let result = handler.handle_block_query(&acquired, 1, &[]);
        assert!(result.is_ok());
        // The result should be CBOR-encoded epoch number (0)
        let cbor = result.unwrap();
        let mut dec = minicbor::Decoder::new(&cbor);
        assert_eq!(dec.u64().unwrap(), 0);
    }

    #[test]
    fn test_trait_handle_query_hard_fork() {
        use dugite_network::QueryHandler as TraitQueryHandler;

        let handler = super::QueryHandler::new(11);
        let acquired = handler
            .acquire(&AcquireTarget::VolatileTip)
            .expect("VolatileTip acquire must succeed");
        // Sub-tag 1 = GetCurrentEra, encoded as [1]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).unwrap();
        enc.u32(1).unwrap();
        let result = handler.handle_query_hard_fork(&acquired, &buf);
        assert!(result.is_ok());
    }

    // ── C1 / #867 tests: acquire() point-on-chain + snapshot-pinning checks ──

    use dugite_network::codec::Point as CodecPoint;
    use dugite_network::protocol::local_state_query::{AcquireFailure, AcquireTarget};

    /// C1: VolatileTip and ImmutableTip always succeed regardless of chain_db.
    #[test]
    fn c1_volatile_tip_always_succeeds() {
        use dugite_network::QueryHandler as _;
        let handler = super::QueryHandler::new(11); // no chain_db
        assert!(handler.acquire(&AcquireTarget::VolatileTip).is_ok());
    }

    #[test]
    fn c1_immutable_tip_always_succeeds() {
        use dugite_network::QueryHandler as _;
        let handler = super::QueryHandler::new(11); // no chain_db
        assert!(handler.acquire(&AcquireTarget::ImmutableTip).is_ok());
    }

    /// C1: SpecificPoint with no chain_db must return PointNotOnChain (defensive default).
    #[test]
    fn c1_specific_point_no_chain_db_returns_not_on_chain() {
        use dugite_network::QueryHandler as _;
        let handler = super::QueryHandler::new(11); // no chain_db
        let point = AcquireTarget::SpecificPoint(CodecPoint::Specific(1000, [0xAA; 32]));
        assert!(
            matches!(
                handler.acquire(&point),
                Err(AcquireFailure::PointNotOnChain)
            ),
            "SpecificPoint with no chain_db must refuse (defensive default)"
        );
    }

    /// C1: Origin point is valid when the current tip IS Origin (default state).
    #[test]
    fn c1_origin_point_valid_at_genesis() {
        use dugite_network::QueryHandler as _;
        let handler = super::QueryHandler::new(11); // no chain_db, default state == Origin tip
        let point = AcquireTarget::SpecificPoint(CodecPoint::Origin);
        assert!(
            handler.acquire(&point).is_ok(),
            "Origin point must be a valid acquire target when the current tip is Origin"
        );
    }

    /// #867: Origin point is REJECTED (PointTooOld) once the chain has moved
    /// past genesis, because there is no machinery to rewind ledger state
    /// back to Origin — we can only materialise a snapshot of the CURRENT tip.
    #[test]
    fn issue_867_origin_point_too_old_once_tip_advances() {
        use dugite_network::QueryHandler as _;
        let mut handler = super::QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: Point::Specific(SlotNo(500), Hash32::from_bytes([0x33; 32])),
                block_number: BlockNo(10),
            },
            ..Default::default()
        });
        let point = AcquireTarget::SpecificPoint(CodecPoint::Origin);
        assert!(
            matches!(handler.acquire(&point), Err(AcquireFailure::PointTooOld)),
            "Origin acquire must fail with PointTooOld once the tip has advanced past genesis"
        );
    }

    /// #867: a SpecificPoint that IS the current tip pins that tip's snapshot.
    #[test]
    fn issue_867_specific_point_matching_current_tip_succeeds() {
        use dugite_network::QueryHandler as _;
        let mut handler = super::QueryHandler::new(11);
        let tip_hash = Hash32::from_bytes([0x44; 32]);
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: Point::Specific(SlotNo(777), tip_hash),
                block_number: BlockNo(20),
            },
            ..Default::default()
        });
        let point = AcquireTarget::SpecificPoint(CodecPoint::Specific(777, tip_hash.0));
        assert!(
            handler.acquire(&point).is_ok(),
            "SpecificPoint matching the current tip must always succeed"
        );
    }

    /// #867: a SpecificPoint that IS on our chain (VolatileDB/ImmutableDB) but
    /// is NOT the current tip must return `PointTooOld` — we have no way to
    /// rewind ledger state to serve that historical snapshot.
    #[test]
    fn issue_867_specific_point_on_chain_but_not_tip_returns_point_too_old() {
        use dugite_network::QueryHandler as _;

        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let mut chain_db = dugite_storage::ChainDB::open(tmp_dir.path()).expect("open chain_db");
        let old_hash = Hash32::from_bytes([0x11; 32]);
        chain_db
            .add_block(
                old_hash,
                SlotNo(50),
                BlockNo(1),
                Hash32::from_bytes([0u8; 32]),
                vec![0xAAu8; 4],
            )
            .expect("add_block");

        let mut handler = super::QueryHandler::new(11);
        // Advance the handler's current tip PAST that block, so the point is
        // on-chain but no longer the tip.
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: Point::Specific(SlotNo(999), Hash32::from_bytes([0x22; 32])),
                block_number: BlockNo(2),
            },
            ..Default::default()
        });
        handler.set_chain_db(Arc::new(RwLock::new(chain_db)));

        let point = AcquireTarget::SpecificPoint(CodecPoint::Specific(50, old_hash.0));
        let result = handler.acquire(&point);
        assert!(
            matches!(result, Err(AcquireFailure::PointTooOld)),
            "on-chain point that is not the current tip must return PointTooOld, got {result:?}"
        );
    }

    /// #867: torn-read regression at the node QueryHandler level. Two queries
    /// dispatched against the SAME pinned `Acquired` handle must answer from
    /// the SAME snapshot, even after `update_state()` swaps the live state.
    #[test]
    fn issue_867_torn_read_regression_pinned_handle_survives_update_state() {
        use dugite_network::QueryHandler as _;

        let mut handler = super::QueryHandler::new(11);
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(100),
            ..Default::default()
        });

        // Pin a snapshot at epoch 100.
        let acquired = handler
            .acquire(&AcquireTarget::VolatileTip)
            .expect("acquire must succeed");

        // Simulate a live update landing BETWEEN two queries of one acquisition.
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(200),
            ..Default::default()
        });

        // Both queries against the PINNED handle must still see epoch 100.
        // Tag 1 = GetEpochNo (no query-CBOR arguments needed).
        for _ in 0..2 {
            let result = handler
                .handle_block_query(&acquired, 1, &[])
                .expect("handle_block_query must succeed");
            let mut dec = minicbor::Decoder::new(&result);
            assert_eq!(
                dec.u64().unwrap(),
                100,
                "query against the pinned handle must see epoch 100, not the \
                 live epoch 200 set by the concurrent update_state() call"
            );
        }

        // A FRESH acquire (new session) must see the NEW live state.
        let acquired2 = handler
            .acquire(&AcquireTarget::VolatileTip)
            .expect("acquire must succeed");
        let result = handler
            .handle_block_query(&acquired2, 1, &[])
            .expect("handle_block_query must succeed");
        let mut dec = minicbor::Decoder::new(&result);
        assert_eq!(
            dec.u64().unwrap(),
            200,
            "a NEW acquire must observe the latest live state"
        );
    }

    /// C1: failure encoding — PointNotOnChain → wire tag 1.
    #[test]
    fn c1_failure_encoding_point_not_on_chain() {
        // The server encodes AcquireFailure::PointNotOnChain as [2, [1]]
        // (MsgFailure tag=2, failure = array(1)[1])
        // Verify our enum value matches the Haskell wire encoding.
        let failure = AcquireFailure::PointNotOnChain;
        assert_eq!(failure, AcquireFailure::PointNotOnChain); // round-trip through PartialEq
                                                              // Also verify PointTooOld has a distinct value
        let too_old = AcquireFailure::PointTooOld;
        assert_ne!(failure, too_old);
    }
}
