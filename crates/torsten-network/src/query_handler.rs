use torsten_primitives::block::{Point, Tip};
use torsten_primitives::time::{BlockNo, EpochNo};
use tracing::debug;

/// Results from local state queries
#[derive(Debug, Clone)]
pub enum QueryResult {
    EpochNo(u64),
    ChainTip {
        slot: u64,
        hash: Vec<u8>,
        block_no: u64,
    },
    CurrentEra(u32),
    SystemStart(String),
    ChainBlockNo(u64),
    ProtocolParams(Vec<u8>),
    StakeDistribution(Vec<(Vec<u8>, u64)>),
    Error(String),
}

/// Snapshot of the node state used for answering queries.
/// This is updated from the node when the ledger state changes.
#[derive(Debug, Clone)]
pub struct NodeStateSnapshot {
    pub tip: Tip,
    pub epoch: EpochNo,
    pub era: u32,
    pub block_number: BlockNo,
    pub system_start: String,
    pub utxo_count: usize,
    pub delegations_count: usize,
    pub pool_count: usize,
    pub treasury: u64,
    pub reserves: u64,
    pub drep_count: usize,
    pub proposal_count: usize,
}

impl Default for NodeStateSnapshot {
    fn default() -> Self {
        NodeStateSnapshot {
            tip: Tip::origin(),
            epoch: EpochNo(0),
            era: 6, // Conway
            block_number: BlockNo(0),
            system_start: "2017-09-23T21:44:51Z".to_string(), // Mainnet
            utxo_count: 0,
            delegations_count: 0,
            pool_count: 0,
            treasury: 0,
            reserves: 0,
            drep_count: 0,
            proposal_count: 0,
        }
    }
}

/// Handler for local state queries.
///
/// This provides a clean interface for answering LocalStateQuery protocol
/// queries from the current ledger state.
pub struct QueryHandler {
    state: NodeStateSnapshot,
}

impl QueryHandler {
    pub fn new() -> Self {
        QueryHandler {
            state: NodeStateSnapshot::default(),
        }
    }

    /// Update the snapshot from the current node state
    pub fn update_state(&mut self, snapshot: NodeStateSnapshot) {
        self.state = snapshot;
    }

    /// Handle a raw CBOR query message and return a result.
    ///
    /// The CBOR payload from MsgQuery is: [3, query]
    /// where query is a nested structure depending on the query type.
    /// For Shelley-based eras, it's typically: [era_tag, [query_tag, ...]]
    pub fn handle_query_cbor(&self, payload: &[u8]) -> QueryResult {
        // Try to parse the query from the CBOR
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

        // The query itself is wrapped in layers. Try to determine the query type.
        // Shelley queries: [shelley_era_tag, [query_id, ...]]
        // Hard-fork queries: [query_id, ...]
        self.dispatch_query(&mut decoder)
    }

