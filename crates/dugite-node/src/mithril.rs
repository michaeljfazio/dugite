//! Mithril snapshot import for fast initial sync.
//!
//! Downloads a Mithril-certified snapshot of the Cardano immutable DB via the
//! `mithril-client` SDK's **Cardano Database (V2)** API, unpacks the cardano-node
//! chunk files (and the Haskell ledger-state ancillary), verifies them against
//! the certified Merkle root, and places them into Dugite's ImmutableDB.
//!
//! All networks use the V2 `/artifact/cardano-database` API. The legacy V1
//! `/artifact/snapshots` endpoint has been retired by the Mithril aggregators
//! (preview first) and is no longer used.
//!
//! After import, [`replay_from_chunk_files`] replays the certified chunks to
//! rebuild ledger state (or the node loads the ancillary Haskell ledger state).

use anyhow::{Context, Result};
#[cfg(test)]
use dugite_primitives::hash::Hash32;
#[cfg(test)]
use dugite_primitives::time::{BlockNo, SlotNo};
use memmap2::Mmap;
use std::fs;
use std::path::Path;
#[cfg(test)]
use tracing::debug;
use tracing::{info, warn};

/// Mithril aggregator endpoints per network
const MAINNET_AGGREGATOR: &str =
    "https://aggregator.release-mainnet.api.mithril.network/aggregator";
const PREVIEW_AGGREGATOR: &str =
    "https://aggregator.pre-release-preview.api.mithril.network/aggregator";
const PREPROD_AGGREGATOR: &str =
    "https://aggregator.release-preprod.api.mithril.network/aggregator";

// ---------------------------------------------------------------------------
// Mithril genesis verification keys (from mithril-infra/configuration/)
// ---------------------------------------------------------------------------

/// Mainnet genesis verification key (Ed25519, JSON hex-encoded).
/// Source: https://github.com/input-output-hk/mithril/blob/main/mithril-infra/configuration/release-mainnet/genesis.vkey
const MAINNET_GENESIS_VKEY: &str =
    "5b3139312c36362c3134302c3138352c3133382c31312c3233372c3230372c3235302c3134342c32372c322c3138382c33302c31322c38312c3135352c3230342c31302c3137392c37352c32332c3133382c3139362c3231372c352c31342c32302c35372c37392c33392c3137365d";

/// Preview genesis verification key (Ed25519, JSON hex-encoded).
/// Source: https://github.com/input-output-hk/mithril/blob/main/mithril-infra/configuration/pre-release-preview/genesis.vkey
const PREVIEW_GENESIS_VKEY: &str =
    "5b3132372c37332c3132342c3136312c362c3133372c3133312c3231332c3230372c3131372c3139382c38352c3137362c3139392c3136322c3234312c36382c3132332c3131392c3134352c31332c3233322c3234332c34392c3232392c322c3234392c3230352c3230352c33392c3233352c34345d";

/// Preprod genesis verification key (Ed25519, JSON hex-encoded).
/// Same key as preview.
/// Source: https://github.com/input-output-hk/mithril/blob/main/mithril-infra/configuration/release-preprod/genesis.vkey
const PREPROD_GENESIS_VKEY: &str =
    "5b3132372c37332c3132342c3136312c362c3133372c3133312c3231332c3230372c3131372c3139382c38352c3137362c3139392c3136322c3234312c36382c3132332c3131392c3134352c31332c3233322c3234332c34392c3232392c322c3234392c3230352c3230352c33392c3233352c34345d";

/// Get the genesis verification key for a given network magic.
fn genesis_verification_key(network_magic: u64) -> Option<&'static str> {
    match network_magic {
        764824073 => Some(MAINNET_GENESIS_VKEY),
        2 => Some(PREVIEW_GENESIS_VKEY),
        1 => Some(PREPROD_GENESIS_VKEY),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Secondary index parsing
// ---------------------------------------------------------------------------

/// Entry from a cardano-node secondary index file.
/// Each entry is 56 bytes in the secondary index.
#[derive(Debug, Clone)]
struct SecondaryIndexEntry {
    block_offset: u64,
    _header_offset: u16,
    _header_size: u16,
    _checksum: u32,
    _header_hash: [u8; 32],
    block_or_ebb: u64,
}

impl SecondaryIndexEntry {
    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 56 {
            return None;
        }
        let block_offset = u64::from_be_bytes(data[0..8].try_into().ok()?);
        let header_offset = u16::from_be_bytes(data[8..10].try_into().ok()?);
        let header_size = u16::from_be_bytes(data[10..12].try_into().ok()?);
        let checksum = u32::from_be_bytes(data[12..16].try_into().ok()?);
        let mut header_hash = [0u8; 32];
        header_hash.copy_from_slice(&data[16..48]);
        let block_or_ebb = u64::from_be_bytes(data[48..56].try_into().ok()?);

        Some(SecondaryIndexEntry {
            block_offset,
            _header_offset: header_offset,
            _header_size: header_size,
            _checksum: checksum,
            _header_hash: header_hash,
            block_or_ebb,
        })
    }
}

/// Verify a block's CRC32 checksum against the secondary index entry.
#[cfg(test)]
fn verify_block_checksum(block_data: &[u8], expected: u32) -> bool {
    crc32fast::hash(block_data) == expected
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Get the aggregator URL for a given network magic
pub fn aggregator_url(network_magic: u64) -> &'static str {
    match network_magic {
        764824073 => MAINNET_AGGREGATOR,
        2 => PREVIEW_AGGREGATOR,
        1 => PREPROD_AGGREGATOR,
        _ => MAINNET_AGGREGATOR,
    }
}

