//! Write-ahead log (WAL) for crash recovery.
//!
//! Every write operation (insert/delete) is appended to the WAL before being
//! applied to the memtable. On crash recovery, the WAL is replayed to
//! reconstruct the memtable state that was lost.
//!
//! ## Entry format
//!
//! ```text
//! [total_len: u32 LE]     — byte count from op_tag to end of value (excl CRC)
//! [op_tag:    u8]         — 1 = insert, 2 = delete
//! [key_len:   u16 LE]
//! [key_bytes: key_len]
//! [value_len: u16 LE]     — only if op_tag == 1
//! [value:     value_len]  — only if op_tag == 1
//! [crc32:     u32 LE]     — CRC of bytes from op_tag to end of value
//! ```
//!
//! ## Segment rotation
//!
//! When a segment exceeds `wal_segment_size`, it is closed and a new segment
//! is started. Old segments are deleted after a successful memtable flush.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::error::{Error, Result};
use crate::key::Key;
use crate::value::Value;

/// Op tag for insert operations.
const OP_INSERT: u8 = 1;
/// Op tag for delete operations.
const OP_DELETE: u8 = 2;

/// A WAL operation recovered from the log.
#[derive(Debug, Clone)]
pub enum WalOp {
    Insert(Key, Value),
    Delete(Key),
}

/// WAL writer: appends operations to the current segment.
pub struct WalWriter {
    /// Directory containing WAL segment files.
    wal_dir: PathBuf,
    /// Current segment writer.
    writer: Option<BufWriter<File>>,
    /// Current segment number.
    segment_num: u64,
    /// Bytes written to the current segment.
    segment_bytes: usize,
    /// Maximum segment size before rotation.
    max_segment_size: usize,
    /// Whether WAL is enabled.
    enabled: bool,
}

impl WalWriter {
    /// Create a new WAL writer.
    ///
    /// If WAL is disabled, all write operations are no-ops.
    pub fn new(wal_dir: &Path, max_segment_size: usize, enabled: bool) -> Result<Self> {
        if enabled {
            fs::create_dir_all(wal_dir)?;
        }

        // Find the highest existing segment number
        let segment_num = if enabled {
            find_max_segment(wal_dir) + 1
        } else {
            0
        };

        let writer = if enabled {
            Some(open_segment(wal_dir, segment_num)?)
        } else {
            None
        };

        Ok(WalWriter {
            wal_dir: wal_dir.to_path_buf(),
            writer,
            segment_num,
            segment_bytes: 0,
            max_segment_size,
            enabled,
        })
    }

    /// Enable or disable the WAL at runtime.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Append an insert operation to the WAL.
    ///
    /// Each call issues a `write_all` + `flush` for durability.  For bulk
    /// block-level writes prefer [`log_batch`] which serialises all entries
    /// into a single `write_all` + one `flush`, eliminating the per-entry
    /// syscall overhead (~10 µs context-switch per flush on Linux).
    pub fn log_insert(&mut self, key: &Key, value: &Value) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.maybe_rotate()?;

        let writer = self.writer.as_mut().unwrap();

        // Calculate payload: op_tag(1) + key_len(2) + key + value_len(2) + value
        let payload_len = 1 + 2 + key.len() + 2 + value.len();

        // Build the payload for CRC computation
        let mut payload = Vec::with_capacity(payload_len);
        payload.push(OP_INSERT);
        payload.write_u16::<LittleEndian>(key.len() as u16)?;
        payload.extend_from_slice(key.as_ref());
        payload.write_u16::<LittleEndian>(value.len() as u16)?;
        payload.extend_from_slice(value.as_ref());

        let crc = crc32fast::hash(&payload);

        // Write: total_len + payload + crc
        writer.write_u32::<LittleEndian>(payload_len as u32)?;
        writer.write_all(&payload)?;
        writer.write_u32::<LittleEndian>(crc)?;
        writer.flush()?;

