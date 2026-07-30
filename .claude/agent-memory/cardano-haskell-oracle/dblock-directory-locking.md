---
name: dblock-directory-locking
description: Exact cardano-node DB directory locking mechanism (Ouroboros.Consensus.Node.DbLock) - lock file name, path, timeout, exception shape
type: reference
---

# DB directory locking (ouroboros-consensus)

File: `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Node/DbLock.hs`
(monorepo `IntersectMBO/ouroboros-consensus`, `main` branch, read 2026-07-30;
full file is only 98 lines, quoted here nearly in full)

## Mechanism

```haskell
withLockDB :: MountPoint -> IO a -> IO a
withLockDB mountPoint =
  withLockDB_ ioFileLock mountPoint dbLockFsPath dbLockTimeout

dbLockFsPath :: FsPath
dbLockFsPath = fsPathFromList ["lock"]

dbLockTimeout :: DiffTime
dbLockTimeout = Time.secondsToDiffTime 2
```

- **Lock file name**: `lock` (empty file), placed directly at the root of
  the `MountPoint` — i.e. `<database-path>/lock`, NOT inside the ImmutableDB
  or VolatileDB subdirectories.
- **Mechanism**: OS-level advisory file lock via `ioFileLock` (an
  `Ouroboros.Consensus.Util.FileLock` abstraction; on POSIX this is backed by
  `System.FileLock` / `flock(2)`-style locking — an OS file lock, not a
  pidfile convention).
- **Timeout**: hardcoded 2 seconds (`dbLockTimeout`). The lock-acquisition
  itself runs on a forked `async` thread (to avoid blocking the main thread
  on an uninterruptible FFI call); the caller does `timeout lockTimeout (wait
  lockFileAsync)`. If the timeout fires, the forked thread is deliberately
  **not** cancelled ("we leave the thread taking the lock running in case of
  a timeout... since if we fail to take the lock, the whole process will
  soon die").
- **Failure exception**:
  ```haskell
  newtype DbLocked = DbLocked FilePath
    deriving (Eq, Show)
  instance Exception DbLocked where
    displayException (DbLocked f) =
      "The db is used by another process. File \"" <> f <> "\" is locked"
  ```
  So the message shape a user sees is literally:
  `The db is used by another process. File "<db-path>/lock" is locked`

## Call site and ordering vs the DB marker check

`ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Node.hs`, function
`stdWithCheckedDB` (around line 818-844):
```haskell
stdWithCheckedDB pb tracer databasePath networkMagic body = do
  -- Check the DB marker first, before doing the lock file, since if the
  -- marker is not present, it expects an empty DB dir.
  either throwIO return =<< checkDbMarker hasFS mountPoint networkMagic
  -- Then create the lock file.
  withLockDB mountPoint $ runWithCheckedDB pb tracer hasFS body
 where
  mountPoint = MountPoint databasePath
  hasFS = ioHasFS mountPoint
```
Order is: (1) `checkDbMarker` (verifies/writes a network-magic marker file
so you can't accidentally point a mainnet DB dir at a testnet config or vice
versa — a *different* mechanism from the lock), THEN (2) `withLockDB`
acquires the actual OS lock, THEN (3) `runWithCheckedDB` looks for the clean
shutdown marker and finally opens the ChainDB (which is where ImmutableDB
validation from [[immutabledb-validation-reconstruction]] happens).

`mountPoint` passed to `withLockDB` is the same `MountPoint databasePath`
used for the whole ChainDB — confirming the lock file lives at the very top
of `--database-path`, sibling to `immutable/`, `volatile/`, `ledger/`, etc.

## Rust translation notes for dugite

dugite's `--database-path` root is the natural place for an equivalent
`lock` file. Match Haskell's shape for parity/familiarity if dugite ever
needs to interop with tooling that expects this convention: empty file named
`lock`, advisory OS lock (e.g. via the `fs2` or `fslock` crate wrapping
`flock`), ~2s acquire timeout, and an error message that names the exact
locked file path so operators immediately recognize "another dugite/cardano
process already has this DB open" rather than a generic IO error.
