//! UTxO-related query handlers (tags 6, 7, 15).

use std::sync::Arc;
use tracing::debug;

use super::types::{NodeStateSnapshot, QueryResult, UtxoQueryProvider};
use dugite_network::UtxoViewPoint;

/// Handle GetUTxOByAddress (tag 6).
///
/// Argument: tag(258) Set<Address> or single address bytes
pub(crate) fn handle_utxo_by_address(
    state: &NodeStateSnapshot,
    utxo_provider: &Option<Arc<dyn UtxoQueryProvider>>,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetUTxOByAddress");
    let mut addresses: Vec<Vec<u8>> = Vec::new();
    let pos = decoder.position();
    // Try single bare address bytes first (most common case)
    if let Ok(bytes) = decoder.bytes() {
        addresses.push(bytes.to_vec());
    } else {
        // Try tag(258) Set<Address>
        decoder.set_position(pos);
        let _ = decoder.tag(); // consume tag(258)
        if let Ok(Some(n)) = decoder.array() {
            for _ in 0..n {
                if let Ok(bytes) = decoder.bytes() {
                    addresses.push(bytes.to_vec());
                }
            }
        }
    }
    // Fallback: use remaining decoder bytes as raw address
    if addresses.is_empty() {
        decoder.set_position(pos);
        let remaining = &decoder.input()[pos..];
        if !remaining.is_empty() {
            addresses.push(remaining.to_vec());
        }
    }
    if let Some(provider) = utxo_provider {
        let at = acquired_point(state);
        let mut all_utxos = Vec::new();
        for addr in &addresses {
            match provider.utxos_at_address_bytes(addr, &at) {
                Some(found) => all_utxos.extend(found),
                None => return unpinnable("GetUTxOByAddress", &at),
            }
        }
        QueryResult::UtxoByAddress(all_utxos)
    } else {
        QueryResult::UtxoByAddress(vec![])
    }
}

/// The chain point this acquisition pinned.
///
/// Taken from the ACQUIRED snapshot, which `handle_query` supplies as the
/// shadow handler's `state` — not from the live ledger. That is the whole
/// point of #1068: every query in one `MsgAcquire..MsgRelease` session must
/// answer from the same ledger point.
pub(crate) fn acquired_point(state: &NodeStateSnapshot) -> UtxoViewPoint {
    match &state.tip.point {
        dugite_primitives::block::Point::Origin => UtxoViewPoint::Origin,
        dugite_primitives::block::Point::Specific(slot, hash) => UtxoViewPoint::Specific {
            slot: slot.0,
            hash: *hash.as_bytes(),
        },
    }
}

/// The acquired point is no longer reconstructible.
///
/// Upstream cannot reach this state — its `LedgerDB` retains the acquired
/// state for the acquisition's life — and neither can dugite in practice,
/// since `acquire` only pins the current tip and the volatile window is `k`
/// blocks. Answering from the live set instead would be precisely the silent
/// torn read #1068 removes, so this refuses rather than guesses.
fn unpinnable(query: &str, at: &UtxoViewPoint) -> QueryResult {
    tracing::warn!(
        query,
        ?at,
        "UTxO query: the acquired chain point has fallen out of the volatile \
         window; refusing to answer from a different ledger point"
    );
    QueryResult::Error(format!(
        "{query}: the acquired chain point is no longer available; \
         re-acquire and retry"
    ))
}

/// Hard cap on the number of UTxO entries returned by `GetUTxOWhole` (C2 fix).
///
/// `GetUTxOWhole` materializes the entire UTxO set under a blocking read lock.
/// At mainnet scale (~10M entries × ~100 bytes) this can allocate ~1 GB per
/// call and hold the ledger `RwLock` for seconds, stalling block validation and
/// forging. The cap rejects queries that would exceed this limit, matching the
/// defensive behaviour of Haskell cardano-node which gates access by permissions.
///
/// 500,000 entries × ~200 bytes CBOR ≈ 100 MB — large but bounded.
/// Operators running indexers that need the full UTxO should use a dedicated
/// db-sync instance or the Mithril snapshot protocol instead.
pub const MAX_UTXO_QUERY_ENTRIES: usize = 500_000;