        self.segment_bytes += 4 + payload_len + 4;
        Ok(())
    }

    /// Append a delete operation to the WAL.
    ///
    /// Each call issues a `write_all` + `flush` for durability.  For bulk
    /// block-level writes prefer [`log_batch`] which serialises all entries
    /// into a single `write_all` + one `flush`.
    pub fn log_delete(&mut self, key: &Key) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.maybe_rotate()?;

        let writer = self.writer.as_mut().unwrap();

        // Payload: op_tag(1) + key_len(2) + key
        let payload_len = 1 + 2 + key.len();

        let mut payload = Vec::with_capacity(payload_len);
        payload.push(OP_DELETE);
        payload.write_u16::<LittleEndian>(key.len() as u16)?;
        payload.extend_from_slice(key.as_ref());

        let crc = crc32fast::hash(&payload);

        writer.write_u32::<LittleEndian>(payload_len as u32)?;
        writer.write_all(&payload)?;
        writer.write_u32::<LittleEndian>(crc)?;
        writer.flush()?;

        self.segment_bytes += 4 + payload_len + 4;
        Ok(())
    }

    /// Append a batch of insert and delete operations in a single `write_all` + `flush`.
    ///
    /// This is the preferred write path for block-level UTxO diffs.  Compared
    /// to calling [`log_insert`] / [`log_delete`] per entry, batching amortises
    /// the `flush()` syscall cost (≈10 µs context switch on Linux) across all
    /// entries in the batch — a ~300-entry Babbage block pays one flush instead
    /// of 300.
    ///
    /// ## Durability
    ///
    /// All entries are serialised into a single contiguous buffer and written
    /// atomically via one `write_all` call followed by one `flush`.  A crash
    /// between `write_all` and `flush` leaves a partial segment that the WAL
    /// replayer skips via CRC mismatch — exactly the same recovery path as a
    /// mid-entry crash in the single-entry path.  Within the batch the CRC is
    /// per entry (not per batch) so the replayer recovers as many complete
    /// entries as it can from the tail segment.
    ///
    /// ## Segment rotation
    ///
    /// Rotation is checked once before writing.  If the batch itself would
    /// overflow the segment limit the write still proceeds — the segment is
    /// allowed to exceed `max_segment_size` by at most one batch rather than
    /// splitting a batch across two segments (which would complicate the
    /// replayer).  The next single-entry write or batch will trigger rotation.
    pub fn log_batch(&mut self, inserts: &[(Key, Value)], deletes: &[Key]) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if inserts.is_empty() && deletes.is_empty() {
            return Ok(());
        }

        // Rotate before writing if the current segment is already full.
        self.maybe_rotate()?;

        // Pre-compute total buffer size: one framed entry per insert/delete.
        //
        // Insert entry:  4 (total_len) + payload_len + 4 (crc)
        //   payload_len = 1 (op) + 2 (key_len) + key + 2 (value_len) + value
        // Delete entry:  4 (total_len) + payload_len + 4 (crc)
        //   payload_len = 1 (op) + 2 (key_len) + key
        let mut total_bytes: usize = 0;
        for (key, value) in inserts.iter() {
            let payload_len = 1 + 2 + key.len() + 2 + value.len();
            total_bytes += 4 + payload_len + 4;
        }
        for key in deletes.iter() {
            let payload_len = 1 + 2 + key.len();
            total_bytes += 4 + payload_len + 4;
        }

        let mut buf: Vec<u8> = Vec::with_capacity(total_bytes);

        // Encode inserts.
        for (key, value) in inserts.iter() {
            let payload_len = 1 + 2 + key.len() + 2 + value.len();
            // Build payload first for CRC.
            let mut payload = Vec::with_capacity(payload_len);
            payload.push(OP_INSERT);
            payload.write_u16::<LittleEndian>(key.len() as u16)?;
            payload.extend_from_slice(key.as_ref());
            payload.write_u16::<LittleEndian>(value.len() as u16)?;
            payload.extend_from_slice(value.as_ref());
            let crc = crc32fast::hash(&payload);
            buf.write_u32::<LittleEndian>(payload_len as u32)?;
            buf.extend_from_slice(&payload);
            buf.write_u32::<LittleEndian>(crc)?;
        }

        // Encode deletes.
        for key in deletes.iter() {
            let payload_len = 1 + 2 + key.len();
            let mut payload = Vec::with_capacity(payload_len);
            payload.push(OP_DELETE);
            payload.write_u16::<LittleEndian>(key.len() as u16)?;
            payload.extend_from_slice(key.as_ref());
            let crc = crc32fast::hash(&payload);
            buf.write_u32::<LittleEndian>(payload_len as u32)?;
            buf.extend_from_slice(&payload);
            buf.write_u32::<LittleEndian>(crc)?;
        }

        // Single write_all + flush — the key performance gain vs per-entry path.
        let writer = self.writer.as_mut().unwrap();
        writer.write_all(&buf)?;
        writer.flush()?;

        self.segment_bytes += buf.len();
        Ok(())
    }

    /// Delete all WAL segments (called after successful memtable flush).
    pub fn clear(&mut self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        // Close current writer
        self.writer = None;

        // Delete all segment files
        if let Ok(entries) = fs::read_dir(&self.wal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("wal-") && name.ends_with(".log") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }

        // Start a fresh segment
        self.segment_num += 1;
        self.segment_bytes = 0;
        self.writer = Some(open_segment(&self.wal_dir, self.segment_num)?);
        Ok(())
    }

    /// Rotate to a new segment if the current one is full.
    fn maybe_rotate(&mut self) -> Result<()> {
        if self.segment_bytes >= self.max_segment_size {
            self.writer = None; // Close current
            self.segment_num += 1;
            self.segment_bytes = 0;
            self.writer = Some(open_segment(&self.wal_dir, self.segment_num)?);
        }
        Ok(())
    }
}

