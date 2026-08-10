//! Read-only block storage over Cardano immutable chunk files.
//!
//! Provides O(1) block lookups by hash and sequential slot-based queries
//! over the chunk files produced by Mithril snapshot import or the node
//! itself. Chunk files use the same on-disk format as cardano-node's
//! ImmutableDB (`.chunk` + `.secondary` index files).
//!
//! On startup, builds an in-memory hash index from secondary index files.
//! Slot-based queries use binary search over per-chunk metadata followed
//! by a secondary index scan within the target chunk.
//!
//! ## I/O backends
//!
//! By default, chunk file reads use `memmap2`.  On Linux, enable the
//! `io-uring` feature for kernel-bypassed async I/O via `io_uring`,
//! which improves throughput on NVMe storage for large sequential scans.

use crate::block_index::{BlockIndex, BlockLocation, InMemoryBlockIndex};
use crate::chunk_reader::{self, ChunkReaderTrait};
use crate::config::ImmutableConfig;
use dugite_primitives::hash::Hash32;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{debug, warn};

/// Read a CRC32 checksum from bytes 12..16 of a secondary index entry.
///
/// Returns 0 if the field is all zeros (legacy entries without CRC).
#[inline]
fn read_crc32_from_entry(data: &[u8]) -> u32 {
    if data.len() < 16 {
        return 0;
    }
    u32::from_be_bytes([data[12], data[13], data[14], data[15]])
}

/// Secondary index entry size in bytes.
const SECONDARY_ENTRY_SIZE: usize = 56;

#[derive(Error, Debug)]
pub enum ImmutableDBError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Malformed secondary index entry in chunk {chunk}: {reason}")]
    MalformedSecondaryEntry { chunk: u64, reason: String },
    #[error(
        "inconsistent chunk {chunk:05} in ImmutableDB: {reason}. Refusing to \
         open with a hole below the tip (issues #926/#928) — restore the \
         damaged chunk (e.g. `dugite-node mithril-import`) or remove the \
         damaged chunk files manually"
    )]
    InconsistentChunk { chunk: u64, reason: String },
}

/// Clean-shutdown marker file name (issue #928).
///
/// Present iff the last write session ended with a graceful [`ImmutableDB::flush`].
/// Removed when the DB is opened for writing (entering write mode means the
/// on-disk state is no longer known-clean until the next flush). Its absence
/// at open forces a rebuild of the persistent mmap hash index, whose entries
/// can otherwise reflect torn OS page writeback from a killed process.
const CLEAN_MARKER: &str = "clean";

/// Records the slots-per-chunk this database was laid out with.
///
/// Not a cardano-node file — consensus needs none, because it recomputes the
/// chunk size from the same genesis on every open and a wrong genesis fails
/// earlier. dugite substitutes a default `k` when Byron genesis will not load,
/// so the value it would use is not guaranteed to be the one already on disk.
const CHUNK_SIZE_MARKER: &str = "chunk_size";