/// Handle GetUTxOWhole (tag 7).
///
/// Returns the entire UTxO set as a CBOR map. Used by chain indexers
/// (db-sync, Ogmios, Oura) to bootstrap their UTxO state.
///
/// C2: enforces `MAX_UTXO_QUERY_ENTRIES` to prevent a single N2C client from
/// holding the ledger read lock for multiple seconds at mainnet scale.
pub(crate) fn handle_utxo_whole(
    state: &NodeStateSnapshot,
    utxo_provider: &Option<Arc<dyn UtxoQueryProvider>>,
) -> QueryResult {
    debug!("Query: GetUTxOWhole");
    if let Some(provider) = utxo_provider {
        let at = acquired_point(state);
        let Some(entries) = provider.utxos_all(&at) else {
            return unpinnable("GetUTxOWhole", &at);
        };
        if entries.len() > MAX_UTXO_QUERY_ENTRIES {
            // Return an error result rather than materializing gigabytes of data.
            // Clients that genuinely need the full UTxO set should use a dedicated
            // indexer (db-sync, Mithril) rather than the N2C query protocol.
            return QueryResult::Error(format!(
                "GetUTxOWhole: UTxO set too large ({} entries, max {}); \
                 use a dedicated indexer for full UTxO access",
                entries.len(),
                MAX_UTXO_QUERY_ENTRIES,
            ));
        }
        QueryResult::UtxoByAddress(entries)
    } else {
        QueryResult::UtxoByAddress(vec![])
    }
}