/// Replay all WAL segments in a directory, returning operations in order.
///
/// Corrupted or truncated entries at the end of a segment are silently skipped
/// (this is expected after a crash — the partially written entry is discarded).
pub fn replay_wal(wal_dir: &Path) -> Result<Vec<WalOp>> {
    if !wal_dir.exists() {
        return Ok(Vec::new());
    }

    let mut segments: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(wal_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("wal-") && name.ends_with(".log") {
            segments.push(entry.path());
        }
    }
    segments.sort();

    let mut ops = Vec::new();
    for segment_path in &segments {
        replay_segment(segment_path, &mut ops)?;
    }

    Ok(ops)
}

/// Replay a single WAL segment file.
fn replay_segment(path: &Path, ops: &mut Vec<WalOp>) -> Result<()> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut offset: u64 = 0;

    while offset < file_len {
        // Try to read the next entry. If we hit EOF or corruption, stop.
        match read_wal_entry(&mut reader, offset) {
            Ok((op, bytes_read)) => {
                ops.push(op);
                offset += bytes_read as u64;
            }
            Err(_) => {
                // Truncated/corrupted entry at end of segment — normal after crash
                break;
            }
        }
    }

    Ok(())
}

/// Read a single WAL entry from a reader.
/// Returns the operation and the total bytes consumed.
fn read_wal_entry<R: Read>(reader: &mut R, offset: u64) -> Result<(WalOp, usize)> {
    let total_len = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| Error::WalCorruption {
            offset,
            detail: format!("failed to read entry length: {e}"),
        })? as usize;

    // Read the payload
    let mut payload = vec![0u8; total_len];
    reader
        .read_exact(&mut payload)
        .map_err(|e| Error::WalCorruption {
            offset,
            detail: format!("truncated payload: {e}"),
        })?;

    // Read and verify CRC
    let expected_crc = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| Error::WalCorruption {
            offset,
            detail: format!("failed to read CRC: {e}"),
        })?;

    let actual_crc = crc32fast::hash(&payload);
    if actual_crc != expected_crc {
        return Err(Error::WalCorruption {
            offset,
            detail: format!("CRC mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"),
        });
    }

    // Parse the payload
    let mut cursor = io::Cursor::new(&payload);
    let op_tag = cursor.read_u8().map_err(|e| Error::WalCorruption {
        offset,
        detail: format!("failed to read op tag: {e}"),
    })?;

    let key_len = cursor
        .read_u16::<LittleEndian>()
        .map_err(|e| Error::WalCorruption {
            offset,
            detail: format!("failed to read key length: {e}"),
        })? as usize;

    let mut key_buf = vec![0u8; key_len];
    cursor
        .read_exact(&mut key_buf)
        .map_err(|e| Error::WalCorruption {
            offset,
            detail: format!("truncated key: {e}"),
        })?;

    let op = match op_tag {
        OP_INSERT => {
            let value_len = cursor
                .read_u16::<LittleEndian>()
                .map_err(|e| Error::WalCorruption {
                    offset,
                    detail: format!("failed to read value length: {e}"),
                })? as usize;
            let mut value_buf = vec![0u8; value_len];
            cursor
                .read_exact(&mut value_buf)
                .map_err(|e| Error::WalCorruption {
                    offset,
                    detail: format!("truncated value: {e}"),
                })?;
            WalOp::Insert(Key::new(key_buf), Value::new(value_buf))
        }
        OP_DELETE => WalOp::Delete(Key::new(key_buf)),
        _ => {
            return Err(Error::WalCorruption {
                offset,
                detail: format!("unknown op tag: {op_tag}"),
            });
        }
    };

    let bytes_consumed = 4 + total_len + 4; // len + payload + crc
    Ok((op, bytes_consumed))
}