/// Read a big-endian u64 from an 8-byte slice without panicking.
///
/// Returns `None` if the slice is not exactly 8 bytes.
#[inline]
fn read_be_u64(data: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = data.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

/// Per-chunk metadata for slot-based lookups.
#[derive(Debug, Clone)]
struct ChunkMeta {
    chunk_num: u64,
    first_slot: u64,
    last_slot: u64,
}

/// Active chunk being written to.
struct ActiveChunk {
    /// Chunk number, always `slot / chunk_size` for every block inside it.
    ///
    /// This is the ImmutableDB's central invariant and the reason cardano-node
    /// can locate a block without consulting any index: given a slot it
    /// computes the chunk number arithmetically and opens that file. The
    /// chunk's first slot is therefore `chunk_num * chunk_size` — derived, not
    /// carried, so the two can never drift (issue #1081).
    chunk_num: u64,
    chunk_file: std::io::BufWriter<std::fs::File>,
    /// Incrementally-appended on-disk secondary index (issue #926).
    ///
    /// Every `append_block` writes the 56-byte entry here immediately
    /// (unbuffered — one small syscall per block, alongside the block
    /// write). Haskell's ImmutableDB appends its index files per block the
    /// same way; keeping the index memory-only until flush() is what lost
    /// ten hours of index in the 2026-07-28 incident. A crash now loses at
    /// most the OS-buffered tail, which open-time reconciliation truncates.
    secondary_file: std::fs::File,
    secondary_entries: Vec<SecondaryEntry>,
    current_offset: u64,
    /// In-memory block data for the active chunk (not yet readable via memmap).
    /// Keyed by block hash for O(1) lookup.
    pending_blocks: HashMap<Hash32, Vec<u8>>,
}

/// A buffered secondary index entry (written on finalize or flush).
#[derive(Clone)]
struct SecondaryEntry {
    block_offset: u64,
    header_hash: [u8; 32],
    /// The block's absolute slot, ALWAYS — including for an EBB.
    ///
    /// The on-disk `blockOrEBB` field is a union and holds the EPOCH NUMBER
    /// for an EBB; [`Self::encode`] performs that substitution. Keeping the
    /// real slot in memory means slot-ordered reads and chunk slot ranges do
    /// not have to know which entries are boundary blocks.
    slot: u64,
    /// CRC32 checksum of the block CBOR data (0 for legacy entries).
    checksum: u32,
    /// Byte offset from the start of the block CBOR to the header element.
    /// Used by db-sync for efficient header extraction. 0 if unknown.
    header_offset: u16,
    /// Byte size of the header CBOR element.
    /// Used by db-sync for efficient header extraction. 0 if unknown.
    header_size: u16,
    /// True if this is a Byron Epoch Boundary Block.
    /// Not serialized to disk — used for primary index generation.
    is_ebb: bool,
}

impl SecondaryEntry {
    /// Serialize for the `.secondary` file.
    ///
    /// `chunk_num` is needed because the on-disk `blockOrEBB` field carries the
    /// EPOCH NUMBER for a boundary block, and for Byron the epoch number and
    /// the chunk number are the same thing (the chunk size IS the Byron epoch
    /// length). Writing the EBB's slot instead is what put
    /// `…, 21600, 21600, …` in a dugite mainnet index, which cardano-node
    /// rejects as non-increasing (issue #1081).
    fn encode(&self, chunk_num: u64) -> [u8; SECONDARY_ENTRY_SIZE] {
        let block_or_ebb = if self.is_ebb { chunk_num } else { self.slot };
        let mut entry = [0u8; SECONDARY_ENTRY_SIZE];
        entry[0..8].copy_from_slice(&self.block_offset.to_be_bytes());
        // bytes 8..10: header_offset (u16 big-endian)
        entry[8..10].copy_from_slice(&self.header_offset.to_be_bytes());
        // bytes 10..12: header_size (u16 big-endian)
        entry[10..12].copy_from_slice(&self.header_size.to_be_bytes());
        // bytes 12..16: CRC32 checksum
        entry[12..16].copy_from_slice(&self.checksum.to_be_bytes());
        entry[16..48].copy_from_slice(&self.header_hash);
        entry[48..56].copy_from_slice(&block_or_ebb.to_be_bytes());
        entry
    }
}

/// Extract the byte offset and size of the block header within block CBOR.
///
/// Cardano post-Shelley blocks are encoded as:
///   `array(2) [era_id, array(N) [header, tx_bodies, witnesses, ...]]`
///
/// Byron blocks (era 0) use the same outer structure but with a different
/// inner layout; we still extract the first element of the inner structure.
///
/// This function performs minimal CBOR parsing — it only needs to skip the
/// outer array tag, the era_id integer, the inner array tag, and then read
/// the header element length. Returns `(header_offset, header_size)` or
/// `(0, 0)` if parsing fails.
fn extract_header_bounds(cbor: &[u8]) -> (u16, u16) {
    // Helper: decode a CBOR initial byte and return (major_type, argument, bytes_consumed).
    // For indefinite-length items (additional info 31), argument is 0.
    fn decode_cbor_head(data: &[u8]) -> Option<(u8, u64, usize)> {
        if data.is_empty() {
            return None;
        }
        let major = data[0] >> 5;
        let additional = data[0] & 0x1f;
        match additional {
            0..=23 => Some((major, additional as u64, 1)),
            24 => {
                if data.len() < 2 {
                    return None;
                }
                Some((major, data[1] as u64, 2))
            }
            25 => {
                if data.len() < 3 {
                    return None;
                }
                let val = u16::from_be_bytes([data[1], data[2]]);
                Some((major, val as u64, 3))
            }
            26 => {
                if data.len() < 5 {
                    return None;
                }
                let val = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                Some((major, val as u64, 5))
            }
            27 => {
                if data.len() < 9 {
                    return None;
                }
                let val = u64::from_be_bytes([
                    data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
                ]);
                Some((major, val, 9))
            }
            31 => Some((major, 0, 1)), // indefinite length
            _ => None,
        }
    }

    // Helper: skip one complete CBOR data item, returning bytes consumed.
    fn skip_cbor_item(data: &[u8]) -> Option<usize> {
        let (major, arg, head_len) = decode_cbor_head(data)?;
        match major {
            // 0: unsigned int, 1: negative int — head only
            0 | 1 => Some(head_len),
            // 2: byte string, 3: text string — head + arg bytes
            2 | 3 => Some(head_len + arg as usize),
            // 4: array — head + skip `arg` items
            4 => {
                let mut pos = head_len;
                for _ in 0..arg {
                    pos += skip_cbor_item(&data[pos..])?;
                }
                Some(pos)
            }
            // 5: map — head + skip 2*arg items (key+value pairs)
            5 => {
                let mut pos = head_len;
                for _ in 0..arg * 2 {
                    pos += skip_cbor_item(&data[pos..])?;
                }
                Some(pos)
            }
            // 6: tag — head + one nested item
            6 => {
                let nested = skip_cbor_item(&data[head_len..])?;
                Some(head_len + nested)
            }
            // 7: simple/float — head only
            7 => Some(head_len),
            _ => None,
        }
    }

    let result = (|| -> Option<(u16, u16)> {
        let mut pos = 0;

        // Outer: array(2) [era_id, block_body]
        let (major, _len, head_len) = decode_cbor_head(&cbor[pos..])?;
        if major != 4 {
            return None;
        }
        pos += head_len;

        // Skip era_id (unsigned integer)
        let era_skip = skip_cbor_item(&cbor[pos..])?;
        pos += era_skip;

        // Inner: array(N) [header, ...]
        let (major, _len, head_len) = decode_cbor_head(&cbor[pos..])?;
        if major != 4 {
            return None;
        }
        pos += head_len;

        // `pos` now points to the start of the header element.
        let header_start = pos;
        let header_skip = skip_cbor_item(&cbor[pos..])?;

        // Clamp to u16 range (headers should never exceed 64 KiB)
        let offset = u16::try_from(header_start).ok()?;
        let size = u16::try_from(header_skip).ok()?;
        Some((offset, size))
    })();

    result.unwrap_or((0, 0))
}

/// Storage backed by Cardano immutable chunk files.
///
/// Each chunk file (`.chunk`) stores raw block CBOR sequentially.
/// Secondary index files (`.secondary`) provide 56-byte entries with
/// block boundaries, header hashes, and slot numbers.
///
/// Supports both read-only mode (via `open`) and read-write mode
/// (via `open_for_writing`) with append-only writes.
pub struct ImmutableDB {
    dir: PathBuf,
    chunks: Vec<ChunkMeta>,
    block_index: BlockIndex,
    total_blocks: u64,
    tip_slot: u64,
    tip_hash: Hash32,
    tip_block_no: u64,
    /// Active chunk for writing.
    ///
    /// `None` in read-only mode, and also `None` in write mode until the first
    /// `append_block` — the chunk number is a function of the block's slot, so
    /// there is nothing to open before one arrives. Use [`Self::is_writable`]
    /// rather than this field to ask whether the DB accepts writes.
    active_chunk: Option<ActiveChunk>,
    /// Slots per chunk, fixed for the lifetime of the database.
    ///
    /// `None` in read-only mode. cardano-node derives this once from the Byron
    /// genesis (`10 * k`) and holds it constant across every era, so a Shelley
    /// epoch of 432000 slots spans 20 chunks of 21600 — see
    /// [`Self::open_for_writing`].
    chunk_size: Option<u64>,
    /// Highest chunk NUMBER present in the directory at open, whether or not it
    /// holds any blocks.
    ///
    /// Read from the filenames rather than from `chunks`, which skips chunks
    /// with an empty secondary index — those are exactly the gap-fillers a
    /// previous session wrote, and treating them as absent would re-create them
    /// and, worse, pick a stale tail to continue from.
    last_chunk_on_disk: Option<u64>,
    /// CRC32 checksums for blocks (hash -> checksum). Zero means no checksum (legacy).
    checksums: HashMap<Hash32, u32>,
}

impl ImmutableDB {
    /// Open an ImmutableDB from a directory of chunk files using default (in-memory) config.
    ///
    /// Scans all `.chunk` and `.secondary` files and builds an in-memory
    /// hash index for O(1) block lookups. For preview (~4M blocks) this
    /// uses ~300 MB of memory; mainnet will need an on-disk index.
    ///
    /// Opening reconciles every chunk's index against its data first
    /// (issues #926/#928, see [`Self::reconcile_chunks_on_disk`]): crash
    /// damage at the chain tail is repaired on disk (truncated/quarantined,
    /// like Haskell's ImmutableDB open-time validation), and damage below
    /// the tail refuses to open rather than serving a chain with a hole.
    pub fn open(dir: &Path) -> Result<Self, ImmutableDBError> {
        Self::open_with_config(dir, &ImmutableConfig::default())
    }

    /// Open an ImmutableDB from a directory of chunk files with the given config.
    pub fn open_with_config(
        dir: &Path,
        config: &ImmutableConfig,
    ) -> Result<Self, ImmutableDBError> {
        debug!(dir = %dir.display(), index_type = ?config.index_type, "Opening ImmutableDB");

        // #926/#928: make on-disk state self-consistent (or refuse) before
        // the scan below trusts any of it.
        Self::reconcile_chunks_on_disk(dir)?;

        let mut chunk_nums = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(num_str) = name_str.strip_suffix(".chunk") {
                if let Ok(num) = num_str.parse::<u64>() {
                    chunk_nums.push(num);
                }
            }
        }
        chunk_nums.sort();

        if chunk_nums.is_empty() {
            debug!("ImmutableDB: no chunk files found");
            let block_index = BlockIndex::new(config, dir)?;
            return Ok(ImmutableDB {
                dir: dir.to_path_buf(),
                chunks: Vec::new(),
                block_index,
                total_blocks: 0,
                tip_slot: 0,
                tip_hash: Hash32::ZERO,
                tip_block_no: 0,
                active_chunk: None,
                chunk_size: None,
                last_chunk_on_disk: None,
                checksums: HashMap::new(),
            });
        }

        // First pass: count total entries for pre-allocation
        let mut total_entry_count = 0usize;
        let mut chunks = Vec::with_capacity(chunk_nums.len());
        let mut total_blocks = 0u64;
        let mut tip_slot = 0u64;
        let mut tip_hash = Hash32::ZERO;

        // Collect all (hash, location) pairs for building the index
        let mut all_entries: Vec<(Hash32, BlockLocation)> = Vec::new();
        let mut checksums: HashMap<Hash32, u32> = HashMap::new();

        for &chunk_num in &chunk_nums {
            let secondary_path = dir.join(format!("{chunk_num:05}.secondary"));
            let chunk_path = dir.join(format!("{chunk_num:05}.chunk"));

            if !secondary_path.exists() || !chunk_path.exists() {
                continue;
            }

            let chunk_len = chunk_path.metadata()?.len();
            let secondary_data = fs::read(&secondary_path)?;
            let entry_count = secondary_data.len() / SECONDARY_ENTRY_SIZE;
            if entry_count == 0 {
                if !secondary_data.is_empty() {
                    warn!(
                        chunk = chunk_num,
                        bytes = secondary_data.len(),
                        "Secondary index too small for a single entry ({SECONDARY_ENTRY_SIZE} bytes required), skipping"
                    );
                }
                continue;
            }

            // Warn about trailing bytes that don't form a complete entry
            let remainder = secondary_data.len() % SECONDARY_ENTRY_SIZE;
            if remainder != 0 {
                warn!(
                    chunk = chunk_num,
                    total_bytes = secondary_data.len(),
                    trailing_bytes = remainder,
                    "Secondary index has trailing bytes (possibly truncated)"
                );
            }

            // Parse secondary index entries: (block_offset, header_hash, slot, checksum)
            let mut entries: Vec<(u64, [u8; 32], u64, u32)> = Vec::with_capacity(entry_count);
            let mut pos = 0;
            while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                let data = &secondary_data[pos..];
                let block_offset = match read_be_u64(&data[0..8]) {
                    Some(v) => v,
                    None => {
                        warn!(
                            chunk = chunk_num,
                            pos, "Malformed block_offset in secondary index, skipping entry"
                        );
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                let mut header_hash = [0u8; 32];
                header_hash.copy_from_slice(&data[16..48]);
                let checksum = read_crc32_from_entry(data);
                let block_or_ebb = match read_be_u64(&data[48..56]) {
                    Some(v) => v,
                    None => {
                        warn!(
                            chunk = chunk_num,
                            pos, "Malformed slot/ebb in secondary index, skipping entry"
                        );
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                entries.push((block_offset, header_hash, block_or_ebb, checksum));
                pos += SECONDARY_ENTRY_SIZE;
            }

            let mut first_slot = u64::MAX;
            let mut last_slot = 0u64;

            for i in 0..entries.len() {
                let (block_offset, header_hash, slot, checksum) = entries[i];
                let block_end = if i + 1 < entries.len() {
                    entries[i + 1].0
                } else {
                    chunk_len
                };

                let hash = Hash32::from_bytes(header_hash);
                all_entries.push((
                    hash,
                    BlockLocation {
                        chunk_num,
                        block_offset,
                        block_end,
                    },
                ));

                // Store non-zero checksums for read-time verification
                if checksum != 0 {
                    checksums.insert(hash, checksum);
                }

                if slot < first_slot {
                    first_slot = slot;
                }
                if slot > last_slot {
                    last_slot = slot;
                }
                if slot >= tip_slot {
                    tip_slot = slot;
                    tip_hash = hash;
                }
            }

            chunks.push(ChunkMeta {
                chunk_num,
                first_slot,
                last_slot,
            });
            total_blocks += entry_count as u64;
            total_entry_count += entry_count;
        }

        // Build the block index from collected entries
        let block_index = match config.index_type {
            crate::config::BlockIndexType::InMemory => {
                let mut idx = InMemoryBlockIndex::with_capacity(total_entry_count);
                for (hash, loc) in &all_entries {
                    idx.insert(*hash, *loc);
                }
                BlockIndex::InMemory(idx)
            }
            crate::config::BlockIndexType::Mmap => {
                // Reuse the existing mmap file only when the entry count
                // matches AND the last write session shut down cleanly.
                // After an unclean stop, mmap pages may have hit disk via OS
                // writeback in any order (issue #928): a stale index can
                // claim blocks no secondary entry backs, miss blocks, or
                // point at reconciled-away offsets — and the count gate
                // alone cannot see that. Rebuilding from the just-scanned
                // secondary entries restores ground truth.
                let clean_shutdown = dir.join(CLEAN_MARKER).exists();
                let mmap_path = dir.join("hash_index.dat");
                let reuse = if mmap_path.exists() && !clean_shutdown {
                    warn!(
                        "ImmutableDB: unclean shutdown detected (no clean \
                         marker) — rebuilding mmap block index from secondary \
                         entries (#928)"
                    );
                    None
                } else if mmap_path.exists() {
                    match crate::block_index::MmapBlockIndex::new(dir, config.mmap_load_factor) {
                        Ok(idx) if idx.count_matches(total_blocks) => {
                            debug!("Reusing existing mmap block index");
                            Some(idx)
                        }
                        Ok(_) => {
                            debug!("Mmap block index count mismatch, rebuilding");
                            None
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                };

                match reuse {
                    Some(idx) => BlockIndex::Mmap(idx),
                    None => {
                        let idx = crate::block_index::MmapBlockIndex::build_from_entries(
                            dir,
                            &all_entries,
                            config.mmap_load_factor,
                        )?;
                        BlockIndex::Mmap(idx)
                    }
                }
            }
        };

        debug!(
            chunks = chunks.len(),
            total_blocks,
            tip_slot,
            index_entries = block_index.len(),
            "ImmutableDB opened"
        );

        // tip.meta carries the tip block_no (not stored in secondary
        // entries) — but it is trusted ONLY when its (slot, hash) agree with
        // the last indexed entry (#928). In the 2026-07-28 incident a stale
        // tip.meta claimed a tip 38k slots past the indexed chain, seeding
        // `last_flushed_block_no` so the flusher believed blocks were
        // flushed that no index backed. On disagreement the indexed chain
        // wins: recover the block_no from the tip block itself and rewrite
        // tip.meta.
        let tip_block_no = match Self::read_tip_meta(dir) {
            Some((meta_slot, meta_hash, meta_bn))
                if meta_slot == tip_slot && meta_hash == tip_hash =>
            {
                meta_bn
            }
            Some((meta_slot, meta_hash, meta_bn)) if total_blocks > 0 => {
                let recovered = block_index
                    .lookup(&tip_hash)
                    .and_then(|loc| {
                        Self::read_range_std(dir, loc.chunk_num, loc.block_offset, loc.block_end)
                    })
                    .and_then(|cbor| {
                        dugite_serialization::extract_block_identity(&cbor)
                            .ok()
                            .map(|(_, bn, _)| bn.0)
                    });
                let clamped_bn = recovered.unwrap_or(total_blocks);
                warn!(
                    meta_slot,
                    meta_hash = %meta_hash.to_hex(),
                    meta_block_no = meta_bn,
                    indexed_slot = tip_slot,
                    indexed_hash = %tip_hash.to_hex(),
                    clamped_block_no = clamped_bn,
                    block_no_recovered_by_decode = recovered.is_some(),
                    "ImmutableDB: tip.meta disagrees with the indexed chain — \
                     preferring the indexed chain and rewriting tip.meta (#928)"
                );
                if let Err(e) = Self::write_tip_meta(dir, tip_slot, &tip_hash, clamped_bn) {
                    warn!(error = %e, "ImmutableDB: failed to rewrite clamped tip.meta");
                }
                clamped_bn
            }
            Some(_) => {
                // Stale tip.meta over an empty DB (everything reconciled
                // away): drop it so nothing downstream trusts it.
                warn!(
                    "ImmutableDB: removing stale tip.meta over an empty \
                     database (#928)"
                );
                let _ = fs::remove_file(dir.join("tip.meta"));
                0
            }
            None => total_blocks,
        };

        Ok(ImmutableDB {
            dir: dir.to_path_buf(),
            chunks,
            block_index,
            total_blocks,
            tip_slot,
            tip_hash,
            tip_block_no,
            active_chunk: None,
            chunk_size: None,
            // `chunk_nums` is the filename scan, so this counts gap-filling
            // chunks whose secondary index is empty and which are therefore
            // absent from `chunks`.
            last_chunk_on_disk: chunk_nums.last().copied(),
            checksums,
        })
    }

    /// Reconcile every on-disk chunk's secondary index against its data
    /// before the open scan trusts either (issues #926/#928).
    ///
    /// Policy (mirrors Haskell `ImmutableDB.Impl.Validation`, where chunk
    /// files are the source of truth and validation always repairs or
    /// truncates to a definite, validated tip — never a silent skip):
    ///
    /// - **Tail (highest-numbered) chunk** — full CRC verification of every
    ///   entry against the chunk bytes, with the last entry's true block end
    ///   recovered by CRC scan (the index stores offsets only). Invalid
    ///   entries and un-indexed data beyond the valid prefix are physically
    ///   truncated; the lost blocks are re-fetched by normal sync. A
    ///   non-empty tail chunk whose index verifies NOTHING is quarantined:
    ///   the data file is renamed to `.chunk.orphaned` (preserved for
    ///   forensics, out of the writer's namespace) and its indexes removed.
    ///   This is precisely the 2026-07-28 incident state — previously the
    ///   chunk was silently skipped and `open_for_writing` then reused its
    ///   number, `File::create` truncating live finalized-chain data.
    /// - **Body (non-tail) chunks** — cheap structural checks only (whole
    ///   56-byte entries, offsets strictly increasing and in-bounds). Any
    ///   violation is a hard [`ImmutableDBError::InconsistentChunk`]: a
    ///   damaged chunk below the tip means the served chain would have a
    ///   hole below the claimed tip, and per the crash-don't-diverge policy
    ///   dugite refuses to run in that state. (Read-time CRC verification
    ///   in `get_block` still guards block content.)
    /// - A `.secondary` without its `.chunk` is warned about and ignored;
    ///   empty (0-byte) chunk artifacts from a previous open are removed.
    fn reconcile_chunks_on_disk(dir: &Path) -> Result<(), ImmutableDBError> {
        let mut chunk_nums = Vec::new();
        let mut secondary_nums = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(num_str) = name_str.strip_suffix(".chunk") {
                if let Ok(num) = num_str.parse::<u64>() {
                    chunk_nums.push(num);
                }
            } else if let Some(num_str) = name_str.strip_suffix(".secondary") {
                if let Ok(num) = num_str.parse::<u64>() {
                    secondary_nums.push(num);
                }
            }
        }
        chunk_nums.sort();

        for &num in &secondary_nums {
            if !chunk_nums.contains(&num) {
                warn!(
                    chunk = num,
                    "ImmutableDB: secondary index without a chunk data file — ignoring"
                );
            }
        }

        // Tail-grade reconciliation cascades downward: when the tail chunk
        // is removed entirely (empty open artifact, quarantined orphan), the
        // next chunk down becomes the data tail and gets the same full CRC
        // treatment — a crash during a previous open's own reconciliation
        // must not leave the true tail with only body-grade checks. Pristine
        // finalized chunks make this cheap (one chunk read per cascade step,
        // and the cascade only continues while chunks are being removed).
        while let Some(&tail) = chunk_nums.last() {
            if Self::reconcile_tail_chunk(dir, tail)? {
                chunk_nums.pop();
                continue;
            }
            break;
        }
        if chunk_nums.is_empty() {
            return Ok(());
        }
        for &num in &chunk_nums[..chunk_nums.len() - 1] {
            Self::check_body_chunk(dir, num)?;
        }

        // Cross-chunk chain linkage (#926/#928): per-chunk validation cannot
        // see a hole BETWEEN chunks — the incident DB's chunk 05919 carried
        // 12 internally-CRC-valid entries while 38k slots below them were
        // missing, and tip.meta agreed with the orphan island, so every
        // per-chunk check passed. Haskell's validateChunk throws
        // ChunkFileDoesntFit when a chunk's first block does not chain onto
        // the previous chunk's tip; this is the dugite equivalent.
        Self::check_chunk_boundaries(dir, |cbor| {
            dugite_serialization::decode_block_minimal(cbor)
                .ok()
                .map(|b| *b.prev_hash().as_bytes())
        })
    }

    /// Verify that each indexed chunk's first block chains (prev_hash) onto
    /// the previous chunk's last indexed block.
    ///
    /// - Mismatch at the TAIL boundary: the tail chunk is an orphan island
    ///   above a hole — quarantine it (bounded loss; the tip falls back to
    ///   the previous chunk and sync re-fetches forward). This automates the
    ///   2026-07-28 incident recovery.
    /// - Mismatch deeper in the chain: hard error. Auto-truncating from a
    ///   deep break would discard an unbounded amount of chain; the operator
    ///   chooses (usually `mithril-import`).
    /// - A first block the decoder cannot read (e.g. an era gap) skips the
    ///   check with a warning — never destroy data on a decoder limitation.
    ///
    /// `decode_prev_hash` is injected so tests can exercise the policy
    /// without crafting real decodable blocks; production passes
    /// `decode_block_minimal`.
    fn check_chunk_boundaries<F>(dir: &Path, decode_prev_hash: F) -> Result<(), ImmutableDBError>
    where
        F: Fn(&[u8]) -> Option<[u8; 32]>,
    {
        // Re-list: the per-chunk passes above may have quarantined the tail.
        let mut nums: Vec<u64> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name();
            let name_str = name.to_string_lossy();
            if let Some(num_str) = name_str.strip_suffix(".chunk") {
                if let Ok(num) = num_str.parse::<u64>() {
                    if dir.join(format!("{num:05}.secondary")).exists() {
                        nums.push(num);
                    }
                }
            }
        }
        nums.sort();

        for w in nums.windows(2) {
            let (prev, next) = (w[0], w[1]);
            let prev_entries =
                Self::read_secondary_entries(&dir.join(format!("{prev:05}.secondary")))?;
            let next_entries =
                Self::read_secondary_entries(&dir.join(format!("{next:05}.secondary")))?;
            let (Some(&(_, _, prev_tip_hash)), Some(&(first_off, first_crc, _))) =
                (prev_entries.last(), next_entries.first())
            else {
                continue; // empty artifacts — nothing to link
            };

            // First block of `next`: its end is the second entry's offset,
            // or (single-entry chunk) recovered by CRC scan.
            let first_end = if let Some(&(second_off, _, _)) = next_entries.get(1) {
                second_off
            } else {
                let chunk_data = fs::read(dir.join(format!("{next:05}.chunk")))?;
                match Self::find_last_entry_end(&chunk_data, first_off as usize, first_crc) {
                    Some(e) => e as u64,
                    None => continue, // tail chunk already reconciled; be lenient
                }
            };
            let Some(first_block) = Self::read_range_std(dir, next, first_off, first_end) else {
                continue;
            };

            let Some(prev_hash) = decode_prev_hash(&first_block) else {
                warn!(
                    chunk = next,
                    "ImmutableDB: cannot decode the chunk's first block to \
                     verify chain linkage — skipping the boundary check"
                );
                continue;
            };

            if prev_hash == prev_tip_hash {
                continue;
            }

            let is_tail = next == *nums.last().unwrap();
            if is_tail {
                let chunk_len = dir
                    .join(format!("{next:05}.chunk"))
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0);
                warn!(
                    prev_chunk = prev,
                    prev_tip_hash = %Hash32::from_bytes(prev_tip_hash).to_hex(),
                    first_block_prev_hash = %Hash32::from_bytes(prev_hash).to_hex(),
                    "ImmutableDB: tail chunk does not chain onto the previous \
                     chunk — quarantining the orphan island above the hole (#926)"
                );
                Self::quarantine_tail_chunk(
                    dir,
                    next,
                    chunk_len,
                    "first block does not chain onto the previous chunk's tip",
                )?;
                return Ok(());
            }
            return Err(ImmutableDBError::InconsistentChunk {
                chunk: next,
                reason: format!(
                    "first block's prev_hash {} does not chain onto chunk \
                     {prev:05}'s tip {} — the chain has a hole below the tail",
                    Hash32::from_bytes(prev_hash).to_hex(),
                    Hash32::from_bytes(prev_tip_hash).to_hex()
                ),
            });
        }
        Ok(())
    }

    /// Parse a secondary index file into `(block_offset, crc, header_hash)`
    /// triples (whole entries only).
    fn read_secondary_entries(path: &Path) -> Result<Vec<(u64, u32, [u8; 32])>, ImmutableDBError> {
        let data = fs::read(path)?;
        let mut out = Vec::with_capacity(data.len() / SECONDARY_ENTRY_SIZE);
        let mut pos = 0;
        while pos + SECONDARY_ENTRY_SIZE <= data.len() {
            let e = &data[pos..pos + SECONDARY_ENTRY_SIZE];
            let offset = read_be_u64(&e[0..8]).unwrap_or(u64::MAX);
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&e[16..48]);
            out.push((offset, read_crc32_from_entry(e), hash));
            pos += SECONDARY_ENTRY_SIZE;
        }
        Ok(out)
    }

    /// Structural check for a non-tail chunk (no data read; see
    /// [`Self::reconcile_chunks_on_disk`] for policy).
    fn check_body_chunk(dir: &Path, num: u64) -> Result<(), ImmutableDBError> {
        let chunk_path = dir.join(format!("{num:05}.chunk"));
        let secondary_path = dir.join(format!("{num:05}.secondary"));
        let chunk_len = chunk_path.metadata()?.len();

        if chunk_len == 0 {
            // Empty artifact from a prior `open_for_writing` — harmless.
            return Ok(());
        }
        if !secondary_path.exists() {
            return Err(ImmutableDBError::InconsistentChunk {
                chunk: num,
                reason: format!(
                    "chunk data present ({chunk_len} bytes) but its secondary \
                     index is missing below the chain tail"
                ),
            });
        }
        let secondary_data = fs::read(&secondary_path)?;
        if secondary_data.is_empty() || secondary_data.len() % SECONDARY_ENTRY_SIZE != 0 {
            return Err(ImmutableDBError::InconsistentChunk {
                chunk: num,
                reason: format!(
                    "secondary index is empty or torn ({} bytes) below the chain tail",
                    secondary_data.len()
                ),
            });
        }
        let mut prev_offset: Option<u64> = None;
        let mut pos = 0;
        while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
            let offset = read_be_u64(&secondary_data[pos..pos + 8]).unwrap_or(u64::MAX);
            if offset >= chunk_len || prev_offset.is_some_and(|p| offset <= p) {
                return Err(ImmutableDBError::InconsistentChunk {
                    chunk: num,
                    reason: format!(
                        "secondary entry at byte {pos} has block offset {offset} \
                         (chunk is {chunk_len} bytes, previous offset {prev_offset:?})"
                    ),
                });
            }
            prev_offset = Some(offset);
            pos += SECONDARY_ENTRY_SIZE;
        }
        Ok(())
    }

    /// Full CRC reconciliation + on-disk repair for the tail chunk (see
    /// [`Self::reconcile_chunks_on_disk`] for policy).
    ///
    /// Returns `true` when the chunk no longer exists afterwards (removed
    /// empty artifact or quarantined) — the caller then treats the next
    /// chunk down as the tail and reconciles it too.
    fn reconcile_tail_chunk(dir: &Path, num: u64) -> Result<bool, ImmutableDBError> {
        let chunk_path = dir.join(format!("{num:05}.chunk"));
        let secondary_path = dir.join(format!("{num:05}.secondary"));
        let primary_path = dir.join(format!("{num:05}.primary"));
        let chunk_len = chunk_path.metadata()?.len();

        let secondary_data = if secondary_path.exists() {
            fs::read(&secondary_path)?
        } else {
            Vec::new()
        };

        // No usable index at all.
        if secondary_data.len() < SECONDARY_ENTRY_SIZE {
            if chunk_len == 0 {
                // Fresh artifact of a previous open — remove quietly so the
                // chunk number is legitimately reusable.
                let _ = fs::remove_file(&chunk_path);
                let _ = fs::remove_file(&secondary_path);
                let _ = fs::remove_file(&primary_path);
                return Ok(true);
            }
            Self::quarantine_tail_chunk(dir, num, chunk_len, "no secondary index")?;
            return Ok(true);
        }

        // Torn trailing bytes (crash mid-entry-write): drop them.
        let whole = secondary_data.len() - secondary_data.len() % SECONDARY_ENTRY_SIZE;
        let had_torn_secondary_tail = whole != secondary_data.len();
        if had_torn_secondary_tail {
            warn!(
                chunk = num,
                torn_bytes = secondary_data.len() - whole,
                "ImmutableDB: truncating torn trailing bytes from tail chunk's \
                 secondary index"
            );
        }
        let secondary_data = &secondary_data[..whole];

        let mut entries: Vec<(u64, u32)> = Vec::with_capacity(whole / SECONDARY_ENTRY_SIZE);
        let mut pos = 0;
        while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
            let data = &secondary_data[pos..pos + SECONDARY_ENTRY_SIZE];
            let offset = read_be_u64(&data[0..8]).unwrap_or(u64::MAX);
            entries.push((offset, read_crc32_from_entry(data)));
            pos += SECONDARY_ENTRY_SIZE;
        }

        let chunk_data = fs::read(&chunk_path)?;

        // Walk the valid prefix: each entry's data must be in-bounds,
        // contiguous with its neighbour, and CRC-match (unless legacy CRC 0).
        let mut valid = 0usize;
        let mut data_end = 0u64;
        for i in 0..entries.len() {
            let (start, crc) = entries[i];
            if start != data_end {
                break; // non-contiguous / out-of-order — everything after is suspect
            }
            let end = if i + 1 < entries.len() {
                entries[i + 1].0
            } else {
                match Self::find_last_entry_end(&chunk_data, start as usize, crc) {
                    Some(e) => e as u64,
                    None => break,
                }
            };
            if start >= end || end > chunk_data.len() as u64 {
                break;
            }
            if crc != 0 && crc32fast::hash(&chunk_data[start as usize..end as usize]) != crc {
                break;
            }
            valid = i + 1;
            data_end = end;
        }

        if valid == 0 {
            if chunk_len == 0 {
                let _ = fs::remove_file(&chunk_path);
                let _ = fs::remove_file(&secondary_path);
                let _ = fs::remove_file(&primary_path);
                warn!(
                    chunk = num,
                    "ImmutableDB: removed empty tail chunk with unverifiable index"
                );
                return Ok(true);
            }
            Self::quarantine_tail_chunk(
                dir,
                num,
                chunk_len,
                "no secondary entry verifies against the chunk data",
            )?;
            return Ok(true);
        }

        let pristine = valid == entries.len() && data_end == chunk_len && !had_torn_secondary_tail;
        if pristine {
            return Ok(false);
        }

        warn!(
            chunk = num,
            valid_entries = valid,
            dropped_entries = entries.len() - valid,
            data_end,
            chunk_len,
            truncated_tail_bytes = chunk_len.saturating_sub(data_end),
            "ImmutableDB: reconciling tail chunk — truncating to the verified \
             prefix; dropped blocks will be re-fetched from peers (#926)"
        );

        // Truncate index first (a shorter index over longer data is the
        // recoverable direction), then the data file.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&secondary_path)?;
        file.set_len((valid * SECONDARY_ENTRY_SIZE) as u64)?;
        file.sync_all()?;
        let file = std::fs::OpenOptions::new().write(true).open(&chunk_path)?;
        file.set_len(data_end)?;
        file.sync_all()?;
        // The primary index no longer matches; remove it (rebuilt on the
        // next finalize/flush for the active chunk; historical interop
        // consumers reconstruct from the secondary).
        let _ = fs::remove_file(&primary_path);
        Ok(false)
    }

    /// Quarantine a tail chunk whose data cannot be served: rename the data
    /// file out of the writer's namespace and drop its indexes.
    fn quarantine_tail_chunk(
        dir: &Path,
        num: u64,
        chunk_len: u64,
        reason: &str,
    ) -> Result<(), ImmutableDBError> {
        let chunk_path = dir.join(format!("{num:05}.chunk"));
        let orphan_path = dir.join(format!("{num:05}.chunk.orphaned"));
        warn!(
            chunk = num,
            chunk_len,
            reason,
            orphan = %orphan_path.display(),
            "ImmutableDB: quarantining unservable tail chunk — data preserved, \
             blocks will be re-fetched from peers (#926)"
        );
        fs::rename(&chunk_path, &orphan_path)?;
        let _ = fs::remove_file(dir.join(format!("{num:05}.secondary")));
        let _ = fs::remove_file(dir.join(format!("{num:05}.primary")));
        Ok(())
    }

    /// Recover the last indexed block's true end offset by CRC scan.
    ///
    /// The secondary index stores block start offsets only; the last entry's
    /// end is conventionally "end of chunk", which is wrong exactly when a
    /// crash left un-indexed data after it. Scan forward from `start`,
    /// accepting the first position whose running CRC32 matches the stored
    /// checksum AND which is either end-of-data or the start of another
    /// stored block (every stored block is the multi-era envelope
    /// `array(2) [...]`, first byte 0x82 — Byron EBBs included). Legacy
    /// entries (CRC 0) cannot be delimited; fall back to end-of-chunk as
    /// before (finalized legacy chunks have no tails).
    fn find_last_entry_end(chunk_data: &[u8], start: usize, expected_crc: u32) -> Option<usize> {
        if expected_crc == 0 {
            return Some(chunk_data.len());
        }
        if start >= chunk_data.len() {
            return None;
        }
        let mut hasher = crc32fast::Hasher::new();
        let mut pos = start;
        while pos < chunk_data.len() {
            hasher.update(&chunk_data[pos..pos + 1]);
            pos += 1;
            let at_boundary = pos == chunk_data.len() || chunk_data[pos] == 0x82;
            if at_boundary && hasher.clone().finalize() == expected_crc {
                return Some(pos);
            }
        }
        None
    }

    /// Get block CBOR by header hash.
    ///
    /// Verifies CRC32 checksum if one was stored in the secondary index.
    /// On mismatch, logs an error and returns `None` so the caller can
    /// re-fetch from a peer rather than silently propagating corrupt data.
    /// Legacy entries (no stored CRC) are returned without verification.
    pub fn get_block(&self, hash: &Hash32) -> Option<Vec<u8>> {
        // Check active chunk's pending blocks first (not yet on disk via memmap)
        if let Some(ref active) = self.active_chunk {
            if let Some(cbor) = active.pending_blocks.get(hash) {
                return Some(Vec::clone(cbor));
            }
        }
        let loc = self.block_index.lookup(hash)?;
        let cbor = self.read_block_at(&loc)?;

        // Verify CRC32 if we have a stored checksum
        if let Some(&expected_crc) = self.checksums.get(hash) {
            let actual_crc = crc32fast::hash(&cbor);
            if actual_crc != expected_crc {
                warn!(
                    hash = %hash.to_hex(),
                    expected = expected_crc,
                    actual = actual_crc,
                    "CRC32 mismatch for block — rejecting corrupted data"
                );
                return None;
            }
        }

        Some(cbor)
    }

    /// Check if a block exists by header hash.
    pub fn has_block(&self, hash: &Hash32) -> bool {
        self.block_index.contains(hash)
    }

    /// Check that a block exists AND its bytes actually verify (issue #928).
    ///
    /// `has_block` answers from the index alone, which is exactly what a
    /// stale index gets wrong: the flush path used it to skip re-flushing
    /// blocks whose immutable copy was unreachable, silently dropping them
    /// when the volatile copy was then discarded. This variant reads the
    /// block and CRC-verifies it (via [`Self::get_block`]) — a phantom index
    /// entry or corrupt backing bytes count as absent, so the caller
    /// re-appends the block instead of losing it. Costs one block read; use
    /// on decision paths where a false "present" loses data, not on hot
    /// serving paths.
    pub fn has_verified_block(&self, hash: &Hash32) -> bool {
        self.get_block(hash).is_some()
    }

    /// Absolute slot of an immutable block, by header hash (#908).
    ///
    /// Every block in the ImmutableDB is by construction on the canonical chain
    /// (the layer is append-only and only ever receives k-deep blocks), so this
    /// is the authoritative "what slot is this canonical hash at?" lookup that
    /// the ChainSync server needs to validate a client's `MsgFindIntersect`
    /// point.
    ///
    /// Resolution: the block index gives the chunk and byte offset; the chunk's
    /// secondary index maps that offset to the slot. Both the active (unflushed)
    /// chunk's in-memory entries and on-disk chunks are covered.
    ///
    /// Returns `None` for an unknown hash — including a VolatileDB-only block,
    /// which the caller must resolve against the volatile selected chain.
    ///
    /// Byron EBBs are excluded: per the Haskell convention the secondary index's
    /// slot field holds the **epoch number** for an EBB, not an absolute slot,
    /// so it is not a value a caller may compare against a wire `Point`. (The
    /// on-disk entry does not carry the EBB flag, but an epoch number can never
    /// equal the EBB's absolute slot for any epoch past 0, so such a point is
    /// rejected by the caller's slot-match check anyway — the same outcome.)
    pub fn slot_of(&self, hash: &Hash32) -> Option<u64> {
        // Active chunk: entries are still in memory, not yet in a .secondary file.
        if let Some(ref active) = self.active_chunk {
            if let Some(e) = active
                .secondary_entries
                .iter()
                .find(|e| &Hash32::from_bytes(e.header_hash) == hash)
            {
                return if e.is_ebb { None } else { Some(e.slot) };
            }
        }

        let loc = self.block_index.lookup(hash)?;
        let secondary_path = self.dir.join(format!("{:05}.secondary", loc.chunk_num));
        let secondary_data = fs::read(&secondary_path).ok()?;

        let mut pos = 0;
        while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
            let entry = &secondary_data[pos..];
            if read_be_u64(&entry[0..8]) == Some(loc.block_offset)
                && entry[16..48] == hash.as_bytes()[..]
            {
                return read_be_u64(&entry[48..56]);
            }
            pos += SECONDARY_ENTRY_SIZE;
        }
        None
    }

    /// Total number of blocks across all chunk files.
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    /// Tip slot of the immutable chain.
    pub fn tip_slot(&self) -> u64 {
        self.tip_slot
    }

    /// Tip hash of the immutable chain.
    pub fn tip_hash(&self) -> Hash32 {
        self.tip_hash
    }

    /// Tip block number of the immutable chain.
    pub fn tip_block_no(&self) -> u64 {
        self.tip_block_no
    }

    /// Returns `true` if any present chunk's observed slot range contains
    /// `slot` (i.e., `first_slot <= slot <= last_slot` for some chunk).
    ///
    /// This is the inverse of "does `slot` fall in a chunk-level gap". A
    /// chunk-level gap arises when the mithril aggregator's main archive
    /// (covering chunks 0..immutable_file_number) plus the ancillary's
    /// partial chunk at the tip have a missing chunk between them — the
    /// chunk for the ledger snapshot's tip slot is not delivered by either
    /// archive even though that chunk's nominal slot range is between two
    /// present chunks.
    ///
    /// Callers use this to distinguish "empty slot inside a present chunk"
    /// (a real fork — the canonical chain says no block existed at that slot)
    /// from "slot is in a missing chunk's range" (we can't conclude anything;
    /// trust the haskell-ledger snapshot and let ChainSync backfill).
    pub fn chunk_covers_slot(&self, slot: u64) -> bool {
        self.chunks
            .iter()
            .any(|c| c.first_slot <= slot && slot <= c.last_slot)
    }

    /// Directory containing the chunk files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Read tip metadata from a `tip.meta` file in the directory.
    fn read_tip_meta(dir: &Path) -> Option<(u64, Hash32, u64)> {
        let meta_path = dir.join("tip.meta");
        let data = fs::read(&meta_path).ok()?;
        if data.len() < 48 {
            return None;
        }
        let slot = u64::from_be_bytes(data[0..8].try_into().ok()?);
        let mut hash_bytes = [0u8; 32];
        hash_bytes.copy_from_slice(&data[8..40]);
        let block_no = u64::from_be_bytes(data[40..48].try_into().ok()?);
        Some((slot, Hash32::from_bytes(hash_bytes), block_no))
    }

    /// Write tip metadata to a `tip.meta` file.
    fn write_tip_meta(
        dir: &Path,
        slot: u64,
        hash: &Hash32,
        block_no: u64,
    ) -> Result<(), ImmutableDBError> {
        let meta_path = dir.join("tip.meta");
        let mut data = [0u8; 48];
        data[0..8].copy_from_slice(&slot.to_be_bytes());
        data[8..40].copy_from_slice(hash.as_bytes());
        data[40..48].copy_from_slice(&block_no.to_be_bytes());
        fs::write(&meta_path, data)?;
        Ok(())
    }

    /// Get the first block strictly after `after_slot`.
    ///
    /// Uses binary search on chunk metadata to find the starting chunk,
    /// then scans the secondary index within that chunk.
    pub fn get_next_block_after_slot(&self, after_slot: u64) -> Option<(u64, Hash32, Vec<u8>)> {
        // Find the first chunk whose last_slot > after_slot
        let start_idx = self.chunks.partition_point(|c| c.last_slot <= after_slot);

        for chunk_meta in &self.chunks[start_idx..] {
            let secondary_path = self
                .dir
                .join(format!("{:05}.secondary", chunk_meta.chunk_num));
            let secondary_data = match fs::read(&secondary_path) {
                Ok(data) => data,
                Err(_) => continue,
            };

            let chunk_path = self.dir.join(format!("{:05}.chunk", chunk_meta.chunk_num));
            let chunk_len = match chunk_path.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            let mut pos = 0;
            while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                let data = &secondary_data[pos..];
                let block_offset = match read_be_u64(&data[0..8]) {
                    Some(v) => v,
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                let mut header_hash = [0u8; 32];
                header_hash.copy_from_slice(&data[16..48]);
                let slot = match read_be_u64(&data[48..56]) {
                    Some(v) => self.entry_slot(chunk_meta.chunk_num, v, pos == 0),
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };

                if slot > after_slot {
                    // Compute block end from next entry or chunk length.
                    // Only read the next entry's offset if a full entry exists
                    // (not just trailing garbage bytes).
                    let next_pos = pos + SECONDARY_ENTRY_SIZE;
                    let block_end = if next_pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                        read_be_u64(&secondary_data[next_pos..next_pos + 8]).unwrap_or(chunk_len)
                    } else {
                        chunk_len
                    };

                    let loc = BlockLocation {
                        chunk_num: chunk_meta.chunk_num,
                        block_offset,
                        block_end,
                    };
                    if let Some(cbor) = self.read_block_at(&loc) {
                        return Some((slot, Hash32::from_bytes(header_hash), cbor));
                    }
                }

                pos += SECONDARY_ENTRY_SIZE;
            }
        }

        None
    }

    /// Get the chain-order successor of the block identified by `(slot, hash)`.
    ///
    /// A Byron Epoch Boundary Block (EBB) shares its absolute slot with the
    /// first main block of the epoch, so a slot-only cursor
    /// ([`get_next_block_after_slot`]) cannot step from the EBB to the
    /// same-slot main block — it would skip it.  This point-aware lookup
    /// locates the entry for `(slot, hash)` in the secondary index and
    /// returns the NEXT entry in chain order (same chunk, or the first entry
    /// of a following chunk), mirroring cardano-node's by-point iterators
    /// (`Ouroboros.Consensus.Storage.ImmutableDB.Impl.getSlotInfo`
    /// disambiguates same-slot EBB/main pairs by header hash).
    ///
    /// When `(slot, hash)` is not found (fork block, volatile-only cursor,
    /// or stale hash), falls back to the strict `slot >` lookup, which is
    /// the pre-existing behavior.
    pub fn get_next_block_after_point(
        &self,
        slot: u64,
        hash: &Hash32,
    ) -> Option<(u64, Hash32, Vec<u8>)> {
        // Chunks are chain-ordered with non-decreasing slots; the cursor
        // entry lives in the first chunk whose last_slot >= slot.
        let start_idx = self.chunks.partition_point(|c| c.last_slot < slot);

        // True once the cursor entry has been located — the next valid
        // entry (possibly in a later chunk) is the result.
        let mut serve_next = false;

        for chunk_meta in &self.chunks[start_idx..] {
            let secondary_path = self
                .dir
                .join(format!("{:05}.secondary", chunk_meta.chunk_num));
            let secondary_data = match fs::read(&secondary_path) {
                Ok(data) => data,
                Err(_) => continue,
            };

            let chunk_path = self.dir.join(format!("{:05}.chunk", chunk_meta.chunk_num));
            let chunk_len = match chunk_path.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            let mut pos = 0;
            while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                let data = &secondary_data[pos..];
                let (Some(block_offset), Some(block_or_ebb)) =
                    (read_be_u64(&data[0..8]), read_be_u64(&data[48..56]))
                else {
                    pos += SECONDARY_ENTRY_SIZE;
                    continue;
                };
                let entry_slot = self.entry_slot(chunk_meta.chunk_num, block_or_ebb, pos == 0);

                if serve_next {
                    let next_pos = pos + SECONDARY_ENTRY_SIZE;
                    let block_end = if next_pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                        read_be_u64(&secondary_data[next_pos..next_pos + 8]).unwrap_or(chunk_len)
                    } else {
                        chunk_len
                    };
                    let loc = BlockLocation {
                        chunk_num: chunk_meta.chunk_num,
                        block_offset,
                        block_end,
                    };
                    let mut header_hash = [0u8; 32];
                    header_hash.copy_from_slice(&data[16..48]);
                    if let Some(cbor) = self.read_block_at(&loc) {
                        return Some((entry_slot, Hash32::from_bytes(header_hash), cbor));
                    }
                    // Unreadable block — keep scanning (matches the slot
                    // cursor's skip-on-read-failure behavior).
                } else if entry_slot == slot && data[16..48] == *hash.as_bytes() {
                    serve_next = true;
                } else if entry_slot > slot {
                    // Scanned past the cursor slot without finding the hash —
                    // the cursor block is not in the immutable chain.
                    return self.get_next_block_after_slot(slot);
                }

                pos += SECONDARY_ENTRY_SIZE;
            }
        }

        if serve_next {
            // Cursor is the immutable tip — no successor here.
            return None;
        }
        // Cursor slot beyond every chunk, or hash never seen.
        self.get_next_block_after_slot(slot)
    }

    /// Get the first block at or after a given slot (inclusive `>=` comparison).
    ///
    /// Unlike [`get_next_block_after_slot`] which uses strict `>`, this method
    /// includes blocks at exactly `slot`.  Used to serve the genesis EBB at
    /// slot 0 when the ChainSync cursor is at Origin.
    pub fn get_block_at_or_after_slot(&self, slot: u64) -> Option<(u64, Hash32, Vec<u8>)> {
        // For slot 0, start from the very first chunk.
        // For slot > 0, we can still use the partition_point optimisation but
        // with a less-strict predicate: skip chunks whose last_slot < slot
        // (not <=).
        let start_idx = if slot == 0 {
            0
        } else {
            self.chunks.partition_point(|c| c.last_slot < slot)
        };

        for chunk_meta in &self.chunks[start_idx..] {
            let secondary_path = self
                .dir
                .join(format!("{:05}.secondary", chunk_meta.chunk_num));
            let secondary_data = match fs::read(&secondary_path) {
                Ok(data) => data,
                Err(_) => continue,
            };

            let chunk_path = self.dir.join(format!("{:05}.chunk", chunk_meta.chunk_num));
            let chunk_len = match chunk_path.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            let mut pos = 0;
            while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                let data = &secondary_data[pos..];
                let block_offset = match read_be_u64(&data[0..8]) {
                    Some(v) => v,
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                let mut header_hash = [0u8; 32];
                header_hash.copy_from_slice(&data[16..48]);
                let entry_slot = match read_be_u64(&data[48..56]) {
                    Some(v) => self.entry_slot(chunk_meta.chunk_num, v, pos == 0),
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };

                // Inclusive: entry_slot >= slot (not strict >).
                if entry_slot >= slot {
                    let next_pos = pos + SECONDARY_ENTRY_SIZE;
                    let block_end = if next_pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                        read_be_u64(&secondary_data[next_pos..next_pos + 8]).unwrap_or(chunk_len)
                    } else {
                        chunk_len
                    };

                    let loc = BlockLocation {
                        chunk_num: chunk_meta.chunk_num,
                        block_offset,
                        block_end,
                    };
                    if let Some(cbor) = self.read_block_at(&loc) {
                        return Some((entry_slot, Hash32::from_bytes(header_hash), cbor));
                    }
                }

                pos += SECONDARY_ENTRY_SIZE;
            }
        }

        None
    }

    /// Get blocks in slot range `[from_slot, to_slot]` inclusive.
    ///
    /// Uses the batched [`ChunkReader::read_ranges`] API to read all
    /// matching blocks from each chunk file in a single I/O operation
    /// when possible (e.g. io_uring submits all reads at once).
    pub fn get_blocks_in_slot_range(&self, from_slot: u64, to_slot: u64) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let reader = chunk_reader::default_reader();

        let start_idx = self.chunks.partition_point(|c| c.last_slot < from_slot);

        for chunk_meta in &self.chunks[start_idx..] {
            if chunk_meta.first_slot > to_slot {
                break;
            }

            let secondary_path = self
                .dir
                .join(format!("{:05}.secondary", chunk_meta.chunk_num));
            let chunk_path = self.dir.join(format!("{:05}.chunk", chunk_meta.chunk_num));

            let secondary_data = match fs::read(&secondary_path) {
                Ok(data) => data,
                Err(_) => continue,
            };
            let chunk_len = match chunk_path.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            // Parse all entries to get offsets and slots
            let entry_count = secondary_data.len() / SECONDARY_ENTRY_SIZE;
            let mut entries: Vec<(u64, u64)> = Vec::with_capacity(entry_count);
            let mut pos = 0;
            while pos + SECONDARY_ENTRY_SIZE <= secondary_data.len() {
                let data = &secondary_data[pos..];
                let block_offset = match read_be_u64(&data[0..8]) {
                    Some(v) => v,
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                let slot = match read_be_u64(&data[48..56]) {
                    Some(v) => self.entry_slot(chunk_meta.chunk_num, v, pos == 0),
                    None => {
                        pos += SECONDARY_ENTRY_SIZE;
                        continue;
                    }
                };
                entries.push((block_offset, slot));
                pos += SECONDARY_ENTRY_SIZE;
            }

            // Collect the (offset, length) ranges for blocks in the slot window.
            let mut ranges: Vec<(u64, usize)> = Vec::new();
            for i in 0..entries.len() {
                let (block_offset, slot) = entries[i];
                if slot < from_slot {
                    continue;
                }
                if slot > to_slot {
                    break;
                }

                let block_end = if i + 1 < entries.len() {
                    entries[i + 1].0
                } else {
                    chunk_len
                };
                if block_offset < block_end {
                    ranges.push((block_offset, (block_end - block_offset) as usize));
                }
            }

            // Batch-read all selected ranges from this chunk file.
            let batch = reader.read_ranges(&chunk_path, &ranges);
            for block in batch.into_iter().flatten() {
                result.push(block);
            }
        }

        result
    }

    /// Open an ImmutableDB for writing, appending to existing chunk files.
    ///
    /// Scans existing chunks read-only (like `open`), then enters write mode.
    /// No chunk file is created here: which chunk a block belongs to is a
    /// function of its slot, so the first `append_block` opens it.
    ///
    /// # The chunk size, and why it is not the epoch length
    ///
    /// cardano-node derives the chunk size ONCE, from the Byron genesis, and
    /// holds it constant for the entire chain regardless of era — mainnet and
    /// preprod use 21600 slots (`10 * k`, k = 2160) and preview 4320 (k = 432).
    /// A Shelley epoch of 432000 slots therefore spans **20 chunks**, not one.
    ///
    /// Naming a chunk after the epoch (which dugite did until #1081) makes the
    /// two agree only for Byron, where the epoch length happens to equal the
    /// chunk size. Everywhere else the numbering drifts, and consensus — which
    /// locates a block by computing `slot / chunk_size` and opening that file —
    /// asks for a chunk that was never written.
    ///
    /// # Parameters
    /// - `chunk_size`: slots per chunk, `10 * k` from the Byron genesis
    pub fn open_for_writing(dir: &Path, chunk_size: u64) -> Result<Self, ImmutableDBError> {
        Self::open_for_writing_with_config(dir, &ImmutableConfig::default(), chunk_size)
    }

    /// Open an ImmutableDB for writing with the given config.
    ///
    /// See [`Self::open_for_writing`] for the meaning of `chunk_size`.
    pub fn open_for_writing_with_config(
        dir: &Path,
        config: &ImmutableConfig,
        chunk_size: u64,
    ) -> Result<Self, ImmutableDBError> {
        if chunk_size == 0 {
            return Err(ImmutableDBError::Io(std::io::Error::other(
                "ImmutableDB chunk_size must be non-zero",
            )));
        }

        let mut db = Self::open_with_config(dir, config)?;

        // Entering write mode: the on-disk state is no longer known-clean
        // until the next graceful flush() (issue #928).
        let _ = fs::remove_file(dir.join(CLEAN_MARKER));

        // The chunk size decides the on-disk layout, so it must be the same on
        // every open of the same database. It is derived from the Byron
        // genesis `k`, and dugite falls back to a DEFAULT k when that file
        // cannot be read — which would silently re-lay the database at 21600
        // slots per chunk on a network that uses 4320. Record it once and
        // refuse a mismatch, so a misconfiguration is a startup error rather
        // than a database no implementation can read.
        let recorded_path = dir.join(CHUNK_SIZE_MARKER);
        match fs::read_to_string(&recorded_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            Some(recorded) if recorded != chunk_size => {
                return Err(ImmutableDBError::InconsistentChunk {
                    chunk: 0,
                    reason: format!(
                        "this database was written with {recorded} slots per chunk but \
                         the node is configured for {chunk_size} (Byron genesis k = {}). \
                         Check the Byron genesis file for this network; opening would \
                         produce a layout no implementation can read",
                        chunk_size / 10
                    ),
                });
            }
            Some(_) => {}
            None => fs::write(&recorded_path, chunk_size.to_string())?,
        }

        db.chunk_size = Some(chunk_size);

        // A database written before #1081 carries chunks whose number does not
        // match their contents' slots. Reads still work (they go through the
        // hash index), but appending cannot repair the existing layout, and
        // cardano-node will not open it. Say so once, at open, naming the
        // remedy — silently producing a half-correct database is how this
        // defect stayed invisible for so long.
        if let Some(tail) = db.chunks.last() {
            let expected = tail.last_slot / chunk_size;
            if expected != tail.chunk_num && tail.last_slot != tail.chunk_num {
                warn!(
                    chunk = tail.chunk_num,
                    last_slot = tail.last_slot,
                    consensus_would_look_in = expected,
                    "ImmutableDB: chunk layout predates the slot-derived numbering \
                     — this database is not readable by cardano-node and a re-sync \
                     is required to make it so. New chunks will be numbered correctly."
                );
            }
        }

        debug!(
            chunk_size,
            existing_chunks = db.chunks.len(),
            "ImmutableDB opened for writing"
        );

        Ok(db)
    }

    /// Make `target` the active chunk, finalizing whatever preceded it and
    /// materialising any chunk range the chain skipped over.
    ///
    /// Chunks are append-only and strictly increasing, so a target at or below
    /// the active chunk is a caller error and is refused rather than silently
    /// truncating a written file.
    fn roll_to_chunk(&mut self, target: u64, chunk_size: u64) -> Result<(), ImmutableDBError> {
        // The chunk immediately after the last one that exists on disk — where
        // a gap, if any, begins.
        let mut gap_from = match self.active_chunk.as_ref() {
            Some(active) => {
                if target == active.chunk_num {
                    return Ok(());
                }
                if target < active.chunk_num {
                    return Err(ImmutableDBError::InconsistentChunk {
                        chunk: target,
                        reason: format!(
                            "block belongs in chunk {target} but chunk {} is already \
                             open for writing — blocks must be appended in slot order",
                            active.chunk_num
                        ),
                    });
                }
                let n = active.chunk_num;
                self.finalize_active_chunk(chunk_size)?;
                n + 1
            }
            // First append of this session.
            None => match self.last_chunk_on_disk {
                // The block belongs in the chunk the last session left behind:
                // continue it rather than starting a new one. A restart lands
                // here on almost every block, since a chunk spans `10 * k`
                // slots and a node rarely stops exactly on that boundary.
                Some(tail) if target == tail => {
                    return self.reopen_chunk_for_append(tail, chunk_size);
                }
                Some(tail) if target < tail => {
                    return Err(ImmutableDBError::InconsistentChunk {
                        chunk: target,
                        reason: format!(
                            "block belongs in chunk {target} but the database already \
                             holds chunk {tail}; appending would rewrite history"
                        ),
                    });
                }
                Some(tail) => tail + 1,
                // Nothing on disk. Upstream measures its gap from
                // `firstChunkNo` in exactly this situation, so a fresh
                // database whose first block is not in chunk 0 still gets
                // chunk 0 — an ImmutableDB is a chain from its own start, not
                // a sparse map.
                None => 0,
            },
        };

        // Refuse an implausible fill rather than writing millions of files.
        // The ImmutableDB only ever receives blocks that are already k-deep on
        // a validated chain, so a gap this size is a bad slot, not a quiet
        // period.
        const MAX_GAP_CHUNKS: u64 = 1000;
        if target.saturating_sub(gap_from) > MAX_GAP_CHUNKS {
            return Err(ImmutableDBError::InconsistentChunk {
                chunk: target,
                reason: format!(
                    "block would leave a gap of {} empty chunks after chunk {}; \
                     refusing to materialise it (limit {MAX_GAP_CHUNKS})",
                    target - gap_from,
                    gap_from.saturating_sub(1)
                ),
            });
        }

        // A slot range with no blocks in it still gets its files. Consensus's
        // iterator walks chunk numbers one at a time and reads each chunk's
        // primary index to find the next filled slot; an absent file is not
        // read as "empty", it raises `FsResourceDoesNotExist` and aborts the
        // open. Upstream keeps the sequence dense for the same reason —
        // `appendBlockImpl` starts `countChunks chunk initialChunk` new chunks
        // when a block skips ahead, not just the one it needs (issue #1081).
        while gap_from < target {
            Self::write_empty_chunk(&self.dir, gap_from, chunk_size)?;
            self.chunks.push(ChunkMeta {
                chunk_num: gap_from,
                // No blocks, so no slot range is covered. `chunk_covers_slot`
                // asks `first <= slot <= last`, which is false for every slot
                // when first > last — correct for an empty chunk.
                first_slot: 1,
                last_slot: 0,
            });
            gap_from += 1;
        }

        let chunk_path = self.dir.join(format!("{target:05}.chunk"));
        // #926 belt-and-braces: after reconciliation every non-empty chunk file
        // is indexed, so a collision here means the caller is re-writing history
        // (or the database predates #1081 and its tail overshot into this
        // chunk's range). Refuse to truncate rather than trust that reasoning.
        if let Ok(meta) = chunk_path.metadata() {
            if meta.len() > 0 {
                return Err(ImmutableDBError::InconsistentChunk {
                    chunk: target,
                    reason: format!(
                        "refusing to reuse chunk number {target}: target chunk file \
                         already contains {} bytes of data",
                        meta.len()
                    ),
                });
            }
        }
        let file = std::fs::File::create(&chunk_path)?;
        let secondary_path = self.dir.join(format!("{target:05}.secondary"));
        let secondary_file = std::fs::File::create(&secondary_path)?;

        self.active_chunk = Some(ActiveChunk {
            chunk_num: target,
            chunk_file: std::io::BufWriter::new(file),
            secondary_file,
            secondary_entries: Vec::new(),
            current_offset: 0,
            pending_blocks: HashMap::new(),
        });
        self.last_chunk_on_disk = Some(target);
        Ok(())
    }

    /// Continue writing into a chunk a previous session left on disk.
    ///
    /// A chunk covers a fixed slot range, so a restart almost always lands
    /// back inside the chunk it stopped in. Its existing blocks stay where
    /// they are — the file is opened for APPEND and the secondary index is
    /// read back so subsequent offsets continue from the right place.
    fn reopen_chunk_for_append(
        &mut self,
        chunk_num: u64,
        chunk_size: u64,
    ) -> Result<(), ImmutableDBError> {
        let chunk_path = self.dir.join(format!("{chunk_num:05}.chunk"));
        let secondary_path = self.dir.join(format!("{chunk_num:05}.secondary"));

        let current_offset = chunk_path.metadata().map(|m| m.len()).unwrap_or(0);

        // Reconciliation at open already truncated both files to a mutually
        // consistent prefix (#926), so what is here is what we continue from.
        let secondary_data = fs::read(&secondary_path).unwrap_or_default();
        let mut secondary_entries = Vec::with_capacity(secondary_data.len() / SECONDARY_ENTRY_SIZE);
        for (i, raw) in secondary_data
            .chunks_exact(SECONDARY_ENTRY_SIZE)
            .enumerate()
        {
            let block_offset = read_be_u64(&raw[0..8]).unwrap_or(0);
            let block_or_ebb = read_be_u64(&raw[48..56]).unwrap_or(0);
            let mut header_hash = [0u8; 32];
            header_hash.copy_from_slice(&raw[16..48]);
            // See `entry_slot`: an EBB is the chunk's first entry AND carries
            // the chunk's own number. The positional half is what keeps chunk
            // 0's genesis block from being mistaken for the epoch-0 EBB.
            let is_ebb = i == 0 && block_or_ebb == chunk_num;
            secondary_entries.push(SecondaryEntry {
                block_offset,
                header_hash,
                // Restore the in-memory invariant: `slot` is the real slot.
                slot: if is_ebb {
                    chunk_num.saturating_mul(chunk_size)
                } else {
                    block_or_ebb
                },
                checksum: u32::from_be_bytes([raw[12], raw[13], raw[14], raw[15]]),
                header_offset: u16::from_be_bytes([raw[8], raw[9]]),
                header_size: u16::from_be_bytes([raw[10], raw[11]]),
                is_ebb,
            });
        }

        let chunk_file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&chunk_path)?;
        let secondary_file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&secondary_path)?;

        debug!(
            chunk = chunk_num,
            entries = secondary_entries.len(),
            current_offset,
            "ImmutableDB: continuing the tail chunk"
        );

        self.active_chunk = Some(ActiveChunk {
            chunk_num,
            chunk_file: std::io::BufWriter::new(chunk_file),
            secondary_file,
            secondary_entries,
            current_offset,
            pending_blocks: HashMap::new(),
        });
        Ok(())
    }

    /// Write the three files for a chunk that contains no blocks.
    ///
    /// Empty `.chunk` and `.secondary`, and a `.primary` whose offsets are all
    /// zero — the shape upstream's `backfillChunk` produces for a chunk it
    /// starts and immediately finalises.
    fn write_empty_chunk(
        dir: &Path,
        chunk_num: u64,
        chunk_size: u64,
    ) -> Result<(), ImmutableDBError> {
        std::fs::File::create(dir.join(format!("{chunk_num:05}.chunk")))?;
        std::fs::File::create(dir.join(format!("{chunk_num:05}.secondary")))?;
        Self::write_primary_index(dir, chunk_num, chunk_size, &[])
    }

    /// Finalize the active chunk: fsync its data, write its primary index and
    /// record its slot range. Leaves the DB with no active chunk.
    fn finalize_active_chunk(&mut self, chunk_size: u64) -> Result<(), ImmutableDBError> {
        use std::io::Write;

        let active = match self.active_chunk.take() {
            Some(a) => a,
            None => return Ok(()),
        };

        // Flush and fsync the chunk file to guarantee durability before
        // syncing the secondary index. Without this, a crash could leave the
        // chunk file with missing tail data while the secondary index already
        // references those blocks.
        let mut chunk_file = active.chunk_file;
        chunk_file.flush()?;
        chunk_file.get_ref().sync_data()?;

        // The secondary entries were appended incrementally on every
        // append_block (issue #926); fsync so the chunk is fully recoverable
        // on restart even if the OS crashes immediately after this call.
        active.secondary_file.sync_data()?;

        Self::write_primary_index(
            &self.dir,
            active.chunk_num,
            chunk_size,
            &active.secondary_entries,
        )?;

        if let (Some(first), Some(last)) = (
            active.secondary_entries.first(),
            active.secondary_entries.last(),
        ) {
            self.chunks.push(ChunkMeta {
                chunk_num: active.chunk_num,
                first_slot: first.slot,
                last_slot: last.slot,
            });
        }

        debug!(chunk = active.chunk_num, "ImmutableDB: chunk finalized");
        Ok(())
    }

    /// Append a block to the active chunk.
    ///
    /// Updates the in-memory hash index immediately so the block is
    /// readable before the secondary index is flushed.
    ///
    /// `slot` is always the block's own slot, EBB or not.
    ///
    /// For a Byron Epoch Boundary Block (`is_ebb = true`) the secondary index
    /// records the EPOCH NUMBER rather than the slot — upstream's `BlockOrEBB`
    /// is a union and `chunkSlotForBlockOrEBB` dispatches on it:
    ///
    /// ```haskell
    /// data BlockOrEBB = Block !SlotNo | EBB !EpochNo
    /// chunkSlotForBoundaryBlock ci epoch = ... unsafeEpochNoToChunkNo epoch ...
    /// ```
    ///
    /// The epoch is DERIVED here rather than demanded from the caller. dugite's
    /// Byron decoder sets an EBB's slot to `epoch * byron_epoch_length`, and the
    /// chunk size *is* the Byron epoch length, so `slot / chunk_size` recovers
    /// the epoch exactly. Requiring the caller to substitute the epoch number
    /// for the slot is what produced a mainnet secondary index carrying
    /// `…, 21600, 21600, …` — the EBB's slot where its epoch number belonged,
    /// which cardano-node rejects as non-increasing (issue #1081).
    pub fn append_block(
        &mut self,
        slot: u64,
        _block_no: u64,
        hash: &Hash32,
        cbor: &[u8],
        is_ebb: bool,
    ) -> Result<(), ImmutableDBError> {
        use std::io::Write;

        let chunk_size = self.chunk_size.ok_or_else(|| {
            ImmutableDBError::Io(std::io::Error::other("ImmutableDB not opened for writing"))
        })?;

        // The chunk is a function of the block, not of when the DB was opened.
        let target = slot / chunk_size;
        self.roll_to_chunk(target, chunk_size)?;

        let active = self
            .active_chunk
            .as_mut()
            .expect("roll_to_chunk installs an active chunk");

        let block_offset = active.current_offset;
        active.chunk_file.write_all(cbor)?;
        active.current_offset += cbor.len() as u64;

        // Compute CRC32 of the block CBOR for integrity verification
        let checksum = crc32fast::hash(cbor);

        // Extract header offset and size for db-sync compatibility
        let (header_offset, header_size) = extract_header_bounds(cbor);

        // Persist the secondary entry incrementally (issue #926) and buffer
        // it (plus the block data) for reads and primary-index generation.
        let entry = SecondaryEntry {
            block_offset,
            header_hash: *hash.as_bytes(),
            slot,
            checksum,
            header_offset,
            header_size,
            is_ebb,
        };
        active
            .secondary_file
            .write_all(&entry.encode(active.chunk_num))?;
        active.secondary_entries.push(entry);
        active.pending_blocks.insert(*hash, cbor.to_vec());

        // Update index for immediate reads
        let block_end = active.current_offset;
        self.block_index.insert(
            *hash,
            BlockLocation {
                chunk_num: active.chunk_num,
                block_offset,
                block_end,
            },
        );

        // Store checksum for read-time verification
        self.checksums.insert(*hash, checksum);

        self.total_blocks += 1;
        if slot >= self.tip_slot {
            self.tip_slot = slot;
            self.tip_hash = *hash;
            self.tip_block_no = _block_no;
        }

        Ok(())
    }

    /// Flush the active chunk's data and secondary index to disk without
    /// starting a new chunk, and stamp the clean-shutdown marker.
    ///
    /// Call this on graceful shutdown only — the marker asserts that all
    /// on-disk state (including the mmap hash index) was written by an
    /// orderly stop. If a periodic flush is ever introduced, it must NOT
    /// write the marker (see [`CLEAN_MARKER`]).
    pub fn flush(&mut self) -> Result<(), ImmutableDBError> {
        use std::io::Write;

        // Read before the mutable borrow of `active_chunk` below.
        let Some(chunk_size) = self.chunk_size else {
            // Read-only: nothing to flush and no claim to make about the
            // on-disk state.
            return Ok(());
        };

        let active = match self.active_chunk.as_mut() {
            Some(a) => a,
            None => {
                // Opened for writing but no block was ever appended, so there
                // is no active chunk. The on-disk state is still exactly what
                // the last graceful stop left, so stamp the marker — otherwise
                // an open/close with no writes would force the next open to
                // rebuild the hash index for nothing (#928).
                self.block_index.persist()?;
                fs::write(self.dir.join(CLEAN_MARKER), b"")?;
                return Ok(());
            }
        };

        // Flush and fsync chunk data to guarantee durability. Without
        // sync_data(), the OS may buffer writes indefinitely and a crash
        // could lose the tail of the active chunk.
        active.chunk_file.flush()?;
        active.chunk_file.get_ref().sync_data()?;

        // The secondary entries were appended incrementally on every
        // append_block (issue #926); fsync them.
        active.secondary_file.sync_data()?;

        // Write primary index for Haskell ImmutableDB interoperability
        Self::write_primary_index(
            &self.dir,
            active.chunk_num,
            chunk_size,
            &active.secondary_entries,
        )?;

        // Update chunk metadata (replace existing entry for this chunk if present)
        if let (Some(first), Some(last)) = (
            active.secondary_entries.first(),
            active.secondary_entries.last(),
        ) {
            let chunk_num = active.chunk_num;
            if let Some(existing) = self.chunks.iter_mut().find(|c| c.chunk_num == chunk_num) {
                existing.first_slot = first.slot;
                existing.last_slot = last.slot;
            } else {
                self.chunks.push(ChunkMeta {
                    chunk_num,
                    first_slot: first.slot,
                    last_slot: last.slot,
                });
            }
        }

        // Persist tip metadata
        if self.tip_slot > 0 {
            Self::write_tip_meta(&self.dir, self.tip_slot, &self.tip_hash, self.tip_block_no)?;
        }

        // Persist block index (mmap flush)
        self.block_index.persist()?;

        // Everything durable — stamp the clean-shutdown marker (#928).
        fs::write(self.dir.join(CLEAN_MARKER), b"")?;

        debug!(
            chunk = active.chunk_num,
            entries = active.secondary_entries.len(),
            "ImmutableDB: flushed active chunk"
        );
        Ok(())
    }

    /// Write the `.primary` index file for a finalized chunk.
    ///
    /// Uses the exact Haskell ImmutableDB format from
    /// `Ouroboros.Consensus.Storage.ImmutableDB.Impl.Index.Primary`:
    ///
    /// ## On-disk format
    /// - **Version byte**: `0x01` (1 byte)
    /// - **Entries**: sequence of `u32 BE` values — each is a **byte offset**
    ///   into the `.secondary` file (always a multiple of 56)
    ///
    /// ## Entry layout
    ///
    /// A chunk of `C` slots always gets `C + 2` offsets, whatever its era and
    /// whether or not it actually holds an EBB:
    ///
    /// - relative slot **0** is the EBB slot,
    /// - relative slots `1..=C` are the regular slots, so a block at absolute
    ///   slot `s` sits at `s - chunk_num * C + 1`,
    /// - one trailing sentinel holding the total `.secondary` size.
    ///
    /// Upstream derives both from `ChunkSize`, not from the chunk's contents:
    ///
    /// ```haskell
    /// -- Chunks/Internal.hs
    /// simpleChunkInfo (EpochSize sz) = UniformChunkSize (ChunkSize True sz)
    ///
    /// maxRelativeIndex ChunkSize{..}
    ///   | chunkCanContainEBB = numRegularBlocks
    ///   | otherwise          = numRegularBlocks - 1
    ///
    /// -- Chunks/Layout.hs
    /// chunkSlotForRegularBlock (UniformChunkSize sz@ChunkSize{..}) (SlotNo slot) =
    ///   ... RelativeSlot ... $ if chunkCanContainEBB then withinChunk + 1
    ///                                                else withinChunk
    ///   where (chunk, withinChunk) = slot `divMod` numRegularBlocks
    /// ```
    ///
    /// `chunkCanContainEBB` is `True` for the whole Cardano chain — the hard-fork
    /// combinator takes its `ChunkInfo` from the FIRST era (`hd cfgs`), which is
    /// always Byron. Confirmed by measurement: 3102 of 3103 primary indexes in a
    /// real cardano-node mainnet DB are byte-identical in size at 86409 =
    /// `1 + 21602 * 4`, Byron through Alonzo (issue #1081).
    ///
    /// Deciding the arity from whether an EBB is *present* — which dugite did
    /// until #1081 — makes every Shelley+ chunk both the wrong length and
    /// shifted one slot down, since index 0 stays reserved regardless.
    ///
    /// ## Slot lookup
    /// To check if slot `r` has a block: `offset[r+1] > offset[r]`
    /// The secondary entry is at byte `offset[r]` in the `.secondary` file.
    fn write_primary_index(
        dir: &Path,
        chunk_num: u64,
        chunk_size: u64,
        secondary_entries: &[SecondaryEntry],
    ) -> Result<(), ImmutableDBError> {
        use std::io::Write;

        // Relative slots 0..=chunk_size — index 0 is the EBB slot.
        let num_slots = chunk_size + 1;
        let first_slot_of_chunk = chunk_num.saturating_mul(chunk_size);

        // Which relative slots are filled, in secondary-file order.
        let mut filled: HashSet<u64> = HashSet::with_capacity(secondary_entries.len());
        for entry in secondary_entries {
            let relative_slot = if entry.is_ebb {
                0
            } else {
                entry.slot.saturating_sub(first_slot_of_chunk) + 1
            };
            filled.insert(relative_slot);
        }

        let mut offsets: Vec<u32> = Vec::with_capacity(num_slots as usize + 1);
        // Running byte offset into the secondary index; each entry is
        // SECONDARY_ENTRY_SIZE (56) bytes.
        let mut current_offset: u32 = 0;
        for r in 0..num_slots {
            offsets.push(current_offset);
            if filled.contains(&r) {
                current_offset += SECONDARY_ENTRY_SIZE as u32;
            }
        }
        // Sentinel: total size of the secondary file
        offsets.push(current_offset);

        // Write to disk: version byte + u32 BE offsets
        let primary_path = dir.join(format!("{chunk_num:05}.primary"));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&primary_path)?);
        // Version byte (matches Haskell currentVersionNumber = 1)
        file.write_all(&[0x01])?;
        for &offset in &offsets {
            file.write_all(&offset.to_be_bytes())?;
        }
        file.flush()?;
        file.get_ref().sync_data()?;

        debug!(
            chunk = chunk_num,
            entries = offsets.len(),
            blocks = secondary_entries.len(),
            "ImmutableDB: primary index written"
        );

        Ok(())
    }

    /// Return up to `max_count` historical (slot, hash) points sampled at
    /// **Fibonacci-spaced chunk offsets** from the immutable tip.
    ///
    /// The offsets are `1, 2, 3, 5, 8, 13, 21, 34, …` chunks behind the
    /// most-recent chunk, capped at the total number of chunks.  This gives
    /// exponential-ish coverage back toward genesis with the densest sampling
    /// near the tip — matching the Haskell ChainSync intersection-discovery
    /// heuristic in `ouroboros-network` (`updateBestKnownPoints`).
    ///
    /// Used for ChainSync `MsgFindIntersect`.  When the immutable tip itself
    /// is contaminated (orphan fork block) or the peer's chain diverged many
    /// epochs back, the Fibonacci-spaced anchors give the peer a high
    /// probability of finding a common ancestor without falling back to
    /// `Origin`.  Issue #701.
    pub fn get_historical_points(&self, max_count: usize) -> Vec<(u64, Hash32)> {
        if self.chunks.is_empty() || max_count == 0 {
            return Vec::new();
        }
        let total_chunks = self.chunks.len();
        let offsets = fibonacci_chunk_offsets(total_chunks, max_count);

        let mut points = Vec::with_capacity(offsets.len());
        for offset in offsets {
            // `total_chunks - 1` is the most-recent chunk; subtract offset
            // to walk back.  Clamp to 0 (oldest chunk) if offset exceeds.
            let idx = total_chunks.saturating_sub(1 + offset);
            let Some(chunk_meta) = self.chunks.get(idx) else {
                continue;
            };
            let secondary_path = self
                .dir
                .join(format!("{:05}.secondary", chunk_meta.chunk_num));
            let secondary_data = match fs::read(&secondary_path) {
                Ok(data) => data,
                Err(_) => continue,
            };
            if secondary_data.len() < SECONDARY_ENTRY_SIZE {
                continue;
            }
            // Read the LAST entry in the secondary index — that's the chunk's
            // tip block.
            let last_entry_offset =
                (secondary_data.len() / SECONDARY_ENTRY_SIZE - 1) * SECONDARY_ENTRY_SIZE;
            let data = &secondary_data[last_entry_offset..];
            let mut header_hash = [0u8; 32];
            header_hash.copy_from_slice(&data[16..48]);
            if let Some(slot) = read_be_u64(&data[48..56]) {
                let hash = Hash32::from_bytes(header_hash);
                if !points.iter().any(|(s, h)| *s == slot && *h == hash) {
                    points.push((slot, hash));
                }
            }
        }
        points
    }

    /// Resolve a secondary entry's `blockOrEBB` union into an absolute slot.
    ///
    /// `blockOrEBB` holds the EPOCH NUMBER for a Byron boundary block and the
    /// slot for everything else. Two conditions identify the EBB, and BOTH are
    /// needed:
    ///
    /// * the value equals the chunk number (a Byron epoch and its chunk are the
    ///   same number), and
    /// * it is the chunk's FIRST entry — an EBB occupies relative slot 0, and
    ///   entries are stored in increasing relative-slot order, so nothing can
    ///   precede it.
    ///
    /// Dropping the positional half silently breaks CHUNK 0, where the epoch-0
    /// EBB and the genesis block at slot 0 both store `0`. They belong at
    /// relative slots 0 and 1; treating both as the EBB merges them into one.
    /// Verified against a real cardano-node mainnet database — with the
    /// positional test, regenerating the primary index of chunks 0, 1, 7, 100,
    /// 207, 208, 1000, 2351, 3000 and 3050 is byte-identical; without it,
    /// chunk 0 alone differs.
    ///
    /// Returns the raw value when the chunk size is unknown (a read-only open),
    /// which is what every reader did unconditionally before #1081 — including
    /// for the cardano-node-written Byron chunks a Mithril bootstrap installs.
    fn entry_slot(&self, chunk_num: u64, block_or_ebb: u64, is_first_entry: bool) -> u64 {
        match self.chunk_size {
            Some(cs) if is_first_entry && block_or_ebb == chunk_num => chunk_num.saturating_mul(cs),
            _ => block_or_ebb,
        }
    }

    /// Whether this ImmutableDB is open for writing.
    ///
    /// Keyed on the chunk size rather than on an active chunk: a chunk is only
    /// opened once a block establishes which one it should be, so a writable DB
    /// has no active chunk until its first `append_block`.
    pub fn is_writable(&self) -> bool {
        self.chunk_size.is_some()
    }

    /// Read a byte range from a chunk file with plain std I/O.
    ///
    /// Repair-path only (open-time tip.meta recovery) — runs before the
    /// configured reader backend is available.
    fn read_range_std(dir: &Path, chunk_num: u64, start: u64, end: u64) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        if end <= start {
            return None;
        }
        let mut f = fs::File::open(dir.join(format!("{chunk_num:05}.chunk"))).ok()?;
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![0u8; (end - start) as usize];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// Read a block from a chunk file at the given location.
    ///
    /// Uses the configured I/O backend (memmap2 or io_uring).
    fn read_block_at(&self, loc: &BlockLocation) -> Option<Vec<u8>> {
        let chunk_path = self.dir.join(format!("{:05}.chunk", loc.chunk_num));
        let start = loc.block_offset;
        let end = loc.block_end;
        if end <= start {
            warn!(
                chunk = loc.chunk_num,
                offset = start,
                end,
                "Invalid block location (end <= start)"
            );
            return None;
        }
        let len = (end - start) as usize;
        let reader = chunk_reader::default_reader();
        let result = reader.read_range(&chunk_path, start, len);
        if result.is_none() {
            warn!(
                chunk = loc.chunk_num,
                offset = start,
                end,
                "Failed to read block from chunk file"
            );
        }
        result
    }
}

