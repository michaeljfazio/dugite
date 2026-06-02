//! Offline UTxO-backend snapshot converter — the dugite mirror of
//! cardano-node's `snapshot-converter` (`ouroboros-consensus-cardano/app/
//! snapshot-converter.hs`).
//!
//! Converts a ledger snapshot between the in-memory and LSM UTxO backends
//! WITHOUT a full chain replay. The non-UTxO ledger state is backend-agnostic
//! and is carried across verbatim; only the UTxO *tables* are re-encoded into
//! the target backend's representation:
//!
//! - **→ mem**: stream the source LSM `ledger` snapshot into an in-memory
//!   `UtxoSet` (the snapshot bincode then carries the UTxO inline).
//! - **→ lsm**: stream the source in-memory `UtxoSet` into a fresh target LSM
//!   store + `ledger` snapshot (the bincode then carries an empty `utxo_set`).
//!
//! Because both backends hold the *identical* UTxO set, the converted snapshot
//! is observationally equivalent (same set → same ledger → same hashes).
//! Haskell asserts this via its `InMemV2 ≡ LSM` state-machine property tests;
//! dugite asserts it via the round-trip equivalence test
//! `convert_roundtrip_mem_lsm_mem_preserves_utxo_set` (below).
//!
//! The source is read through `UtxoStore::open_from_snapshot(.., "ledger")`,
//! which opens the consistent point-in-time LSM snapshot (hard-linked SSTs) —
//! so a conversion is safe to run even while the source node is live.

use std::path::Path;

use anyhow::{anyhow, Result};
use dugite_ledger::{LedgerState, SnapshotBackend, UtxoSet, UtxoStore};
use dugite_primitives::transaction::{TransactionInput, TransactionOutput};
use tracing::info;

/// Outcome of a conversion — surfaced for logging and test assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertStats {
    pub source_backend: SnapshotBackend,
    pub target_backend: SnapshotBackend,
    pub utxo_count: u64,
    pub slot: u64,
}

/// Detect the backend a snapshot was written with: prefer the `.meta.json`
/// sidecar tag, falling back to inference from the loaded state + db layout
/// (back-compat for pre-meta snapshots, e.g. an existing `db-mainnet`).
pub fn detect_source_backend(
    snapshot_bin: &Path,
    state: &LedgerState,
    source_db: &Path,
) -> Option<SnapshotBackend> {
    if let Some(meta) = dugite_ledger::SnapshotMeta::load(snapshot_bin) {
        if let Some(b) = SnapshotBackend::from_tag(&meta.backend) {
            return Some(b);
        }
    }
    dugite_ledger::infer_backend_from_snapshot(state, source_db)
}

/// Open a source LSM `ledger` snapshot and stream every `(TxIn, TxOut)` into
/// `sink`. Returns the number of entries streamed.
fn stream_source_lsm<F: FnMut(TransactionInput, TransactionOutput)>(
    source_db: &Path,
    mut sink: F,
) -> Result<u64> {
    let store_path = source_db.join("utxo-store");
    let store = UtxoStore::open_from_snapshot(&store_path, "ledger").map_err(|e| {
        anyhow!(
            "open source LSM `ledger` snapshot at {}: {e} \
             (a snapshot is written on each epoch-boundary save)",
            store_path.display()
        )
    })?;
    let mut n = 0u64;
    store.scan_all(|i, o| {
        sink(i, o);
        n += 1;
    });
    Ok(n)
}