/// Run the Mithril snapshot import.
///
/// Downloads the latest Mithril-certified **Cardano Database (V2)** snapshot via
/// the `mithril-client` SDK, unpacks the cardano-node immutable chunk files
/// (and, unless `--no-include-ancillary` is set, the Haskell ledger-state
/// ancillary archive), verifies them against the certified Merkle root, and
/// places them into Dugite's ImmutableDB layout under `database_path`.
///
/// All networks use the V2 `/artifact/cardano-database` API. The legacy V1
/// `/artifact/snapshots` endpoint has been retired by the Mithril aggregators
/// (preview first) and is no longer queried.
pub async fn import_snapshot(
    network_magic: u64,
    database_path: &Path,
    temp_dir: Option<&Path>,
    genesis_vkey_override: Option<&str>,
    skip_verification: bool,
    allow_stale_pparams: bool,
    include_ancillary: bool,
) -> Result<()> {
    let aggregator = aggregator_url(network_magic);
    info!(aggregator = %aggregator, "Fetching latest Mithril Cardano Database (V2) snapshot");

    // The mithril-client builder wires the genesis verification key in up front,
    // so it is required even when certificate verification is skipped. Known
    // networks always have one; private networks must pass `--mithril-genesis-vkey`.
    let genesis_vkey = genesis_vkey_override
        .or_else(|| genesis_verification_key(network_magic))
        .context(
            "No Mithril genesis verification key for this network. \
             Use --mithril-genesis-vkey to provide one for private networks.",
        )?;

    let mut builder = mithril_client::ClientBuilder::new(
        mithril_client::AggregatorDiscoveryType::Url(aggregator.to_string()),
    )
    .set_genesis_verification_key(mithril_client::GenesisVerificationKey::JsonHex(
        genesis_vkey.to_string(),
    ));

    // The ancillary archive (Haskell ledger state + tip chunk) carries its own
    // Ed25519 manifest signature; wire the key in so the SDK verifies it on unpack.
    if include_ancillary {
        if let Some(akey) = ancillary_verification_key_hex(network_magic) {
            builder = builder.set_ancillary_verification_key(Some(akey.to_string()));
        } else {
            warn!(
                network_magic,
                "No Mithril ancillary verification key for this network; \
                 ancillary signature cannot be verified"
            );
        }
    }

    let client = builder.build().context("Failed to build Mithril client")?;
    let cardano_db = client.cardano_database_v2();

    // Step 1: List Cardano Database (V2) snapshots and pick the latest.
    let snapshots = cardano_db
        .list()
        .await
        .context("Failed to list Cardano Database (V2) snapshots")?;
    let latest = snapshots
        .first()
        .context("No Cardano Database (V2) snapshots available from aggregator")?;

    // Step 2: Fetch the full snapshot detail (Merkle root, immutables, ancillary).
    let snapshot = cardano_db
        .get(&latest.hash)
        .await
        .context("Failed to fetch Cardano Database (V2) snapshot detail")?
        .with_context(|| format!("Cardano Database snapshot {} not found", latest.hash))?;

    info!(
        hash = %snapshot.hash,
        epoch = %snapshot.beacon.epoch,
        immutable = snapshot.beacon.immutable_file_number,
        "Mithril Cardano Database snapshot found",
    );

    // Step 3: Verify the certificate chain back to the genesis certificate.
    //
    // `verify_chain` walks every certificate's STM multi-signature back to the
    // genesis certificate (Ed25519 over the hardcoded genesis key). The returned
    // certificate is the trust anchor the Merkle-proof check (Step 5) is bound to.
    let certificate = if skip_verification {
        warn!(
            "Mithril certificate chain verification SKIPPED (--skip-certificate-verification). \
             The snapshot is trusted without cryptographic proof. \
             Do NOT use this in production."
        );
        None
    } else {
        info!("Verifying Mithril certificate chain...");
        let certificate = client
            .certificate()
            .verify_chain(&snapshot.certificate_hash)
            .await
            .context("Mithril certificate chain verification FAILED — snapshot rejected")?;
        info!(
            certificate_hash = %snapshot.certificate_hash,
            epoch = %certificate.epoch,
            "Certificate chain verified"
        );
        Some(certificate)
    };

    // Step 4: Download + unpack into a fresh target directory.
    //
    // The SDK writes the standard cardano-node `db/` layout: `immutable/` holds
    // the chunk files (0..=N, plus the ancillary tip chunk N+1 when ancillary is
    // included) and `ledger/` holds the Haskell ledger-state snapshot. Immutable
    // files are fetched per-file in parallel (`max_parallel_downloads`).
    let work_dir = temp_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("dugite-mithril"));
    let target_dir = work_dir.join("cardano-database");
    // Start from a clean target so chunk files from a prior (different) snapshot
    // cannot leak into this import. An interrupted download therefore restarts
    // from scratch rather than resuming.
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir).with_context(|| {
            format!("Failed to clear stale target dir {}", target_dir.display())
        })?;
    }
    fs::create_dir_all(&target_dir)?;

    // Operator override for download concurrency (per-immutable-file). The SDK
    // default is 20; clamp to a sane range.
    let parallelism = std::env::var("DUGITE_MITHRIL_DOWNLOAD_PARALLELISM")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.clamp(1, 32))
        .unwrap_or(20);

    let range = mithril_client::cardano_database_client::ImmutableFileRange::Full;

    info!(
        include_ancillary,
        parallelism, "Downloading Mithril snapshot (Cardano Database V2)..."
    );

    let primary_opts = mithril_client::cardano_database_client::DownloadUnpackOptions {
        allow_override: true,
        include_ancillary,
        max_parallel_downloads: parallelism,
    };
    match cardano_db
        .download_unpack(&snapshot, &range, &target_dir, primary_opts)
        .await
    {
        Ok(()) => {}
        // Without the ancillary archive there is no live ledger state, so the
        // node would fall back to genesis-default protocol parameters at the
        // imported tip (issue #335). Surface ancillary failure as fatal unless
        // the caller opted into the stale-PParams path, in which case retry
        // without ancillary and derive ledger state by replaying from genesis.
        Err(e) if include_ancillary && allow_stale_pparams => {
            warn!(
                error = format!("{e:#}"),
                "Snapshot download (with ancillary) failed — retrying WITHOUT ancillary \
                 because --allow-stale-pparams was set. Querying protocol-parameters will \
                 return stale values until the chain is replayed from genesis (issue #335)."
            );
            let fallback_opts = mithril_client::cardano_database_client::DownloadUnpackOptions {
                allow_override: true,
                include_ancillary: false,
                max_parallel_downloads: parallelism,
            };
            cardano_db
                .download_unpack(&snapshot, &range, &target_dir, fallback_opts)
                .await
                .context("Mithril snapshot download failed (no-ancillary fallback)")?;
        }
        Err(e) => {
            return Err(e).context("Mithril snapshot download/unpack failed");
        }
    }

    // Step 5: Verify the unpacked database against the certified Merkle root.
    //
    // `verify_cardano_database` recomputes the per-immutable-file digests over the
    // unpacked files, rebuilds the Merkle tree, and checks the root against the
    // certificate's signed message — proving the bytes on disk are exactly what
    // >= 2/3 of Cardano stake signed. Skipped only under --skip-certificate-verification.
    if let Some(certificate) = certificate.as_ref() {
        info!("Verifying unpacked database against certificate (Merkle proof)...");
        let verified_digests = cardano_db
            .download_and_verify_digests(certificate, &snapshot)
            .await
            .context("Failed to download/verify Mithril digests")?;
        cardano_db
            .verify_cardano_database(
                certificate,
                &snapshot,
                &range,
                /* allow_missing */ false,
                &target_dir,
                &verified_digests,
            )
            .await
            .context(
                "Mithril Cardano Database verification FAILED — the unpacked snapshot does \
                 not match the certified Merkle root; snapshot rejected",
            )?;
        info!("Database content verified against certificate");
    }

    // Step 6: Move immutable chunk files into permanent ImmutableDB storage.
    // ChainDB reads historical blocks directly from these chunk files; the
    // directory is NOT deleted after replay — it is the permanent immutable store.
    let src_immutable = target_dir.join("immutable");
    if !src_immutable.is_dir() {
        anyhow::bail!(
            "Mithril download produced no immutable/ directory at {}",
            src_immutable.display()
        );
    }
    let dest_immutable = database_path.join("immutable");
    info!("Moving chunk files to permanent storage");
    fs::create_dir_all(database_path)?;
    // Remove any pre-existing immutable directory so stale chunk files from a
    // prior run cannot pollute the new import (hash_index.dat is rebuilt on open).
    if dest_immutable.exists() {
        if let Err(e) = fs::remove_dir_all(&dest_immutable) {
            warn!(error = %e, "Failed to remove old immutable directory before import");
        }
    }
    if let Err(e) = fs::rename(&src_immutable, &dest_immutable) {
        // rename can fail across filesystems (e.g. tmpfs -> APFS); fall back to copy.
        warn!(error = %e, "rename failed, falling back to copy");
        copy_dir_recursive(&src_immutable, &dest_immutable)?;
    }

    // Step 7: Place the Haskell ledger state so Node::new can deserialise it
    // directly and skip the full block-replay path.
    if include_ancillary {
        let src_ledger = target_dir.join("ledger");
        if src_ledger.is_dir() {
            let haskell_ledger_dir = database_path.join("haskell-ledger");
            if haskell_ledger_dir.exists() {
                if let Err(e) = fs::remove_dir_all(&haskell_ledger_dir) {
                    warn!(error = %e, "Failed to remove old haskell-ledger directory");
                }
            }
            if let Err(e) = fs::rename(&src_ledger, &haskell_ledger_dir) {
                warn!(error = %e, "rename of ledger/ failed, falling back to copy");
                copy_dir_recursive(&src_ledger, &haskell_ledger_dir)?;
            }
            info!(path = %haskell_ledger_dir.display(), "Haskell ledger state saved");
        } else if allow_stale_pparams {
            // Reached only via the no-ancillary fallback above.
            warn!(
                "No ledger/ directory was unpacked; proceeding with genesis-default \
                 protocol parameters because --allow-stale-pparams was set (issue #335)."
            );
        } else {
            anyhow::bail!(
                "Ancillary requested but no ledger/ directory was unpacked. Without it the \
                 imported ledger state would use genesis-default protocol parameters \
                 (issue #335). Re-run with --allow-stale-pparams to override (NOT recommended)."
            );
        }
    } else {
        // `--no-include-ancillary`: the byte-exact verification path (issue #670).
        // Ledger state is derived from a from-genesis replay on the next run.
        info!(
            "Ancillary archive download skipped (--no-include-ancillary); \
             the next `dugite-node run` will derive ledger state by replaying \
             the certified chunks from genesis"
        );
    }

    // Step 8: Clear stale UTxO store and dugite ledger snapshots.
    //
    // The on-disk LSM UTxO store and any `ledger-snapshot*.bin` reference the
    // previous immutable tip; left in place they would leave phantom UTxOs and
    // cause invalid transaction propagation. The newly-placed haskell-ledger/
    // directory is intentionally kept — Node::new reads from it on startup.
    let utxo_store_path = database_path.join("utxo-store");
    if utxo_store_path.exists() {
        info!("Removing stale UTxO store (will be rebuilt during replay)");
        if let Err(e) = fs::remove_dir_all(&utxo_store_path) {
            warn!(error = %e, "Failed to remove UTxO store directory");
        }
    }
    for entry in fs::read_dir(database_path).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("ledger-snapshot") && name_str.ends_with(".bin") {
            info!(file = %name_str, "Removing stale ledger snapshot");
            let _ = fs::remove_file(entry.path());
        }
    }

    // Step 9: Clean up the temporary download directory.
    info!("Cleaning up temporary files");
    if let Err(e) = fs::remove_dir_all(&target_dir) {
        warn!(error = %e, "Failed to remove temporary download directory");
    }

    info!("Mithril import complete");
    Ok(())
}

