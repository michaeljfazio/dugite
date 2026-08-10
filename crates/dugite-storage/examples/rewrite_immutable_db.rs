//! Re-write a prefix of an existing ImmutableDB through dugite's own writer.
//!
//! The regression test #1081 asks for has always been the same one sentence:
//! hand a dugite-written ImmutableDB to cardano-node and require it to open.
//! Producing one used to mean syncing a chain. This reads real blocks out of a
//! cardano-node database and appends them through [`ImmutableDB::append_block`],
//! so the result is laid out entirely by dugite while containing bytes a real
//! node will accept.
//!
//! ```text
//! cargo run --release -p dugite-storage --example rewrite_immutable_db -- \
//!     <source-immutable-dir> <dest-immutable-dir> <chunk-size> [chunk-count]
//!
//! # mainnet: 10 * k = 21600, first 3 chunks (Byron, includes EBBs)
//! cargo run --release -p dugite-storage --example rewrite_immutable_db -- \
//!     db-cn-mainnet/immutable /tmp/db-dugite-written/immutable 21600 3
//! ```
//!
//! Then point cardano-node at the parent of `<dest-immutable-dir>`. Before
//! #1081 it failed at open with `FsResourceDoesNotExist` on a chunk dugite
//! never wrote, or `blocks have non-increasing slot numbers`.
//!
//! The source database is only ever READ.

use dugite_primitives::hash::Hash32;
use dugite_storage::chain_db::write_db_marker;
use dugite_storage::immutable_db::ImmutableDB;
use std::path::{Path, PathBuf};

const SECONDARY_ENTRY_SIZE: usize = 56;

fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b.try_into().expect("8 bytes"))
}

/// One block, as located by the secondary index.
struct Entry {
    slot: u64,
    hash: Hash32,
    is_ebb: bool,
    cbor: Vec<u8>,
}

fn read_chunk(src: &Path, chunk_num: u64, chunk_size: u64) -> std::io::Result<Vec<Entry>> {
    let secondary = std::fs::read(src.join(format!("{chunk_num:05}.secondary")))?;
    let data = std::fs::read(src.join(format!("{chunk_num:05}.chunk")))?;

    let n = secondary.len() / SECONDARY_ENTRY_SIZE;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let raw = &secondary[i * SECONDARY_ENTRY_SIZE..(i + 1) * SECONDARY_ENTRY_SIZE];
        let offset = be_u64(&raw[0..8]) as usize;
        let block_or_ebb = be_u64(&raw[48..56]);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&raw[16..48]);

        // An EBB is a chunk's FIRST entry and stores its EPOCH, which for Byron
        // is the chunk number. Both halves matter: chunk 0's genesis block also
        // stores 0.
        let is_ebb = i == 0 && block_or_ebb == chunk_num;
        let slot = if is_ebb {
            chunk_num * chunk_size
        } else {
            block_or_ebb
        };

        // Blocks run back to back; this one ends where the next begins.
        let end = if i + 1 < n {
            let next = &secondary[(i + 1) * SECONDARY_ENTRY_SIZE..];
            be_u64(&next[0..8]) as usize
        } else {
            data.len()
        };

        out.push(Entry {
            slot,
            hash: Hash32::from_bytes(hash),
            is_ebb,
            cbor: data[offset..end].to_vec(),
        });
    }
    Ok(out)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "usage: {} <source-immutable-dir> <dest-immutable-dir> <chunk-size> \
             [chunk-count] [network-magic]",
            args[0]
        );
        std::process::exit(2);
    }
    let src = PathBuf::from(&args[1]);
    let dest = PathBuf::from(&args[2]);
    let chunk_size: u64 = args[3].parse()?;
    let limit: usize = args
        .get(4)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(usize::MAX);
    let network_magic: u64 = args
        .get(5)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(764_824_073);

    let mut chunk_nums: Vec<u64> = std::fs::read_dir(&src)?
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".chunk")?.parse::<u64>().ok()
        })
        .collect();
    chunk_nums.sort_unstable();
    chunk_nums.truncate(limit);

    if dest.exists() {
        return Err(format!("destination {} already exists", dest.display()).into());
    }
    std::fs::create_dir_all(&dest)?;

    let mut db = ImmutableDB::open_for_writing(&dest, chunk_size)?;
    let (mut blocks, mut ebbs) = (0u64, 0u64);

    for chunk_num in &chunk_nums {
        let entries = read_chunk(&src, *chunk_num, chunk_size)?;
        for e in entries {
            if e.is_ebb {
                ebbs += 1;
            }
            blocks += 1;
            db.append_block(e.slot, blocks, &e.hash, &e.cbor, e.is_ebb)?;
        }
        println!("chunk {chunk_num:05} -> {blocks} blocks written");
    }
    db.flush()?;

    // The node stamps this at startup; without it cardano-node refuses the
    // directory before reading a chunk.
    let db_root = dest.parent().unwrap_or(&dest);
    write_db_marker(db_root, network_magic)?;

    println!(
        "\nwrote {blocks} blocks ({ebbs} EBBs) from {} source chunks into {}",
        chunk_nums.len(),
        dest.display()
    );
    println!(
        "now point cardano-node at {}",
        dest.parent().unwrap_or(&dest).display()
    );
    Ok(())
}