/// Convert `source_db`'s `ledger-snapshot.bin` to `target_backend`, writing the
/// result (`ledger-snapshot.bin` + `.meta.json`, plus a `utxo-store/` for an
/// LSM target) into `target_db`. The source is never modified.
pub fn convert_snapshot(
    source_db: &Path,
    target_db: &Path,
    target_backend: SnapshotBackend,
) -> Result<ConvertStats> {
    let source_bin = source_db.join("ledger-snapshot.bin");
    let mut state = LedgerState::load_snapshot(&source_bin)
        .map_err(|e| anyhow!("load source snapshot {}: {e}", source_bin.display()))?;
    let source_backend =
        detect_source_backend(&source_bin, &state, source_db).ok_or_else(|| {
            anyhow!(
                "cannot determine the source backend of {} (no `.meta.json` sidecar \
             and the state is indeterminate)",
                source_bin.display()
            )
        })?;
    let slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);

    std::fs::create_dir_all(target_db)?;
    let target_bin = target_db.join("ledger-snapshot.bin");

    info!(
        source = ?source_backend,
        target = ?target_backend,
        slot,
        "snapshot-convert: starting"
    );

    let utxo_count = match target_backend {
        SnapshotBackend::DugiteMem => {
            // The target snapshot carries the UTxO inline in the bincode, so the
            // in-memory `utxo_set` must be populated.
            match source_backend {
                SnapshotBackend::DugiteLsm => {
                    let mut mem = UtxoSet::new();
                    let n = stream_source_lsm(source_db, |i, o| mem.insert(i, o))?;
                    state.utxo.utxo_set = mem;
                    n
                }
                // Already in-memory: the loaded state already carries the UTxO.
                SnapshotBackend::DugiteMem => state.utxo.utxo_set.len() as u64,
            }
            // `save_snapshot` tags the output `dugite-mem` (no store attached).
        }
        SnapshotBackend::DugiteLsm => {
            let store_path = target_db.join("utxo-store");
            std::fs::create_dir_all(&store_path)?;
            let mut store = UtxoStore::open(&store_path)
                .map_err(|e| anyhow!("open target LSM store {}: {e}", store_path.display()))?;
            let n = match source_backend {
                SnapshotBackend::DugiteMem => {
                    let mut n = 0u64;
                    state.utxo.utxo_set.scan_all(|i, o| {
                        store.insert(i.clone(), o.clone());
                        n += 1;
                    });
                    n
                }
                SnapshotBackend::DugiteLsm => {
                    stream_source_lsm(source_db, |i, o| store.insert(i, o))?
                }
            };
            // Flush the memtable to a consistent on-disk `ledger` snapshot so the
            // target node finds the UTxO on startup (cardano-lsm has no WAL).
            store
                .save_snapshot("ledger")
                .map_err(|e| anyhow!("save target LSM `ledger` snapshot: {e}"))?;
            // Replace the in-memory `utxo_set` with an empty, store-attached one so
            // the bincode carries an empty UTxO and `save_snapshot` tags it
            // `dugite-lsm`.
            let mut empty = UtxoSet::new();
            empty.attach_store(store);
            state.utxo.utxo_set = empty;
            n
        }
    };

    state
        .save_snapshot(&target_bin)
        .map_err(|e| anyhow!("write target snapshot {}: {e}", target_bin.display()))?;

    info!(
        target = %target_bin.display(),
        utxo_count,
        "snapshot-convert: complete"
    );

    Ok(ConvertStats {
        source_backend,
        target_backend,
        utxo_count,
        slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_ledger::UtxoSet;
    use dugite_primitives::address::{Address, ByronAddress};
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::transaction::OutputDatum;
    use dugite_primitives::value::Value as TxValue;

    fn mk_input(b: u8, index: u32) -> TransactionInput {
        TransactionInput {
            transaction_id: Hash32::from_bytes([b; 32]),
            index,
        }
    }

    fn mk_output(lovelace: u64) -> TransactionOutput {
        TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![0u8; 32],
            }),
            value: TxValue::lovelace(lovelace),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Order-independent fingerprint of a UTxO set: (entry count, total lovelace).
    fn utxo_fingerprint(s: &UtxoSet) -> (usize, u64) {
        (s.len(), s.total_lovelace().0)
    }

    /// The Rust mirror of Haskell's `InMemV2 ≡ LSM` state-machine property:
    /// converting a snapshot mem → lsm → mem must preserve the UTxO set
    /// *exactly* (same count, same total lovelace, same per-entry mapping).
    /// This is the byte-exact cross-backend equivalence gate.
    #[test]
    fn convert_roundtrip_mem_lsm_mem_preserves_utxo_set() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        // 1. Build an in-memory LedgerState with N distinct UTxOs (index is
        //    unique per entry, so all keys are distinct; lovelace varies).
        let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());
        let n = 500u32;
        for i in 0..n {
            state.utxo.utxo_set.insert(
                mk_input((i % 251) as u8 + 1, i),
                mk_output(1_000_000 + i as u64 * 7),
            );
        }
        let orig_fp = utxo_fingerprint(&state.utxo.utxo_set);
        assert_eq!(orig_fp.0, n as usize);
        state
            .save_snapshot(&src.join("ledger-snapshot.bin"))
            .unwrap();
        assert_eq!(
            dugite_ledger::SnapshotMeta::load(&src.join("ledger-snapshot.bin"))
                .unwrap()
                .backend,
            "dugite-mem"
        );

        // 2. mem → lsm: the bincode must now carry an empty in-mem set with a
        //    `dugite-lsm` tag, and the UTxO must live in a `utxo-store/`.
        let lsm = tmp.path().join("lsm");
        let s1 = convert_snapshot(&src, &lsm, SnapshotBackend::DugiteLsm).unwrap();
        assert_eq!(s1.source_backend, SnapshotBackend::DugiteMem);
        assert_eq!(s1.target_backend, SnapshotBackend::DugiteLsm);
        assert_eq!(s1.utxo_count, n as u64);
        let lsm_state = LedgerState::load_snapshot(&lsm.join("ledger-snapshot.bin")).unwrap();
        assert!(
            lsm_state.utxo.utxo_set.is_empty(),
            "an LSM snapshot's in-mem utxo_set must be empty"
        );
        assert!(lsm.join("utxo-store").exists());
        assert_eq!(
            dugite_ledger::SnapshotMeta::load(&lsm.join("ledger-snapshot.bin"))
                .unwrap()
                .backend,
            "dugite-lsm"
        );

        // 3. lsm → mem.
        let mem2 = tmp.path().join("mem2");
        let s2 = convert_snapshot(&lsm, &mem2, SnapshotBackend::DugiteMem).unwrap();
        assert_eq!(s2.source_backend, SnapshotBackend::DugiteLsm);
        assert_eq!(s2.utxo_count, n as u64);

        // 4. The round-tripped set must be identical to the original.
        let back = LedgerState::load_snapshot(&mem2.join("ledger-snapshot.bin")).unwrap();
        assert_eq!(
            utxo_fingerprint(&back.utxo.utxo_set),
            orig_fp,
            "count + total lovelace must be preserved across mem→lsm→mem"
        );
        state.utxo.utxo_set.scan_all(|i, o| {
            assert_eq!(
                back.utxo.utxo_set.lookup(i).as_ref(),
                Some(o),
                "every UTxO entry must round-trip identically"
            );
        });
        assert_eq!(
            dugite_ledger::SnapshotMeta::load(&mem2.join("ledger-snapshot.bin"))
                .unwrap()
                .backend,
            "dugite-mem"
        );
    }
}