/// Handle GetUTxOByTxIn (tag 15).
///
/// Argument: Set<TxIn> where TxIn = [tx_hash, output_index]
pub(crate) fn handle_utxo_by_txin(
    state: &NodeStateSnapshot,
    utxo_provider: &Option<Arc<dyn UtxoQueryProvider>>,
    decoder: &mut minicbor::Decoder<'_>,
) -> QueryResult {
    debug!("Query: GetUTxOByTxIn");
    let mut inputs = Vec::new();
    // Try tag(258) Set wrapper first, fall back to bare array
    let pos = decoder.position();
    if decoder.tag().is_err() {
        decoder.set_position(pos);
    }
    if let Ok(Some(n)) = decoder.array() {
        for _ in 0..n {
            if let Ok(Some(_)) = decoder.array() {
                if let (Ok(tx_hash), Ok(idx)) = (decoder.bytes(), decoder.u32()) {
                    inputs.push((tx_hash.to_vec(), idx));
                } else {
                    debug!("Skipping malformed TxIn entry in GetUTxOByTxIn");
                }
            }
        }
    }
    if let Some(provider) = utxo_provider {
        let at = acquired_point(state);
        match provider.utxos_by_tx_inputs(&inputs, &at) {
            Some(found) => QueryResult::UtxoByAddress(found),
            None => unpinnable("GetUTxOByTxIn", &at),
        }
    } else {
        QueryResult::UtxoByAddress(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockUtxoProvider {
        utxos: Vec<super::super::types::UtxoSnapshot>,
    }

    impl UtxoQueryProvider for MockUtxoProvider {
        fn utxos_at_address_bytes(
            &self,
            addr_bytes: &[u8],
            _at: &UtxoViewPoint,
        ) -> Option<Vec<super::super::types::UtxoSnapshot>> {
            Some(
                self.utxos
                    .iter()
                    .filter(|u| u.address_bytes == addr_bytes)
                    .cloned()
                    .collect(),
            )
        }

        fn utxos_by_tx_inputs(
            &self,
            inputs: &[(Vec<u8>, u32)],
            _at: &UtxoViewPoint,
        ) -> Option<Vec<super::super::types::UtxoSnapshot>> {
            Some(
                self.utxos
                    .iter()
                    .filter(|u| {
                        inputs
                            .iter()
                            .any(|(h, i)| h == &u.tx_hash && *i == u.output_index)
                    })
                    .cloned()
                    .collect(),
            )
        }

        fn utxos_all(&self, _at: &UtxoViewPoint) -> Option<Vec<super::super::types::UtxoSnapshot>> {
            Some(self.utxos.clone())
        }
    }

    fn make_utxo(
        tx_hash: Vec<u8>,
        index: u32,
        addr: Vec<u8>,
        lovelace: u64,
    ) -> super::super::types::UtxoSnapshot {
        super::super::types::UtxoSnapshot {
            tx_hash,
            output_index: index,
            address_bytes: addr,
            lovelace,
            multi_asset: vec![],
            datum_hash: None,
            inline_datum: None,
            script_ref: None,
            raw_cbor: None,
        }
    }

    fn make_provider(
        utxos: Vec<super::super::types::UtxoSnapshot>,
    ) -> Option<Arc<dyn UtxoQueryProvider>> {
        Some(Arc::new(MockUtxoProvider { utxos }))
    }

    #[test]
    fn test_utxo_by_address_single() {
        let addr = vec![0x61; 29]; // enterprise address
        let provider = make_provider(vec![
            make_utxo(vec![1u8; 32], 0, addr.clone(), 5_000_000),
            make_utxo(vec![2u8; 32], 1, vec![0x62; 29], 3_000_000),
        ]);
        // Encode single address bytes
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).bytes(&addr).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let state = super::super::types::NodeStateSnapshot::default();
        let result = handle_utxo_by_address(&state, &provider, &mut dec);
        match result {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 1);
                assert_eq!(utxos[0].lovelace, 5_000_000);
            }
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_address_set() {
        let addr1 = vec![0x61; 29];
        let addr2 = vec![0x62; 29];
        let provider = make_provider(vec![
            make_utxo(vec![1u8; 32], 0, addr1.clone(), 5_000_000),
            make_utxo(vec![2u8; 32], 0, addr2.clone(), 3_000_000),
        ]);
        // Encode tag(258) Set<Address>
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(2).ok();
        enc.bytes(&addr1).ok();
        enc.bytes(&addr2).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let state = super::super::types::NodeStateSnapshot::default();
        let result = handle_utxo_by_address(&state, &provider, &mut dec);
        match result {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 2);
            }
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_address_no_provider() {
        let addr = vec![0x61; 29];
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).bytes(&addr).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let state = super::super::types::NodeStateSnapshot::default();
        let result = handle_utxo_by_address(&state, &None, &mut dec);
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_address_empty_result() {
        let addr = vec![0xFF; 29]; // address not in set
        let provider = make_provider(vec![make_utxo(vec![1u8; 32], 0, vec![0x61; 29], 5_000_000)]);
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).bytes(&addr).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let state = super::super::types::NodeStateSnapshot::default();
        let result = handle_utxo_by_address(&state, &provider, &mut dec);
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_whole_no_provider() {
        let result = handle_utxo_whole(&super::super::types::NodeStateSnapshot::default(), &None);
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_whole_returns_all() {
        let provider = make_provider(vec![
            make_utxo(vec![1u8; 32], 0, vec![0x61; 29], 5_000_000),
            make_utxo(vec![2u8; 32], 1, vec![0x62; 29], 3_000_000),
        ]);
        let result = handle_utxo_whole(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 2);
            }
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_whole_empty_store() {
        let provider = make_provider(vec![]);
        let result = handle_utxo_whole(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    // ── C2 tests: GetUTxOWhole entry cap ──────────────────────────────────────

    /// C2: UTxO set at exactly the limit must succeed.
    #[test]
    fn c2_utxo_whole_at_limit_succeeds() {
        let utxos: Vec<_> = (0..MAX_UTXO_QUERY_ENTRIES)
            .map(|i| {
                let mut hash = vec![0u8; 32];
                hash[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                make_utxo(hash, 0, vec![0x61; 29], 1_000_000)
            })
            .collect();
        let provider = make_provider(utxos);
        let result = handle_utxo_whole(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
        );
        match result {
            QueryResult::UtxoByAddress(entries) => {
                assert_eq!(entries.len(), MAX_UTXO_QUERY_ENTRIES);
            }
            QueryResult::Error(msg) => panic!("Expected success at limit, got error: {msg}"),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    /// C2: UTxO set one over the limit must return Error.
    #[test]
    fn c2_utxo_whole_over_limit_returns_error() {
        let utxos: Vec<_> = (0..=MAX_UTXO_QUERY_ENTRIES)
            .map(|i| {
                let mut hash = vec![0u8; 32];
                hash[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                make_utxo(hash, 0, vec![0x61; 29], 1_000_000)
            })
            .collect();
        let provider = make_provider(utxos);
        let result = handle_utxo_whole(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
        );
        match result {
            QueryResult::Error(msg) => {
                assert!(
                    msg.contains("too large"),
                    "Error message should mention size: {msg}"
                );
                assert!(
                    !msg.contains('{') && !msg.contains('}'),
                    "Error must not expose internal formatting: {msg}"
                );
            }
            QueryResult::UtxoByAddress(_) => {
                panic!("Expected Error for oversized UTxO set, got UtxoByAddress")
            }
            _ => panic!("Expected Error variant"),
        }
    }

    #[test]
    fn test_utxo_by_txin_single() {
        let tx_hash = vec![0xAA; 32];
        let provider = make_provider(vec![
            make_utxo(tx_hash.clone(), 0, vec![0x61; 29], 5_000_000),
            make_utxo(vec![0xBB; 32], 1, vec![0x62; 29], 3_000_000),
        ]);
        // Encode array(1) [ [tx_hash, 0] ]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).ok();
        enc.array(2).ok();
        enc.bytes(&tx_hash).ok();
        enc.u32(0).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let result = handle_utxo_by_txin(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
            &mut dec,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 1);
                assert_eq!(utxos[0].tx_hash, tx_hash);
            }
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_txin_no_provider() {
        let mut buf = Vec::new();
        minicbor::Encoder::new(&mut buf).array(0).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let result = handle_utxo_by_txin(
            &super::super::types::NodeStateSnapshot::default(),
            &None,
            &mut dec,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_txin_not_found() {
        let provider = make_provider(vec![make_utxo(
            vec![0xAA; 32],
            0,
            vec![0x61; 29],
            5_000_000,
        )]);
        // Query for a TxIn that doesn't exist
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).ok();
        enc.array(2).ok();
        enc.bytes(&[0xFF; 32]).ok();
        enc.u32(99).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let result = handle_utxo_by_txin(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
            &mut dec,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => assert!(utxos.is_empty()),
            _ => panic!("Expected UtxoByAddress"),
        }
    }

    #[test]
    fn test_utxo_by_txin_multiple() {
        let tx1 = vec![0xAA; 32];
        let tx2 = vec![0xBB; 32];
        let provider = make_provider(vec![
            make_utxo(tx1.clone(), 0, vec![0x61; 29], 5_000_000),
            make_utxo(tx2.clone(), 1, vec![0x62; 29], 3_000_000),
            make_utxo(vec![0xCC; 32], 2, vec![0x63; 29], 1_000_000),
        ]);
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).ok();
        enc.array(2).ok();
        enc.bytes(&tx1).ok();
        enc.u32(0).ok();
        enc.array(2).ok();
        enc.bytes(&tx2).ok();
        enc.u32(1).ok();
        let mut dec = minicbor::Decoder::new(&buf);

        let result = handle_utxo_by_txin(
            &super::super::types::NodeStateSnapshot::default(),
            &provider,
            &mut dec,
        );
        match result {
            QueryResult::UtxoByAddress(utxos) => {
                assert_eq!(utxos.len(), 2);
            }
            _ => panic!("Expected UtxoByAddress"),
        }
    }
}