/// Copy a directory recursively (fallback when rename fails across filesystems).
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Iterate blocks from cardano-node immutable chunk files in sequential order.
///
/// This is used for fast ledger replay after Mithril import. Reading chunk files
/// sequentially is orders of magnitude faster than random LSM lookups because
/// chunk files are already laid out in block order on disk.
///
/// Uses secondary index entries for block boundaries to avoid redundant
/// decode — the callback receives raw CBOR slices that are decoded once by the
/// caller for ledger application.
///
/// `start_after_slot` enables gap-fill skipping (issue #502): when the caller's
/// ledger tip is at slot `s`, passing `start_after_slot = s` makes this function
/// binary-search the chunk list to find the first chunk that may contain blocks
/// past `s`. Pass `0` to iterate from genesis (e.g. for offline dump/replay).
/// `byron_epoch_length` is required for slot computation on Byron blocks during
/// the binary-search probe and may be `0` when `start_after_slot == 0`.
///
/// Calls the provided callback for each block in order. The callback receives
/// the raw CBOR bytes. Returns the total number of blocks iterated.
pub fn replay_from_chunk_files<F>(
    immutable_dir: &Path,
    start_after_slot: u64,
    byron_epoch_length: u64,
    mut on_block: F,
) -> Result<u64>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    let mut chunk_numbers: Vec<u64> = Vec::new();
    for entry in fs::read_dir(immutable_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(num_str) = name_str.strip_suffix(".chunk") {
            if let Ok(num) = num_str.parse::<u64>() {
                chunk_numbers.push(num);
            }
        }
    }
    chunk_numbers.sort();

    // Gap-fill optimization (#502): when start_after_slot > 0, skip whole
    // chunks whose blocks are already applied. Without this, a fresh Mithril
    // import that lands a few hundred slots behind the immutable tip iterates
    // every chunk entry from genesis (4.3M on preview, ~10M on mainnet) just
    // to apply ~20 blocks.
    let start_idx = find_replay_start_chunk_idx(&chunk_numbers, start_after_slot, |chunk_num| {
        read_chunk_first_block_slot(immutable_dir, chunk_num, byron_epoch_length)
    })?;

    let mut total_blocks = 0u64;

    for chunk_num in &chunk_numbers[start_idx..] {
        let chunk_path = immutable_dir.join(format!("{chunk_num:05}.chunk"));
        let secondary_path = immutable_dir.join(format!("{chunk_num:05}.secondary"));

        // Fast path: use secondary index for block boundaries (no full decode)
        if secondary_path.exists() {
            let count = replay_chunk_with_index(&chunk_path, &secondary_path, &mut on_block)?;
            if count > 0 {
                total_blocks += count;
                continue;
            }
        }

        // Fallback: sequential CBOR probe for block boundaries (no full decode)
        let count = replay_chunk_sequential(&chunk_path, &mut on_block)?;
        total_blocks += count;
    }

    Ok(total_blocks)
}

/// Locate the first chunk index that may contain blocks past `start_after_slot`.
///
/// Binary-searches `chunk_numbers` (already sorted ascending) by probing each
/// chunk's first block slot via `chunk_first_slot`. Returns the start of the
/// range that must be scanned to find all blocks > `start_after_slot`.
///
/// The result is the smallest index `i` such that `chunk_first_slot(i) > start_after_slot`,
/// minus one — the step-back includes any straggler blocks in the boundary chunk
/// whose slots lie between `chunk_first_slot(i-1)` and `start_after_slot`.
///
/// When `start_after_slot == 0` (full replay from genesis) or the chunk list is
/// empty, returns 0 without probing. A `None` result from `chunk_first_slot`
/// (unreadable / corrupt) is treated as "include this and earlier chunks" to
/// avoid silently dropping blocks.
fn find_replay_start_chunk_idx<F>(
    chunk_numbers: &[u64],
    start_after_slot: u64,
    mut chunk_first_slot: F,
) -> Result<usize>
where
    F: FnMut(u64) -> Result<Option<u64>>,
{
    if start_after_slot == 0 || chunk_numbers.is_empty() {
        return Ok(0);
    }

    let mut lo = 0usize;
    let mut hi = chunk_numbers.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        match chunk_first_slot(chunk_numbers[mid])? {
            Some(slot) if slot <= start_after_slot => lo = mid + 1,
            _ => hi = mid,
        }
    }
    Ok(lo.saturating_sub(1))
}