/// Find the highest segment number in the WAL directory.
fn find_max_segment(wal_dir: &Path) -> u64 {
    let mut max = 0u64;
    if let Ok(entries) = fs::read_dir(wal_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(num_str) = name
                .strip_prefix("wal-")
                .and_then(|s| s.strip_suffix(".log"))
            {
                if let Ok(num) = num_str.parse::<u64>() {
                    max = max.max(num);
                }
            }
        }
    }
    max
}

/// Open a new WAL segment file for writing.
fn open_segment(wal_dir: &Path, segment_num: u64) -> Result<BufWriter<File>> {
    let path = wal_dir.join(format!("wal-{segment_num:06}.log"));
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(BufWriter::new(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── log_batch tests ──────────────────────────────────────────────────

    /// log_batch with inserts only: all entries replay correctly and
    /// produce the same result as individual log_insert calls.
    #[test]
    fn test_wal_log_batch_inserts_only() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let inserts: Vec<(Key, Value)> = (0u8..5)
            .map(|i| (Key::from([i]), Value::from([i * 10])))
            .collect();

        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer.log_batch(&inserts, &[]).unwrap();
        }

        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 5);
        for (idx, op) in ops.iter().enumerate() {
            match op {
                WalOp::Insert(k, v) => {
                    assert_eq!(k.as_ref(), &[idx as u8]);
                    assert_eq!(v.as_ref(), &[idx as u8 * 10]);
                }
                _ => panic!("expected insert at idx {idx}"),
            }
        }
    }

    /// log_batch with deletes only: tombstones replay correctly.
    #[test]
    fn test_wal_log_batch_deletes_only() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let deletes: Vec<Key> = (10u8..13).map(|i| Key::from([i])).collect();

        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer.log_batch(&[], &deletes).unwrap();
        }

        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 3);
        for (idx, op) in ops.iter().enumerate() {
            match op {
                WalOp::Delete(k) => {
                    assert_eq!(k.as_ref(), &[(10 + idx) as u8]);
                }
                _ => panic!("expected delete at idx {idx}"),
            }
        }
    }

    /// log_batch with mixed inserts and deletes: all entries present in
    /// declaration order (inserts first, then deletes — matching encode order).
    #[test]
    fn test_wal_log_batch_mixed() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let inserts = vec![
            (Key::from([1u8]), Value::from([100u8])),
            (Key::from([2u8]), Value::from([200u8])),
        ];
        let deletes = vec![Key::from([3u8]), Key::from([4u8])];

        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer.log_batch(&inserts, &deletes).unwrap();
        }

        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 4, "expected 2 inserts + 2 deletes");
        // inserts are encoded first
        match &ops[0] {
            WalOp::Insert(k, v) => {
                assert_eq!(k.as_ref(), &[1u8]);
                assert_eq!(v.as_ref(), &[100u8]);
            }
            _ => panic!("ops[0] should be insert"),
        }
        match &ops[1] {
            WalOp::Insert(k, v) => {
                assert_eq!(k.as_ref(), &[2u8]);
                assert_eq!(v.as_ref(), &[200u8]);
            }
            _ => panic!("ops[1] should be insert"),
        }
        match &ops[2] {
            WalOp::Delete(k) => assert_eq!(k.as_ref(), &[3u8]),
            _ => panic!("ops[2] should be delete"),
        }
        match &ops[3] {
            WalOp::Delete(k) => assert_eq!(k.as_ref(), &[4u8]),
            _ => panic!("ops[3] should be delete"),
        }
    }

    /// log_batch on a disabled WAL is a no-op (no file created, returns Ok).
    #[test]
    fn test_wal_log_batch_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let inserts = vec![(Key::from([1u8]), Value::from([99u8]))];
        let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, false).unwrap();
        writer.log_batch(&inserts, &[]).unwrap();

        // No WAL directory should be created when WAL is disabled.
        assert!(!wal_dir.exists());
    }

    /// log_batch empty: no-op, no file created beyond the open segment.
    #[test]
    fn test_wal_log_batch_empty() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer.log_batch(&[], &[]).unwrap();
        }

        // Replay should return 0 ops (empty batch is a no-op).
        let ops = replay_wal(&wal_dir).unwrap();
        assert!(ops.is_empty());
    }

    /// log_batch results are identical to individual log_insert / log_delete calls
    /// (round-trip parity).
    #[test]
    fn test_wal_log_batch_parity_with_individual() {
        let dir_batch = tempfile::tempdir().unwrap();
        let dir_individual = tempfile::tempdir().unwrap();

        let inserts: Vec<(Key, Value)> = (0u8..8)
            .map(|i| (Key::from([i, i + 1]), Value::from(vec![i; 4])))
            .collect();
        let deletes: Vec<Key> = (8u8..11).map(|i| Key::from([i])).collect();

        // Write via batch
        {
            let mut w = WalWriter::new(dir_batch.path(), 64 * 1024 * 1024, true).unwrap();
            w.log_batch(&inserts, &deletes).unwrap();
        }

        // Write via individual calls
        {
            let mut w = WalWriter::new(dir_individual.path(), 64 * 1024 * 1024, true).unwrap();
            for (k, v) in &inserts {
                w.log_insert(k, v).unwrap();
            }
            for k in &deletes {
                w.log_delete(k).unwrap();
            }
        }

        let batch_ops = replay_wal(dir_batch.path()).unwrap();
        let individual_ops = replay_wal(dir_individual.path()).unwrap();

        assert_eq!(batch_ops.len(), individual_ops.len(), "op count must match");
        for (b, i) in batch_ops.iter().zip(individual_ops.iter()) {
            match (b, i) {
                (WalOp::Insert(bk, bv), WalOp::Insert(ik, iv)) => {
                    assert_eq!(bk.as_ref(), ik.as_ref(), "insert key mismatch");
                    assert_eq!(bv.as_ref(), iv.as_ref(), "insert value mismatch");
                }
                (WalOp::Delete(bk), WalOp::Delete(ik)) => {
                    assert_eq!(bk.as_ref(), ik.as_ref(), "delete key mismatch");
                }
                _ => panic!("op type mismatch between batch and individual"),
            }
        }
    }

    #[test]
    fn test_wal_write_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        // Write some operations
        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer
                .log_insert(&Key::from([1, 2, 3]), &Value::from([10, 20, 30]))
                .unwrap();
            writer
                .log_insert(&Key::from([4, 5, 6]), &Value::from([40, 50, 60]))
                .unwrap();
            writer.log_delete(&Key::from([1, 2, 3])).unwrap();
        }

        // Replay
        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 3);

        match &ops[0] {
            WalOp::Insert(k, v) => {
                assert_eq!(k.as_ref(), &[1, 2, 3]);
                assert_eq!(v.as_ref(), &[10, 20, 30]);
            }
            _ => panic!("expected insert"),
        }
        match &ops[1] {
            WalOp::Insert(k, v) => {
                assert_eq!(k.as_ref(), &[4, 5, 6]);
                assert_eq!(v.as_ref(), &[40, 50, 60]);
            }
            _ => panic!("expected insert"),
        }
        match &ops[2] {
            WalOp::Delete(k) => {
                assert_eq!(k.as_ref(), &[1, 2, 3]);
            }
            _ => panic!("expected delete"),
        }
    }

    #[test]
    fn test_wal_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, false).unwrap();
        writer
            .log_insert(&Key::from([1]), &Value::from([2]))
            .unwrap();

        // WAL dir should not exist
        assert!(!wal_dir.exists());
    }

    #[test]
    fn test_wal_truncated_entry() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");
        fs::create_dir_all(&wal_dir).unwrap();

        // Write a valid entry then corrupt the file
        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer
                .log_insert(&Key::from([1, 2]), &Value::from([10, 20]))
                .unwrap();
        }

        // Append garbage (simulates partial write during crash)
        let segment_path = wal_dir.join("wal-000001.log");
        let mut file = OpenOptions::new().append(true).open(&segment_path).unwrap();
        file.write_all(&[0xFF, 0xFF, 0xFF]).unwrap();

        // Replay should recover the valid entry and skip the garbage
        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn test_wal_clear() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
        writer
            .log_insert(&Key::from([1]), &Value::from([10]))
            .unwrap();

        writer.clear().unwrap();

        // After clear, replay should return no operations
        let ops = replay_wal(&wal_dir).unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_wal_segment_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");

        // Very small segment size to force rotation
        let mut writer = WalWriter::new(&wal_dir, 50, true).unwrap();

        for i in 0u8..20 {
            writer
                .log_insert(&Key::from([i]), &Value::from(vec![i; 10]))
                .unwrap();
        }

        // Should have created multiple segments
        let segment_count = fs::read_dir(&wal_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".log")
            })
            .count();
        assert!(
            segment_count > 1,
            "expected multiple segments, got {segment_count}"
        );

        // All operations should still replay correctly
        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 20);
    }

    #[test]
    fn test_wal_crc_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = dir.path().join("wal");
        fs::create_dir_all(&wal_dir).unwrap();

        // Write two entries
        {
            let mut writer = WalWriter::new(&wal_dir, 64 * 1024 * 1024, true).unwrap();
            writer
                .log_insert(&Key::from([1]), &Value::from([10]))
                .unwrap();
            writer
                .log_insert(&Key::from([2]), &Value::from([20]))
                .unwrap();
        }

        // Corrupt the CRC of the second entry (flip a byte in the middle of the file)
        let segment_path = wal_dir.join("wal-000001.log");
        let mut data = fs::read(&segment_path).unwrap();
        // Corrupt a byte in the second entry's payload area
        let mid = data.len() / 2 + 2;
        if mid < data.len() {
            data[mid] ^= 0xFF;
        }
        fs::write(&segment_path, &data).unwrap();

        // Replay should recover the first entry but stop at corruption
        let ops = replay_wal(&wal_dir).unwrap();
        assert_eq!(ops.len(), 1); // Only first entry survives
    }
}
