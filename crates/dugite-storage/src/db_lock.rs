//! Exclusive advisory lock on the ChainDB database directory (issue #929).
//!
//! Two dugite-node processes pointed at the same `--database-path` would
//! both obtain writable handles to the same chunk files, secondary indexes,
//! `tip.meta`, and `hash_index.dat`, and interleave writes with no error.
//! The ImmutableDB write path is stateful in-process (`ActiveChunk` buffered
//! index, `current_offset`, `BufWriter`), so a second writer is guaranteed
//! corruption, not merely contention.
//!
//! cardano-node locks its DB directory at open — ouroboros-consensus
//! `Ouroboros.Consensus.Node.DbLock.withLockDB` takes an OS file lock on a
//! `lock` file at the database-path root and fails with `The db is used by
//! another process` on conflict. This is the dugite equivalent: an advisory
//! `flock(2)` on `<db>/lock`, acquired in `ChainDB::open` before any other
//! file is touched and held for the lifetime of the [`crate::ChainDB`].
//! Advisory locks are released by the OS on process death, so a crashed
//! process never leaves a stale lock behind.

use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use tracing::debug;

use crate::chain_db::ChainDBError;

/// An exclusive advisory lock on a database directory.
///
/// Held for the lifetime of the owning `ChainDB`. Dropping it closes the
/// file descriptor, which releases the `flock`, allowing another process
/// (or a later open in this process) to acquire the database.
pub struct DbDirLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl DbDirLock {
    /// Acquire an exclusive lock on the database directory.
    ///
    /// Creates (or reuses) `<db_path>/lock` and takes a non-blocking
    /// exclusive `flock` on it. On success the holder's pid is written into
    /// the file so a conflicting open can name it. On conflict returns
    /// [`ChainDBError::DatabaseLocked`] naming the lock file and, when
    /// readable, the pid of the current holder.
    pub fn acquire(db_path: &Path) -> Result<Self, ChainDBError> {
        let lock_path = db_path.join("lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;

        if file.try_lock_exclusive().is_err() {
            // Best-effort: the holder wrote its pid into the file.
            let mut contents = String::new();
            let _ = file.read_to_string(&mut contents);
            let holder_pid = contents.trim().to_string();
            return Err(ChainDBError::DatabaseLocked {
                lock_path: lock_path.display().to_string(),
                holder: if holder_pid.is_empty() {
                    "unknown pid".to_string()
                } else {
                    format!("pid {holder_pid}")
                },
            });
        }

        // Record our pid for the conflict message of a later contender.
        // Advisory-informational only — failures here must not fail the open.
        let _ = file.set_len(0);
        let _ = file.rewind();
        let _ = write!(file, "{}", std::process::id());
        let _ = file.flush();

        debug!(lock = %lock_path.display(), pid = std::process::id(), "ChainDB: database directory lock acquired");

        Ok(DbDirLock {
            _file: file,
            path: lock_path,
        })
    }

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// The flock is released when `_file` is closed on drop. The lock file itself
// is deliberately left in place — unlinking it would open an acquire/unlink
// race between two contenders.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_writes_pid_and_conflict_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = DbDirLock::acquire(dir.path()).unwrap();
        let err = match DbDirLock::acquire(dir.path()) {
            Ok(_) => panic!("second acquire must fail"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(&std::process::id().to_string()),
            "conflict error must carry the holder pid: {msg}"
        );
    }

    #[test]
    fn released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _lock = DbDirLock::acquire(dir.path()).unwrap();
        }
        let _lock2 = DbDirLock::acquire(dir.path()).expect("reacquire after drop");
    }
}