/// Read the first block's slot for a chunk, used by [`find_replay_start_chunk_idx`]
/// to binary-search for the gap-fill start chunk without iterating every block.
///
/// The slot is taken directly from the secondary index entry 0's `block_or_ebb`
/// field (bytes 48..56). cardano-node serialises `BlockOrEBB` as a raw u64 BE:
///
///   * for Shelley+ blocks the value is the absolute `SlotNo`,
///   * for Byron Main blocks the value is the absolute `SlotNo`,
///   * for Byron EBBs the value is the `EpochNo` (epoch-relative).
///
/// EBB-encoded epoch numbers are always far smaller than any post-Byron tip
/// slot, so the binary search still places Byron chunks correctly to the left
/// of any modern target slot. Using the index avoids invoking the block decoder
/// on the first block of every probed chunk — that path was failing for several
/// thousand Byron / edge-case chunks and tripping the binary search into
/// returning `start_idx = 0` (full-chain replay from genesis).
///
/// The `byron_epoch_length` parameter is retained for the legacy fallback when
/// the secondary index is missing.
///
/// Returns `None` on missing files, empty chunks, or missing/short secondary
/// index — callers treat `None` as a hint to include the chunk in replay rather
/// than risk dropping a block.
fn read_chunk_first_block_slot(
    immutable_dir: &Path,
    chunk_num: u64,
    byron_epoch_length: u64,
) -> Result<Option<u64>> {
    let chunk_path = immutable_dir.join(format!("{chunk_num:05}.chunk"));
    let secondary_path = immutable_dir.join(format!("{chunk_num:05}.secondary"));

    // Fast path: read slot directly from the on-disk secondary index entry 0.
    // No decoder dependency, no block read.
    if secondary_path.exists() {
        if let Ok(secondary_data) = fs::read(&secondary_path) {
            if secondary_data.len() >= 56 {
                if let Some(entry) = SecondaryIndexEntry::from_bytes(&secondary_data) {
                    return Ok(Some(entry.block_or_ebb));
                }
            }
        }
    }

    // Fallback (no secondary index): probe the chunk for the first CBOR item
    // and decode just enough to extract the slot. This path is only exercised
    // for partial / un-finalised chunks.
    let chunk_file = match fs::File::open(&chunk_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let chunk_len = chunk_file.metadata()?.len() as usize;
    if chunk_len == 0 {
        return Ok(None);
    }
    // SAFETY: File is opened read-only and not modified externally during the lifetime of this Mmap.
    let chunk_data = unsafe { Mmap::map(&chunk_file).context("Failed to mmap chunk file")? };
    let (block_start, block_end) = probe_first_cbor_item(&chunk_data);
    if block_start >= chunk_len || block_end > chunk_len || block_start >= block_end {
        return Ok(None);
    }
    match dugite_serialization::decode_block_minimal_with_byron_epoch_length(
        &chunk_data[block_start..block_end],
        byron_epoch_length,
    ) {
        Ok(block) => Ok(Some(block.slot().0)),
        Err(_) => Ok(None),
    }
}

/// Scan a chunk buffer for the first valid CBOR item, returning its byte range.
/// Returns `(len, len)` (empty range) when no item is found.
fn probe_first_cbor_item(chunk_data: &[u8]) -> (usize, usize) {
    let mut offset = 0;
    while offset < chunk_data.len() {
        let remaining = &chunk_data[offset..];
        match cbor_item_size(remaining) {
            Some(size) if size > 0 => return (offset, offset + size),
            _ => offset += 1,
        }
    }
    (chunk_data.len(), chunk_data.len())
}

/// Replay a single chunk file using secondary index for block boundaries.
/// Returns raw CBOR slices without a full block decode (the caller decodes once).
fn replay_chunk_with_index<F>(
    chunk_path: &Path,
    secondary_path: &Path,
    on_block: &mut F,
) -> Result<u64>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    let secondary_data = fs::read(secondary_path).context("Failed to read secondary index")?;
    let chunk_file = fs::File::open(chunk_path).context("Failed to open chunk file")?;
    // SAFETY: File is opened read-only and not modified externally during the lifetime of this Mmap.
    let chunk_data = unsafe { Mmap::map(&chunk_file).context("Failed to mmap chunk file")? };

    let mut entries = Vec::new();
    let mut offset = 0;
    while offset + 56 <= secondary_data.len() {
        if let Some(entry) = SecondaryIndexEntry::from_bytes(&secondary_data[offset..]) {
            entries.push(entry);
        }
        offset += 56;
    }

    let mut count = 0u64;
    for i in 0..entries.len() {
        let entry = &entries[i];
        let block_start = entry.block_offset as usize;
        let block_end = if i + 1 < entries.len() {
            entries[i + 1].block_offset as usize
        } else {
            chunk_data.len()
        };

        if block_start >= chunk_data.len() || block_end > chunk_data.len() {
            continue;
        }

        on_block(&chunk_data[block_start..block_end])?;
        count += 1;
    }

    Ok(count)
}

/// Replay a single chunk file by sequential CBOR probing (no full decode).
fn replay_chunk_sequential<F>(chunk_path: &Path, on_block: &mut F) -> Result<u64>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    let chunk_file = fs::File::open(chunk_path).context("Failed to open chunk file")?;
    let chunk_len = chunk_file.metadata()?.len() as usize;
    if chunk_len == 0 {
        return Ok(0);
    }
    // SAFETY: File is opened read-only and not modified externally during the lifetime of this Mmap.
    let chunk_data = unsafe { Mmap::map(&chunk_file).context("Failed to mmap chunk file")? };

    let mut count = 0u64;
    let mut offset = 0;
    while offset < chunk_data.len() {
        let remaining = &chunk_data[offset..];
        let item_size = match cbor_item_size(remaining) {
            Some(size) if size > 0 => size,
            _ => {
                offset += 1;
                continue;
            }
        };
        // Validate the probed boundary actually yields a decodable block
        // before invoking the callback. Without this gate, every random
        // byte sequence inside a block body that looks like a CBOR item
        // (which is very common — tx CBOR has nested arrays/maps/strings)
        // would call into the callback and log "decode failed" warnings.
        // Sequential probing without a secondary index is inherently lossy;
        // we only want to surface boundaries that are actually blocks.
        if dugite_serialization::decode_block_minimal(&remaining[..item_size]).is_err() {
            offset += 1;
            continue;
        }
        on_block(&remaining[..item_size])?;
        count += 1;
        offset += item_size;
    }

    Ok(count)
}

/// A parsed block: (slot, hash, block_number, raw_cbor)
#[cfg(test)]
type ParsedBlock = (SlotNo, Hash32, BlockNo, Vec<u8>);