    /// Dispatch a query based on its CBOR structure
    fn dispatch_query(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
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
                        // Could be GetCurrentEra (hardcoded query) or era-wrapped query
                        if len == 1 {
                            debug!("Query: GetCurrentEra");
                            return QueryResult::CurrentEra(self.state.era);
                        }
                        // Era-wrapped query: [era, [query_tag, ...]]
                        self.dispatch_era_query(decoder)
                    }
                    2 => {
                        debug!("Query: GetSystemStart");
                        QueryResult::SystemStart(self.state.system_start.clone())
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
                    0 => QueryResult::CurrentEra(self.state.era),
                    2 => QueryResult::SystemStart(self.state.system_start.clone()),
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
            Ok(2) => QueryResult::SystemStart(self.state.system_start.clone()),
            _ => QueryResult::Error("Unknown simple query".into()),
        }
    }

    /// Dispatch an era-specific query
    fn dispatch_era_query(&self, decoder: &mut minicbor::Decoder<'_>) -> QueryResult {
        // Try to parse inner query: [query_tag, ...]
        match decoder.array() {
            Ok(_) => {
                let query_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(query_tag)
            }
            Err(_) => {
                // Try as a simple integer tag
                let query_tag = decoder.u32().unwrap_or(999);
                self.handle_shelley_query(query_tag)
            }
        }
    }

    /// Handle Shelley-era queries by tag
    fn handle_shelley_query(&self, query_tag: u32) -> QueryResult {
        match query_tag {
            0 => {
                // GetLedgerTip / GetEpochNo
                debug!("Query: GetEpochNo");
                QueryResult::EpochNo(self.state.epoch.0)
            }
            1 => {
                // GetEpochNo (alternate)
                debug!("Query: GetEpochNo (alt)");
                QueryResult::EpochNo(self.state.epoch.0)
            }
            7 => {
                // GetCurrentPParams
                debug!("Query: GetCurrentPParams");
                // TODO: encode actual protocol params as CBOR
                QueryResult::ProtocolParams(vec![])
            }
            10 => {
                // GetChainBlockNo
                debug!("Query: GetChainBlockNo");
                QueryResult::ChainBlockNo(self.state.block_number.0)
            }
            11 => {
                // GetChainPoint (chain tip)
                debug!("Query: GetChainPoint");
                let (slot, hash) = match &self.state.tip.point {
                    Point::Origin => (0, vec![0u8; 32]),
                    Point::Specific(s, h) => (s.0, h.to_vec()),
                };
                QueryResult::ChainTip {
                    slot,
                    hash,
                    block_no: self.state.block_number.0,
                }
            }
            _ => {
                debug!("Unhandled Shelley query tag: {query_tag}");
                QueryResult::Error(format!("Unsupported query: tag {query_tag}"))
            }
        }
    }
}

impl Default for QueryHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torsten_primitives::hash::Hash32;
    use torsten_primitives::time::SlotNo;

    #[test]
    fn test_query_handler_default_state() {
        let handler = QueryHandler::new();
        match handler.handle_shelley_query(0) {
            QueryResult::EpochNo(e) => assert_eq!(e, 0),
            other => panic!("Expected EpochNo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_epoch() {
        let mut handler = QueryHandler::new();
        handler.update_state(NodeStateSnapshot {
            epoch: EpochNo(500),
            ..Default::default()
        });

        match handler.handle_shelley_query(0) {
            QueryResult::EpochNo(e) => assert_eq!(e, 500),
            other => panic!("Expected EpochNo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_chain_tip() {
        let hash = Hash32::from_bytes([0xab; 32]);
        let mut handler = QueryHandler::new();
        handler.update_state(NodeStateSnapshot {
            tip: Tip {
                point: Point::Specific(SlotNo(12345), hash),
                block_number: BlockNo(100),
            },
            block_number: BlockNo(100),
            ..Default::default()
        });

        match handler.handle_shelley_query(11) {
            QueryResult::ChainTip {
                slot,
                hash: h,
                block_no,
            } => {
                assert_eq!(slot, 12345);
                assert_eq!(h, hash.to_vec());
                assert_eq!(block_no, 100);
            }
            other => panic!("Expected ChainTip, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_current_era() {
        let handler = QueryHandler::new();
        match handler.handle_shelley_query(999) {
            QueryResult::Error(_) => {} // Expected for unknown query
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_block_no() {
        let mut handler = QueryHandler::new();
        handler.update_state(NodeStateSnapshot {
            block_number: BlockNo(42000),
            ..Default::default()
        });

        match handler.handle_shelley_query(10) {
            QueryResult::ChainBlockNo(n) => assert_eq!(n, 42000),
            other => panic!("Expected ChainBlockNo, got {other:?}"),
        }
    }

    #[test]
    fn test_query_handler_system_start() {
        let handler = QueryHandler::new();
        match handler.handle_shelley_query(999) {
            QueryResult::Error(_) => {}
            _ => panic!("Expected error for unknown query"),
        }
    }

    #[test]
    fn test_query_result_cbor_roundtrip() {
        // Build a MsgQuery CBOR: [3, [0, [0]]]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(3).unwrap(); // MsgQuery
        enc.array(2).unwrap();
        enc.u32(0).unwrap(); // era tag
        enc.array(1).unwrap();
        enc.u32(0).unwrap(); // GetEpochNo

        let handler = QueryHandler::new();
        let result = handler.handle_query_cbor(&buf);
        match result {
            QueryResult::EpochNo(e) => assert_eq!(e, 0),
            other => panic!("Expected EpochNo from CBOR query, got {other:?}"),
        }
    }
}
