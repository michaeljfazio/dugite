---
name: immutabledb-validation-reconstruction
description: ImmutableDB open-time validation policies, chunk/index reconstruction algorithm, and how ChainDB derives+cross-checks its tip at startup
type: reference
---

# ImmutableDB open-time validation (ouroboros-consensus)

File: `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ImmutableDB/Impl/Validation.hs`
(monorepo `IntersectMBO/ouroboros-consensus`, `main` branch, read 2026-07-30)

## ValidationPolicy (Impl/Types.hs)

Two constructors, chosen by the caller of `ImmutableDB.openDB`:
- `ValidateMostRecentChunk` — only the most recent chunk on disk is validated;
  prior chunks/index files are never even checked for presence. Used for
  fast restarts after a clean shutdown.
- `ValidateAllChunks` — every chunk from chunk 0 through the last chunk on
  disk is validated. Used after an unclean shutdown / crash.

Both can throw `MissingFileError`/`InvalidFileError` for the chunk(s) they
actually inspect.

## validateChunk: the reconstruction algorithm

Docstring (Validation.hs:318-350), verbatim:
> * Invalid or missing chunk files will cause truncation. All blocks after a
>   gap in blocks (due to a missing blocks or invalid block(s)) are truncated.
> * Chunk files are the main source of truth. Primary and secondary index
>   files can be reconstructed from the chunk files using the
>   'ChunkFileParser'. If index files are missing, corrupt, or do not match
>   the chunk files, they are overwritten.
> * The 'ChunkFileParser' checks whether the hashes (header hash) line up
>   within an chunk. When they do not, we truncate the chunk, including the
>   block of which its previous hash does not match the hash of the previous
>   block.
> * For each block, the 'ChunkFileParser' checks whether the checksum ...
>   from the secondary index file match the ones retrieved from the actual
>   block. If they don't match or if the secondary index file is missing or
>   corrupt, we have to do the expensive integrity check of the block itself.
> * This function checks whether the first block in the chunk fits onto the
>   last block of the previous chunk by checking the hashes. If they do not
>   fit, this chunk is truncated and () is thrown.
> * When an invalid block needs to be truncated, trailing empty slots are
>   also truncated so that the tip of the database will always point to a
>   valid block or EBB.

**Chunk files are the ONLY source of truth.** Index files are always
disposable/derived:
```haskell
-- Reconstruct the primary index from the 'Secondary.Entry's.
let primaryIndex = reconstructPrimaryIndex (Proxy @blk) chunkInfo
      shouldBeFinalised chunk (map Secondary.blockOrEBB entries)
primaryIndexFileMatches <- ... Primary.load ... >>= \case
  Left () -> ... return False   -- corrupt
  Right primaryIndexFromFile -> return $ primaryIndexFromFile == primaryIndex
unless primaryIndexFileMatches $ do
  traceWith validationTracer $ RewritePrimaryIndex chunk
  Primary.write hasFS chunk primaryIndex
```
Same pattern for the secondary index (built from `parseChunkFile`'s output,
compared byte-for-byte, rewritten on any mismatch). **Missing, truncated, or
corrupt index files are silently reconstructed from the chunk file and
overwritten on disk — this is not an error, it's the normal-path recovery
mechanism.** Never silently skips a chunk that has an index problem; only a
genuinely missing/unparseable *chunk* file (or a chunk whose first block's
prevHash doesn't chain onto the previous chunk's last block) causes
`throwError ()`, which triggers truncation of everything from that point on.

`parseChunkFile` extends past the index deliberately: if the chunk file has
more data than the index knew about (extra valid blocks) or a deserialisation
error partway through, the file is physically `hTruncate`d to
`endOfLastValidBlock`, and the reconstructed indices only ever cover what
validated successfully.

## Two validation strategies

`validateAllChunks` (Validation.hs:188-265): walks chunk 0 → last chunk,
threading the previous chunk's last block hash into `chunkFileDoesntFit`
checks. Tracks `lastValid :: (ChunkNo, WithOrigin (Tip blk))` — the last
chunk+tip that validated cleanly. On any single-chunk validation failure
(`Left ()`), immediately stops, calls `cleanup lastValid chunk` (removes all
files after `lastValid`'s chunk, and un-finalises that chunk if it wasn't
the one that failed), and returns `lastValid`. **The DB tip is moved back to
the last chunk that fully validated — never forward, never guessed.**

`validateMostRecentChunk` (Validation.hs:271-306): tries only the last chunk
on disk; if it has no valid block, tries chunk-1, chunk-2, ... down to 0,
stopping at the first chunk (walking backwards) that has ≥1 valid block. If
NONE of the chunks have a valid block, falls back to `(firstChunkNo, Origin)`
— i.e. the ImmutableDB tip becomes Origin (empty DB), all chunk files are
removed via `removeFilesStartingFrom hasFS firstChunkNo`.

`reconstructPrimaryIndex` (Validation.hs:556-609) errors out (`error
"blocks have non-increasing slot numbers"`) if entries aren't strictly
increasing — this is an internal invariant violation, not a normal
runtime path (would indicate the chunk parser itself is broken).

## Tip derivation + cross-check (Question 2)

The ImmutableDB tip comes **purely from validated chunk/index contents** —
there is no separate persisted "tip file". `validateAndReopen` computes
`(chunk, tip)` from `validate`, and that `tip :: WithOrigin (Tip blk)` is
exactly the last valid block's summary (`summaryToTipInfo`), built from the
last `BlockSummary` the chunk parser produced.

Cross-check happens one layer up, in `ChainDB.Impl.openDBInternal`
(`ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Storage/ChainDB/Impl.hs:147-195`):
```haskell
immutableDB <- ImmutableDB.openDB argsImmutableDb
immutableDbTip <- atomically $ ImmutableDB.getTip immutableDB
...
lgrDB <- LedgerDB.openDB argsLgrDb (ImmutableDB.streamAPI immutableDB)
  (ImmutableDB.tipToPoint immutableDbTip)
  (case immutableDbTip of Origin -> IsNotEBB; NotOrigin tip -> ImmutableDB.tipIsEBB tip)
  (Query.getAnyKnownBlock immutableDB volatileDB) ledgerDbGetVolatileSuffix
```
`LedgerDB.openDB` is handed the ImmutableDB's validated tip point as the
target to reconstruct/replay the ledger state up to (loading the newest
on-disk snapshot ≤ that point, then replaying blocks from the ImmutableDB's
`streamAPI` up to `immutableDbTip`). If a block on the way can't be applied,
or the snapshot is inconsistent, this replay fails loudly — that failure
(not a separate tip-file mismatch check) IS the cross-check between
ImmutableDB content and ledger-state content. There is no independent
"tip.json"-style file to disagree with; the ImmutableDB and LedgerDB are
reconciled by literally replaying one against the other at every open.

See also [[dblock-directory-locking]] for the lock taken before any of this
runs, and [[chainsync-intersection-vs-rollback-distinction]] for the
unrelated ChainSync-level rollback checks.