/// Parse a chunk file using the secondary index for block boundaries.
///
/// Uses memory-mapped I/O for the chunk file to avoid loading the entire file
/// into memory. The secondary index is small enough to read directly.
#[cfg(test)]
fn parse_chunk_with_index(
    chunk_path: &Path,
    secondary_path: &Path,
    checksum_failures: &mut u64,
) -> Result<Vec<ParsedBlock>> {
    let secondary_data = fs::read(secondary_path).context("Failed to read secondary index file")?;

    // Memory-map the chunk file instead of reading it entirely into memory
    let chunk_file = fs::File::open(chunk_path).context("Failed to open chunk file")?;
    // SAFETY: File is opened read-only and not modified externally during the lifetime of this Mmap.
    let chunk_data = unsafe { Mmap::map(&chunk_file).context("Failed to mmap chunk file")? };

    // Parse secondary index entries (56 bytes each, no header)
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset + 56 <= secondary_data.len() {
        if let Some(entry) = SecondaryIndexEntry::from_bytes(&secondary_data[offset..]) {
            entries.push(entry);
        }
        offset += 56;
    }

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut blocks = Vec::with_capacity(entries.len());

    for i in 0..entries.len() {
        let entry = &entries[i];
        let block_start = entry.block_offset as usize;

        // Block end is either the next block's offset or the end of the chunk file
        let block_end = if i + 1 < entries.len() {
            entries[i + 1].block_offset as usize
        } else {
            chunk_data.len()
        };

        if block_start >= chunk_data.len() || block_end > chunk_data.len() {
            warn!(
                block_start,
                block_end,
                chunk_len = chunk_data.len(),
                "Invalid block offset in secondary index, skipping"
            );
            continue;
        }

        let block_cbor = &chunk_data[block_start..block_end];

        // Verify CRC32 checksum from secondary index
        if entry._checksum != 0 && !verify_block_checksum(block_cbor, entry._checksum) {
            *checksum_failures += 1;
            warn!(
                chunk = %chunk_path.display(),
                offset = block_start,
                expected_crc = entry._checksum,
                actual_crc = crc32fast::hash(block_cbor),
                "Block CRC32 checksum mismatch"
            );
            // Continue importing — the block may still be valid (checksum
            // could be computed over a different range in some eras)
        }

        // Extract block identity (slot + block_no) via the in-house
        // serialization layer. The hash comes from the secondary index;
        // we trust the index over re-decoding the header.
        match dugite_serialization::extract_block_identity(block_cbor) {
            Ok((slot, block_no, _header_hash)) => {
                let hash = Hash32::from_bytes(entry._header_hash);
                blocks.push((slot, hash, block_no, block_cbor.to_vec()));
            }
            Err(e) => {
                // Log but continue — might be an EBB or corrupted block
                debug!(
                    chunk = %chunk_path.display(),
                    offset = block_start,
                    error = %e,
                    "Failed to decode block from chunk file"
                );
            }
        }
    }

    Ok(blocks)
}

/// Parse a chunk file by sequential CBOR decoding (fallback when no secondary index).
///
/// Uses memory-mapped I/O and proper CBOR size probing to avoid O(n^2)
/// byte-by-byte scanning on decode failures.
#[cfg(test)]
fn parse_chunk_sequential(chunk_path: &Path) -> Result<Vec<ParsedBlock>> {
    let chunk_file = fs::File::open(chunk_path).context("Failed to open chunk file")?;
    let chunk_len = chunk_file.metadata()?.len() as usize;
    if chunk_len == 0 {
        return Ok(Vec::new());
    }

    // SAFETY: File is opened read-only and not modified externally during the lifetime of this Mmap.
    let chunk_data = unsafe { Mmap::map(&chunk_file).context("Failed to mmap chunk file")? };

    let mut blocks = Vec::new();
    let mut offset = 0;

    while offset < chunk_data.len() {
        let remaining = &chunk_data[offset..];
        if remaining.is_empty() {
            break;
        }

        // First, probe the CBOR item size to know how many bytes to skip
        // regardless of whether the in-house decoder can decode this particular era/block.
        let item_size = match cbor_item_size(remaining) {
            Some(size) if size > 0 => size,
            _ => {
                // Not valid CBOR at this offset — skip one byte
                offset += 1;
                continue;
            }
        };

        // Try to decode the CBOR item as a Cardano block
        match dugite_serialization::extract_block_identity(&remaining[..item_size]) {
            Ok((slot, block_no, hash)) => {
                blocks.push((slot, hash, block_no, remaining[..item_size].to_vec()));
            }
            Err(_) => {
                // Valid CBOR but not a decodable Cardano block (e.g. EBB) — skip it
            }
        }

        offset += item_size;
    }

    Ok(blocks)
}

/// Determine the size of the next CBOR item in a byte slice
fn cbor_item_size(data: &[u8]) -> Option<usize> {
    let mut decoder = minicbor::Decoder::new(data);
    let start = decoder.position();
    skip_item(&mut decoder).ok()?;
    Some(decoder.position() - start)
}