/// Generate Fibonacci-spaced offsets `[1, 2, 3, 5, 8, 13, 21, …]` bounded
/// by `total_chunks` (offsets must be strictly less than this) and capped at
/// `max_count` entries.
///
/// Used by [`ImmutableDB::get_historical_points`] to produce
/// exponential-density anchors for ChainSync intersection.  Issue #701.
fn fibonacci_chunk_offsets(total_chunks: usize, max_count: usize) -> Vec<usize> {
    if total_chunks == 0 || max_count == 0 {
        return Vec::new();
    }
    let mut offsets: Vec<usize> = Vec::with_capacity(max_count);
    let mut a: usize = 1;
    let mut b: usize = 2;
    while offsets.len() < max_count && a < total_chunks {
        offsets.push(a);
        let next = a.saturating_add(b);
        a = b;
        b = next;
        if b == a {
            // Saturation guard (unreachable in practice — usize overflow).
            break;
        }
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal chunk file + secondary index for testing.
    fn create_test_chunk(
        dir: &Path,
        chunk_num: u64,
        blocks: &[(&[u8], [u8; 32], u64)], // (cbor, hash, slot)
    ) {
        let chunk_path = dir.join(format!("{chunk_num:05}.chunk"));
        let secondary_path = dir.join(format!("{chunk_num:05}.secondary"));

        let mut chunk_file = fs::File::create(&chunk_path).unwrap();
        let mut secondary_file = fs::File::create(&secondary_path).unwrap();

        let mut offset = 0u64;
        for (cbor, hash, slot) in blocks {
            // Write block CBOR to chunk file
            chunk_file.write_all(cbor).unwrap();

            // Write 56-byte secondary index entry
            let mut entry = [0u8; 56];
            entry[0..8].copy_from_slice(&offset.to_be_bytes()); // block_offset
                                                                // header_offset (2), header_size (2), checksum (4) — zeros for test
            entry[16..48].copy_from_slice(hash); // header_hash
            entry[48..56].copy_from_slice(&slot.to_be_bytes()); // block_or_ebb
            secondary_file.write_all(&entry).unwrap();

            offset += cbor.len() as u64;
        }
    }

    #[test]
    fn test_open_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 0);
        assert_eq!(db.tip_slot(), 0);
    }

    // ── #926 / #928 crash-durability and open-time reconciliation ──────────

    /// A block payload that looks like an on-disk Cardano block for
    /// reconciliation purposes: every stored block is the multi-era
    /// envelope `array(2) [era_tag, inner]`, so it starts with 0x82. The
    /// reconcile CRC-scan uses that prefix to delimit un-indexed tails.
    /// `fill` gives each block distinct content; `len` controls whether the
    /// write path's BufWriter writes through (len > 8 KiB) or buffers.
    fn envelope_payload(fill: u8, len: usize) -> Vec<u8> {
        let mut v = vec![fill; len];
        v[0] = 0x82;
        v
    }

    /// #926 (durability): secondary-index entries are written incrementally
    /// on append, so a hard process kill without flush() loses at most the
    /// buffered tail — NOT ten hours of index like the 2026-07-28 incident.
    /// Large blocks force the chunk BufWriter to write through, so both data
    /// and index are on disk; the reopened DB must serve all blocks.
    #[test]
    fn secondary_index_survives_kill_without_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        let payloads: Vec<Vec<u8>> = (1..=3u8).map(|i| envelope_payload(i, 16 * 1024)).collect();
        for (i, p) in payloads.iter().enumerate() {
            let hash = Hash32::from_bytes([(i + 1) as u8; 32]);
            db.append_block(100 + i as u64, 1 + i as u64, &hash, p, false)
                .unwrap();
        }
        // Simulated hard kill: no flush(), no Drop (Drop would flush the
        // BufWriter). File descriptors leak until process exit — fine.
        std::mem::forget(db);

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(
            db2.total_blocks(),
            3,
            "incrementally-written index must survive a kill"
        );
        for (i, p) in payloads.iter().enumerate() {
            let hash = Hash32::from_bytes([(i + 1) as u8; 32]);
            assert_eq!(
                db2.get_block(&hash).as_deref(),
                Some(p.as_slice()),
                "block {i} must be readable after crash-reopen"
            );
        }
    }

    /// #926: chunk data beyond the last valid secondary entry (index lost
    /// its tail in a crash) is truncated at open — never silently served,
    /// never left to be overwritten in place. The truncated blocks are
    /// re-fetched from peers by the normal sync path.
    #[test]
    fn unindexed_chunk_tail_is_truncated_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        let payloads: Vec<Vec<u8>> = (1..=3u8).map(|i| envelope_payload(i, 16 * 1024)).collect();
        for (i, p) in payloads.iter().enumerate() {
            let hash = Hash32::from_bytes([(i + 1) as u8; 32]);
            db.append_block(100 + i as u64, 1 + i as u64, &hash, p, false)
                .unwrap();
        }
        db.flush().unwrap();
        drop(db);

        // Damage: index covers only the first 2 blocks (the incident's
        // "index behind data" shape).
        let secondary_path = dir.path().join("00000.secondary");
        let sec = fs::read(&secondary_path).unwrap();
        fs::write(&secondary_path, &sec[..2 * SECONDARY_ENTRY_SIZE]).unwrap();

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db2.total_blocks(), 2, "only indexed blocks survive");
        assert_eq!(db2.tip_slot(), 101);
        assert!(db2.get_block(&Hash32::from_bytes([3u8; 32])).is_none());
        // The chunk file itself must have been truncated to the indexed
        // prefix so a later writer can never collide with orphan bytes.
        let chunk_len = dir.path().join("00000.chunk").metadata().unwrap().len();
        assert_eq!(
            chunk_len,
            (payloads[0].len() + payloads[1].len()) as u64,
            "un-indexed tail must be physically truncated"
        );
    }

    /// #926: a non-empty chunk with NO secondary index at the chain tail is
    /// quarantined (renamed, never silently skipped) and its data preserved;
    /// the DB opens consistently at the previous chunk's tip.
    #[test]
    fn index_less_last_chunk_is_quarantined_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        db.append_block(
            100,
            1,
            &Hash32::from_bytes([1u8; 32]),
            &envelope_payload(1, 16 * 1024),
            false,
        )
        .unwrap();
        db.append_block(
            432_100,
            2,
            &Hash32::from_bytes([2u8; 32]),
            &envelope_payload(2, 16 * 1024),
            false,
        )
        .unwrap();
        db.flush().unwrap();
        drop(db);

        // External damage: chunk 1 loses its whole secondary index.
        fs::remove_file(dir.path().join("00001.secondary")).unwrap();
        let _ = fs::remove_file(dir.path().join("00001.primary"));

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db2.total_blocks(), 1, "chunk 0 must survive");
        assert_eq!(db2.tip_slot(), 100, "tip falls back to chunk 0's tip");
        // Data preserved for forensics, out of the writer's namespace.
        assert!(
            dir.path().join("00001.chunk.orphaned").exists(),
            "quarantined chunk data must be preserved"
        );
        assert!(
            !dir.path().join("00001.chunk").exists(),
            "an index-less chunk file must not remain where a writer could \
             collide with it"
        );
    }

    /// #926: an index-less chunk in the MIDDLE of the chain (not the tail)
    /// is a hard open error — silently skipping it produced a served chain
    /// with a hole below the claimed tip in the 2026-07-28 incident.
    #[test]
    fn index_less_middle_chunk_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(dir.path(), 0, &[(b"aaaa", [1u8; 32], 10)]);
        create_test_chunk(dir.path(), 1, &[(b"bbbb", [2u8; 32], 20)]);
        create_test_chunk(dir.path(), 2, &[(b"cccc", [3u8; 32], 30)]);
        fs::remove_file(dir.path().join("00001.secondary")).unwrap();

        let res = ImmutableDB::open(dir.path());
        assert!(
            res.is_err(),
            "an index-less middle chunk must refuse to open, not skip"
        );
    }

    /// #928: a stale tip.meta (pointing past the indexed chain, as in the
    /// incident: 38k slots ahead) is clamped to the last indexed entry at
    /// open and rewritten.
    #[test]
    fn stale_tip_meta_is_clamped_to_indexed_chain() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        for i in 1..=2u8 {
            db.append_block(
                100 + i as u64,
                i as u64,
                &Hash32::from_bytes([i; 32]),
                &envelope_payload(i, 16 * 1024),
                false,
            )
            .unwrap();
        }
        db.flush().unwrap();
        drop(db);

        // Damage: tip.meta claims a tip 38k slots past the indexed chain.
        ImmutableDB::write_tip_meta(
            dir.path(),
            140_000,
            &Hash32::from_bytes([9u8; 32]),
            4_985_211,
        )
        .unwrap();

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db2.tip_slot(), 102, "tip slot comes from the indexed chain");
        assert_eq!(
            db2.tip_hash(),
            Hash32::from_bytes([2u8; 32]),
            "tip hash comes from the indexed chain"
        );
        assert_ne!(
            db2.tip_block_no(),
            4_985_211,
            "a tip.meta block_no unbacked by the index must not be trusted"
        );
        // And the on-disk tip.meta must have been repaired.
        let (s, h, _) = ImmutableDB::read_tip_meta(dir.path()).unwrap();
        assert_eq!((s, h), (102, Hash32::from_bytes([2u8; 32])));
    }

    /// #928: `has_verified_block` reads the block and checks its CRC —
    /// an index entry whose backing bytes are corrupt must NOT count as
    /// present (the flush path uses this so a phantom index entry can no
    /// longer suppress the re-flush of a live volatile block).
    #[test]
    fn has_verified_block_rejects_corrupt_backing_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        let hash = Hash32::from_bytes([1u8; 32]);
        db.append_block(100, 1, &hash, &envelope_payload(1, 16 * 1024), false)
            .unwrap();
        db.flush().unwrap();
        drop(db);

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert!(db2.has_block(&hash));
        assert!(db2.has_verified_block(&hash), "intact block verifies");

        // Bit rot AFTER open: flip one payload byte on disk.
        let chunk_path = dir.path().join("00000.chunk");
        let mut data = fs::read(&chunk_path).unwrap();
        data[1024] ^= 0xff;
        fs::write(&chunk_path, &data).unwrap();

        assert!(db2.has_block(&hash), "the index still claims the block");
        assert!(
            !db2.has_verified_block(&hash),
            "corrupt backing data must not count as present"
        );
    }

    /// #926: tail-grade reconciliation cascades below removed empty tail
    /// artifacts — a crash-damaged data tail sitting under a fresh empty
    /// chunk (created by an open that was itself killed) must still get the
    /// full CRC + truncation treatment, not body-grade checks.
    #[test]
    fn tail_reconcile_cascades_below_empty_tail_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        let payloads: Vec<Vec<u8>> = (1..=3u8).map(|i| envelope_payload(i, 16 * 1024)).collect();
        for (i, p) in payloads.iter().enumerate() {
            let hash = Hash32::from_bytes([(i + 1) as u8; 32]);
            db.append_block(100 + i as u64, 1 + i as u64, &hash, p, false)
                .unwrap();
        }
        db.flush().unwrap();
        drop(db);

        // Chunk 0's index loses its last entry (crash shape), and an empty
        // chunk-1 artifact sits above it (a later open was killed before
        // appending anything).
        let secondary_path = dir.path().join("00000.secondary");
        let sec = fs::read(&secondary_path).unwrap();
        fs::write(&secondary_path, &sec[..2 * SECONDARY_ENTRY_SIZE]).unwrap();
        fs::write(dir.path().join("00001.chunk"), b"").unwrap();
        fs::write(dir.path().join("00001.secondary"), b"").unwrap();

        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db2.total_blocks(), 2, "damaged true tail must be repaired");
        assert!(!dir.path().join("00001.chunk").exists());
        let chunk_len = dir.path().join("00000.chunk").metadata().unwrap().len();
        assert_eq!(
            chunk_len,
            (payloads[0].len() + payloads[1].len()) as u64,
            "un-indexed tail must be truncated despite the artifact above it"
        );
    }

    /// Stub prev-hash decoder for boundary-check tests: reads the prev hash
    /// from bytes[1..33] of the junk payload (production uses
    /// `decode_block_minimal`, injected at the same seam).
    fn stub_decode_prev(cbor: &[u8]) -> Option<[u8; 32]> {
        if cbor.len() < 33 || cbor[0] != 0x82 {
            return None;
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(&cbor[1..33]);
        Some(h)
    }

    /// Envelope payload carrying an explicit prev-hash for the stub decoder.
    fn linked_payload(prev: [u8; 32], fill: u8, len: usize) -> Vec<u8> {
        let mut v = vec![fill; len.max(64)];
        v[0] = 0x82;
        v[1..33].copy_from_slice(&prev);
        v
    }

    /// #926: adjacent chunks whose blocks chain correctly pass the boundary
    /// check untouched.
    #[test]
    fn chunk_boundary_check_passes_linked_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let h1 = [1u8; 32];
        let h2 = [2u8; 32];
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        db.append_block(
            100,
            1,
            &Hash32::from_bytes(h1),
            &linked_payload([0u8; 32], 1, 64),
            false,
        )
        .unwrap();
        db.append_block(
            101,
            2,
            &Hash32::from_bytes(h2),
            &linked_payload(h1, 2, 64),
            false,
        )
        .unwrap();
        db.append_block(
            432_100,
            3,
            &Hash32::from_bytes([3u8; 32]),
            &linked_payload(h2, 3, 64),
            false,
        )
        .unwrap();
        db.flush().unwrap();
        drop(db);

        ImmutableDB::check_chunk_boundaries(dir.path(), stub_decode_prev).unwrap();
        assert!(dir.path().join("00001.chunk").exists());
        assert!(!dir.path().join("00001.chunk.orphaned").exists());
    }

    /// #926 (the incident shape): a tail chunk that does NOT chain onto the
    /// previous chunk is an orphan island above a hole — it must be
    /// quarantined so the tip falls back to the last connected block.
    #[test]
    fn chunk_boundary_break_at_tail_quarantines_orphan_island() {
        let dir = tempfile::tempdir().unwrap();
        let h1 = [1u8; 32];
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        db.append_block(
            100,
            1,
            &Hash32::from_bytes(h1),
            &linked_payload([0u8; 32], 1, 64),
            false,
        )
        .unwrap();
        // Tail chunk's first block claims a prev that is NOT chunk 0's tip
        // (simulating the incident's 38k-slot indexed hole).
        db.append_block(
            432_100,
            9,
            &Hash32::from_bytes([9u8; 32]),
            &linked_payload([0x77u8; 32], 9, 64),
            false,
        )
        .unwrap();
        db.flush().unwrap();
        drop(db);

        ImmutableDB::check_chunk_boundaries(dir.path(), stub_decode_prev).unwrap();
        assert!(
            dir.path().join("00001.chunk.orphaned").exists(),
            "orphan island must be quarantined"
        );
        assert!(!dir.path().join("00001.secondary").exists());

        // End-to-end: a subsequent open lands on the last connected block.
        let db2 = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db2.total_blocks(), 1);
        assert_eq!(db2.tip_slot(), 100);
        assert_eq!(db2.tip_hash(), Hash32::from_bytes(h1));
    }

    /// #926: a chain break BELOW the tail refuses to open (no unbounded
    /// auto-truncation).
    #[test]
    fn chunk_boundary_break_below_tail_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let h1 = [1u8; 32];
        let h9 = [9u8; 32];
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        db.append_block(
            100,
            1,
            &Hash32::from_bytes(h1),
            &linked_payload([0u8; 32], 1, 64),
            false,
        )
        .unwrap();
        // Chunk 1 breaks the chain (prev = garbage)…
        db.append_block(
            432_100,
            9,
            &Hash32::from_bytes(h9),
            &linked_payload([0x77u8; 32], 9, 64),
            false,
        )
        .unwrap();
        // …and chunk 2 chains onto chunk 1, so the break is NOT at the tail.
        db.append_block(
            864_100,
            10,
            &Hash32::from_bytes([10u8; 32]),
            &linked_payload(h9, 10, 64),
            false,
        )
        .unwrap();
        db.flush().unwrap();
        drop(db);

        let res = ImmutableDB::check_chunk_boundaries(dir.path(), stub_decode_prev);
        assert!(
            res.is_err(),
            "a chain break below the tail must refuse, not auto-truncate"
        );
    }

    /// #928: clean-shutdown marker lifecycle — created by flush(), removed
    /// when the DB is opened for writing (entering write mode means the
    /// on-disk state is no longer known-clean until the next flush).
    #[test]
    fn clean_marker_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        assert!(
            !dir.path().join("clean").exists(),
            "no marker while in write mode"
        );
        db.append_block(
            100,
            1,
            &Hash32::from_bytes([1u8; 32]),
            &envelope_payload(1, 64),
            false,
        )
        .unwrap();
        db.flush().unwrap();
        assert!(
            dir.path().join("clean").exists(),
            "flush (graceful shutdown) writes the marker"
        );
        drop(db);

        let db2 = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
        assert!(
            !dir.path().join("clean").exists(),
            "opening for writing removes the marker"
        );
        drop(db2);
    }

    /// A Byron EBB and the first main block of the epoch share an absolute
    /// slot (mainnet: 171 of 176 Byron boundaries).  A slot-only cursor
    /// (`get_next_block_after_slot`) cannot step from the EBB to the
    /// same-slot main block — the point-aware lookup must.
    #[test]
    fn test_get_next_block_after_point_steps_through_same_slot_pair() {
        let dir = tempfile::tempdir().unwrap();
        let pred = [1u8; 32];
        let ebb = [2u8; 32];
        let main = [3u8; 32];
        let next = [4u8; 32];
        create_test_chunk(
            dir.path(),
            0,
            &[
                (b"pred", pred, 99),
                (b"ebb_", ebb, 100),
                (b"main", main, 100),
                (b"next", next, 101),
            ],
        );
        let db = ImmutableDB::open(dir.path()).unwrap();

        // pred -> EBB (first block at slot 100, chain order)
        let (s, h, cbor) = db
            .get_next_block_after_point(99, &Hash32::from_bytes(pred))
            .expect("EBB after pred");
        assert_eq!((s, h), (100, Hash32::from_bytes(ebb)));
        assert_eq!(cbor, b"ebb_");

        // EBB -> same-slot main block (the case slot-only cursors skip)
        let (s, h, cbor) = db
            .get_next_block_after_point(100, &Hash32::from_bytes(ebb))
            .expect("main block after EBB at the same slot");
        assert_eq!((s, h), (100, Hash32::from_bytes(main)));
        assert_eq!(cbor, b"main");

        // main -> next slot
        let (s, h, _) = db
            .get_next_block_after_point(100, &Hash32::from_bytes(main))
            .expect("block after slot-100 main");
        assert_eq!((s, h), (101, Hash32::from_bytes(next)));

        // tip -> none
        assert!(db
            .get_next_block_after_point(101, &Hash32::from_bytes(next))
            .is_none());
    }

    /// When the cursor hash is not stored at the given slot (e.g. a fork or
    /// volatile-only block), fall back to the strict `slot >` lookup.
    #[test]
    fn test_get_next_block_after_point_falls_back_for_unknown_hash() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[
                (b"ebb_", [2u8; 32], 100),
                (b"main", [3u8; 32], 100),
                (b"next", [4u8; 32], 101),
            ],
        );
        let db = ImmutableDB::open(dir.path()).unwrap();

        let (s, h, _) = db
            .get_next_block_after_point(100, &Hash32::from_bytes([0x99; 32]))
            .expect("fallback to strict-after lookup");
        assert_eq!((s, h), (101, Hash32::from_bytes([4u8; 32])));
    }

    /// The chain-order successor of a chunk's last block is the next chunk's
    /// first block.
    #[test]
    fn test_get_next_block_after_point_crosses_chunk_boundary() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"a", [1u8; 32], 10), (b"b", [2u8; 32], 20)],
        );
        create_test_chunk(dir.path(), 1, &[(b"c", [3u8; 32], 30)]);
        let db = ImmutableDB::open(dir.path()).unwrap();

        let (s, h, cbor) = db
            .get_next_block_after_point(20, &Hash32::from_bytes([2u8; 32]))
            .expect("first block of next chunk");
        assert_eq!((s, h), (30, Hash32::from_bytes([3u8; 32])));
        assert_eq!(cbor, b"c");
    }

    #[test]
    fn test_get_block_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let hash = [42u8; 32];
        create_test_chunk(dir.path(), 0, &[(b"block_data", hash, 100)]);

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 1);
        assert!(db.has_block(&Hash32::from_bytes(hash)));
        assert_eq!(
            db.get_block(&Hash32::from_bytes(hash)).unwrap(),
            b"block_data"
        );
    }

    #[test]
    fn test_missing_block() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(dir.path(), 0, &[(b"data", [1u8; 32], 100)]);

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert!(!db.has_block(&Hash32::from_bytes([99u8; 32])));
        assert!(db.get_block(&Hash32::from_bytes([99u8; 32])).is_none());
    }

    #[test]
    fn test_chunk_covers_slot_within_chunk() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"a", [1u8; 32], 10), (b"b", [2u8; 32], 30)],
        );
        let db = ImmutableDB::open(dir.path()).unwrap();
        // Slot at first/last entry and slot between them are all covered.
        assert!(db.chunk_covers_slot(10));
        assert!(db.chunk_covers_slot(20));
        assert!(db.chunk_covers_slot(30));
        // Outside the chunk's observed range: not covered.
        assert!(!db.chunk_covers_slot(9));
        assert!(!db.chunk_covers_slot(31));
    }

    #[test]
    fn test_chunk_covers_slot_with_gap_between_chunks() {
        // Mirror the production failure mode: main mithril provides chunks
        // 0 and 2 (i.e. the chunk number-skipping case where the aggregator
        // delivers a non-contiguous chunk sequence). A snapshot tip slot that
        // falls in the gap between two present chunks must NOT be reported
        // as covered — that's the signal the canonicality check needs to
        // trust the haskell-ledger snapshot.
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(dir.path(), 0, &[(b"a", [1u8; 32], 10)]);
        create_test_chunk(dir.path(), 2, &[(b"c", [3u8; 32], 50)]);
        let db = ImmutableDB::open(dir.path()).unwrap();

        assert!(db.chunk_covers_slot(10), "first chunk's slot is covered");
        assert!(db.chunk_covers_slot(50), "third chunk's slot is covered");
        assert!(
            !db.chunk_covers_slot(20),
            "slot inside the missing-chunk gap must report uncovered"
        );
        assert!(
            !db.chunk_covers_slot(40),
            "slot just before next chunk's first block must report uncovered"
        );
    }

    #[test]
    fn test_chunk_covers_slot_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert!(!db.chunk_covers_slot(0));
        assert!(!db.chunk_covers_slot(1_000_000));
    }

    #[test]
    fn test_multiple_chunks() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"block_a", [1u8; 32], 10), (b"block_b", [2u8; 32], 20)],
        );
        create_test_chunk(dir.path(), 1, &[(b"block_c", [3u8; 32], 30)]);

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 3);
        assert_eq!(db.tip_slot(), 30);
        assert!(db.has_block(&Hash32::from_bytes([1u8; 32])));
        assert!(db.has_block(&Hash32::from_bytes([3u8; 32])));
    }

    #[test]
    fn test_get_next_block_after_slot() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[
                (b"b1", [1u8; 32], 10),
                (b"b2", [2u8; 32], 20),
                (b"b3", [3u8; 32], 30),
            ],
        );

        let db = ImmutableDB::open(dir.path()).unwrap();

        let (slot, hash, cbor) = db.get_next_block_after_slot(0).unwrap();
        assert_eq!(slot, 10);
        assert_eq!(hash, Hash32::from_bytes([1u8; 32]));
        assert_eq!(cbor, b"b1");

        let (slot, _, cbor) = db.get_next_block_after_slot(10).unwrap();
        assert_eq!(slot, 20);
        assert_eq!(cbor, b"b2");

        let (slot, _, _) = db.get_next_block_after_slot(20).unwrap();
        assert_eq!(slot, 30);

        assert!(db.get_next_block_after_slot(30).is_none());
    }

    #[test]
    fn test_get_blocks_in_slot_range() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[
                (b"b1", [1u8; 32], 10),
                (b"b2", [2u8; 32], 20),
                (b"b3", [3u8; 32], 30),
            ],
        );

        let db = ImmutableDB::open(dir.path()).unwrap();

        let blocks = db.get_blocks_in_slot_range(10, 20);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], b"b1");
        assert_eq!(blocks[1], b"b2");

        let blocks = db.get_blocks_in_slot_range(25, 35);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], b"b3");
    }

    #[test]
    fn test_cross_chunk_slot_range() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(dir.path(), 0, &[(b"b1", [1u8; 32], 10)]);
        create_test_chunk(dir.path(), 1, &[(b"b2", [2u8; 32], 20)]);

        let db = ImmutableDB::open(dir.path()).unwrap();

        let blocks = db.get_blocks_in_slot_range(5, 25);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_tip_tracking() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"b1", [1u8; 32], 100), (b"b2", [2u8; 32], 200)],
        );
        create_test_chunk(dir.path(), 1, &[(b"b3", [3u8; 32], 300)]);

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.tip_slot(), 300);
        assert_eq!(db.tip_hash(), Hash32::from_bytes([3u8; 32]));
    }

    // -----------------------------------------------------------------------
    // Malformed / truncated secondary index handling
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_secondary_index() {
        // Empty secondary file should be gracefully skipped
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        // Create a non-empty chunk but an empty secondary index
        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(b"some block data")
            .unwrap();
        fs::File::create(&secondary_path).unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 0);
        assert_eq!(db.tip_slot(), 0);
    }

    #[test]
    fn test_truncated_secondary_index_less_than_entry_size() {
        // Secondary file with fewer than 56 bytes should be skipped
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(b"block data")
            .unwrap();
        // Write only 30 bytes — not enough for a single 56-byte entry
        fs::File::create(&secondary_path)
            .unwrap()
            .write_all(&[0u8; 30])
            .unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 0);
    }

    #[test]
    fn test_truncated_secondary_index_trailing_bytes() {
        // Secondary file with one valid entry + trailing bytes that
        // don't form a complete entry. The valid entry should be parsed;
        // the trailing bytes should be ignored.
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        let block_data = b"hello_block";
        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(block_data)
            .unwrap();

        // Build one valid 56-byte secondary entry
        let mut entry = [0u8; 56];
        entry[0..8].copy_from_slice(&0u64.to_be_bytes()); // block_offset = 0
        entry[16..48].copy_from_slice(&[7u8; 32]); // header_hash
        entry[48..56].copy_from_slice(&42u64.to_be_bytes()); // slot = 42

        let mut secondary_file = fs::File::create(&secondary_path).unwrap();
        secondary_file.write_all(&entry).unwrap();
        // Append 20 trailing garbage bytes (less than a full entry)
        secondary_file.write_all(&[0xFFu8; 20]).unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 1);
        assert_eq!(db.tip_slot(), 42);
        assert!(db.has_block(&Hash32::from_bytes([7u8; 32])));

        // Block data should be readable
        let cbor = db.get_block(&Hash32::from_bytes([7u8; 32])).unwrap();
        assert_eq!(cbor, block_data);
    }

    #[test]
    fn test_corrupted_secondary_data_graceful() {
        // Even with corrupted data in the secondary index, the parser
        // should not panic. It may produce wrong block locations, but
        // read_block_at will catch invalid offsets.
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(b"data")
            .unwrap();

        // Write 56 bytes of garbage — valid entry size but nonsensical values
        let garbage = [0xABu8; 56];
        fs::File::create(&secondary_path)
            .unwrap()
            .write_all(&garbage)
            .unwrap();

        // Should not panic — open-time reconciliation detects that the
        // garbage secondary entry fails the offset/CRC check against the
        // chunk data, so the tail chunk is quarantined.  The result is 0
        // blocks rather than 1, because no valid blocks survived.
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 0);

        // Lookup for the garbage hash must not panic
        let hash = {
            let mut h = [0u8; 32];
            h.copy_from_slice(&garbage[16..48]);
            Hash32::from_bytes(h)
        };
        // Block was rejected during validation; get_block returns None
        let result = db.get_block(&hash);
        assert!(result.is_none());
    }

    #[test]
    fn test_missing_chunk_file_skipped() {
        // Secondary exists but chunk file is missing — should skip gracefully
        let dir = tempfile::tempdir().unwrap();
        // Only create a secondary file, no .chunk file
        let secondary_path = dir.path().join("00000.secondary");
        let mut entry = [0u8; 56];
        entry[48..56].copy_from_slice(&100u64.to_be_bytes());
        fs::File::create(&secondary_path)
            .unwrap()
            .write_all(&entry)
            .unwrap();

        // Also need the chunk file to exist for it to be discovered
        // (chunks are discovered by scanning for .chunk files)
        // So this should result in 0 blocks since there's no .chunk
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 0);
    }

    #[test]
    fn test_read_be_u64_helper() {
        // Verify the helper function
        assert_eq!(read_be_u64(&[0, 0, 0, 0, 0, 0, 0, 1]), Some(1));
        assert_eq!(
            read_be_u64(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            Some(u64::MAX)
        );
        assert_eq!(read_be_u64(&[0, 0, 0, 0, 0, 0, 0]), None); // 7 bytes
        assert_eq!(read_be_u64(&[0, 0, 0, 0, 0, 0, 0, 0, 0]), None); // 9 bytes
        assert_eq!(read_be_u64(&[]), None); // empty
    }

    #[test]
    fn test_get_next_block_after_slot_with_truncated_secondary() {
        // Create a valid chunk + secondary, then verify queries work with
        // trailing bytes in the secondary index.
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(b"b1b2")
            .unwrap();

        let mut secondary_file = fs::File::create(&secondary_path).unwrap();

        // Entry 1: offset=0, hash=[1;32], slot=10
        let mut e1 = [0u8; 56];
        e1[0..8].copy_from_slice(&0u64.to_be_bytes());
        e1[16..48].copy_from_slice(&[1u8; 32]);
        e1[48..56].copy_from_slice(&10u64.to_be_bytes());
        secondary_file.write_all(&e1).unwrap();

        // Entry 2: offset=2, hash=[2;32], slot=20
        let mut e2 = [0u8; 56];
        e2[0..8].copy_from_slice(&2u64.to_be_bytes());
        e2[16..48].copy_from_slice(&[2u8; 32]);
        e2[48..56].copy_from_slice(&20u64.to_be_bytes());
        secondary_file.write_all(&e2).unwrap();

        // Trailing garbage (less than a full entry)
        secondary_file.write_all(&[0xCC; 10]).unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 2);

        // get_next_block_after_slot should work correctly
        let result = db.get_next_block_after_slot(0);
        assert!(result.is_some());
        let (slot, _, _) = result.unwrap();
        assert_eq!(slot, 10);

        let result = db.get_next_block_after_slot(10);
        assert!(result.is_some());
        let (slot, _, _) = result.unwrap();
        assert_eq!(slot, 20);
    }

    #[test]
    fn test_get_blocks_in_slot_range_with_truncated_secondary() {
        // Verify slot range queries gracefully handle trailing bytes
        let dir = tempfile::tempdir().unwrap();
        let chunk_path = dir.path().join("00000.chunk");
        let secondary_path = dir.path().join("00000.secondary");

        fs::File::create(&chunk_path)
            .unwrap()
            .write_all(b"aabb")
            .unwrap();

        let mut secondary_file = fs::File::create(&secondary_path).unwrap();

        let mut e1 = [0u8; 56];
        e1[0..8].copy_from_slice(&0u64.to_be_bytes());
        e1[16..48].copy_from_slice(&[1u8; 32]);
        e1[48..56].copy_from_slice(&10u64.to_be_bytes());
        secondary_file.write_all(&e1).unwrap();

        let mut e2 = [0u8; 56];
        e2[0..8].copy_from_slice(&2u64.to_be_bytes());
        e2[16..48].copy_from_slice(&[2u8; 32]);
        e2[48..56].copy_from_slice(&20u64.to_be_bytes());
        secondary_file.write_all(&e2).unwrap();

        // 40 trailing garbage bytes
        secondary_file.write_all(&[0xDD; 40]).unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();

        let blocks = db.get_blocks_in_slot_range(5, 25);
        assert_eq!(blocks.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Mmap block index integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_open_with_mmap_config() {
        let dir = tempfile::tempdir().unwrap();
        let hash = [42u8; 32];
        create_test_chunk(dir.path(), 0, &[(b"block_data", hash, 100)]);

        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };
        let db = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db.total_blocks(), 1);
        assert!(db.has_block(&Hash32::from_bytes(hash)));
        assert_eq!(
            db.get_block(&Hash32::from_bytes(hash)).unwrap(),
            b"block_data"
        );
        // hash_index.dat should be created
        assert!(dir.path().join("hash_index.dat").exists());
    }

    #[test]
    fn test_mmap_multiple_chunks() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"block_a", [1u8; 32], 10), (b"block_b", [2u8; 32], 20)],
        );
        create_test_chunk(dir.path(), 1, &[(b"block_c", [3u8; 32], 30)]);

        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };
        let db = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db.total_blocks(), 3);
        assert_eq!(db.tip_slot(), 30);
        assert!(db.has_block(&Hash32::from_bytes([1u8; 32])));
        assert!(db.has_block(&Hash32::from_bytes([3u8; 32])));
    }

    #[test]
    fn test_mmap_reuses_existing_index() {
        let dir = tempfile::tempdir().unwrap();
        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };

        create_test_chunk(dir.path(), 0, &[(b"b1", [1u8; 32], 10)]);

        // First open — builds hash_index.dat
        let db1 = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db1.total_blocks(), 1);
        drop(db1);

        // Second open — should reuse existing hash_index.dat (count matches)
        let db2 = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db2.total_blocks(), 1);
        assert!(db2.has_block(&Hash32::from_bytes([1u8; 32])));
    }

    #[test]
    fn test_mmap_rebuild_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };

        create_test_chunk(dir.path(), 0, &[(b"b1", [1u8; 32], 10)]);

        // First open — builds hash_index.dat
        let db1 = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        drop(db1);

        // Add another chunk — now the index is stale
        create_test_chunk(dir.path(), 1, &[(b"b2", [2u8; 32], 20)]);

        // Reopen — should rebuild since count changed
        let db2 = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db2.total_blocks(), 2);
        assert!(db2.has_block(&Hash32::from_bytes([1u8; 32])));
        assert!(db2.has_block(&Hash32::from_bytes([2u8; 32])));
    }

    #[test]
    fn test_open_empty_dir_with_mmap() {
        let dir = tempfile::tempdir().unwrap();
        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };
        let db = ImmutableDB::open_with_config(dir.path(), &config).unwrap();
        assert_eq!(db.total_blocks(), 0);
        assert_eq!(db.tip_slot(), 0);
    }

    #[test]
    fn test_open_for_writing_with_mmap_config() {
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(dir.path(), 0, &[(b"b1", [1u8; 32], 10)]);

        let config = ImmutableConfig {
            index_type: crate::config::BlockIndexType::Mmap,
            mmap_load_factor: 0.7,
            mmap_initial_capacity: 0,
        };
        let mut db =
            ImmutableDB::open_for_writing_with_config(dir.path(), &config, 432_000).unwrap();
        assert!(db.is_writable());
        assert_eq!(db.total_blocks(), 1);

        // Append a block
        let new_hash = Hash32::from_bytes([99u8; 32]);
        db.append_block(20, 2, &new_hash, b"new_block", false)
            .unwrap();
        assert!(db.has_block(&new_hash));
        assert_eq!(db.get_block(&new_hash).unwrap(), b"new_block");
        assert_eq!(db.total_blocks(), 2);
    }

    #[test]
    fn test_default_config_matches_original_behavior() {
        // Default config should produce identical results to open()
        let dir = tempfile::tempdir().unwrap();
        create_test_chunk(
            dir.path(),
            0,
            &[(b"b1", [1u8; 32], 10), (b"b2", [2u8; 32], 20)],
        );

        let db_default = ImmutableDB::open(dir.path()).unwrap();
        let db_config =
            ImmutableDB::open_with_config(dir.path(), &ImmutableConfig::default()).unwrap();

        assert_eq!(db_default.total_blocks(), db_config.total_blocks());
        assert_eq!(db_default.tip_slot(), db_config.tip_slot());
        assert_eq!(db_default.tip_hash(), db_config.tip_hash());
    }

    // -----------------------------------------------------------------------
    // CRC32 verification tests
    // -----------------------------------------------------------------------

    /// Build a chunk file + secondary index with CRC32 checksums in the
    /// secondary entries.
    fn create_test_chunk_with_crc(
        dir: &Path,
        chunk_num: u64,
        blocks: &[(&[u8], [u8; 32], u64)], // (cbor, hash, slot)
    ) {
        let chunk_path = dir.join(format!("{chunk_num:05}.chunk"));
        let secondary_path = dir.join(format!("{chunk_num:05}.secondary"));

        let mut chunk_file = fs::File::create(&chunk_path).unwrap();
        let mut secondary_file = fs::File::create(&secondary_path).unwrap();

        let mut offset = 0u64;
        for (cbor, hash, slot) in blocks {
            chunk_file.write_all(cbor).unwrap();

            let checksum = crc32fast::hash(cbor);

            let mut entry = [0u8; 56];
            entry[0..8].copy_from_slice(&offset.to_be_bytes());
            // bytes 12..16: CRC32 checksum
            entry[12..16].copy_from_slice(&checksum.to_be_bytes());
            entry[16..48].copy_from_slice(hash);
            entry[48..56].copy_from_slice(&slot.to_be_bytes());
            secondary_file.write_all(&entry).unwrap();

            offset += cbor.len() as u64;
        }
    }

    #[test]
    fn test_crc32_write_and_verify() {
        // Blocks written via append_block should have CRC32 stored
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();

        let hash = Hash32::from_bytes([42u8; 32]);
        let cbor = b"test block data with CRC";
        db.append_block(100, 1, &hash, cbor, false).unwrap();

        // Read back — should succeed with valid CRC
        let result = db.get_block(&hash).unwrap();
        assert_eq!(result, cbor);

        // Verify the checksum was stored
        assert_eq!(db.checksums.get(&hash), Some(&crc32fast::hash(cbor)));
    }

    #[test]
    fn test_crc32_persisted_in_secondary_index() {
        // Write blocks, flush, re-open, verify CRC is loaded from secondary index
        let dir = tempfile::tempdir().unwrap();
        let hash = Hash32::from_bytes([42u8; 32]);
        let cbor = b"block for CRC persistence test";

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
            db.append_block(100, 1, &hash, cbor, false).unwrap();
            db.flush().unwrap();
        }

        // Re-open and verify CRC is loaded
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert!(db.checksums.contains_key(&hash));
        assert_eq!(db.checksums[&hash], crc32fast::hash(cbor));

        // Read should succeed
        let result = db.get_block(&hash).unwrap();
        assert_eq!(result, cbor);
    }

    #[test]
    fn test_crc32_mismatch_detection_rejects_corrupted_data() {
        // Create a chunk with valid CRC, then corrupt the chunk data.
        // The read should return None to prevent propagation of corrupt data.
        let dir = tempfile::tempdir().unwrap();
        let hash = [42u8; 32];
        let cbor = b"original data";

        // Create chunk with correct CRC
        create_test_chunk_with_crc(dir.path(), 0, &[(cbor, hash, 100)]);

        // Now corrupt the chunk file by overwriting the data
        let chunk_path = dir.path().join("00000.chunk");
        fs::write(&chunk_path, b"corrupted dat").unwrap(); // same length, different content

        let db = ImmutableDB::open(dir.path()).unwrap();

        // Open-time reconciliation detects the CRC mismatch and quarantines
        // the unverifiable tail chunk entirely.
        // The block is no longer accessible — not just rejected at read time.
        let hash32 = Hash32::from_bytes(hash);
        assert!(
            !db.checksums.contains_key(&hash32),
            "corrupted block should be evicted from checksums on startup validation"
        );

        // Read must return None — the block was removed from the index
        let result = db.get_block(&hash32);
        assert!(result.is_none());
    }

    #[test]
    fn test_crc32_legacy_entries_no_checksum() {
        // Legacy entries (checksum=0) should not trigger CRC verification
        let dir = tempfile::tempdir().unwrap();
        let hash = [42u8; 32];

        // create_test_chunk writes entries with checksum=0 (legacy)
        create_test_chunk(dir.path(), 0, &[(b"block_data", hash, 100)]);

        let db = ImmutableDB::open(dir.path()).unwrap();

        // No checksum should be stored for legacy entries
        assert!(!db.checksums.contains_key(&Hash32::from_bytes(hash)));

        // Read should work without CRC verification
        let result = db.get_block(&Hash32::from_bytes(hash)).unwrap();
        assert_eq!(result, b"block_data");
    }

    #[test]
    fn test_crc32_valid_read_after_write_and_reopen() {
        // Full round-trip: write, flush, reopen, read with CRC verification
        let dir = tempfile::tempdir().unwrap();
        let blocks = vec![
            (Hash32::from_bytes([1u8; 32]), b"block_one".as_slice()),
            (Hash32::from_bytes([2u8; 32]), b"block_two".as_slice()),
            (Hash32::from_bytes([3u8; 32]), b"block_three".as_slice()),
        ];

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
            for (i, (hash, cbor)) in blocks.iter().enumerate() {
                db.append_block((i as u64 + 1) * 10, i as u64 + 1, hash, cbor, false)
                    .unwrap();
            }
            db.flush().unwrap();
        }

        // Re-open and verify all blocks pass CRC verification
        let db = ImmutableDB::open(dir.path()).unwrap();
        for (hash, cbor) in &blocks {
            let result = db.get_block(hash).unwrap();
            assert_eq!(result, *cbor);
            assert!(db.checksums.contains_key(hash));
        }
    }

    // -----------------------------------------------------------------------
    // Additional edge case tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_append_block_at_slot_zero() {
        // First block at slot 0 should work correctly
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();

        let hash = Hash32::from_bytes([1u8; 32]);
        db.append_block(0, 0, &hash, b"genesis_block", false)
            .unwrap();

        assert_eq!(db.total_blocks(), 1);
        assert_eq!(db.tip_slot(), 0);
        assert!(db.has_block(&hash));
        assert_eq!(db.get_block(&hash).unwrap(), b"genesis_block");
    }

    /// A block at an absurd slot is REFUSED, not stored.
    ///
    /// Its chunk is `slot / chunk_size`, so `u64::MAX` names chunk 4.2e13 and
    /// storing it means materialising every chunk below it — upstream measures
    /// its gap the same way and would attempt the same thing. Before #1081 the
    /// chunk number was unrelated to the slot, so this landed in chunk 0 and
    /// looked fine; the layout it produced was the defect.
    #[test]
    fn test_append_block_at_max_slot() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();

        let hash = Hash32::from_bytes([1u8; 32]);
        let err = db
            .append_block(u64::MAX, 1, &hash, b"far_future_block", false)
            .unwrap_err();
        assert!(
            matches!(err, ImmutableDBError::InconsistentChunk { .. }),
            "expected a refusal naming the gap, got {err:?}"
        );
        assert_eq!(db.total_blocks(), 0, "nothing may be stored");
        assert!(!db.has_block(&hash));

        // A block at a plausible slot in the same session still works.
        db.append_block(4_320_000, 1, &hash, b"real_block", false)
            .unwrap();
        assert_eq!(db.total_blocks(), 1);
        assert_eq!(db.tip_slot(), 4_320_000);
        assert_eq!(db.get_block(&hash).unwrap(), b"real_block");
    }

    #[test]
    fn test_secondary_index_survives_flush_and_reopen() {
        // Write blocks, flush, reopen and verify all data survives
        let dir = tempfile::tempdir().unwrap();
        let hashes: Vec<Hash32> = (1..=5u8).map(|i| Hash32::from_bytes([i; 32])).collect();

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
            for (i, hash) in hashes.iter().enumerate() {
                let cbor = format!("block_{}", i + 1);
                db.append_block(
                    (i as u64 + 1) * 100,
                    i as u64 + 1,
                    hash,
                    cbor.as_bytes(),
                    false,
                )
                .unwrap();
            }
            db.flush().unwrap();
        }

        // Reopen and verify all blocks
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 5);
        assert_eq!(db.tip_slot(), 500);
        for (i, hash) in hashes.iter().enumerate() {
            assert!(db.has_block(hash));
            let cbor = db.get_block(hash).unwrap();
            assert_eq!(cbor, format!("block_{}", i + 1).as_bytes());
        }
    }

    #[test]
    fn test_secondary_index_missing_chunk_file_read() {
        // Create chunk + secondary, then delete chunk file.
        // has_block returns true (index exists) but get_block returns None (can't read).
        let dir = tempfile::tempdir().unwrap();
        let hash = [42u8; 32];
        create_test_chunk(dir.path(), 0, &[(b"block_data", hash, 100)]);

        // Delete the chunk file but keep secondary
        fs::remove_file(dir.path().join("00000.chunk")).unwrap();

        let db = ImmutableDB::open(dir.path()).unwrap();
        // Chunk file is gone so no blocks discovered (chunks found by .chunk files)
        assert_eq!(db.total_blocks(), 0);
    }

    #[test]
    fn test_crc32_mismatch_rejects_corrupted_block() {
        // Corrupt the block data on disk, verify get_block returns None
        // (CRC mismatch rejects the block to prevent propagation of corrupt data)
        let dir = tempfile::tempdir().unwrap();
        let hash = Hash32::from_bytes([42u8; 32]);
        let original = b"original_block_data_here";

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
            db.append_block(100, 1, &hash, original, false).unwrap();
            db.flush().unwrap();
        }

        // Corrupt the chunk file (overwrite with same-length different content)
        let chunk_path = dir.path().join("00000.chunk");
        let corrupted = b"CORRUPTED_block_data_hXX";
        assert_eq!(corrupted.len(), original.len());
        fs::write(&chunk_path, corrupted).unwrap();

        // Reopen - CRC mismatch should reject the corrupted block
        let db = ImmutableDB::open(dir.path()).unwrap();
        let result = db.get_block(&hash);
        assert!(result.is_none());
    }

    #[test]
    fn test_finalize_and_reopen() {
        // Write blocks to active chunk, finalize, write more, flush, reopen
        let dir = tempfile::tempdir().unwrap();
        let h1 = Hash32::from_bytes([1u8; 32]);
        let h2 = Hash32::from_bytes([2u8; 32]);
        let h3 = Hash32::from_bytes([3u8; 32]);

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 432_000).unwrap();
            db.append_block(10, 1, &h1, b"epoch0_block1", false)
                .unwrap();
            db.append_block(20, 2, &h2, b"epoch0_block2", false)
                .unwrap();
            db.append_block(30, 3, &h3, b"epoch1_block1", false)
                .unwrap();
            db.flush().unwrap();
        }

        // Reopen and verify all blocks across finalized + active chunks
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 3);
        assert!(db.has_block(&h1));
        assert!(db.has_block(&h2));
        assert!(db.has_block(&h3));
        assert_eq!(db.get_block(&h1).unwrap(), b"epoch0_block1");
        assert_eq!(db.get_block(&h3).unwrap(), b"epoch1_block1");
    }

    #[test]
    fn test_read_crc32_from_entry_helper() {
        // Short entry returns 0
        assert_eq!(read_crc32_from_entry(&[0u8; 10]), 0);

        // Entry with CRC at bytes 12..16
        let mut entry = [0u8; 56];
        entry[12..16].copy_from_slice(&42u32.to_be_bytes());
        assert_eq!(read_crc32_from_entry(&entry), 42);

        // All-zero CRC field
        assert_eq!(read_crc32_from_entry(&[0u8; 56]), 0);
    }

    // -----------------------------------------------------------------------
    // Primary index, EBB, and chunk numbering tests (issue #312)
    // -----------------------------------------------------------------------

    /// Helper to read a u32 BE at a given byte offset within the primary index data.
    fn read_primary_offset(data: &[u8], entry_index: usize) -> u32 {
        // Skip the 1-byte version header, then each entry is 4 bytes
        let offset = 1 + entry_index * 4;
        u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    /// A chunk holding NO EBB still reserves relative slot 0 for one, so a
    /// block at absolute slot `s` sits at primary entry `s - first + 1` and the
    /// index has `chunk_size + 2` entries.
    ///
    /// Both facts are measured off a real cardano-node mainnet database, where
    /// chunks 208 (Shelley), 2351 (Mary) and 3000 (Alonzo) all carry 21602
    /// entries and all match `slot - first + 1` — never `slot - first`.
    /// dugite chose the arity and the mapping from whether an EBB happened to
    /// be present, so every Shelley+ chunk was both the wrong length and
    /// shifted down one slot (issue #1081).
    #[test]
    fn primary_index_reserves_the_ebb_slot_even_with_no_ebb() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_size = 1000u64;

        let mut db = ImmutableDB::open_for_writing(dir.path(), chunk_size).unwrap();
        db.append_block(100, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        db.append_block(200, 2, &Hash32::from_bytes([2u8; 32]), b"b2", false)
            .unwrap();
        db.append_block(300, 3, &Hash32::from_bytes([3u8; 32]), b"b3", false)
            .unwrap();
        db.flush().unwrap();

        let data = fs::read(dir.path().join("00000.primary")).unwrap();

        // `chunk_size` regular slots + 1 EBB slot + 1 sentinel.
        let num_entries = chunk_size as usize + 2;
        assert_eq!(
            data.len(),
            1 + num_entries * 4,
            "primary index arity must come from the chunk size, not the contents"
        );
        assert_eq!(data[0], 0x01, "Version byte should be 0x01");

        // Slot 0 is the EBB slot and is empty here, so nothing has moved yet.
        assert_eq!(read_primary_offset(&data, 0), 0);
        assert_eq!(read_primary_offset(&data, 100), 0);

        // Slot 100 → entry 101. Entry 100 must be EMPTY: that is the whole
        // off-by-one, and asserting only entry 101 would pass under the bug
        // too, because offsets are non-decreasing.
        assert_eq!(read_primary_offset(&data, 101), 0);
        assert_eq!(read_primary_offset(&data, 102), 56);

        // Slot 200 → entry 201; slot 300 → entry 301.
        assert_eq!(read_primary_offset(&data, 201), 56);
        assert_eq!(read_primary_offset(&data, 202), 112);
        assert_eq!(read_primary_offset(&data, 301), 112);
        assert_eq!(read_primary_offset(&data, 302), 168);

        // Sentinel (last entry) = total secondary file size = 3 * 56 = 168
        assert_eq!(read_primary_offset(&data, num_entries - 1), 168);
    }

    #[test]
    fn test_primary_index_written_on_flush() {
        // flush() should also write .primary for the active chunk.
        let dir = tempfile::tempdir().unwrap();

        let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
        db.append_block(50, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        db.flush().unwrap();

        let primary_path = dir.path().join("00000.primary");
        assert!(
            primary_path.exists(),
            ".primary file should exist after flush"
        );
    }

    #[test]
    fn test_primary_index_ebb_entry() {
        // EBB-capable chunk (has_ebb=true): epoch_length + 2 entries.
        // Slot 0 = EBB, slots 1..epoch_length = regular.
        let dir = tempfile::tempdir().unwrap();
        let epoch_length = 500u64;

        let mut db = ImmutableDB::open_for_writing(dir.path(), epoch_length).unwrap();
        // EBB: pass epoch number (0) as slot, is_ebb=true
        db.append_block(0, 0, &Hash32::from_bytes([1u8; 32]), b"ebb", true)
            .unwrap();
        // Regular block at slot 100
        db.append_block(100, 1, &Hash32::from_bytes([2u8; 32]), b"regular", false)
            .unwrap();
        db.flush().unwrap();

        let data = fs::read(dir.path().join("00000.primary")).unwrap();

        // EBB chunk: chunk_size + 2 entries + version byte — the SAME arity as
        // the no-EBB chunk above.
        let num_entries = epoch_length as usize + 2;
        assert_eq!(data.len(), 1 + num_entries * 4);
        assert_eq!(data[0], 0x01, "Version byte");

        // Entry 0 (EBB position): offset = 0 (EBB is first secondary entry)
        assert_eq!(read_primary_offset(&data, 0), 0);
        // Entry 1 (after EBB): offset = 56 (EBB consumed one secondary entry)
        assert_eq!(read_primary_offset(&data, 1), 56);

        // Slot 100 maps to entry[101] in EBB-capable chunk (offset by 1 for EBB slot)
        // No blocks between slot 0 and slot 100, so offset stays at 56
        assert_eq!(read_primary_offset(&data, 100), 56);
        // Entry after slot 100: offset = 56 + 56 = 112
        assert_eq!(read_primary_offset(&data, 101), 56);
        assert_eq!(read_primary_offset(&data, 102), 112);

        // Sentinel = total secondary size = 2 * 56 = 112
        assert_eq!(read_primary_offset(&data, num_entries - 1), 112);
    }

    /// An EBB is appended with its own SLOT, like every other block, and the
    /// secondary index records its EPOCH NUMBER — the two differ by a factor of
    /// the chunk size, and writing the slot is what made a dugite mainnet DB
    /// read `…, 21600, 21600, …` where cardano-node writes `…, 1, …` (#1081).
    #[test]
    fn test_secondary_entry_ebb_stores_epoch_number() {
        let dir = tempfile::tempdir().unwrap();

        let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
        // The EBB for epoch 5 sits at slot 5 * 1000, as dugite's Byron decoder
        // assigns it (`epoch * byron_epoch_length`).
        db.append_block(5000, 0, &Hash32::from_bytes([1u8; 32]), b"ebb_epoch5", true)
            .unwrap();
        db.flush().unwrap();

        // It lands in chunk 5, and its `blockOrEBB` is the epoch, not the slot.
        let secondary_data = fs::read(dir.path().join("00005.secondary")).unwrap();
        assert_eq!(secondary_data.len(), SECONDARY_ENTRY_SIZE);

        let slot_bytes: [u8; 8] = secondary_data[48..56].try_into().unwrap();
        let slot_value = u64::from_be_bytes(slot_bytes);
        assert_eq!(
            slot_value, 5,
            "EBB block_or_ebb should contain the epoch number, not slot 5000"
        );

        // And it occupies relative slot 0, so entry 1 has already advanced.
        let primary = fs::read(dir.path().join("00005.primary")).unwrap();
        assert_eq!(read_primary_offset(&primary, 0), 0);
        assert_eq!(read_primary_offset(&primary, 1), 56);
    }

    /// dugite's primary index is BYTE-IDENTICAL to cardano-node's for the same
    /// chunk contents.
    ///
    /// The fixture is a real cardano-node mainnet chunk — 00208, the first
    /// Shelley chunk, 1079 blocks — copied unmodified out of a synced
    /// `db-cn-mainnet`. Its secondary index is fed through dugite's writer and
    /// the result compared against cardano-node's own `.primary`.
    ///
    /// This is the only check that can see the whole of #1081's second half.
    /// The chunk-number invariant (`slot / chunkSize == chunkIndex`) is
    /// satisfied by an index with the wrong arity, the wrong relative-slot
    /// mapping, or both — cardano-node still could not open it. Comparing
    /// against the real bytes cannot be satisfied by a self-consistent wrong
    /// answer, which a same-process round-trip always can.
    #[test]
    fn primary_index_is_byte_identical_to_cardano_nodes() {
        const MAINNET_CHUNK_SIZE: u64 = 21600; // 10 * k, k = 2160
        const CHUNK: u64 = 208;

        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cardano-node-immutable");
        let secondary = fs::read(fixtures.join("00208.secondary")).unwrap();
        let expected_primary = fs::read(fixtures.join("00208.primary")).unwrap();

        // Rebuild the entries exactly as `append_block` would have recorded
        // them, from cardano-node's own secondary index.
        let first_slot = CHUNK * MAINNET_CHUNK_SIZE;
        let entries: Vec<SecondaryEntry> = secondary
            .chunks_exact(SECONDARY_ENTRY_SIZE)
            .enumerate()
            .map(|(i, raw)| {
                let block_or_ebb = read_be_u64(&raw[48..56]).unwrap();
                let is_ebb = i == 0 && block_or_ebb == CHUNK;
                let mut header_hash = [0u8; 32];
                header_hash.copy_from_slice(&raw[16..48]);
                SecondaryEntry {
                    block_offset: read_be_u64(&raw[0..8]).unwrap(),
                    header_hash,
                    slot: if is_ebb { first_slot } else { block_or_ebb },
                    checksum: u32::from_be_bytes([raw[12], raw[13], raw[14], raw[15]]),
                    header_offset: u16::from_be_bytes([raw[8], raw[9]]),
                    header_size: u16::from_be_bytes([raw[10], raw[11]]),
                    is_ebb,
                }
            })
            .collect();
        assert_eq!(entries.len(), 1079, "fixture should hold 1079 blocks");

        let dir = tempfile::tempdir().unwrap();
        ImmutableDB::write_primary_index(dir.path(), CHUNK, MAINNET_CHUNK_SIZE, &entries).unwrap();
        let ours = fs::read(dir.path().join("00208.primary")).unwrap();

        assert_eq!(
            ours.len(),
            expected_primary.len(),
            "primary index length differs: {} vs cardano-node's {}",
            ours.len(),
            expected_primary.len()
        );
        if ours != expected_primary {
            let first = ours
                .iter()
                .zip(&expected_primary)
                .position(|(a, b)| a != b)
                .unwrap();
            panic!(
                "primary index differs from cardano-node's at byte {first} \
                 (entry {}): ours {:?} vs theirs {:?}",
                first.saturating_sub(1) / 4,
                &ours[first..(first + 8).min(ours.len())],
                &expected_primary[first..(first + 8).min(expected_primary.len())]
            );
        }
    }

    /// Re-encoding cardano-node's secondary index reproduces it byte for byte.
    ///
    /// Pins the entry layout and, with a Byron fixture, would pin the
    /// `blockOrEBB` union too. Chunk 00208 holds no EBB — every Byron chunk is
    /// ~21600 entries and too large to vendor — so the EBB arm is covered by
    /// [`test_secondary_entry_ebb_stores_epoch_number`] instead.
    #[test]
    fn secondary_index_round_trips_cardano_nodes_bytes() {
        const CHUNK: u64 = 208;
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cardano-node-immutable");
        let secondary = fs::read(fixtures.join("00208.secondary")).unwrap();

        let mut ours = Vec::with_capacity(secondary.len());
        for (i, raw) in secondary.chunks_exact(SECONDARY_ENTRY_SIZE).enumerate() {
            let block_or_ebb = read_be_u64(&raw[48..56]).unwrap();
            let is_ebb = i == 0 && block_or_ebb == CHUNK;
            let mut header_hash = [0u8; 32];
            header_hash.copy_from_slice(&raw[16..48]);
            let entry = SecondaryEntry {
                block_offset: read_be_u64(&raw[0..8]).unwrap(),
                header_hash,
                slot: if is_ebb { CHUNK * 21600 } else { block_or_ebb },
                checksum: u32::from_be_bytes([raw[12], raw[13], raw[14], raw[15]]),
                header_offset: u16::from_be_bytes([raw[8], raw[9]]),
                header_size: u16::from_be_bytes([raw[10], raw[11]]),
                is_ebb,
            };
            ours.extend_from_slice(&entry.encode(CHUNK));
        }

        assert_eq!(ours, secondary, "secondary entry layout differs");
    }

    /// Chunk 0's epoch-0 EBB and its genesis block both store `blockOrEBB = 0`,
    /// and they must still occupy DIFFERENT relative slots (0 and 1).
    ///
    /// This is the one place the union is genuinely ambiguous on disk, and it
    /// is resolved positionally: only a chunk's first entry can be an EBB.
    /// Reading it back must not merge the two — which is what a
    /// `blockOrEBB == chunk_num` test alone does, and it survives a restart
    /// into `reopen_chunk_for_append` where the primary index is rewritten.
    ///
    /// Found by regenerating chunk 0 of a real cardano-node mainnet database:
    /// with the value test alone it was the ONLY chunk of ten that differed.
    #[test]
    fn chunk_zero_ebb_and_genesis_block_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let ebb = Hash32::from_bytes([0xEBu8; 32]);
        let genesis = Hash32::from_bytes([0x01u8; 32]);
        let second = Hash32::from_bytes([0x02u8; 32]);

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
            // The epoch-0 boundary block, then the block at slot 0.
            db.append_block(0, 0, &ebb, b"ebb0", true).unwrap();
            db.append_block(0, 1, &genesis, b"genesis", false).unwrap();
            db.flush().unwrap();
        }

        let expected = fs::read(dir.path().join("00000.primary")).unwrap();
        // Relative slot 0 = EBB, relative slot 1 = the slot-0 block.
        assert_eq!(read_primary_offset(&expected, 0), 0);
        assert_eq!(read_primary_offset(&expected, 1), 56);
        assert_eq!(read_primary_offset(&expected, 2), 112);

        // Restart and append into the same chunk: the reopen path re-reads the
        // secondary index and rewrites the primary from it.
        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
            db.append_block(5, 2, &second, b"slot5", false).unwrap();
            db.flush().unwrap();
        }

        let after = fs::read(dir.path().join("00000.primary")).unwrap();
        assert_eq!(
            read_primary_offset(&after, 0),
            0,
            "the EBB must stay at relative slot 0"
        );
        assert_eq!(
            read_primary_offset(&after, 1),
            56,
            "the genesis block must stay at relative slot 1, not merged into the EBB"
        );
        assert_eq!(read_primary_offset(&after, 2), 112);
        // Slot 5 → relative slot 6.
        assert_eq!(read_primary_offset(&after, 6), 112);
        assert_eq!(read_primary_offset(&after, 7), 168);

        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 3);
        assert!(db.has_block(&ebb) && db.has_block(&genesis) && db.has_block(&second));
    }

    /// Reopening a database with a different chunk size is refused.
    ///
    /// `k` is a GENESIS parameter — it is not in `PParams` and no governance
    /// action can change it — so the size is stable for a given network. What
    /// is not stable is dugite's ability to read the genesis file: `Node::new`
    /// falls back to `DEFAULT_SECURITY_PARAM_K` on a load failure and only
    /// warns, which on preview would re-lay the database at 21600 slots per
    /// chunk instead of 4320.
    #[test]
    fn reopening_with_a_different_chunk_size_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        let mut db = ImmutableDB::open_for_writing(dir.path(), 4320).unwrap();
        db.append_block(10, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        db.flush().unwrap();
        drop(db);

        // Same size: fine.
        drop(ImmutableDB::open_for_writing(dir.path(), 4320).unwrap());

        // preview's 4320 vs mainnet's 21600 — the exact confusion the Byron
        // genesis fallback can produce.
        let msg = match ImmutableDB::open_for_writing(dir.path(), 21600) {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("a chunk-size change must be refused"),
        };
        assert!(
            msg.contains("4320") && msg.contains("21600"),
            "the error must name both sizes, got: {msg}"
        );

        // The refusal must not have damaged anything.
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 1);
    }

    #[test]
    fn every_block_lands_in_the_chunk_its_slot_maps_to() {
        // THE ImmutableDB INVARIANT, and the reason cardano-node cannot open a
        // dugite-written database:
        //
        //     for chunk NNNNN, every block's slot satisfies
        //         slot / chunkSize == NNNNN
        //
        // cardano-node numbers chunks by a FIXED slot range — the Byron epoch
        // length, held constant across every era — and computes
        // `chunkIndex(slot)` to locate any block. dugite instead names the chunk
        // after the caller's epoch, clamped to `last + 1`, and only ever rolls on
        // `open_for_writing`. So one chunk absorbs everything written between
        // restarts: measured on real databases, 235 MB against cardano-node's
        // uniform ~2 MB, with blocks up to 33 chunk-ranges past where their
        // number says they belong.
        //
        // Consensus then computes an index for the tip's slot, finds no such
        // file, and fails to open:
        //
        //     FsResourceDoesNotExist … immutable/06040.primary
        //
        // This test drives the WRITER rather than inspecting an existing
        // database, so it fails on the defect itself and not on a historical
        // artefact — a DB fixture would also be repaired away by any future
        // reconciliation change.
        const CHUNK_SIZE: u64 = 21600; // mainnet/preprod Byron epoch length
        let dir = tempfile::tempdir().unwrap();
        let mut db = ImmutableDB::open_for_writing(dir.path(), CHUNK_SIZE).unwrap();

        // Blocks spanning three nominal chunk ranges, written in one session —
        // exactly what a node does between restarts.
        let slots = [10u64, 500, CHUNK_SIZE + 7, CHUNK_SIZE * 2 + 3];
        for (i, slot) in slots.iter().enumerate() {
            db.append_block(
                *slot,
                i as u64 + 1,
                &Hash32::from_bytes([i as u8 + 1; 32]),
                b"block",
                false,
            )
            .unwrap();
        }
        db.flush().unwrap();
        drop(db);

        // Read every secondary index back and check the invariant per chunk.
        let mut violations = Vec::new();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("secondary") {
                continue;
            }
            let idx: u64 = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            for chunk in bytes.chunks_exact(56) {
                let slot = u64::from_be_bytes(chunk[48..56].try_into().unwrap());
                // An EBB stores the EPOCH NUMBER here, which in Byron equals the
                // chunk index — not a violation. (Skipping this is what made the
                // standalone checker condemn every correct database.)
                if slot == idx {
                    continue;
                }
                if slot / CHUNK_SIZE != idx {
                    violations.push((idx, slot, slot / CHUNK_SIZE));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "blocks landed in the wrong chunk — cardano-node cannot open this DB. \
             (chunk, slot, chunk consensus would look for): {violations:?}"
        );
    }

    /// A block whose slot skips several chunk ranges rolls straight to its own
    /// chunk AND materialises the ranges it jumped over.
    ///
    /// Upstream does the same in `appendBlockImpl` — it starts
    /// `countChunks chunk initialChunk` new chunks, not one — because its
    /// iterator walks chunk numbers one at a time and reads each chunk's
    /// primary index to find the next filled slot. A missing file is not read
    /// as "empty"; it raises `FsResourceDoesNotExist` and aborts the open,
    /// which is the failure originally reported in #1081.
    #[test]
    fn skipped_chunk_ranges_are_materialised_not_left_as_holes() {
        let dir = tempfile::tempdir().unwrap();

        let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
        db.append_block(10, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        // Slot 5010 belongs in chunk 5, four chunk ranges beyond chunk 0.
        db.append_block(5010, 2, &Hash32::from_bytes([2u8; 32]), b"b2", false)
            .unwrap();
        db.flush().unwrap();

        // Every chunk from 0 through 5 must have all three files — the empty
        // ones included.
        for n in 0..=5u64 {
            for ext in ["chunk", "secondary", "primary"] {
                let p = dir.path().join(format!("{n:05}.{ext}"));
                assert!(p.exists(), "chunk {n} is missing its .{ext} file");
            }
        }

        // The skipped chunks are genuinely empty, and their primary index is a
        // full-length run of zero offsets rather than a truncated file.
        for n in 1..=4u64 {
            let data = fs::read(dir.path().join(format!("{n:05}.primary"))).unwrap();
            assert_eq!(data.len(), 1 + (1000 + 2) * 4, "chunk {n} primary length");
            assert!(
                data[1..].iter().all(|&b| b == 0),
                "chunk {n} must have all-zero offsets"
            );
            assert_eq!(
                fs::read(dir.path().join(format!("{n:05}.secondary")))
                    .unwrap()
                    .len(),
                0
            );
        }

        // And the blocks are still readable across the gap after a reopen.
        drop(db);
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 2);
        assert_eq!(db.tip_slot(), 5010);
    }

    #[test]
    fn test_primary_index_round_trip() {
        // Write, finalize, reopen → verify all three index files exist
        // and the DB reads correctly.
        let dir = tempfile::tempdir().unwrap();
        let h1 = Hash32::from_bytes([1u8; 32]);
        let h2 = Hash32::from_bytes([2u8; 32]);
        let h3 = Hash32::from_bytes([3u8; 32]);

        {
            let mut db = ImmutableDB::open_for_writing(dir.path(), 1000).unwrap();
            db.append_block(10, 1, &h1, b"block1", false).unwrap();
            db.append_block(20, 2, &h2, b"block2", false).unwrap();
            // Slot 1050 is in chunk 1 — the roll is driven by the slot.
            db.append_block(1050, 3, &h3, b"block3", false).unwrap();
            db.flush().unwrap();
        }

        // Verify all index files exist
        assert!(dir.path().join("00000.chunk").exists());
        assert!(dir.path().join("00000.secondary").exists());
        assert!(dir.path().join("00000.primary").exists());
        assert!(dir.path().join("00001.chunk").exists());
        assert!(dir.path().join("00001.secondary").exists());
        assert!(dir.path().join("00001.primary").exists());

        // Reopen and verify all blocks readable
        let db = ImmutableDB::open(dir.path()).unwrap();
        assert_eq!(db.total_blocks(), 3);
        assert_eq!(db.get_block(&h1).unwrap(), b"block1");
        assert_eq!(db.get_block(&h2).unwrap(), b"block2");
        assert_eq!(db.get_block(&h3).unwrap(), b"block3");
    }

    #[test]
    fn test_primary_index_empty_chunk() {
        // An empty chunk should produce a valid primary index with all offsets = 0.
        let dir = tempfile::tempdir().unwrap();

        let mut db = ImmutableDB::open_for_writing(dir.path(), 100).unwrap();
        // Chunk 0 is skipped over entirely by a block in chunk 1, and is
        // materialised as an empty chunk rather than left as a hole.
        db.append_block(150, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        db.flush().unwrap();

        let primary_path = dir.path().join("00000.primary");
        assert!(primary_path.exists());

        let data = fs::read(&primary_path).unwrap();
        // 100 regular slots + 1 EBB slot + 1 sentinel + version byte
        assert_eq!(data.len(), 1 + 102 * 4);
        assert_eq!(data[0], 0x01, "Version byte");
        for i in 0..102 {
            assert_eq!(
                read_primary_offset(&data, i),
                0,
                "Empty chunk: all offsets should be 0"
            );
        }
    }

    /// The chunk's first slot is `chunk_num * chunk_size` — derived, never
    /// carried alongside. A block at absolute slot 5100 with chunk size 500
    /// lands in chunk 10 at relative slot 100, hence entry 101.
    #[test]
    fn test_primary_index_nonzero_epoch_start() {
        let dir = tempfile::tempdir().unwrap();
        let chunk_size = 500u64;

        let mut db = ImmutableDB::open_for_writing(dir.path(), chunk_size).unwrap();
        // Blocks at absolute slots 5100, 5200, 5300 → all in chunk 10
        // (5100 / 500 == 10), relative slots 100, 200, 300.
        db.append_block(5100, 1, &Hash32::from_bytes([1u8; 32]), b"b1", false)
            .unwrap();
        db.append_block(5200, 2, &Hash32::from_bytes([2u8; 32]), b"b2", false)
            .unwrap();
        db.append_block(5300, 3, &Hash32::from_bytes([3u8; 32]), b"b3", false)
            .unwrap();
        db.flush().unwrap();

        let data = fs::read(dir.path().join("00010.primary")).unwrap();
        assert_eq!(data[0], 0x01);

        // Relative slot 100 → entry[101]; entry[100] must stay empty.
        assert_eq!(read_primary_offset(&data, 101), 0);
        assert_eq!(read_primary_offset(&data, 102), 56);

        // Relative slot 200 → entry[201]; relative slot 300 → entry[301].
        assert_eq!(read_primary_offset(&data, 202), 112);
        assert_eq!(read_primary_offset(&data, 302), 168);

        // Sentinel = 3 * 56 = 168
        let sentinel_idx = chunk_size as usize + 1;
        assert_eq!(read_primary_offset(&data, sentinel_idx), 168);
    }

    /// Fibonacci offsets must start [1, 2, 3, 5, 8, 13, 21, …] and stop when
    /// offset >= total_chunks OR max_count reached.
    #[test]
    fn fibonacci_chunk_offsets_basic_sequence() {
        assert_eq!(
            fibonacci_chunk_offsets(100, 8),
            vec![1, 2, 3, 5, 8, 13, 21, 34]
        );
        assert_eq!(fibonacci_chunk_offsets(100, 0), Vec::<usize>::new());
        assert_eq!(fibonacci_chunk_offsets(0, 8), Vec::<usize>::new());
    }

    /// Bound check — offsets never reach total_chunks.
    #[test]
    fn fibonacci_chunk_offsets_bounded_by_total_chunks() {
        // 10 chunks: 1,2,3,5,8 — next would be 13 ≥ 10, stop.
        assert_eq!(fibonacci_chunk_offsets(10, 100), vec![1, 2, 3, 5, 8]);
        assert_eq!(fibonacci_chunk_offsets(5, 100), vec![1, 2, 3]);
        assert_eq!(fibonacci_chunk_offsets(2, 100), vec![1]);
        assert_eq!(fibonacci_chunk_offsets(1, 100), Vec::<usize>::new());
    }

    /// max_count cap honoured.
    #[test]
    fn fibonacci_chunk_offsets_max_count_cap() {
        assert_eq!(fibonacci_chunk_offsets(1000, 3), vec![1, 2, 3]);
        assert_eq!(fibonacci_chunk_offsets(1000, 5), vec![1, 2, 3, 5, 8]);
    }

    /// At Haskell's mainnet `k=2160` we expect 18 chunks at the deepest
    /// offset (Fibonacci value just below the k-block window) — the
    /// distribution should give logarithmic-spaced anchors that fit in
    /// `DEEP_HISTORICAL_DEPTH = 8` slots.
    #[test]
    fn fibonacci_chunk_offsets_haskell_parity_for_deep_historical_depth() {
        let offsets = fibonacci_chunk_offsets(5000, 8);
        assert_eq!(offsets, vec![1, 2, 3, 5, 8, 13, 21, 34]);
        // Densest near tip, sparse far from tip — the gap between
        // consecutive offsets grows geometrically.
        for win in offsets.windows(2) {
            assert!(win[1] > win[0]);
        }
    }
}