/// Recursively skip one CBOR data item in the decoder.
fn skip_item(decoder: &mut minicbor::Decoder) -> Result<(), minicbor::decode::Error> {
    use minicbor::data::Type;
    match decoder.datatype()? {
        Type::Array | Type::ArrayIndef => {
            let len = decoder.array()?;
            if let Some(n) = len {
                for _ in 0..n {
                    skip_item(decoder)?;
                }
            } else {
                loop {
                    if decoder.datatype()? == Type::Break {
                        decoder.skip()?;
                        break;
                    }
                    skip_item(decoder)?;
                }
            }
            Ok(())
        }
        Type::Map | Type::MapIndef => {
            let len = decoder.map()?;
            if let Some(n) = len {
                for _ in 0..n {
                    skip_item(decoder)?;
                    skip_item(decoder)?;
                }
            } else {
                loop {
                    if decoder.datatype()? == Type::Break {
                        decoder.skip()?;
                        break;
                    }
                    skip_item(decoder)?;
                    skip_item(decoder)?;
                }
            }
            Ok(())
        }
        Type::Tag => {
            decoder.tag()?;
            skip_item(decoder)
        }
        _ => {
            decoder.skip()?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Ancillary verification keys per network
// ---------------------------------------------------------------------------
//
// These are hex-encoded JSON byte arrays (the same encoding Mithril uses for
// genesis verification keys). Each decodes to a 32-byte Ed25519 public key
// used to verify the ancillary manifest signature.
//
// Source: mithril-infra/configuration/<env>/ancillary.vkey

#[allow(dead_code)]
/// Preview ancillary verification key.
const PREVIEW_ANCILLARY_VKEY: &str = "5b3138392c3139322c3231362c3135302c3131342c3231362c3233372c3231302c34352c31382c32312c3139362c3230382c3234362c3134362c322c3235322c3234332c3235312c3139372c32382c3135372c3230342c3134352c33302c31342c3232382c3136382c3132392c38332c3133362c33365d";

#[allow(dead_code)]
/// Preprod ancillary verification key.
const PREPROD_ANCILLARY_VKEY: &str = "5b3138392c3139322c3231362c3135302c3131342c3231362c3233372c3231302c34352c31382c32312c3139362c3230382c3234362c3134362c322c3235322c3234332c3235312c3139372c32382c3135372c3230342c3134352c33302c31342c3232382c3136382c3132392c38332c3133362c33365d";

#[allow(dead_code)]
/// Mainnet ancillary verification key.
const MAINNET_ANCILLARY_VKEY: &str = "5b32332c37312c39362c3133332c34372c3235332c3232362c3133362c3233352c35372c3136342c3130362c3138362c322c32312c32392c3132302c3136332c38392c3132312c3137372c3133382c3230382c3133382c3231342c39392c35382c32322c302c35382c332c36395d";

/// Look up the ancillary verification key (raw Mithril hex-JSON encoding) for a
/// given network magic, as accepted by `ClientBuilder::set_ancillary_verification_key`.
/// Returns `None` for unknown/private networks.
fn ancillary_verification_key_hex(network_magic: u64) -> Option<&'static str> {
    match network_magic {
        764824073 => Some(MAINNET_ANCILLARY_VKEY),
        2 => Some(PREVIEW_ANCILLARY_VKEY),
        1 => Some(PREPROD_ANCILLARY_VKEY),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn test_aggregator_url_mainnet() {
        assert_eq!(
            aggregator_url(764824073),
            "https://aggregator.release-mainnet.api.mithril.network/aggregator"
        );
    }

    #[test]
    fn test_aggregator_url_preview() {
        assert_eq!(
            aggregator_url(2),
            "https://aggregator.pre-release-preview.api.mithril.network/aggregator"
        );
    }

    #[test]
    fn test_aggregator_url_preprod() {
        assert_eq!(
            aggregator_url(1),
            "https://aggregator.release-preprod.api.mithril.network/aggregator"
        );
    }

    #[test]
    fn test_aggregator_url_unknown_defaults_to_mainnet() {
        assert_eq!(aggregator_url(999), aggregator_url(764824073));
    }

    #[test]
    fn test_secondary_index_entry_parse() {
        let mut data = [0u8; 56];
        // block_offset = 1024
        data[0..8].copy_from_slice(&1024u64.to_be_bytes());
        // header_offset = 2
        data[8..10].copy_from_slice(&2u16.to_be_bytes());
        // header_size = 100
        data[10..12].copy_from_slice(&100u16.to_be_bytes());
        // checksum = 12345
        data[12..16].copy_from_slice(&12345u32.to_be_bytes());
        // header_hash
        data[16..48].copy_from_slice(&[0xAB; 32]);
        // block_or_ebb (slot 5000)
        data[48..56].copy_from_slice(&5000u64.to_be_bytes());

        let entry = SecondaryIndexEntry::from_bytes(&data).unwrap();
        assert_eq!(entry.block_offset, 1024);
        assert_eq!(entry._header_offset, 2);
        assert_eq!(entry._header_size, 100);
        assert_eq!(entry._checksum, 12345);
        assert_eq!(entry._header_hash, [0xAB; 32]);
        assert_eq!(entry.block_or_ebb, 5000);
    }

    #[test]
    fn test_secondary_index_entry_too_short() {
        let data = [0u8; 55]; // 1 byte short
        assert!(SecondaryIndexEntry::from_bytes(&data).is_none());
    }

    #[test]
    fn test_cbor_item_size_simple() {
        // CBOR array of 2 elements: [1, 2]
        // 0x82 = array(2), 0x01 = unsigned(1), 0x02 = unsigned(2)
        let data = [0x82, 0x01, 0x02, 0xFF]; // extra byte should not be consumed
        let size = cbor_item_size(&data).unwrap();
        assert_eq!(size, 3);
    }

    #[test]
    fn test_cbor_item_size_nested() {
        // [[1, 2], [3]]
        // 0x82 (array 2), 0x82 0x01 0x02 (array [1,2]), 0x81 0x03 (array [3])
        let data = [0x82, 0x82, 0x01, 0x02, 0x81, 0x03];
        let size = cbor_item_size(&data).unwrap();
        assert_eq!(size, 6);
    }

    #[test]
    fn test_cbor_item_size_map() {
        // {1: 2} — 0xA1 0x01 0x02
        let data = [0xA1, 0x01, 0x02];
        let size = cbor_item_size(&data).unwrap();
        assert_eq!(size, 3);
    }

    #[test]
    fn test_cbor_item_size_tagged() {
        // tag(24) + bytes(2) [0x01, 0x02]
        // 0xD8 0x18 0x42 0x01 0x02
        let data = [0xD8, 0x18, 0x42, 0x01, 0x02];
        let size = cbor_item_size(&data).unwrap();
        assert_eq!(size, 5);
    }

    #[test]
    fn test_cbor_item_size_invalid() {
        let data = [0xFF]; // CBOR break — not a valid top-level item
                           // May or may not return None depending on minicbor's skip behaviour
                           // Just ensure it doesn't panic
        let _ = cbor_item_size(&data);
    }

    #[test]
    fn test_verify_block_checksum_valid() {
        let data = b"hello world";
        let crc = crc32fast::hash(data);
        assert!(verify_block_checksum(data, crc));
    }

    #[test]
    fn test_verify_block_checksum_invalid() {
        let data = b"hello world";
        assert!(!verify_block_checksum(data, 0xDEADBEEF));
    }

    #[test]
    fn test_verify_block_checksum_empty() {
        let data = b"";
        let crc = crc32fast::hash(data);
        assert!(verify_block_checksum(data, crc));
    }

    #[test]
    fn test_secondary_index_multiple_entries() {
        // Parse 3 sequential entries from a contiguous buffer
        let mut buf = [0u8; 56 * 3];
        for (i, offset_val) in [0u64, 1000, 2000].iter().enumerate() {
            let base = i * 56;
            buf[base..base + 8].copy_from_slice(&offset_val.to_be_bytes());
            buf[base + 8..base + 10].copy_from_slice(&0u16.to_be_bytes());
            buf[base + 10..base + 12].copy_from_slice(&100u16.to_be_bytes());
            buf[base + 12..base + 16].copy_from_slice(&(i as u32).to_be_bytes());
            buf[base + 16..base + 48].copy_from_slice(&[i as u8; 32]);
            buf[base + 48..base + 56].copy_from_slice(&(i as u64).to_be_bytes());
        }

        let mut entries = Vec::new();
        let mut offset = 0;
        while offset + 56 <= buf.len() {
            if let Some(entry) = SecondaryIndexEntry::from_bytes(&buf[offset..]) {
                entries.push(entry);
            }
            offset += 56;
        }

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].block_offset, 0);
        assert_eq!(entries[1].block_offset, 1000);
        assert_eq!(entries[2].block_offset, 2000);
    }

    #[test]
    fn test_parse_chunk_sequential_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        fs::write(&chunk_path, b"").unwrap();

        let blocks = parse_chunk_sequential(&chunk_path).unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_parse_chunk_sequential_invalid_cbor() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        // Write some random non-CBOR data
        fs::write(&chunk_path, [0xDE, 0xAD, 0xBE, 0xEF]).unwrap();

        let blocks = parse_chunk_sequential(&chunk_path).unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_parse_chunk_sequential_valid_cbor_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        // Valid CBOR: [1, 2] — but not a Cardano block
        fs::write(&chunk_path, [0x82, 0x01, 0x02]).unwrap();

        let blocks = parse_chunk_sequential(&chunk_path).unwrap();
        // Should skip it (valid CBOR but not a decodable block)
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_parse_chunk_with_index_missing_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        // Only create secondary, not chunk
        fs::write(&secondary_path, [0u8; 56]).unwrap();

        let mut failures = 0;
        let result = parse_chunk_with_index(&chunk_path, &secondary_path, &mut failures);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_chunk_with_index_empty_secondary() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        fs::write(&chunk_path, b"some data").unwrap();
        fs::write(&secondary_path, b"").unwrap(); // empty secondary

        let mut failures = 0;
        let blocks = parse_chunk_with_index(&chunk_path, &secondary_path, &mut failures).unwrap();
        assert!(blocks.is_empty());
        assert_eq!(failures, 0);
    }

    #[test]
    fn test_parse_chunk_with_index_bad_offset() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        // Write a small chunk file
        fs::write(&chunk_path, [0x82, 0x01, 0x02]).unwrap();

        // Write a secondary index entry with an offset beyond the chunk file
        let mut entry_data = [0u8; 56];
        entry_data[0..8].copy_from_slice(&9999u64.to_be_bytes()); // offset way past end
        fs::write(&secondary_path, entry_data).unwrap();

        let mut failures = 0;
        let blocks = parse_chunk_with_index(&chunk_path, &secondary_path, &mut failures).unwrap();
        assert!(blocks.is_empty()); // should skip the invalid entry
    }

    #[test]
    fn test_beacon_hash_matches_mithril_test_vector() {
        // Test vector from Mithril source: compute_beacon_hash("testnet", {epoch: 10, immutable: 100})
        let mut hasher = Sha256::new();
        hasher.update("testnet".as_bytes());
        hasher.update(10u64.to_be_bytes());
        hasher.update(100u64.to_be_bytes());
        let beacon_hash = hex::encode(hasher.finalize());
        assert_eq!(
            beacon_hash,
            "48cbf709b56204d8315aefd3a416b45398094f6fd51785c5b7dcaf7f35aacbfb"
        );
    }

    #[test]
    fn test_genesis_verification_key_known_networks() {
        // Mainnet has a distinct key
        assert!(genesis_verification_key(764824073).is_some());
        let mainnet_key = genesis_verification_key(764824073).unwrap();
        assert!(mainnet_key.starts_with("5b31393"));
        assert_ne!(
            mainnet_key,
            genesis_verification_key(2).unwrap(),
            "mainnet key should differ from preview"
        );

        // Preview
        assert!(genesis_verification_key(2).is_some());

        // Preprod (same key as preview)
        assert!(genesis_verification_key(1).is_some());
        assert_eq!(
            genesis_verification_key(2).unwrap(),
            genesis_verification_key(1).unwrap(),
            "preview and preprod share the same genesis key"
        );
    }

    #[test]
    fn test_genesis_verification_key_unknown_network() {
        assert!(genesis_verification_key(999).is_none());
        assert!(genesis_verification_key(0).is_none());
    }

    #[test]
    fn test_genesis_keys_are_valid_hex() {
        // Each genesis key should be a valid hex string that decodes to a JSON array
        for magic in [764824073, 2, 1] {
            let key = genesis_verification_key(magic).unwrap();
            let decoded = hex::decode(key)
                .unwrap_or_else(|_| panic!("genesis key for magic {magic} is not valid hex"));
            let json_str = std::str::from_utf8(&decoded)
                .unwrap_or_else(|_| panic!("genesis key for magic {magic} is not valid UTF-8"));
            assert!(
                json_str.starts_with('[') && json_str.ends_with(']'),
                "genesis key for magic {magic} should decode to a JSON array, got: {json_str}"
            );
        }
    }

    /// Integration test: verify a real Mithril preview certificate chain.
    ///
    /// This test hits the real Mithril aggregator API and verifies that we can
    /// successfully build a client, fetch a snapshot, and verify its certificate
    /// chain back to genesis. Run manually with:
    ///   cargo nextest run -p dugite-node -E 'test(verify_preview_certificate_chain)' -- --ignored
    #[tokio::test]
    #[ignore]
    async fn test_verify_preview_certificate_chain() {
        let aggregator = aggregator_url(2); // preview
        let genesis_vkey = genesis_verification_key(2).unwrap();

        // Build the Mithril client
        let client = mithril_client::ClientBuilder::new(
            mithril_client::AggregatorDiscoveryType::Url(aggregator.to_string()),
        )
        .set_genesis_verification_key(mithril_client::GenesisVerificationKey::JsonHex(
            genesis_vkey.to_string(),
        ))
        .build()
        .expect("Failed to build Mithril client");

        // Fetch latest snapshot to get its certificate_hash
        let http = reqwest::Client::builder()
            .user_agent("dugite-test/0.1")
            .build()
            .unwrap();

        let snapshots: Vec<serde_json::Value> = http
            .get(format!("{aggregator}/artifact/snapshots"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let cert_hash = snapshots[0]["certificate_hash"]
            .as_str()
            .expect("No certificate_hash in snapshot");

        // Verify the certificate chain — this is the core test
        let certificate = client
            .certificate()
            .verify_chain(cert_hash)
            .await
            .expect("Certificate chain verification failed");

        // Epoch implements Display, so just verify it's not the default
        let epoch_str = format!("{}", certificate.epoch);
        assert!(epoch_str != "0", "certificate epoch should be positive");
        println!(
            "Certificate chain verified: epoch={}, hash={}",
            certificate.epoch, cert_hash
        );
    }

    /// Build a chunk-first-slot lookup over `chunks: &[(chunk_num, first_slot)]`
    /// that also counts how many lookups the binary search performs. Returns
    /// (lookup_fn, counter handle).
    fn counting_lookup(
        chunks: Vec<(u64, u64)>,
    ) -> (
        impl FnMut(u64) -> Result<Option<u64>>,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counter_clone = counter.clone();
        let lookup = move |chunk_num: u64| -> Result<Option<u64>> {
            counter_clone.set(counter_clone.get() + 1);
            Ok(chunks
                .iter()
                .find(|(n, _)| *n == chunk_num)
                .map(|(_, s)| *s))
        };
        (lookup, counter)
    }

    #[test]
    fn test_find_replay_start_chunk_idx_full_replay_from_genesis() {
        // start_after_slot == 0 means "replay from genesis"; no probing at all.
        let chunks = vec![(0u64, 0u64), (1, 100), (2, 200), (3, 300)];
        let chunk_nums: Vec<u64> = chunks.iter().map(|(n, _)| *n).collect();
        let (lookup, counter) = counting_lookup(chunks);
        let idx = find_replay_start_chunk_idx(&chunk_nums, 0, lookup).unwrap();
        assert_eq!(idx, 0, "genesis replay must start at index 0");
        assert_eq!(counter.get(), 0, "no probes should be performed");
    }

    #[test]
    fn test_find_replay_start_chunk_idx_empty_chunk_list() {
        let chunk_nums: Vec<u64> = vec![];
        let (lookup, counter) = counting_lookup(vec![]);
        let idx = find_replay_start_chunk_idx(&chunk_nums, 1000, lookup).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn test_find_replay_start_chunk_idx_boundary_chunk_included() {
        // Chunks 0,1,2,3 cover slot ranges [0..100), [100..200), [200..300), [300..400).
        // Ledger tip at slot 150 means we need to apply blocks 151..199 from chunk 1
        // plus all of chunks 2 and 3. find_replay_start_chunk_idx must return 1.
        let chunks = vec![(0u64, 0u64), (1, 100), (2, 200), (3, 300)];
        let chunk_nums: Vec<u64> = chunks.iter().map(|(n, _)| *n).collect();
        let (lookup, _counter) = counting_lookup(chunks);
        let idx = find_replay_start_chunk_idx(&chunk_nums, 150, lookup).unwrap();
        assert_eq!(
            idx, 1,
            "boundary chunk must be included so straggler blocks at slots > 150 aren't dropped"
        );
    }

    #[test]
    fn test_find_replay_start_chunk_idx_skips_when_tip_equals_chunk_first_slot() {
        // start_after_slot exactly equal to a chunk's first slot must still
        // include that chunk's predecessor (a block at slot 100 itself is
        // already applied, but the predecessor chunk may have blocks > 100
        // — no, actually predecessor's last slot < 100, so we could skip,
        // but our conservative step-back is fine).
        let chunks = vec![(0u64, 0u64), (1, 100), (2, 200)];
        let chunk_nums: Vec<u64> = chunks.iter().map(|(n, _)| *n).collect();
        let (lookup, _counter) = counting_lookup(chunks);
        let idx = find_replay_start_chunk_idx(&chunk_nums, 100, lookup).unwrap();
        // Binary search finds smallest i where first(i) > 100 → i=2; step back → 1.
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_find_replay_start_chunk_idx_tip_past_all_chunks() {
        // If start_after_slot exceeds every chunk's first slot, we still
        // process the last chunk: its blocks may all be ≤ start_after_slot,
        // but the caller's existing per-block skip handles that. This is
        // the only chunk that *could* contain blocks past start_after_slot.
        let chunks = vec![(0u64, 0u64), (1, 100), (2, 200)];
        let chunk_nums: Vec<u64> = chunks.iter().map(|(n, _)| *n).collect();
        let (lookup, _counter) = counting_lookup(chunks);
        let idx = find_replay_start_chunk_idx(&chunk_nums, 9999, lookup).unwrap();
        assert_eq!(idx, 2);
    }

    #[test]
    fn test_find_replay_start_chunk_idx_large_chunk_list_uses_log_probes() {
        // The whole point of #502: avoid linear scans. With 26000 chunks,
        // binary search must probe O(log n) ≈ 15 — not 26000.
        let chunks: Vec<(u64, u64)> = (0..26_000u64).map(|n| (n, n * 100)).collect();
        let chunk_nums: Vec<u64> = chunks.iter().map(|(n, _)| *n).collect();
        let (lookup, counter) = counting_lookup(chunks);
        let _ = find_replay_start_chunk_idx(&chunk_nums, 100_000, lookup).unwrap();
        assert!(
            counter.get() < 20,
            "binary search must do O(log n) probes, got {}",
            counter.get()
        );
    }

    #[test]
    fn test_find_replay_start_chunk_idx_unreadable_chunk_treated_as_includable() {
        // A chunk that returns None (unreadable / corrupt) must not be skipped
        // — biases toward correctness over speed.
        let chunk_nums = vec![0u64, 1, 2, 3];
        let lookup = |chunk_num: u64| -> Result<Option<u64>> {
            // Only chunk 3 is readable, with slot 300.
            Ok(if chunk_num == 3 { Some(300) } else { None })
        };
        let idx = find_replay_start_chunk_idx(&chunk_nums, 100, lookup).unwrap();
        // All None responses push hi down; lo never advances → lo=0; step back → 0.
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_replay_from_chunk_files_empty_dir() {
        // Sanity: integration-style smoke test of the public entrypoint.
        let dir = tempfile::tempdir().unwrap();
        let count = replay_from_chunk_files(dir.path(), 0, 0, |_cbor| Ok(())).unwrap();
        assert_eq!(count, 0);
    }

    /// Synthesise a single 56-byte secondary index entry with the given
    /// `block_or_ebb` slot value (and a zero block_offset / dummy hash).
    fn make_secondary_entry(block_or_ebb_slot: u64) -> Vec<u8> {
        let mut buf = vec![0u8; 56];
        buf[0..8].copy_from_slice(&0u64.to_be_bytes()); // block_offset
        buf[8..10].copy_from_slice(&0u16.to_be_bytes()); // header_offset
        buf[10..12].copy_from_slice(&0u16.to_be_bytes()); // header_size
        buf[12..16].copy_from_slice(&0u32.to_be_bytes()); // checksum
                                                          // bytes [16..48] header_hash zeroed
        buf[48..56].copy_from_slice(&block_or_ebb_slot.to_be_bytes());
        buf
    }

    /// Regression: `read_chunk_first_block_slot` must NOT depend on the in-
    /// house block decoder. Mainnet replay bug (May 2026): 2722 of 8687
    /// chunks had Byron / edge-case bytes that the decoder couldn't parse,
    /// so the helper returned None for them; the binary search interpreted
    /// None as "search left" and landed at start_idx=0, triggering a
    /// full-chain replay from genesis (227K decode errors logged).
    ///
    /// The fix reads the slot from the secondary-index `block_or_ebb` field
    /// directly. This test plants a `.chunk` file with garbage bytes the
    /// decoder cannot parse and asserts the helper still returns the slot
    /// from the secondary index.
    #[test]
    fn test_read_chunk_first_block_slot_uses_secondary_index() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_num = 5u64;
        // Chunk file with garbage bytes (NOT valid CBOR / block) — decoder
        // would error on this if invoked.
        fs::write(
            dir.path().join(format!("{chunk_num:05}.chunk")),
            b"this is not a valid block",
        )
        .unwrap();
        // Secondary index with block_or_ebb = 12345.
        fs::write(
            dir.path().join(format!("{chunk_num:05}.secondary")),
            make_secondary_entry(12345),
        )
        .unwrap();

        let slot = read_chunk_first_block_slot(dir.path(), chunk_num, 21600).unwrap();
        assert_eq!(slot, Some(12345));
    }

    /// Variant: missing secondary file → fallback path engages, returns None
    /// for un-decodable chunk bytes (preserved legacy behaviour).
    #[test]
    fn test_read_chunk_first_block_slot_fallback_on_missing_secondary() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_num = 7u64;
        fs::write(
            dir.path().join(format!("{chunk_num:05}.chunk")),
            b"this is not a valid block",
        )
        .unwrap();
        // No secondary file present.
        let slot = read_chunk_first_block_slot(dir.path(), chunk_num, 21600).unwrap();
        assert_eq!(slot, None);
    }

    /// Variant: empty / short secondary file → fallback engages.
    #[test]
    fn test_read_chunk_first_block_slot_short_secondary_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_num = 9u64;
        fs::write(
            dir.path().join(format!("{chunk_num:05}.chunk")),
            b"this is not a valid block",
        )
        .unwrap();
        fs::write(
            dir.path().join(format!("{chunk_num:05}.secondary")),
            b"too short", // < 56 bytes
        )
        .unwrap();
        let slot = read_chunk_first_block_slot(dir.path(), chunk_num, 21600).unwrap();
        assert_eq!(slot, None);
    }
}
